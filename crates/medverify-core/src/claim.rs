use crate::encoding::{decode_epoch_day, encode_code_b36, encode_epoch_day};
use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use time::Date;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
pub struct ProcedureCode(pub(crate) u32);

impl ProcedureCode {
    pub fn new(code: &str) -> Option<Self> {
        encode_code_b36(code).map(Self)
    }

    pub fn bytes(self) -> [u8; 5] {
        crate::encoding::decode_code_b36(self.0).expect("validated procedure code")
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
pub struct ServiceDate(pub(crate) u16);

impl ServiceDate {
    pub fn new(date: Date) -> Option<Self> {
        encode_epoch_day(date).map(Self)
    }

    pub fn date(self) -> Date {
        decode_epoch_day(self.0).expect("validated service date")
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
pub enum Setting {
    Practitioner = 1,
    HospitalOutpatient = 2,
    DmeSupplier = 4,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
pub struct ServiceDay<'a> {
    pub beneficiary: &'a str,
    pub billing_provider: &'a str,
    pub date: ServiceDate,
    pub setting: Setting,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
pub enum ServiceKind {
    EvaluationAndManagement,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize)]
pub struct Modifier(pub(crate) [u8; 2]);

impl Modifier {
    pub fn new(code: &str) -> Option<Self> {
        let bytes: [u8; 2] = code.as_bytes().try_into().ok()?;
        bytes
            .iter()
            .all(u8::is_ascii_alphanumeric)
            .then(|| Self(bytes.map(|byte| byte.to_ascii_uppercase())))
    }

    pub fn bytes(self) -> [u8; 2] {
        self.0
    }
}

/// Clinical or coding facts verified by the caller for a specific line pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum EvidenceBasis {
    AnatomicSite,
    UnrelatedPostoperativeEm,
    SignificantSeparateEm,
    SeparateHospitalEmEncounter,
    DecisionForMajorSurgery,
    StagedOrRelatedPostoperativeProcedure,
    UnplannedRelatedReturnToOperatingRoom,
    UnrelatedPostoperativeProcedure,
    RepeatClinicalLaboratoryTest,
    SeparateEncounter,
    SeparatePractitioner,
    SeparateStructure,
    NonOverlappingService,
    /// A documented distinct service where policy permits a generic modifier.
    DistinctService,
    /// A repeat of the same procedure by the same practitioner on the same day (modifier 76).
    RepeatProcedureSamePractitioner,
    /// A repeat of the same procedure by a different practitioner on the same day (modifier 77).
    RepeatProcedureOtherPractitioner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ModifierEvidence {
    pub related_line_id: u64,
    pub modifier: Modifier,
    pub basis: EvidenceBasis,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ClaimLine<'a> {
    /// Unique within the service-day group, including when evaluating multiple claims.
    pub id: u64,
    pub service_day: ServiceDay<'a>,
    pub code: ProcedureCode,
    pub service_kind: ServiceKind,
    pub units: NonZeroU64,
    pub modifiers: &'a [Modifier],
    /// Include documented applicable modifiers here even if they were omitted from the bill.
    pub modifier_evidence: &'a [ModifierEvidence],
    pub abn_liability_shift_attempted: bool,
    /// Opaque identifier of the claim that carries the line; `None` when claim boundaries are unknown.
    pub claim_id: Option<&'a str>,
}

/// Whether a modifier may be reported on this kind of service. PTP bypass and MUE repeat
/// reporting both use this one rule: `59` never describes an E/M service, and `24`, `25`, `27`
/// and `57` describe only E/M services.
pub(crate) fn fits_service(modifier: Modifier, line: &ClaimLine<'_>) -> bool {
    let em = line.service_kind == ServiceKind::EvaluationAndManagement;
    match &modifier.0 {
        b"24" | b"25" | b"57" => em,
        b"27" => em && line.service_day.setting == Setting::HospitalOutpatient,
        b"59" => !em,
        _ => true,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvaluationError<'a> {
    DuplicateLineId {
        service_day: ServiceDay<'a>,
        line_id: u64,
    },
    UnitTotalOverflow {
        service_day: ServiceDay<'a>,
        code: ProcedureCode,
    },
}

impl fmt::Display for EvaluationError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateLineId { line_id, .. } => write!(f, "duplicate claim line ID {line_id}"),
            Self::UnitTotalOverflow { .. } => f.write_str("service-day unit total exceeds u64"),
        }
    }
}

impl std::error::Error for EvaluationError<'_> {}

type ServiceGroups<'a, 'lines> = BTreeMap<ServiceDay<'a>, Vec<&'lines ClaimLine<'a>>>;

pub(crate) fn group_lines<'a, 'lines>(
    lines: &'lines [ClaimLine<'a>],
) -> Result<ServiceGroups<'a, 'lines>, EvaluationError<'a>> {
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for line in lines {
        groups.entry(line.service_day).or_default().push(line);
    }
    for (&service_day, group) in &mut groups {
        group.sort_unstable_by_key(|line| line.id);
        for pair in group.windows(2) {
            if pair[0].id == pair[1].id {
                return Err(EvaluationError::DuplicateLineId {
                    service_day,
                    line_id: pair[0].id,
                });
            }
        }
    }
    Ok(groups)
}
