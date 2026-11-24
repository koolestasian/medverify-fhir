use medverify_core::cms::{self, CmsError};
use medverify_core::encoding::{encode_code_b36, encode_epoch_day, NO_DELETION_DATE};
use medverify_core::index::{CatalogMode, MmapMueIndex, MmapPtpIndex};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::{tempdir, TempDir};
use time::{Date, Month};

const CANARY: &str = "CANARY-DESCRIPTOR-7Q";
const FILES: [&str; 2] = ["ncci_ptp.bin", "ncci_mue.bin"];
const USAGE: &str = "usage: medverify-index <synthetic|licensed|cms-tsv> --data DIR --out DIR";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cms_tsv")
}

fn index_cli(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_medverify-index"))
        .args(args)
        .output()
        .unwrap()
}

fn build(source: &str, data: &Path, out: &Path) -> Output {
    index_cli(&[
        source.as_ref(),
        "--data".as_ref(),
        data.as_os_str(),
        "--out".as_ref(),
        out.as_os_str(),
    ])
}

fn cms_tsv(data: &Path, out: &Path) -> Output {
    build("cms-tsv", data, out)
}

/// A copy of the release fixture that a test may mutate.
fn release_copy() -> (TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let data = dir.path().join("release");
    fs::create_dir(&data).unwrap();
    for entry in fs::read_dir(fixtures().join("release")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), data.join(entry.file_name())).unwrap();
    }
    (dir, data)
}

type Mutation = Box<dyn Fn(&mut Vec<Vec<String>>)>;

fn lines(data: &Path, file: &str) -> Vec<Vec<String>> {
    fs::read_to_string(data.join(file))
        .unwrap()
        .lines()
        .map(|line| line.split('\t').map(str::to_owned).collect())
        .collect()
}

fn store(data: &Path, file: &str, rows: Vec<Vec<String>>) {
    let text: String = rows.iter().map(|row| row.join("\t") + "\n").collect();
    fs::write(data.join(file), text).unwrap();
}

/// Replaces one cell. `line` is the 1-based physical line, `col` is 0-based.
fn set_cell(data: &Path, file: &str, line: usize, col: usize, value: &str) {
    let mut rows = lines(data, file);
    rows[line - 1][col] = value.to_owned();
    store(data, file, rows);
}

fn drop_cell(data: &Path, file: &str, line: usize, col: usize) {
    let mut rows = lines(data, file);
    rows[line - 1].remove(col);
    store(data, file, rows);
}

fn edit_lines(data: &Path, file: &str, change: impl FnOnce(&mut Vec<Vec<String>>)) {
    let mut rows = lines(data, file);
    change(&mut rows);
    store(data, file, rows);
}

fn edit_manifest(data: &Path, change: impl FnOnce(String) -> String) {
    let path = data.join("manifest.tsv");
    fs::write(&path, change(fs::read_to_string(&path).unwrap())).unwrap();
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn output_bytes(out: &Path) -> Vec<Vec<u8>> {
    FILES
        .iter()
        .map(|name| fs::read(out.join(name)).unwrap())
        .collect()
}

/// A failed run prints one fixed line, publishes nothing and echoes no cell text.
fn assert_rejected(output: &Output, expected: &[&str], leaked: &[&str]) {
    let message = stderr(output);
    assert_eq!(output.status.code(), Some(1), "{message}");
    assert!(output.stdout.is_empty());
    assert_eq!(message.trim_end().lines().count(), 1, "{message}");
    for text in expected {
        assert!(message.contains(text), "missing {text:?} in {message}");
    }
    for text in leaked.iter().chain(&[CANARY]) {
        assert!(!message.contains(text), "leaked {text:?} in {message}");
    }
}

fn day(year: i32, month: Month, day: u8) -> u16 {
    encode_epoch_day(Date::from_calendar_date(year, month, day).unwrap()).unwrap()
}

#[test]
fn usage_names_all_three_sources() {
    let output = index_cli(&[]);
    assert_rejected(&output, &[USAGE], &[]);
    let output = index_cli(&["csv".as_ref()]);
    assert_rejected(&output, &[USAGE], &[]);
}

#[test]
fn load_decodes_every_edit_kind() {
    let (ptp, mue) = cms::load(&fixtures().join("release")).unwrap();
    assert_eq!((ptp.len(), mue.len()), (8, 7));
    let pair = |a: &str, b: &str, setting, eff| {
        *ptp.iter()
            .find(|r| {
                r.col1_b36 == encode_code_b36(a).unwrap()
                    && r.col2_b36 == encode_code_b36(b).unwrap()
                    && r.setting_flags == setting
                    && r.eff_days == eff
            })
            .unwrap()
    };
    let open = pair("Y9001", "Y9002", 1, day(2026, Month::October, 1));
    assert_eq!(
        (open.ccmi, open.del_days, open.flags),
        (1, NO_DELETION_DATE, 0)
    );
    let dated = pair("Y9001", "Y9003", 1, day(2000, Month::January, 1));
    assert_eq!(dated.del_days, day(2027, Month::January, 1));
    assert_eq!((dated.ccmi, dated.flags), (0, 1));
    let retro = pair("Y9001", "Y9004", 1, day(2026, Month::March, 1));
    assert_eq!((retro.ccmi, retro.del_days), (9, retro.eff_days));
    let hospital = pair("Y9001", "Y9002", 2, day(2026, Month::January, 1));
    assert_eq!((hospital.ccmi, hospital.rationale_id), (0, 0));
    for row in &mue {
        assert_eq!((row.flags, row.reserved, row.rationale_id), (0, 0, 0));
    }
    let limit = |code: &str, setting| {
        *mue.iter()
            .find(|r| r.code_b36 == encode_code_b36(code).unwrap() && r.setting_flags == setting)
            .unwrap()
    };
    assert_eq!(limit("Y9012", 1).max_units, 65535);
    assert_eq!(limit("Y9013", 1).max_units, 0);
    assert_eq!(limit("Y9011", 1).mai, 1);
    assert_eq!(limit("Y9012", 1).mai, 2);
    assert_eq!(limit("Y9013", 1).mai, 3);
    assert_eq!(limit("Y9015", 4).mai, 1);
    let hospital_limit = limit("Y9011", 2);
    assert_eq!(hospital_limit.del_days, day(2027, Month::January, 1));
    assert_eq!(hospital_limit.eff_days, day(2026, Month::October, 1));
    assert_eq!(limit("Y9011", 4).del_days, NO_DELETION_DATE);
}

#[test]
fn matches_the_normalized_csv_build_byte_for_byte() {
    let dir = tempdir().unwrap();
    let csv_out = dir.path().join("csv");
    let tsv_out = dir.path().join("tsv");
    let csv = build("licensed", &fixtures().join("normalized"), &csv_out);
    assert!(csv.status.success(), "{}", stderr(&csv));
    let tsv = cms_tsv(&fixtures().join("release"), &tsv_out);
    assert!(tsv.status.success(), "{}", stderr(&tsv));
    assert_eq!(output_bytes(&csv_out), output_bytes(&tsv_out));
    let summary = "v323r0: 8 PTP records, 7 MUE records\n";
    assert_eq!(String::from_utf8_lossy(&csv.stdout), summary);
    assert_eq!(String::from_utf8_lossy(&tsv.stdout), summary);

    let ptp = MmapPtpIndex::open(tsv_out.join("ncci_ptp.bin")).unwrap();
    let mue = MmapMueIndex::open(tsv_out.join("ncci_mue.bin")).unwrap();
    assert_eq!(ptp.catalog_mode(), CatalogMode::Licensed);
    assert_eq!((ptp.records().len(), mue.records().len()), (8, 7));
}

#[test]
fn descriptor_text_never_reaches_the_output() {
    let dir = tempdir().unwrap();
    let out = dir.path().join("out");
    let release = fixtures().join("release");
    let sources = fs::read_dir(&release).unwrap().filter_map(|e| {
        let text = fs::read_to_string(e.unwrap().path()).unwrap();
        Some(text).filter(|t| t.contains(CANARY))
    });
    assert_eq!(sources.count(), 6, "the canary is in every table");
    let output = cms_tsv(&release, &out);
    assert!(output.status.success());
    let all = [
        output_bytes(&out).concat(),
        output.stdout.clone(),
        output.stderr.clone(),
    ]
    .concat();
    assert!(!all.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()));
    assert!(!all.windows(9).any(|w| w == b"synthetic"));
}

