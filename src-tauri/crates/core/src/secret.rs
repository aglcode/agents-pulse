//! Secret handling (architecture §6.4).
//!
//! We use [`secrecy`], which is built on [`zeroize`]: memory is zeroed on drop,
//! `Debug` prints `[REDACTED]`, and there is no `Display`. `Serialize` exists only
//! for types marked `SerializableSecret`, and we never mark any.
//! Reading a secret requires an explicit `expose_secret()` call, which reviewers can grep for.
//!
//! The doctests below are part of the test suite.
//!
//! A secret cannot be serialized:
//! ```compile_fail
//! fn needs_serialize<T: serde::Serialize>(_: &T) {}
//! let s = agentpulse_core::secret::SecretString::from("token");
//! needs_serialize(&s);
//! ```
//!
//! A secret cannot be formatted with `{}`:
//! ```compile_fail
//! let s = agentpulse_core::secret::SecretString::from("token");
//! let _ = format!("{}", s);
//! ```
//!
//! Control case: the same scaffolding compiles for a normal `String`, so the two
//! `compile_fail` cases above fail because of the secret type, not because of a typo:
//! ```
//! fn needs_serialize<T: serde::Serialize>(_: &T) {}
//! let s = String::from("not a secret");
//! needs_serialize(&s);
//! let _ = format!("{}", s);
//! ```
//!
//! `Debug` is redacted:
//! ```
//! let s = agentpulse_core::secret::SecretString::from("token-123");
//! assert!(!format!("{s:?}").contains("token-123"));
//! assert!(format!("{s:?}").contains("REDACTED"));
//! ```

pub use secrecy::{ExposeSecret, SecretBox, SecretString};
pub use zeroize::{Zeroize, Zeroizing};
