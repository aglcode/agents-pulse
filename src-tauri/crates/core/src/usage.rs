//! Token usage vectors and the per-field deltas used to keep rollups exact.
//!
//! `None` means the provider did not report that field. It is distinct from
//! `Some(0)` in storage, but counts as 0 when summing.
//!
//! Field semantics, the same for every provider (collectors must convert):
//! * `input`: fresh (uncached) input tokens. Excludes cache reads and writes.
//! * `cache_read` / `cache_write`: cached input read / written.
//! * `output`: all output tokens, **including** reasoning.
//! * `reasoning`: the reasoning portion of `output` (a subset; never add it to `output`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub reasoning: Option<u64>,
}

impl TokenUsage {
    /// Fields as summable i64 values in a fixed order:
    /// input, output, cache_read, cache_write, reasoning.
    pub fn sums(&self) -> [i64; 5] {
        let f = |v: Option<u64>| v.map_or(0, |x| i64::try_from(x).unwrap_or(i64::MAX));
        [
            f(self.input),
            f(self.output),
            f(self.cache_read),
            f(self.cache_write),
            f(self.reasoning),
        ]
    }
}

/// Signed per-field change applied to a rollup bucket, plus the record-count change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenDelta {
    pub fields: [i64; 5],
    pub records: i64,
}

impl TokenDelta {
    /// Contribution of a whole record (used on insert, or when moving into a bucket).
    pub fn of(usage: &TokenUsage) -> Self {
        Self { fields: usage.sums(), records: 1 }
    }

    /// Change within the same bucket when a record is revised from `old` to `new`.
    pub fn revision(old: &TokenUsage, new: &TokenUsage) -> Self {
        let (o, n) = (old.sums(), new.sums());
        let mut fields = [0i64; 5];
        for i in 0..5 {
            fields[i] = n[i] - o[i];
        }
        Self { fields, records: 0 }
    }

    pub fn negate(self) -> Self {
        let mut fields = self.fields;
        for f in &mut fields {
            *f = -*f;
        }
        Self { fields, records: -self.records }
    }

    pub fn is_zero(&self) -> bool {
        self.records == 0 && self.fields.iter().all(|f| *f == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_delta_covers_every_field_including_decreases_and_null_to_value() {
        let old = TokenUsage { input: Some(100), output: Some(10), cache_read: None, cache_write: Some(5), reasoning: None };
        let new = TokenUsage { input: Some(90), output: Some(40), cache_read: Some(7), cache_write: Some(5), reasoning: None };
        let d = TokenDelta::revision(&old, &new);
        assert_eq!(d.fields, [-10, 30, 7, 0, 0]);
        assert_eq!(d.records, 0);
    }

    #[test]
    fn negate_and_zero() {
        let d = TokenDelta::of(&TokenUsage { input: Some(3), ..Default::default() });
        assert_eq!(d.negate().fields[0], -3);
        assert_eq!(d.negate().records, -1);
        assert!(TokenDelta::default().is_zero());
    }
}
