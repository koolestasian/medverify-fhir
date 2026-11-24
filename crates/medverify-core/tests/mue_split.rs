use medverify_core::claim::{
    ClaimLine, EvaluationError, EvidenceBasis, Modifier, ModifierEvidence, ProcedureCode,
    ServiceDate, ServiceDay, ServiceKind, Setting,
};
use medverify_core::index::{MmapMueIndex, MmapPtpIndex};
use medverify_core::mue::{self, MueFinding, MueScope};
use proptest::prelude::*;
use std::{path::Path, process::Command, sync::OnceLock};
use time::macros::date;

const MAI1: &str = "Y9011";
const MAI2: &str = "Y9021";
const MAI3: &str = "Y9031";

fn published() -> &'static (MmapPtpIndex, MmapMueIndex) {
    static INDEXES: OnceLock<(MmapPtpIndex, MmapMueIndex)> = OnceLock::new();
    INDEXES.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let result = Command::new(env!("CARGO_BIN_EXE_medverify-index"))
            .args(["synthetic", "--data"])
            .arg(data)
            .arg("--out")
            .arg(dir.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        (
            MmapPtpIndex::open(dir.path().join("ncci_ptp.bin")).unwrap(),
            MmapMueIndex::open(dir.path().join("ncci_mue.bin")).unwrap(),
        )
    })
}

fn line(id: u64, code: &str, units: u64, claim_id: Option<&'static str>) -> ClaimLine<'static> {
    ClaimLine {
        id,
        service_day: ServiceDay {
            beneficiary: "patient-a",
            billing_provider: "billing-a",
            date: ServiceDate::new(date!(2026 - 10 - 01)).unwrap(),
            setting: Setting::Practitioner,
        },
        code: ProcedureCode::new(code).unwrap(),
        service_kind: ServiceKind::Other,
        units: units.try_into().unwrap(),
        modifiers: &[],
        modifier_evidence: &[],
        abn_liability_shift_attempted: false,
        claim_id,
    }
}

fn modifier(code: &str) -> Modifier {
    Modifier::new(code).unwrap()
}

fn evidence(related_line_id: u64, code: &str, basis: EvidenceBasis) -> ModifierEvidence {
    ModifierEvidence {
        related_line_id,
        modifier: modifier(code),
        basis,
    }
}

/// Adds submitted modifiers and their evidence to a line.
fn justified<'a>(
    mut line: ClaimLine<'a>,
    modifiers: &'a [Modifier],
    documented: &'a [ModifierEvidence],
) -> ClaimLine<'a> {
    line.modifiers = modifiers;
    line.modifier_evidence = documented;
    line
}

fn scopes<'a>(findings: &[MueFinding<'a>]) -> Vec<MueScope<'a>> {
    findings.iter().map(|f| f.scope).collect()
}

fn run<'a>(lines: &[ClaimLine<'a>]) -> Vec<MueFinding<'a>> {
    mue::evaluate(&published().1, lines).unwrap()
}

#[test]
fn two_unmodified_lines_with_unknown_boundary_flag_the_higher_id() {
    let findings = run(&[line(2, MAI1, 2, None), line(1, MAI1, 2, None)]);
    assert_eq!(
        scopes(&findings),
        [MueScope::PossibleUnsupportedSplit { line_id: 2 }]
    );
    assert_eq!(findings[0].max_units, 3);
    assert_eq!(findings[0].total_units, 2);
    assert_eq!(findings[0].code.bytes(), *b"Y9011");
}

#[test]
fn two_unmodified_lines_on_one_known_claim_are_an_unsupported_split() {
    let findings = run(&[line(1, MAI1, 2, Some("a")), line(2, MAI1, 2, Some("a"))]);
    assert_eq!(
        scopes(&findings),
        [MueScope::UnsupportedSplit {
            line_id: 2,
            claim_id: "a"
        }]
    );
}

#[test]
fn many_one_unit_lines_on_one_claim_flag_all_but_the_first() {
    let lines: Vec<_> = (1..=6).map(|id| line(id, MAI1, 1, Some("a"))).collect();
    let findings = run(&lines);
    assert_eq!(
        scopes(&findings),
        (2..=6)
            .map(|line_id| MueScope::UnsupportedSplit {
                line_id,
                claim_id: "a"
            })
            .collect::<Vec<_>>()
    );
    assert!(findings.iter().all(|f| f.total_units == 1));
}

#[test]
fn justified_repeat_is_not_a_split() {
    let mods = [modifier("76")];
    let documented = [evidence(
        1,
        "76",
        EvidenceBasis::RepeatProcedureSamePractitioner,
    )];
    for claim in [None, Some("a")] {
        let findings = run(&[
            line(1, MAI1, 1, claim),
            justified(line(2, MAI1, 1, claim), &mods, &documented),
        ]);
        assert!(findings.is_empty(), "{findings:?}");
    }
}

#[test]
fn every_repeat_table_entry_supports_a_separate_line() {
    use EvidenceBasis::*;
    let mut table = vec![
        ("76", RepeatProcedureSamePractitioner),
        ("77", RepeatProcedureOtherPractitioner),
        ("91", RepeatClinicalLaboratoryTest),
        ("59", DistinctService),
        ("XE", SeparateEncounter),
        ("XP", SeparatePractitioner),
        ("XS", SeparateStructure),
        ("XU", NonOverlappingService),
    ];
    for code in [
        "E1", "E2", "E3", "E4", "FA", "F1", "F5", "F9", "TA", "T1", "T5", "T9", "LT", "RT", "LC",
        "LD", "RC", "LM", "RI",
    ] {
        table.push((code, AnatomicSite));
    }
    for (code, basis) in table {
        let mods = [modifier(code)];
        let later = [evidence(1, code, basis)];
        let findings = run(&[
            line(1, MAI1, 1, Some("a")),
            justified(line(2, MAI1, 1, Some("a")), &mods, &later),
        ]);
        assert!(findings.is_empty(), "{code}: {findings:?}");
        // Either line can carry the modifier.
        let earlier = [evidence(2, code, basis)];
        let findings = run(&[
            justified(line(1, MAI1, 1, Some("a")), &mods, &earlier),
            line(2, MAI1, 1, Some("a")),
        ]);
        assert!(findings.is_empty(), "{code}: {findings:?}");
    }
}

