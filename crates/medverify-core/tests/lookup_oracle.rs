//! Compares indexed lookups with a linear scan: among the rows with the exact key and
//! `eff_days <= day < del_days`, the last one in file order wins.

use medverify_core::claim::{
    ClaimLine, ProcedureCode, ServiceDate, ServiceDay, ServiceKind, Setting,
};
use medverify_core::encoding::{
    decode_code_b36, decode_epoch_day, encode_code_b36, MueRecord, NcciPtpRecord,
    PtpDirectoryEntry, MAX_PROCEDURE_CODE, NO_DELETION_DATE,
};
use medverify_core::index::{
    write_mue, write_ptp, write_ptp_v1, CatalogMode, MmapMueIndex, MmapPtpIndex,
};
use medverify_core::mue;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::{fs::File, num::NonZeroU64};

const POOL: [&str; 8] = [
    "Y9001", "Y9002", "Y9003", "Y9010", "Y9011", "Y9050", "Y9100", "Y9119",
];
const PTP_SETTINGS: [u8; 3] = [1, 2, 2];
const MUE_SETTINGS: [u8; 3] = [1, 2, 4];
const FIXED_DAYS: [u16; 3] = [0, 1, NO_DELETION_DATE];

fn code(text: &str) -> u32 {
    encode_code_b36(text).unwrap()
}

/// Rows of one cluster share a rule, so overlapping or nested members never conflict.
/// Clusters of one key are disjoint and may touch, and carry unrelated rules.
#[derive(Clone, Debug)]
struct Cluster {
    gap: u16,
    span: u16,
    members: Vec<(u16, u16)>,
    ccmi: u8,
    rationale: u8,
    flags: u8,
    units: u16,
    mai: u8,
    open_ended: bool,
}

#[derive(Clone, Debug)]
struct KeySpec {
    col1: usize,
    col2: usize,
    setting: usize,
    clusters: Vec<Cluster>,
    empties: Vec<u16>,
}

fn cluster() -> impl Strategy<Value = Cluster> {
    (
        0u16..20,
        1u16..40,
        prop::collection::vec((0u16..40, 0u16..40), 1..5),
        0u8..2,
        0u8..3,
        0u8..2,
        1u16..500,
        1u8..4,
        any::<bool>(),
    )
        .prop_map(
            |(gap, span, members, ccmi, rationale, flags, units, mai, open_ended)| Cluster {
                gap,
                span,
                members,
                ccmi,
                rationale,
                flags,
                units,
                mai,
                open_ended,
            },
        )
}

fn specs() -> impl Strategy<Value = Vec<KeySpec>> {
    let key = (
        0..POOL.len(),
        0..POOL.len(),
        0..3usize,
        prop::collection::vec(cluster(), 0..4),
        prop::collection::vec(0u16..150, 0..3),
    )
        .prop_map(|(col1, col2, setting, clusters, empties)| KeySpec {
            col1,
            col2,
            setting,
            clusters,
            empties,
        });
    prop::collection::vec(key, 1..10)
}

/// Half-open intervals with the cluster that owns each, in generation order.
fn intervals(spec: &KeySpec) -> Vec<(u16, u16, &Cluster)> {
    let mut out = Vec::new();
    let mut previous_end = 0;
    let last = spec.clusters.len().wrapping_sub(1);
    for (i, c) in spec.clusters.iter().enumerate() {
        let low = previous_end + c.gap;
        let high = low + c.span;
        previous_end = high;
        for (j, &(a, b)) in c.members.iter().enumerate() {
            let start = low + a % c.span;
            let end = if c.open_ended && i == last && j == 0 {
                NO_DELETION_DATE
            } else {
                start + 1 + b % (high - start)
            };
            out.push((start, end, c));
        }
    }
    out
}

fn ptp_rows(specs: &[KeySpec]) -> Vec<NcciPtpRecord> {
    let mut by_key = BTreeMap::new();
    for s in specs {
        by_key.insert((s.col1, s.col2, PTP_SETTINGS[s.setting]), s);
    }
    let mut rows = Vec::new();
    for (&(col1, col2, setting_flags), spec) in &by_key {
        let row = |eff_days, del_days, ccmi, rationale_id, flags| NcciPtpRecord {
            col1_b36: code(POOL[col1]),
            col2_b36: code(POOL[col2]),
            eff_days,
            del_days,
            ccmi,
            setting_flags,
            rationale_id,
            flags,
        };
        for (eff, del, c) in intervals(spec) {
            rows.push(row(eff, del, c.ccmi, c.rationale, c.flags));
        }
        for &day in &spec.empties {
            rows.push(row(day, day, 9, 0, 0));
        }
    }
    rows.reverse();
    rows
}

