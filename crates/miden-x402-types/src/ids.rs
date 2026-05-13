//! Validated hex newtypes for Miden identifiers.
//!
//! Miden identifiers travel on the wire as `0x`-prefixed lowercase hex
//! strings. The lengths are fixed:
//!
//! - [`AccountIdHex`]: 30 hex characters (15 bytes — Miden `AccountId` is
//!   carried as a u128 with the top bits as type/version tags).
//! - [`NoteIdHex`] and [`TransactionIdHex`]: 64 hex characters (32 bytes —
//!   both are Miden `Word` values, 4 × `u64`).
//!
//! The newtypes serialise transparently as JSON strings and reject malformed
//! input at deserialisation time.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

const ACCOUNT_ID_HEX_LEN: usize = 30;
const WORD_HEX_LEN: usize = 64;

/// Errors that can occur while parsing a Miden hex identifier.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdError {
    /// The string did not start with the required `0x` prefix.
    #[error("missing 0x prefix")]
    MissingPrefix,
    /// The hex portion was not the expected length.
    #[error("wrong hex length: expected {expected}, got {got}")]
    WrongLength { expected: usize, got: usize },
    /// The hex portion contained non-hex characters or was not lowercase.
    #[error("invalid hex digit at position {pos}")]
    NotHex { pos: usize },
}

fn validate(input: &str, expected_hex_len: usize) -> Result<(), IdError> {
    let body = input.strip_prefix("0x").ok_or(IdError::MissingPrefix)?;
    if body.len() != expected_hex_len {
        return Err(IdError::WrongLength {
            expected: expected_hex_len,
            got: body.len(),
        });
    }
    for (offset, byte) in body.bytes().enumerate() {
        let ok = matches!(byte, b'0'..=b'9' | b'a'..=b'f');
        if !ok {
            return Err(IdError::NotHex {
                pos: offset + 2, // +2 for the "0x" prefix in user-facing positions
            });
        }
    }
    Ok(())
}

macro_rules! hex_newtype {
    ($name:ident, $hex_len:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            /// Returns the underlying `0x`-prefixed hex string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes the newtype, returning the underlying `String`.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                validate(s, $hex_len)?;
                Ok(Self(s.to_owned()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                validate(&value, $hex_len)?;
                Ok(Self(value))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::try_from(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

hex_newtype!(
    AccountIdHex,
    ACCOUNT_ID_HEX_LEN,
    "A Miden account identifier, 30 lowercase hex characters with a `0x` prefix."
);
hex_newtype!(
    NoteIdHex,
    WORD_HEX_LEN,
    "A Miden note identifier (`Word`), 64 lowercase hex characters with a `0x` prefix."
);
hex_newtype!(
    TransactionIdHex,
    WORD_HEX_LEN,
    "A Miden transaction identifier (`Word`), 64 lowercase hex characters with a `0x` prefix."
);

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ACCOUNT: &str = "0x0a7d175ed63ec5200fb2ced86f6aa5";
    const SAMPLE_WORD: &str = "0xabcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    #[test]
    fn account_id_accepts_sample_faucet() {
        let id: AccountIdHex = SAMPLE_ACCOUNT.parse().expect("valid");
        assert_eq!(id.as_str(), SAMPLE_ACCOUNT);
    }

    #[test]
    fn account_id_rejects_missing_prefix() {
        let raw = SAMPLE_ACCOUNT.trim_start_matches("0x").to_owned();
        assert_eq!(AccountIdHex::from_str(&raw), Err(IdError::MissingPrefix));
    }

    #[test]
    fn account_id_rejects_wrong_length() {
        let res = AccountIdHex::from_str("0xdeadbeef");
        assert!(matches!(
            res,
            Err(IdError::WrongLength {
                expected: 30,
                got: 8
            })
        ));
    }

    #[test]
    fn account_id_rejects_uppercase() {
        // Uppercase hex is intentionally rejected so that wire data is canonical.
        let upper = format!("0x{}", "A".repeat(30));
        assert!(matches!(
            AccountIdHex::from_str(&upper),
            Err(IdError::NotHex { .. })
        ));
    }

    #[test]
    fn account_id_rejects_non_hex() {
        let bad = format!("0x{}", "z".repeat(30));
        assert!(matches!(
            AccountIdHex::from_str(&bad),
            Err(IdError::NotHex { .. })
        ));
    }

    #[test]
    fn note_id_accepts_word_length() {
        let id: NoteIdHex = SAMPLE_WORD.parse().expect("valid");
        assert_eq!(id.as_str(), SAMPLE_WORD);
    }

    #[test]
    fn transaction_id_accepts_word_length() {
        let id: TransactionIdHex = SAMPLE_WORD.parse().expect("valid");
        assert_eq!(id.as_str(), SAMPLE_WORD);
    }

    #[test]
    fn note_id_rejects_account_length() {
        // The faucet id is the right shape for an account but not for a note.
        let res = NoteIdHex::from_str(SAMPLE_ACCOUNT);
        assert!(matches!(
            res,
            Err(IdError::WrongLength {
                expected: 64,
                got: 30
            })
        ));
    }

    #[test]
    fn serde_round_trip_account_id() {
        let id: AccountIdHex = SAMPLE_ACCOUNT.parse().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{SAMPLE_ACCOUNT}\""));
        let decoded: AccountIdHex = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn serde_rejects_malformed_account_id() {
        let res: Result<AccountIdHex, _> = serde_json::from_str("\"not-hex\"");
        assert!(res.is_err());
    }
}
