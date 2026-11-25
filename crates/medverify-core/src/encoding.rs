use time::{Date, Duration};

pub const MAX_PROCEDURE_CODE: u32 = 36_u32.pow(5) - 1;
pub const EPOCH: Date = match Date::from_ordinal_date(1990, 1) {
    Ok(date) => date,
    Err(_) => panic!("invalid index epoch"),
};
pub const NO_DELETION_DATE: u16 = u16::MAX;

pub fn encode_code_b36(code: &str) -> Option<u32> {
    let bytes: &[u8; 5] = code.as_bytes().try_into().ok()?;
    encode_b36_bytes(bytes)
}

fn encode_b36_bytes(bytes: &[u8; 5]) -> Option<u32> {
    let [a, b, c, d, e] = *bytes;
    Some(
        (((b36_digit(a)? * 36 + b36_digit(b)?) * 36 + b36_digit(c)?) * 36 + b36_digit(d)?) * 36
            + b36_digit(e)?,
    )
}

fn b36_digit(byte: u8) -> Option<u32> {
    Some(u32::from(match byte.to_ascii_uppercase() {
        upper @ b'A'..=b'Z' => upper - b'A' + 10,
        digit @ b'0'..=b'9' => digit - b'0',
        _ => return None,
    }))
}

pub fn decode_code_b36(mut value: u32) -> Option<[u8; 5]> {
    if value > MAX_PROCEDURE_CODE {
        return None;
    }
    let mut code = [0; 5];
    let mut i = code.len();
    while i > 0 {
        i -= 1;
        let quotient = value / 36;
        code[i] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"[(value - quotient * 36) as usize];
        value = quotient;
    }
    Some(code)
}

pub fn encode_epoch_day(date: Date) -> Option<u16> {
    let days = u16::try_from((date - EPOCH).whole_days()).ok()?;
    (days != NO_DELETION_DATE).then_some(days)
}

