//! RFC 7636 Proof Key for Code Exchange (S256) and CSRF `state` generation.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

/// A PKCE verifier (43..=128 unreserved chars) and its S256 challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// 32 random bytes → 43-char base64url verifier.
    pub fn generate() -> Self {
        let bytes: [u8; 32] = rand::random();
        Self::from_verifier(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Build from a known verifier (tests / RFC vectors).
    pub fn from_verifier(verifier: impl Into<String>) -> Self {
        let verifier = verifier.into();
        debug_assert!(
            (43..=128).contains(&verifier.len()),
            "PKCE verifier length {}",
            verifier.len()
        );
        let challenge = challenge_for(&verifier);
        Self {
            verifier,
            challenge,
        }
    }
}

/// `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Random opaque value bound to one authorization request.
pub fn random_state() -> String {
    let bytes: [u8; 24] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_verifier_is_unreserved_and_sized() {
        let p = Pkce::generate();
        assert_eq!(p.verifier.len(), 43);
        assert!(p
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(p.challenge, challenge_for(&p.verifier));
        assert_ne!(p.verifier, Pkce::generate().verifier);
    }

    #[test]
    fn state_is_random() {
        let a = random_state();
        let b = random_state();
        assert_ne!(a, b);
        assert!(a.len() >= 32);
    }
}
