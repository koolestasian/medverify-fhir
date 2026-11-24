//! Deterministic synthetic catalogs and claims for scale tests, with an independent oracle.
//!
//! Every code starts with `Y`, so catalogs built here need `CatalogMode::SyntheticStress`.
//! All dates and settings are fixed: claims are practitioner claims on [`SERVICE_DAY`].

use crate::claim::{ClaimLine, ProcedureCode, ServiceDate, ServiceDay, ServiceKind, Setting};
use crate::encoding::{MueRecord, NcciPtpRecord, NO_DELETION_DATE};
use crate::index::{MmapMueIndex, MmapPtpIndex};
use crate::mue::{MueFinding, MueScope};
use crate::ncci::{PtpFinding, PtpOutcome};
use crate::{mue, ncci};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroU64;

/// Seed shared by the CLI and the benchmark. The ASCII bytes of "MVSTRESS".
pub const SEED: u64 = 0x4d56_5354_5245_5353;
/// 2026-10-01 as an index epoch day.
pub const SERVICE_DAY: u16 = 13_422;
/// 2026-01-01 as an index epoch day.
const MUE_EFFECTIVE_DAY: u16 = 13_149;

const CODE_SPACE: u32 = 36 * 36 * 36 * 36;
/// `Y` is base-36 digit 34.
const Y_BASE: u32 = 34 * CODE_SPACE;
const MEAN_BUCKET: u64 = 300;
/// Fewer codes would leave small catalogs without room for distinct pair keys.
const MIN_UNIVERSE: usize = 1024;
/// Shares of the (column one, column two, setting) keys, in parts per 10,000.
const CCMI9_PER_10K: u64 = 235;
const TWO_ROWS_PER_10K: u64 = 1_500;
const MIN_ACTIVE_DAY: u64 = 9_000;
const CLAIM_LINES: usize = 10;
const CLAIM_ID: &str = "stress-claim";

