use medverify_core::claim::{self, Setting};
use medverify_core::mue::{MueFinding, MueScope};
use medverify_core::ncci::{ModifierIssueKind, PtpFinding, PtpOutcome};
use pyo3::IntoPyObject;

#[derive(IntoPyObject)]
struct ServiceDay {
    beneficiary: String,
    billing_provider: String,
    date: String,
    setting: &'static str,
}

impl From<claim::ServiceDay<'_>> for ServiceDay {
    fn from(day: claim::ServiceDay<'_>) -> Self {
        Self {
            beneficiary: day.beneficiary.to_owned(),
            billing_provider: day.billing_provider.to_owned(),
            date: day.date.date().to_string(),
            setting: match day.setting {
                Setting::Practitioner => "practitioner",
                Setting::HospitalOutpatient => "hospital",
                Setting::DmeSupplier => "dme",
            },
        }
    }
}

#[derive(IntoPyObject)]
struct ModifierIssue {
    line_id: u64,
    modifier: String,
    kind: &'static str,
    required_modifier: Option<String>,
}

#[derive(IntoPyObject)]
pub struct Pair {
    service_day: ServiceDay,
    column_one_line_id: u64,
    column_two_line_id: u64,
    outcome: &'static str,
    modifier_issues: Vec<ModifierIssue>,
    abn_cannot_shift_liability: bool,
}

fn ascii(bytes: impl AsRef<[u8]>) -> String {
    str::from_utf8(bytes.as_ref())
        .expect("validated ASCII code")
        .to_owned()
}

impl From<PtpFinding<'_>> for Pair {
    fn from(f: PtpFinding<'_>) -> Self {
        Self {
            service_day: f.service_day.into(),
            column_one_line_id: f.column_one_line_id,
            column_two_line_id: f.column_two_line_id,
            outcome: match f.outcome {
                PtpOutcome::Bypassed => "bypassed",
                PtpOutcome::ModifierProhibited => "modifier_prohibited",
                PtpOutcome::SupportedModifierRequired => "supported_modifier_required",
            },
            modifier_issues: f
                .modifier_issues
                .into_iter()
                .map(|issue| {
                    let (kind, required_modifier) = match issue.kind {
                        ModifierIssueKind::MissingEvidence => ("missing_evidence", None),
                        ModifierIssueKind::WrongService => ("wrong_service", None),
                        ModifierIssueKind::MoreSpecificModifierRequired(m) => {
                            ("more_specific_modifier_required", Some(ascii(m.bytes())))
                        }
                    };
                    ModifierIssue {
                        line_id: issue.line_id,
                        modifier: ascii(issue.modifier.bytes()),
                        kind,
                        required_modifier,
                    }
                })
                .collect(),
            abn_cannot_shift_liability: f.abn_cannot_shift_liability,
        }
    }
}

#[derive(IntoPyObject)]
pub struct Limit {
    service_day: ServiceDay,
    code: String,
    scope: &'static str,
    line_id: Option<u64>,
    claim_id: Option<String>,
    max_units: u16,
    total_units: u64,
    abn_cannot_shift_liability: bool,
}

impl From<MueFinding<'_>> for Limit {
    fn from(f: MueFinding<'_>) -> Self {
        let (scope, line_id, claim_id) = match f.scope {
            MueScope::ClaimLine { line_id } => ("claim_line", Some(line_id), None),
            MueScope::UnsupportedSplit { line_id, claim_id } => (
                "unsupported_split",
                Some(line_id),
                Some(claim_id.to_owned()),
            ),
            MueScope::PossibleUnsupportedSplit { line_id } => {
                ("possible_unsupported_split", Some(line_id), None)
            }
            MueScope::AbsoluteServiceDayLimit => ("absolute_service_day_limit", None, None),
            MueScope::ServiceDayClinicalReview => ("service_day_clinical_review", None, None),
        };
        Self {
            service_day: f.service_day.into(),
            code: ascii(f.code.bytes()),
            scope,
            line_id,
            claim_id,
            max_units: f.max_units,
            total_units: f.total_units,
            abn_cannot_shift_liability: f.abn_cannot_shift_liability,
        }
    }
}

#[derive(IntoPyObject)]
pub struct Findings {
    pub ptp: Vec<Pair>,
    pub mue: Vec<Limit>,
}
