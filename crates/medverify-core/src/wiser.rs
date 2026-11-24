use crate::claim::ProcedureCode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use time::macros::format_description;
use time::{Date, Duration, Month};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    KneeArthroscopy,
    InducedNerveLesions,
    VagusNerveStimulation,
    PhrenicNerveStimulation,
    ElectricalNerveStimulation,
    IncontinenceDevices,
    SacralNerveStimulation,
    ImpotenceTreatment,
    VertebralAugmentation,
    EpiduralSteroidInjection,
    CervicalFusion,
    HypoglossalNerveStimulation,
    SkinSubstitutes,
    DeepBrainStimulation,
    PercutaneousLumbarDecompression,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Inactive,
    Delayed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Site {
    HospitalOutpatient,
    Asc,
    Office,
    Home,
    Inpatient,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    OriginalMedicare,
    RailroadMedicare,
    Other,
}

fn date<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
    let value = String::deserialize(deserializer)?;
    Date::parse(&value, format_description!("[year]-[month]-[day]"))
        .map_err(serde::de::Error::custom)
}

fn serialize_date<S: serde::Serializer>(date: &Date, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(date)
}

fn optional_date<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Date>, D::Error> {
    date(deserializer).map(Some)
}

fn same_diagnosis(left: &str, right: &str) -> bool {
    fn normalized(value: &str) -> impl Iterator<Item = u8> + '_ {
        value
            .bytes()
            .filter(|&byte| byte != b'.')
            .map(|byte| byte.to_ascii_uppercase())
    }
    normalized(left).next().is_some() && normalized(left).eq(normalized(right))
}

