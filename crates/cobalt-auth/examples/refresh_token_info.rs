//! Dev helper (not shipped): describe the refresh token stored for a profile (length, shape) and
//! try one silent refresh for a resource, printing the full error.
//! Usage: cargo run -p cobalt-auth --example refresh_token_info -- <profile-id> [resource] [tenant]
use cobalt_auth::{entra::refresh, CredentialResolver, EntraConfig, KeyringStore, SecretStore};
use cobalt_core::{ConnectionSettings, ProfileId};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let id = ProfileId::parse(&args.next().expect("profile id")).expect("uuid");
    let resource = args.next().unwrap_or_else(|| "https://api.fabric.microsoft.com".into());
    let tenant = args.next();
    let rt = KeyringStore.get(&CredentialResolver::refresh_token_ref(&id)).expect("keyring").expect("no refresh token stored for this profile");
    let s = rt.expose();
    let chars: Vec<char> = s.chars().collect();
    println!("refresh token: {} chars, {} bytes, starts {:?}, ends {:?}, dots {}", chars.len(), s.len(), chars.iter().take(12).collect::<String>(), chars.iter().rev().take(12).collect::<String>().chars().rev().collect::<String>(), s.matches('.').count());
    println!("has whitespace: {}", s.chars().any(char::is_whitespace));
    let cfg = EntraConfig::from_settings(&ConnectionSettings::default()).with_tenant(tenant.as_deref()).with_resource(&resource);
    match refresh(&cfg, &rt).await {
        Ok(ts) => println!("refresh OK: scope {}", ts.access.scope),
        Err(e) => println!("refresh FAILED: {e:#?}"),
    }
}
