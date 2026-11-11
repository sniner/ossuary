//! The `Date:` header (RFC 5322, 3.3), read into RFC 3339.
//!
//! A date with a zone is written in UTC with `Z`, because only then does
//! a text comparison of two values agree with their order in time. A
//! date without a zone, or with a zone name that is not known, is written
//! as local time without a suffix: the zone is not invented.
//!
//! mail-parser's own date parser is not used. It reads an unknown zone
//! name as `+0000`, rejects a time without seconds, which RFC 5322
//! allows, and accepts the 31st of February.

/// The value of a `Date:` header as RFC 3339, or `None` when it is not a
/// date: `2026-09-04T10:34:56Z` from `Fri, 4 Sep 2026 12:34:56 +0200`,
/// `2026-09-04T12:34:56` from the same without a zone. A missing second
/// is written as `00`.
pub fn rfc3339(text: &str) -> Option<String> {
    let text = without_comments(text);
    let mut tokens = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|token| !token.is_empty())
        .peekable();
    // The day of the week is not checked against the date. Mail programs
    // also write it in other languages (`Do,`).
    if tokens
        .peek()
        .is_some_and(|token| token.chars().all(char::is_alphabetic) && month(token).is_none())
    {
        tokens.next();
    }
    let day: u32 = number(tokens.next()?, 1, 2)?;
    let month = month(tokens.next()?)?;
    let year = year(tokens.next()?)?;
    let (hour, minute, second) = time(tokens.next()?)?;
    let offset = match tokens.next().map(zone) {
        None | Some(Zone::Unknown) => None,
        Some(Zone::Offset(minutes)) => Some(minutes),
        Some(Zone::Invalid) => return None,
    };
    if day > days_in_month(year, month) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let Some(offset) = offset else {
        return Some(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
        ));
    };
    // Shifted by whole minutes, with the second kept apart: a leap second
    // (60) stays 60 and does not carry into the next minute.
    let minutes =
        days_from_civil(year, month, day) * 1440 + i64::from(hour) * 60 + i64::from(minute)
            - i64::from(offset);
    let (year, month, day) = civil_from_days(minutes.div_euclid(1440));
    let minute_of_day = minutes.rem_euclid(1440);
    if !(0..=9999).contains(&year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{second:02}Z",
        minute_of_day / 60,
        minute_of_day % 60
    ))
}

/// The text with its comments removed (RFC 5322, 3.2.2): parentheses,
/// nested, with `\` quoting the next character. An unclosed comment runs
/// to the end.
fn without_comments(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' if depth > 0 => {
                chars.next();
            }
            '(' => depth += 1,
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    kept.push(' ');
                }
            }
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    kept
}

/// A number of `min` to `max` ASCII digits.
fn number<T: std::str::FromStr>(token: &str, min: usize, max: usize) -> Option<T> {
    let digits = (min..=max).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_digit());
    digits.then(|| token.parse().ok()).flatten()
}

/// The month by its English name, abbreviated to at least three letters
/// (`Sep`, `Sept`, `September`), in any case.
fn month(token: &str) -> Option<u32> {
    const NAMES: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let token = token.to_ascii_lowercase();
    if token.len() < 3 {
        return None;
    }
    NAMES
        .iter()
        .position(|name| name.starts_with(&token))
        .and_then(|index| u32::try_from(index + 1).ok())
}

/// The year, with the two- and three-digit years of RFC 5322, 4.3: below
/// 50 in the 2000s, otherwise in the 1900s, and three digits plus 1900.
fn year(token: &str) -> Option<i64> {
    let value: i64 = number(token, 2, 4)?;
    let year = match token.len() {
        2 if value < 50 => 2000 + value,
        2 | 3 => 1900 + value,
        _ => value,
    };
    (year >= 1900).then_some(year)
}

/// `HH:MM` or `HH:MM:SS`.
fn time(token: &str) -> Option<(u32, u32, u32)> {
    let mut parts = token.split(':');
    let hour = number(parts.next()?, 1, 2)?;
    let minute = number(parts.next()?, 2, 2)?;
    let second = match parts.next() {
        Some(part) => number(part, 2, 2)?,
        None => 0,
    };
    parts.next().is_none().then_some((hour, minute, second))
}

/// What the token after the time says about the zone.
enum Zone {
    /// Minutes east of UTC.
    Offset(i32),
    /// No zone this reader knows: the time stays without a zone.
    Unknown,
    /// A numeric zone that is not one: the date is unreadable.
    Invalid,
}

fn zone(token: &str) -> Zone {
    let Some(digits) = token.strip_prefix(['+', '-']) else {
        return named_zone(token).map_or(Zone::Unknown, Zone::Offset);
    };
    let digits = digits.replacen(':', "", 1);
    let hours: Option<i32> = digits.get(..2).and_then(|part| number(part, 2, 2));
    let minutes: Option<i32> = digits.get(2..).and_then(|part| number(part, 2, 2));
    match (hours, minutes) {
        (Some(hours), Some(minutes)) if minutes <= 59 => {
            let offset = hours * 60 + minutes;
            Zone::Offset(if token.starts_with('-') {
                -offset
            } else {
                offset
            })
        }
        _ => Zone::Invalid,
    }
}

