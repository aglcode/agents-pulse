//! Time is stored everywhere as unix milliseconds in UTC (architecture §5).

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix epoch milliseconds, UTC.
pub type UnixMs = i64;

pub const HOUR_MS: i64 = 3_600_000;
pub const DAY_MS: i64 = 24 * HOUR_MS;

/// Current wall-clock time in unix milliseconds. Returns 0 if the clock is before 1970.
pub fn now_ms() -> UnixMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Start of the UTC hour containing `ms`.
pub fn hour_bucket(ms: UnixMs) -> UnixMs {
    ms.div_euclid(HOUR_MS) * HOUR_MS
}

/// Start of the local day containing `now`, expressed in UTC milliseconds.
/// `utc_offset_minutes` is local time minus UTC (e.g. +480 for UTC+8).
pub fn local_day_start_utc(now: UnixMs, utc_offset_minutes: i32) -> UnixMs {
    let offset = i64::from(utc_offset_minutes) * 60_000;
    (now + offset).div_euclid(DAY_MS) * DAY_MS - offset
}

/// Parse an RFC 3339 timestamp (`2026-10-04T03:17:34.123Z`, `…+08:00`) into unix ms.
/// Returns `None` for anything malformed. Fractions beyond milliseconds are truncated.
pub fn parse_rfc3339_ms(s: &str) -> Option<UnixMs> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { std::str::from_utf8(b.get(r)?).ok()?.parse().ok() };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut i = 19;
    let mut millis = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            if i - start < 3 {
                millis = millis * 10 + i64::from(b[i] - b'0');
            }
            i += 1;
        }
        if i == start {
            return None;
        }
        for _ in (i - start)..3 {
            millis *= 10;
        }
    }
    let offset_secs = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let off = num(i + 1..i + 3)? * 3600 + num(i + 4..i + 6)? * 60;
            if *sign == b'+' { off } else { -off }
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant's algorithm), valid for the proleptic Gregorian calendar.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs) * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_variants() {
        assert_eq!(parse_rfc3339_ms("2026-11-03T01:00:00Z"), Some(1_793_667_600_000));
        assert_eq!(parse_rfc3339_ms("2026-11-03T01:00:00.123Z"), Some(1_793_667_600_123));
        assert_eq!(parse_rfc3339_ms("2026-11-03T01:00:00.123456789Z"), Some(1_793_667_600_123));
        assert_eq!(parse_rfc3339_ms("2026-11-03T09:00:00.5+08:00"), Some(1_793_667_600_500));
        assert_eq!(parse_rfc3339_ms("2026-11-02T20:00:00-05:00"), Some(1_793_667_600_000));
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        for bad in ["", "2026-11-03", "2026-13-03T01:00:00Z", "2026-11-03T01:00:00", "2026-11-03T01:00:00.Z", "garbage-in-here-xxxxx"] {
            assert_eq!(parse_rfc3339_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn hour_bucket_floors() {
        assert_eq!(hour_bucket(HOUR_MS * 5 + 1), HOUR_MS * 5);
        assert_eq!(hour_bucket(HOUR_MS * 5), HOUR_MS * 5);
    }

    #[test]
    fn local_day_start_respects_offset() {
        // 2026-11-03T01:00Z is 09:00 local in UTC+8, so the local day began at 2026-11-02T16:00Z.
        let t = 1_793_667_600_000; // 2026-11-03T01:00:00Z
        let start = local_day_start_utc(t, 480);
        assert_eq!(start, t - 9 * HOUR_MS);
        // Same instant in UTC: day began at 00:00Z.
        assert_eq!(local_day_start_utc(t, 0), t - HOUR_MS);
    }
}
