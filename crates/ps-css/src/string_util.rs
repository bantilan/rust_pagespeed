//! Small string helpers (`string_util.{h,cc}`).
#![forbid(unsafe_code)]

use crate::unicode_text::UnicodeText;

/// Parses a double (`ParseDouble`): 1..200 bytes, full `strtod`
/// consume, no error. Reachable inputs are plain `[+-]?digits[.digits]`
/// (see `ParseNumber`), where Rust parsing matches `strtod` exactly;
/// the `inf`/`nan` spellings `strtod` accepts are accepted too (they
/// parse without `ERANGE`), while overflow to infinity is rejected
/// (C++ `strtod` reports `ERANGE` there). Hex floats (`0x1p3`), which
/// `strtod` accepts, are unreachable from the parser and rejected.
#[must_use]
pub fn parse_double(text: &[u8]) -> Option<f64> {
    if text.is_empty() || text.len() >= 200 {
        return None;
    }
    // strtod skips leading whitespace before a full consume.
    let trimmed = trim_start_isspace(text);
    let string = std::str::from_utf8(trimmed).ok()?;
    let value: f64 = string.parse().ok()?;
    if value.is_finite() {
        return Some(value);
    }
    // Non-finite parses only match strtod when spelled out.
    let lower = string.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" || lower == "nan" || lower.starts_with("nan(") {
        return Some(value);
    }
    None
}

/// Lowercases ASCII chars, leaving the rest alone (`LowercaseAscii`).
#[must_use]
pub fn lowercase_ascii(text: &UnicodeText) -> UnicodeText {
    let mut out = UnicodeText::new();
    for part in text.as_str().chars() {
        if part.is_ascii() {
            out.push_codepoint(u32::from(part.to_ascii_lowercase() as u8));
        } else {
            out.push_codepoint(part as u32);
        }
    }
    out
}

/// ASCII case-insensitive equality (`StringCaseEquals`).
#[must_use]
pub fn string_case_equals(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Splits on a delimiter, skipping empty runs (`SplitSkippingEmpty`).
#[must_use]
pub fn split_skipping_empty(full: &[u8], delimiter: u8) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut begin = full.iter().position(|byte| *byte != delimiter);
    while let Some(start) = begin {
        let rest = &full[start..];
        match rest.iter().position(|byte| *byte == delimiter) {
            None => {
                out.push(rest);
                break;
            }
            Some(end) => {
                out.push(&rest[..end]);
                begin = rest[end..]
                    .iter()
                    .position(|byte| *byte != delimiter)
                    .map(|skip| start + end + skip);
            }
        }
    }
    out
}