#[test]
fn modifiers_outside_the_repeat_table_never_support_a_split() {
    use EvidenceBasis::*;
    for (code, basis) in [
        ("25", SignificantSeparateEm),
        ("24", UnrelatedPostoperativeEm),
        ("78", UnplannedRelatedReturnToOperatingRoom),
        ("79", UnrelatedPostoperativeProcedure),
        ("58", StagedOrRelatedPostoperativeProcedure),
        ("59", RepeatProcedureSamePractitioner),
        ("91", DistinctService),
    ] {
        let mods = [modifier(code)];
        let documented = [evidence(1, code, basis)];
        let findings = run(&[
            line(1, MAI1, 1, Some("a")),
            justified(line(2, MAI1, 1, Some("a")), &mods, &documented),
        ]);
        assert_eq!(findings.len(), 1, "{code}");
    }
}

#[test]
fn an_individually_excessive_justified_line_has_only_the_limit_finding() {
    let mods = [modifier("76")];
    let documented = [evidence(
        1,
        "76",
        EvidenceBasis::RepeatProcedureSamePractitioner,
    )];
    let findings = run(&[
        line(1, MAI1, 1, Some("a")),
        justified(line(2, MAI1, 4, Some("a")), &mods, &documented),
    ]);
    assert_eq!(scopes(&findings), [MueScope::ClaimLine { line_id: 2 }]);
    assert_eq!(findings[0].total_units, 4);
}

#[test]
fn an_excessive_unsupported_line_reports_the_limit_then_the_split() {
    let mut second = line(2, MAI1, 4, Some("a"));
    second.abn_liability_shift_attempted = true;
    let findings = run(&[second, line(1, MAI1, 4, Some("a"))]);
    assert_eq!(
        scopes(&findings),
        [
            MueScope::ClaimLine { line_id: 1 },
            MueScope::ClaimLine { line_id: 2 },
            MueScope::UnsupportedSplit {
                line_id: 2,
                claim_id: "a"
            },
        ]
    );
    let split = findings[2];
    assert_eq!(
        (
            split.max_units,
            split.total_units,
            split.abn_cannot_shift_liability
        ),
        (3, 4, true)
    );
    assert!(!findings[0].abn_cannot_shift_liability);
}

#[test]
fn limit_and_split_findings_interleave_by_line_id() {
    let findings = run(&[
        line(3, MAI1, 5, None),
        line(1, MAI1, 5, None),
        line(2, MAI1, 1, None),
    ]);
    assert_eq!(
        scopes(&findings),
        [
            MueScope::ClaimLine { line_id: 1 },
            MueScope::PossibleUnsupportedSplit { line_id: 2 },
            MueScope::ClaimLine { line_id: 3 },
            MueScope::PossibleUnsupportedSplit { line_id: 3 },
        ]
    );
}

#[test]
fn incorrect_or_missing_evidence_leaves_the_split_flagged() {
    use EvidenceBasis::*;
    let flagged = MueScope::UnsupportedSplit {
        line_id: 2,
        claim_id: "a",
    };
    let m76 = [modifier("76")];
    let m25 = [modifier("25")];
    let other_code = line(3, MAI2, 1, Some("a"));
    let cases: [(&[Modifier], &[ModifierEvidence]); 8] = [
        // wrong basis
        (&m76, &[evidence(1, "76", DistinctService)]),
        // related to a line of a different code
        (&m76, &[evidence(3, "76", RepeatProcedureSamePractitioner)]),
        // related to itself
        (&m76, &[evidence(2, "76", RepeatProcedureSamePractitioner)]),
        // related to a line that does not exist
        (&m76, &[evidence(9, "76", RepeatProcedureSamePractitioner)]),
        // modifier not submitted
        (&[], &[evidence(1, "76", RepeatProcedureSamePractitioner)]),
        // evidence for a different modifier
        (&m76, &[evidence(1, "77", RepeatProcedureOtherPractitioner)]),
        // missing evidence
        (&m76, &[]),
        // modifier outside the table
        (&m25, &[evidence(1, "25", SignificantSeparateEm)]),
    ];
    for (i, (mods, documented)) in cases.into_iter().enumerate() {
        let findings = run(&[
            line(1, MAI1, 1, Some("a")),
            justified(line(2, MAI1, 1, Some("a")), mods, documented),
            other_code,
        ]);
        assert_eq!(scopes(&findings), [flagged], "case {i}");
    }
}

#[test]
fn evidence_must_name_a_line_of_the_same_service_day() {
    let mods = [modifier("76")];
    let documented = [evidence(
        1,
        "76",
        EvidenceBasis::RepeatProcedureSamePractitioner,
    )];
    let mut elsewhere = line(1, MAI1, 1, Some("a"));
    elsewhere.service_day.beneficiary = "patient-b";
    let findings = run(&[
        elsewhere,
        line(3, MAI1, 1, Some("a")),
        justified(line(2, MAI1, 1, Some("a")), &mods, &documented),
    ]);
    assert_eq!(
        scopes(&findings),
        [MueScope::UnsupportedSplit {
            line_id: 3,
            claim_id: "a"
        }]
    );
}

const REPEAT_76: EvidenceBasis = EvidenceBasis::RepeatProcedureSamePractitioner;

#[test]
fn evidence_naming_a_line_of_another_claim_does_not_clear_a_split() {
    let mods = [modifier("76")];
    let flagged = |claim_id| MueScope::UnsupportedSplit {
        line_id: 2,
        claim_id,
    };
    // Claim a: two 2-unit lines, and line 2 names line 3 as the justification.
    let to_line_3 = [evidence(3, "76", REPEAT_76)];
    let a_lines = [
        line(1, MAI1, 2, Some("a")),
        justified(line(2, MAI1, 2, Some("a")), &mods, &to_line_3),
    ];
    // Line 3 does not exist yet, so the evidence is unsupported.
    assert_eq!(scopes(&run(&a_lines)), [flagged("a")]);
    // Adding the referenced line on claim b must not turn the evidence into support.
    let with_b = [a_lines[0], a_lines[1], line(3, MAI1, 2, Some("b"))];
    assert_eq!(scopes(&run(&with_b)), [flagged("a")]);
}

