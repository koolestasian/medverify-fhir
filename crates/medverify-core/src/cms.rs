//! Reads an operator's Medicare Q4 2026 (`v323r0`) PTP and MUE release tables, saved as
//! tab-separated text. Every index this builds is labeled `v323r0`, so input from another
//! quarter is rejected: an MUE manifest `effective` date outside 2026-10-01 to 2026-12-31 and a
//! PTP row effective on or after 2027-01-01 are errors rather than a mislabeled index.
//!
//! `DIR/manifest.tsv` is UTF-8 and tab-separated. Its first line is exactly
//! `role\tsetting\tfile\teffective\tend_exclusive`, followed by one row per table file.
//! Nothing is inferred from file names.
//!
//! - `role`: `ptp` or `mue`. `setting`: `practitioner` or `hospital`; `dme` also for `mue`.
//! - `file`: a bare file name inside `DIR`. A file may appear once. At least one ptp row and
//!   one mue row are required.
//! - `effective` (`YYYY-MM-DD`) must be in Q4 2026 for mue rows and `end_exclusive` may be empty
//!   (open-ended): MUE tables carry no row dates. Both must be empty for ptp rows.
//!
//! PTP table, CMS columns A-G: up to 20 preamble lines, a header row, then data rows.
//! The header is the first line whose seven cells, lowercased with only `a-z0-9` kept, are:
//! `column1*`, `column2*`, `*1996*`, `effectivedate*`, `deletiondate*`, `modifier*`,
//! `*rationale*`. The CMS title `Column1/Column2 Edits` can precede the header.
//! A malformed or missing header is header drift. Data rows: two 5-character codes
//! `[0-9A-Z]{5}`, `*` or empty (edit existed before 1996), effective date `YYYYMMDD`,
//! deletion date `YYYYMMDD`, `*` or empty,
//! modifier indicator `0`, `1` or `9`, rationale. CMS lists the last active day, so the
//! stored end is the deletion date plus one day. Indicator 9 needs a deletion date equal to
//! the effective date and stores the empty interval `[effective, effective)`. A row that takes
//! effect after Q4 2026 (on or after 2027-01-01) is a field error on `effective date`.
//!
//! MUE table, columns A-D: same preamble rule. Header cells: `hcpcs*`, `*muevalue*`,
//! `*adjudicationindicator*`, `*rationale*`. The units header (column B) must also name the
//! manifest row's setting, normalized as above: it contains `practitioner`, `hospital` or
//! `dme`. A table filed under another setting's row is a setting mismatch, so swapped files
//! cannot apply one setting's limits to another. PTP headers carry no setting, so a PTP file
//! listed under the wrong setting cannot be detected; the manifest is the only record of it.
//! Data rows: code, units `0..=65535`, indicator, rationale. The indicator is exactly one digit
//! `1`, `2` or `3`, then either the end of the field or one ASCII space and any text (CMS
//! writes `1 Line Edit` and `2 Date of Service Edit: Policy`); the text is dropped. `1.5`,
//! `12`, `1-x`, `1Line`, `0`, `4` and an empty field are rejected.
//!
//! A table needs at least one data row after its header. A table with a header and nothing
//! after it, or only blank rows, is an error rather than an empty edit list.
//!
//! Cells are trimmed of ASCII spaces and read as bytes, so the rationale columns and
//! preamble may hold Latin-1. A parsed field must be ASCII. Rationale and descriptor text is
//! never stored, and no cell content appears in an error.
//!
//! Manifest rows in errors are physical line numbers in `manifest.tsv` (line 1 is the
//! header); table rows are physical line numbers in the table file.

use crate::encoding::{
    encode_code_b36, encode_epoch_day, MueRecord, NcciPtpRecord, NO_DELETION_DATE,
};
use crate::index;
use csv::ByteRecord;
use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use time::{
    macros::{date, format_description},
    Date, Month,
};

const MANIFEST: &str = "manifest.tsv";
const MANIFEST_HEADER: &str = "role\tsetting\tfile\teffective\tend_exclusive";
const MAX_PREAMBLE: usize = 20;
const MAX_UNITS: u32 = u16::MAX as u32;

/// Fixed field name and reason for a rejected cell.
type Fault = (&'static str, &'static str);