#[test]
fn malformed_rows_name_the_manifest_row_line_and_field() {
    // (file, line, column, new value, manifest row, table kind, field)
    let ptp = "ptp practitioner";
    let cases: &[(&str, usize, usize, &str, usize, &str, &str)] = &[
        ("ptp_practitioner_1.txt", 4, 0, "Y90!1", 2, ptp, "column 1"),
        ("ptp_practitioner_1.txt", 4, 0, "y9001", 2, ptp, "column 1"),
        (
            "ptp_practitioner_1.txt",
            4,
            0,
            "Y9001-CANARY-DESCRIPTOR-7Q",
            2,
            ptp,
            "column 1",
        ),
        ("ptp_practitioner_1.txt", 4, 1, "Y900", 2, ptp, "column 2"),
        ("ptp_practitioner_1.txt", 4, 1, "", 2, ptp, "column 2"),
        (
            "ptp_practitioner_1.txt",
            4,
            3,
            "20261301",
            2,
            ptp,
            "effective date",
        ),
        (
            "ptp_practitioner_1.txt",
            4,
            3,
            "2026-10-01",
            2,
            ptp,
            "effective date",
        ),
        (
            "ptp_practitioner_1.txt",
            4,
            3,
            "19891231",
            2,
            ptp,
            "effective date",
        ),
        (
            "ptp_practitioner_1.txt",
            4,
            3,
            "20260230",
            2,
            ptp,
            "effective date",
        ),
        (
            "ptp_practitioner_1.txt",
            5,
            4,
            "20261301",
            2,
            ptp,
            "deletion date",
        ),
        (
            "ptp_practitioner_1.txt",
            5,
            4,
            "soon",
            2,
            ptp,
            "deletion date",
        ),
        // Indicator 9 stays an error without a deletion date equal to the effective date.
        ("ptp_practitioner_1.txt", 6, 4, "", 2, ptp, "deletion date"),
        (
            "ptp_practitioner_1.txt",
            5,
            4,
            "19991231",
            2,
            ptp,
            "deletion date",
        ),
        (
            "ptp_practitioner_1.txt",
            5,
            4,
            "21690606",
            2,
            ptp,
            "deletion date",
        ),
        (
            "ptp_practitioner_1.txt",
            6,
            4,
            "20260302",
            2,
            ptp,
            "deletion date",
        ),
        ("ptp_practitioner_1.txt", 6, 4, "*", 2, ptp, "deletion date"),
        (
            "ptp_practitioner_1.txt",
            4,
            5,
            "2",
            2,
            ptp,
            "modifier indicator",
        ),
        (
            "ptp_practitioner_1.txt",
            4,
            5,
            "",
            2,
            ptp,
            "modifier indicator",
        ),
        (
            "ptp_practitioner_1.txt",
            4,
            5,
            "1-CANARY-DESCRIPTOR-7Q",
            2,
            ptp,
            "modifier indicator",
        ),
        ("ptp_practitioner_1.txt", 4, 2, "x", 2, ptp, "prior to 1996"),
        (
            "ptp_practitioner_1.txt",
            4,
            2,
            "**",
            2,
            ptp,
            "prior to 1996",
        ),
        (
            "ptp_practitioner_1.txt",
            4,
            2,
            "N-CANARY-DESCRIPTOR-7Q",
            2,
            ptp,
            "prior to 1996",
        ),
        ("ptp_practitioner_2.txt", 3, 0, "Y9-05", 3, ptp, "column 1"),
        (
            "ptp_hospital.txt",
            3,
            3,
            "20100100",
            4,
            "ptp hospital",
            "effective date",
        ),
        (
            "mue_practitioner.txt",
            2,
            0,
            "y9011",
            5,
            "mue practitioner",
            "code",
        ),
        (
            "mue_practitioner.txt",
            2,
            0,
            "Y9011X",
            5,
            "mue practitioner",
            "code",
        ),
        (
            "mue_practitioner.txt",
            3,
            1,
            "65536",
            5,
            "mue practitioner",
            "units",
        ),
        (
            "mue_practitioner.txt",
            3,
            1,
            "999999",
            5,
            "mue practitioner",
            "units",
        ),
        (
            "mue_practitioner.txt",
            3,
            1,
            "-1",
            5,
            "mue practitioner",
            "units",
        ),
        (
            "mue_practitioner.txt",
            3,
            1,
            "1.5",
            5,
            "mue practitioner",
            "units",
        ),
        (
            "mue_practitioner.txt",
            3,
            1,
            "CANARY-DESCRIPTOR-7Q",
            5,
            "mue practitioner",
            "units",
        ),
        (
            "mue_practitioner.txt",
            3,
            1,
            "",
            5,
            "mue practitioner",
            "units",
        ),
        (
            "mue_practitioner.txt",
            4,
            2,
            "4 Line Edit",
            5,
            "mue practitioner",
            "adjudication indicator",
        ),
        (
            "mue_practitioner.txt",
            4,
            2,
            "",
            5,
            "mue practitioner",
            "adjudication indicator",
        ),
        (
            "mue_practitioner.txt",
            4,
            2,
            "12 Line Edit",
            5,
            "mue practitioner",
            "adjudication indicator",
        ),
        (
            "mue_practitioner.txt",
            4,
            2,
            "x1 Line Edit",
            5,
            "mue practitioner",
            "adjudication indicator",
        ),
        (
            "mue_hospital.txt",
            3,
            2,
            "0 CANARY-DESCRIPTOR-7Q",
            6,
            "mue hospital",
            "adjudication indicator",
        ),
        ("mue_dme.txt", 4, 0, "Y9011-", 7, "mue dme", "code"),
    ];
    for &(file, line, col, value, row, kind, field) in cases {
        let (dir, data) = release_copy();
        set_cell(&data, file, line, col, value);
        let out = dir.path().join("out");
        let output = cms_tsv(&data, &out);
        let expected =
            format!("cms-tsv: manifest row {row} ({kind}), table row {line}, field {field}: ");
        let leaked: Vec<&str> = Some(value).filter(|v| v.len() > 2).into_iter().collect();
        assert!(
            !output.status.success(),
            "{file} {line} {col} {value:?} was accepted"
        );
        assert_rejected(&output, &[&expected], &leaked);
        assert!(!out.exists(), "{file} {line} {col} {value:?}");
    }
}

