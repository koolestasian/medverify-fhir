//! Version 1 stores a 64-byte header followed by little-endian, 16-byte records.
//! Version 2 (PTP only) puts a directory of 12-byte entries, one per column-one code,
//! between the header and the records. Each setting has its own row. Date intervals are
//! half-open.

use crate::encoding::{
    MueRecord, NcciPtpRecord, PtpDirectoryEntry, MAX_PROCEDURE_CODE, NO_DELETION_DATE,
};
use bytemuck::Pod;
use memmap2::{Mmap, MmapMut};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::path::Path;

const HEADER: usize = 64;
const ROW: usize = 16;
const ENTRY: usize = 12;
const MAGIC: &[u8; 8] = b"MVNCCI01";
const SALT: [u32; 8] = [
    0x47b6_137b,
    0x4497_4d91,
    0x8824_ad5b,
    0xa2b7_289d,
    0x7054_95c7,
    0x2df1_424b,
    0x9efc_4947,
    0x5c6b_fb31,
];

#[derive(Clone, Copy, Default)]
#[repr(C, align(32))]
struct Block([u32; 8]);

/// Split-block Bloom filter over the distinct PTP keys, built at load; a negative answer is exact.
struct PairFilter {
    blocks: Vec<Block>,
    mask: u64,
}

impl PairFilter {
    fn new(distinct_keys: usize) -> Self {
        let blocks = distinct_keys.div_ceil(32).max(1).next_power_of_two();
        Self {
            blocks: vec![Block::default(); blocks],
            mask: blocks as u64 - 1,
        }
    }

    fn from_sorted(rows: &[NcciPtpRecord]) -> Self {
        let key = |row: &NcciPtpRecord| (row.col1_b36, row.col2_b36, row.setting_flags);
        let distinct = rows
            .iter()
            .zip(rows.iter().skip(1))
            .filter(|(a, b)| key(a) != key(b))
            .count()
            + usize::from(!rows.is_empty());
        let mut filter = Self::new(distinct);
        for (i, row) in rows.iter().enumerate() {
            if i == 0 || key(&rows[i - 1]) != key(row) {
                filter.insert(row.col1_b36, row.col2_b36, row.setting_flags);
            }
        }
        filter
    }

    /// `None` for values no valid record can hold.
    fn locate(&self, col1: u32, col2: u32, setting: u8) -> Option<(usize, [u32; 8])> {
        if col1 > MAX_PROCEDURE_CODE || col2 > MAX_PROCEDURE_CODE || setting > 3 {
            return None;
        }
        let packed = (u64::from(col1) << 28) | (u64::from(col2) << 2) | u64::from(setting);
        let mut hash = packed;
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        hash ^= hash >> 33;
        let low = hash as u32;
        let bits = SALT.map(|salt| 1 << (low.wrapping_mul(salt) >> 27));
        Some((((hash >> 32) & self.mask) as usize, bits))
    }

    fn insert(&mut self, col1: u32, col2: u32, setting: u8) {
        let (block, bits) = self
            .locate(col1, col2, setting)
            .expect("validated index record");
        for (word, bit) in self.blocks[block].0.iter_mut().zip(bits) {
            *word |= bit;
        }
    }