/// A manifest row, for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableRef {
    pub manifest_row: usize,
    pub role: &'static str,
    pub setting: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmsError {
    /// `manifest_row` is `None` for `manifest.tsv` itself.
    Io {
        manifest_row: Option<usize>,
        kind: io::ErrorKind,
    },
    Manifest {
        row: usize,
        field: &'static str,
        reason: &'static str,
    },
    MissingRole(&'static str),
    HeaderDrift {
        table: TableRef,
        line: Option<u64>,
    },
    /// The header matches the layout, but the header names a different setting than the manifest row.
    SettingMismatch {
        table: TableRef,
        line: u64,
    },
    /// A valid header with no data rows after it.
    EmptyTable {
        table: TableRef,
    },
    Row {
        table: TableRef,
        line: u64,
        field: &'static str,
        reason: &'static str,
    },
    /// Two rows give one edit different values on overlapping days. `earlier` starts first.
    Conflict {
        earlier: RowRef,
        later: RowRef,
        field: &'static str,
    },
}

/// A data row of a table, for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowRef {
    pub table: TableRef,
    pub line: u64,
}

impl fmt::Display for RowRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} table row {}", self.table, self.line)
    }
}

impl fmt::Display for TableRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "manifest row {} ({} {})",
            self.manifest_row, self.role, self.setting
        )
    }
}

impl fmt::Display for CmsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                manifest_row: None,
                kind,
            } => write!(f, "cms-tsv: cannot read {MANIFEST}: {kind}"),
            Self::Io {
                manifest_row: Some(row),
                kind,
            } => write!(
                f,
                "cms-tsv: manifest row {row}, cannot read table file: {kind}"
            ),
            Self::Manifest { row, field, reason } => {
                write!(f, "cms-tsv: manifest row {row}, field {field}: {reason}")
            }
            Self::MissingRole(role) => write!(f, "cms-tsv: manifest has no {role} rows"),
            Self::HeaderDrift { table, line } => {
                write!(f, "cms-tsv: {table}")?;
                if let Some(line) = line {
                    write!(f, ", table row {line}")?;
                }
                write!(f, ": header drift, columns differ from the CMS layout")
            }
            Self::SettingMismatch { table, line } => write!(
                f,
                "cms-tsv: {table}, table row {line}: header names a different setting than the manifest"
            ),
            Self::EmptyTable { table } => write!(
                f,
                "cms-tsv: {table}: table has a header but no data rows"
            ),
            Self::Row {
                table,
                line,
                field,
                reason,
            } => write!(
                f,
                "cms-tsv: {table}, table row {line}, field {field}: {reason}"
            ),
            Self::Conflict {
                earlier,
                later,
                field,
            } => write!(
                f,
                "cms-tsv: conflicting overlapping edits: {earlier} and {later} overlap with a different {field}"
            ),
        }
    }
}

impl std::error::Error for CmsError {}

enum Kind {
    Ptp,
    /// Encoded `(effective, end_exclusive)` from the manifest.
    Mue(u16, u16),
}

struct Entry {
    table: TableRef,
    kind: Kind,
    setting: u8,
    file: String,
}

fn manifest_fault(row: usize, field: &'static str, reason: &'static str) -> CmsError {
    CmsError::Manifest { row, field, reason }
}

fn iso_day(value: &str) -> Option<Date> {
    Date::parse(value, format_description!("[year]-[month]-[day]")).ok()
}

fn bare_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("..")
        && !name.contains(['/', '\\', ':', '\0'])
        && !Path::new(name).is_absolute()
}

