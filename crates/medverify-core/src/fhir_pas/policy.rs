use super::{Action, RequestItem};
pub use crate::state::State;
use jiff::civil::{date, Date};
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    OriginalMedicare,
    MedicareAdvantage,
    ApplicableIntegratedPlan,
    MedicaidFfs,
    MedicaidManagedCare,
    ChipFfs,
    ChipManagedCare,
    FederalExchangeQhp,
    StateRegulatedCommercial,
    StatePublicEmployees,
    SelfFundedErisa,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    Medical,
    Drug,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Standard,
    Expedited,
}

/// The urgency a PAS request asks for, as far as its structured data states it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestPriority {
    /// HL7 process priority `normal` or `deferred`, with no urgent level of service, and nothing
    /// else in Claim.priority or Claim.supportingInfo that the verifier does not read.
    Standard,
    /// HL7 process priority `stat`, or the X12 urgent level of service `U`.
    Expedited,
    /// The X12 emergency level of service `03`. Emergency services are covered regardless of
    /// prior authorization (42 CFR 422.113(b)(2)(ii)), so whether a prior-authorization clock
    /// applies is the caller's determination.
    Emergency,
    /// No HL7 process priority code, or `normal`/`deferred` beside content the verifier does not
    /// read: text, extensions or another coding in Claim.priority, or narrative or documents in
    /// Claim.supportingInfo. That content may carry a physician's indication that the standard
    /// timeframe could seriously jeopardize the enrollee's health, which requires an expedited
    /// decision (42 CFR 422.570(c)(2)(ii)). The verifier does not interpret free text.
    Undetermined,
}

