use crate::claim::{
    fits_service, group_lines, ClaimLine, EvaluationError, EvidenceBasis, Modifier,
    ModifierEvidence, ProcedureCode, ServiceDay, ServiceKind,
};
use crate::encoding::MueRecord;
use crate::index::MmapMueIndex;
use std::collections::BTreeMap;

/// A resulting denial can be appealed for all three adjudication indicators.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum MueScope<'a> {
    ClaimLine {
        line_id: u64,
    },
    /// An additional line of an MAI 1 code on the same known claim without supported separate reporting.
    UnsupportedSplit {
        line_id: u64,
        claim_id: &'a str,
    },
    /// An additional MAI 1 line whose claim boundary is unknown; review whether it is a split.
    PossibleUnsupportedSplit {
        line_id: u64,
    },
    /// MAI 2 applies an absolute service-day limit in claims processing.
    AbsoluteServiceDayLimit,
    /// MAI 3 can be reviewed using evidence of services furnished, coding and medical necessity.
    ServiceDayClinicalReview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct MueFinding<'a> {
    pub service_day: ServiceDay<'a>,
    pub code: ProcedureCode,
    pub scope: MueScope<'a>,
    pub max_units: u16,
    pub total_units: u64,
    pub abn_cannot_shift_liability: bool,
}

/// Evaluates published MUEs across all supplied lines, including lines from multiple claims.
/// MAI 1 limits apply per line, and a further line of the same code is flagged unless a repeat
/// or distinct-service modifier with matching evidence supports it. MAI 2 and 3 sum units across
/// all lines of the service day. An absent finding carries no coverage determination.
pub fn evaluate<'a>(
    index: &MmapMueIndex,
    lines: &[ClaimLine<'a>],
) -> Result<Vec<MueFinding<'a>>, EvaluationError<'a>> {
    let mut findings = Vec::new();
    for (service_day, mut group) in group_lines(lines)? {
        // The group is sorted by line id; a stable sort by code keeps that order within a code.
        group.sort_by_key(|line| line.code);
        for lines in group.chunk_by(|a, b| a.code == b.code) {
            let code = lines[0].code;
            let Some(edit) = index.active_on(code.0, service_day.setting as u8, service_day.date.0)
            else {
                continue;
            };
            if let ([line], 1) = (lines, edit.mai) {
                // A code on one line can only exceed its own limit; there is no further line.
                findings.extend(excess(
                    service_day,
                    edit,
                    MueScope::ClaimLine { line_id: line.id },
                    line.units.get(),
                    line.abn_liability_shift_attempted,
                ));
            } else if edit.mai == 1 {
                let splits = split_scopes(lines);
                for (&line, split) in lines.iter().zip(splits) {
                    findings.extend(excess(
                        service_day,
                        edit,
                        MueScope::ClaimLine { line_id: line.id },
                        line.units.get(),
                        line.abn_liability_shift_attempted,
                    ));
                    if let Some(scope) = split {
                        findings.push(finding(
                            service_day,
                            edit,
                            scope,
                            line.units.get(),
                            line.abn_liability_shift_attempted,
                        ));
                    }
                }
            } else {
                findings.extend(service_day_excess(service_day, edit, lines)?);
            }
        }
    }
    Ok(findings)
}