#[test]
fn mirrored_cross_claim_evidence_does_not_clear_a_split() {
    // A line of claim b names a line of claim a.
    let mods = [modifier("76")];
    let to_line_1 = [evidence(1, "76", REPEAT_76)];
    let mirror = [
        line(1, MAI1, 2, Some("a")),
        line(2, MAI1, 2, Some("b")),
        justified(line(3, MAI1, 2, Some("b")), &mods, &to_line_1),
    ];
    assert_eq!(
        scopes(&run(&mirror)),
        [MueScope::UnsupportedSplit {
            line_id: 3,
            claim_id: "b"
        }]
    );
}

#[test]
fn evidence_does_not_reach_from_a_known_claim_to_an_unknown_boundary() {
    let mods = [modifier("76")];
    // The justified line is on claim a, the line it names has no claim.
    let to_line_3 = [evidence(3, "76", REPEAT_76)];
    let findings = run(&[
        line(1, MAI1, 1, Some("a")),
        justified(line(2, MAI1, 1, Some("a")), &mods, &to_line_3),
        line(3, MAI1, 1, None),
    ]);
    assert_eq!(
        scopes(&findings),
        [
            MueScope::UnsupportedSplit {
                line_id: 2,
                claim_id: "a"
            },
            MueScope::PossibleUnsupportedSplit { line_id: 3 },
        ]
    );
}

#[test]
fn evidence_does_not_reach_from_an_unknown_boundary_to_a_known_claim() {
    let mods = [modifier("76")];
    // The justified line has no claim, the line it names is on claim a.
    let to_line_3 = [evidence(3, "76", REPEAT_76)];
    let findings = run(&[
        line(1, MAI1, 1, None),
        justified(line(2, MAI1, 1, None), &mods, &to_line_3),
        line(3, MAI1, 1, Some("a")),
    ]);
    assert_eq!(
        scopes(&findings),
        [
            MueScope::PossibleUnsupportedSplit { line_id: 1 },
            MueScope::PossibleUnsupportedSplit { line_id: 2 },
        ]
    );
}

#[test]
fn evidence_naming_a_line_of_the_same_claim_is_still_accepted() {
    let mods = [modifier("76")];
    let to_line_1 = [evidence(1, "76", REPEAT_76)];
    for claim in [Some("a"), None] {
        let findings = run(&[
            line(1, MAI1, 2, claim),
            justified(line(2, MAI1, 2, claim), &mods, &to_line_1),
        ]);
        assert!(findings.is_empty(), "{claim:?}: {findings:?}");
    }
    // A line of another claim on the same day changes nothing.
    let findings = run(&[
        line(1, MAI1, 2, Some("a")),
        justified(line(2, MAI1, 2, Some("a")), &mods, &to_line_1),
        line(3, MAI1, 2, Some("b")),
    ]);
    assert!(findings.is_empty(), "{findings:?}");
}

