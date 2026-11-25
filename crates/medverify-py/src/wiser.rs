use crate::{extract, output, state};
use medverify_core::{claim::ProcedureCode, wiser as core};
use pyo3::{exceptions::PyValueError, prelude::*};
use serde::{Deserialize, Serialize};
use time::{macros::format_description, Date};

fn date(value: &str) -> PyResult<Date> {
    Date::parse(value, format_description!("[year]-[month]-[day]"))
        .map_err(|_| PyValueError::new_err("expected a YYYY-MM-DD date"))
}

fn npi(value: &str) -> PyResult<core::Npi> {
    core::Npi::new(value).ok_or_else(|| PyValueError::new_err("NPI must be ten ASCII digits"))
}

fn procedure(value: &str) -> PyResult<ProcedureCode> {
    ProcedureCode::new(value)
        .ok_or_else(|| PyValueError::new_err("procedure code must be five ASCII letters or digits"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PriorRequest {
    npi: String,
    request_id: String,
    submitted: String,
    revision: u32,
    outcome: core::AssessmentOutcome,
}

impl PriorRequest {
    fn as_request(&self) -> PyResult<core::PriorRequest<'_>> {
        Ok(core::PriorRequest {
            npi: npi(&self.npi)?,
            request_id: &self.request_id,
            submitted: date(&self.submitted)?,
            revision: self.revision,
            outcome: self.outcome,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exemption {
    npi: String,
    issuer: String,
    notice_id: String,
    effective: String,
    through: String,
}

impl Exemption {
    fn as_exemption(&self) -> PyResult<core::Exemption<'_>> {
        Ok(core::Exemption {
            npi: npi(&self.npi)?,
            issuer: &self.issuer,
            notice_id: &self.notice_id,
            effective: date(&self.effective)?,
            through: date(&self.through)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Service {
    state: String,
    plan: core::Plan,
    site: core::Site,
    date: String,
    #[serde(default)]
    emergency: bool,
    #[serde(default)]
    inpatient_only: bool,
    #[serde(default)]
    subject_to_other_cms_prior_authorization: bool,
    #[serde(default)]
    provider_under_upic_review: bool,
    #[serde(default)]
    facility_claim: bool,
    code: String,
    #[serde(default)]
    diagnoses: Vec<String>,
    #[serde(default)]
    same_claim_codes: Vec<String>,
    individual_npi: Option<String>,
    exemption: Option<Exemption>,
}

impl Service {
    fn route(&self, policy: &core::Policy, stage: core::Stage) -> PyResult<core::Route> {
        let diagnoses: Vec<_> = self.diagnoses.iter().map(String::as_str).collect();
        let same_claim_codes = self
            .same_claim_codes
            .iter()
            .map(|code| procedure(code))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(policy.route(
            &core::Service {
                state: state(&self.state)?,
                plan: self.plan,
                site: self.site,
                date: date(&self.date)?,
                emergency: self.emergency,
                inpatient_only: self.inpatient_only,
                subject_to_other_cms_prior_authorization: self
                    .subject_to_other_cms_prior_authorization,
                provider_under_upic_review: self.provider_under_upic_review,
                facility_claim: self.facility_claim,
                code: procedure(&self.code)?,
                diagnoses: &diagnoses,
                same_claim_codes: &same_claim_codes,
                individual_npi: self.individual_npi.as_deref().map(npi).transpose()?,
                exemption: self
                    .exemption
                    .as_ref()
                    .map(Exemption::as_exemption)
                    .transpose()?,
            },
            stage,
        ))
    }
}

#[derive(Serialize)]
struct Route {
    kind: &'static str,
    category: Option<core::Category>,
    decision: Option<core::PriorDecision>,
}

impl From<core::Route> for Route {
    fn from(route: core::Route) -> Self {
        use core::Route::*;
        let (kind, category, decision) = match route {
            OutOfScope => ("out_of_scope", None, None),
            Inactive(category) => ("inactive", Some(category), None),
            Delayed(category) => ("delayed", Some(category), None),
            Exempt(category) => ("exempt", Some(category), None),
            VoluntaryPriorAuthorization(category) => {
                ("voluntary_prior_authorization", Some(category), None)
            }
            PrePaymentReview(category) => ("pre_payment_review", Some(category), None),
            PriorAuthorizationDecision(category, decision) => (
                "prior_authorization_decision",
                Some(category),
                Some(decision),
            ),
        };
        Self {
            kind,
            category,
            decision,
        }
    }
}

#[pyclass(frozen, module = "medverify.wiser")]
struct Policy(core::Policy);

#[pymethods]
impl Policy {
    /// Load the bundled Washington rules and synthetic procedure mappings.
    #[staticmethod]
    fn washington_synthetic() -> Self {
        Self(core::Policy::washington_synthetic())
    }

    /// Load a policy with operator-supplied Appendix mappings.
    #[staticmethod]
    fn from_toml(text: &str) -> PyResult<Self> {
        core::Policy::from_toml(text).map(Self).map_err(|error| {
            let message = match error {
                core::PolicyError::Toml(_) => "invalid WISeR policy TOML".to_owned(),
                other => other.to_string(),
            };
            PyValueError::new_err(message)
        })
    }

    fn route<'py>(
        &self,
        py: Python<'py>,
        service: &Bound<'_, PyAny>,
        stage: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let service: Service = extract(service, "WISeR service")?;
        let stage = extract(stage, "WISeR stage")?;
        output(py, Route::from(service.route(&self.0, stage)?))
    }

    fn gold_card_rule<'py>(&self, py: Python<'py>, as_of: &str) -> PyResult<Bound<'py, PyAny>> {
        output(py, self.0.gold_card_rule(date(as_of)?))
    }

    fn gold_card_eligible(
        &self,
        py: Python<'_>,
        individual_npi: &str,
        period: &Bound<'_, PyAny>,
        requests: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        let individual_npi = npi(individual_npi)?;
        let period = extract(period, "WISeR assessment period")?;
        let requests: Vec<PriorRequest> = extract(requests, "WISeR prior requests")?;
        py.detach(|| {
            let requests = requests
                .iter()
                .map(PriorRequest::as_request)
                .collect::<PyResult<Vec<_>>>()?;
            Ok(self.0.gold_card_eligible(individual_npi, period, &requests))
        })
    }

    fn associated_denials(&self, py: Python<'_>, lines: &Bound<'_, PyAny>) -> PyResult<Vec<u32>> {
        let lines: Vec<core::AdjudicatedLine> = extract(lines, "WISeR adjudicated lines")?;
        Ok(py.detach(|| self.0.associated_denials(&lines)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EsiEvidence {
    date: String,
    region: core::Region,
    approach: core::Approach,
    indication: core::EsiIndication,
    supporting_exam_and_imaging: bool,
    functional_impact_and_baseline: bool,
    pain_weeks: u16,
    conservative_care: core::ConservativeCare,
    active_rehabilitation: bool,
    guidance: core::ImagingGuidance,
    separately_billed_guidance: bool,
    bilateral_indicated: bool,
    billing: core::EsiBilling,
    lines: Vec<core::EsiLine>,
    history: Vec<core::EsiSession>,
    repeat: Option<core::RepeatEsi>,
}

#[pyfunction]
fn evaluate_esi<'py>(py: Python<'py>, evidence: &Bound<'_, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let input: EsiEvidence = extract(evidence, "WISeR ESI evidence")?;
    let evidence = core::EsiEvidence {
        date: date(&input.date)?,
        region: input.region,
        approach: input.approach,
        indication: input.indication,
        supporting_exam_and_imaging: input.supporting_exam_and_imaging,
        functional_impact_and_baseline: input.functional_impact_and_baseline,
        pain_weeks: input.pain_weeks,
        conservative_care: input.conservative_care,
        active_rehabilitation: input.active_rehabilitation,
        guidance: input.guidance,
        separately_billed_guidance: input.separately_billed_guidance,
        bilateral_indicated: input.bilateral_indicated,
        billing: input.billing,
        lines: &input.lines,
        history: &input.history,
        repeat: input.repeat,
    };
    output(py, py.detach(|| core::evaluate_esi(&evidence)))
}

#[pyfunction]
fn evaluate_fusion<'py>(
    py: Python<'py>,
    evidence: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let evidence = extract(evidence, "WISeR fusion evidence")?;
    output(py, core::evaluate_fusion(evidence))
}

#[pyfunction]
fn validate_utn<'py>(
    py: Python<'py>,
    utn: &str,
    decision: &str,
    service: &str,
    site: &Bound<'_, PyAny>,
    field: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let utn = core::Utn::new(utn)
        .ok_or_else(|| PyValueError::new_err("UTN must be fourteen ASCII letters or digits"))?;
    output(
        py,
        core::validate_utn(
            utn,
            date(decision)?,
            date(service)?,
            extract(site, "WISeR site")?,
            extract::<UtnField>(field, "WISeR UTN field")?.as_field(),
        )
        .err(),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum UtnField {
    Electronic837 {
        loop_id: u16,
        qualifier: String,
        ref02: String,
    },
    AscFacility837 {
        loop_id: u16,
        qualifier: String,
        ref02: String,
    },
    PaperCms1500Item23(String),
    AscFacilityCms1500Item23(String),
    ElectronicInstitutionalTreatmentAuthorization(String),
    ManualInstitutionalTreatmentAuthorization(String),
}

impl UtnField {
    fn as_field(&self) -> core::UtnField<'_> {
        use core::UtnField as Field;
        match self {
            Self::Electronic837 {
                loop_id,
                qualifier,
                ref02,
            } => Field::Electronic837 {
                loop_id: *loop_id,
                qualifier,
                ref02,
            },
            Self::AscFacility837 {
                loop_id,
                qualifier,
                ref02,
            } => Field::AscFacility837 {
                loop_id: *loop_id,
                qualifier,
                ref02,
            },
            Self::PaperCms1500Item23(value) => Field::PaperCms1500Item23(value),
            Self::AscFacilityCms1500Item23(value) => Field::AscFacilityCms1500Item23(value),
            Self::ElectronicInstitutionalTreatmentAuthorization(value) => {
                Field::ElectronicInstitutionalTreatmentAuthorization(value)
            }
            Self::ManualInstitutionalTreatmentAuthorization(value) => {
                Field::ManualInstitutionalTreatmentAuthorization(value)
            }
        }
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Policy>()?;
    module.add_function(wrap_pyfunction!(evaluate_esi, module)?)?;
    module.add_function(wrap_pyfunction!(evaluate_fusion, module)?)?;
    module.add_function(wrap_pyfunction!(validate_utn, module)?)
}