fn mue_rows(specs: &[KeySpec]) -> Vec<MueRecord> {
    let mut by_key = BTreeMap::new();
    for s in specs {
        by_key.insert((s.col1, MUE_SETTINGS[s.setting]), s);
    }
    let mut rows = Vec::new();
    for (&(col1, setting_flags), spec) in &by_key {
        for (eff_days, del_days, c) in intervals(spec) {
            rows.push(MueRecord {
                code_b36: code(POOL[col1]),
                max_units: c.units,
                eff_days,
                del_days,
                mai: c.mai,
                setting_flags,
                rationale_id: c.rationale,
                flags: 0,
                reserved: 0,
            });
        }
    }
    rows.reverse();
    rows
}

fn days_around(eff: u16, del: u16) -> Vec<u16> {
    let mut days = FIXED_DAYS.to_vec();
    days.extend(
        [eff.checked_sub(1), Some(eff), eff.checked_add(1)]
            .into_iter()
            .flatten(),
    );
    days.extend([del.checked_sub(1), Some(del)].into_iter().flatten());
    days
}

fn ptp_probes(sorted: &[NcciPtpRecord]) -> BTreeSet<(u32, u32, u8, u16)> {
    let mut out = BTreeSet::new();
    out.insert((code("Y9001"), code("Y9002"), 1, 5));
    for r in sorted {
        let (c1, c2, s) = (r.col1_b36, r.col2_b36, r.setting_flags);
        let mut keys = vec![
            (c1, c2, s),
            (c1, c2 + 1, s),
            (c1, c2.saturating_sub(1), s),
            (c2, c1, s),
            (c1 + 1, c2, s),
            (c1.saturating_sub(1), c2, s),
            (0, c2, s),
            (MAX_PROCEDURE_CODE, c2, s),
            (c1, 0, s),
            (c1, MAX_PROCEDURE_CODE, s),
        ];
        keys.extend([0, 1, 2, 4].map(|setting| (c1, c2, setting)));
        // Between this row's column two and another one in the same bucket.
        keys.extend(
            sorted
                .iter()
                .filter(|o| o.col1_b36 == c1)
                .map(|o| (c1, (c2 + o.col2_b36) / 2, s)),
        );
        for (a, b, setting) in keys {
            for day in days_around(r.eff_days, r.del_days) {
                out.insert((a, b, setting, day));
            }
        }
    }
    out
}

fn ptp_oracle(sorted: &[NcciPtpRecord], probe: (u32, u32, u8, u16)) -> Option<NcciPtpRecord> {
    let (col1, col2, setting, day) = probe;
    sorted
        .iter()
        .rev()
        .find(|r| {
            (r.col1_b36, r.col2_b36, r.setting_flags) == (col1, col2, setting)
                && r.eff_days <= day
                && day < r.del_days
        })
        .copied()
}

fn check_ptp(rows: Vec<NcciPtpRecord>) -> Result<(), TestCaseError> {
    let dir = tempfile::tempdir().unwrap();
    let (v2_path, v1_path) = (dir.path().join("v2"), dir.path().join("v1"));
    write_ptp(
        File::create(&v2_path).unwrap(),
        CatalogMode::Synthetic,
        rows.clone(),
    )
    .unwrap();
    write_ptp_v1(
        File::create(&v1_path).unwrap(),
        CatalogMode::Synthetic,
        rows.clone(),
    )
    .unwrap();
    let (v2, v1) = (
        MmapPtpIndex::open(&v2_path).unwrap(),
        MmapPtpIndex::open(&v1_path).unwrap(),
    );
    let mut sorted = rows;
    sorted.sort_by_key(|r| {
        (
            (r.col1_b36, r.col2_b36, r.setting_flags),
            (r.eff_days, r.del_days),
            (r.ccmi, r.rationale_id, r.flags),
        )
    });
    prop_assert_eq!((v1.format_version(), v2.format_version()), (1, 2));
    prop_assert_eq!(v1.records(), &sorted[..]);
    prop_assert_eq!(v2.records(), &sorted[..]);
    prop_assert!(v1.directory().is_empty());
    let mut directory: Vec<PtpDirectoryEntry> = Vec::new();
    for (i, r) in sorted.iter().enumerate() {
        match directory.last_mut() {
            Some(entry) if entry.col1_b36 == r.col1_b36 => entry.row_count += 1,
            _ => directory.push(PtpDirectoryEntry {
                col1_b36: r.col1_b36,
                first_row: i as u32,
                row_count: 1,
            }),
        }
    }
    prop_assert_eq!(v2.directory(), &directory[..]);
    for probe @ (col1, col2, setting, day) in ptp_probes(&sorted) {
        let want = ptp_oracle(&sorted, probe);
        prop_assert_eq!(
            v2.active_on(col1, col2, setting, day).copied(),
            want,
            "v2 {:?}",
            probe
        );
        prop_assert_eq!(
            v1.active_on(col1, col2, setting, day).copied(),
            want,
            "v1 {:?}",
            probe
        );
    }
    Ok(())
}

