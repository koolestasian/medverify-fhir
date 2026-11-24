use csv::StringRecord;
use medverify_core::cms;
use medverify_core::encoding::{
    encode_code_b36, encode_epoch_day, MueRecord, NcciPtpRecord, NO_DELETION_DATE,
};
use medverify_core::index::{write_mue, write_ptp, CatalogMode, MmapMueIndex, MmapPtpIndex};
use medverify_core::stress;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use time::{macros::format_description, Date};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const PTP_FILE: &str = "ncci_ptp.bin";
const MUE_FILE: &str = "ncci_mue.bin";
const USAGE: &str = "usage: medverify-index <synthetic|licensed|cms-tsv> --data DIR --out DIR     or medverify-index synthetic-stress --rows N [--out DIR]     or medverify-index verify-licensed DIR PTP_ROWS MUE_ROWS";
const STRESS_CLAIMS: usize = 4_096;
const STRESS_FAILED: &str = "synthetic stress verification failed";

fn read_csv(
    path: &Path,
    headers: &[&str],
    mut consume: impl FnMut(&StringRecord) -> Result<()>,
) -> Result<()> {
    let mut reader = csv::Reader::from_path(path)?;
    if !reader.headers()?.iter().eq(headers.iter().copied()) {
        return Err(format!("unexpected columns in {}", path.display()).into());
    }
    let mut row = StringRecord::new();
    while reader.read_record(&mut row)? {
        consume(&row).map_err(|e| {
            format!(
                "{} line {}: {e}",
                path.display(),
                row.position().unwrap().line()
            )
        })?;
    }
    Ok(())
}

fn validate_catalog(data: &Path) -> Result<()> {
    let mut expected: HashSet<_> = (9001..=9120).map(|n| format!("Y{n}")).collect();
    read_csv(
        &data.join("procedures_synthetic.csv"),
        &["code", "label"],
        |row| {
            if !expected.remove(&row[0]) || row[1].trim().is_empty() {
                return Err("invalid synthetic procedure or label".into());
            }
            Ok(())
        },
    )?;
    if !expected.is_empty() {
        return Err("incomplete synthetic catalog".into());
    }
    Ok(())
}

fn procedure(code: &str) -> Result<u32> {
    encode_code_b36(code)
        .filter(|_| !code.bytes().any(|b| b.is_ascii_lowercase()))
        .ok_or_else(|| "invalid procedure code".into())
}

fn setting(value: &str) -> Result<u8> {
    match value {
        "practitioner" => Ok(1),
        "hospital" => Ok(2),
        "dme" => Ok(4),
        _ => Err("invalid setting".into()),
    }
}

fn day(value: &str) -> Result<u16> {
    let date = Date::parse(value, format_description!("[year]-[month]-[day]"))?;
    encode_epoch_day(date).ok_or_else(|| "date outside index epoch".into())
}

fn end_day(value: &str) -> Result<u16> {
    if value.is_empty() {
        Ok(NO_DELETION_DATE)
    } else {
        day(value)
    }
}

fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    let mut a = File::open(a)?;
    let mut b = File::open(b)?;
    let mut left = [0; 8192];
    let mut right = [0; 8192];
    loop {
        let n = a.read(&mut left)?;
        if n == 0 {
            return Ok(b.read(&mut right)? == 0);
        }
        b.read_exact(&mut right[..n])?;
        if left[..n] != right[..n] {
            return Ok(false);
        }
    }
}

type Records = (Vec<NcciPtpRecord>, Vec<MueRecord>);