fn service_day_excess<'a>(
    service_day: ServiceDay<'a>,
    edit: &MueRecord,
    lines: &[&ClaimLine<'a>],
) -> Result<Option<MueFinding<'a>>, EvaluationError<'a>> {
    let mut total = 0_u64;
    let mut abn_attempted = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        total = total
            .checked_add(line.units.get())
            .ok_or(EvaluationError::UnitTotalOverflow {
                service_day,
                code: ProcedureCode(edit.code_b36),
            })?;
        abn_attempted |= line.abn_liability_shift_attempted;
        i += 1;
    }
    let scope = if edit.mai == 2 {
        MueScope::AbsoluteServiceDayLimit
    } else {
        MueScope::ServiceDayClinicalReview
    };
    Ok(excess(service_day, edit, scope, total, abn_attempted))
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(20)]
fn mue_mai2_split_and_overflow() {
    use crate::claim::{ServiceDate, Setting};
    use std::num::NonZeroU64;

    let left_units = NonZeroU64::new(kani::any());
    let right_units = NonZeroU64::new(kani::any());
    kani::assume(left_units.is_some() && right_units.is_some());

    let service_day = ServiceDay {
        beneficiary: "proof-beneficiary",
        billing_provider: "proof-provider",
        date: ServiceDate(0),
        setting: Setting::Practitioner,
    };
    let edit = MueRecord {
        code_b36: 0,
        max_units: kani::any(),
        eff_days: 0,
        del_days: crate::encoding::NO_DELETION_DATE,
        mai: 2,
        setting_flags: Setting::Practitioner as u8,
        rationale_id: 0,
        flags: 0,
        reserved: 0,
    };
    let left = ClaimLine {
        id: 1,
        service_day,
        code: ProcedureCode(edit.code_b36),
        service_kind: ServiceKind::Other,
        units: left_units.unwrap(),
        modifiers: &[],
        modifier_evidence: &[],
        abn_liability_shift_attempted: kani::any(),
        claim_id: Some("proof-claim-one"),
    };
    let right = ClaimLine {
        id: 2,
        units: right_units.unwrap(),
        abn_liability_shift_attempted: kani::any(),
        claim_id: Some("proof-claim-two"),
        ..left
    };
    let split = service_day_excess(service_day, &edit, &[&left, &right]);
    let total = u128::from(left.units.get()) + u128::from(right.units.get());
    if total > u128::from(u64::MAX) {
        kani::assert(
            split
                == Err(EvaluationError::UnitTotalOverflow {
                    service_day,
                    code: left.code,
                }),
            "MAI 2 reports overflow for sums above u64::MAX",
        );
    } else {
        let merged = ClaimLine {
            units: NonZeroU64::new(total as u64).unwrap(),
            abn_liability_shift_attempted: left.abn_liability_shift_attempted
                || right.abn_liability_shift_attempted,
            ..left
        };
        kani::assert(
            split == service_day_excess(service_day, &edit, &[&merged]),
            "MAI 2 returns the same finding after a unit-preserving split",
        );
        let Ok(finding) = split else {
            kani::assert(
                false,
                "a representable sum does not return an evaluation error",
            );
            return;
        };
        if let Some(finding) = finding {
            kani::assert(
                finding.scope == MueScope::AbsoluteServiceDayLimit
                    && finding.total_units == total as u64
                    && total > u128::from(edit.max_units),
                "MAI 2 findings record an exact total above the day limit",
            );
        } else {
            kani::assert(
                total <= u128::from(edit.max_units),
                "MAI 2 has no finding when the total is within the day limit",
            );
        }
    }
}

fn finding<'a>(
    service_day: ServiceDay<'a>,
    edit: &MueRecord,
    scope: MueScope<'a>,
    total_units: u64,
    abn_cannot_shift_liability: bool,
) -> MueFinding<'a> {
    MueFinding {
        service_day,
        code: ProcedureCode(edit.code_b36),
        scope,
        max_units: edit.max_units,
        total_units,
        abn_cannot_shift_liability,
    }
}

fn excess<'a>(
    service_day: ServiceDay<'a>,
    edit: &MueRecord,
    scope: MueScope<'a>,
    total_units: u64,
    abn_cannot_shift_liability: bool,
) -> Option<MueFinding<'a>> {
    (total_units > u64::from(edit.max_units)).then(|| {
        finding(
            service_day,
            edit,
            scope,
            total_units,
            abn_cannot_shift_liability,
        )
    })
}

