use medverify_core::encoding::{encode_code_b36, MueRecord, NcciPtpRecord, PtpDirectoryEntry};
use medverify_core::index::{
    write_mue, write_ptp, write_ptp_v1, CatalogMode, MmapMueIndex, MmapPtpIndex,
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};
use tempfile::tempdir;

fn pair(start: u16, end: u16, ccmi: u8) -> NcciPtpRecord {
    NcciPtpRecord {
        col1_b36: encode_code_b36("Y9001").unwrap(),
        col2_b36: encode_code_b36("Y9002").unwrap(),
        eff_days: start,
        del_days: end,
        ccmi,
        setting_flags: 1,
        rationale_id: 0,
        flags: 0,
    }
}

fn limit() -> MueRecord {
    MueRecord {
        code_b36: encode_code_b36("Y9120").unwrap(),
        max_units: 300,
        eff_days: 256,
        del_days: u16::MAX,
        mai: 3,
        setting_flags: 4,
        rationale_id: 0,
        flags: 0,
        reserved: 0,
    }
}

fn ptp_bytes(rows: Vec<NcciPtpRecord>) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_ptp(&mut bytes, CatalogMode::Synthetic, rows).unwrap();
    bytes
}

fn ptp_v1_bytes(rows: Vec<NcciPtpRecord>) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_ptp_v1(&mut bytes, CatalogMode::Synthetic, rows).unwrap();
    bytes
}

fn sign_payload(bytes: &mut [u8]) {
    let digest = Sha256::digest(&bytes[64..]);
    bytes[32..64].copy_from_slice(&digest);
}

fn open_bytes(bytes: &[u8]) -> std::io::Result<MmapPtpIndex> {
    let dir = tempdir().unwrap();
    let path = dir.path().join("index");
    fs::write(&path, bytes).unwrap();
    MmapPtpIndex::open(&path)
}

/// Offset of the first record in a version 2 file with `buckets` directory entries.
const fn records_at(buckets: usize) -> usize {
    64 + 12 * buckets
}

#[test]
fn little_endian_header_records_and_immutable_snapshots() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("edits");
    let row = pair(256, 32767, 1);
    let bytes = ptp_bytes(vec![row]);
    assert_eq!(&bytes[..16], b"MVNCCI01\x02\x00\x01\x00\x10\x00\x00\x00");
    assert_eq!(&bytes[16..20], &1u32.to_le_bytes());
    assert_eq!(&bytes[20..24], &1u32.to_le_bytes());
    assert_eq!(&bytes[24..32], b"synth01\0");
    assert_eq!(bytes[32..64], Sha256::digest(&bytes[64..])[..]);
    assert_eq!(&bytes[64..68], &row.col1_b36.to_le_bytes());
    assert_eq!(&bytes[68..72], &0u32.to_le_bytes());
    assert_eq!(&bytes[72..76], &1u32.to_le_bytes());
    assert_eq!(&bytes[76..80], &row.col1_b36.to_le_bytes());
    assert_eq!(&bytes[80..84], &row.col2_b36.to_le_bytes());
    assert_eq!(&bytes[84..], &[0, 1, 255, 127, 1, 1, 0, 0]);
    fs::write(&path, bytes).unwrap();
    let ptp = MmapPtpIndex::open(&path).unwrap();
    assert!(MmapMueIndex::open(&path).is_err());
    assert_eq!(ptp.catalog_mode(), CatalogMode::Synthetic);
    assert_eq!(ptp.release(), "synth01");
    assert_eq!(ptp.format_version(), 2);
    assert_eq!(
        ptp.directory(),
        &[PtpDirectoryEntry {
            col1_b36: row.col1_b36,
            first_row: 0,
            row_count: 1
        }]
    );

    let mut bytes = Vec::new();
    write_mue(&mut bytes, CatalogMode::Licensed, vec![limit()]).unwrap();
    assert_eq!(&bytes[68..], &[44, 1, 0, 1, 255, 255, 3, 4, 0, 0, 0, 0]);
    fs::write(&path, bytes).unwrap();
    let mue = MmapMueIndex::open(&path).unwrap();
    assert!(MmapPtpIndex::open(&path).is_err());
    assert_eq!(mue.catalog_mode(), CatalogMode::Licensed);
    assert_eq!(mue.release(), "v323r0");
    fs::write(&path, b"x").unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(ptp.records(), &[row]);
    assert_eq!(mue.records(), &[limit()]);
}