const PTP_STREAM: u64 = 0;
const MUE_STREAM: u64 = 0x6d75_6520_7374_7265;
const CLAIM_STREAM: u64 = 0x636c_6169_6d20_7374;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn new(seed: u64, stream: u64) -> Self {
        Self(seed ^ stream)
    }

    fn draw(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`, by multiply and shift; the bias is below 2^-32 for the sizes used here.
    fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.draw()) * u128::from(n)) >> 64) as u64
    }
}

/// A seeded bijection from `0..CODE_SPACE` to the `Y????` codes, so neighbours are far apart.
struct Codes {
    stride: u64,
    offset: u64,
}

impl Codes {
    fn new(rng: &mut Rng) -> Self {
        let space = u64::from(CODE_SPACE);
        // 36^4 = 2^8 * 3^8, so an odd stride that 3 does not divide is coprime to it.
        let stride = loop {
            let stride = space / 8 + rng.below(space - space / 4);
            if stride % 2 == 1 && !stride.is_multiple_of(3) {
                break stride;
            }
        };
        Self {
            stride,
            offset: rng.below(space),
        }
    }

    fn code(&self, index: usize) -> u32 {
        let space = u64::from(CODE_SPACE);
        let low = (index as u64 * self.stride + self.offset) % space;
        Y_BASE + low as u32
    }
}

/// Heavy-tailed bucket sizes that sum to `total`. Each weight is a mantissa in `[1, 2)` times
/// two to a binomial power, so the median is about half the mean and a few buckets are large.
fn bucket_sizes(total: u64, buckets: usize, cap: u64, rng: &mut Rng) -> Vec<u64> {
    assert!(total >= buckets as u64 && total <= buckets as u64 * cap);
    let weights: Vec<u64> = (0..buckets)
        .map(|_| {
            let r = rng.draw();
            (256 + (r & 255)) << ((r >> 8) & 0xfff).count_ones()
        })
        .collect();
    let sum: u128 = weights.iter().map(|&w| u128::from(w)).sum();
    let mut sizes: Vec<u64> = weights
        .iter()
        .map(|&w| ((u128::from(w) * u128::from(total) / sum) as u64).clamp(1, cap))
        .collect();
    let mut missing = total as i64 - sizes.iter().sum::<u64>() as i64;
    let mut next = 0;
    while missing != 0 {
        let size = &mut sizes[next % buckets];
        if missing > 0 && *size < cap {
            *size += 1;
            missing -= 1;
        } else if missing < 0 && *size > 1 {
            *size -= 1;
            missing += 1;
        }
        next += 1;
    }
    sizes
}

fn record(col1: u32, col2: u32, setting: u8, (eff, del): (u16, u16), ccmi: u8) -> NcciPtpRecord {
    NcciPtpRecord {
        col1_b36: col1,
        col2_b36: col2,
        eff_days: eff,
        del_days: del,
        ccmi,
        setting_flags: setting,
        rationale_id: 0,
        flags: 0,
    }
}

/// Exactly `rows` PTP records that `write_ptp` accepts under `CatalogMode::SyntheticStress`.
///
/// Column-one buckets average about 300 rows with a heavy tail, so 4,506,527 rows give about
/// 15,000 buckets and some hold several thousand rows. Column-two codes come from the same
/// code universe as column-one codes. Both settings occur. About 15% of keys carry an earlier
/// closed interval and an open one with the other indicator, about 2% of rows are CCMI 9
/// empty intervals that are never active, and every other key is active on [`SERVICE_DAY`].
pub fn ptp_records(rows: u32, seed: u64) -> Vec<NcciPtpRecord> {
    if rows == 0 {
        return Vec::new();
    }
    let mut rng = Rng::new(seed, PTP_STREAM);
    let total = u64::from(rows);
    let buckets = ((total + MEAN_BUCKET / 2) / MEAN_BUCKET).clamp(1, u64::from(CODE_SPACE / 4));
    let buckets = buckets as usize;
    let universe = buckets.max(MIN_UNIVERSE);
    // Half of the (column two, setting) keys of a bucket is the most it can use.
    let cap = universe as u64 - 1;
    let codes = Codes::new(&mut rng);
    let sizes = bucket_sizes(total, buckets, cap, &mut rng);

    let key_space = 2 * (universe - 1);
    let mut taken = vec![0_u32; key_space];
    let mut out = Vec::with_capacity(rows as usize);
    for (bucket, &size) in sizes.iter().enumerate() {
        let stamp = bucket as u32 + 1;
        let col1 = codes.code(bucket);
        let mut remaining = size;
        while remaining > 0 {
            let key = loop {
                let key = rng.below(key_space as u64) as usize;
                if taken[key] != stamp {
                    taken[key] = stamp;
                    break key;
                }
            };
            let setting = (key & 1) as u8 + 1;
            let other = key >> 1;
            let col2 = codes.code(other + usize::from(other >= bucket));
            let kind = rng.below(10_000);
            let span = 13_422 - MIN_ACTIVE_DAY;
            if kind < CCMI9_PER_10K {
                let day = 11_000 + rng.below(3_000) as u16;
                out.push(record(col1, col2, setting, (day, day), 9));
                remaining -= 1;
            } else if kind < CCMI9_PER_10K + TWO_ROWS_PER_10K && remaining >= 2 {
                let open = (MIN_ACTIVE_DAY + rng.below(span + 1)) as u16;
                let start = open - 1 - rng.below(1_500) as u16;
                let end = start + 1 + rng.below(u64::from(open - start)) as u16;
                let first = rng.below(2) as u8;
                out.push(record(col1, col2, setting, (start, end), first));
                out.push(record(
                    col1,
                    col2,
                    setting,
                    (open, NO_DELETION_DATE),
                    1 - first,
                ));
                remaining -= 2;
            } else {
                let start = (MIN_ACTIVE_DAY + rng.below(span + 1)) as u16;
                let end = if rng.below(4) == 0 {
                    SERVICE_DAY + 1 + rng.below(1_500) as u16
                } else {
                    NO_DELETION_DATE
                };
                out.push(record(
                    col1,
                    col2,
                    setting,
                    (start, end),
                    rng.below(2) as u8,
                ));
                remaining -= 1;
            }
        }
    }
    debug_assert_eq!(out.len(), rows as usize);
    out
}

/// One practitioner row for every code that appears in `ptp`, effective 2026-01-01 and open
/// ended. MAI cycles 1, 2, 3 over the sorted codes and the unit limit is 1 to 4.
pub fn mue_records(ptp: &[NcciPtpRecord], seed: u64) -> Vec<MueRecord> {
    let mut codes: Vec<u32> = ptp
        .iter()
        .flat_map(|row| [row.col1_b36, row.col2_b36])
        .collect();
    codes.sort_unstable();
    codes.dedup();
    let mut rng = Rng::new(seed, MUE_STREAM);
    codes
        .iter()
        .enumerate()
        .map(|(i, &code)| MueRecord {
            code_b36: code,
            max_units: 1 + rng.below(4) as u16,
            eff_days: MUE_EFFECTIVE_DAY,
            del_days: NO_DELETION_DATE,
            mai: (i % 3) as u8 + 1,
            setting_flags: Setting::Practitioner as u8,
            rationale_id: 0,
            flags: 0,
            reserved: 0,
        })
        .collect()
}

fn random_code(rng: &mut Rng) -> u32 {
    Y_BASE + rng.below(u64::from(CODE_SPACE)) as u32
}

fn claim_line(id: u64, code: u32, units: u64) -> ClaimLine<'static> {
    ClaimLine {
        id,
        service_day: ServiceDay {
            beneficiary: "stress-beneficiary",
            billing_provider: "stress-provider",
            date: ServiceDate(SERVICE_DAY),
            setting: Setting::Practitioner,
        },
        code: ProcedureCode(code),
        service_kind: ServiceKind::Other,
        units: NonZeroU64::new(units).unwrap(),
        modifiers: &[],
        modifier_evidence: &[],
        abn_liability_shift_attempted: false,
        // One known claim, so the MAI 1 split rules see a claim boundary.
        claim_id: Some(CLAIM_ID),
    }
}

/// `count` claims of ten lines with ten distinct codes, 1 to 3 units, and no modifiers.
///
/// Line 1 is the column-one code of a uniformly chosen directory bucket. Lines 2 to 4 are
/// column-two codes of that bucket's practitioner rows, so their pairs usually hit. Lines 5 to
/// 10 are column-one codes of other uniformly chosen buckets, so their probes search populated
/// buckets and mostly miss inside them. A claim has 45 pairs and 90 directional probes.
/// When a bucket cannot supply enough distinct codes, random `Y????` codes fill the gap.
///
/// # Panics
/// If the index has no directory, that is a version 1 or empty index.
pub fn claims(ptp: &MmapPtpIndex, count: usize, seed: u64) -> Vec<Vec<ClaimLine<'static>>> {
    let directory = ptp.directory();
    let records = ptp.records();
    assert!(
        !directory.is_empty(),
        "stress claims need a version 2 index with rows"
    );
    let mut rng = Rng::new(seed, CLAIM_STREAM);
    (0..count)
        .map(|_| {
            let home = directory[rng.below(directory.len() as u64) as usize];
            let first = home.first_row as usize;
            let bucket = &records[first..first + home.row_count as usize];
            let mut codes = vec![home.col1_b36];
            for _ in 0..64 {
                if codes.len() == 4 {
                    break;
                }
                let row = bucket[rng.below(bucket.len() as u64) as usize];
                if row.setting_flags == Setting::Practitioner as u8
                    && !codes.contains(&row.col2_b36)
                {
                    codes.push(row.col2_b36);
                }
            }
            while codes.len() < 4 {
                let code = random_code(&mut rng);
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
            for attempt in 0.. {
                if codes.len() == CLAIM_LINES {
                    break;
                }
                let code = if attempt < 64 {
                    directory[rng.below(directory.len() as u64) as usize].col1_b36
                } else {
                    random_code(&mut rng)
                };
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
            codes
                .into_iter()
                .enumerate()
                .map(|(i, code)| claim_line(i as u64 + 1, code, 1 + rng.below(3)))
                .collect()
        })
        .collect()
}

/// Where the directional probes of claims landed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbeMix {
    pub probes: u64,
    /// An edit is active for the probe.
    pub hits: u64,
    /// The column-one code has rows, but none is active for the probe.
    pub interior_misses: u64,
    /// The column-one code has no rows at all.
    pub bucket_misses: u64,
}

/// Expected PTP and MUE findings and the probe mix of one claim.
pub type Expectation<'a> = (Vec<PtpFinding<'a>>, Vec<MueFinding<'a>>, ProbeMix);

type Key = (u32, u32, u8);

struct Probe {
    claim: usize,
    key: Key,
    day: u16,
    lines: (u64, u64),
    abn_liability_shift_attempted: bool,
    active_ccmi: Option<u8>,
}

/// Expected results of one claim, computed from the records alone with no index lookup.
/// See [`expected_all`] for the supported claims.
pub fn expected<'a>(
    ptp_records: &[NcciPtpRecord],
    mue_records: &[MueRecord],
    claim: &[ClaimLine<'a>],
) -> Expectation<'a> {
    expected_all(ptp_records, mue_records, &[claim])
        .pop()
        .unwrap()
}

/// [`expected`] for many claims in one pass over the PTP records.
///
/// Each claim must use one service day and carry no modifiers. Stress claims use distinct
/// codes, so repeated MAI 1 codes are treated as a broken claim.
///
/// # Panics
/// If a claim mixes service days, carries modifiers or repeats an MAI 1 code.
pub fn expected_all<'a, C: AsRef<[ClaimLine<'a>]>>(
    ptp_records: &[NcciPtpRecord],
    mue_records: &[MueRecord],
    claims: &[C],
) -> Vec<Expectation<'a>> {
    let mut probes = Vec::new();
    for (claim, lines) in claims.iter().enumerate() {
        let lines = lines.as_ref();
        for line in lines {
            assert!(
                line.service_day == lines[0].service_day && line.modifiers.is_empty(),
                "oracle claims share one service day and carry no modifiers"
            );
        }
        let setting = lines
            .first()
            .map_or(0, |line| line.service_day.setting as u8);
        let day = lines.first().map_or(0, |line| line.service_day.date.0);
        for (i, left) in lines.iter().enumerate() {
            for right in &lines[i + 1..] {
                for (one, two) in [(left, right), (right, left)] {
                    probes.push(Probe {
                        claim,
                        key: (one.code.0, two.code.0, setting),
                        day,
                        lines: (one.id, two.id),
                        abn_liability_shift_attempted: two.abn_liability_shift_attempted,
                        active_ccmi: None,
                    });
                }
            }
        }
    }

    let mut by_key = HashMap::<Key, Vec<usize>>::new();
    for (i, probe) in probes.iter().enumerate() {
        by_key.entry(probe.key).or_default().push(i);
    }
    let wanted: HashSet<u32> = probes.iter().map(|probe| probe.key.0).collect();
    let mut present = HashSet::new();
    let mut last = None;
    for row in ptp_records {
        if last != Some(row.col1_b36) {
            last = Some(row.col1_b36);
            if wanted.contains(&row.col1_b36) {
                present.insert(row.col1_b36);
            }
        }
        let key = (row.col1_b36, row.col2_b36, row.setting_flags);
        for &i in by_key.get(&key).into_iter().flatten() {
            let probe = &mut probes[i];
            if row.eff_days <= probe.day && probe.day < row.del_days {
                probe.active_ccmi = Some(row.ccmi);
            }
        }
    }

    let mut limits = HashMap::<u32, Vec<&MueRecord>>::new();
    for row in mue_records {
        limits.entry(row.code_b36).or_default().push(row);
    }
    let mut results: Vec<Expectation<'a>> = claims
        .iter()
        .map(|claim| {
            let findings = mue_findings(&limits, claim.as_ref());
            (Vec::new(), findings, ProbeMix::default())
        })
        .collect();
    for probe in &probes {
        let (pairs, _, mix) = &mut results[probe.claim];
        mix.probes += 1;
        if let Some(ccmi) = probe.active_ccmi {
            mix.hits += 1;
            let (column_one_line_id, column_two_line_id) = probe.lines;
            pairs.push(PtpFinding {
                service_day: claims[probe.claim].as_ref()[0].service_day,
                column_one_line_id,
                column_two_line_id,
                outcome: match ccmi {
                    0 => PtpOutcome::ModifierProhibited,
                    1 => PtpOutcome::SupportedModifierRequired,
                    _ => panic!("invalid active PTP modifier indicator"),
                },
                modifier_issues: Vec::new(),
                abn_cannot_shift_liability: probe.abn_liability_shift_attempted,
            });
        } else if present.contains(&probe.key.0) {
            mix.interior_misses += 1;
        } else {
            mix.bucket_misses += 1;
        }
    }
    for (pairs, _, _) in &mut results {
        pairs.sort_unstable_by_key(|f| (f.column_two_line_id, f.column_one_line_id));
    }
    results
}

fn mue_findings<'a>(
    limits: &HashMap<u32, Vec<&MueRecord>>,
    lines: &[ClaimLine<'a>],
) -> Vec<MueFinding<'a>> {
    let mut by_code = BTreeMap::<u32, Vec<&ClaimLine<'a>>>::new();
    for line in lines {
        by_code.entry(line.code.0).or_default().push(line);
    }
    let mut findings = Vec::new();
    for (code, group) in by_code {
        let day = group[0].service_day;
        let Some(edit) = limits.get(&code).and_then(|rows| {
            rows.iter().find(|row| {
                row.setting_flags == day.setting as u8
                    && row.eff_days <= day.date.0
                    && day.date.0 < row.del_days
            })
        }) else {
            continue;
        };
        let mut check_limit = |scope, total_units, abn_cannot_shift_liability| {
            if total_units > u64::from(edit.max_units) {
                findings.push(MueFinding {
                    service_day: day,
                    code: ProcedureCode(code),
                    scope,
                    max_units: edit.max_units,
                    total_units,
                    abn_cannot_shift_liability,
                });
            }
        };
        match edit.mai {
            1 => {
                assert_eq!(group.len(), 1, "oracle claims use distinct codes");
                check_limit(
                    MueScope::ClaimLine {
                        line_id: group[0].id,
                    },
                    group[0].units.get(),
                    group[0].abn_liability_shift_attempted,
                );
            }
            2 | 3 => check_limit(
                if edit.mai == 2 {
                    MueScope::AbsoluteServiceDayLimit
                } else {
                    MueScope::ServiceDayClinicalReview
                },
                group.iter().map(|line| line.units.get()).sum(),
                group.iter().any(|line| line.abn_liability_shift_attempted),
            ),
            _ => panic!("invalid MUE adjudication indicator"),
        }
    }
    findings
}

/// Compares `ncci::evaluate` and `mue::evaluate` with the oracle for every claim and returns
/// the combined probe mix. The oracle reads `ptp_records` and `mue_records`, not the indexes.
pub fn check_claims(
    ptp: &MmapPtpIndex,
    mue: &MmapMueIndex,
    ptp_records: &[NcciPtpRecord],
    mue_records: &[MueRecord],
    claims: &[Vec<ClaimLine<'_>>],
) -> Result<ProbeMix, String> {
    let mut total = ProbeMix::default();
    let oracle = expected_all(ptp_records, mue_records, claims);
    for (n, (claim, (pairs, limits, mix))) in claims.iter().zip(oracle).enumerate() {
        let findings = ncci::evaluate(ptp, claim).map_err(|e| format!("claim {n}: {e}"))?;
        if findings != pairs {
            return Err(format!("claim {n}: PTP findings differ from the oracle"));
        }
        let found = mue::evaluate(mue, claim).map_err(|e| format!("claim {n}: {e}"))?;
        if found != limits {
            return Err(format!("claim {n}: MUE findings differ from the oracle"));
        }
        total.probes += mix.probes;
        total.hits += mix.hits;
        total.interior_misses += mix.interior_misses;
        total.bucket_misses += mix.bucket_misses;
    }
    Ok(total)
}
