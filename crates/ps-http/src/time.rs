//! HTTP date handling (`time_util.{h,cc}` date subset).
//!
//! Formatting always emits IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`).
//! Parsing accepts the three standard HTTP date formats (IMF-fixdate,
//! RFC 850, asctime); the C++ implementation additionally accepts the
//! broader NSPR range, which is a documented narrowing (real origins
//! emit IMF-fixdate).
#![forbid(unsafe_code)]

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Days from civil date (Howard Hinnant's algorithm), days since 1970-01-01.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// Civil date from days since 1970-01-01 (inverse of above).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146096
    } / 146097;
    let day_of_era = shifted - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = (if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Breaks ms-since-epoch into UTC (year, month, day, hour, minute,
/// second) for fixed-format stamps (`gmtime_r` use sites).
#[must_use]
pub fn utc_ymd_hms(time_ms: i64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = time_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let clock = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        (clock / 3600) as u32,
        (clock % 3600 / 60) as u32,
        (clock % 60) as u32,
    )
}

/// Formats ms-since-epoch as IMF-fixdate (`ConvertTimeToString`).
#[must_use]
pub fn format_http_date(time_ms: i64) -> String {
    let secs = time_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let clock = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 was a Thursday (index 4 with Sunday = 0).
    let weekday = WEEKDAYS[((days + 4).rem_euclid(7)) as usize];
    format!(
        "{weekday}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        day,
        MONTHS[(month - 1) as usize],
        year,
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    )
}

/// Parses an HTTP date (`ConvertStringToTime`, 3-format subset).
///
/// Accepts IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), RFC 850
/// (`Sunday, 06-Nov-94 08:49:37 GMT`, 2-digit years pivot like C's
/// `getdate` interpretation used by NSPR: 00-68 → 2000s, 69-99 → 1900s),
/// and asctime (`Sun Nov  6 08:49:37 1994`). Returns ms-since-epoch.
#[must_use]
pub fn parse_http_date(text: &str) -> Option<i64> {
    parse_imf_fixdate(text)
        .or_else(|| parse_rfc_850(text))
        .or_else(|| parse_asctime(text))
}

fn month_number(name: &str) -> Option<u32> {
    MONTHS
        .iter()
        .position(|month| month.eq_ignore_ascii_case(name))
        .map(|index| index as u32 + 1)
}

fn valid_time(hour: u32, min: u32, sec: u32) -> bool {
    hour < 24 && min < 60 && sec < 61
}

fn to_ms(year: i64, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> Option<i64> {
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || !valid_time(hour, min, sec)
    {
        return None;
    }
    Some(
        (days_from_civil(year, month as i64, day as i64) * 86_400
            + i64::from(hour) * 3600
            + i64::from(min) * 60
            + i64::from(sec))
            * 1000,
    )
}

/// `Sun, 06 Nov 1994 08:49:37 GMT`.
fn parse_imf_fixdate(text: &str) -> Option<i64> {
    // Weekday verified loosely (3 letters + ", "), like most parsers.
    let (weekday, rest) = text.split_once(", ")?;
    if weekday.len() != 3 || !weekday.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mut parts = rest.splitn(4, ' ');
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    let time = time.strip_suffix(" GMT")?;
    let mut clock = time.splitn(3, ':');
    let hour: u32 = clock.next()?.parse().ok()?;
    let min: u32 = clock.next()?.parse().ok()?;
    let sec: u32 = clock.next()?.parse().ok()?;
    to_ms(year, month, day, hour, min, sec)
}

/// `Sunday, 06-Nov-94 08:49:37 GMT`.
fn parse_rfc_850(text: &str) -> Option<i64> {
    let (_weekday, rest) = text.split_once(", ")?;
    let (date, time) = rest.split_once(' ')?;
    let mut date_parts = date.splitn(3, '-');
    let day: u32 = date_parts.next()?.parse().ok()?;
    let month = month_number(date_parts.next()?)?;
    let year2: i64 = date_parts.next()?.parse().ok()?;
    if !(0..100).contains(&year2) {
        return None;
    }
    // NSPR-style pivot: 69-99 → 1900s, 00-68 → 2000s.
    let year = if year2 >= 69 {
        1900 + year2
    } else {
        2000 + year2
    };
    let time = time.strip_suffix(" GMT")?;
    let mut clock = time.splitn(3, ':');
    let hour: u32 = clock.next()?.parse().ok()?;
    let min: u32 = clock.next()?.parse().ok()?;
    let sec: u32 = clock.next()?.parse().ok()?;
    to_ms(year, month, day, hour, min, sec)
}

/// `Sun Nov  6 08:49:37 1994` (day space-padded to width 2).
fn parse_asctime(text: &str) -> Option<i64> {
    if text.len() < 24 {
        return None;
    }
    // Fixed columns: DDD MMM _D HH:MM:SS YYYY.
    if !text.as_bytes()[0..3]
        .iter()
        .all(|b| b.is_ascii_alphabetic())
    {
        return None;
    }
    let month = month_number(&text[4..7])?;
    let day: u32 = text[8..10].trim().parse().ok()?;
    let hour: u32 = text[11..13].parse().ok()?;
    if text.as_bytes()[13] != b':' || text.as_bytes()[16] != b':' {
        return None;
    }
    let min: u32 = text[14..16].parse().ok()?;
    let sec: u32 = text[17..19].parse().ok()?;
    let year: i64 = text[20..24].parse().ok()?;
    if text.len() != 24 {
        return None;
    }
    to_ms(year, month, day, hour, min, sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_dates() {
        // RFC 7231 example timestamp.
        assert_eq!(
            format_http_date(784_111_777_000),
            "Sun, 06 Nov 1994 08:49:37 GMT"
        );
        assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777_000)
        );
        assert_eq!(
            parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"),
            Some(784_111_777_000)
        );
        assert_eq!(
            parse_http_date("Sun Nov  6 08:49:37 1994"),
            Some(784_111_777_000)
        );
    }

    #[test]
    fn pivot_and_round_trip() {
        // 69 -> 1969, 68 -> 2068.
        let ms69 = parse_http_date("Sunday, 06-Nov-69 08:49:37 GMT").unwrap();
        let ms68 = parse_http_date("Sunday, 06-Nov-68 08:49:37 GMT").unwrap();
        assert!(ms69 < 0);
        assert_eq!(format_http_date(ms69), "Thu, 06 Nov 1969 08:49:37 GMT");
        assert_eq!(format_http_date(ms68), "Tue, 06 Nov 2068 08:49:37 GMT");
        // Format/parse round-trips across a range of timestamps.
        for days in [0, 1, 365, 10000, 20000, 50000] {
            for secs in [0, 1, 3600, 86399] {
                let ms = (days * 86_400 + secs) * 1000;
                assert_eq!(parse_http_date(&format_http_date(ms)), Some(ms));
            }
        }
        // Rejects.
        assert_eq!(parse_http_date(""), None);
        assert_eq!(parse_http_date("0"), None);
        assert_eq!(parse_http_date("Sun, 32 Nov 1994 08:49:37 GMT"), None);
        assert_eq!(parse_http_date("Sun, 06 Foo 1994 08:49:37 GMT"), None);
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 25:49:37 GMT"), None);
        // Leap day accepted only on leap years.
        assert!(parse_http_date("Tue, 29 Feb 2000 00:00:00 GMT").is_some());
        assert_eq!(parse_http_date("Mon, 29 Feb 1999 00:00:00 GMT"), None);
    }

    #[test]
    fn utc_breakdown_matches_http_date() {
        // 1994-11-06 08:49:37 UTC (the IMF-fixdate golden above).
        let ms = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(utc_ymd_hms(ms), (1994, 11, 6, 8, 49, 37));
        assert_eq!(utc_ymd_hms(0), (1970, 1, 1, 0, 0, 0));
    }
}