#[test]
fn missing_columns_are_rejected() {
    for (file, line, col, row, kind, count) in [
        ("ptp_practitioner_1.txt", 4, 2, 2, "ptp practitioner", 7),
        ("ptp_practitioner_1.txt", 5, 6, 2, "ptp practitioner", 7),
        ("ptp_hospital.txt", 2, 0, 4, "ptp hospital", 7),
        ("mue_practitioner.txt", 2, 1, 5, "mue practitioner", 4),
        ("mue_dme.txt", 3, 3, 7, "mue dme", 4),
    ] {
        let (dir, data) = release_copy();
        drop_cell(&data, file, line, col);
        let output = cms_tsv(&data, &dir.path().join("out"));
        let expected = format!(
            "cms-tsv: manifest row {row} ({kind}), table row {line}, field column count: expected {count} columns"
        );
        assert_rejected(&output, &[&expected], &[]);
        // An extra cell is also a layout violation.
        let (dir, data) = release_copy();
        edit_lines(&data, file, |rows| rows[line - 1].push("x".into()));
        assert_rejected(&cms_tsv(&data, &dir.path().join("out")), &[&expected], &[]);
    }
}

#[test]
fn header_drift_is_rejected() {
    let drift = |row: usize, kind: &str| format!("cms-tsv: manifest row {row} ({kind})");
    let cases: Vec<(&str, usize, &str, Mutation)> = vec![
        (
            "ptp_hospital.txt",
            4,
            "ptp hospital",
            Box::new(|rows| rows[0].swap(0, 1)),
        ),
        (
            "ptp_hospital.txt",
            4,
            "ptp hospital",
            Box::new(|rows| rows[0].swap(3, 4)),
        ),
        (
            "ptp_hospital.txt",
            4,
            "ptp hospital",
            Box::new(|rows| rows[0][3] = "Start Date".into()),
        ),
        (
            "ptp_hospital.txt",
            4,
            "ptp hospital",
            Box::new(|rows| rows[0].push("Extra".into())),
        ),
        (
            "ptp_hospital.txt",
            4,
            "ptp hospital",
            Box::new(|rows| {
                rows[0].pop();
            }),
        ),
        (
            "ptp_hospital.txt",
            4,
            "ptp hospital",
            Box::new(|rows| {
                rows.remove(0);
            }),
        ),
        (
            "ptp_practitioner_1.txt",
            2,
            "ptp practitioner",
            Box::new(|rows| {
                rows.remove(2);
            }),
        ),
        (
            "mue_hospital.txt",
            6,
            "mue hospital",
            Box::new(|rows| rows[0].swap(1, 2)),
        ),
        (
            "mue_hospital.txt",
            6,
            "mue hospital",
            Box::new(|rows| rows[0][2] = "Indicator".into()),
        ),
        (
            "mue_hospital.txt",
            6,
            "mue hospital",
            Box::new(|rows| rows[0].push("Extra".into())),
        ),
        (
            "mue_hospital.txt",
            6,
            "mue hospital",
            Box::new(|rows| {
                rows.remove(0);
            }),
        ),
        (
            "mue_dme.txt",
            7,
            "mue dme",
            Box::new(|rows| {
                rows.remove(1);
            }),
        ),
    ];
    for (file, row, kind, change) in cases {
        let (dir, data) = release_copy();
        edit_lines(&data, file, change);
        let out = dir.path().join("out");
        let output = cms_tsv(&data, &out);
        assert_rejected(
            &output,
            &[
                &drift(row, kind),
                ": header drift, columns differ from the CMS layout",
            ],
            &[],
        );
        assert!(!out.exists(), "{file}");
    }
}

#[test]
fn preamble_is_limited_to_twenty_lines() {
    for (count, accepted) in [(20, true), (21, false)] {
        let (dir, data) = release_copy();
        edit_lines(&data, "ptp_hospital.txt", |rows| {
            let notes = (0..count).map(|i| vec![format!("note {i} {CANARY}")]);
            rows.splice(0..0, notes);
        });
        let out = dir.path().join("out");
        let output = cms_tsv(&data, &out);
        if accepted {
            assert!(output.status.success(), "{}", stderr(&output));
        } else {
            assert_rejected(
                &output,
                &["manifest row 4 (ptp hospital), table row 21: header drift"],
                &[],
            );
            assert!(!out.exists());
        }
    }
}