    fn may_contain(&self, col1: u32, col2: u32, setting: u8) -> bool {
        self.locate(col1, col2, setting)
            .is_some_and(|(block, bits)| {
                let words = &self.blocks[block].0;
                words.iter().zip(bits).all(|(word, bit)| word & bit != 0)
            })
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogMode {
    Synthetic = 0,
    Licensed = 1,
    /// Any code starting with `Y`, for scale tests.
    SyntheticStress = 2,
}

impl CatalogMode {
    pub fn release(self) -> &'static str {
        match self {
            Self::Synthetic => "synth01",
            Self::Licensed => "v323r0",
            Self::SyntheticStress => "stress01",
        }
    }

    fn accepts(self, code: u32) -> bool {
        if self == Self::Licensed {
            return code <= MAX_PROCEDURE_CODE;
        }
        let Some(bytes) = crate::encoding::decode_code_b36(code) else {
            return false;
        };
        if self == Self::SyntheticStress {
            return bytes[0] == b'Y';
        }
        // Base 36 also places letter-suffixed codes between Y9001 and Y9120.
        bytes[1..].iter().all(u8::is_ascii_digit) && (*b"Y9001"..=*b"Y9120").contains(&bytes)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

type Key = (u32, u32, u8);
type Rule = (u16, u8, u8, u8);

trait Record: Pod {
    const KIND: u8;
    /// Source field names of the four `Rule` components, for conflict diagnostics.
    const RULE_FIELDS: [&'static str; 4];
    fn decode(bytes: &[u8; ROW]) -> Self;
    fn key(&self) -> Key;
    fn dates(&self) -> (u16, u16);
    fn rule(&self) -> Rule;
    fn valid(&self, mode: CatalogMode) -> bool;

    fn order(&self) -> (Key, (u16, u16), Rule) {
        (self.key(), self.dates(), self.rule())
    }
}

fn word(bytes: &[u8; ROW], i: usize) -> u16 {
    u16::from_le_bytes([bytes[i], bytes[i + 1]])
}

fn code(bytes: &[u8; ROW], i: usize) -> u32 {
    u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap())
}

impl Record for NcciPtpRecord {
    const KIND: u8 = 1;
    const RULE_FIELDS: [&'static str; 4] =
        ["rule", "modifier indicator", "rationale", "prior to 1996"];
    fn decode(b: &[u8; ROW]) -> Self {
        Self {
            col1_b36: code(b, 0),
            col2_b36: code(b, 4),
            eff_days: word(b, 8),
            del_days: word(b, 10),
            ccmi: b[12],
            setting_flags: b[13],
            rationale_id: b[14],
            flags: b[15],
        }
    }
    fn key(&self) -> Key {
        (self.col1_b36, self.col2_b36, self.setting_flags)
    }
    fn dates(&self) -> (u16, u16) {
        (self.eff_days, self.del_days)
    }
    fn rule(&self) -> Rule {
        (0, self.ccmi, self.rationale_id, self.flags)
    }
    fn valid(&self, mode: CatalogMode) -> bool {
        mode.accepts(self.col1_b36)
            && mode.accepts(self.col2_b36)
            && matches!(self.setting_flags, 1 | 2)
            && self.flags <= 1
            && self.eff_days != NO_DELETION_DATE
            && match self.ccmi {
                0 | 1 => self.eff_days < self.del_days,
                9 => self.eff_days == self.del_days,
                _ => false,
            }
    }
}

impl Record for MueRecord {
    const KIND: u8 = 2;
    const RULE_FIELDS: [&'static str; 4] =
        ["units", "adjudication indicator", "rationale", "flags"];
    fn decode(b: &[u8; ROW]) -> Self {
        Self {
            code_b36: code(b, 0),
            max_units: word(b, 4),
            eff_days: word(b, 6),
            del_days: word(b, 8),
            mai: b[10],
            setting_flags: b[11],
            rationale_id: b[12],
            flags: b[13],
            reserved: word(b, 14),
        }
    }
    fn key(&self) -> Key {
        (self.code_b36, 0, self.setting_flags)
    }
    fn dates(&self) -> (u16, u16) {
        (self.eff_days, self.del_days)
    }
    fn rule(&self) -> Rule {
        (self.max_units, self.mai, self.rationale_id, self.flags)
    }
    fn valid(&self, mode: CatalogMode) -> bool {
        mode.accepts(self.code_b36)
            && matches!(self.setting_flags, 1 | 2 | 4)
            && (1..=3).contains(&self.mai)
            && self.eff_days < self.del_days
            && self.flags == 0
            && self.reserved == 0
    }
}

/// Two records of one key whose date intervals overlap but whose rules differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// Position, in the slice that was checked, of the record whose interval starts first.
    pub earlier: usize,
    /// Position of the record that overlaps it with a different rule.
    pub later: usize,
    /// The first source field whose value differs between them.
    pub field: &'static str,
}

/// Finds overlapping PTP edits that disagree. Positions refer to `rows` as given, so a caller
/// that kept each record's source can report both sources. Records need not be sorted.
pub fn ptp_conflict(rows: &[NcciPtpRecord]) -> Option<Conflict> {
    conflict_in(rows)
}

/// Finds overlapping MUE edits that disagree, as [`ptp_conflict`] does.
pub fn mue_conflict(rows: &[MueRecord]) -> Option<Conflict> {
    conflict_in(rows)
}

fn conflict_in<R: Record>(rows: &[R]) -> Option<Conflict> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_unstable_by_key(|&i| (rows[i].order(), i));
    let mut overlaps = Overlaps::default();
    order
        .into_iter()
        .find_map(|i| overlaps.check(rows, i).err())
}

/// The longest active interval seen so far for the current key, with its position.
#[derive(Default)]
struct Overlaps {
    active: Option<usize>,
}

impl Overlaps {
    /// `rows[at]` must not precede any earlier checked row in `Record::order`.
    fn check<R: Record>(&mut self, rows: &[R], at: usize) -> Result<(), Conflict> {
        let row = rows[at];
        let (start, end) = row.dates();
        if start == end {
            return Ok(());
        }
        // A shorter nested interval or an inactive edit must not hide a longer one.
        if let Some(position) = self.active {
            let active = rows[position];
            if active.key() == row.key() && active.dates().1 > start {
                if active.rule() != row.rule() {
                    let (a, b) = (active.rule(), row.rule());
                    let differs = [a.0 != b.0, a.1 != b.1, a.2 != b.2, a.3 != b.3];
                    let field = R::RULE_FIELDS[differs.iter().position(|&d| d).unwrap_or(0)];
                    return Err(Conflict {
                        earlier: position,
                        later: at,
                        field,
                    });
                }
                if active.dates().1 >= end {
                    return Ok(());
                }
            }
        }
        self.active = Some(at);
        Ok(())
    }
}

/// Checks validity, order and overlaps of records in stored order.
struct Validator {
    mode: CatalogMode,
    overlaps: Overlaps,
}

impl Validator {
    fn new(mode: CatalogMode) -> Self {
        Self {
            mode,
            overlaps: Overlaps::default(),
        }
    }

    /// `rows[..=at]` are the records checked so far, in stored order.
    fn check<R: Record>(&mut self, rows: &[R], at: usize) -> io::Result<()> {
        let row = rows[at];
        if !row.valid(self.mode) {
            return Err(invalid("invalid index record"));
        }
        if at > 0 && rows[at - 1].order() > row.order() {
            return Err(invalid("index records out of order"));
        }
        self.overlaps
            .check(rows, at)
            .map_err(|_| invalid("conflicting overlapping edits"))
    }
}

fn base_header(version: u16, kind: u8, mode: CatalogMode) -> [u8; HEADER] {
    let mut h = [0; HEADER];
    h[..8].copy_from_slice(MAGIC);
    h[8..10].copy_from_slice(&version.to_le_bytes());
    h[10] = kind;
    h[11] = mode as u8;
    h[12..16].copy_from_slice(&(ROW as u32).to_le_bytes());
    let release = mode.release().as_bytes();
    h[24..24 + release.len()].copy_from_slice(release);
    h
}

fn header_v1<R: Record>(mode: CatalogMode, count: u64) -> [u8; HEADER] {
    let mut h = base_header(1, R::KIND, mode);
    h[16..24].copy_from_slice(&count.to_le_bytes());
    h
}

fn header_v2(mode: CatalogMode, records: u32, directory: u32) -> [u8; HEADER] {
    let mut h = base_header(2, NcciPtpRecord::KIND, mode);
    h[16..20].copy_from_slice(&records.to_le_bytes());
    h[20..24].copy_from_slice(&directory.to_le_bytes());
    h
}

fn sort_and_check<R: Record>(mode: CatalogMode, rows: &mut [R]) -> io::Result<()> {
    rows.sort_unstable_by_key(Record::order);
    let mut validator = Validator::new(mode);
    for at in 0..rows.len() {
        validator.check(rows, at)?;
    }
    Ok(())
}

fn write_index<R: Record>(
    mut output: impl Write,
    mode: CatalogMode,
    rows: &mut [R],
) -> io::Result<()> {
    sort_and_check(mode, rows)?;
    let mut h = header_v1::<R>(mode, rows.len() as u64);
    #[cfg(target_endian = "big")]
    for row in rows.iter_mut() {
        *row = R::decode(bytemuck::bytes_of(row).try_into().unwrap());
    }
    let payload = bytemuck::cast_slice(rows);
    h[32..].copy_from_slice(&Sha256::digest(payload));
    output.write_all(&h)?;
    output.write_all(payload)
}

/// One entry per column-one code, over rows that are already sorted.
fn directory_of(rows: &[NcciPtpRecord]) -> io::Result<Vec<PtpDirectoryEntry>> {
    let mut directory: Vec<PtpDirectoryEntry> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        match directory.last_mut() {
            Some(entry) if entry.col1_b36 == row.col1_b36 => entry.row_count += 1,
            _ => directory.push(PtpDirectoryEntry {
                col1_b36: row.col1_b36,
                first_row: u32::try_from(i).map_err(|_| invalid("too many index records"))?,
                row_count: 1,
            }),
        }
    }
    Ok(directory)
}

pub fn write_ptp(
    mut output: impl Write,
    mode: CatalogMode,
    mut rows: Vec<NcciPtpRecord>,
) -> io::Result<()> {
    let count = u32::try_from(rows.len()).map_err(|_| invalid("too many index records"))?;
    sort_and_check(mode, &mut rows)?;
    let mut directory = directory_of(&rows)?;
    let mut h = header_v2(mode, count, directory.len() as u32);
    for entry in &mut directory {
        *entry = PtpDirectoryEntry {
            col1_b36: entry.col1_b36.to_le(),
            first_row: entry.first_row.to_le(),
            row_count: entry.row_count.to_le(),
        };
    }
    #[cfg(target_endian = "big")]
    for row in rows.iter_mut() {
        *row = NcciPtpRecord::decode(bytemuck::bytes_of(row).try_into().unwrap());
    }
    let directory: &[u8] = bytemuck::cast_slice(&directory);
    let records: &[u8] = bytemuck::cast_slice(&rows);
    let digest = Sha256::new()
        .chain_update(directory)
        .chain_update(records)
        .finalize();
    h[32..].copy_from_slice(&digest);
    output.write_all(&h)?;
    output.write_all(directory)?;
    output.write_all(records)
}

/// Legacy layout without a directory.
pub fn write_ptp_v1(
    output: impl Write,
    mode: CatalogMode,
    mut rows: Vec<NcciPtpRecord>,
) -> io::Result<()> {
    write_index(output, mode, &mut rows)
}

pub fn write_mue(
    output: impl Write,
    mode: CatalogMode,
    mut rows: Vec<MueRecord>,
) -> io::Result<()> {
    write_index(output, mode, &mut rows)
}

pub struct MappedIndex<R> {
    map: Mmap,
    mode: CatalogMode,
    version: u16,
    directory_len: usize,
    records_offset: usize,
    filter: Option<PairFilter>,
    record: PhantomData<R>,
}

pub type MmapPtpIndex = MappedIndex<NcciPtpRecord>;
pub type MmapMueIndex = MappedIndex<MueRecord>;

impl<R: Pod> MappedIndex<R> {
    pub fn records(&self) -> &[R] {
        bytemuck::cast_slice(&self.map[self.records_offset..])
    }
    pub fn catalog_mode(&self) -> CatalogMode {
        self.mode
    }
    pub fn release(&self) -> &'static str {
        self.mode.release()
    }
    pub fn format_version(&self) -> u16 {
        self.version
    }
}

impl MmapPtpIndex {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        open(path.as_ref())
    }