#[test]
fn header_length_and_digest_corruption() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("edits");
    let v2 = ptp_bytes(vec![pair(0, 1, 0)]);
    let v1 = ptp_v1_bytes(vec![pair(0, 1, 0)]);
    for offset in [0, 8, 10, 11, 12, 16, 20, 24, 31, 32, 64, 70, 76, 90] {
        let mut broken = v2.clone();
        broken[offset] ^= 0xff;
        fs::write(&path, broken).unwrap();
        assert!(MmapPtpIndex::open(&path).is_err(), "v2 offset {offset}");
    }
    for offset in [0, 8, 10, 11, 12, 16, 24, 31, 32, 64] {
        let mut broken = v1.clone();
        broken[offset] ^= 0xff;
        fs::write(&path, broken).unwrap();
        assert!(MmapPtpIndex::open(&path).is_err(), "v1 offset {offset}");
    }
    let mut v2_overflow = v2.clone();
    v2_overflow[16..24].fill(255);
    let mut v1_overflow = v1.clone();
    v1_overflow[16..24].fill(255);
    for broken in [
        &v2[..8],
        &v2[..79],
        &v2[..v2.len() - 1],
        &[&v2[..], &[0]].concat(),
        &v2_overflow,
        &v1[..8],
        &v1[..79],
        &[&v1[..], &[0]].concat(),
        &v1_overflow,
    ] {
        fs::write(&path, broken).unwrap();
        assert!(MmapPtpIndex::open(&path).is_err());
    }
}

#[test]
fn legacy_v1_ptp_files_still_open_and_match_the_old_bytes() {
    let row = pair(256, 32767, 1);
    let bytes = ptp_v1_bytes(vec![row]);
    assert_eq!(&bytes[..16], b"MVNCCI01\x01\x00\x01\x00\x10\x00\x00\x00");
    assert_eq!(&bytes[16..24], &1u64.to_le_bytes());
    assert_eq!(&bytes[24..32], b"synth01\0");
    assert_eq!(bytes[32..64], Sha256::digest(&bytes[64..])[..]);
    assert_eq!(&bytes[64..68], &row.col1_b36.to_le_bytes());
    assert_eq!(&bytes[68..72], &row.col2_b36.to_le_bytes());
    assert_eq!(&bytes[72..], &[0, 1, 255, 127, 1, 1, 0, 0]);
    let ptp = open_bytes(&bytes).unwrap();
    assert_eq!(ptp.format_version(), 1);
    assert!(ptp.directory().is_empty());
    assert_eq!(ptp.records(), &[row]);
    let (col1, col2) = (row.col1_b36, row.col2_b36);
    assert_eq!(ptp.active_on(col1, col2, 1, 256), Some(&row));
    assert_eq!(ptp.active_on(col1, col2, 1, 32767), None);
    assert_eq!(ptp.active_on(col2, col1, 1, 256), None);
    assert!(open_bytes(&ptp_v1_bytes(Vec::new()))
        .unwrap()
        .records()
        .is_empty());
}