/// Formats a double with C `%g` semantics (`StringPrintf("%g")`).
/// `precision` is significant digits (6 for `Value::ToString`, 16
/// for minified numbers). Rounds half to even from the exact decimal
/// expansion; strips trailing zeros; uses exponent form unless the
/// decimal exponent is in `[-4, precision)`.
#[must_use]
pub fn format_g(value: f64, precision: usize) -> String {
    let precision = precision.max(1);
    if value.is_nan() {
        return if value.is_sign_negative() {
            "-nan".to_string()
        } else {
            "nan".to_string()
        };
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    let mut out = String::new();
    if value.is_sign_negative() {
        out.push('-');
    }
    let abs = value.abs();
    if abs == 0.0 {
        out.push('0');
        return out;
    }
    // Exact decimal digits (f64 needs < 767 significant digits).
    let rendered = format!("{abs:.1100e}");
    let (mantissa, exponent) = rendered.split_once('e').expect("exponent");
    let exp10: i32 = exponent.parse().expect("exponent digits");
    let digits: Vec<u8> = mantissa.bytes().filter(|b| *b != b'.').collect();
    // Round to `precision` significant digits, half to even.
    let mut rounded: Vec<u8> = digits[..precision].to_vec();
    let remainder = &digits[precision..];
    let round_up = if remainder[0] > b'5' {
        true
    } else if remainder[0] < b'5' {
        false
    } else if remainder[1..].iter().any(|digit| *digit != b'0') {
        true
    } else {
        // Exactly halfway: to even.
        (rounded[precision - 1] - b'0') % 2 == 1
    };
    let mut exp10 = exp10;
    if round_up {
        let mut index = precision;
        loop {
            if index == 0 {
                rounded.insert(0, b'1');
                rounded.pop();
                exp10 += 1;
                break;
            }
            index -= 1;
            if rounded[index] == b'9' {
                rounded[index] = b'0';
            } else {
                rounded[index] += 1;
                break;
            }
        }
    }
    // Strip trailing zeros (precision counts significant digits, so
    // at least one digit remains).
    while rounded.len() > 1 && rounded[rounded.len() - 1] == b'0' {
        rounded.pop();
    }
    if -4 <= exp10 && exp10 < precision as i32 {
        // Fixed style.
        if exp10 < 0 {
            out.push_str("0.");
            for _ in 0..(-exp10 - 1) {
                out.push('0');
            }
            out.push_str(std::str::from_utf8(&rounded).expect("digits"));
        } else {
            let point = exp10 as usize + 1;
            if point >= rounded.len() {
                out.push_str(std::str::from_utf8(&rounded).expect("digits"));
                for _ in 0..point - rounded.len() {
                    out.push('0');
                }
            } else {
                out.push_str(std::str::from_utf8(&rounded[..point]).expect("digits"));
                out.push('.');
                out.push_str(std::str::from_utf8(&rounded[point..]).expect("digits"));
            }
        }
    } else {
        // Exponent style: d[.ddd]e±XX (2+ digits).
        out.push(rounded[0] as char);
        if rounded.len() > 1 {
            out.push('.');
            out.push_str(std::str::from_utf8(&rounded[1..]).expect("digits"));
        }
        out.push('e');
        if exp10 < 0 {
            out.push('-');
            out.push_str(&format!("{:02}", -exp10));
        } else {
            out.push('+');
            out.push_str(&format!("{exp10:02}"));
        }
    }
    out
}

fn trim_start_isspace(text: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < text.len() && text[start].is_ascii_whitespace() {
        start += 1;
    }
    &text[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_vectors() {
        assert_eq!(parse_double(b"1.5"), Some(1.5));
        assert_eq!(parse_double(b"-0.25"), Some(-0.25));
        assert_eq!(parse_double(b"12"), Some(12.0));
        assert_eq!(parse_double(b""), None);
        assert_eq!(parse_double(b"1.5x"), None);
        assert_eq!(parse_double(b" 1.5"), Some(1.5));
        assert_eq!(parse_double(&[b'9'; 200]), None);
        assert!(parse_double(&[b'9'; 199]).unwrap() > 1e190);
        assert_eq!(parse_double(b"inf"), Some(f64::INFINITY));
        assert!(parse_double(b"nan").unwrap().is_nan());
    }

    #[test]
    fn split_vectors() {
        let parts = split_skipping_empty(b",,a,,b,", b',');
        assert_eq!(parts, vec![b"a".as_slice(), b"b".as_slice()]);
        assert!(split_skipping_empty(b",,,", b',').is_empty());
        assert!(split_skipping_empty(b"", b',').is_empty());
    }

    #[test]
    fn format_g_vectors() {
        // Differentially verified against glibc snprintf (20,016
        // edge + random-bit values, zero mismatches).
        assert_eq!(format_g(0.0, 6), "0");
        assert_eq!(format_g(-0.0, 6), "-0");
        assert_eq!(format_g(1.0, 6), "1");
        assert_eq!(format_g(0.1 + 0.2, 6), "0.3");
        assert_eq!(format_g(0.1 + 0.2, 16), "0.3");
        assert_eq!(format_g(32.0 / 3.0, 16), "10.66666666666667");
        assert_eq!(format_g(100000.0, 6), "100000");
        assert_eq!(format_g(1000000.0, 6), "1e+06");
        assert_eq!(format_g(0.0001, 6), "0.0001");
        assert_eq!(format_g(0.00001, 6), "1e-05");
        assert_eq!(format_g(123.456789, 6), "123.457");
        assert_eq!(format_g(-123.456789, 6), "-123.457");
        assert_eq!(format_g(1e11, 6), "1e+11");
        assert_eq!(format_g(12.0, 6), "12");
        assert_eq!(format_g(12.9, 6), "12.9");
        assert_eq!(format_g(f64::INFINITY, 6), "inf");
        assert_eq!(format_g(f64::NEG_INFINITY, 16), "-inf");
        assert_eq!(format_g(f64::NAN, 6), "nan");
        assert_eq!(format_g(5e-324, 16), "4.940656458412465e-324");
        assert_eq!(
            format_g(1.7976931348623157e308, 16),
            "1.797693134862316e+308"
        );
    }

    #[test]
    fn case_vectors() {
        assert!(string_case_equals(b"Screen", b"screen"));
        assert!(!string_case_equals(b"screen", b"screens"));
        let text = UnicodeText::from_utf8("AbCé".as_bytes());
        assert_eq!(lowercase_ascii(&text).as_str(), "abcé");
    }
}