    /// Size of the load-time pair filter.
    pub fn filter_bytes(&self) -> usize {
        self.filter.as_ref().map_or(0, |filter| {
            filter.blocks.len() * std::mem::size_of::<Block>()
        })
    }

    /// The column-one directory; empty for version 1 files.
    pub fn directory(&self) -> &[PtpDirectoryEntry] {
        bytemuck::cast_slice(&self.map[HEADER..HEADER + self.directory_len * ENTRY])
    }

    /// Finds a column-ordered PTP pair for the encoded setting and service day.
    pub fn active_on(&self, col1: u32, col2: u32, setting: u8, day: u16) -> Option<&NcciPtpRecord> {
        // A negative answer is exact, so no record is read for it.
        if let Some(filter) = &self.filter {
            if !filter.may_contain(col1, col2, setting) {
                return None;
            }
        }
        let key = (col1, col2, setting);
        if self.version == 1 {
            return active_record(self.records(), key, day);
        }
        let directory = self.directory();
        let entry = directory[directory
            .binary_search_by_key(&col1, |entry| entry.col1_b36)
            .ok()?];
        let first = entry.first_row as usize;
        let bucket = self
            .records()
            .get(first..first + entry.row_count as usize)?;
        active_record(bucket, key, day)
    }
}

impl MmapMueIndex {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        open(path.as_ref())
    }

    pub(crate) fn active_on(&self, code: u32, setting: u8, day: u16) -> Option<&MueRecord> {
        active_record(self.records(), (code, 0, setting), day)
    }
}