#[test]
fn manifest_errors_are_rejected() {
    let ptp_hospital = "ptp\thospital\tptp_hospital.txt\t\t";
    let mue_practitioner = "mue\tpractitioner\tmue_practitioner.txt\t2026-10-01\t";
    let cases: Vec<(&str, String, String, &str)> = vec![
        (
            "wrong header",
            "role\tsetting".into(),
            "kind\tsetting".into(),
            "cms-tsv: manifest row 1, field header: unexpected columns",
        ),
        (
            "unknown role",
            ptp_hospital.into(),
            ptp_hospital.replacen("ptp", "pair", 1),
            "manifest row 4, field role: expected ptp or mue",
        ),
        (
            "unknown setting",
            ptp_hospital.into(),
            ptp_hospital.replace("hospital\t", "clinic\t"),
            "manifest row 4, field setting",
        ),
        (
            "dme ptp",
            ptp_hospital.into(),
            ptp_hospital.replace("hospital\t", "dme\t"),
            "manifest row 4, field setting",
        ),
        (
            "ptp with effective",
            ptp_hospital.into(),
            "ptp\thospital\tptp_hospital.txt\t2026-01-01\t".into(),
            "manifest row 4, field effective: must be empty for ptp",
        ),
        (
            "ptp with end",
            ptp_hospital.into(),
            "ptp\thospital\tptp_hospital.txt\t\t2027-01-01".into(),
            "manifest row 4, field end_exclusive: must be empty for ptp",
        ),
        (
            "mue without effective",
            mue_practitioner.into(),
            "mue\tpractitioner\tmue_practitioner.txt\t\t".into(),
            "manifest row 5, field effective: expected YYYY-MM-DD",
        ),
        (
            "mue bad effective",
            mue_practitioner.into(),
            mue_practitioner.replace("2026-10-01", "20261001"),
            "manifest row 5, field effective",
        ),
        (
            "mue bad end",
            mue_practitioner.into(),
            format!("{mue_practitioner}2026-13-01"),
            "manifest row 5, field end_exclusive: expected YYYY-MM-DD",
        ),
        (
            "mue end not after effective",
            mue_practitioner.into(),
            format!("{mue_practitioner}2026-10-01"),
            "manifest row 5, field end_exclusive: must be after effective",
        ),
        (
            "parent path",
            ptp_hospital.into(),
            ptp_hospital.replace("ptp_hospital.txt", "../x"),
            "manifest row 4, field file: expected a bare file name",
        ),
        (
            "embedded parent path",
            ptp_hospital.into(),
            ptp_hospital.replace("ptp_hospital.txt", "a..b.txt"),
            "manifest row 4, field file",
        ),
        (
            "subdirectory",
            ptp_hospital.into(),
            ptp_hospital.replace("ptp_hospital.txt", "sub/x.txt"),
            "manifest row 4, field file",
        ),
        (
            "backslash",
            ptp_hospital.into(),
            ptp_hospital.replace("ptp_hospital.txt", "sub\\x.txt"),
            "manifest row 4, field file",
        ),
        (
            "absolute path",
            ptp_hospital.into(),
            ptp_hospital.replace("ptp_hospital.txt", "/etc/x"),
            "manifest row 4, field file",
        ),
        (
            "drive path",
            ptp_hospital.into(),
            ptp_hospital.replace("ptp_hospital.txt", "C:x"),
            "manifest row 4, field file",
        ),
        (
            "empty file",
            ptp_hospital.into(),
            ptp_hospital.replace("ptp_hospital.txt", ""),
            "manifest row 4, field file",
        ),
        (
            "duplicate file",
            "ptp\tpractitioner\tptp_practitioner_2.txt".into(),
            "ptp\tpractitioner\tptp_practitioner_1.txt".into(),
            "manifest row 3, field file: listed more than once",
        ),
        (
            "duplicate file by case",
            "ptp\tpractitioner\tptp_practitioner_2.txt".into(),
            "ptp\tpractitioner\tPTP_Practitioner_1.TXT".into(),
            "manifest row 3, field file: listed more than once",
        ),
        (
            "too few columns",
            ptp_hospital.into(),
            "ptp\thospital\tptp_hospital.txt".into(),
            "manifest row 4, field columns: expected 5 columns",
        ),
        (
            "too many columns",
            ptp_hospital.into(),
            format!("{ptp_hospital}\tx"),
            "manifest row 4, field columns: expected 5 columns",
        ),
        (
            "missing file",
            "ptp_hospital.txt".into(),
            "ptp_missing.txt".into(),
            "cms-tsv: manifest row 4, cannot read table file: ",
        ),
    ];
    for (name, from, to, expected) in cases {
        let (dir, data) = release_copy();
        edit_manifest(&data, |text| {
            assert!(text.contains(&from), "{name}");
            text.replacen(&from, &to, 1)
        });
        let out = dir.path().join("out");
        assert_rejected(&cms_tsv(&data, &out), &[expected], &[to.as_str()]);
        assert!(!out.exists(), "{name}");
    }
}

#[test]
fn manifest_must_name_both_roles() {
    for (role, other, expected) in [
        ("mue", "ptp", "cms-tsv: manifest has no mue rows"),
        ("ptp", "mue", "cms-tsv: manifest has no ptp rows"),
    ] {
        let (dir, data) = release_copy();
        edit_manifest(&data, |text| {
            text.lines()
                .filter(|line| !line.starts_with(&format!("{role}\t")))
                .map(|line| line.to_owned() + "\n")
                .collect()
        });
        assert!(fs::read_to_string(data.join("manifest.tsv"))
            .unwrap()
            .contains(&format!("{other}\t")));
        assert_rejected(&cms_tsv(&data, &dir.path().join("out")), &[expected], &[]);
    }
    let (dir, data) = release_copy();
    fs::write(data.join("manifest.tsv"), "").unwrap();
    let output = cms_tsv(&data, &dir.path().join("out"));
    assert_rejected(&output, &["cms-tsv: manifest row 1, field header"], &[]);
    fs::remove_file(data.join("manifest.tsv")).unwrap();
    let output = cms_tsv(&data, &dir.path().join("out"));
    assert_rejected(&output, &["cms-tsv: cannot read manifest.tsv: "], &[]);
}

#[test]
fn a_table_with_a_header_and_no_data_rows_is_rejected() {
    // (file, lines up to and including the header, manifest row, table kind)
    let cases = [
        ("ptp_practitioner_2.txt", 1, 3, "ptp practitioner"),
        ("ptp_hospital.txt", 1, 4, "ptp hospital"),
        ("mue_hospital.txt", 1, 6, "mue hospital"),
        ("mue_dme.txt", 2, 7, "mue dme"),
    ];
    for blank_rows in [0, 3] {
        for (file, header_end, row, kind) in cases {
            let (dir, data) = release_copy();
            edit_lines(&data, file, |rows| {
                rows.truncate(header_end);
                rows.extend((0..blank_rows).map(|_| vec![String::new(); 7]));
            });
            let out = dir.path().join("out");
            let output = cms_tsv(&data, &out);
            let expected = format!(
                "cms-tsv: manifest row {row} ({kind}): table has a header but no data rows"
            );
            assert_rejected(&output, &[&expected], &[]);
            // Nothing is published and no staging directory is left behind.
            assert!(!out.exists(), "{file} with {blank_rows} blank rows");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "{file}");
        }
    }
    // The other tables of the same role still have rows, so only the empty one is at fault.
    let (_dir, data) = release_copy();
    edit_lines(&data, "ptp_practitioner_2.txt", |rows| rows.truncate(1));
    let error = cms::load(&data).unwrap_err();
    assert!(matches!(
        error,
        CmsError::EmptyTable { table } if table.manifest_row == 3
    ));
    assert_eq!(
        error.to_string(),
        "cms-tsv: manifest row 3 (ptp practitioner): table has a header but no data rows"
    );
}

