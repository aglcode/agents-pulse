//! Stable hashing for keys we must not store in clear text: identity values,
//! session ids, file paths and install locations.

use sha2::{Digest, Sha256};

/// SHA-256 over the parts, separated by an ASCII unit separator (so `["ab","c"]`
/// and `["a","bc"]` differ), hex-encoded.
pub fn stable_hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            h.update([0x1f]);
        }
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_prevents_ambiguity() {
        assert_ne!(stable_hash(&["ab", "c"]), stable_hash(&["a", "bc"]));
        assert_eq!(stable_hash(&["x"]), stable_hash(&["x"]));
        assert_eq!(stable_hash(&["x"]).len(), 64);
    }
}