fn read_csv_records(mode: CatalogMode, data: &Path) -> Result<Records> {
    let suffix = match mode {
        CatalogMode::Synthetic => {
            validate_catalog(data)?;
            "_synthetic"
        }
        CatalogMode::Licensed | CatalogMode::SyntheticStress => "",
    };
    let mut ptp = Vec::new();
    read_csv(
        &data.join(format!("ncci_ptp{suffix}.csv")),
        &[
            "col1",
            "col2",
            "setting",
            "effective",
            "end_exclusive",
            "ccmi",
            "prior_1996",
        ],
        |row| {
            ptp.push(NcciPtpRecord {
                col1_b36: procedure(&row[0])?,
                col2_b36: procedure(&row[1])?,
                setting_flags: setting(&row[2])?,
                eff_days: day(&row[3])?,
                del_days: end_day(&row[4])?,
                ccmi: row[5].parse()?,
                flags: row[6].parse()?,
                rationale_id: 0,
            });
            Ok(())
        },
    )?;
    let mut mue = Vec::new();
    read_csv(
        &data.join(format!("ncci_mue{suffix}.csv")),
        &[
            "code",
            "setting",
            "effective",
            "end_exclusive",
            "max_units",
            "mai",
        ],
        |row| {
            mue.push(MueRecord {
                code_b36: procedure(&row[0])?,
                setting_flags: setting(&row[1])?,
                eff_days: day(&row[2])?,
                del_days: end_day(&row[3])?,
                max_units: row[4].parse()?,
                mai: row[5].parse()?,
                flags: 0,
                reserved: 0,
                rationale_id: 0,
            });
            Ok(())
        },
    )?;
    Ok((ptp, mue))
}

/// Writes both indexes into a staging directory beside `out`, then renames it into place.
fn publish(mode: CatalogMode, (ptp, mue): Records, out: &Path) -> Result<()> {
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let stage = tempfile::tempdir_in(parent)?;
    let (ptp_count, mue_count) = (ptp.len(), mue.len());
    write_ptp(File::create(stage.path().join(PTP_FILE))?, mode, ptp)?;
    write_mue(File::create(stage.path().join(MUE_FILE))?, mode, mue)?;

    // A rename publishes both files together. A concurrent identical build may win.
    if out
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_none())
    {
        if let Err(e) = fs::remove_dir(out) {
            if !matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) {
                return Err(e.into());
            }
        }
    }
    if let Err(e) = fs::rename(stage.path(), out) {
        if !out.is_dir() {
            return Err(e.into());
        }
        for name in [PTP_FILE, MUE_FILE] {
            if !same_file(&stage.path().join(name), &out.join(name))? {
                return Err("destination contains a different index; use a fresh directory".into());
            }
        }
    }
    println!(
        "{}: {ptp_count} PTP records, {mue_count} MUE records",
        mode.release()
    );
    Ok(())
}

/// A positive row count; `name` is the option or argument the message blames.
fn parse_rows(value: Option<OsString>, name: &str) -> Result<u32> {
    value
        .as_deref()
        .and_then(|v| v.to_str())
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok())
        .filter(|&rows| rows > 0)
        .ok_or_else(|| format!("{name} needs a decimal number from 1 to 4294967295").into())
}

fn percent(part: u64, whole: u64) -> String {
    format!("{:.2}%", 100.0 * part as f64 / whole as f64)
}