#[test]
fn swapped_mue_files_are_rejected_by_the_setting_in_their_header() {
    // (setting, manifest row, file, table row of the header)
    let tables = [
        ("practitioner", 5, "mue_practitioner.txt", 1),
        ("hospital", 6, "mue_hospital.txt", 1),
        ("dme", 7, "mue_dme.txt", 2),
    ];
    for (a, b) in [(0, 1), (0, 2), (1, 2)] {
        let ((first_setting, first_row, first_file, _), (_, _, second_file, second_header)) =
            (tables[a], tables[b]);
        let (dir, data) = release_copy();
        edit_manifest(&data, |text| {
            text.replace(first_file, "@first@")
                .replace(second_file, first_file)
                .replace("@first@", second_file)
        });
        let out = dir.path().join("out");
        let output = cms_tsv(&data, &out);
        // The first row of the manifest that no longer matches its file is reported.
        let expected = format!(
            "cms-tsv: manifest row {first_row} (mue {first_setting}), table row {second_header}: header names a different setting than the manifest"
        );
        assert_rejected(&output, &[&expected], &[]);
        assert!(!out.exists(), "{first_file} and {second_file}");
    }
    // Each table in its own place is accepted, and the check ignores case and punctuation.
    let (dir, data) = release_copy();
    swap_text(
        &data,
        "mue_practitioner.txt",
        "Practitioner Services",
        "PRACTITIONER-services",
    );
    let output = cms_tsv(&data, &dir.path().join("out"));
    assert!(output.status.success(), "{}", stderr(&output));
    // A byte that is not ASCII inside the setting word breaks the match.
    let (dir, data) = release_copy();
    let bytes = fs::read(data.join("mue_practitioner.txt")).unwrap();
    let at = bytes
        .windows(12)
        .position(|w| w == b"Practitioner")
        .unwrap();
    let mut changed = bytes[..at].to_vec();
    changed.extend_from_slice(b"Practiti\xf6ner");
    changed.extend_from_slice(&bytes[at + 12..]);
    fs::write(data.join("mue_practitioner.txt"), changed).unwrap();
    let output = cms_tsv(&data, &dir.path().join("out"));
    assert_rejected(
        &output,
        &["cms-tsv: manifest row 5 (mue practitioner), table row 1: header names a different setting"],
        &[],
    );
}

/// Replaces the first occurrence of `from` in a file.
fn swap_text(data: &Path, file: &str, from: &str, to: &str) {
    let text = fs::read_to_string(data.join(file)).unwrap();
    assert!(text.contains(from));
    fs::write(data.join(file), text.replacen(from, to, 1)).unwrap();
}

#[test]
fn adjudication_indicator_accepts_one_digit_then_an_optional_space_and_text() {
    let indicator = |data: &Path, value: &str| {
        set_cell(data, "mue_practitioner.txt", 2, 2, value);
        cms::load(data).map(|(_, mue)| {
            mue.iter()
                .find(|r| r.code_b36 == encode_code_b36("Y9011").unwrap() && r.setting_flags == 1)
                .unwrap()
                .mai
        })
    };
    let (_dir, data) = release_copy();
    for (value, mai) in [
        ("1", 1),
        ("2", 2),
        ("3", 3),
        ("1 Line Edit", 1),
        ("2 Date of Service Edit: Policy", 2),
        ("3 Date of Service Edit: Clinical", 3),
        ("1  two spaces", 1),
        ("2 7", 2),
        (" 3 ", 3),
    ] {
        assert_eq!(indicator(&data, value).unwrap(), mai, "{value:?}");
    }
    for value in [
        "1.5",
        "12",
        "1-x",
        "1Line",
        "0",
        "4",
        "",
        "9 Line Edit",
        "x1",
        "1\u{e9}",
        "1:",
        "1\u{a0}Line",
        ".1",
    ] {
        let error = indicator(&data, value).unwrap_err();
        assert!(
            matches!(
                error,
                CmsError::Row {
                    line: 2,
                    field: "adjudication indicator",
                    ..
                }
            ),
            "{value:?}: {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.starts_with(
                "cms-tsv: manifest row 5 (mue practitioner), table row 2, field adjudication indicator: "
            ),
            "{message}"
        );
        assert!(
            value.len() <= 2 || !message.contains(value),
            "leaked {value:?}"
        );
    }
}

#[test]
fn partial_failure_publishes_nothing_and_keeps_a_previous_build() {
    let (dir, data) = release_copy();
    let out = dir.path().join("out");
    // The first table is valid and the last one is not.
    set_cell(&data, "mue_dme.txt", 4, 1, "70000");
    let failed = cms_tsv(&data, &out);
    assert_rejected(
        &failed,
        &["manifest row 7 (mue dme), table row 4, field units"],
        &[],
    );
    assert!(!out.exists());
    let staging = fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(staging, 1, "no staging directory is left behind");

    let good = cms_tsv(&fixtures().join("release"), &out);
    assert!(good.status.success());
    let before = output_bytes(&out);
    let failed = cms_tsv(&data, &out);
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(output_bytes(&out), before);

    // A valid build with different contents must not replace the published pair.
    set_cell(&data, "mue_dme.txt", 4, 1, "2");
    let different = cms_tsv(&data, &out);
    assert_rejected(
        &different,
        &["destination contains a different index; use a fresh directory"],
        &[],
    );
    assert_eq!(output_bytes(&out), before);
    // The same release again is accepted.
    let same = cms_tsv(&fixtures().join("release"), &out);
    assert!(same.status.success());
    assert_eq!(output_bytes(&out), before);
}

