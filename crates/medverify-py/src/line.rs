use medverify_core::claim::{
    self, EvidenceBasis, Modifier, ProcedureCode, ServiceDate, ServiceDay, ServiceKind, Setting,
};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::num::NonZeroU64;
use time::{macros::format_description, Date};

fn modifier(value: &str) -> PyResult<Modifier> {
    Modifier::new(value)
        .ok_or_else(|| PyValueError::new_err("modifier must be two ASCII letters or digits"))
}

#[pyclass(frozen, from_py_object, module = "medverify._medverify")]
#[derive(Clone)]
pub struct ModifierEvidence(claim::ModifierEvidence);

#[pymethods]
impl ModifierEvidence {
    #[new]
    fn new(related_line_id: u64, modifier: &str, basis: &str) -> PyResult<Self> {
        use EvidenceBasis::*;
        let basis = match basis {
            "anatomic_site" => AnatomicSite,
            "unrelated_postoperative_em" => UnrelatedPostoperativeEm,
            "significant_separate_em" => SignificantSeparateEm,
            "separate_hospital_em_encounter" => SeparateHospitalEmEncounter,
            "decision_for_major_surgery" => DecisionForMajorSurgery,
            "staged_or_related_postoperative_procedure" => StagedOrRelatedPostoperativeProcedure,
            "unplanned_related_return_to_operating_room" => UnplannedRelatedReturnToOperatingRoom,
            "unrelated_postoperative_procedure" => UnrelatedPostoperativeProcedure,
            "repeat_clinical_laboratory_test" => RepeatClinicalLaboratoryTest,
            "separate_encounter" => SeparateEncounter,
            "separate_practitioner" => SeparatePractitioner,
            "separate_structure" => SeparateStructure,
            "non_overlapping_service" => NonOverlappingService,
            "distinct_service" => DistinctService,
            "repeat_procedure_same_practitioner" => RepeatProcedureSamePractitioner,
            "repeat_procedure_other_practitioner" => RepeatProcedureOtherPractitioner,
            _ => return Err(PyValueError::new_err("unknown modifier evidence basis")),
        };
        Ok(Self(claim::ModifierEvidence {
            related_line_id,
            modifier: self::modifier(modifier)?,
            basis,
        }))
    }
}

#[pyclass(frozen, from_py_object, module = "medverify._medverify")]
#[derive(Clone)]
pub struct ClaimLine {
    id: u64,
    beneficiary: String,
    billing_provider: String,
    date: ServiceDate,
    setting: Setting,
    code: ProcedureCode,
    units: NonZeroU64,
    service_kind: ServiceKind,
    modifiers: Vec<Modifier>,
    modifier_evidence: Vec<claim::ModifierEvidence>,
    abn_liability_shift_attempted: bool,
    claim_id: Option<String>,
}

#[pymethods]
impl ClaimLine {
    #[new]
    #[pyo3(signature = (*, id, beneficiary, billing_provider, date, setting, code, units, service_kind="other", modifiers=None, modifier_evidence=None, abn_liability_shift_attempted=false, claim_id=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: u64,
        beneficiary: String,
        billing_provider: String,
        date: &str,
        setting: &str,
        code: &str,
        units: u64,
        service_kind: &str,
        modifiers: Option<Vec<String>>,
        modifier_evidence: Option<Vec<ModifierEvidence>>,
        abn_liability_shift_attempted: bool,
        claim_id: Option<String>,
    ) -> PyResult<Self> {
        if beneficiary.trim().is_empty() || billing_provider.trim().is_empty() {
            return Err(PyValueError::new_err(
                "beneficiary and billing_provider must be nonempty",
            ));
        }
        if claim_id.as_ref().is_some_and(|id| id.trim().is_empty()) {
            return Err(PyValueError::new_err("claim_id must be nonempty"));
        }
        let date = Date::parse(date, format_description!("[year]-[month]-[day]"))
            .ok()
            .and_then(ServiceDate::new)
            .ok_or_else(|| {
                PyValueError::new_err("expected an encodable YYYY-MM-DD service date")
            })?;
        let setting = match setting {
            "practitioner" => Setting::Practitioner,
            "hospital" => Setting::HospitalOutpatient,
            "dme" => Setting::DmeSupplier,
            _ => return Err(PyValueError::new_err("unknown setting")),
        };
        let service_kind = match service_kind {
            "other" => ServiceKind::Other,
            "evaluation_and_management" => ServiceKind::EvaluationAndManagement,
            _ => return Err(PyValueError::new_err("unknown service kind")),
        };
        Ok(Self {
            id,
            beneficiary,
            billing_provider,
            date,
            setting,
            code: ProcedureCode::new(code).ok_or_else(|| {
                PyValueError::new_err("procedure code must be five ASCII letters or digits")
            })?,
            units: NonZeroU64::new(units)
                .ok_or_else(|| PyValueError::new_err("units must be positive"))?,
            service_kind,
            modifiers: modifiers
                .unwrap_or_default()
                .iter()
                .map(|s| modifier(s))
                .collect::<PyResult<_>>()?,
            modifier_evidence: modifier_evidence
                .unwrap_or_default()
                .into_iter()
                .map(|e| e.0)
                .collect(),
            abn_liability_shift_attempted,
            claim_id,
        })
    }
}

impl ClaimLine {
    pub fn as_line(&self) -> claim::ClaimLine<'_> {
        claim::ClaimLine {
            id: self.id,
            service_day: ServiceDay {
                beneficiary: &self.beneficiary,
                billing_provider: &self.billing_provider,
                date: self.date,
                setting: self.setting,
            },
            code: self.code,
            units: self.units,
            service_kind: self.service_kind,
            modifiers: &self.modifiers,
            modifier_evidence: &self.modifier_evidence,
            abn_liability_shift_attempted: self.abn_liability_shift_attempted,
            claim_id: self.claim_id.as_deref(),
        }
    }
}
