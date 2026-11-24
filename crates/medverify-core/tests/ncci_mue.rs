use medverify_core::claim::{
    ClaimLine, EvaluationError, EvidenceBasis, Modifier, ModifierEvidence, ProcedureCode,
    ServiceDate, ServiceDay, ServiceKind, Setting,
};
use medverify_core::encoding::{encode_code_b36, MueRecord, NcciPtpRecord};
use medverify_core::index::{write_mue, write_ptp, CatalogMode, MmapMueIndex, MmapPtpIndex};
use medverify_core::mue::{self, MueScope};
use medverify_core::ncci::{self, ModifierIssueKind, PtpOutcome};
use proptest::prelude::*;
use std::{fs::File, path::Path, process::Command, sync::OnceLock};
use time::{macros::date, Date, Duration};

const RELEASE: Date = date!(2026 - 10 - 01);

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

fn line(id: u64, code: &str, units: u64) -> ClaimLine<'static> {
    ClaimLine {
        id,
        service_day: ServiceDay {
            beneficiary: "patient-a",
            billing_provider: "billing-a",
            date: ServiceDate::new(RELEASE).unwrap(),
            setting: Setting::Practitioner,
        },
        code: ProcedureCode::new(code).unwrap(),
        service_kind: ServiceKind::Other,
        units: units.try_into().unwrap(),
        modifiers: &[],
        modifier_evidence: &[],
        abn_liability_shift_attempted: false,
        claim_id: None,
    }
}

fn evidence(related_line_id: u64, code: &str, basis: EvidenceBasis) -> ModifierEvidence {
    ModifierEvidence {
        related_line_id,
        modifier: Modifier::new(code).unwrap(),
        basis,
    }
}

fn change_group(line: &mut ClaimLine<'_>, dimension: usize) {
    match dimension {
        0 => line.service_day.beneficiary = "patient-b",
        1 => line.service_day.billing_provider = "billing-b",
        2 => line.service_day.date = ServiceDate::new(RELEASE.next_day().unwrap()).unwrap(),
        3 => line.service_day.setting = Setting::HospitalOutpatient,
        _ => unreachable!(),
    }
}

#[test]
fn compiler_dates_preserve_the_epoch_leap_day_and_release_boundary() {
    let (ptp, mue) = published();
    for (dos, expected) in [
        (date!(1990 - 01 - 01), Some(PtpOutcome::ModifierProhibited)),
        (date!(1990 - 01 - 02), None),
    ] {
        let mut pair = [line(1, "Y9119", 1), line(2, "Y9120", 1)];
        for line in &mut pair {
            line.service_day.date = ServiceDate::new(dos).unwrap();
        }
        let findings = ncci::evaluate(ptp, &pair).unwrap();
        assert_eq!(findings.first().map(|f| f.outcome), expected);
        if let Some(finding) = findings.first() {
            assert_eq!(finding.column_two_line_id, 1);
        }
    }
    for (dos, limit) in [
        (date!(2024 - 02 - 28), None),
        (date!(2024 - 02 - 29), Some(2)),
        (date!(2026 - 09 - 30), Some(2)),
        (RELEASE, Some(3)),
        (date!(2169 - 06 - 05), Some(3)),
    ] {
        let mut claim = line(1, "Y9011", 4);
        claim.service_day.date = ServiceDate::new(dos).unwrap();
        assert_eq!(claim.service_day.date.date(), dos);
        assert_eq!(
            mue::evaluate(mue, &[claim])
                .unwrap()
                .first()
                .map(|f| f.max_units),
            limit
        );
    }
    for invalid in [date!(1989 - 12 - 31), date!(2169 - 06 - 06)] {
        assert!(ServiceDate::new(invalid).is_none());
    }
    assert!(ncci::evaluate(
        ptp,
        &[line(1, "Y9100", 1), line(2, "Y9101", 1)].map(|mut line| {
            line.service_day.setting = Setting::HospitalOutpatient;
            line
        }),
    )
    .unwrap()
    .is_empty());
}