fn procedure<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<ProcedureCode, D::Error> {
    ProcedureCode::new(&String::deserialize(deserializer)?)
        .ok_or_else(|| serde::de::Error::custom("invalid procedure code"))
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoldCardRule {
    #[serde(deserialize_with = "date")]
    #[serde(serialize_with = "serialize_date")]
    pub effective: Date,
    pub percent: u8,
    pub minimum_requests: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Primary {
    #[serde(deserialize_with = "procedure")]
    code: ProcedureCode,
    category: Category,
    status: Status,
    #[serde(default, deserialize_with = "optional_date")]
    effective: Option<Date>,
    #[serde(default)]
    requires_indication: bool,
    #[serde(default)]
    sites: Vec<Site>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Indication {
    #[serde(deserialize_with = "procedure")]
    primary: ProcedureCode,
    diagnosis: String,
    // An exclusion takes precedence over a matching inclusion.
    #[serde(default)]
    excluded: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Association {
    #[serde(deserialize_with = "procedure")]
    primary: ProcedureCode,
    #[serde(deserialize_with = "procedure")]
    associated: ProcedureCode,
    #[serde(default)]
    required_pair: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(deserialize_with = "date")]
    request_start: Date,
    #[serde(deserialize_with = "date")]
    service_start: Date,
    #[serde(deserialize_with = "date")]
    service_end: Date,
    participant: String,
    #[serde(deserialize_with = "date")]
    exemption_start: Date,
    gold_card: Vec<GoldCardRule>,
    appendix_a: Vec<Primary>,
    #[serde(default)]
    appendix_b: Vec<Indication>,
    #[serde(default)]
    appendix_c: Vec<Association>,
}

#[derive(Debug)]
pub enum PolicyError {
    Toml(toml::de::Error),
    InvalidDates,
    InvalidThreshold,
    InvalidAppendix,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(error) => error.fmt(f),
            Self::InvalidDates => f.write_str("invalid WISeR policy dates"),
            Self::InvalidThreshold => f.write_str("invalid WISeR gold-card threshold"),
            Self::InvalidAppendix => f.write_str("invalid WISeR Appendix mapping"),
        }
    }
}

impl std::error::Error for PolicyError {}

impl Policy {
    pub fn washington_synthetic() -> Self {
        Self::from_toml(include_str!("../../../policy/wa_wiser.toml"))
            .expect("bundled Washington WISeR policy")
    }

    pub fn from_toml(input: &str) -> Result<Self, PolicyError> {
        let mut policy: Self = toml::from_str(input).map_err(PolicyError::Toml)?;
        if policy.request_start > policy.service_start
            || policy.service_start > policy.exemption_start
            || policy.exemption_start > policy.service_end
            || policy.participant.trim().is_empty()
        {
            return Err(PolicyError::InvalidDates);
        }
        policy.gold_card.sort_by_key(|rule| rule.effective);
        if !policy
            .gold_card
            .first()
            .is_some_and(|rule| rule.effective <= policy.request_start)
            || policy
                .gold_card
                .windows(2)
                .any(|rules| rules[0].effective == rules[1].effective)
            || policy
                .gold_card
                .iter()
                .any(|rule| rule.percent == 0 || rule.percent > 100 || rule.minimum_requests < 10)
        {
            return Err(PolicyError::InvalidThreshold);
        }
        let mut versions = BTreeSet::new();
        let codes: BTreeSet<_> = policy.appendix_a.iter().map(|row| row.code).collect();
        if codes.is_empty()
            || policy.appendix_a.iter().any(|row| {
                !versions.insert((row.code, row.effective.unwrap_or(policy.service_start)))
                    || row.requires_indication
                        && !policy
                            .appendix_b
                            .iter()
                            .any(|entry| entry.primary == row.code && !entry.excluded)
            })
            || policy
                .appendix_b
                .iter()
                .any(|row| !codes.contains(&row.primary) || row.diagnosis.trim().is_empty())
            || policy
                .appendix_c
                .iter()
                .any(|row| !codes.contains(&row.primary) || row.primary == row.associated)
        {
            return Err(PolicyError::InvalidAppendix);
        }
        Ok(policy)
    }

    pub fn gold_card_rule(&self, as_of: Date) -> Option<GoldCardRule> {
        self.gold_card
            .iter()
            .rev()
            .find(|rule| rule.effective <= as_of)
            .copied()
    }

    pub fn gold_card_eligible(
        &self,
        individual_npi: Npi,
        period: AssessmentPeriod,
        requests: &[PriorRequest<'_>],
    ) -> bool {
        let Some(rule) = self.gold_card_rule(period.eligibility_date) else {
            return false;
        };
        if period.from < self.request_start
            || period.from > period.through
            || period.through > period.eligibility_date
            || !(self.exemption_start..=self.service_end).contains(&period.eligibility_date)
        {
            return false;
        }
        let mut latest = BTreeMap::new();
        for entry in requests.iter().filter(|entry| {
            entry.npi == individual_npi
                && !entry.request_id.trim().is_empty()
                && (period.from..=period.through).contains(&entry.submitted)
        }) {
            let key = (entry.submitted, entry.revision);
            latest
                .entry(entry.request_id)
                .and_modify(|current: &mut ((Date, u32), Option<AssessmentOutcome>)| {
                    if key > current.0 {
                        *current = (key, Some(entry.outcome));
                    } else if key == current.0 && current.1 != Some(entry.outcome) {
                        current.1 = None;
                    }
                })
                .or_insert((key, Some(entry.outcome)));
        }
        let mut affirmed = 0u64;
        let mut count = 0u64;
        for (_, outcome) in latest.into_values() {
            match outcome {
                Some(AssessmentOutcome::Affirmed) => {
                    affirmed += 1;
                    count += 1;
                }
                Some(AssessmentOutcome::NonAffirmed) => count += 1,
                Some(AssessmentOutcome::Dismissed) => {}
                None => return false,
            }
        }
        count >= u64::from(rule.minimum_requests)
            && u128::from(affirmed) * 100 >= u128::from(count) * u128::from(rule.percent)
    }

    fn primary(&self, code: ProcedureCode, service_date: Date) -> Option<&Primary> {
        self.appendix_a
            .iter()
            .filter(|row| {
                row.code == code && row.effective.unwrap_or(self.service_start) <= service_date
            })
            .max_by_key(|row| row.effective.unwrap_or(self.service_start))
    }

    pub fn route(&self, service: &Service<'_>, stage: Stage) -> Route {
        if service.state != "WA"
            || service.plan != Plan::OriginalMedicare
            || service.emergency
            || service.inpatient_only
            || service.subject_to_other_cms_prior_authorization
            || service.provider_under_upic_review
            || (matches!(service.site, Site::HospitalOutpatient | Site::Asc)
                && !service.facility_claim)
            || !matches!(
                service.site,
                Site::HospitalOutpatient | Site::Asc | Site::Office | Site::Home
            )
            || !(self.service_start..=self.service_end).contains(&service.date)
        {
            return Route::OutOfScope;
        }
        let Some(row) = self.primary(service.code, service.date) else {
            return Route::OutOfScope;
        };
        match row.status {
            Status::Inactive => return Route::Inactive(row.category),
            Status::Delayed => return Route::Delayed(row.category),
            Status::Active => {}
        }
        if !row.sites.is_empty() && !row.sites.contains(&service.site) {
            return Route::OutOfScope;
        }
        let mut included = false;
        for entry in self
            .appendix_b
            .iter()
            .filter(|entry| entry.primary == service.code)
        {
            if service
                .diagnoses
                .iter()
                .any(|diagnosis| same_diagnosis(diagnosis, &entry.diagnosis))
            {
                if entry.excluded {
                    return Route::OutOfScope;
                }
                included = true;
            }
        }
        if row.requires_indication && !included {
            return Route::OutOfScope;
        }
        let mut pairs = self
            .appendix_c
            .iter()
            .filter(|entry| entry.primary == service.code && entry.required_pair)
            .peekable();
        if pairs.peek().is_some()
            && !pairs.any(|entry| service.same_claim_codes.contains(&entry.associated))
        {
            return Route::OutOfScope;
        }
        if let Some(exemption) = service.exemption {
            if service.individual_npi == Some(exemption.npi)
                && exemption.issuer == self.participant
                && !exemption.notice_id.trim().is_empty()
                && exemption.effective >= self.exemption_start
                && exemption.effective <= service.date
                && service.date <= exemption.through
            {
                return Route::Exempt(row.category);
            }
        }
        match stage {
            Stage::Planning {
                as_of,
                elect_prior_authorization,
            } if elect_prior_authorization
                && (self.request_start..=service.date).contains(&as_of) =>
            {
                Route::VoluntaryPriorAuthorization(row.category)
            }
            Stage::Claim(Some(decision)) => {
                Route::PriorAuthorizationDecision(row.category, decision)
            }
            Stage::Planning { .. } | Stage::Claim(None) => Route::PrePaymentReview(row.category),
        }
    }

    /// Pass the linked primary and associated lines from one encounter.
    pub fn associated_denials(&self, lines: &[AdjudicatedLine]) -> Vec<u32> {
        lines
            .iter()
            .filter(|line| {
                self.appendix_c.iter().any(|row| {
                    row.associated == line.code
                        && lines
                            .iter()
                            .any(|primary| primary.code == row.primary && primary.wiser_denied)
                })
            })
            .map(|line| line.sequence)
            .collect()
    }
}

/// Individual status is supplied by the caller from the provider record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Npi(u64);

impl Npi {
    pub fn new(value: &str) -> Option<Self> {
        (value.len() == 10 && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| Self(value.parse().expect("ten decimal digits fit in u64")))
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentPeriod {
    #[serde(deserialize_with = "date")]
    pub from: Date,
    #[serde(deserialize_with = "date")]
    pub through: Date,
    /// The eligibility date selects the threshold even when the assessment period ended earlier.
    #[serde(deserialize_with = "date")]
    pub eligibility_date: Date,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentOutcome {
    Affirmed,
    NonAffirmed,
    Dismissed,
}

#[derive(Clone, Copy, Debug)]
pub struct PriorRequest<'a> {
    pub npi: Npi,
    pub request_id: &'a str,
    pub submitted: Date,
    pub revision: u32,
    pub outcome: AssessmentOutcome,
}

/// Evidence from a verified participant notice supplied by the caller.
#[derive(Clone, Copy, Debug)]
pub struct Exemption<'a> {
    pub npi: Npi,
    pub issuer: &'a str,
    pub notice_id: &'a str,
    pub effective: Date,
    pub through: Date,
}

#[derive(Clone, Copy, Debug)]
pub struct Service<'a> {
    pub state: &'a str,
    pub plan: Plan,
    pub site: Site,
    pub date: Date,
    pub emergency: bool,
    pub inpatient_only: bool,
    pub subject_to_other_cms_prior_authorization: bool,
    pub provider_under_upic_review: bool,
    pub facility_claim: bool,
    pub code: ProcedureCode,
    pub diagnoses: &'a [&'a str],
    pub same_claim_codes: &'a [ProcedureCode],
    pub individual_npi: Option<Npi>,
    pub exemption: Option<Exemption<'a>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorDecision {
    ProvisionalAffirmation,
    NonAffirmation,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Stage {
    Planning {
        #[serde(deserialize_with = "date")]
        as_of: Date,
        elect_prior_authorization: bool,
    },
    Claim(Option<PriorDecision>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    OutOfScope,
    Inactive(Category),
    Delayed(Category),
    Exempt(Category),
    VoluntaryPriorAuthorization(Category),
    PrePaymentReview(Category),
    PriorAuthorizationDecision(Category, PriorDecision),
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjudicatedLine {
    pub sequence: u32,
    #[serde(deserialize_with = "procedure")]
    pub code: ProcedureCode,
    /// The primary was non-affirmed or denied in WISeR pre-payment review.
    pub wiser_denied: bool,
}

/// A valid UTN must be the one assigned to the authorization in the decision notice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Utn([u8; 14]);

impl Utn {
    pub fn new(value: &str) -> Option<Self> {
        let bytes: [u8; 14] = value.as_bytes().try_into().ok()?;
        bytes
            .iter()
            .all(u8::is_ascii_alphanumeric)
            .then_some(Self(bytes))
    }

    pub fn valid_on(self, decision: Date, service: Date) -> bool {
        (0..120).contains(&(service - decision).whole_days())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum UtnField<'a> {
    Electronic837 {
        loop_id: u16,
        qualifier: &'a str,
        ref02: &'a str,
    },
    AscFacility837 {
        loop_id: u16,
        qualifier: &'a str,
        ref02: &'a str,
    },
    PaperCms1500Item23(&'a str),
    AscFacilityCms1500Item23(&'a str),
    ElectronicInstitutionalTreatmentAuthorization(&'a str),
    ManualInstitutionalTreatmentAuthorization(&'a str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UtnError {
    OutsideValidity,
    WrongClaim,
    WrongPlacement,
    WrongUtn,
}

pub fn validate_utn(
    utn: Utn,
    decision: Date,
    service: Date,
    site: Site,
    field: UtnField<'_>,
) -> Result<(), UtnError> {
    if !utn.valid_on(decision, service) {
        return Err(UtnError::OutsideValidity);
    }
    let submitted = match field {
        UtnField::ElectronicInstitutionalTreatmentAuthorization(value)
            if site == Site::HospitalOutpatient =>
        {
            if value.len() > 18 {
                return Err(UtnError::WrongPlacement);
            }
            value.trim_end_matches(' ').as_bytes()
        }
        UtnField::ManualInstitutionalTreatmentAuthorization(value)
            if site == Site::HospitalOutpatient =>
        {
            if value.len() != 32 {
                return Err(UtnError::WrongPlacement);
            }
            let value = value.as_bytes();
            if value[..18].windows(14).any(|part| part == utn.0) {
                return Err(UtnError::WrongPlacement);
            }
            &value[18..32]
        }
        UtnField::Electronic837 {
            loop_id,
            qualifier,
            ref02,
        }
        | UtnField::AscFacility837 {
            loop_id,
            qualifier,
            ref02,
        } => {
            let facility = matches!(field, UtnField::AscFacility837 { .. });
            if !(facility && site == Site::Asc
                || !facility && matches!(site, Site::Office | Site::Home))
            {
                return Err(UtnError::WrongClaim);
            }
            if loop_id != 2300 || qualifier != "G1" {
                return Err(UtnError::WrongPlacement);
            }
            ref02.as_bytes()
        }
        UtnField::PaperCms1500Item23(value) if matches!(site, Site::Office | Site::Home) => {
            value.as_bytes().get(..14).ok_or(UtnError::WrongPlacement)?
        }
        UtnField::AscFacilityCms1500Item23(value) if site == Site::Asc => {
            value.as_bytes().get(..14).ok_or(UtnError::WrongPlacement)?
        }
        _ => return Err(UtnError::WrongClaim),
    };
    (submitted == utn.0).then_some(()).ok_or(UtnError::WrongUtn)
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    CervicalThoracic,
    LumbarSacral,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approach {
    Transforaminal,
    Interlaminar,
    Caudal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EsiIndication {
    RadicularSymptoms,
    PostLaminectomySyndrome,
    AcuteHerpesZoster,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConservativeCare {
    FailedAfterWeeks(u16),
    Intolerant,
    RefractoryZoster,
    Undocumented,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ImagingGuidance {
    CtOrFluoroscopyWithContrast,
    CtOrFluoroscopyWithoutContrast {
        documented_contraindication: bool,
    },
    UltrasoundWithoutContrast {
        documented_allergy_or_pregnancy: bool,
    },
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Laterality {
    None,
    Right,
    Left,
    Bilateral50,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EsiBilling {
    Professional,
    AscFacility,
    HospitalOutpatientFacility,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EsiLine {
    pub region: Region,
    pub level: u8,
    pub laterality: Laterality,
    pub units: u8,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EsiSession {
    #[serde(deserialize_with = "date")]
    pub date: Date,
    pub region: Region,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RepeatEsi {
    SustainedImprovement {
        percent: u8,
        months: u8,
        same_scale: bool,
    },
    PoorInitialResponse {
        #[serde(deserialize_with = "date")]
        initial_date: Date,
        changed_approach_level_or_drug: bool,
        documented_rationale: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct EsiEvidence<'a> {
    pub date: Date,
    pub region: Region,
    pub approach: Approach,
    pub indication: EsiIndication,
    pub supporting_exam_and_imaging: bool,
    pub functional_impact_and_baseline: bool,
    pub pain_weeks: u16,
    pub conservative_care: ConservativeCare,
    pub active_rehabilitation: bool,
    pub guidance: ImagingGuidance,
    pub separately_billed_guidance: bool,
    pub bilateral_indicated: bool,
    pub billing: EsiBilling,
    pub lines: &'a [EsiLine],
    /// Earlier sessions for this beneficiary, across claims.
    pub history: &'a [EsiSession],
    pub repeat: Option<RepeatEsi>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EsiFinding {
    Indication,
    ConservativeCare,
    Imaging,
    SeparatelyBilledGuidance,
    Frequency,
    Region,
    Levels,
    BilateralBilling,
    RepeatEvidence,
}

fn year_earlier(date: Date) -> Option<Date> {
    let day = if date.month() == Month::February && date.day() == 29 {
        28
    } else {
        date.day()
    };
    Date::from_calendar_date(date.year() - 1, date.month(), day).ok()
}

pub fn evaluate_esi(evidence: &EsiEvidence<'_>) -> Vec<EsiFinding> {
    use EsiFinding as F;
    let mut findings = Vec::new();
    let zoster = evidence.indication == EsiIndication::AcuteHerpesZoster;
    if evidence.indication == EsiIndication::Other
        || !evidence.supporting_exam_and_imaging
        || !evidence.functional_impact_and_baseline
    {
        findings.push(F::Indication);
    }
    let conservative = match evidence.conservative_care {
        ConservativeCare::FailedAfterWeeks(weeks) => evidence.pain_weeks >= 4 && weeks >= 4,
        ConservativeCare::Intolerant => evidence.pain_weeks >= 4,
        ConservativeCare::RefractoryZoster => zoster,
        ConservativeCare::Undocumented => false,
    };
    if !conservative || !evidence.active_rehabilitation {
        findings.push(F::ConservativeCare);
    }
    if !matches!(
        evidence.guidance,
        ImagingGuidance::CtOrFluoroscopyWithContrast
            | ImagingGuidance::CtOrFluoroscopyWithoutContrast {
                documented_contraindication: true
            }
            | ImagingGuidance::UltrasoundWithoutContrast {
                documented_allergy_or_pregnancy: true
            }
    ) {
        findings.push(F::Imaging);
    }
    if evidence.separately_billed_guidance {
        findings.push(F::SeparatelyBilledGuidance);
    }
    let cutoff = year_earlier(evidence.date);
    let prior_dates: BTreeSet<_> = evidence
        .history
        .iter()
        .filter(|session| {
            session.region == evidence.region
                && session.date <= evidence.date
                && cutoff.is_none_or(|start| session.date > start)
        })
        .map(|session| session.date)
        .collect();
    if prior_dates.len() >= 4 || prior_dates.contains(&evidence.date) {
        findings.push(F::Frequency);
    }
    if evidence
        .lines
        .iter()
        .any(|line| line.region != evidence.region)
        || evidence
            .history
            .iter()
            .any(|session| session.date == evidence.date && session.region != evidence.region)
    {
        findings.push(F::Region);
    }
    let levels: BTreeSet<_> = evidence.lines.iter().map(|line| line.level).collect();
    let transforaminal = evidence.approach == Approach::Transforaminal;
    if levels.is_empty() || levels.contains(&0) || levels.len() > if transforaminal { 2 } else { 1 }
    {
        findings.push(F::Levels);
    }
    for level in levels {
        let lines: Vec<_> = evidence
            .lines
            .iter()
            .filter(|line| line.level == level)
            .map(|line| line.laterality)
            .collect();
        let valid = match lines.as_slice() {
            [Laterality::None | Laterality::Right | Laterality::Left] => true,
            [Laterality::Bilateral50] => {
                transforaminal
                    && evidence.bilateral_indicated
                    && evidence.billing != EsiBilling::AscFacility
            }
            [Laterality::Right, Laterality::Left] | [Laterality::Left, Laterality::Right] => {
                transforaminal
                    && evidence.bilateral_indicated
                    && evidence.billing == EsiBilling::AscFacility
            }
            _ => false,
        };
        if !valid
            || evidence
                .lines
                .iter()
                .any(|line| line.level == level && line.units != 1)
        {
            findings.push(F::BilateralBilling);
            break;
        }
    }
    if (!prior_dates.is_empty() || evidence.repeat.is_some())
        && !match evidence.repeat {
            Some(RepeatEsi::SustainedImprovement {
                percent,
                months,
                same_scale,
            }) => (50..=100).contains(&percent) && months >= 3 && same_scale,
            Some(RepeatEsi::PoorInitialResponse {
                initial_date,
                changed_approach_level_or_drug,
                documented_rationale,
            }) => {
                evidence.date - initial_date >= Duration::days(14)
                    && changed_approach_level_or_drug
                    && documented_rationale
            }
            None => false,
        }
    {
        findings.push(F::RepeatEvidence);
    }
    findings
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instability {
    /// Translation or subluxation on lateral or dynamic radiographs, in tenths of a millimeter.
    pub translation_tenths_mm: u16,
    pub angulation_tenths_deg: u16,
}

impl Instability {
    pub fn present(self) -> bool {
        self.translation_tenths_mm > 35 || self.angulation_tenths_deg > 110
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stenosis {
    Central,
    LateralRecess,
    Foraminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImagingCause {
    DegenerativeDiscDisease,
    Tumor,
    PostInfection,
    CongenitalShortPedicles,
    OssifiedPosteriorLongitudinalLigament,
    CordCompression,
    Instability,
    Other,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neurology {
    pub myelopathy_class: u8,
    pub progressive_deficit_during_care: bool,
    pub progressive_motor_weakness: bool,
    pub motor_weakness_limits_adls: bool,
    pub cord_related_sphincter_loss: bool,
    pub gait_dexterity_or_sphincter_symptoms: bool,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Deformity {
    QualifyingCervicalDeformity,
    SymptomaticPseudarthrosis {
        imaging_nonunion: bool,
        months_since_fusion: u16,
        imaging_hardware_failure: bool,
    },
    PostLaminectomy,
    RheumatoidArthritis,
    DegenerativeSpondylolisthesis,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FusionIndication {
    NerveRootImpingement,
    SymptomaticCanalStenosis,
    Trauma {
        injury_documented: bool,
        likely_instability: bool,
        concordant_neurologic_deficit: bool,
    },
    Tumor {
        documented: bool,
        deficit: bool,
        stabilization_needed: bool,
        treatment_likely_to_destabilize_or_cause_deficit: bool,
    },
    Infection {
        confirmed: bool,
        vertebral_destruction: bool,
        debridement_destabilizes: bool,
    },
    Deformity {
        kind: Deformity,
        substantial_functional_limitation: bool,
        progressing: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FusionEvidence {
    pub indication: FusionIndication,
    pub arm_pain_vas: u8,
    pub arm_pain_weeks: u16,
    pub failed_multimodal_care: bool,
    pub limits_adls: bool,
    pub other_causes_excluded: bool,
    pub isolated_axial_pain: bool,
    pub mri_or_ct_level_matches: bool,
    pub stenosis: Option<Stenosis>,
    pub imaging_cause: ImagingCause,
    pub instability: Instability,
    pub neurology: Neurology,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionFinding {
    Symptoms,
    ConservativeCare,
    OtherCauses,
    Imaging,
    IndicationEvidence,
}

pub fn evaluate_fusion(evidence: FusionEvidence) -> Vec<FusionFinding> {
    use FusionFinding as F;
    let instability = evidence.instability.present();
    let neuro = evidence.neurology;
    let conservative = (4..=10).contains(&evidence.arm_pain_vas)
        && evidence.arm_pain_weeks >= 12
        && evidence.failed_multimodal_care;
    let exception = neuro.myelopathy_class >= 3
        || neuro.progressive_deficit_during_care
        || neuro.progressive_motor_weakness
        || neuro.motor_weakness_limits_adls
        || neuro.cord_related_sphincter_loss
        || ((7..=10).contains(&evidence.arm_pain_vas)
            && evidence.limits_adls
            && evidence.mri_or_ct_level_matches);
    let mut findings = Vec::new();
    match evidence.indication {
        FusionIndication::NerveRootImpingement | FusionIndication::SymptomaticCanalStenosis => {
            let canal = matches!(
                evidence.indication,
                FusionIndication::SymptomaticCanalStenosis
            );
            let imaging_cause = match evidence.imaging_cause {
                ImagingCause::DegenerativeDiscDisease
                | ImagingCause::Tumor
                | ImagingCause::PostInfection => true,
                ImagingCause::CongenitalShortPedicles
                | ImagingCause::OssifiedPosteriorLongitudinalLigament
                | ImagingCause::CordCompression => canal,
                ImagingCause::Instability => instability,
                ImagingCause::Other => false,
            };
            if evidence.isolated_axial_pain || (!canal && !evidence.limits_adls) {
                findings.push(F::Symptoms);
            }
            if !(conservative
                || exception
                || (canal && (evidence.limits_adls || neuro.gait_dexterity_or_sphincter_symptoms)))
            {
                findings.push(F::ConservativeCare);
            }
            if !evidence.other_causes_excluded {
                findings.push(F::OtherCauses);
            }
            if !evidence.mri_or_ct_level_matches
                || !imaging_cause
                || evidence.stenosis.is_none()
                || (canal && evidence.stenosis != Some(Stenosis::Central))
            {
                findings.push(F::Imaging);
            }
        }
        indication => {
            let supported = match indication {
                FusionIndication::Trauma {
                    injury_documented,
                    likely_instability,
                    concordant_neurologic_deficit,
                } => {
                    injury_documented
                        && (instability || likely_instability || concordant_neurologic_deficit)
                }
                FusionIndication::Tumor {
                    documented,
                    deficit,
                    stabilization_needed,
                    treatment_likely_to_destabilize_or_cause_deficit,
                } => {
                    documented
                        && (instability
                            || (deficit && stabilization_needed)
                            || treatment_likely_to_destabilize_or_cause_deficit)
                }
                FusionIndication::Infection {
                    confirmed,
                    vertebral_destruction,
                    debridement_destabilizes,
                } => {
                    confirmed && (instability || vertebral_destruction || debridement_destabilizes)
                }
                FusionIndication::Deformity {
                    kind,
                    substantial_functional_limitation,
                    progressing,
                } => match kind {
                    Deformity::QualifyingCervicalDeformity => true,
                    Deformity::SymptomaticPseudarthrosis {
                        imaging_nonunion,
                        months_since_fusion,
                        imaging_hardware_failure,
                    } => {
                        imaging_hardware_failure || (imaging_nonunion && months_since_fusion >= 12)
                    }
                    Deformity::PostLaminectomy | Deformity::RheumatoidArthritis => instability,
                    Deformity::DegenerativeSpondylolisthesis => {
                        instability && substantial_functional_limitation
                    }
                    Deformity::Other => progressing,
                },
                FusionIndication::NerveRootImpingement
                | FusionIndication::SymptomaticCanalStenosis => unreachable!(),
            };
            if !supported {
                findings.push(F::IndicationEvidence);
            }
        }
    }
    findings
}