#[test]
fn validates_intervals_fields_and_order_even_with_a_valid_digest() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("edits");
    const R: usize = records_at(1);
    let rows = ptp_bytes(vec![pair(1, 5, 0), pair(5, 10, 1)]);
    let mut swapped = rows.clone();
    swapped[R..].rotate_left(16);
    let mut overlap = rows.clone();
    overlap[R + 24..R + 26].copy_from_slice(&4u16.to_le_bytes());
    let mut reversed = rows.clone();
    reversed[R + 10] = 0;
    let mut outside_catalog = rows.clone();
    outside_catalog[R..R + 4].copy_from_slice(&encode_code_b36("Y900A").unwrap().to_le_bytes());
    let mut wrong_setting = rows.clone();
    wrong_setting[R + 13] = 4;
    let mut bad_ccmi = rows;
    bad_ccmi[R + 12] = 9;
    for mut broken in [
        swapped,
        overlap,
        reversed,
        outside_catalog,
        wrong_setting,
        bad_ccmi,
    ] {
        sign_payload(&mut broken);
        fs::write(&path, broken).unwrap();
        assert!(MmapPtpIndex::open(&path).is_err());
    }

    let next = MueRecord {
        eff_days: 300,
        ..limit()
    };
    let first = MueRecord {
        del_days: 300,
        ..limit()
    };
    let mut mue = Vec::new();
    write_mue(&mut mue, CatalogMode::Synthetic, vec![next, first]).unwrap();
    for (offset, value) in [(74, 0), (75, 3), (77, 1), (78, 1), (72, 0), (87, 0)] {
        let mut broken = mue.clone();
        broken[offset] = value;
        sign_payload(&mut broken);
        fs::write(&path, broken).unwrap();
        assert!(MmapMueIndex::open(&path).is_err(), "offset {offset}");
    }
    mue[64..].rotate_left(16);
    sign_payload(&mut mue);
    fs::write(&path, mue).unwrap();
    assert!(MmapMueIndex::open(&path).is_err());
}

#[test]
fn rejects_conflicts_hidden_by_nested_or_inactive_intervals() {
    let mut rows = vec![
        pair(1, 40, 1),
        pair(3, 7, 1),
        pair(9, 9, 9),
        pair(40, 80, 0),
    ];
    rows.push(NcciPtpRecord {
        setting_flags: 2,
        ..pair(4, 39, 0)
    });
    assert!(write_ptp(Vec::new(), CatalogMode::Synthetic, rows.clone()).is_ok());
    rows.push(pair(11, 15, 0));
    assert!(write_ptp(Vec::new(), CatalogMode::Synthetic, rows).is_err());
    let other_limit = MueRecord {
        max_units: 1,
        ..limit()
    };
    assert!(write_mue(
        Vec::new(),
        CatalogMode::Synthetic,
        vec![limit(), other_limit]
    )
    .is_err());
}

#[test]
fn empty_indexes_round_trip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("empty");
    fs::write(&path, ptp_bytes(Vec::new())).unwrap();
    assert!(MmapPtpIndex::open(&path).unwrap().records().is_empty());
}

fn build(data: &Path, out: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_medverify-index"))
        .args(["synthetic", "--data"])
        .arg(data)
        .arg("--out")
        .arg(out)
        .output()
        .unwrap()
}

#[test]
fn compiler_reproduces_the_same_pair_after_input_reordering() {
    let dir = tempdir().unwrap();
    let data = dir.path().join("input");
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    fs::create_dir(&data).unwrap();
    fs::create_dir(&first).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    assert!(build(&source, &first).status.success());
    for name in [
        "procedures_synthetic.csv",
        "ncci_ptp_synthetic.csv",
        "ncci_mue_synthetic.csv",
    ] {
        let contents = fs::read_to_string(source.join(name)).unwrap();
        let mut lines = contents.lines();
        let mut shuffled = vec![lines.next().unwrap()];
        shuffled.extend(lines.rev());
        fs::write(data.join(name), shuffled.join("\n") + "\n").unwrap();
    }
    assert!(build(&data, &second).status.success());
    assert!(build(&data, &first).status.success());
    for name in ["ncci_ptp.bin", "ncci_mue.bin"] {
        assert_eq!(
            fs::read(first.join(name)).unwrap(),
            fs::read(second.join(name)).unwrap()
        );
    }
    let ptp = MmapPtpIndex::open(first.join("ncci_ptp.bin")).unwrap();
    assert!(ptp
        .records()
        .iter()
        .any(|r| r.ccmi == 9 && r.eff_days == r.del_days));
    assert!(MmapMueIndex::open(first.join("ncci_mue.bin"))
        .unwrap()
        .records()
        .contains(&MueRecord {
            code_b36: encode_code_b36("Y9011").unwrap(),
            max_units: 1,
            eff_days: 13422,
            ..limit()
        }));
    let bad = data.join("ncci_mue_synthetic.csv");
    let original = fs::read_to_string(&bad).unwrap();
    fs::write(&bad, original + "Y9011,practitioner,2026-11-01,,5,2\n").unwrap();
    let failed = build(&data, &first);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("conflicting overlapping edits"));
    assert!(!build(&data, &dir.path().join("bad")).status.success());
    assert!(!dir.path().join("bad").exists());
    for name in ["ncci_ptp.bin", "ncci_mue.bin"] {
        assert_eq!(
            fs::read(first.join(name)).unwrap(),
            fs::read(second.join(name)).unwrap()
        );
    }
}

