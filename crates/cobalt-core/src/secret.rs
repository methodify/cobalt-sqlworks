use serde::{Deserialize, Serialize};
use std::fmt;

/// A reference to a secret held in the OS credential store, never the secret itself.
///
/// `key` is the keyring "user" field under [`crate::KEYRING_SERVICE`]; conventionally
/// `"{profile_id}:{kind}"` where kind is `password`, `refresh_token`, or `client_secret`.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SecretRef {
    pub key: String,
}

impl SecretRef {
    pub fn for_profile(profile: &crate::ProfileId, kind: &str) -> Self {
        Self { key: format!("{profile}:{kind}") }
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretRef({})", self.key)
    }
}

/// An in-memory secret value. Debug/Display never print it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(***)")
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Self(s)
    }
}
impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}