impl From<Priority> for RequestPriority {
    fn from(priority: Priority) -> Self {
        match priority {
            Priority::Standard => Self::Standard,
            Priority::Expedited => Self::Expedited,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingScope {
    Applies,
    DrugExcluded,
    PayerExcluded,
    BeforeEffectiveDate,
    UnclassifiedService,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payer {
    pub plan: Plan,
    /// Start of the rating period in effect at receipt for Medicaid or CHIP managed care.
    pub rating_period_start: Option<Date>,
}

impl Payer {
    pub fn timing_scope(
        self,
        received: Date,
        service: Option<ServiceKind>,
    ) -> Result<TimingScope, TimingError> {
        if !matches!(
            self.plan,
            Plan::MedicareAdvantage
                | Plan::ApplicableIntegratedPlan
                | Plan::MedicaidFfs
                | Plan::MedicaidManagedCare
                | Plan::ChipFfs
                | Plan::ChipManagedCare
        ) {
            return Ok(TimingScope::PayerExcluded);
        }
        match service {
            Some(ServiceKind::Drug) => return Ok(TimingScope::DrugExcluded),
            None => return Ok(TimingScope::UnclassifiedService),
            Some(ServiceKind::Medical) => {}
        }
        let effective = date(2026, 1, 1);
        if received < effective {
            return Ok(TimingScope::BeforeEffectiveDate);
        }
        if matches!(self.plan, Plan::MedicaidManagedCare | Plan::ChipManagedCare) {
            let start = self
                .rating_period_start
                .filter(|value| *value <= received)
                .ok_or(TimingError::RatingPeriod)?;
            if start < effective {
                return Ok(TimingScope::BeforeEffectiveDate);
            }
        }
        Ok(TimingScope::Applies)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionReason {
    EnrolleeRequested,
    ProviderRequested,
    MedicaidAgencyNeedsProviderInformation,
    ChipPhysicianOrPlanNeedsInformation,
    ManagedCareNeedsInformationInEnrolleeInterest,
    IntegratedPlanInformationLikelyToLeadToApprovalInEnrolleeInterest,
    MaNoncontractEvidenceMayChangeDenialInEnrolleeInterest,
    MaExtraordinaryCircumstancesInEnrolleeInterest,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionNotice {
    pub sent_at: Timestamp,
    pub written_reason: bool,
    pub grievance_rights: Option<GrievanceRights>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrievanceRights {
    Standard,
    Expedited,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extension {
    pub calendar_days: u8,
    pub reason: ExtensionReason,
    pub granted_at: Timestamp,
    pub notice: Option<ExtensionNotice>,
}

impl ExtensionReason {
    fn permitted(self, plan: Plan, priority: Priority) -> bool {
        use ExtensionReason as R;
        match plan {
            Plan::MedicareAdvantage => matches!(
                self,
                R::EnrolleeRequested
                    | R::MaNoncontractEvidenceMayChangeDenialInEnrolleeInterest
                    | R::MaExtraordinaryCircumstancesInEnrolleeInterest
            ),
            Plan::ApplicableIntegratedPlan => matches!(
                self,
                R::EnrolleeRequested
                    | R::ProviderRequested
                    | R::IntegratedPlanInformationLikelyToLeadToApprovalInEnrolleeInterest
            ),
            Plan::MedicaidManagedCare | Plan::ChipManagedCare => {
                matches!(
                    self,
                    R::EnrolleeRequested | R::ManagedCareNeedsInformationInEnrolleeInterest
                ) || (priority == Priority::Standard && self == R::ProviderRequested)
            }
            Plan::MedicaidFfs => {
                priority == Priority::Standard
                    && matches!(
                        self,
                        R::EnrolleeRequested
                            | R::ProviderRequested
                            | R::MedicaidAgencyNeedsProviderInformation
                    )
            }
            Plan::ChipFfs => {
                priority == Priority::Standard
                    && matches!(
                        self,
                        R::EnrolleeRequested | R::ChipPhysicianOrPlanNeedsInformation
                    )
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimingError {
    RatingPeriod,
    EventOrder,
    InvalidExtension,
    MissingExtensionNotice,
    DateOverflow,
    /// An expedited request was assessed against the standard clock.
    PriorityDowngrade,
    /// The request's priority is undetermined; the caller must determine it first.
    PriorityUndetermined,
    /// The request has the X12 emergency level of service; the caller must establish that a
    /// prior-authorization clock applies, and which one, first.
    EmergencyLevelOfService,
    /// A determination was supplied for a request whose priority is already coded.
    PriorityAlreadyCoded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Timeliness {
    OutsideScope(TimingScope),
    OnTime { deadline: Timestamp },
    Late { deadline: Timestamp },
    AwaitingDecision { deadline: Timestamp },
    Overdue { deadline: Timestamp },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommunicatedAction {
    pub action: Action,
    /// Actual time of notice, as recorded by the transport or workflow.
    pub at: Timestamp,
}

#[derive(Clone, Copy, Debug)]
pub struct Timeline<'a> {
    pub payer: Payer,
    /// Receipt in the calendar time zone used by the applicable program.
    pub received: &'a Zoned,
    pub priority: Priority,
    pub checked_at: Timestamp,
    pub extension: Option<Extension>,
    /// An earlier bound imposed by clinical urgency or a shorter applicable deadline.
    pub earlier_deadline: Option<Timestamp>,
}

impl Timeline<'_> {
    pub fn assess_item(
        self,
        item: RequestItem<'_>,
        classify: impl Fn(&str, &str) -> Option<ServiceKind>,
        event: Option<CommunicatedAction>,
    ) -> Result<Timeliness, TimingError> {
        self.assess_determined_item(item, None, classify, event)
    }

    /// [`Timeline::assess_item`] with the caller's priority determination, if any. Whether the
    /// deadline rule applies is decided first: a drug, payer or effective-date exclusion (or an
    /// unclassified service) is reported as such whatever determination accompanies it, because
    /// there is no clock for it to resolve. Only for an item within the rule is the determination
    /// applied, with [`RequestItem::determine`]'s protections against replacing a coded priority
    /// or lowering a coded `stat`.
    pub fn assess_determined_item(
        self,
        item: RequestItem<'_>,
        determined: Option<Priority>,
        classify: impl Fn(&str, &str) -> Option<ServiceKind>,
        event: Option<CommunicatedAction>,
    ) -> Result<Timeliness, TimingError> {
        let kind = item.service_kind(classify);
        // Items the deadline rule excludes keep their exclusion; there is no clock to check.
        let scope = self.payer.timing_scope(self.received.date(), kind)?;
        if scope != TimingScope::Applies {
            return Ok(Timeliness::OutsideScope(scope));
        }
        let item = match determined {
            Some(priority) => item.determine(priority)?,
            None => item,
        };
        match item.priority() {
            // No timing verdict on a clock the request's own data cannot support.
            RequestPriority::Undetermined => Err(TimingError::PriorityUndetermined),
            RequestPriority::Emergency => Err(TimingError::EmergencyLevelOfService),
            // A payer may expedite a standard request, but must not relax an expedited one.
            RequestPriority::Expedited if self.priority == Priority::Standard => {
                Err(TimingError::PriorityDowngrade)
            }
            RequestPriority::Expedited | RequestPriority::Standard => self.assess(kind, event),
        }
    }

    /// Assess the clock in `self` alone. This takes no request, so it applies none of the
    /// request-priority checks in [`Timeline::assess_item`]; use that for a validated request.
    pub fn assess(
        self,
        kind: Option<ServiceKind>,
        event: Option<CommunicatedAction>,
    ) -> Result<Timeliness, TimingError> {
        let scope = self.payer.timing_scope(self.received.date(), kind)?;
        if scope != TimingScope::Applies {
            return Ok(Timeliness::OutsideScope(scope));
        }
        let received = self.received.timestamp();
        if self.checked_at < received
            || self
                .earlier_deadline
                .is_some_and(|deadline| deadline < received)
            || event.is_some_and(|event| event.at < received || event.at > self.checked_at)
        {
            return Err(TimingError::EventOrder);
        }
        let base = self
            .received
            .checked_add(match self.priority {
                Priority::Standard => jiff::Span::new().days(7),
                Priority::Expedited => jiff::Span::new().hours(72),
            })
            .map_err(|_| TimingError::DateOverflow)?;
        let mut deadline = base.timestamp();
        if let Some(extension) = self.extension {
            if !(1..=14).contains(&extension.calendar_days)
                || !extension.reason.permitted(self.payer.plan, self.priority)
                || extension.granted_at > self.checked_at
                || extension.granted_at < received
                || extension.granted_at > deadline
                || self
                    .earlier_deadline
                    .is_some_and(|limit| extension.granted_at > limit)
                || event.is_some_and(|event| {
                    event.action != Action::Pending && event.at < extension.granted_at
                })
            {
                return Err(TimingError::InvalidExtension);
            }
            deadline = base
                .checked_add(jiff::Span::new().days(extension.calendar_days))
                .map_err(|_| TimingError::DateOverflow)?
                .timestamp();
            let rights = match self.payer.plan {
                Plan::MedicareAdvantage | Plan::ApplicableIntegratedPlan => {
                    Some(GrievanceRights::Expedited)
                }
                Plan::MedicaidManagedCare | Plan::ChipManagedCare
                    if self.priority == Priority::Standard =>
                {
                    Some(GrievanceRights::Standard)
                }
                _ => None,
            };
            if let Some(rights) = rights {
                let notice_deadline = self
                    .earlier_deadline
                    .map_or(deadline, |earlier| earlier.min(deadline));
                if !extension.notice.is_some_and(|notice| {
                    notice.written_reason
                        && notice.grievance_rights == Some(rights)
                        && notice.sent_at >= extension.granted_at
                        && notice.sent_at <= self.checked_at
                        && notice.sent_at <= notice_deadline
                }) {
                    return Err(TimingError::MissingExtensionNotice);
                }
            }
        }
        if let Some(earlier) = self.earlier_deadline {
            deadline = deadline.min(earlier);
        }
        Ok(
            match event.filter(|event| event.action != Action::Pending) {
                Some(event) if event.at <= deadline => Timeliness::OnTime { deadline },
                Some(_) => Timeliness::Late { deadline },
                None if self.checked_at > deadline => Timeliness::Overdue { deadline },
                None => Timeliness::AwaitingDecision { deadline },
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approval,
    Denial,
    Modification,
    Delay,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    MedicalNecessity,
    ClinicalJudgment,
    Administrative,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Qualification {
    LicensedCompetentClinician,
    MarylandQualifiedReviewer,
    MedicalDirector,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerRule {
    pub state: State,
    #[serde(deserialize_with = "parse_date")]
    pub effective: Date,
    #[serde(default, deserialize_with = "parse_optional_date")]
    pub issued_or_renewed_from: Option<Date>,
    pub plans: Vec<Plan>,
    pub decisions: Vec<Decision>,
    pub bases: Vec<Basis>,
    pub qualification: Option<Qualification>,
    #[serde(default)]
    pub prohibit_automated_contribution: bool,
    #[serde(default)]
    pub prohibit_automated_only: bool,
    pub source: String,
}

fn parse_date<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
    String::deserialize(deserializer)?
        .parse()
        .map_err(serde::de::Error::custom)
}
fn parse_optional_date<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Date>, D::Error> {
    parse_date(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewerTable {
    rule: Vec<ReviewerRule>,
}

pub struct ReviewerPolicy {
    rules: Vec<ReviewerRule>,
}

#[derive(Clone, Copy, Debug)]
pub struct ReviewContext {
    pub state: State,
    pub plan: Plan,
    pub on: Date,
    pub issued_or_renewed: Option<Date>,
    pub decision: Decision,
    pub basis: Basis,
    pub specialty: Specialty,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Specialty {
    Medical,
    MentalHealthOrSubstanceUse,
    Dental,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Automation {
    NoneOrAdministrativeSupport,
    /// Automation made part of the clinical determination.
    ContributedToDecision,
    SoleDecisionMaker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profession {
    Physician,
    Dentist,
    OtherClinician,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerEvidence {
    pub profession: Profession,
    pub licensed_and_in_scope: bool,
    pub individually_reviewed: bool,
    pub independent_judgment: bool,
    pub competent_for_service: bool,
    pub actual_clinical_experience: bool,
    pub board_certified_or_eligible_in_same_specialty: bool,
    pub medical_director: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewError {
    MissingPlanDate,
    UnqualifiedReviewer,
    ProhibitedAutomation,
}

pub(crate) const REVIEWER_POLICY_TOML: &str = include_str!("../../../../policy/state_hitl.toml");

impl ReviewerPolicy {
    pub fn state_rules() -> Self {
        Self::from_toml(REVIEWER_POLICY_TOML).expect("bundled state reviewer policy")
    }

    pub fn from_toml(input: &str) -> Result<Self, toml::de::Error> {
        let table: ReviewerTable = toml::from_str(input)?;
        Ok(Self { rules: table.rule })
    }

    pub fn applicable_rules(
        &self,
        context: ReviewContext,
    ) -> Result<Vec<&ReviewerRule>, ReviewError> {
        let mut matches = Vec::new();
        for rule in self.rules.iter().filter(|rule| {
            rule.state == context.state
                && rule.effective <= context.on
                && rule.plans.contains(&context.plan)
                && rule.decisions.contains(&context.decision)
                && rule.bases.contains(&context.basis)
        }) {
            if let Some(effective) = rule.issued_or_renewed_from {
                if context
                    .issued_or_renewed
                    .filter(|date| *date <= context.on)
                    .ok_or(ReviewError::MissingPlanDate)?
                    < effective
                {
                    continue;
                }
            }
            matches.push(rule);
        }
        Ok(matches)
    }

    pub fn verify(
        &self,
        context: ReviewContext,
        automation: Automation,
        reviewer: Option<ReviewerEvidence>,
    ) -> Result<(), ReviewError> {
        for rule in self.applicable_rules(context)? {
            if (rule.prohibit_automated_contribution
                && automation != Automation::NoneOrAdministrativeSupport)
                || (rule.prohibit_automated_only && automation == Automation::SoleDecisionMaker)
            {
                return Err(ReviewError::ProhibitedAutomation);
            }
            if let Some(qualification) = rule.qualification {
                if !reviewer
                    .is_some_and(|evidence| evidence.meets(qualification, context.specialty))
                {
                    return Err(ReviewError::UnqualifiedReviewer);
                }
            }
        }
        Ok(())
    }
}

impl ReviewerEvidence {
    fn meets(self, qualification: Qualification, specialty: Specialty) -> bool {
        self.licensed_and_in_scope
            && self.individually_reviewed
            && self.independent_judgment
            && match qualification {
                Qualification::LicensedCompetentClinician => self.competent_for_service,
                Qualification::MedicalDirector => {
                    self.medical_director && self.profession == Profession::Physician
                }
                Qualification::MarylandQualifiedReviewer => match specialty {
                    Specialty::Medical => {
                        self.profession == Profession::Physician
                            && self.actual_clinical_experience
                            && self.board_certified_or_eligible_in_same_specialty
                    }
                    Specialty::MentalHealthOrSubstanceUse => {
                        self.profession == Profession::Physician
                            && (self.actual_clinical_experience
                                || self.board_certified_or_eligible_in_same_specialty)
                    }
                    Specialty::Dental => {
                        self.profession == Profession::Dentist && self.actual_clinical_experience
                    }
                },
            }
    }
}