type DirectoryEdit = Box<dyn Fn(&mut Vec<[u32; 3]>)>;

fn code(text: &str) -> u32 {
    encode_code_b36(text).unwrap()
}

fn edge(col1: &str, col2: &str, setting: u8) -> NcciPtpRecord {
    NcciPtpRecord {
        col1_b36: code(col1),
        col2_b36: code(col2),
        setting_flags: setting,
        ..pair(1, 5, 1)
    }
}

/// Three buckets (Y9001: 2 rows, Y9005: 1, Y9010: 2) in shuffled input order.
fn buckets() -> Vec<NcciPtpRecord> {
    vec![
        edge("Y9010", "Y9001", 2),
        edge("Y9001", "Y9003", 1),
        edge("Y9005", "Y9002", 1),
        edge("Y9010", "Y9001", 1),
        edge("Y9001", "Y9002", 1),
    ]
}

fn le_words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Rewrites the directory of a v2 file, fixing the count field, the length and the digest.
fn with_directory(bytes: &[u8], edit: impl FnOnce(&mut Vec<[u32; 3]>)) -> Vec<u8> {
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    let split = records_at(count);
    let mut entries: Vec<[u32; 3]> = bytes[64..split]
        .as_chunks::<12>()
        .0
        .iter()
        .map(|c| {
            let word = |i: usize| u32::from_le_bytes(c[i..i + 4].try_into().unwrap());
            [word(0), word(4), word(8)]
        })
        .collect();
    edit(&mut entries);
    let mut out = bytes[..64].to_vec();
    out[20..24].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    out.extend(le_words(&entries.concat()));
    out.extend_from_slice(&bytes[split..]);
    sign_payload(&mut out);
    out
}

#[test]
fn v2_directory_bytes_for_a_multi_bucket_input() {
    let bytes = ptp_bytes(buckets());
    assert_eq!(&bytes[..16], b"MVNCCI01\x02\x00\x01\x00\x10\x00\x00\x00");
    assert_eq!(&bytes[16..20], &5u32.to_le_bytes());
    assert_eq!(&bytes[20..24], &3u32.to_le_bytes());
    assert_eq!(&bytes[24..32], b"synth01\0");
    assert_eq!(bytes.len(), 64 + 3 * 12 + 5 * 16);
    assert_eq!(bytes[32..64], Sha256::digest(&bytes[64..])[..]);
    let expected = le_words(&[
        code("Y9001"),
        0,
        2,
        code("Y9005"),
        2,
        1,
        code("Y9010"),
        3,
        2,
    ]);
    assert_eq!(&bytes[64..100], &expected[..]);

    let index = open_bytes(&bytes).unwrap();
    assert_eq!(index.format_version(), 2);
    assert_eq!(
        index.directory(),
        &[
            PtpDirectoryEntry {
                col1_b36: code("Y9001"),
                first_row: 0,
                row_count: 2
            },
            PtpDirectoryEntry {
                col1_b36: code("Y9005"),
                first_row: 2,
                row_count: 1
            },
            PtpDirectoryEntry {
                col1_b36: code("Y9010"),
                first_row: 3,
                row_count: 2
            },
        ]
    );
    let mut sorted = buckets();
    sorted.sort_by_key(|r| (r.col1_b36, r.col2_b36, r.setting_flags));
    assert_eq!(index.records(), &sorted[..]);
    for row in &sorted {
        assert_eq!(
            index.active_on(row.col1_b36, row.col2_b36, row.setting_flags, 1),
            Some(row)
        );
    }
    assert_eq!(index.active_on(code("Y9004"), code("Y9002"), 1, 1), None);
    assert_eq!(index.active_on(code("Y9001"), code("Y9002"), 2, 1), None);
    assert_eq!(index.active_on(code("Y9001"), code("Y9002"), 1, 5), None);
}