#[test]
fn writer_validation_still_applies() {
    let (dir, data) = release_copy();
    let out = dir.path().join("out");
    edit_lines(&data, "ptp_practitioner_1.txt", |rows| {
        let row = ["Y9001", "Y9002", "", "20261101", "20261201", "0", CANARY];
        rows.push(row.iter().map(|c| c.to_string()).collect());
    });
    let output = cms_tsv(&data, &out);
    // Both rows of one file are named, with the field that disagrees and no cell text.
    assert_rejected(
        &output,
        &["cms-tsv: conflicting overlapping edits: manifest row 2 (ptp practitioner) table row 4 and manifest row 2 (ptp practitioner) table row 7 overlap with a different modifier indicator"],
        &[CANARY],
    );
    assert!(!out.exists());

    let good = cms_tsv(&fixtures().join("release"), &out);
    assert!(good.status.success());
    let before = output_bytes(&out);
    assert_eq!(cms_tsv(&data, &out).status.code(), Some(1));
    assert_eq!(output_bytes(&out), before);

    // Two quarters of MUE limits for one code that disagree on the same days.
    let (dir, data) = release_copy();
    edit_manifest(&data, |text| {
        text.replace(
            "mue\tdme\tmue_dme.txt\t2026-10-01\t",
            "mue\tdme\tmue_dme.txt\t2026-10-01\t\nmue\tdme\tmue_dme_again.txt\t2026-11-01\t",
        )
    });
    fs::copy(data.join("mue_dme.txt"), data.join("mue_dme_again.txt")).unwrap();
    set_cell(&data, "mue_dme_again.txt", 3, 1, "9");
    let output = cms_tsv(&data, &dir.path().join("out"));
    // A conflict spanning two files names a row of each.
    assert_rejected(
        &output,
        &["cms-tsv: conflicting overlapping edits: manifest row 7 (mue dme) table row 3 and manifest row 8 (mue dme) table row 3 overlap with a different units"],
        &[CANARY],
    );
    let error = cms::load(&data).unwrap_err();
    assert!(
        matches!(
            error,
            CmsError::Conflict { earlier, later, field: "units" }
                if (earlier.table.manifest_row, earlier.line) == (7, 3)
                    && (later.table.manifest_row, later.line) == (8, 3)
        ),
        "{error:?}"
    );
}

#[test]
fn a_conflict_is_reported_from_either_file_order() {
    // The same disagreement listed in the opposite manifest order still names both rows,
    // earliest effective interval first.
    let (dir, data) = release_copy();
    edit_manifest(&data, |text| {
        text.replace(
            "mue\tdme\tmue_dme.txt\t2026-10-01\t",
            "mue\tdme\tmue_dme_again.txt\t2026-11-01\t\nmue\tdme\tmue_dme.txt\t2026-10-01\t",
        )
    });
    fs::copy(data.join("mue_dme.txt"), data.join("mue_dme_again.txt")).unwrap();
    set_cell(&data, "mue_dme_again.txt", 3, 1, "9");
    let output = cms_tsv(&data, &dir.path().join("out"));
    assert_rejected(
        &output,
        &["cms-tsv: conflicting overlapping edits: manifest row 8 (mue dme) table row 3 and manifest row 7 (mue dme) table row 3 overlap with a different units"],
        &[CANARY],
    );
}

#[test]
fn latin1_text_is_accepted_only_in_discarded_text() {
    let baseline = tempdir().unwrap();
    let good = cms_tsv(&fixtures().join("release"), &baseline.path().join("out"));
    assert!(good.status.success());
    let expected = output_bytes(&baseline.path().join("out"));

    let (dir, data) = release_copy();
    let swap = |file: &str, from: &str, to: &[u8]| {
        let bytes = fs::read(data.join(file)).unwrap();
        let at = bytes
            .windows(from.len())
            .position(|w| w == from.as_bytes())
            .unwrap();
        let mut changed = bytes[..at].to_vec();
        changed.extend_from_slice(to);
        changed.extend_from_slice(&bytes[at + from.len()..]);
        fs::write(data.join(file), changed).unwrap();
    };
    swap(
        "ptp_practitioner_1.txt",
        "fixtures only",
        b"fixtures \xe9only",
    );
    swap("ptp_hospital.txt", "rationale six", b"rationale s\xe9x");
    swap("mue_practitioner.txt", "1 Line Edit", b"1 Lin\xe9 Edit");
    // The setting word of a MUE header is read, so the Latin-1 byte goes elsewhere in the header.
    swap("mue_practitioner.txt", "Services MUE", b"Servic\xe9s MUE");
    let out = dir.path().join("out");
    let output = cms_tsv(&data, &out);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output_bytes(&out), expected);

    swap("ptp_hospital.txt", "Y9007", b"Y90\xe907");
    let output = cms_tsv(&data, &dir.path().join("second"));
    assert_rejected(
        &output,
        &["manifest row 4 (ptp hospital), table row 3, field column 1: "],
        &[],
    );
    assert!(!stderr(&output).contains(['\u{e9}', '\u{fffd}']));
}

#[test]
fn line_endings_and_blank_lines_do_not_change_the_output() {
    let baseline = tempdir().unwrap();
    assert!(
        cms_tsv(&fixtures().join("release"), &baseline.path().join("out"))
            .status
            .success()
    );
    let expected = output_bytes(&baseline.path().join("out"));

    let (dir, data) = release_copy();
    for entry in fs::read_dir(&data).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        let text = if path.ends_with("manifest.tsv") {
            text.replace('\n', "\r\n")
        } else {
            format!("\n{}\n", text.replace('\n', "\r\n\r\n"))
        };
        fs::write(&path, text).unwrap();
    }
    let out = dir.path().join("out");
    let output = cms_tsv(&data, &out);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output_bytes(&out), expected);
}

#[test]
fn load_errors_have_fixed_display_text() {
    let (_dir, data) = release_copy();
    set_cell(&data, "ptp_practitioner_1.txt", 4, 3, "bad");
    let error = cms::load(&data).unwrap_err();
    assert!(matches!(
        error,
        CmsError::Row {
            line: 4,
            field: "effective date",
            ..
        }
    ));
    assert_eq!(
        error.to_string(),
        "cms-tsv: manifest row 2 (ptp practitioner), table row 4, field effective date: expected YYYYMMDD"
    );
    fs::remove_file(data.join("ptp_hospital.txt")).unwrap();
    set_cell(&data, "ptp_practitioner_1.txt", 4, 3, "20261001");
    let error = cms::load(&data).unwrap_err();
    assert!(matches!(
        error,
        CmsError::Io {
            manifest_row: Some(4),
            kind: std::io::ErrorKind::NotFound
        }
    ));
}

/// The index pair built from the unmodified release fixture.
fn release_output() -> Vec<Vec<u8>> {
    let dir = tempdir().unwrap();
    let out = dir.path().join("out");
    let output = cms_tsv(&fixtures().join("release"), &out);
    assert!(output.status.success(), "{}", stderr(&output));
    output_bytes(&out)
}

const PTP_FILES: [&str; 3] = [
    "ptp_practitioner_1.txt",
    "ptp_practitioner_2.txt",
    "ptp_hospital.txt",
];

/// Inserts a row directly above the header row of a PTP table.
fn insert_above_header(data: &Path, file: &str, row: Vec<String>) {
    edit_lines(data, file, |rows| {
        let header = rows
            .iter()
            .position(|row| row[0].starts_with("Column 1"))
            .unwrap();
        rows.insert(header, row);
    });
}