fn read_manifest(dir: &Path) -> Result<Vec<Entry>, CmsError> {
    let io_error = |e: io::Error| CmsError::Io {
        manifest_row: None,
        kind: e.kind(),
    };
    let mut bytes = Vec::new();
    File::open(dir.join(MANIFEST))
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(io_error)?;
    let mut lines = bytes.split(|&b| b == b'\n').enumerate().peekable();
    let mut entries = Vec::new();
    let mut files = HashSet::new();
    let mut header_seen = false;
    while let Some((index, raw)) = lines.next() {
        let row = index + 1;
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if raw.is_empty() && (header_seen || lines.peek().is_none()) {
            continue;
        }
        let line =
            std::str::from_utf8(raw).map_err(|_| manifest_fault(row, "row", "not valid UTF-8"))?;
        if !header_seen {
            if line != MANIFEST_HEADER {
                return Err(manifest_fault(1, "header", "unexpected columns"));
            }
            header_seen = true;
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        let [role, setting, file, effective, end] = cells[..] else {
            return Err(manifest_fault(row, "columns", "expected 5 columns"));
        };
        let is_ptp = match role {
            "ptp" => true,
            "mue" => false,
            _ => return Err(manifest_fault(row, "role", "expected ptp or mue")),
        };
        let (flag, setting_name) = match (setting, is_ptp) {
            ("practitioner", _) => (1, "practitioner"),
            ("hospital", _) => (2, "hospital"),
            ("dme", false) => (4, "dme"),
            _ => {
                return Err(manifest_fault(
                    row,
                    "setting",
                    "expected practitioner or hospital, or dme for mue",
                ))
            }
        };
        if !bare_name(file) {
            return Err(manifest_fault(row, "file", "expected a bare file name"));
        }
        if !files.insert(file.to_ascii_lowercase()) {
            return Err(manifest_fault(row, "file", "listed more than once"));
        }
        let kind = if is_ptp {
            if !effective.is_empty() {
                return Err(manifest_fault(row, "effective", "must be empty for ptp"));
            }
            if !end.is_empty() {
                return Err(manifest_fault(
                    row,
                    "end_exclusive",
                    "must be empty for ptp",
                ));
            }
            Kind::Ptp
        } else {
            let effective_date = iso_day(effective)
                .ok_or_else(|| manifest_fault(row, "effective", "expected YYYY-MM-DD"))?;
            if !(date!(2026 - 10 - 01)..date!(2027 - 01 - 01)).contains(&effective_date) {
                return Err(manifest_fault(
                    row,
                    "effective",
                    "must be in Q4 2026 for v323r0",
                ));
            }
            let start = encode_epoch_day(effective_date).unwrap();
            let end = if end.is_empty() {
                NO_DELETION_DATE
            } else {
                iso_day(end)
                    .and_then(encode_epoch_day)
                    .ok_or_else(|| manifest_fault(row, "end_exclusive", "expected YYYY-MM-DD"))?
            };
            if end <= start {
                return Err(manifest_fault(
                    row,
                    "end_exclusive",
                    "must be after effective",
                ));
            }
            Kind::Mue(start, end)
        };
        entries.push(Entry {
            table: TableRef {
                manifest_row: row,
                role: if is_ptp { "ptp" } else { "mue" },
                setting: setting_name,
            },
            kind,
            setting: flag,
            file: file.to_owned(),
        });
    }
    if !header_seen {
        return Err(manifest_fault(1, "header", "unexpected columns"));
    }
    for role in ["ptp", "mue"] {
        if !entries.iter().any(|e| e.table.role == role) {
            return Err(CmsError::MissingRole(role));
        }
    }
    Ok(entries)
}

fn trim(cell: &[u8]) -> &[u8] {
    let start = cell.iter().position(|&b| b != b' ').unwrap_or(cell.len());
    let end = cell
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(start, |i| i + 1);
    &cell[start..end]
}

fn normalized(cell: &[u8]) -> String {
    cell.iter()
        .filter(|b| b.is_ascii_alphanumeric())
        .map(|b| char::from(b.to_ascii_lowercase()))
        .collect()
}

/// How a table's header row is recognized.
struct Layout {
    prefix: &'static str,
    cells: &'static [(Match, &'static str)],
    /// The header cell that names the table's setting, when the layout has one.
    setting_cell: Option<usize>,
}

#[derive(Clone, Copy)]
enum Match {
    Starts,
    Contains,
}

const PTP_LAYOUT: Layout = Layout {
    prefix: "column1",
    cells: &[
        (Match::Starts, "column1"),
        (Match::Starts, "column2"),
        (Match::Contains, "1996"),
        (Match::Starts, "effectivedate"),
        (Match::Starts, "deletiondate"),
        (Match::Starts, "modifier"),
        (Match::Contains, "rationale"),
    ],
    setting_cell: None,
};

const MUE_LAYOUT: Layout = Layout {
    prefix: "hcpcs",
    cells: &[
        (Match::Starts, "hcpcs"),
        (Match::Contains, "muevalue"),
        (Match::Contains, "adjudicationindicator"),
        (Match::Contains, "rationale"),
    ],
    setting_cell: Some(1),
};

impl Layout {
    fn is_header(&self, row: &ByteRecord) -> bool {
        row.len() == self.cells.len()
            && row.iter().zip(self.cells).all(|(cell, &(how, text))| {
                let cell = normalized(cell);
                match how {
                    Match::Starts => cell.starts_with(text),
                    Match::Contains => cell.contains(text),
                }
            })
    }

    /// Whether the header cell that names a setting names `setting`; true for layouts without one.
    fn names_setting(&self, row: &ByteRecord, setting: &str) -> bool {
        self.setting_cell
            .is_none_or(|cell| normalized(&row[cell]).contains(setting))
    }