/// Builds a synthetic stress catalog, publishes it, reopens it and checks claims against the
/// oracle in `medverify_core::stress`.
fn run_stress(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let (mut rows, mut out) = (None, None);
    while let Some(option) = args.next() {
        match option.to_str() {
            Some("--rows") if rows.is_none() => {
                rows = Some(parse_rows(args.next(), "--rows")?);
            }
            Some("--out") if out.is_none() => {
                out = Some(PathBuf::from(args.next().ok_or("missing directory")?));
            }
            _ => return Err("unknown or duplicate option".into()),
        }
    }
    let rows = rows.ok_or("--rows is required")?;
    let out = out.unwrap_or_else(|| PathBuf::from("target/synthetic-stress"));

    let ptp_records = stress::ptp_records(rows, stress::SEED);
    let mue_records = stress::mue_records(&ptp_records, stress::SEED);
    let mode = CatalogMode::SyntheticStress;
    publish(mode, (ptp_records.clone(), mue_records.clone()), &out)?;

    let ptp = MmapPtpIndex::open(out.join(PTP_FILE))?;
    let mue = MmapMueIndex::open(out.join(MUE_FILE))?;
    if ptp.records().len() != rows as usize
        || mue.records().len() != mue_records.len()
        || ptp.catalog_mode() != mode
        || mue.catalog_mode() != mode
        || ptp.format_version() != 2
    {
        return Err(STRESS_FAILED.into());
    }
    let claims = stress::claims(&ptp, STRESS_CLAIMS, stress::SEED);
    let mix = stress::check_claims(&ptp, &mue, &ptp_records, &mue_records, &claims)
        .map_err(|_| STRESS_FAILED)?;

    let mut sizes: Vec<u32> = ptp.directory().iter().map(|e| e.row_count).collect();
    sizes.sort_unstable();
    let directory_bytes = std::mem::size_of_val(ptp.directory());
    let record_bytes = std::mem::size_of_val(ptp.records());
    println!("seed: {:#018x}", stress::SEED);
    println!("PTP rows: {}", ptp.records().len());
    println!("directory buckets: {}", sizes.len());
    println!(
        "bucket size min/median/max: {}/{}/{}",
        sizes[0],
        sizes[sizes.len() / 2],
        sizes[sizes.len() - 1]
    );
    println!("MUE rows: {}", mue.records().len());
    println!(
        "PTP payload bytes: {} (directory {directory_bytes} + records {record_bytes})",
        directory_bytes + record_bytes
    );
    println!(
        "PTP file bytes: {}",
        fs::metadata(out.join(PTP_FILE))?.len()
    );
    println!(
        "MUE file bytes: {}",
        fs::metadata(out.join(MUE_FILE))?.len()
    );
    println!("pair filter bytes: {}", ptp.filter_bytes());
    println!("claims checked: {}", claims.len());
    println!("probes: {}", mix.probes);
    println!("hits: {} ({})", mix.hits, percent(mix.hits, mix.probes));
    println!(
        "interior misses: {} ({})",
        mix.interior_misses,
        percent(mix.interior_misses, mix.probes)
    );
    println!(
        "bucket misses: {} ({})",
        mix.bucket_misses,
        percent(mix.bucket_misses, mix.probes)
    );
    Ok(())
}

fn verify_licensed(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let (Some(dir), Some(ptp_rows), Some(mue_rows), None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        return Err(USAGE.into());
    };
    let dir = PathBuf::from(dir);
    let ptp_rows = parse_rows(Some(ptp_rows), "PTP_ROWS")? as usize;
    let mue_rows = parse_rows(Some(mue_rows), "MUE_ROWS")? as usize;
    let ptp = MmapPtpIndex::open(dir.join(PTP_FILE))?;
    let mue = MmapMueIndex::open(dir.join(MUE_FILE))?;
    if ptp.catalog_mode() != CatalogMode::Licensed
        || mue.catalog_mode() != CatalogMode::Licensed
        || ptp.format_version() != 2
        || mue.format_version() != 1
        || ptp.records().len() != ptp_rows
        || mue.records().len() != mue_rows
    {
        return Err("licensed indexes differ from expected formats or row counts".into());
    }
    Ok(())
}

fn run() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let source = args.next();
    let source = source.as_deref().and_then(|s| s.to_str());
    if source == Some("synthetic-stress") {
        return run_stress(args);
    }
    if source == Some("verify-licensed") {
        return verify_licensed(args);
    }
    if !matches!(source, Some("synthetic" | "licensed" | "cms-tsv")) {
        return Err(USAGE.into());
    }
    let (mut data, mut out) = (None, None);
    while let Some(option) = args.next() {
        let slot = match option.to_str() {
            Some("--data") if data.is_none() => &mut data,
            Some("--out") if out.is_none() => &mut out,
            _ => return Err("unknown or duplicate option".into()),
        };
        *slot = Some(PathBuf::from(args.next().ok_or("missing directory")?));
    }
    let data = data.ok_or("--data is required")?;
    let out = out.ok_or("--out is required")?;
    let (mode, records) = match source {
        Some("synthetic") => (
            CatalogMode::Synthetic,
            read_csv_records(CatalogMode::Synthetic, &data)?,
        ),
        Some("licensed") => (
            CatalogMode::Licensed,
            read_csv_records(CatalogMode::Licensed, &data)?,
        ),
        _ => (CatalogMode::Licensed, cms::load(&data)?),
    };
    publish(mode, records, &out)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
