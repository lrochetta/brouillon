//! Data inspector: the value of the bytes at the cursor read as every common scalar type.
//!
//! Pure and allocation-light: `inspect` takes the (at most 16) bytes at the cursor and returns one
//! row per interpretation. A row whose width exceeds the bytes available has no value (`None`, the
//! panel shows it muted). Dates are UTC, ISO 8601, computed by hand (no time crate).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Endian {
    #[default]
    Little,
    Big,
}

impl Endian {
    pub fn toggled(self) -> Self {
        match self {
            Endian::Little => Endian::Big,
            Endian::Big => Endian::Little,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Endian::Little => "LE",
            Endian::Big => "BE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectId {
    Binary,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    Bool,
    Ascii,
    Utf8,
    Utf16,
    UnixSecs,
    UnixMillis,
    FileTime,
    ColorRgb,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectRow {
    pub id: InspectId,
    pub label: &'static str,
    /// `None`: not enough bytes for this width, or not a valid value.
    pub value: Option<String>,
}

/// How many bytes `inspect` looks at.
pub const WINDOW: usize = 16;

fn take<const N: usize>(b: &[u8], e: Endian) -> Option<[u8; N]> {
    let mut a: [u8; N] = b.get(..N)?.try_into().ok()?;
    if e == Endian::Big {
        a.reverse(); // normalise to little-endian order for from_le_bytes
    }
    Some(a)
}

fn fmt_float(v: f64, single: bool) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "+Inf".into() } else { "-Inf".into() }
    } else if v != 0.0 && (v.abs() >= 1e16 || v.abs() < 1e-5) {
        if single { format!("{:e}", v as f32) } else { format!("{v:e}") }
    } else if single {
        format!("{}", v as f32)
    } else {
        format!("{v}")
    }
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`; years outside 0..=9999 give `None`.
pub fn iso_utc(secs: i64) -> Option<String> {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    if !(0..=9999).contains(&y) {
        return None;
    }
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60))
}

fn ascii_repr(b: u8) -> String {
    match b {
        0x20..=0x7E => format!("'{}'", b as char),
        0x00 => "NUL".into(),
        0x09 => "TAB".into(),
        0x0A => "LF".into(),
        0x0D => "CR".into(),
        0x7F => "DEL".into(),
        _ => format!("\\x{b:02X}"),
    }
}

fn char_repr(c: char) -> String {
    if c.is_control() { format!("U+{:04X}", c as u32) } else { format!("'{c}' U+{:04X}", c as u32) }
}

fn utf8_char(b: &[u8]) -> Option<String> {
    let first = *b.first()?;
    let n = match first {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };
    let s = std::str::from_utf8(b.get(..n)?).ok()?;
    Some(char_repr(s.chars().next()?))
}

fn utf16_char(b: &[u8], e: Endian) -> Option<String> {
    let unit = |i: usize| -> Option<u16> {
        let p: [u8; 2] = b.get(i..i + 2)?.try_into().ok()?;
        Some(if e == Endian::Little { u16::from_le_bytes(p) } else { u16::from_be_bytes(p) })
    };
    let hi = unit(0)?;
    let c = if (0xD800..0xDC00).contains(&hi) {
        let lo = unit(2)?;
        char::decode_utf16([hi, lo]).next()?.ok()?
    } else {
        char::from_u32(hi as u32)?
    };
    Some(char_repr(c))
}

/// Every interpretation of the bytes at the cursor. `bytes` is whatever is available (up to
/// [`WINDOW`]; extra bytes are ignored).
pub fn inspect(bytes: &[u8], e: Endian) -> Vec<InspectRow> {
    use InspectId::*;
    let b = &bytes[..bytes.len().min(WINDOW)];
    let row = |id, label, value: Option<String>| InspectRow { id, label, value };
    let u16v = take::<2>(b, e).map(u16::from_le_bytes);
    let u32v = take::<4>(b, e).map(u32::from_le_bytes);
    let u64v = take::<8>(b, e).map(u64::from_le_bytes);
    let unix_s = u32v.map(|v| v as i32 as i64).and_then(iso_utc);
    let unix_ms = u64v
        .map(|v| v as i64)
        .and_then(|ms| iso_utc(ms.div_euclid(1000)).map(|s| format!("{}.{:03}Z", &s[..s.len() - 1], ms.rem_euclid(1000))));
    let filetime = u64v.and_then(|t| iso_utc((t / 10_000_000) as i64 - 11_644_473_600));
    vec![
        row(Binary, "Binary", b.first().map(|v| format!("{v:08b}"))),
        row(U8, "u8", b.first().map(|v| v.to_string())),
        row(I8, "i8", b.first().map(|v| (*v as i8).to_string())),
        row(U16, "u16", u16v.map(|v| v.to_string())),
        row(I16, "i16", u16v.map(|v| (v as i16).to_string())),
        row(U32, "u32", u32v.map(|v| v.to_string())),
        row(I32, "i32", u32v.map(|v| (v as i32).to_string())),
        row(U64, "u64", u64v.map(|v| v.to_string())),
        row(I64, "i64", u64v.map(|v| (v as i64).to_string())),
        row(F32, "f32", u32v.map(|v| fmt_float(f32::from_bits(v) as f64, true))),
        row(F64, "f64", u64v.map(|v| fmt_float(f64::from_bits(v), false))),
        row(Bool, "bool", b.first().map(|v| if *v == 0 { "false" } else { "true" }.to_string())),
        row(Ascii, "ASCII", b.first().map(|v| ascii_repr(*v))),
        row(Utf8, "UTF-8", utf8_char(b)),
        row(Utf16, "UTF-16", utf16_char(b, e)),
        row(UnixSecs, "Unix s (i32)", unix_s),
        row(UnixMillis, "Unix ms (i64)", unix_ms),
        row(FileTime, "FILETIME", filetime),
        row(ColorRgb, "Colour RGB", b.get(..3).map(|c| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]))),
    ]
}

/// The value of one row, for tests and the status bar.
pub fn value_of(rows: &[InspectRow], id: InspectId) -> Option<&str> {
    rows.iter().find(|r| r.id == id).and_then(|r| r.value.as_deref())
}

/// The (unsigned, signed) rows whose width equals a selection of 1, 2, 4 or 8 bytes (emphasised).
pub fn rows_for_selection_len(len: u64) -> Option<(InspectId, InspectId)> {
    use InspectId::*;
    match len {
        1 => Some((U8, I8)),
        2 => Some((U16, I16)),
        4 => Some((U32, I32)),
        8 => Some((U64, I64)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use InspectId::*;

    #[test]
    fn integers_both_endians() {
        let b = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let le = inspect(&b, Endian::Little);
        assert_eq!(value_of(&le, U8), Some("1"));
        assert_eq!(value_of(&le, U16), Some("513"));
        assert_eq!(value_of(&le, U32), Some("67305985"));
        assert_eq!(value_of(&le, U64), Some("578437695752307201"));
        let be = inspect(&b, Endian::Big);
        assert_eq!(value_of(&be, U8), Some("1"));
        assert_eq!(value_of(&be, U16), Some("258"));
        assert_eq!(value_of(&be, U32), Some("16909060"));
        assert_eq!(value_of(&be, U64), Some("72623859790382856"));
    }

    #[test]
    fn signed_values() {
        let b = [0xFF; 8];
        let r = inspect(&b, Endian::Little);
        for id in [I8, I16, I32, I64] {
            assert_eq!(value_of(&r, id), Some("-1"));
        }
        assert_eq!(value_of(&r, U8), Some("255"));
        assert_eq!(value_of(&r, U16), Some("65535"));
        assert_eq!(value_of(&r, Binary), Some("11111111"));
    }

    #[test]
    fn short_input_gives_none_for_wide_rows() {
        let r = inspect(&[0x41, 0x42, 0x43], Endian::Little);
        assert_eq!(value_of(&r, U16), Some("16961"));
        assert_eq!(value_of(&r, U32), None);
        assert_eq!(value_of(&r, U64), None);
        assert_eq!(value_of(&r, F32), None);
        assert_eq!(value_of(&r, ColorRgb), Some("#414243"));
        let empty = inspect(&[], Endian::Little);
        assert!(empty.iter().all(|r| r.value.is_none()));
        assert_eq!(empty.len(), 19);
    }

    #[test]
    fn floats_and_specials() {
        let r = inspect(&1.5f32.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, F32), Some("1.5"));
        let r = inspect(&(-2.25f64).to_be_bytes(), Endian::Big);
        assert_eq!(value_of(&r, F64), Some("-2.25"));
        let r = inspect(&f32::NAN.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, F32), Some("NaN"));
        let r = inspect(&f32::INFINITY.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, F32), Some("+Inf"));
        let r = inspect(&f64::NEG_INFINITY.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, F64), Some("-Inf"));
        let r = inspect(&1e30f32.to_le_bytes(), Endian::Little);
        assert!(value_of(&r, F32).unwrap().contains('e'));
    }

    #[test]
    fn dates() {
        // 2001-09-09T01:46:40Z = 1_000_000_000
        let r = inspect(&1_000_000_000u32.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, UnixSecs), Some("2001-09-09T01:46:40Z"));
        // negative i32 = before 1970
        let r = inspect(&(-86_400i32).to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, UnixSecs), Some("1969-12-31T00:00:00Z"));
        let r = inspect(&1_000_000_000_123i64.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, UnixMillis), Some("2001-09-09T01:46:40.123Z"));
        // FILETIME of the Unix epoch
        let ft = 116_444_736_000_000_000u64;
        let r = inspect(&ft.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, FileTime), Some("1970-01-01T00:00:00Z"));
        let r = inspect(&ft.to_be_bytes(), Endian::Big);
        assert_eq!(value_of(&r, FileTime), Some("1970-01-01T00:00:00Z"));
        // FILETIME zero = 1601-01-01
        let r = inspect(&0u64.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, FileTime), Some("1601-01-01T00:00:00Z"));
        // absurd value: no date
        let r = inspect(&u64::MAX.to_le_bytes(), Endian::Little);
        assert_eq!(value_of(&r, FileTime), None);
    }

    #[test]
    fn civil_leap_years() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(iso_utc(951_782_400).as_deref(), Some("2000-02-29T00:00:00Z")); // leap (div by 400)
        assert_eq!(iso_utc(951_868_800).as_deref(), Some("2000-03-01T00:00:00Z"));
        assert_eq!(iso_utc(4_107_542_400).as_deref(), Some("2100-03-01T00:00:00Z")); // 2100 not leap
        assert_eq!(iso_utc(1_709_164_800).as_deref(), Some("2024-02-29T00:00:00Z"));
    }

    #[test]
    fn chars_bool_color() {
        let r = inspect(b"Hello", Endian::Little);
        assert_eq!(value_of(&r, Ascii), Some("'H'"));
        assert_eq!(value_of(&r, Utf8), Some("'H' U+0048"));
        assert_eq!(value_of(&r, Bool), Some("true"));
        let r = inspect(&[0, 0, 0], Endian::Little);
        assert_eq!(value_of(&r, Bool), Some("false"));
        assert_eq!(value_of(&r, Ascii), Some("NUL"));
        let r = inspect(&[0xC3, 0xA9], Endian::Little);
        assert_eq!(value_of(&r, Utf8), Some("'\u{e9}' U+00E9"));
        assert_eq!(value_of(&inspect(&[0x80], Endian::Little), Utf8), None);
        assert_eq!(value_of(&inspect(&[0xE2, 0x82], Endian::Little), Utf8), None);
        assert_eq!(value_of(&inspect(&[0xFF], Endian::Little), Ascii), Some("\\xFF"));
    }

    #[test]
    fn utf16_units_and_pairs() {
        let r = inspect(&[0x41, 0x00], Endian::Little);
        assert_eq!(value_of(&r, Utf16), Some("'A' U+0041"));
        let r = inspect(&[0x00, 0x41], Endian::Big);
        assert_eq!(value_of(&r, Utf16), Some("'A' U+0041"));
        let r = inspect(&[0x3D, 0xD8, 0x00, 0xDE], Endian::Little);
        assert_eq!(value_of(&r, Utf16), Some("'\u{1F600}' U+1F600"));
        assert_eq!(value_of(&inspect(&[0x3D, 0xD8], Endian::Little), Utf16), None);
        assert_eq!(value_of(&inspect(&[0x3D, 0xD8, 0x41, 0x00], Endian::Little), Utf16), None);
    }

    #[test]
    fn selection_width_row() {
        assert_eq!(rows_for_selection_len(4), Some((U32, I32)));
        assert_eq!(rows_for_selection_len(3), None);
    }
}
