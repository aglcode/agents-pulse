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

#[cfg(test)]
mod tests {
    use super::*;

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