#[test]
fn lookup_survives_nested_and_inactive_rows_and_keeps_both_targets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ptp");
    let code = |s| encode_code_b36(s).unwrap();
    let pair = |eff_days, del_days, ccmi| NcciPtpRecord {
        col1_b36: code("Y9001"),
        col2_b36: code("Y9002"),
        eff_days,
        del_days,
        ccmi,
        setting_flags: Setting::Practitioner as u8,
        rationale_id: 0,
        flags: 0,
    };
    write_ptp(
        File::create(&path).unwrap(),
        CatalogMode::Synthetic,
        vec![
            pair(1, 10, 0),
            pair(2, 3, 0),
            pair(5, 5, 9),
            NcciPtpRecord {
                col1_b36: code("Y9002"),
                col2_b36: code("Y9001"),
                ..pair(5, 7, 1)
            },
        ],
    )
    .unwrap();
    let index = MmapPtpIndex::open(&path).unwrap();
    let mut claims = [line(1, "Y9002", 1), line(2, "Y9001", 1)];
    claims[0].abn_liability_shift_attempted = true;
    for (day, targets) in [(0, &[][..]), (1, &[1]), (5, &[1, 2]), (7, &[1]), (10, &[])] {
        for line in &mut claims {
            line.service_day.date =
                ServiceDate::new(date!(1990 - 01 - 01) + Duration::days(day)).unwrap();
        }
        let findings = ncci::evaluate(&index, &claims).unwrap();
        assert!(findings
            .iter()
            .map(|f| f.column_two_line_id)
            .eq(targets.iter().copied()));
        for f in findings {
            assert_eq!(f.abn_cannot_shift_liability, f.column_two_line_id == 1);
            assert_eq!(f.column_one_line_id + f.column_two_line_id, 3);
        }
    }
}

#[test]
fn finite_mue_honors_both_bounds_and_ignores_other_procedures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mue");
    let code = encode_code_b36("Y9001").unwrap();
    let row = MueRecord {
        code_b36: code,
        max_units: 1,
        eff_days: 1,
        del_days: 10,
        mai: 3,
        setting_flags: Setting::Practitioner as u8,
        rationale_id: 0,
        flags: 0,
        reserved: 0,
    };
    write_mue(
        File::create(&path).unwrap(),
        CatalogMode::Synthetic,
        vec![
            row,
            MueRecord {
                eff_days: 2,
                del_days: 3,
                ..row
            },
        ],
    )
    .unwrap();
    let index = MmapMueIndex::open(&path).unwrap();
    for day in [0, 1, 3, 9, 10] {
        let mut lines = [
            line(1, "Y9001", 1),
            line(2, "Y9001", 1),
            line(3, "Y9002", 500),
        ];
        for line in &mut lines {
            line.service_day.date =
                ServiceDate::new(date!(1990 - 01 - 01) + Duration::days(day)).unwrap();
        }
        assert!(mue::evaluate(&index, &lines[1..]).unwrap().is_empty());
        let findings = mue::evaluate(&index, &lines).unwrap();
        assert_eq!(findings.len(), usize::from((1..10).contains(&day)));
        for finding in findings {
            assert_eq!(finding.scope, MueScope::ServiceDayClinicalReview);
            assert_eq!(finding.code.bytes(), *b"Y9001");
            assert_eq!(finding.total_units, 2);
        }
    }
}