#[test]
fn identical_directory_edit_is_accepted_and_malformed_directories_are_rejected() {
    let bytes = ptp_bytes(buckets());
    assert_eq!(with_directory(&bytes, |_| {}), bytes);
    let beyond = code("Y9020");
    let cases: [(&str, DirectoryEdit); 15] = [
        ("unsorted", Box::new(|e| e.swap(0, 1))),
        ("duplicate col1", Box::new(|e| e[1][0] = e[0][0])),
        ("gap", Box::new(|e| e[1][1] += 1)),
        ("overlap", Box::new(|e| e[1][1] -= 1)),
        ("first row not zero", Box::new(|e| e[0][1] = 1)),
        ("zero row count", Box::new(move |e| e.push([beyond, 5, 0]))),
        ("zero count in the middle", Box::new(|e| e[1][2] = 0)),
        ("coverage short", Box::new(|e| e[2][2] -= 1)),
        ("coverage long", Box::new(|e| e[2][2] += 1)),
        ("entry col1 differs from rows", Box::new(|e| e[1][0] += 1)),
        ("extra entry", Box::new(move |e| e.push([beyond, 5, 1]))),
        (
            "dropped entry",
            Box::new(|e| {
                e.pop();
            }),
        ),
        ("empty directory with records", Box::new(Vec::clear)),
        ("first entry takes all rows", Box::new(|e| e[0][2] = 5)),
        ("counts overflow", Box::new(|e| e[2][2] = u32::MAX)),
    ];
    for (name, edit) in cases {
        let broken = with_directory(&bytes, edit);
        assert!(open_bytes(&broken).is_err(), "{name}");
    }

    let empty = ptp_bytes(Vec::new());
    assert_eq!(with_directory(&empty, |_| {}), empty);
    assert!(open_bytes(&with_directory(&empty, |e| e.push([code("Y9001"), 0, 1]))).is_err());
}

#[test]
fn directory_errors_do_not_echo_file_contents() {
    let bytes = ptp_bytes(buckets());
    let broken = with_directory(&bytes, |e| e[1][0] += 1);
    let error = open_bytes(&broken).err().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(!error.to_string().chars().any(|c| c.is_ascii_digit()));
}

#[test]
fn v2_with_the_mue_kind_and_unknown_versions_are_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("index");
    let mut mue = Vec::new();
    write_mue(&mut mue, CatalogMode::Synthetic, vec![limit()]).unwrap();
    let mut v2_mue = mue[..64].to_vec();
    v2_mue[8] = 2;
    v2_mue[16..24].copy_from_slice(&le_words(&[1, 1]));
    v2_mue.extend(le_words(&[limit().code_b36, 0, 1]));
    v2_mue.extend_from_slice(&mue[64..]);
    sign_payload(&mut v2_mue);
    fs::write(&path, &v2_mue).unwrap();
    assert!(MmapMueIndex::open(&path).is_err());
    assert!(MmapPtpIndex::open(&path).is_err());
    v2_mue[10] = 1;
    fs::write(&path, &v2_mue).unwrap();
    assert!(MmapPtpIndex::open(&path).is_err());

    let bytes = ptp_bytes(buckets());
    for version in [0_u16, 3, 256] {
        let mut broken = bytes.clone();
        broken[8..10].copy_from_slice(&version.to_le_bytes());
        sign_payload(&mut broken);
        assert!(open_bytes(&broken).is_err(), "version {version}");
    }
    let mut mislabeled = ptp_v1_bytes(buckets());
    mislabeled[8] = 2;
    sign_payload(&mut mislabeled);
    assert!(open_bytes(&mislabeled).is_err());
}