/// Rows are ordered by `(key, eff_days)`, so one upper bound finds every candidate.
fn active_record<R: Record>(rows: &[R], key: Key, day: u16) -> Option<&R> {
    let end = rows.partition_point(|row| (row.key(), row.dates().0) <= (key, day));
    // A later short or empty interval can coexist with an earlier active one.
    rows[..end]
        .iter()
        .rev()
        .take_while(|row| row.key() == key)
        .find(|row| day < row.dates().1)
}

fn check_directory<R: Record>(directory: &[PtpDirectoryEntry], rows: &[R]) -> io::Result<()> {
    if directory.is_empty() != rows.is_empty() {
        return Err(invalid("index directory disagrees with record count"));
    }
    let mut next = 0_u32;
    let mut previous = None;
    for entry in directory {
        if previous.is_some_and(|p| p >= entry.col1_b36) {
            return Err(invalid("index directory out of order"));
        }
        previous = Some(entry.col1_b36);
        if entry.first_row != next || entry.row_count == 0 {
            return Err(invalid("index directory has a gap, overlap or empty entry"));
        }
        next = next
            .checked_add(entry.row_count)
            .ok_or_else(|| invalid("index directory overflows"))?;
        let bucket = rows
            .get(entry.first_row as usize..next as usize)
            .ok_or_else(|| invalid("index directory exceeds records"))?;
        if bucket.iter().any(|row| row.key().0 != entry.col1_b36) {
            return Err(invalid("index directory disagrees with records"));
        }
    }
    if next as usize != rows.len() {
        return Err(invalid("index directory does not cover all records"));
    }
    Ok(())
}