#[test]
fn approved_modifiers_need_the_right_evidence_for_either_column() {
    use EvidenceBasis::*;
    let (ptp, _) = published();
    for (modifiers, basis, is_em) in [
        (&["24"][..], UnrelatedPostoperativeEm, true),
        (&["25"], SignificantSeparateEm, true),
        (&["57"], DecisionForMajorSurgery, true),
        (&["58"], StagedOrRelatedPostoperativeProcedure, false),
        (&["59"], DistinctService, false),
        (&["78"], UnplannedRelatedReturnToOperatingRoom, false),
        (&["79"], UnrelatedPostoperativeProcedure, false),
        (&["91"], RepeatClinicalLaboratoryTest, false),
        (&["XE"], SeparateEncounter, false),
        (&["XP"], SeparatePractitioner, false),
        (&["XS"], SeparateStructure, false),
        (&["XU"], NonOverlappingService, false),
        (
            &[
                "E1", "E2", "E3", "E4", "FA", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9",
                "TA", "T1", "T2", "T3", "T4", "T5", "T6", "T7", "T8", "T9", "LT", "RT", "LC", "LD",
                "RC", "LM", "RI",
            ],
            AnatomicSite,
            false,
        ),
    ] {
        for &code in modifiers {
            for (column, related) in [(0, 2), (1, 1)] {
                let modifier = [Modifier::new(code).unwrap()];
                let documented = [evidence(related, code, basis)];
                let mut lines = [line(1, "Y9001", 1), line(2, "Y9002", 1)];
                if is_em {
                    lines[column].service_kind = ServiceKind::EvaluationAndManagement;
                }
                lines[column].modifiers = &modifier;
                let findings = ncci::evaluate(ptp, &lines).unwrap();
                assert_eq!(
                    findings[0].outcome,
                    PtpOutcome::SupportedModifierRequired,
                    "{code}"
                );
                assert_eq!(
                    findings[0].modifier_issues[0].kind,
                    ModifierIssueKind::MissingEvidence
                );
                lines[column].modifier_evidence = &documented;
                let findings = ncci::evaluate(ptp, &lines).unwrap();
                assert_eq!(findings[0].outcome, PtpOutcome::Bypassed, "{code}");
                assert!(findings[0].modifier_issues.is_empty());
            }
        }
    }
    let modifier = [Modifier::new("27").unwrap()];
    let documented = [evidence(2, "27", SeparateHospitalEmEncounter)];
    let mut lines = [line(1, "Y9006", 1), line(2, "Y9005", 1)];
    for line in &mut lines {
        line.service_day.setting = Setting::HospitalOutpatient;
    }
    lines[0].service_kind = ServiceKind::EvaluationAndManagement;
    lines[0].modifiers = &modifier;
    lines[0].modifier_evidence = &documented;
    assert_eq!(
        ncci::evaluate(ptp, &lines).unwrap()[0].outcome,
        PtpOutcome::Bypassed
    );
}

#[test]
fn specific_modifiers_and_mismatched_evidence_are_audited() {
    use EvidenceBasis::*;
    let (ptp, _) = published();
    let xe = Modifier::new("XE").unwrap();
    let xs = Modifier::new("XS").unwrap();
    let lt = Modifier::new("LT").unwrap();
    let mods = [
        xe,
        xs,
        Modifier::new("25").unwrap(),
        Modifier::new("76").unwrap(),
    ];
    let documentation = [
        evidence(2, "XE", SeparateStructure),
        evidence(2, "XS", SeparateStructure),
        evidence(2, "LT", AnatomicSite),
        evidence(2, "25", SignificantSeparateEm),
        evidence(2, "76", DistinctService),
    ];
    let mut claim = line(1, "Y9001", 1);
    claim.modifiers = &mods;
    claim.modifier_evidence = &documentation;
    let findings = ncci::evaluate(ptp, &[claim, line(2, "Y9002", 1)]).unwrap();
    assert_eq!(findings[0].outcome, PtpOutcome::SupportedModifierRequired);
    assert!(findings[0]
        .modifier_issues
        .iter()
        .map(|issue| (issue.modifier.bytes(), issue.kind))
        .eq([
            (*b"25", ModifierIssueKind::WrongService),
            (*b"XE", ModifierIssueKind::MissingEvidence),
            (*b"XS", ModifierIssueKind::MoreSpecificModifierRequired(lt)),
        ]));
}