/// The zones RFC 5322 names (3.3 and 4.3), and the unambiguous names of
/// European zones that mail programs write instead of an offset. A
/// military zone other than `Z` counts as `-0000`, UTC, as RFC 5322, 4.3
/// says. Names that stand for more than one zone (`IST`, `BST`) are not
/// listed.
fn named_zone(token: &str) -> Option<i32> {
    let hours = match token.to_ascii_uppercase().as_str() {
        "UT" | "UTC" | "GMT" | "Z" | "WET" => 0,
        "EDT" => -4,
        "EST" | "CDT" => -5,
        "CST" | "MDT" => -6,
        "MST" | "PDT" => -7,
        "PST" => -8,
        "CET" | "MET" | "MEZ" | "WEST" => 1,
        "CEST" | "MEST" | "MESZ" | "EET" => 2,
        "EEST" => 3,
        military
            if military.len() == 1
                && military != "J"
                && military.bytes().all(|b| b.is_ascii_alphabetic()) =>
        {
            0
        }
        _ => return None,
    };
    Some(hours * 60)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date of a day since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Option<String> {
        rfc3339(text)
    }

    #[test]
    fn a_date_with_an_offset_is_written_in_utc() {
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 +0200").as_deref(),
            Some("2026-09-04T10:34:56Z")
        );
        assert_eq!(
            read("Fri, 04 Sep 2026 07:34:56 -0500").as_deref(),
            Some("2026-09-04T12:34:56Z")
        );
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 -0000").as_deref(),
            Some("2026-09-04T12:34:56Z"),
            "-0000 is UTC without a known local zone"
        );
        assert_eq!(
            read("Thu, 1 Jan 2026 00:30:00 +0100").as_deref(),
            Some("2025-12-31T23:30:00Z"),
            "the shift crosses the year"
        );
        assert_eq!(
            read("Sun, 28 Feb 2027 22:00:00 -0300").as_deref(),
            Some("2027-03-01T01:00:00Z"),
            "and the end of a month that is not a leap February"
        );
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 +05:30").as_deref(),
            Some("2026-09-04T07:04:56Z")
        );
    }

    #[test]
    fn comments_weekdays_and_the_case_of_names_do_not_matter() {
        let utc = Some("2026-09-04T10:34:56Z");
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 +0200 (CEST)").as_deref(),
            utc
        );
        assert_eq!(read("4 Sep 2026 12:34:56 +0200").as_deref(), utc);
        assert_eq!(read("Fr, 4 Sep 2026 12:34:56 +0200").as_deref(), utc);
        assert_eq!(read("  fri,4 SEP 2026 12:34:56 +0200").as_deref(), utc);
        assert_eq!(read("Fri, 4 September 2026 12:34:56 +0200").as_deref(), utc);
        assert_eq!(read("Fri, 4 Sept 2026 12:34:56 +0200").as_deref(), utc);
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 +0200 trailing").as_deref(),
            utc
        );
    }

    #[test]
    fn zone_names_are_read_when_they_name_one_zone() {
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 CEST").as_deref(),
            Some("2026-09-04T10:34:56Z")
        );
        assert_eq!(
            read("Fri, 4 Dec 2026 12:34:56 MEZ").as_deref(),
            Some("2026-12-04T11:34:56Z")
        );
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 GMT").as_deref(),
            Some("2026-09-04T12:34:56Z")
        );
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 EDT").as_deref(),
            Some("2026-09-04T16:34:56Z")
        );
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 z").as_deref(),
            Some("2026-09-04T12:34:56Z")
        );
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56 IST").as_deref(),
            Some("2026-09-04T12:34:56"),
            "India, Ireland or Israel: the zone is left out"
        );
    }

    #[test]
    fn a_date_without_a_zone_stays_local_time() {
        assert_eq!(
            read("Fri, 4 Sep 2026 12:34:56").as_deref(),
            Some("2026-09-04T12:34:56")
        );
    }

    #[test]
    fn the_obsolete_forms_of_rfc_5322_are_read() {
        assert_eq!(
            read("4 Sep 97 12:34 EST").as_deref(),
            Some("1997-09-04T17:34:00Z"),
            "two-digit year from 50 on, no seconds"
        );
        assert_eq!(
            read("4 Sep 07 12:34:56 +0000").as_deref(),
            Some("2007-09-04T12:34:56Z"),
            "two-digit year below 50"
        );
        assert_eq!(
            read("4 Sep 107 12:34:56 +0000").as_deref(),
            Some("2007-09-04T12:34:56Z"),
            "three-digit year"
        );
        assert_eq!(
            read("Wed, 31 Dec 2025 23:59:60 +0000").as_deref(),
            Some("2025-12-31T23:59:60Z"),
            "a leap second stays in its minute"
        );
    }

    #[test]
    fn what_is_not_a_date_is_none() {
        for text in [
            "",
            "irgendwann",
            "Fri, 31 Feb 2026 12:34:56 +0200",
            "Thu, 29 Feb 2026 12:34:56 +0200",
            "Fri, 4 Sep 2026 24:00:00 +0200",
            "Fri, 4 Sep 2026 12:61:00 +0200",
            "Fri, 4 Sep 2026 12:34:56 +0260",
            "Fri, 4 Sep 2026 12:34:56 +02",
            "Fri, 4 Okt 2026 12:34:56 +0200",
            "Fri, 4 Sep 1880 12:34:56 +0200",
            "Fri, 4 Sep 2026",
            "2026-09-04 12:34:56",
        ] {
            assert_eq!(read(text), None, "{text:?}");
        }
        assert_eq!(
            read("Tue, 29 Feb 2028 12:00:00 +0000").as_deref(),
            Some("2028-02-29T12:00:00Z")
        );
    }
}