#[test]
fn the_cms_title_line_above_a_ptp_header_is_accepted() {
    let expected = release_output();
    let title = "Column1/Column2 Edits";
    // MLN901346 Figure 5: the title as a single cell, or padded to the seven table columns.
    let single = vec![title.to_owned()];
    let padded: Vec<String> = std::iter::once(title.to_owned())
        .chain(std::iter::repeat_n(String::new(), 6))
        .collect();
    for file in PTP_FILES {
        for shape in [&single, &padded] {
            let (dir, data) = release_copy();
            insert_above_header(&data, file, shape.clone());
            let out = dir.path().join("out");
            let output = cms_tsv(&data, &out);
            let cells = shape.len();
            assert!(
                output.status.success(),
                "{file} with {cells} cells: {}",
                stderr(&output)
            );
            assert_eq!(output_bytes(&out), expected, "{file} with {cells} cells");
        }
    }
    // Every PTP table at once.
    let (dir, data) = release_copy();
    for file in PTP_FILES {
        insert_above_header(&data, file, padded.clone());
    }
    let out = dir.path().join("out");
    let output = cms_tsv(&data, &out);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output_bytes(&out), expected);
}

#[test]
fn a_title_line_with_other_text_is_still_header_drift() {
    let (dir, data) = release_copy();
    let mut title = vec![String::from("Column1/Column2 Edits")];
    title.extend((0..6).map(|i| {
        if i == 2 {
            "extra".into()
        } else {
            String::new()
        }
    }));
    insert_above_header(&data, "ptp_hospital.txt", title);
    let out = dir.path().join("out");
    let output = cms_tsv(&data, &out);
    assert_rejected(
        &output,
        &["cms-tsv: manifest row 4 (ptp hospital), table row 1: header drift"],
        &["extra"],
    );
    assert!(!out.exists());
}

#[test]
fn a_blank_deletion_date_means_no_deletion_like_an_asterisk() {
    let expected = release_output();
    // The rows whose deletion date is `*`: (file, table row).
    let active = [
        ("ptp_practitioner_1.txt", 4),
        ("ptp_practitioner_2.txt", 3),
        ("ptp_hospital.txt", 2),
    ];
    for (file, line) in active {
        let (dir, data) = release_copy();
        assert_eq!(lines(&data, file)[line - 1][4], "*", "{file}");
        set_cell(&data, file, line, 4, "");
        let out = dir.path().join("out");
        let output = cms_tsv(&data, &out);
        assert!(output.status.success(), "{file}: {}", stderr(&output));
        assert_eq!(output_bytes(&out), expected, "{file}");
    }
    let (dir, data) = release_copy();
    for (file, line) in active {
        set_cell(&data, file, line, 4, "");
    }
    let out = dir.path().join("out");
    let output = cms_tsv(&data, &out);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output_bytes(&out), expected);
}

#[test]
fn indicator_9_still_needs_a_deletion_date_equal_to_the_effective_date() {
    // Table row 6 of the first practitioner part is the retroactive indicator 9 edit.
    for value in ["", "*", "20260302"] {
        let (_dir, data) = release_copy();
        assert_eq!(lines(&data, "ptp_practitioner_1.txt")[5][5], "9");
        set_cell(&data, "ptp_practitioner_1.txt", 6, 4, value);
        let error = cms::load(&data).unwrap_err();
        assert_eq!(
            error.to_string(),
            "cms-tsv: manifest row 2 (ptp practitioner), table row 6, field deletion date: must equal the effective date for indicator 9",
            "{value:?}"
        );
    }
}

/// Rewrites a table with a UTF-8 byte-order mark, CRLF line ends and blank lines before the
/// given original lines (1-based line, number of blank lines). Returns the physical line
/// number that each original line now has.
fn crlf_with_blank_lines(data: &Path, file: &str, blanks: &[(usize, usize)]) -> Vec<u64> {
    let text = fs::read_to_string(data.join(file)).unwrap();
    let mut out = String::from("\u{feff}");
    let mut physical = 0;
    let mut positions = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let before = blanks
            .iter()
            .find(|&&(at, _)| at == i + 1)
            .map_or(0, |&(_, count)| count);
        for _ in 0..before {
            out.push_str("\r\n");
            physical += 1;
        }
        physical += 1;
        positions.push(physical);
        out.push_str(line);
        out.push_str("\r\n");
    }
    fs::write(data.join(file), out).unwrap();
    positions
}

#[test]
fn a_conflict_names_the_physical_rows_of_a_crlf_file_with_blank_lines() {
    let (dir, data) = release_copy();
    // The second row now starts before the first one ends, with a different indicator.
    set_cell(&data, "ptp_practitioner_2.txt", 3, 3, "20260901");
    let rows = crlf_with_blank_lines(&data, "ptp_practitioner_2.txt", &[(2, 2), (3, 3)]);
    assert_eq!(rows[..3], [1, 4, 8]);
    let out = dir.path().join("out");
    let output = cms_tsv(&data, &out);
    assert_rejected(
        &output,
        &["cms-tsv: conflicting overlapping edits: manifest row 3 (ptp practitioner) table row 4 and manifest row 3 (ptp practitioner) table row 8 overlap with a different modifier indicator"],
        &[],
    );
    assert!(!out.exists());
}

#[test]
fn a_malformed_row_names_its_physical_line_after_blank_lines() {
    struct Case {
        file: &'static str,
        line: usize,
        column: usize,
        value: &'static str,
        manifest_row: usize,
        kind: &'static str,
        blanks: &'static [(usize, usize)],
        field: &'static str,
    }
    let cases = [
        Case {
            file: "ptp_hospital.txt",
            line: 3,
            column: 0,
            value: "Y9!07",
            manifest_row: 4,
            kind: "ptp hospital",
            blanks: &[(2, 1), (3, 2), (4, 1)],
            field: "column 1",
        },
        Case {
            file: "ptp_practitioner_1.txt",
            line: 5,
            column: 1,
            value: "Y900",
            manifest_row: 2,
            kind: "ptp practitioner",
            blanks: &[(2, 1), (4, 2), (5, 1)],
            field: "column 2",
        },
    ];
    for case in cases {
        let (dir, data) = release_copy();
        set_cell(&data, case.file, case.line, case.column, case.value);
        let physical = crlf_with_blank_lines(&data, case.file, case.blanks)[case.line - 1];
        assert!(
            physical > case.line as u64,
            "blank lines shift the row number"
        );
        let out = dir.path().join("out");
        let output = cms_tsv(&data, &out);
        let expected = format!(
            "cms-tsv: manifest row {} ({}), table row {physical}, field {}: ",
            case.manifest_row, case.kind, case.field
        );
        assert_rejected(&output, &[&expected], &[]);
        assert!(!out.exists(), "{}", case.file);
    }
}

