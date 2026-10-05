//! Service principal with a certificate: the client proves itself with a JWT (`client_assertion`)
//! signed by the certificate's RSA private key, instead of a client secret. The PEM file holds the
//! certificate and the private key (PKCS#8 "PRIVATE KEY" or PKCS#1 "RSA PRIVATE KEY"), the way
//! `az ad sp create-for-rbac --create-cert` and Key Vault exports produce it.

use crate::entra::token::post_token;
use crate::entra::{AccessToken, EntraConfig};
use crate::error::{AuthError, Result};
use base64::Engine;
use chrono::{Duration, Utc};
use cobalt_core::Secret;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::RsaPrivateKey;
use sha1::Digest as _;
use std::path::Path;

const ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// The parts of a PEM bundle this flow needs.
pub struct PemBundle {
    pub key: RsaPrivateKey,
    /// SHA-1 thumbprint of the first certificate, base64url (the JWT `x5t` header).
    pub x5t: String,
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Blocks of a PEM text as (label, DER bytes).
fn pem_blocks(text: &str) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut label: Option<String> = None;
    let mut body = String::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("-----BEGIN ") {
            label = Some(rest.trim_end_matches('-').trim().to_string());
            body.clear();
        } else if l.starts_with("-----END ") {
            if let Some(lab) = label.take() {
                if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body.replace(['\r', '\n', ' '], "")) {
                    out.push((lab, der));
                }
            }
            body.clear();
        } else if label.is_some() {
            body.push_str(l);
        }
    }
    out
}

/// Parse a PEM bundle (certificate + unencrypted RSA private key).
pub fn parse_pem(text: &str) -> Result<PemBundle> {
    let blocks = pem_blocks(text);
    let cert = blocks.iter().find(|(l, _)| l == "CERTIFICATE").map(|(_, d)| d.clone()).ok_or_else(|| AuthError::Other("the PEM file has no CERTIFICATE block".into()))?;
    let key = blocks
        .iter()
        .find_map(|(l, der)| match l.as_str() {
            "PRIVATE KEY" => RsaPrivateKey::from_pkcs8_der(der).ok(),
            "RSA PRIVATE KEY" => RsaPrivateKey::from_pkcs1_der(der).ok(),
            "ENCRYPTED PRIVATE KEY" => None,
            _ => None,
        })
        .ok_or_else(|| {
            if blocks.iter().any(|(l, _)| l == "ENCRYPTED PRIVATE KEY") {
                AuthError::Other("the private key is password-protected; export it unencrypted".into())
            } else {
                AuthError::Other("the PEM file has no RSA private key (PRIVATE KEY or RSA PRIVATE KEY block)".into())
            }
        })?;
    let thumb = sha1::Sha1::digest(&cert);
    Ok(PemBundle { key, x5t: b64url(&thumb) })
}

/// The signed client assertion for `tenant` / `client_id`, valid for ten minutes.
pub fn client_assertion(bundle: &PemBundle, tenant: &str, client_id: &str, now_unix: i64) -> Result<String> {
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT", "x5t": bundle.x5t });
    let claims = serde_json::json!({
        "aud": format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
        "iss": client_id,
        "sub": client_id,
        "jti": uuid::Uuid::new_v4().to_string(),
        "nbf": now_unix - 60,
        "exp": now_unix + 600,
        "iat": now_unix,
    });
    let signing_input = format!("{}.{}", b64url(header.to_string().as_bytes()), b64url(claims.to_string().as_bytes()));
    let signing_key = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(bundle.key.clone());
    let sig = signing_key.try_sign(signing_input.as_bytes()).map_err(|e| AuthError::Other(format!("signing the client assertion failed: {e}")))?;
    Ok(format!("{signing_input}.{}", b64url(&sig.to_bytes())))
}

/// Acquire a SQL token for a service principal using the certificate at `pem_path`.
pub async fn client_certificate_token(cfg: &EntraConfig, tenant: &str, client_id: &str, pem_path: &Path) -> Result<AccessToken> {
    let tenant = tenant.trim();
    if tenant.is_empty() || matches!(tenant, "common" | "organizations" | "consumers") {
        return Err(AuthError::Other("a service principal needs a specific tenant ID or domain".into()));
    }
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Err(AuthError::Other("service principal client ID is empty".into()));
    }
    let text = std::fs::read_to_string(pem_path).map_err(|e| AuthError::Other(format!("cannot read {}: {e}", pem_path.display())))?;
    let bundle = parse_pem(&text)?;
    let assertion = client_assertion(&bundle, tenant, client_id, Utc::now().timestamp())?;
    let cfg = EntraConfig { client_id: client_id.to_owned(), tenant: Some(tenant.to_owned()), ..cfg.clone() };
    let scope = cfg.sql_scope();
    let form = [
        ("client_id", client_id),
        ("client_assertion_type", ASSERTION_TYPE),
        ("client_assertion", assertion.as_str()),
        ("grant_type", "client_credentials"),
        ("scope", scope.as_str()),
    ];
    let t = post_token(&cfg, &form).await?;
    Ok(AccessToken { token: Secret::new(t.access_token), expires_at: Utc::now() + Duration::seconds(t.expires_in.unwrap_or(3600).clamp(0, 86_400)), scope: t.scope.unwrap_or(scope) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::signature::Verifier;

    fn test_pem() -> (String, RsaPrivateKey) {
        let mut rng = rand_core06::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("key");
        let key_pem = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).expect("pem").to_string();
        // any DER-looking bytes do for the certificate block: x5t is just its SHA-1
        let cert = base64::engine::general_purpose::STANDARD.encode(b"not-really-a-certificate");
        (format!("-----BEGIN CERTIFICATE-----\n{cert}\n-----END CERTIFICATE-----\n{key_pem}"), key)
    }

    #[test]
    fn parses_pem_and_signs_a_verifiable_assertion() {
        let (pem, key) = test_pem();
        let bundle = parse_pem(&pem).expect("bundle");
        assert_eq!(bundle.x5t, b64url(&sha1::Sha1::digest(b"not-really-a-certificate")));
        let jwt = client_assertion(&bundle, "tenant-id", "client-id", 1_700_000_000).expect("jwt");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["x5t"], bundle.x5t);
        let claims: serde_json::Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://login.microsoftonline.com/tenant-id/oauth2/v2.0/token");
        assert_eq!(claims["iss"], "client-id");
        assert_eq!(claims["exp"], 1_700_000_600);
        let verifying = rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(rsa::RsaPublicKey::from(&key));
        let sig = rsa::pkcs1v15::Signature::try_from(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap().as_slice()).unwrap();
        verifying.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).expect("signature verifies");
    }

    #[test]
    fn rejects_missing_blocks() {
        assert!(parse_pem("nothing here").is_err());
        let (pem, _) = test_pem();
        let no_cert: String = pem.lines().skip_while(|l| !l.starts_with("-----BEGIN PRIVATE")).collect::<Vec<_>>().join("\n");
        match parse_pem(&no_cert) {
            Err(e) => assert!(e.to_string().contains("CERTIFICATE"), "{e}"),
            Ok(_) => panic!("a PEM without a certificate must be rejected"),
        }
    }
}