    fn looks_like_header(&self, row: &ByteRecord) -> bool {
        row.get(0).is_some_and(|cell| {
            let first = normalized(cell);
            let ptp_title = self.prefix == "column1"
                && first == "column1column2edits"
                && row.iter().skip(1).all(|cell| trim(cell).is_empty());
            first.starts_with(self.prefix) && !ptp_title
        })
    }
}

fn read_table(
    dir: &Path,
    entry: &Entry,
    layout: &Layout,
    mut consume: impl FnMut(&ByteRecord, u64) -> Result<(), Fault>,
) -> Result<(), CmsError> {
    let table = entry.table;
    let io_error = |kind| CmsError::Io {
        manifest_row: Some(table.manifest_row),
        kind,
    };
    let file = File::open(dir.join(&entry.file)).map_err(|e| io_error(e.kind()))?;
    let mut reader = BufReader::new(file);
    let mut row = ByteRecord::new();
    let mut raw = Vec::new();
    let mut line = 0;
    let mut preamble = 0;
    let mut header_found = false;
    let mut data_rows = 0_u64;
    loop {
        raw.clear();
        if reader
            .read_until(b'\n', &mut raw)
            .map_err(|e| io_error(e.kind()))?
            == 0
        {
            break;
        }
        line += 1;
        let bytes = raw.strip_suffix(b"\n").unwrap_or(&raw);
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        let bytes = if line == 1 {
            bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
        } else {
            bytes
        };
        if bytes.is_empty() {
            continue;
        }
        row.clear();
        for cell in bytes.split(|&b| b == b'\t') {
            row.push_field(cell);
        }
        if !header_found {
            if layout.is_header(&row) {
                if !layout.names_setting(&row, table.setting) {
                    return Err(CmsError::SettingMismatch { table, line });
                }
                header_found = true;
            } else if layout.looks_like_header(&row) || preamble >= MAX_PREAMBLE {
                return Err(CmsError::HeaderDrift {
                    table,
                    line: Some(line),
                });
            } else {
                preamble += 1;
            }
            continue;
        }
        if row.iter().all(|cell| trim(cell).is_empty()) {
            continue;
        }
        consume(&row, line).map_err(|(field, reason)| CmsError::Row {
            table,
            line,
            field,
            reason,
        })?;
        data_rows += 1;
    }
    match (header_found, data_rows) {
        (true, 0) => Err(CmsError::EmptyTable { table }),
        (true, _) => Ok(()),
        (false, _) => Err(CmsError::HeaderDrift { table, line: None }),
    }
}

fn code(cell: &[u8]) -> Result<u32, &'static str> {
    std::str::from_utf8(cell)
        .ok()
        .filter(|text| text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'Z')))
        .and_then(encode_code_b36)
        .ok_or("expected 5 characters from 0-9 and A-Z")
}

fn cms_date(cell: &[u8]) -> Result<Date, &'static str> {
    const EXPECTED: &str = "expected YYYYMMDD";
    if cell.len() != 8 || !cell.iter().all(u8::is_ascii_digit) {
        return Err(EXPECTED);
    }
    let number = |range: std::ops::Range<usize>| {
        cell[range]
            .iter()
            .fold(0_u32, |n, b| n * 10 + u32::from(b - b'0'))
    };
    let month = Month::try_from(number(4..6) as u8).map_err(|_| EXPECTED)?;
    Date::from_calendar_date(number(0..4) as i32, month, number(6..8) as u8).map_err(|_| EXPECTED)
}

fn day(date: Date) -> Result<u16, &'static str> {
    encode_epoch_day(date).ok_or("date outside index epoch")
}

fn cells<'a, const N: usize>(
    row: &'a ByteRecord,
    reason: &'static str,
) -> Result<[&'a [u8]; N], Fault> {
    if row.len() != N {
        return Err(("column count", reason));
    }
    Ok(std::array::from_fn(|i| trim(&row[i])))
}