/// Evidence basis that supports reporting a code on a further line under this modifier.
fn repeat_basis(modifier: Modifier) -> Option<EvidenceBasis> {
    use EvidenceBasis::*;

    Some(match &modifier.0 {
        b"76" => RepeatProcedureSamePractitioner,
        b"77" => RepeatProcedureOtherPractitioner,
        b"91" => RepeatClinicalLaboratoryTest,
        b"59" => DistinctService,
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

fn is_repeat(modifier: Modifier) -> bool {
    matches!(&modifier.0, b"76" | b"77" | b"91")
}

/// Whether `line` identifies the anatomic site `modifier` names, by billing the modifier or by
/// documenting it with anatomic-site evidence.
fn identifies_site(line: &ClaimLine<'_>, modifier: Modifier) -> bool {
    line.modifiers.contains(&modifier)
        || line
            .modifier_evidence
            .iter()
            .any(|e| e.modifier == modifier && e.basis == EvidenceBasis::AnatomicSite)
}

/// Whether `modifier` with `basis` can separate `line` from `other`. Repeat modifiers cannot
/// justify an E/M service, and the same anatomic site on both lines separates nothing.
fn fits_pair(
    modifier: Modifier,
    basis: EvidenceBasis,
    line: &ClaimLine<'_>,
    other: &ClaimLine<'_>,
) -> bool {
    repeat_basis(modifier) == Some(basis)
        && fits_service(modifier, line)
        && (!is_repeat(modifier)
            || (line.service_kind != ServiceKind::EvaluationAndManagement
                && other.service_kind != ServiceKind::EvaluationAndManagement))
        && (basis != EvidenceBasis::AnatomicSite || !identifies_site(other, modifier))
}

fn has_specific_evidence(line: &ClaimLine<'_>, other: &ClaimLine<'_>) -> bool {
    line.modifier_evidence.iter().any(|e| {
        e.related_line_id == other.id
            && &e.modifier.0 != b"59"
            && fits_pair(e.modifier, e.basis, line, other)
    })
}

/// The line of `same_code` that `evidence` on `line` separates it from, and whether it is a
/// repeat (`76`, `77`, `91`) rather than a distinct-service or anatomic separation. Evidence
/// counts only for a billed modifier naming another line of the same claim boundary, when it
/// passes `fits_pair` and, for `59`, no more specific modifier is documented for the pair.
fn accepted_edge(
    same_code: &[&ClaimLine<'_>],
    line: &ClaimLine<'_>,
    evidence: &ModifierEvidence,
) -> Option<(usize, bool)> {
    let modifier = evidence.modifier;
    if evidence.related_line_id == line.id || !line.modifiers.contains(&modifier) {
        return None;
    }
    let target = same_code
        .binary_search_by_key(&evidence.related_line_id, |other| other.id)
        .ok()?;
    let other = same_code[target];
    let fits = line.claim_id == other.claim_id
        && fits_pair(modifier, evidence.basis, line, other)
        && (&modifier.0 != b"59"
            || !(has_specific_evidence(line, other) || has_specific_evidence(other, line)));
    fits.then_some((target, is_repeat(modifier)))
}

/// Whether accepted distinct-service or anatomic evidence on either line separates `a` from `b`.
fn separated(same_code: &[&ClaimLine<'_>], a: usize, b: usize) -> bool {
    let names = |from: usize, to: usize| {
        same_code[from]
            .modifier_evidence
            .iter()
            .any(|e| accepted_edge(same_code, same_code[from], e) == Some((to, false)))
    };
    names(a, b) || names(b, a)
}

// In order of preference for the ordinary service slot. A documented repeat never takes it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reporting {
    Plain,
    Distinct,
    Unsupported,
    Repeat,
}

/// Distinct-service or anatomic evidence must separate a line from each plain line of its
/// claim boundary and each lower line already supported by such evidence.
///
/// A repeat must trace back to an original service: a line supported by distinct evidence
/// or a line with no accepted repeat edge. A cycle of repeats establishes nothing.
fn separate_reporting(same_code: &[&ClaimLine<'_>]) -> Vec<Reporting> {
    let n = same_code.len();
    let mut reporting = vec![Reporting::Unsupported; n];
    let mut repeats = vec![Vec::new(); n];
    let mut distinct_edge = vec![false; n];
    let mut repeat_edge = vec![false; n];
    for (i, &line) in same_code.iter().enumerate() {
        for evidence in line.modifier_evidence {
            match accepted_edge(same_code, line, evidence) {
                Some((target, true)) => {
                    repeats[target].push(i);
                    repeat_edge[i] = true;
                }
                Some((_, false)) => distinct_edge[i] = true,
                None => {}
            }
        }
        if !distinct_edge[i] && !repeat_edge[i] {
            reporting[i] = Reporting::Plain;
        }
    }
    // Lines are sorted by id, so "lower" is a fixed order independent of input order.
    for i in 0..n {
        if !distinct_edge[i] {
            continue;
        }
        let distinct = (0..n).all(|j| {
            let original = matches!(reporting[j], Reporting::Plain | Reporting::Distinct);
            j == i
                || same_code[j].claim_id != same_code[i].claim_id
                || !original
                || separated(same_code, i, j)
        });
        if distinct {
            reporting[i] = Reporting::Distinct;
        }
    }
    let mut originals: Vec<usize> = (0..n)
        .filter(|&i| reporting[i] == Reporting::Distinct || !repeat_edge[i])
        .collect();
    while let Some(original) = originals.pop() {
        for &repeat in &repeats[original] {
            if reporting[repeat] == Reporting::Unsupported {
                reporting[repeat] = Reporting::Repeat;
                originals.push(repeat);
            }
        }
    }
    reporting
}

/// One entry per line of `same_code` (sorted by line id): the split finding scope, if any.
fn split_scopes<'a>(same_code: &[&ClaimLine<'a>]) -> Vec<Option<MueScope<'a>>> {
    let reporting = separate_reporting(same_code);
    // Reciprocal evidence can mark both the original and the additional service as distinct.
    // Reserve the ordinary slot before considering any remaining unsupported lines.
    let mut ordinary = BTreeMap::new();
    for (line, &kind) in same_code.iter().zip(&reporting) {
        if kind == Reporting::Repeat {
            continue;
        }
        let candidate = (kind, line.id);
        let first = ordinary.entry(line.claim_id).or_insert(candidate);
        *first = (*first).min(candidate);
    }
    let any_known = same_code.iter().any(|line| line.claim_id.is_some());
    same_code
        .iter()
        .zip(reporting)
        .map(|(line, kind)| {
            if kind == Reporting::Repeat {
                return None;
            }
            let is_ordinary = ordinary[&line.claim_id].1 == line.id;
            if kind == Reporting::Distinct && !is_ordinary {
                return None;
            }
            match line.claim_id {
                Some(claim_id) => (!is_ordinary).then_some(MueScope::UnsupportedSplit {
                    line_id: line.id,
                    claim_id,
                }),
                None => (any_known || !is_ordinary)
                    .then_some(MueScope::PossibleUnsupportedSplit { line_id: line.id }),
            }
        })
        .collect()
}