#[test]
fn stress_catalog_accepts_any_y_code_and_synthetic_stays_narrow() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("stress");
    let stress = CatalogMode::SyntheticStress;
    assert_eq!(stress.release(), "stress01");
    let rows = vec![edge("Y0000", "YZZZZ", 1), edge("YA1B2", "Y9001", 2)];
    let mut bytes = Vec::new();
    write_ptp(&mut bytes, stress, rows.clone()).unwrap();
    assert_eq!(bytes[11], 2);
    assert_eq!(&bytes[24..32], b"stress01");
    fs::write(&path, &bytes).unwrap();
    let ptp = MmapPtpIndex::open(&path).unwrap();
    assert_eq!(ptp.catalog_mode(), stress);
    assert_eq!(ptp.release(), "stress01");
    assert_eq!(ptp.records().len(), 2);
    let mut legacy = Vec::new();
    write_ptp_v1(&mut legacy, stress, rows).unwrap();
    fs::write(&path, &legacy).unwrap();
    assert_eq!(MmapPtpIndex::open(&path).unwrap().catalog_mode(), stress);

    let mut bytes = Vec::new();
    let mue = MueRecord {
        code_b36: code("YZZZZ"),
        ..limit()
    };
    write_mue(&mut bytes, stress, vec![mue]).unwrap();
    fs::write(&path, bytes).unwrap();
    let index = MmapMueIndex::open(&path).unwrap();
    assert_eq!(index.catalog_mode(), stress);
    assert_eq!(index.records(), &[mue]);

    for (a, b) in [("Y0000", "X0001"), ("A0000", "Y0000"), ("00000", "Y0000")] {
        assert!(write_ptp(Vec::new(), stress, vec![edge(a, b, 1)]).is_err());
        assert!(write_ptp_v1(Vec::new(), stress, vec![edge(a, b, 1)]).is_err());
    }
    let outside = MueRecord {
        code_b36: code("ZZZZZ"),
        ..limit()
    };
    assert!(write_mue(Vec::new(), stress, vec![outside]).is_err());

    let synthetic = CatalogMode::Synthetic;
    assert!(write_ptp(Vec::new(), synthetic, vec![edge("Y9001", "Y9120", 1)]).is_ok());
    for outside in ["Y9121", "Y900A", "Y0000", "YZZZZ", "Y9000"] {
        assert!(
            write_ptp(Vec::new(), synthetic, vec![edge("Y9001", outside, 1)]).is_err(),
            "{outside}"
        );
    }
    let mut stress_bytes = Vec::new();
    write_ptp(&mut stress_bytes, stress, vec![edge("Y0000", "Y0001", 1)]).unwrap();
    stress_bytes[11] = 0;
    stress_bytes[24..32].copy_from_slice(b"synth01\0");
    sign_payload(&mut stress_bytes);
    assert!(open_bytes(&stress_bytes).is_err());
}

#[test]
fn ptp_lookup_consults_a_load_time_pair_filter() {
    let rows = vec![pair(256, 300, 1), pair(300, u16::MAX, 0)];
    let (one, two) = (pair(0, 1, 1).col1_b36, pair(0, 1, 1).col2_b36);
    for bytes in [ptp_bytes(rows.clone()), ptp_v1_bytes(rows.clone())] {
        let index = open_bytes(&bytes).unwrap();
        // One distinct key needs one 32-byte block; the filter is not in the file.
        assert_eq!(index.filter_bytes(), 32);
        assert_eq!(index.active_on(one, two, 1, 260).unwrap().ccmi, 1);
        assert_eq!(index.active_on(one, two, 1, 400).unwrap().ccmi, 0);
        assert!(index.active_on(two, one, 1, 260).is_none());
        assert!(index.active_on(one, two, 2, 260).is_none());
        assert!(index.active_on(u32::MAX, two, 1, 260).is_none());
    }
}
