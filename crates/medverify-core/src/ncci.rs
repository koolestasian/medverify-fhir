use crate::claim::{
    fits_service, group_lines, ClaimLine, EvaluationError, EvidenceBasis, Modifier, ServiceDay,
};
use crate::index::MmapPtpIndex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtpOutcome {
    ModifierProhibited,
    SupportedModifierRequired,
    Bypassed,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ModifierIssueKind {
    MissingEvidence,
    WrongService,
    MoreSpecificModifierRequired(Modifier),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ModifierIssue {
    pub line_id: u64,
    pub modifier: Modifier,
    pub kind: ModifierIssueKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtpFinding<'a> {
    pub service_day: ServiceDay<'a>,
    pub column_one_line_id: u64,
    pub column_two_line_id: u64,
    pub outcome: PtpOutcome,
    pub modifier_issues: Vec<ModifierIssue>,
    pub abn_cannot_shift_liability: bool,
}

/// Evaluates published PTP edits. An absent finding carries no coverage determination.
pub fn evaluate<'a>(
    index: &MmapPtpIndex,
    lines: &[ClaimLine<'a>],
) -> Result<Vec<PtpFinding<'a>>, EvaluationError<'a>> {
    let mut findings = Vec::new();
    for (day, group) in group_lines(lines)? {
        for (i, &left) in group.iter().enumerate() {
            for &right in &group[i + 1..] {
                for (one, two) in [(left, right), (right, left)] {
                    let Some(edit) =
                        index.active_on(one.code.0, two.code.0, day.setting as u8, day.date.0)
                    else {
                        continue;
                    };
                    let mut modifier_issues = Vec::new();
                    let one_supported = check_modifiers(one, two, &mut modifier_issues);
                    let two_supported = check_modifiers(two, one, &mut modifier_issues);
                    modifier_issues.sort_unstable();
                    modifier_issues.dedup();
                    let outcome = if edit.ccmi == 0 {
                        PtpOutcome::ModifierProhibited
                    } else if one_supported || two_supported {
                        PtpOutcome::Bypassed
                    } else {
                        PtpOutcome::SupportedModifierRequired
                    };
                    findings.push(PtpFinding {
                        service_day: day,
                        column_one_line_id: one.id,
                        column_two_line_id: two.id,
                        outcome,
                        modifier_issues,
                        abn_cannot_shift_liability: outcome != PtpOutcome::Bypassed
                            && two.abn_liability_shift_attempted,
                    });
                }
            }
        }
    }
    findings.sort_unstable_by_key(|f| (f.service_day, f.column_two_line_id, f.column_one_line_id));
    Ok(findings)
}

fn required_evidence(modifier: Modifier) -> Option<EvidenceBasis> {
    use EvidenceBasis::*;

    Some(match &modifier.0 {
        b"24" => UnrelatedPostoperativeEm,
        b"25" => SignificantSeparateEm,
        b"27" => SeparateHospitalEmEncounter,
        b"57" => DecisionForMajorSurgery,
        b"58" => StagedOrRelatedPostoperativeProcedure,
        b"59" => DistinctService,
        b"78" => UnplannedRelatedReturnToOperatingRoom,
        b"79" => UnrelatedPostoperativeProcedure,
        b"91" => RepeatClinicalLaboratoryTest,
        b"XE" => SeparateEncounter,
        b"XP" => SeparatePractitioner,
        b"XS" => SeparateStructure,
        b"XU" => NonOverlappingService,
        b"FA"
        | b"TA"
        | b"LT"
        | b"RT"
        | b"LC"
        | b"LD"
        | b"RC"
        | b"LM"
        | b"RI"
        | [b'E', b'1'..=b'4']
        | [b'F' | b'T', b'1'..=b'9'] => AnatomicSite,
        _ => return None,
    })
}

fn specific_modifier(
    generic: Modifier,
    line: &ClaimLine<'_>,
    other: &ClaimLine<'_>,
) -> Option<Modifier> {
    [(line, other), (other, line)]
        .into_iter()
        .flat_map(|(line, other)| {
            line.modifier_evidence.iter().filter_map(move |evidence| {
                let specific = evidence.modifier;
                let more_specific = match (&generic.0, &specific.0) {
                    (b"59", b"59") => false,
                    (b"59", _) => true,
                    (b"XS", _) => evidence.basis == EvidenceBasis::AnatomicSite,
                    (b"XE", b"24" | b"25" | b"27" | b"57" | b"58" | b"78" | b"79" | b"91") => true,
                    _ => false,
                };
                (more_specific
                    && evidence.related_line_id == other.id
                    && required_evidence(specific) == Some(evidence.basis)
                    && fits_service(specific, line))
                .then_some(specific)
            })
        })
        .min()
}

fn check_modifiers(
    line: &ClaimLine<'_>,
    other: &ClaimLine<'_>,
    issues: &mut Vec<ModifierIssue>,
) -> bool {
    let mut supported = false;
    for &modifier in line.modifiers {
        let Some(basis) = required_evidence(modifier) else {
            continue;
        };
        let kind = if !fits_service(modifier, line) {
            ModifierIssueKind::WrongService
        } else if let Some(specific) = specific_modifier(modifier, line, other) {
            ModifierIssueKind::MoreSpecificModifierRequired(specific)
        } else if line.modifier_evidence.iter().any(|evidence| {
            evidence.related_line_id == other.id
                && evidence.modifier == modifier
                && evidence.basis == basis
        }) {
            supported = true;
            continue;
        } else {
            ModifierIssueKind::MissingEvidence
        };
        issues.push(ModifierIssue {
            line_id: line.id,
            modifier,
            kind,
        });
    }
    supported
}