fn mue_oracle(sorted: &[MueRecord], probe: (u32, u8, u16)) -> Option<MueRecord> {
    let (code_b36, setting, day) = probe;
    sorted
        .iter()
        .rev()
        .find(|r| {
            (r.code_b36, r.setting_flags) == (code_b36, setting)
                && r.eff_days <= day
                && day < r.del_days
        })
        .copied()
}

fn check_mue(rows: Vec<MueRecord>) -> Result<(), TestCaseError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mue");
    write_mue(
        File::create(&path).unwrap(),
        CatalogMode::Synthetic,
        rows.clone(),
    )
    .unwrap();
    let index = MmapMueIndex::open(&path).unwrap();
    let mut sorted = rows;
    sorted.sort_by_key(|r| {
        (
            (r.code_b36, r.setting_flags),
            (r.eff_days, r.del_days),
            (r.max_units, r.mai, r.rationale_id),
        )
    });
    prop_assert_eq!(index.format_version(), 1);
    prop_assert_eq!(index.records(), &sorted[..]);

    let mut probes = BTreeSet::new();
    probes.insert((code("Y9001"), 1, 5));
    for r in &sorted {
        let codes = [
            r.code_b36,
            r.code_b36 + 1,
            r.code_b36.saturating_sub(1),
            0,
            MAX_PROCEDURE_CODE,
        ];
        for c in codes.into_iter().filter(|&c| c <= MAX_PROCEDURE_CODE) {
            for setting in MUE_SETTINGS {
                for day in days_around(r.eff_days, r.del_days) {
                    probes.insert((c, setting, day));
                }
            }
        }
    }
    for probe @ (code_b36, setting, day) in probes {
        // Day 65535 is the open-ended marker, not a service date.
        let Some(date) = decode_epoch_day(day) else {
            continue;
        };
        let text = decode_code_b36(code_b36).unwrap();
        let line = ClaimLine {
            id: 1,
            service_day: ServiceDay {
                beneficiary: "patient",
                billing_provider: "provider",
                date: ServiceDate::new(date).unwrap(),
                setting: match setting {
                    1 => Setting::Practitioner,
                    2 => Setting::HospitalOutpatient,
                    _ => Setting::DmeSupplier,
                },
            },
            code: ProcedureCode::new(std::str::from_utf8(&text).unwrap()).unwrap(),
            service_kind: ServiceKind::Other,
            // Above every u16 limit, so any active edit produces a finding.
            units: NonZeroU64::new(u64::from(u16::MAX) + 1).unwrap(),
            modifiers: &[],
            modifier_evidence: &[],
            abn_liability_shift_attempted: false,
            claim_id: None,
        };
        let findings = mue::evaluate(&index, &[line]).unwrap();
        let want = mue_oracle(&sorted, probe);
        prop_assert_eq!(findings.len(), usize::from(want.is_some()), "{:?}", probe);
        prop_assert_eq!(
            findings.first().map(|f| f.max_units),
            want.map(|r| r.max_units),
            "{:?}",
            probe
        );
    }
    Ok(())
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

proptest! {
    #[test]
    fn ptp_lookup_matches_the_linear_oracle(specs in specs()) {
        check_ptp(ptp_rows(&specs))?;
    }

    #[test]
    fn mue_lookup_matches_the_linear_oracle(specs in specs()) {
        check_mue(mue_rows(&specs))?;
    }
}