#[test]
fn an_mue_manifest_date_outside_q4_2026_is_rejected() {
    let row = "mue\tpractitioner\tmue_practitioner.txt\t2026-10-01\t";
    for effective in ["2027-01-01", "2026-09-30", "2027-06-15", "2020-01-01"] {
        let (dir, data) = release_copy();
        edit_manifest(&data, |text| {
            text.replace(row, &row.replace("2026-10-01", effective))
        });
        let out = dir.path().join("out");
        let output = cms_tsv(&data, &out);
        assert_rejected(
            &output,
            &["cms-tsv: manifest row 5, field effective: must be in Q4 2026 for v323r0"],
            &[effective],
        );
        assert!(!out.exists(), "{effective}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "{effective}");
    }
    // The first and the last day of the quarter are inside it.
    for effective in ["2026-10-01", "2026-12-31"] {
        let (dir, data) = release_copy();
        edit_manifest(&data, |text| {
            text.replace(row, &row.replace("2026-10-01", effective))
        });
        let output = cms_tsv(&data, &dir.path().join("out"));
        assert!(output.status.success(), "{effective}: {}", stderr(&output));
    }
}

#[test]
fn a_ptp_row_effective_after_q4_2026_is_rejected() {
    for (file, line, row, kind) in [
        ("ptp_practitioner_1.txt", 4, 2, "ptp practitioner"),
        ("ptp_hospital.txt", 2, 4, "ptp hospital"),
        ("ptp_practitioner_2.txt", 3, 3, "ptp practitioner"),
    ] {
        for value in ["20270101", "20271231"] {
            let (dir, data) = release_copy();
            set_cell(&data, file, line, 3, value);
            let out = dir.path().join("out");
            let output = cms_tsv(&data, &out);
            let expected = format!(
                "cms-tsv: manifest row {row} ({kind}), table row {line}, field effective date: after Q4 2026, not part of v323r0"
            );
            assert_rejected(&output, &[&expected], &[value]);
            assert!(!out.exists(), "{file} {value}");
        }
        // The last day of the quarter is still inside it.
        let (dir, data) = release_copy();
        set_cell(&data, file, line, 3, "20261231");
        let output = cms_tsv(&data, &dir.path().join("out"));
        assert!(output.status.success(), "{file}: {}", stderr(&output));
    }
}

fn verify_licensed(dir: &Path, ptp_rows: &str, mue_rows: &str) -> Output {
    index_cli(&[
        "verify-licensed".as_ref(),
        dir.as_os_str(),
        ptp_rows.as_ref(),
        mue_rows.as_ref(),
    ])
}

/// A licensed index pair compiled from the normalized fixture: 8 PTP and 7 MUE rows.
fn licensed_index() -> (TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let out = dir.path().join("licensed");
    let output = build("licensed", &fixtures().join("normalized"), &out);
    assert!(output.status.success(), "{}", stderr(&output));
    (dir, out)
}

#[test]
fn usage_is_one_line_and_names_verify_licensed() {
    let output = index_cli(&[]);
    let message = stderr(&output);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(message.trim_end().lines().count(), 1, "{message}");
    assert!(message.contains("or medverify-index synthetic-stress --rows N [--out DIR]"));
    assert!(message.contains("or medverify-index verify-licensed DIR PTP_ROWS MUE_ROWS"));
    for args in [
        &["verify-licensed"][..],
        &["verify-licensed", "dir", "1"],
        &["verify-licensed", "dir", "1", "1", "extra"],
    ] {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        assert_rejected(&index_cli(&args), &[USAGE], &[]);
    }
}

#[test]
fn verify_licensed_accepts_an_index_with_the_right_counts() {
    let (_dir, out) = licensed_index();
    let output = verify_licensed(&out, "8", "7");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}

#[test]
fn verify_licensed_rejects_wrong_counts_modes_and_malformed_arguments() {
    let (dir, out) = licensed_index();
    let counts = "licensed indexes differ from expected formats or row counts";
    for (ptp, mue) in [("7", "7"), ("9", "7"), ("8", "6"), ("8", "8")] {
        assert_rejected(&verify_licensed(&out, ptp, mue), &[counts], &[]);
    }
    // A synthetic index with its own true counts is still not a licensed index.
    let synthetic = dir.path().join("synthetic");
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let built = build("synthetic", &data, &synthetic);
    assert!(built.status.success(), "{}", stderr(&built));
    let ptp = MmapPtpIndex::open(synthetic.join("ncci_ptp.bin")).unwrap();
    let mue = MmapMueIndex::open(synthetic.join("ncci_mue.bin")).unwrap();
    let (ptp_rows, mue_rows) = (ptp.records().len(), mue.records().len());
    assert_rejected(
        &verify_licensed(&synthetic, &ptp_rows.to_string(), &mue_rows.to_string()),
        &[counts],
        &[],
    );
    // Row counts are positional arguments, so their errors never mention `--rows`.
    for (ptp, mue, argument) in [
        ("x", "7", "PTP_ROWS"),
        ("8", "", "MUE_ROWS"),
        ("0", "7", "PTP_ROWS"),
    ] {
        let output = verify_licensed(&out, ptp, mue);
        let expected = format!("{argument} needs a decimal number from 1 to 4294967295");
        assert_rejected(&output, &[&expected], &["--rows"]);
    }
    let missing = dir.path().join("missing");
    assert_rejected(&verify_licensed(&missing, "8", "7"), &[], &[]);
}

#[test]
fn verify_licensed_rejects_a_rehashed_ptp_record_the_loader_refuses() {
    let (_dir, out) = licensed_index();
    let path = out.join("ncci_ptp.bin");
    let mut bytes = fs::read(&path).unwrap();
    let directory = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    // Byte 12 of the first 16-byte record is its modifier indicator; 7 is not 0, 1 or 9.
    let ccmi = 64 + 12 * directory + 12;
    assert!(matches!(bytes[ccmi], 0 | 1 | 9));
    bytes[ccmi] = 7;
    let digest = Sha256::digest(&bytes[64..]);
    bytes[32..64].copy_from_slice(&digest);
    fs::write(&path, &bytes).unwrap();
    // Header, length and digest are all consistent; only the record is invalid.
    assert!(MmapPtpIndex::open(&path).is_err());
    // The production loader refuses it, which verification reports as it is.
    let output = verify_licensed(&out, "8", "7");
    assert_rejected(&output, &["invalid index record"], &[]);
}
