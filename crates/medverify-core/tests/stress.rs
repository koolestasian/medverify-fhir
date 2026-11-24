use medverify_core::claim::{
    ClaimLine, ProcedureCode, ServiceDate, ServiceDay, ServiceKind, Setting,
};
use medverify_core::encoding::{
    decode_code_b36, encode_code_b36, encode_epoch_day, MueRecord, NcciPtpRecord, NO_DELETION_DATE,
};
use medverify_core::index::{write_mue, write_ptp, CatalogMode, MmapMueIndex, MmapPtpIndex};
use medverify_core::stress::{self, ProbeMix, SEED, SERVICE_DAY};
use medverify_core::{mue, ncci};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::num::NonZeroU64;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::{tempdir, TempDir};
use time::macros::date;

fn ptp_bytes(rows: u32, seed: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_ptp(
        &mut bytes,
        CatalogMode::SyntheticStress,
        stress::ptp_records(rows, seed),
    )
    .unwrap();
    bytes
}

fn mue_bytes(rows: u32, seed: u64) -> Vec<u8> {
    let ptp = stress::ptp_records(rows, seed);
    let mut bytes = Vec::new();
    write_mue(
        &mut bytes,
        CatalogMode::SyntheticStress,
        stress::mue_records(&ptp, seed),
    )
    .unwrap();
    bytes
}

struct Catalog {
    _dir: TempDir,
    ptp_records: Vec<NcciPtpRecord>,
    mue_records: Vec<MueRecord>,
    ptp: MmapPtpIndex,
    mue: MmapMueIndex,
}

fn catalog(rows: u32) -> Catalog {
    let dir = tempdir().unwrap();
    let ptp_records = stress::ptp_records(rows, SEED);
    let mue_records = stress::mue_records(&ptp_records, SEED);
    let mode = CatalogMode::SyntheticStress;
    write_ptp(
        File::create(dir.path().join("ptp.bin")).unwrap(),
        mode,
        ptp_records.clone(),
    )
    .unwrap();
    write_mue(
        File::create(dir.path().join("mue.bin")).unwrap(),
        mode,
        mue_records.clone(),
    )
    .unwrap();
    let ptp = MmapPtpIndex::open(dir.path().join("ptp.bin")).unwrap();
    let mue = MmapMueIndex::open(dir.path().join("mue.bin")).unwrap();
    Catalog {
        _dir: dir,
        ptp_records,
        mue_records,
        ptp,
        mue,
    }
}

fn index_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_medverify-index"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn constants_match_their_dates() {
    assert_eq!(encode_epoch_day(date!(2026 - 10 - 01)), Some(SERVICE_DAY));
    let ptp = stress::ptp_records(97, SEED);
    let mue = stress::mue_records(&ptp, SEED);
    let january = encode_epoch_day(date!(2026 - 01 - 01)).unwrap();
    assert!(mue.iter().all(|row| row.eff_days == january));
}

#[test]
fn generates_the_exact_row_count() {
    assert!(stress::ptp_records(0, SEED).is_empty());
    for rows in [1, 2, 97, 5_000, 20_000] {
        let records = stress::ptp_records(rows, SEED);
        assert_eq!(records.len(), rows as usize);
        let mut output = Vec::new();
        write_ptp(&mut output, CatalogMode::SyntheticStress, records).unwrap();
    }
}

#[test]
fn same_seed_gives_identical_files_and_another_seed_differs() {
    for rows in [97, 20_000] {
        assert_eq!(ptp_bytes(rows, SEED), ptp_bytes(rows, SEED));
        assert_eq!(mue_bytes(rows, SEED), mue_bytes(rows, SEED));
        assert_ne!(ptp_bytes(rows, SEED), ptp_bytes(rows, SEED + 1));
        assert_ne!(mue_bytes(rows, SEED), mue_bytes(rows, SEED + 1));
    }
}

#[test]
fn written_index_opens_as_version_2_stress_catalog() {
    let catalog = catalog(5_000);
    assert_eq!(catalog.ptp.catalog_mode(), CatalogMode::SyntheticStress);
    assert_eq!(catalog.ptp.format_version(), 2);
    assert_eq!(catalog.ptp.records().len(), 5_000);
    assert_eq!(catalog.ptp.release(), "stress01");
    assert_eq!(catalog.mue.catalog_mode(), CatalogMode::SyntheticStress);
    assert_eq!(catalog.mue.records().len(), catalog.mue_records.len());
}