#[test]
fn totals_are_checked_and_duplicate_ids_are_rejected() {
    let (ptp, mue) = published();
    let first = line(1, "Y9021", u64::MAX);
    let second = line(2, "Y9021", 1);
    assert_eq!(
        mue::evaluate(mue, &[first, second]),
        Err(EvaluationError::UnitTotalOverflow {
            service_day: first.service_day,
            code: first.code
        })
    );
    assert_eq!(
        mue::evaluate(mue, &[first]).unwrap()[0].total_units,
        u64::MAX
    );
    let duplicate = EvaluationError::DuplicateLineId {
        service_day: first.service_day,
        line_id: 1,
    };
    assert_eq!(ncci::evaluate(ptp, &[first, first]), Err(duplicate));
    assert_eq!(mue::evaluate(mue, &[first, first]), Err(duplicate));
    assert!(
        mue::evaluate(mue, &[line(1, "Y9120", u64::MAX), line(2, "Y9120", 1)])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn property_case_count_meets_the_required_floor() {
    // The properties below take their case count from PROPTEST_CASES. CI also sets this
    // floor, so a run that silently drops back to proptest's default of 256 fails here.
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

proptest! {
    #[test]
    fn claim_permutation_preserves_findings(order in any::<[u16; 7]>(), flip_modifiers in any::<bool>()) {
        let (ptp, mue) = published();
        let modifiers = [Modifier::new("59").unwrap(), Modifier::new("XS").unwrap()];
        let reversed_modifiers = [modifiers[1], modifiers[0]];
        let mut lines = [
            line(1, "Y9001", 1), line(2, "Y9002", 1), line(3, "Y9001", 1),
            line(4, "Y9021", 1), line(5, "Y9021", 2), line(6, "Y9011", 5), line(7, "Y9031", 6),
        ];
        lines[1].modifiers = &modifiers;
        let expected_ptp = ncci::evaluate(ptp, &lines).unwrap();
        let expected_mue = mue::evaluate(mue, &lines).unwrap();
        if flip_modifiers {
            lines[1].modifiers = &reversed_modifiers;
        }
        lines.sort_unstable_by_key(|line| order[line.id as usize - 1]);
        prop_assert_eq!(ncci::evaluate(ptp, &lines).unwrap(), expected_ptp);
        prop_assert_eq!(mue::evaluate(mue, &lines).unwrap(), expected_mue);
    }

    #[test]
    fn split_lines_preserve_date_of_service_totals(
        left in prop_oneof![1..8u64, 1..=u64::from(u16::MAX)],
        right in prop_oneof![1..8u64, 1..=u64::from(u16::MAX)],
        clinical in any::<bool>(), abn in any::<bool>(),
    ) {
        let (_, mue) = published();
        let (code, scope, max) = if clinical {
            ("Y9031", MueScope::ServiceDayClinicalReview, 3)
        } else {
            ("Y9021", MueScope::AbsoluteServiceDayLimit, 2)
        };
        let mut whole = line(1, code, left + right);
        whole.abn_liability_shift_attempted = abn;
        let mut second = line(2, code, right);
        second.abn_liability_shift_attempted = abn;
        let findings = mue::evaluate(mue, &[whole]).unwrap();
        prop_assert_eq!(mue::evaluate(mue, &[line(1, code, left), second]).unwrap(), &findings[..]);
        prop_assert_eq!(findings.len(), usize::from(left + right > max));
        if let Some(finding) = findings.first() {
            prop_assert_eq!(finding.scope, scope);
            prop_assert_eq!(finding.total_units, left + right);
            prop_assert_eq!(finding.abn_cannot_shift_liability, abn);
        }
    }

    #[test]
    fn claim_line_limits_are_independent_of_other_units(
        left in 1..9u64, right in 1..9u64, abn in any::<bool>(),
    ) {
        let (_, mue) = published();
        let mut first = line(1, "Y9011", left);
        first.abn_liability_shift_attempted = abn;
        let findings = mue::evaluate(mue, &[first, line(2, "Y9011", right)]).unwrap();
        // Every finding is compared. Each line is limited by its own units, and the second
        // unmodified line without a known claim is also a possible split for review.
        let mut expected: Vec<_> = [(1, left, abn), (2, right, false)].into_iter()
            .filter(|(_, units, _)| *units > 3)
            .map(|(line_id, units, abn)| (MueScope::ClaimLine { line_id }, units, abn))
            .collect();
        expected.push((MueScope::PossibleUnsupportedSplit { line_id: 2 }, right, false));
        prop_assert_eq!(findings.into_iter()
            .map(|f| (f.scope, f.total_units, f.abn_cannot_shift_liability)).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn each_service_day_dimension_isolates_edits(
        dimension in 0..4usize, reverse in any::<bool>(), first_id in 1..10_000u64,
    ) {
        let (ptp, mue) = published();
        let mut procedures = [line(first_id, "Y9001", 1), line(first_id + 1, "Y9002", 1)];
        if reverse {
            procedures.reverse();
        }
        prop_assert_eq!(ncci::evaluate(ptp, &procedures).unwrap().len(), 1);
        change_group(&mut procedures[1], dimension);
        prop_assert!(ncci::evaluate(ptp, &procedures).unwrap().is_empty());

        let units = line(first_id, "Y9021", 1);
        let two_units = line(first_id + 1, "Y9021", 2);
        let findings = mue::evaluate(mue, &[two_units, units]).unwrap();
        prop_assert_eq!(findings.len(), 1);
        prop_assert_eq!(findings[0].total_units, 3);
        let mut other = units;
        change_group(&mut other, dimension);
        prop_assert!(mue::evaluate(mue, &[two_units, other]).unwrap().is_empty());
    }

    #[test]
    fn modifier_subsets_require_pair_specific_evidence(bits in 0..256u16) {
        use EvidenceBasis::*;
        let (ptp, _) = published();
        let enabled = |n: u32| bits & (1_u16 << n) != 0;
        let generic = enabled(0);
        let specific = enabled(1);
        let generic_evidence = enabled(2);
        let specific_evidence = enabled(3);
        let em = enabled(4);
        let same_pair = enabled(5);
        let hospital = enabled(6);
        let reverse = enabled(7);
        let submitted: Vec<_> = [("59", generic), ("XE", specific), ("76", true)].into_iter()
            .filter(|(_, on)| *on).map(|(code, _)| Modifier::new(code).unwrap()).collect();
        let related = if same_pair { 2 } else { 3 };
        let documented: Vec<_> = [
            (evidence(related, "59", DistinctService), generic_evidence),
            (evidence(related, "XE", SeparateEncounter), specific_evidence),
        ].into_iter().filter(|(_, on)| *on).map(|(e, _)| e).collect();
        let mut lines = [line(1, "Y9002", 1), line(2, "Y9001", 1), line(3, "Y9120", 1)];
        lines[0].abn_liability_shift_attempted = true;
        lines[0].modifiers = &submitted;
        lines[0].modifier_evidence = &documented;
        if em {
            lines[0].service_kind = ServiceKind::EvaluationAndManagement;
        }
        if hospital {
            for line in &mut lines {
                line.service_day.setting = Setting::HospitalOutpatient;
            }
        }
        if reverse {
            lines.reverse();
        }
        let findings = ncci::evaluate(ptp, &lines).unwrap();
        let supported = same_pair && ((specific && specific_evidence)
            || (generic && generic_evidence && !em && !specific_evidence));
        let expected = if hospital {
            PtpOutcome::ModifierProhibited
        } else if supported {
            PtpOutcome::Bypassed
        } else {
            PtpOutcome::SupportedModifierRequired
        };
        prop_assert_eq!(findings.len(), 1);
        let f = &findings[0];
        prop_assert_eq!((f.column_one_line_id, f.column_two_line_id), (2, 1));
        prop_assert_eq!(f.outcome, expected);
        prop_assert_eq!(f.abn_cannot_shift_liability, expected != PtpOutcome::Bypassed);
        let issue59 = f.modifier_issues.iter().find(|issue| issue.modifier.bytes() == *b"59").map(|issue| issue.kind);
        let expected59 = if !generic {
            None
        } else if em {
            Some(ModifierIssueKind::WrongService)
        } else if same_pair && specific_evidence {
            Some(ModifierIssueKind::MoreSpecificModifierRequired(Modifier::new("XE").unwrap()))
        } else if !generic_evidence || !same_pair {
            Some(ModifierIssueKind::MissingEvidence)
        } else {
            None
        };
        prop_assert_eq!(issue59, expected59);
    }

    #[test]
    fn ptp_cutover_selects_the_rule_on_the_service_date(
        delta in prop_oneof![Just(-1i64), Just(0), -13_422i64..20_000],
    ) {
        let (ptp, _) = published();
        let dos = RELEASE + Duration::days(delta);
        let date = ServiceDate::new(dos).unwrap();
        let mut lines = [line(1, "Y9001", 1), line(2, "Y9002", 1)];
        for line in &mut lines {
            line.service_day.date = date;
        }
        let expected = if dos < date!(1996 - 01 - 01) {
            None
        } else if dos < RELEASE {
            Some(PtpOutcome::ModifierProhibited)
        } else {
            Some(PtpOutcome::SupportedModifierRequired)
        };
        prop_assert_eq!(ncci::evaluate(ptp, &lines).unwrap().first().map(|f| f.outcome), expected);
    }
}