fn evaluation_and_management(mut line: ClaimLine<'static>) -> ClaimLine<'static> {
    line.service_kind = ServiceKind::EvaluationAndManagement;
    line
}

#[test]
fn modifier_59_does_not_support_a_repeat_on_an_evaluation_and_management_line() {
    use EvidenceBasis::*;
    let mods = [modifier("59")];
    let to_line_1 = [evidence(1, "59", DistinctService)];
    let to_line_2 = [evidence(2, "59", DistinctService)];
    for claim in [Some("a"), None] {
        let split = |line_id| match claim {
            Some(claim_id) => MueScope::UnsupportedSplit { line_id, claim_id },
            None => MueScope::PossibleUnsupportedSplit { line_id },
        };
        let first = evaluation_and_management(line(1, MAI1, 2, claim));
        let second = evaluation_and_management(line(2, MAI1, 2, claim));
        // The later line carries the modifier.
        let findings = run(&[first, justified(second, &mods, &to_line_1)]);
        assert_eq!(scopes(&findings), [split(2)], "{claim:?}");
        // The earlier line carries it.
        let findings = run(&[justified(first, &mods, &to_line_2), second]);
        assert_eq!(scopes(&findings), [split(2)], "{claim:?}");
        // Other services keep the bypass.
        let findings = run(&[
            line(1, MAI1, 2, claim),
            justified(line(2, MAI1, 2, claim), &mods, &to_line_1),
        ]);
        assert!(findings.is_empty(), "{claim:?}: {findings:?}");
    }
    // The service-fit rule is specific to the modifier. XE does not name E/M services.
    let xe = [modifier("XE")];
    let separate = [evidence(1, "XE", SeparateEncounter)];
    let findings = run(&[
        evaluation_and_management(line(1, MAI1, 2, Some("a"))),
        justified(
            evaluation_and_management(line(2, MAI1, 2, Some("a"))),
            &xe,
            &separate,
        ),
    ]);
    assert!(findings.is_empty(), "{findings:?}");
}

/// The split finding of `line_id` for a claim boundary: known claim `a`, or unknown.
fn split_of(claim: Option<&'static str>, line_id: u64) -> MueScope<'static> {
    match claim {
        Some(claim_id) => MueScope::UnsupportedSplit { line_id, claim_id },
        None => MueScope::PossibleUnsupportedSplit { line_id },
    }
}

#[test]
fn repeats_that_justify_each_other_in_a_cycle_do_not_clear_a_split() {
    let mods = [modifier("76")];
    let to_1 = [evidence(1, "76", REPEAT_76)];
    let to_2 = [evidence(2, "76", REPEAT_76)];
    let to_3 = [evidence(3, "76", REPEAT_76)];
    for claim in [Some("a"), None] {
        // Two lines naming each other, in either input order.
        let pair = [
            justified(line(1, MAI1, 1, claim), &mods, &to_2),
            justified(line(2, MAI1, 1, claim), &mods, &to_1),
        ];
        for lines in [pair, [pair[1], pair[0]]] {
            assert_eq!(
                scopes(&run(&lines)),
                [split_of(claim, 2)],
                "{claim:?} two-line cycle"
            );
        }
        // Three lines in a ring: only the lowest id is the ordinary service.
        let ring = [
            justified(line(1, MAI1, 1, claim), &mods, &to_2),
            justified(line(2, MAI1, 1, claim), &mods, &to_3),
            justified(line(3, MAI1, 1, claim), &mods, &to_1),
        ];
        let flagged = [split_of(claim, 2), split_of(claim, 3)];
        assert_eq!(scopes(&run(&ring)), flagged, "{claim:?} three-line cycle");
        // A repeat that names a member of the ring joins its fate.
        let to_ring = [evidence(1, "76", REPEAT_76)];
        let tailed = [
            ring[0],
            ring[1],
            ring[2],
            justified(line(4, MAI1, 1, claim), &mods, &to_ring),
        ];
        assert_eq!(
            scopes(&run(&tailed)),
            [split_of(claim, 2), split_of(claim, 3), split_of(claim, 4)],
            "{claim:?} ring with a tail"
        );
    }
}

#[test]
fn a_chain_of_repeats_reaches_the_original_service() {
    let mods = [modifier("76")];
    let to_1 = [evidence(1, "76", REPEAT_76)];
    let to_2 = [evidence(2, "76", REPEAT_76)];
    let to_3 = [evidence(3, "76", REPEAT_76)];
    for claim in [Some("a"), None] {
        // plain <- 76 <- 76, in ascending and descending id order.
        let ascending = [
            line(1, MAI1, 1, claim),
            justified(line(2, MAI1, 1, claim), &mods, &to_1),
            justified(line(3, MAI1, 1, claim), &mods, &to_2),
        ];
        assert!(run(&ascending).is_empty(), "{claim:?}");
        let to_2_up = [evidence(2, "76", REPEAT_76)];
        let descending = [
            justified(line(1, MAI1, 1, claim), &mods, &to_2_up),
            justified(line(2, MAI1, 1, claim), &mods, &to_3),
            line(3, MAI1, 1, claim),
        ];
        assert!(run(&descending).is_empty(), "{claim:?}");
    }
}

#[test]
fn a_repeat_of_a_service_supported_by_distinct_service_evidence_is_supported() {
    // Line 2 is reported as a distinct service from line 1, and line 3 repeats line 2.
    let distinct = [modifier("59")];
    let to_1 = [evidence(1, "59", EvidenceBasis::DistinctService)];
    let repeat = [modifier("76")];
    let to_2 = [evidence(2, "76", REPEAT_76)];
    let findings = run(&[
        line(1, MAI1, 1, Some("a")),
        justified(line(2, MAI1, 1, Some("a")), &distinct, &to_1),
        justified(line(3, MAI1, 1, Some("a")), &repeat, &to_2),
    ]);
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn a_repeat_may_name_a_line_whose_own_repeat_evidence_is_not_accepted() {
    // Line 2 bills 76 with evidence of the wrong basis, so it is itself unsupported, but it
    // is still an original service for the valid repeat on line 3.
    let mods = [modifier("76")];
    let wrong = [evidence(1, "76", EvidenceBasis::DistinctService)];
    let to_2 = [evidence(2, "76", REPEAT_76)];
    for claim in [Some("a"), None] {
        let findings = run(&[
            line(1, MAI1, 1, claim),
            justified(line(2, MAI1, 1, claim), &mods, &wrong),
            justified(line(3, MAI1, 1, claim), &mods, &to_2),
        ]);
        assert_eq!(scopes(&findings), [split_of(claim, 2)], "{claim:?}");
    }
    // Evidence that names the line itself or a missing line is no accepted edge either.
    let to_itself = [evidence(2, "76", REPEAT_76)];
    let to_missing = [evidence(9, "76", REPEAT_76)];
    for unproven in [&to_itself, &to_missing] {
        let findings = run(&[
            line(1, MAI1, 1, Some("a")),
            justified(line(2, MAI1, 1, Some("a")), &mods, unproven),
            justified(line(3, MAI1, 1, Some("a")), &mods, &to_2),
        ]);
        assert_eq!(scopes(&findings), [split_of(Some("a"), 2)]);
    }
}

#[test]
fn modifier_91_never_supports_a_split_when_either_line_is_evaluation_and_management() {
    let mods = [modifier("91")];
    let lab = EvidenceBasis::RepeatClinicalLaboratoryTest;
    let to_1 = [evidence(1, "91", lab)];
    let to_2 = [evidence(2, "91", lab)];
    for claim in [Some("a"), None] {
        // (kind of line 1, kind of line 2)
        for (first_em, second_em, supported) in [
            (false, false, true),
            (true, true, false),
            (true, false, false),
            (false, true, false),
        ] {
            let kind = |line: ClaimLine<'static>, em: bool| {
                if em {
                    evaluation_and_management(line)
                } else {
                    line
                }
            };
            // The later line carries the modifier.
            let findings = run(&[
                kind(line(1, MAI1, 1, claim), first_em),
                justified(kind(line(2, MAI1, 1, claim), second_em), &mods, &to_1),
            ]);
            let expected: Vec<MueScope> = if supported {
                vec![]
            } else {
                vec![split_of(claim, 2)]
            };
            assert_eq!(
                scopes(&findings),
                expected,
                "{claim:?} later 91, em {first_em}/{second_em}"
            );
            // The earlier line carries it.
            let findings = run(&[
                justified(kind(line(1, MAI1, 1, claim), first_em), &mods, &to_2),
                kind(line(2, MAI1, 1, claim), second_em),
            ]);
            assert_eq!(
                scopes(&findings),
                expected,
                "{claim:?} earlier 91, em {first_em}/{second_em}"
            );
        }
    }
}

#[test]
fn modifiers_76_and_77_cannot_clear_evaluation_and_management_splits() {
    for (code, basis) in [
        ("76", REPEAT_76),
        ("77", EvidenceBasis::RepeatProcedureOtherPractitioner),
    ] {
        let mods = [modifier(code)];
        let to_1 = [evidence(1, code, basis)];
        for claim in [Some("a"), None] {
            let first = line(1, MAI1, 2, claim);
            let second = justified(line(2, MAI1, 2, claim), &mods, &to_1);
            assert!(run(&[first, second]).is_empty(), "{code} {claim:?}");
            for positions in [&[0][..], &[1], &[0, 1]] {
                let mut pair = [first, second];
                for &index in positions {
                    pair[index].service_kind = ServiceKind::EvaluationAndManagement;
                }
                assert_eq!(
                    scopes(&run(&pair)),
                    [split_of(claim, 2)],
                    "{code} {claim:?}"
                );
                pair.reverse();
                assert_eq!(
                    scopes(&run(&pair)),
                    [split_of(claim, 2)],
                    "{code} {claim:?}"
                );
            }
        }
    }
}

#[test]
fn an_anatomic_modifier_is_not_accepted_when_the_other_line_identifies_the_same_site() {
    let site = EvidenceBasis::AnatomicSite;
    let fa = [modifier("FA")];
    let f1 = [modifier("F1")];
    let fa_to_1 = [evidence(1, "FA", site)];
    let fa_to_2 = [evidence(2, "FA", site)];
    let f1_to_2 = [evidence(2, "F1", site)];
    for claim in [Some("a"), None] {
        let flagged: &[MueScope] = &[split_of(claim, 2)];
        // Both lines bill FA: the same site on both lines separates nothing.
        let findings = run(&[
            justified(line(1, MAI1, 1, claim), &fa, &fa_to_2),
            justified(line(2, MAI1, 1, claim), &fa, &fa_to_1),
        ]);
        assert_eq!(scopes(&findings), flagged, "{claim:?} both bill FA");
        // The other line identifies FA only through its evidence.
        let findings = run(&[
            justified(line(1, MAI1, 1, claim), &[], &fa_to_2),
            justified(line(2, MAI1, 1, claim), &fa, &fa_to_1),
        ]);
        assert_eq!(scopes(&findings), flagged, "{claim:?} FA only in evidence");
        // The same, with the lines swapped.
        let findings = run(&[
            justified(line(1, MAI1, 1, claim), &fa, &fa_to_2),
            justified(line(2, MAI1, 1, claim), &[], &fa_to_1),
        ]);
        assert_eq!(
            scopes(&findings),
            flagged,
            "{claim:?} FA only in evidence, swapped"
        );
        // Different sites do separate the lines.
        let findings = run(&[
            justified(line(1, MAI1, 1, claim), &f1, &f1_to_2),
            justified(line(2, MAI1, 1, claim), &fa, &fa_to_1),
        ]);
        assert!(findings.is_empty(), "{claim:?} FA against F1: {findings:?}");
        // One line with the site and the other with none stays supported.
        let findings = run(&[
            line(1, MAI1, 1, claim),
            justified(line(2, MAI1, 1, claim), &fa, &fa_to_1),
        ]);
        assert!(
            findings.is_empty(),
            "{claim:?} FA against nothing: {findings:?}"
        );
    }
}

/// Both claim boundaries, and both input orders, give the same findings for `lines`.
fn assert_split_lines(lines: &[ClaimLine<'_>], flagged: &[u64]) {
    for claim_id in [Some("a"), None] {
        let mut lines: Vec<_> = lines.iter().map(|l| ClaimLine { claim_id, ..*l }).collect();
        let expected: Vec<_> = flagged
            .iter()
            .map(|&line_id| match claim_id {
                Some(claim_id) => MueScope::UnsupportedSplit { line_id, claim_id },
                None => MueScope::PossibleUnsupportedSplit { line_id },
            })
            .collect();
        assert_eq!(scopes(&run(&lines)), expected, "{claim_id:?}");
        lines.reverse();
        assert_eq!(scopes(&run(&lines)), expected, "{claim_id:?} reversed");
    }
}

#[test]
fn lines_separated_only_from_one_anchor_line_are_not_separated_from_each_other() {
    let rt = [modifier("RT")];
    let lt = [modifier("LT")];
    let xs = [modifier("XS")];
    let site = |id| [evidence(id, "RT", EvidenceBasis::AnatomicSite)];
    let structure = |id| [evidence(id, "XS", EvidenceBasis::SeparateStructure)];
    let (rt_to_1, xs_to_1) = (site(1), structure(1));

    // Two right-side lines each justified only against line 1: the second is a split.
    assert_split_lines(
        &[
            line(1, MAI1, 1, None),
            justified(line(2, MAI1, 1, None), &rt, &rt_to_1),
            justified(line(3, MAI1, 1, None), &rt, &rt_to_1),
        ],
        &[3],
    );
    // The same with the anchor billed on the left side.
    assert_split_lines(
        &[
            justified(line(1, MAI1, 1, None), &lt, &[]),
            justified(line(2, MAI1, 1, None), &rt, &rt_to_1),
            justified(line(3, MAI1, 1, None), &rt, &rt_to_1),
        ],
        &[3],
    );
    // Three separate-structure lines that each cite only line 1.
    assert_split_lines(
        &[
            line(1, MAI1, 1, None),
            justified(line(2, MAI1, 1, None), &xs, &xs_to_1),
            justified(line(3, MAI1, 1, None), &xs, &xs_to_1),
            justified(line(4, MAI1, 1, None), &xs, &xs_to_1),
        ],
        &[3, 4],
    );
    // A second plain line must be separated too, even though it is itself a split.
    assert_split_lines(
        &[
            line(1, MAI1, 1, None),
            line(2, MAI1, 1, None),
            justified(line(3, MAI1, 1, None), &xs, &xs_to_1),
        ],
        &[2, 3],
    );
}

#[test]
fn lines_separated_from_every_other_line_are_all_supported() {
    let xs = [modifier("XS")];
    let structure = |id| evidence(id, "XS", EvidenceBasis::SeparateStructure);
    let to_1 = [structure(1)];
    let to_1_and_2 = [structure(1), structure(2)];
    let to_1_and_3 = [structure(1), structure(3)];
    assert_split_lines(
        &[
            line(1, MAI1, 1, None),
            justified(line(2, MAI1, 1, None), &xs, &to_1),
            justified(line(3, MAI1, 1, None), &xs, &to_1_and_2),
        ],
        &[],
    );
    // Evidence on the lower line can name the higher one.
    assert_split_lines(
        &[
            line(1, MAI1, 1, None),
            justified(line(2, MAI1, 1, None), &xs, &to_1_and_3),
            justified(line(3, MAI1, 1, None), &xs, &to_1),
        ],
        &[],
    );
    let (rt, lt) = ([modifier("RT")], [modifier("LT")]);
    let rt_to_1 = [evidence(1, "RT", EvidenceBasis::AnatomicSite)];
    let lt_to_1_and_2 = [
        evidence(1, "LT", EvidenceBasis::AnatomicSite),
        evidence(2, "LT", EvidenceBasis::AnatomicSite),
    ];
    assert_split_lines(
        &[
            line(1, MAI1, 1, None),
            justified(line(2, MAI1, 1, None), &rt, &rt_to_1),
            justified(line(3, MAI1, 1, None), &lt, &lt_to_1_and_2),
        ],
        &[],
    );
}

#[test]
fn reciprocal_site_evidence_cannot_clear_another_line_on_the_same_side() {
    let (lt, rt) = ([modifier("LT")], [modifier("RT")]);
    let lt_to_2 = [evidence(2, "LT", EvidenceBasis::AnatomicSite)];
    let rt_to_1 = [evidence(1, "RT", EvidenceBasis::AnatomicSite)];
    let lines = [
        justified(line(1, MAI1, 2, None), &lt, &lt_to_2),
        justified(line(2, MAI1, 2, None), &rt, &rt_to_1),
        justified(line(3, MAI1, 2, None), &rt, &rt_to_1),
    ];
    assert_split_lines(&lines[..2], &[]);
    assert_split_lines(&lines, &[3]);
}

#[test]
fn modifier_59_is_not_accepted_when_a_specific_modifier_is_documented_for_the_pair() {
    use EvidenceBasis::*;
    let only_59 = [modifier("59")];
    let xe_too = [modifier("59"), modifier("XE")];
    let generic = evidence(1, "59", DistinctService);
    let specific = evidence(1, "XE", SeparateEncounter);
    let both = [generic, specific];
    let generic_only = [generic];
    let specific_only = [specific];
    for claim in [Some("a"), None] {
        let flagged: &[MueScope] = &[split_of(claim, 2)];
        // 59 alone is accepted.
        let findings = run(&[
            line(1, MAI1, 1, claim),
            justified(line(2, MAI1, 1, claim), &only_59, &generic_only),
        ]);
        assert!(findings.is_empty(), "{claim:?} 59 alone: {findings:?}");
        // XE is documented but not billed: the generic 59 is not enough.
        let findings = run(&[
            line(1, MAI1, 1, claim),
            justified(line(2, MAI1, 1, claim), &only_59, &both),
        ]);
        assert_eq!(scopes(&findings), flagged, "{claim:?} XE documented only");
        // XE billed with its evidence is accepted, with or without the generic modifier.
        let findings = run(&[
            line(1, MAI1, 1, claim),
            justified(line(2, MAI1, 1, claim), &xe_too, &both),
        ]);
        assert!(findings.is_empty(), "{claim:?} XE and 59: {findings:?}");
        let xe = [modifier("XE")];
        let findings = run(&[
            line(1, MAI1, 1, claim),
            justified(line(2, MAI1, 1, claim), &xe, &specific_only),
        ]);
        assert!(findings.is_empty(), "{claim:?} XE alone: {findings:?}");
        // Specific evidence on the other line also rules out the generic modifier.
        let to_2 = [evidence(2, "XE", SeparateEncounter)];
        let findings = run(&[
            justified(line(1, MAI1, 1, claim), &[], &to_2),
            justified(line(2, MAI1, 1, claim), &only_59, &generic_only),
        ]);
        assert_eq!(scopes(&findings), flagged, "{claim:?} XE on the other line");
    }
}

#[test]
fn lines_on_different_known_claims_do_not_flag_each_other() {
    assert!(run(&[line(1, MAI1, 2, Some("a")), line(2, MAI1, 2, Some("b"))]).is_empty());
    let findings = run(&[
        line(1, MAI1, 1, Some("a")),
        line(2, MAI1, 1, Some("b")),
        line(3, MAI1, 1, Some("a")),
        line(4, MAI1, 1, Some("b")),
    ]);
    assert_eq!(
        scopes(&findings),
        [
            MueScope::UnsupportedSplit {
                line_id: 3,
                claim_id: "a"
            },
            MueScope::UnsupportedSplit {
                line_id: 4,
                claim_id: "b"
            },
        ]
    );
}

#[test]
fn an_unknown_boundary_line_is_flagged_next_to_a_known_claim_line() {
    for (first, second, expected) in [(Some("a"), None, 2), (None, Some("a"), 1)] {
        let findings = run(&[line(1, MAI1, 1, first), line(2, MAI1, 1, second)]);
        assert_eq!(
            scopes(&findings),
            [MueScope::PossibleUnsupportedSplit { line_id: expected }]
        );
    }
    let findings = run(&[
        line(1, MAI1, 1, Some("a")),
        line(2, MAI1, 1, Some("b")),
        line(3, MAI1, 1, None),
    ]);
    assert_eq!(
        scopes(&findings),
        [MueScope::PossibleUnsupportedSplit { line_id: 3 }]
    );
}

#[test]
fn reciprocal_evidence_on_a_known_claim_does_not_clear_an_unknown_line() {
    let xs = [modifier("XS")];
    let to_1 = [evidence(1, "XS", EvidenceBasis::SeparateStructure)];
    let to_2 = [evidence(2, "XS", EvidenceBasis::SeparateStructure)];
    let mut lines = [
        justified(line(1, MAI1, 2, Some("a")), &xs, &to_2),
        justified(line(2, MAI1, 2, Some("a")), &xs, &to_1),
        line(3, MAI1, 2, None),
    ];
    assert!(run(&lines[..2]).is_empty());
    let expected = [MueScope::PossibleUnsupportedSplit { line_id: 3 }];
    assert_eq!(scopes(&run(&lines)), expected);
    lines.reverse();
    assert_eq!(scopes(&run(&lines)), expected);
}

#[test]
fn a_supported_line_is_not_compared_with_other_lines() {
    let mods = [modifier("76")];
    let to_first = [evidence(
        1,
        "76",
        EvidenceBasis::RepeatProcedureSamePractitioner,
    )];
    let to_second = [evidence(
        2,
        "76",
        EvidenceBasis::RepeatProcedureSamePractitioner,
    )];
    let findings = run(&[
        line(1, MAI1, 1, None),
        justified(line(2, MAI1, 1, None), &mods, &to_first),
        line(3, MAI1, 1, None),
    ]);
    assert_eq!(
        scopes(&findings),
        [MueScope::PossibleUnsupportedSplit { line_id: 3 }]
    );
    let findings = run(&[
        line(1, MAI1, 1, Some("a")),
        justified(line(2, MAI1, 1, Some("a")), &mods, &to_first),
        line(3, MAI1, 1, Some("a")),
    ]);
    assert_eq!(
        scopes(&findings),
        [MueScope::UnsupportedSplit {
            line_id: 3,
            claim_id: "a"
        }]
    );
    // Two repeats that justify each other have no original service behind them.
    let findings = run(&[
        justified(line(1, MAI1, 1, None), &mods, &to_second),
        justified(line(2, MAI1, 1, None), &mods, &to_first),
    ]);
    assert_eq!(
        scopes(&findings),
        [MueScope::PossibleUnsupportedSplit { line_id: 2 }]
    );
}

#[test]
fn separate_service_days_do_not_flag_each_other() {
    let mut other_day = line(2, MAI1, 2, None);
    other_day.service_day.beneficiary = "patient-b";
    assert!(run(&[line(1, MAI1, 2, None), other_day]).is_empty());
}

#[test]
fn mai_two_and_three_still_sum_across_claims() {
    for (code, scope, max) in [
        (MAI2, MueScope::AbsoluteServiceDayLimit, 2),
        (MAI3, MueScope::ServiceDayClinicalReview, 3),
    ] {
        let findings = run(&[
            line(1, code, max, Some("a")),
            line(2, code, 1, Some("b")),
            line(3, code, 1, None),
        ]);
        assert_eq!(scopes(&findings), [scope], "{code}");
        assert_eq!(findings[0].total_units, max + 2);
        assert_eq!(u64::from(findings[0].max_units), max);
    }
    assert!(run(&[line(1, MAI2, 1, Some("a")), line(2, MAI2, 1, Some("a"))]).is_empty());
}

#[test]
fn mai_one_never_sums_units_so_it_cannot_overflow() {
    let findings = run(&[line(1, MAI1, u64::MAX, None), line(2, MAI1, 1, None)]);
    assert_eq!(
        scopes(&findings),
        [
            MueScope::ClaimLine { line_id: 1 },
            MueScope::PossibleUnsupportedSplit { line_id: 2 },
        ]
    );
    assert_eq!(findings[0].total_units, u64::MAX);
    assert_eq!(findings[1].total_units, 1);
    let findings = run(&[
        line(1, MAI1, u64::MAX, Some("a")),
        line(2, MAI1, u64::MAX, Some("a")),
    ]);
    assert_eq!(
        scopes(&findings),
        [
            MueScope::ClaimLine { line_id: 1 },
            MueScope::ClaimLine { line_id: 2 },
            MueScope::UnsupportedSplit {
                line_id: 2,
                claim_id: "a"
            },
        ]
    );
}

#[test]
fn mai_two_overflow_still_errors_across_claims() {
    let first = line(1, MAI2, u64::MAX, Some("a"));
    assert_eq!(
        mue::evaluate(&published().1, &[first, line(2, MAI2, 1, Some("b"))]),
        Err(EvaluationError::UnitTotalOverflow {
            service_day: first.service_day,
            code: first.code
        })
    );
}

#[test]
fn property_case_count_meets_the_required_floor() {
    let Ok(floor) = std::env::var("MEDVERIFY_MIN_PROPTEST_CASES") else {
        return;
    };
    let floor: u32 = floor
        .parse()
        .expect("MEDVERIFY_MIN_PROPTEST_CASES is a number");
    let cases = ProptestConfig::default().cases;
    assert!(
        cases >= floor,
        "properties run {cases} cases, floor is {floor}"
    );
}

const CLAIMS: [Option<&str>; 3] = [None, Some("a"), Some("b")];

#[derive(Clone, Debug)]
struct Spec {
    units: u64,
    claim: usize,
    /// 0 none, 1 correct repeat evidence, 2 wrong basis, 3 evidence names the line itself,
    /// 4 correct separate-structure (`XS`) evidence.
    evidence_kind: u8,
    target: usize,
    abn: bool,
    sort_key: u16,
}

fn spec() -> impl Strategy<Value = Spec> {
    (
        // 1..=8 spans the 3-unit limit by as much as the original per-line property did.
        prop_oneof![8 => 1..=8u64, 1 => Just(u64::MAX)],
        0..CLAIMS.len(),
        0..5u8,
        0..8usize,
        any::<bool>(),
        any::<u16>(),
    )
        .prop_map(
            |(units, claim, evidence_kind, target, abn, sort_key)| Spec {
                units,
                claim,
                evidence_kind,
                target,
                abn,
                sort_key,
            },
        )
}

/// Index of the other line a correct justification points at, if any other line exists.
fn target_of(specs: &[Spec], i: usize) -> Option<usize> {
    let n = specs.len();
    (n > 1).then(|| {
        let t = specs[i].target % n;
        if t == i {
            (i + 1) % n
        } else {
            t
        }
    })
}

type Owned = (Vec<Modifier>, Vec<ModifierEvidence>);

fn owned_evidence(specs: &[Spec]) -> Vec<Owned> {
    (0..specs.len())
        .map(|i| {
            let id = i as u64 + 1;
            let target = target_of(specs, i).map_or(id, |t| t as u64 + 1);
            let repeat = EvidenceBasis::RepeatProcedureSamePractitioner;
            match specs[i].evidence_kind {
                0 => (vec![], vec![]),
                1 => (vec![modifier("76")], vec![evidence(target, "76", repeat)]),
                2 => (
                    vec![modifier("76")],
                    vec![evidence(target, "76", EvidenceBasis::DistinctService)],
                ),
                3 => (vec![modifier("76")], vec![evidence(id, "76", repeat)]),
                _ => (
                    vec![modifier("XS")],
                    vec![evidence(target, "XS", EvidenceBasis::SeparateStructure)],
                ),
            }
        })
        .collect()
}

/// Lines in the order given by each spec's sort key.
fn build_lines<'a>(specs: &[Spec], owned: &'a [Owned]) -> Vec<ClaimLine<'a>> {
    let mut order: Vec<usize> = (0..specs.len()).collect();
    order.sort_by_key(|&i| (specs[i].sort_key, i));
    order
        .into_iter()
        .map(|i| {
            let mut l = line(i as u64 + 1, MAI1, specs[i].units, CLAIMS[specs[i].claim]);
            l.abn_liability_shift_attempted = specs[i].abn;
            justified(l, &owned[i].0, &owned[i].1)
        })
        .collect()
}

type Expected = (MueScope<'static>, u16, u64, bool);

/// Whether following accepted repeat edges from `start` reaches a line that has none. A graph of
/// `n` lines with one edge each reaches such a line within `n` steps or never does.
fn reaches_a_line_without_an_edge(edge: &[Option<usize>], start: usize) -> bool {
    let mut at = start;
    for _ in 0..edge.len() {
        match edge[at] {
            None => return true,
            Some(next) => at = next,
        }
    }
    false
}

/// Direct restatement of the split rule, one line at a time, sharing nothing with mue.rs.
fn oracle(specs: &[Spec]) -> Vec<Expected> {
    let n = specs.len();
    // An accepted repeat edge is correct 76 evidence naming a line of the same claim boundary.
    // Wrong-basis evidence and evidence naming the line itself give a line no edge.
    let edge: Vec<Option<usize>> = (0..n)
        .map(|i| {
            target_of(specs, i).filter(|&t| {
                specs[i].evidence_kind == 1 && CLAIMS[specs[t].claim] == CLAIMS[specs[i].claim]
            })
        })
        .collect();
    // A separation is correct XS evidence naming a line of the same claim boundary.
    let separation: Vec<Option<usize>> = (0..n)
        .map(|i| {
            target_of(specs, i).filter(|&t| {
                specs[i].evidence_kind == 4 && CLAIMS[specs[t].claim] == CLAIMS[specs[i].claim]
            })
        })
        .collect();
    let same_claim = |i: usize, j: usize| CLAIMS[specs[i].claim] == CLAIMS[specs[j].claim];
    let separated = |i: usize, j: usize| separation[i] == Some(j) || separation[j] == Some(i);
    let plain = |j: usize| edge[j].is_none() && separation[j].is_none();
    // A separated line is supported when, taken in line-id order, it is separated from every
    // other line of its claim boundary that has no edge or was itself supported this way.
    let mut distinct = Vec::new();
    for (i, target) in separation.iter().enumerate() {
        if target.is_some() {
            let originals = (0..n).filter(|&j| plain(j)).chain(distinct.iter().copied());
            if originals
                .filter(|&j| same_claim(i, j))
                .all(|j| separated(i, j))
            {
                distinct.push(i);
            }
        }
    }
    // A repeat is supported when its chain of repeat edges ends at a line with none. Such an
    // original service is not itself supported by the chain, and a cycle never ends.
    let repeat = |i: usize| edge[i].is_some() && reaches_a_line_without_an_edge(&edge, i);
    let ordinary = |i: usize| {
        let in_boundary = || (0..n).filter(|&j| same_claim(i, j));
        in_boundary()
            .find(|&j| plain(j))
            .or_else(|| distinct.iter().copied().find(|&j| same_claim(i, j)))
            .or_else(|| in_boundary().find(|&j| !repeat(j)))
    };
    let any_known = specs.iter().any(|spec| CLAIMS[spec.claim].is_some());
    let mut out = Vec::new();
    for i in 0..n {
        let id = i as u64 + 1;
        if specs[i].units > 3 {
            out.push((
                MueScope::ClaimLine { line_id: id },
                3,
                specs[i].units,
                specs[i].abn,
            ));
        }
        let is_ordinary = ordinary(i) == Some(i);
        if repeat(i) || (distinct.contains(&i) && !is_ordinary) {
            continue;
        }
        let scope = match CLAIMS[specs[i].claim] {
            Some(claim_id) => (!is_ordinary).then_some(MueScope::UnsupportedSplit {
                line_id: id,
                claim_id,
            }),
            None => (any_known || !is_ordinary)
                .then_some(MueScope::PossibleUnsupportedSplit { line_id: id }),
        };
        if let Some(scope) = scope {
            out.push((scope, 3, specs[i].units, specs[i].abn));
        }
    }
    out
}

proptest! {
    #[test]
    fn mai_one_findings_match_the_oracle_for_any_input_order(
        specs in proptest::collection::vec(spec(), 1..=8),
    ) {
        let owned = owned_evidence(&specs);
        let lines = build_lines(&specs, &owned);
        let actual: Vec<_> = run(&lines)
            .iter()
            .map(|f| (f.scope, f.max_units, f.total_units, f.abn_cannot_shift_liability))
            .collect();
        prop_assert_eq!(actual, oracle(&specs));
    }

    #[test]
    fn mixed_code_findings_ignore_input_order(
        keys in any::<[u16; 8]>(),
        claims in proptest::collection::vec(0..CLAIMS.len(), 8),
        units in proptest::collection::vec(1..=4u64, 8),
    ) {
        let codes = [MAI1, MAI1, MAI2, MAI2, MAI3, MAI3, MAI1, MAI2];
        let mut lines: Vec<_> = (0..8)
            .map(|i| line(i as u64 + 1, codes[i], units[i], CLAIMS[claims[i]]))
            .collect();
        let expected = run(&lines);
        lines.sort_by_key(|l| keys[l.id as usize - 1]);
        prop_assert_eq!(run(&lines), expected);
    }
}