#[test]
fn key_distribution_has_the_documented_shape() {
    let rows = stress::ptp_records(400_000, SEED);
    let mut keys = HashMap::<(u32, u32, u8), Vec<&NcciPtpRecord>>::new();
    let mut buckets = HashMap::<u32, usize>::new();
    for row in &rows {
        keys.entry((row.col1_b36, row.col2_b36, row.setting_flags))
            .or_default()
            .push(row);
        *buckets.entry(row.col1_b36).or_default() += 1;
        assert_eq!(decode_code_b36(row.col1_b36).unwrap()[0], b'Y');
        assert_eq!(decode_code_b36(row.col2_b36).unwrap()[0], b'Y');
        assert_ne!(row.col1_b36, row.col2_b36);
    }

    let two_rows = keys.values().filter(|rows| rows.len() == 2).count();
    assert!(keys.values().all(|rows| rows.len() <= 2));
    let share = two_rows as f64 / keys.len() as f64;
    assert!((0.13..0.17).contains(&share), "two-row key share {share}");

    let empty = rows.iter().filter(|row| row.ccmi == 9).count();
    let share = empty as f64 / rows.len() as f64;
    assert!((0.015..0.025).contains(&share), "CCMI 9 row share {share}");
    let indicators: HashSet<u8> = rows.iter().map(|row| row.ccmi).collect();
    assert_eq!(indicators, HashSet::from([0, 1, 9]));
    let settings: HashSet<u8> = rows.iter().map(|row| row.setting_flags).collect();
    assert_eq!(settings, HashSet::from([1, 2]));

    for rows in keys.values() {
        let active =
            |row: &NcciPtpRecord| row.eff_days <= SERVICE_DAY && SERVICE_DAY < row.del_days;
        if rows[0].ccmi == 9 {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].eff_days, rows[0].del_days);
            assert!(!active(rows[0]));
            continue;
        }
        assert_eq!(rows.iter().filter(|row| active(row)).count(), 1);
        if let &[first, second] = rows.as_slice() {
            let (first, second) = if first.eff_days < second.eff_days {
                (first, second)
            } else {
                (second, first)
            };
            assert!(first.del_days <= second.eff_days);
            assert_eq!(second.del_days, NO_DELETION_DATE);
            assert_ne!(first.ccmi, second.ccmi);
        }
    }

    let mut sizes: Vec<usize> = buckets.values().copied().collect();
    sizes.sort_unstable();
    let mean = rows.len() / sizes.len();
    assert!(
        (1_200..1_500).contains(&sizes.len()),
        "{} buckets",
        sizes.len()
    );
    assert!((250..350).contains(&mean), "mean bucket {mean}");
    assert!(
        sizes[sizes.len() / 2] < mean,
        "median is not below the mean"
    );
    assert!(sizes[sizes.len() - 1] > 1_000);
    assert!(sizes[sizes.len() / 10] < 100, "too few small buckets");

    let col1: HashSet<u32> = buckets.keys().copied().collect();
    assert!(rows.iter().all(|row| col1.contains(&row.col2_b36)));
    let mut sorted: Vec<u32> = col1.into_iter().collect();
    sorted.sort_unstable();
    let adjacent = sorted
        .windows(2)
        .filter(|pair| pair[1] - pair[0] == 1)
        .count();
    assert!(
        adjacent * 100 <= sorted.len(),
        "{adjacent} consecutive codes"
    );
}

#[test]
fn mue_records_cover_each_code_once() {
    let ptp = stress::ptp_records(20_000, SEED);
    let rows = stress::mue_records(&ptp, SEED);
    let codes: HashSet<u32> = ptp
        .iter()
        .flat_map(|row| [row.col1_b36, row.col2_b36])
        .collect();
    assert_eq!(rows.len(), codes.len());
    assert_eq!(
        rows.iter().map(|row| row.code_b36).collect::<HashSet<_>>(),
        codes
    );
    for mai in 1..=3 {
        assert!(rows.iter().any(|row| row.mai == mai));
    }
    for units in 1..=4 {
        assert!(rows.iter().any(|row| row.max_units == units));
    }
    assert!(rows.iter().all(|row| {
        (1..=4).contains(&row.max_units)
            && row.setting_flags == 1
            && row.del_days == NO_DELETION_DATE
    }));
}