pub fn decode_epoch_day(days: u16) -> Option<Date> {
    if days == NO_DELETION_DATE {
        return None;
    }
    EPOCH.checked_add(Duration::days(i64::from(days)))
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct NcciPtpRecord {
    pub col1_b36: u32,
    pub col2_b36: u32,
    pub eff_days: u16,
    pub del_days: u16,
    pub ccmi: u8,
    pub setting_flags: u8,
    pub rationale_id: u8,
    pub flags: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MueRecord {
    pub code_b36: u32,
    pub max_units: u16,
    pub eff_days: u16,
    pub del_days: u16,
    pub mai: u8,
    pub setting_flags: u8,
    pub rationale_id: u8,
    pub flags: u8,
    pub reserved: u16,
}

/// One column-one bucket of a version 2 PTP index: `row_count` records from `first_row`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PtpDirectoryEntry {
    pub col1_b36: u32,
    pub first_row: u32,
    pub row_count: u32,
}

const _: () = {
    assert!(core::mem::size_of::<PtpDirectoryEntry>() == 12);
    assert!(core::mem::align_of::<PtpDirectoryEntry>() == 4);
    assert!(core::mem::size_of::<NcciPtpRecord>() == 16);
    assert!(core::mem::align_of::<NcciPtpRecord>() == 4);
    assert!(core::mem::size_of::<MueRecord>() == 16);
    assert!(core::mem::align_of::<MueRecord>() == 4);
};

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(8)]
fn encoding_b36_order_roundtrip() {
    let value: u32 = kani::any();
    kani::assume(value <= MAX_PROCEDURE_CODE);
    let code = decode_code_b36(value).unwrap();
    kani::assert(
        encode_b36_bytes(&code) == Some(value),
        "base-36 integer round trip",
    );

    let left: [u8; 5] = kani::any();
    let right: [u8; 5] = kani::any();
    let canonical = |b| matches!(b, b'0'..=b'9' | b'A'..=b'Z');
    let mut i = 0;
    while i < 5 {
        kani::assume(canonical(left[i]) && canonical(right[i]));
        i += 1;
    }
    // Big-endian numeric order is unsigned lexicographic order for five equal-width bytes.
    let ascii_order = |[a, b, c, d, e]: [u8; 5]| u64::from_be_bytes([0, 0, 0, a, b, c, d, e]);
    kani::assert(
        ascii_order(left).cmp(&ascii_order(right))
            == encode_b36_bytes(&left)
                .unwrap()
                .cmp(&encode_b36_bytes(&right).unwrap()),
        "base-36 encoding preserves ASCII code order",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::{bytes_of, Zeroable};
    use proptest::prelude::*;

    #[test]
    fn procedure_code_boundaries() {
        assert_eq!(encode_code_b36("00000"), Some(0));
        assert_eq!(encode_code_b36("ZZZZZ"), Some(MAX_PROCEDURE_CODE));
        assert_eq!(decode_code_b36(MAX_PROCEDURE_CODE), Some(*b"ZZZZZ"));
        assert_eq!(decode_code_b36(MAX_PROCEDURE_CODE + 1), None);
        for code in ["0001A", "0591T", "G0471", "U0004", "Y9001"] {
            let value = encode_code_b36(code).unwrap();
            assert_eq!(decode_code_b36(value).unwrap(), code.as_bytes());
            assert_eq!(encode_code_b36(&code.to_ascii_lowercase()), Some(value));
        }
        for invalid in ["", "0001", "000001", "00-01", " a123", "é123"] {
            assert_eq!(encode_code_b36(invalid), None);
        }
    }

    #[test]
    fn date_boundaries() {
        let date =
            |y, m, d| Date::from_calendar_date(y, time::Month::try_from(m).unwrap(), d).unwrap();
        assert_eq!(encode_epoch_day(date(1989, 12, 31)), None);
        assert_eq!(decode_epoch_day(0), Some(EPOCH));
        assert_eq!(encode_epoch_day(date(2026, 10, 1)), Some(13_422));
        let leap_day = date(2000, 2, 29);
        assert_eq!(
            decode_epoch_day(encode_epoch_day(leap_day).unwrap()),
            Some(leap_day)
        );
        assert_eq!(decode_epoch_day(65_534), Some(date(2169, 6, 5)));
        assert_eq!(encode_epoch_day(date(2169, 6, 6)), None);
        assert_eq!(decode_epoch_day(NO_DELETION_DATE), None);
    }

    #[test]
    fn ptp_record_layout() {
        let row = NcciPtpRecord {
            col1_b36: 0x0403_0201,
            col2_b36: 0x0807_0605,
            eff_days: 0x0a09,
            del_days: 0x0c0b,
            ccmi: 0x0d,
            setting_flags: 0x0e,
            rationale_id: 0x0f,
            flags: 0x10,
        };
        let expected = [
            &row.col1_b36.to_ne_bytes()[..],
            &row.col2_b36.to_ne_bytes(),
            &row.eff_days.to_ne_bytes(),
            &row.del_days.to_ne_bytes(),
            &[0x0d, 0x0e, 0x0f, 0x10],
        ]
        .concat();
        assert_eq!(bytes_of(&row), expected);
        assert_eq!(bytes_of(&NcciPtpRecord::zeroed()), &[0; 16]);
    }

    #[test]
    fn ptp_directory_entry_layout() {
        let entry = PtpDirectoryEntry {
            col1_b36: 0x0403_0201,
            first_row: 0x0807_0605,
            row_count: 0x0c0b_0a09,
        };
        let expected = [
            &entry.col1_b36.to_ne_bytes()[..],
            &entry.first_row.to_ne_bytes(),
            &entry.row_count.to_ne_bytes(),
        ]
        .concat();
        assert_eq!(bytes_of(&entry), expected);
        assert_eq!(bytes_of(&PtpDirectoryEntry::zeroed()), &[0; 12]);
    }

    #[test]
    fn mue_record_layout() {
        let row = MueRecord {
            code_b36: 0x0403_0201,
            max_units: 0x0605,
            eff_days: 0x0807,
            del_days: 0x0a09,
            mai: 0x0b,
            setting_flags: 0x0c,
            rationale_id: 0x0d,
            flags: 0x0e,
            reserved: 0,
        };
        let expected = [
            &row.code_b36.to_ne_bytes()[..],
            &row.max_units.to_ne_bytes(),
            &row.eff_days.to_ne_bytes(),
            &row.del_days.to_ne_bytes(),
            &[0x0b, 0x0c, 0x0d, 0x0e, 0, 0],
        ]
        .concat();
        assert_eq!(bytes_of(&row), expected);
        assert_eq!(bytes_of(&MueRecord::zeroed()), &[0; 16]);
    }

    proptest! {
        #[test]
        fn base36_integer_round_trip(value in 0..=MAX_PROCEDURE_CODE) {
            let code = decode_code_b36(value).unwrap();
            prop_assert_eq!(encode_code_b36(core::str::from_utf8(&code).unwrap()), Some(value));
        }

        #[test]
        fn base36_preserves_ascii_order(left in "[0-9A-Z]{5}", right in "[0-9A-Z]{5}") {
            let a = encode_code_b36(&left).unwrap();
            let b = encode_code_b36(&right).unwrap();
            prop_assert_eq!(a.cmp(&b), left.cmp(&right));
            prop_assert_eq!(decode_code_b36(a).unwrap(), left.as_bytes());
        }

        #[test]
        fn epoch_day_round_trip(days in 0..NO_DELETION_DATE) {
            prop_assert_eq!(encode_epoch_day(decode_epoch_day(days).unwrap()), Some(days));
        }
    }
}