fn open<R: Record>(path: &Path) -> io::Result<MappedIndex<R>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut h = [0; HEADER];
    file.read_exact(&mut h)?;
    let mode = match h[11] {
        0 => CatalogMode::Synthetic,
        1 => CatalogMode::Licensed,
        2 => CatalogMode::SyntheticStress,
        _ => return Err(invalid("unknown catalog mode")),
    };
    let version = u16::from_le_bytes([h[8], h[9]]);
    let (count, directory_len, expected) = match version {
        1 => {
            let count = u64::from_le_bytes(h[16..24].try_into().unwrap());
            (count, 0, header_v1::<R>(mode, count))
        }
        2 if R::KIND == NcciPtpRecord::KIND => {
            let count = u32::from_le_bytes(h[16..20].try_into().unwrap());
            let directory = u32::from_le_bytes(h[20..24].try_into().unwrap());
            (
                u64::from(count),
                u64::from(directory),
                header_v2(mode, count, directory),
            )
        }
        _ => return Err(invalid("unsupported index version")),
    };
    if h[..32] != expected[..32] {
        return Err(invalid("invalid index header"));
    }
    let records_offset = HEADER as u64 + directory_len * ENTRY as u64;
    if count
        .checked_mul(ROW as u64)
        .and_then(|n| n.checked_add(records_offset))
        != Some(length)
    {
        return Err(invalid("index length differs from record count"));
    }
    let size = usize::try_from(length).map_err(|_| invalid("index exceeds address space"))?;
    let (records_offset, directory_len) = (records_offset as usize, directory_len as usize);
    let mut map = MmapMut::map_anon(size)?;
    map[..HEADER].copy_from_slice(&h);
    file.read_exact(&mut map[HEADER..])?;
    if file.read(&mut [0])? != 0 {
        return Err(invalid("index grew during read"));
    }
    if !map[HEADER..]
        .as_ptr()
        .cast::<PtpDirectoryEntry>()
        .is_aligned()
        || !map[records_offset..].as_ptr().cast::<R>().is_aligned()
    {
        return Err(invalid("unaligned index payload"));
    }
    if Sha256::digest(&map[HEADER..])[..] != h[32..] {
        return Err(invalid("index payload SHA-256 mismatch"));
    }
    // Records are stored little-endian; decoding in place gives native records on any target.
    for chunk in map[records_offset..].as_chunks_mut::<ROW>().0 {
        let row = R::decode(chunk);
        chunk.copy_from_slice(bytemuck::bytes_of(&row));
    }
    let rows: &[R] = bytemuck::cast_slice(&map[records_offset..]);
    let mut validator = Validator::new(mode);
    for at in 0..rows.len() {
        validator.check(rows, at)?;
    }
    #[cfg(target_endian = "big")]
    for chunk in map[HEADER..records_offset].as_chunks_mut::<ENTRY>().0 {
        let [col1_b36, first_row, row_count] =
            [0, 4, 8].map(|i| u32::from_le_bytes(chunk[i..i + 4].try_into().unwrap()));
        chunk.copy_from_slice(bytemuck::bytes_of(&PtpDirectoryEntry {
            col1_b36,
            first_row,
            row_count,
        }));
    }
    if version == 2 {
        check_directory(
            bytemuck::cast_slice(&map[HEADER..records_offset]),
            bytemuck::cast_slice::<u8, R>(&map[records_offset..]),
        )?;
    }
    let filter = (R::KIND == NcciPtpRecord::KIND)
        .then(|| PairFilter::from_sorted(bytemuck::cast_slice(&map[records_offset..])));
    Ok(MappedIndex {
        map: map.make_read_only()?,
        mode,
        version,
        directory_len,
        records_offset,
        filter,
        record: PhantomData,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: usize, seed: u64) -> impl Iterator<Item = (u32, u32, u8)> {
        let mut state = seed;
        std::iter::repeat_with(move || {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            (
                (z % u64::from(MAX_PROCEDURE_CODE + 1)) as u32,
                ((z >> 24) % u64::from(MAX_PROCEDURE_CODE + 1)) as u32,
                1 + (z >> 60) as u8 % 2,
            )
        })
        .take(n)
    }

    #[test]
    fn filter_has_no_false_negatives_and_few_false_positives() {
        let present: Vec<_> = keys(100_000, 1).collect();
        let mut filter = PairFilter::new(present.len());
        for &(a, b, c) in &present {
            filter.insert(a, b, c);
        }
        assert!(present.iter().all(|&(a, b, c)| filter.may_contain(a, b, c)));
        let known: std::collections::HashSet<_> = present.iter().copied().collect();
        let absent: Vec<_> = keys(200_000, 2)
            .filter(|k| !known.contains(k))
            .take(100_000)
            .collect();
        assert_eq!(absent.len(), 100_000);
        let false_positives = absent
            .iter()
            .filter(|&&(a, b, c)| filter.may_contain(a, b, c))
            .count();
        assert!(false_positives < 3_000, "{false_positives} false positives");
    }

    #[test]
    fn filter_rejects_out_of_range_values() {
        let mut filter = PairFilter::new(1);
        filter.insert(1, 2, 1);
        assert!(filter.may_contain(1, 2, 1));
        assert!(!filter.may_contain(MAX_PROCEDURE_CODE + 1, 2, 1));
        assert!(!filter.may_contain(1, MAX_PROCEDURE_CODE + 1, 1));
        assert!(!filter.may_contain(1, 2, 4));
        assert!(!filter.may_contain(u32::MAX, u32::MAX, u8::MAX));
    }
}