fn line(id: u64, code: &str, units: u64, claim_id: Option<&'static str>) -> ClaimLine<'static> {
    ClaimLine {
        id,
        service_day: ServiceDay {
            beneficiary: "beneficiary",
            billing_provider: "provider",
            date: ServiceDate::new(date!(2026 - 10 - 01)).unwrap(),
            setting: Setting::Practitioner,
        },
        code: ProcedureCode::new(code).unwrap(),
        service_kind: ServiceKind::Other,
        units: NonZeroU64::new(units).unwrap(),
        modifiers: &[],
        modifier_evidence: &[],
        abn_liability_shift_attempted: false,
        claim_id,
    }
}

fn code(text: &str) -> u32 {
    encode_code_b36(text).unwrap()
}

fn edit(col1: &str, col2: &str, setting: u8, dates: (u16, u16), ccmi: u8) -> NcciPtpRecord {
    NcciPtpRecord {
        col1_b36: code(col1),
        col2_b36: code(col2),
        eff_days: dates.0,
        del_days: dates.1,
        ccmi,
        setting_flags: setting,
        rationale_id: 0,
        flags: 0,
    }
}

fn limit(text: &str, max_units: u16, mai: u8) -> MueRecord {
    MueRecord {
        code_b36: code(text),
        max_units,
        eff_days: 0,
        del_days: NO_DELETION_DATE,
        mai,
        setting_flags: 1,
        rationale_id: 0,
        flags: 0,
        reserved: 0,
    }
}

#[test]
fn oracle_classifies_each_probe_and_counts_limits() {
    let ptp = [
        edit("Y0001", "Y0002", 1, (100, NO_DELETION_DATE), 1),
        edit("Y0001", "Y0003", 1, (100, 200), 0),
        edit("Y0002", "Y0003", 2, (100, NO_DELETION_DATE), 1),
        edit("Y0009", "Y0001", 1, (100, NO_DELETION_DATE), 1),
    ];
    let mue = [
        limit("Y0001", 1, 1),
        limit("Y0002", 2, 2),
        limit("Y0003", 3, 3),
    ];
    let claim = [
        line(1, "Y0001", 2, Some("c")),
        line(2, "Y0002", 3, Some("c")),
        line(3, "Y0003", 4, Some("c")),
    ];
    let (pairs, limits, mix) = stress::expected(&ptp, &mue, &claim);
    assert_eq!(pairs.len(), 1);
    assert_eq!(
        (pairs[0].column_one_line_id, pairs[0].column_two_line_id),
        (1, 2)
    );
    assert_eq!(
        pairs[0].outcome,
        ncci::PtpOutcome::SupportedModifierRequired
    );
    assert_eq!(
        limits
            .iter()
            .map(|f| (f.scope, f.max_units, f.total_units))
            .collect::<Vec<_>>(),
        [
            (mue::MueScope::ClaimLine { line_id: 1 }, 1, 2),
            (mue::MueScope::AbsoluteServiceDayLimit, 2, 3),
            (mue::MueScope::ServiceDayClinicalReview, 3, 4),
        ],
    );
    // (1,2) hits; (2,1), (1,3) and (2,3) are interior misses; (3,1) and (3,2) have no bucket.
    assert_eq!(
        mix,
        ProbeMix {
            probes: 6,
            hits: 1,
            interior_misses: 3,
            bucket_misses: 2
        }
    );

    let claim = [line(1, "Y0001", 1, None), line(2, "Y0003", 3, None)];
    let (pairs, limits, _) = stress::expected(&ptp, &mue, &claim);
    assert!(pairs.is_empty());
    assert!(limits.is_empty());
}

#[test]
#[should_panic(expected = "distinct codes")]
fn oracle_rejects_a_claim_with_a_split() {
    let mue = [limit("Y0001", 9, 1)];
    let claim = [
        line(1, "Y0001", 1, Some("c")),
        line(2, "Y0001", 1, Some("c")),
    ];
    stress::expected(&[], &mue, &claim);
}

#[test]
fn claims_have_ten_distinct_lines() {
    let catalog = catalog(20_000);
    let claims = stress::claims(&catalog.ptp, 64, SEED);
    assert_eq!(claims.len(), 64);
    assert_eq!(claims, stress::claims(&catalog.ptp, 64, SEED));
    assert_ne!(claims, stress::claims(&catalog.ptp, 64, SEED + 1));
    for claim in &claims {
        assert_eq!(claim.len(), 10);
        let ids: Vec<u64> = claim.iter().map(|l| l.id).collect();
        assert_eq!(ids, (1..=10).collect::<Vec<_>>());
        let codes: HashSet<_> = claim.iter().map(|l| l.code.bytes()).collect();
        assert_eq!(codes.len(), 10);
        for l in claim {
            assert!((1..=3).contains(&l.units.get()));
            assert!(l.modifiers.is_empty() && l.modifier_evidence.is_empty());
            assert_eq!(l.claim_id, Some("stress-claim"));
            assert_eq!(l.service_day, claim[0].service_day);
            assert_eq!(l.service_day.setting, Setting::Practitioner);
            assert_eq!(l.service_day.date.date(), date!(2026 - 10 - 01));
        }
    }
}

#[test]
fn claims_match_the_oracle_at_20000_rows() {
    let catalog = catalog(20_000);
    let claims = stress::claims(&catalog.ptp, 256, SEED);
    let mut total = ProbeMix::default();
    for claim in &claims {
        let (pairs, limits, mix) =
            stress::expected(&catalog.ptp_records, &catalog.mue_records, claim);
        assert_eq!(ncci::evaluate(&catalog.ptp, claim).unwrap(), pairs);
        assert_eq!(mue::evaluate(&catalog.mue, claim).unwrap(), limits);
        assert_eq!(mix.probes, 90);
        assert_eq!(mix.hits, pairs.len() as u64);
        assert_eq!(
            mix.probes,
            mix.hits + mix.interior_misses + mix.bucket_misses
        );
        total.probes += mix.probes;
        total.hits += mix.hits;
        total.interior_misses += mix.interior_misses;
        total.bucket_misses += mix.bucket_misses;
    }
    assert_eq!(total.probes, 256 * 90);
    assert!(total.hits > 0 && total.interior_misses > 0, "{total:?}");

    let checked = stress::check_claims(
        &catalog.ptp,
        &catalog.mue,
        &catalog.ptp_records,
        &catalog.mue_records,
        &claims,
    );
    assert_eq!(checked, Ok(total));
}

#[test]
fn a_wrong_oracle_input_is_reported() {
    let catalog = catalog(20_000);
    let claims = stress::claims(&catalog.ptp, 256, SEED);
    let result = stress::check_claims(
        &catalog.ptp,
        &catalog.mue,
        &[],
        &catalog.mue_records,
        &claims,
    );
    assert!(result.unwrap_err().contains("PTP findings differ"));

    let loose: Vec<MueRecord> = catalog
        .mue_records
        .iter()
        .map(|row| MueRecord {
            max_units: 1_000,
            ..*row
        })
        .collect();
    let result = stress::check_claims(
        &catalog.ptp,
        &catalog.mue,
        &catalog.ptp_records,
        &loose,
        &claims,
    );
    assert!(result.unwrap_err().contains("MUE findings differ"));

    let flipped: Vec<_> = catalog
        .ptp_records
        .iter()
        .map(|row| NcciPtpRecord {
            ccmi: if row.ccmi == 9 { 9 } else { 1 - row.ccmi },
            ..*row
        })
        .collect();
    let result = stress::check_claims(
        &catalog.ptp,
        &catalog.mue,
        &flipped,
        &catalog.mue_records,
        &claims,
    );
    assert!(result.unwrap_err().contains("PTP findings differ"));

    let rotated: Vec<_> = catalog
        .mue_records
        .iter()
        .map(|row| MueRecord {
            mai: row.mai % 3 + 1,
            ..*row
        })
        .collect();
    let result = stress::check_claims(
        &catalog.ptp,
        &catalog.mue,
        &catalog.ptp_records,
        &rotated,
        &claims,
    );
    assert!(result.unwrap_err().contains("MUE findings differ"));
}

fn open_pair(dir: &Path) -> (MmapPtpIndex, MmapMueIndex) {
    (
        MmapPtpIndex::open(dir.join("ncci_ptp.bin")).unwrap(),
        MmapMueIndex::open(dir.join("ncci_mue.bin")).unwrap(),
    )
}

#[test]
fn cli_builds_and_verifies_a_stress_catalog() {
    let dir = tempdir().unwrap();
    let out = dir.path().join("stress");
    let output = index_cli(&[
        "synthetic-stress",
        "--rows",
        "20000",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report = stdout(&output);
    for fact in [
        "seed: 0x4d56535452455353\n",
        "PTP rows: 20000\n",
        "claims checked: 4096\n",
        "probes: 368640\n",
    ] {
        assert!(report.contains(fact), "missing {fact:?} in {report}");
    }
    for label in [
        "directory buckets: ",
        "bucket size min/median/max: ",
        "MUE rows: ",
        "PTP payload bytes: ",
        "PTP file bytes: ",
        "MUE file bytes: ",
        "pair filter bytes: ",
        "hits: ",
        "interior misses: ",
        "bucket misses: ",
    ] {
        assert!(report.contains(label), "missing {label:?} in {report}");
    }
    let (ptp, mue) = open_pair(&out);
    assert_eq!(ptp.records().len(), 20_000);
    assert_eq!(ptp.catalog_mode(), CatalogMode::SyntheticStress);
    assert_eq!(ptp.format_version(), 2);
    assert_eq!(mue.catalog_mode(), CatalogMode::SyntheticStress);
    let ptp_file = fs::metadata(out.join("ncci_ptp.bin")).unwrap().len();
    assert!(report.contains(&format!("PTP file bytes: {ptp_file}\n")));
    assert!(report.contains(&format!("MUE rows: {}\n", mue.records().len())));

    // The files are the library's output for the same seed.
    assert_eq!(
        fs::read(out.join("ncci_ptp.bin")).unwrap(),
        ptp_bytes(20_000, SEED)
    );
    assert_eq!(
        fs::read(out.join("ncci_mue.bin")).unwrap(),
        mue_bytes(20_000, SEED)
    );
}

#[test]
fn cli_rejects_bad_arguments() {
    let dir = tempdir().unwrap();
    let out = dir.path().join("out");
    let out = out.to_str().unwrap();
    let cases: [&[&str]; 12] = [
        &["synthetic-stress", "--rows", "0", "--out", out],
        &["synthetic-stress", "--rows", "x", "--out", out],
        &["synthetic-stress", "--rows", "+5", "--out", out],
        &["synthetic-stress", "--rows", "-5", "--out", out],
        &["synthetic-stress", "--rows", "4294967296", "--out", out],
        &["synthetic-stress", "--rows", "", "--out", out],
        &[
            "synthetic-stress",
            "--rows",
            "5",
            "--rows",
            "6",
            "--out",
            out,
        ],
        &[
            "synthetic-stress",
            "--rows",
            "5",
            "--out",
            out,
            "--out",
            out,
        ],
        &["synthetic-stress", "--out", out],
        &["synthetic-stress", "--rows"],
        &["synthetic-stress", "--rows", "5", "--data", out],
        &["synthetic-stress", "5"],
    ];
    for args in cases {
        let output = index_cli(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(stderr.trim_end().lines().count(), 1, "{args:?}: {stderr}");
    }
    assert!(!Path::new(out).exists());
}

#[test]
fn usage_names_the_stress_command() {
    let output = index_cli(&[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr.contains("synthetic-stress --rows N [--out DIR]"),
        "{stderr}"
    );
}

#[test]
fn ordinary_synthetic_catalog_rejects_stress_codes() {
    let row = |col1, col2| edit(col1, col2, 1, (100, NO_DELETION_DATE), 1);
    let mut output = Vec::new();
    assert!(write_ptp(
        &mut output,
        CatalogMode::Synthetic,
        vec![row("Y0001", "Y9001")]
    )
    .is_err());
    assert!(write_ptp(
        &mut output,
        CatalogMode::Synthetic,
        vec![row("Y9001", "Y0001")]
    )
    .is_err());
    assert!(write_mue(
        &mut output,
        CatalogMode::Synthetic,
        vec![limit("Y0001", 1, 1)]
    )
    .is_err());
    // The same rows are fine for the stress catalog and for the synthetic range.
    write_ptp(
        &mut output,
        CatalogMode::SyntheticStress,
        vec![row("Y0001", "Y9001")],
    )
    .unwrap();
    write_ptp(
        &mut output,
        CatalogMode::Synthetic,
        vec![row("Y9001", "Y9002")],
    )
    .unwrap();
}