fn ptp_record(row: &ByteRecord, setting_flags: u8) -> Result<NcciPtpRecord, Fault> {
    let [a, b, c, d, e, f, _rationale] = cells(row, "expected 7 columns")?;
    let col1_b36 = code(a).map_err(|r| ("column 1", r))?;
    let col2_b36 = code(b).map_err(|r| ("column 2", r))?;
    let flags = match c {
        b"*" => 1,
        b"" => 0,
        _ => return Err(("prior to 1996", "expected * or empty")),
    };
    let effective = cms_date(d).map_err(|r| ("effective date", r))?;
    if effective >= date!(2027 - 01 - 01) {
        return Err(("effective date", "after Q4 2026, not part of v323r0"));
    }
    let eff_days = day(effective).map_err(|r| ("effective date", r))?;
    let deletion = match e {
        b"" | b"*" => None,
        _ => Some(cms_date(e).map_err(|_| ("deletion date", "expected YYYYMMDD, *, or blank"))?),
    };
    let ccmi = match f {
        b"0" => 0,
        b"1" => 1,
        b"9" => 9,
        _ => return Err(("modifier indicator", "expected 0, 1 or 9")),
    };
    let del_days = match (ccmi, deletion) {
        (9, Some(date)) if date == effective => eff_days,
        (9, _) => {
            return Err((
                "deletion date",
                "must equal the effective date for indicator 9",
            ))
        }
        (_, None) => NO_DELETION_DATE,
        (_, Some(date)) if date < effective => {
            return Err(("deletion date", "must not precede the effective date"))
        }
        (_, Some(date)) => date
            .next_day()
            .ok_or("date outside index epoch")
            .and_then(day)
            .map_err(|r| ("deletion date", r))?,
    };
    Ok(NcciPtpRecord {
        col1_b36,
        col2_b36,
        eff_days,
        del_days,
        ccmi,
        setting_flags,
        rationale_id: 0,
        flags,
    })
}

fn mue_record(row: &ByteRecord, setting_flags: u8, days: (u16, u16)) -> Result<MueRecord, Fault> {
    let [a, b, c, _rationale] = cells(row, "expected 4 columns")?;
    let code_b36 = code(a).map_err(|r| ("code", r))?;
    const UNITS: Fault = ("units", "expected a number from 0 to 65535");
    if b.is_empty() || b.len() > 5 || !b.iter().all(u8::is_ascii_digit) {
        return Err(UNITS);
    }
    let units = b.iter().fold(0_u32, |n, d| n * 10 + u32::from(d - b'0'));
    if units > MAX_UNITS {
        return Err(UNITS);
    }
    // CMS writes the indicator digit alone or followed by a space and its name.
    let mai = match c {
        [digit @ b'1'..=b'3'] | [digit @ b'1'..=b'3', b' ', ..] => digit - b'0',
        _ => {
            return Err((
                "adjudication indicator",
                "expected 1, 2 or 3, alone or followed by a space and text",
            ))
        }
    };
    Ok(MueRecord {
        code_b36,
        max_units: units as u16,
        eff_days: days.0,
        del_days: days.1,
        mai,
        setting_flags,
        rationale_id: 0,
        flags: 0,
        reserved: 0,
    })
}

/// Where a parsed record came from: its manifest entry and table line.
#[derive(Clone, Copy)]
struct Origin {
    entry: usize,
    line: u64,
}

/// Parses every table named by `DIR/manifest.tsv` and rejects overlapping edits that disagree,
/// naming both source rows. The records are not sorted; `index::write_ptp` and
/// `index::write_mue` sort them and validate them again.
pub fn load(dir: &Path) -> Result<(Vec<NcciPtpRecord>, Vec<MueRecord>), CmsError> {
    let entries = read_manifest(dir)?;
    let mut ptp = Vec::new();
    let mut mue = Vec::new();
    let mut ptp_origin = Vec::new();
    let mut mue_origin = Vec::new();
    for (entry_number, entry) in entries.iter().enumerate() {
        match entry.kind {
            Kind::Ptp => read_table(dir, entry, &PTP_LAYOUT, |row, line| {
                ptp.push(ptp_record(row, entry.setting)?);
                ptp_origin.push(Origin {
                    entry: entry_number,
                    line,
                });
                Ok(())
            })?,
            Kind::Mue(start, end) => read_table(dir, entry, &MUE_LAYOUT, |row, line| {
                mue.push(mue_record(row, entry.setting, (start, end))?);
                mue_origin.push(Origin {
                    entry: entry_number,
                    line,
                });
                Ok(())
            })?,
        }
    }
    let source = |origins: &[Origin], at: usize| {
        let origin = origins[at];
        RowRef {
            table: entries[origin.entry].table,
            line: origin.line,
        }
    };
    let conflicts = [
        index::ptp_conflict(&ptp).map(|c| (c, &ptp_origin)),
        index::mue_conflict(&mue).map(|c| (c, &mue_origin)),
    ];
    if let Some((conflict, origins)) = conflicts.into_iter().flatten().next() {
        return Err(CmsError::Conflict {
            earlier: source(origins, conflict.earlier),
            later: source(origins, conflict.later),
            field: conflict.field,
        });
    }
    Ok((ptp, mue))
}
