//! Dev helper (not shipped): print a fresh access token for any resource (`.default` scope) on
//! a profile whose refresh token is in the OS credential store, e.g. the Power BI service
//! (`https://analysis.windows.net/powerbi/api`) for semantic model checks.
//! Usage: cargo run -p cobalt-auth --example print_resource_token -- <profile-id> <resource> [tenant]
use cobalt_auth::{entra::refresh, CredentialResolver, EntraConfig, KeyringStore, SecretStore};
use cobalt_core::{ConnectionSettings, ProfileId};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let id = ProfileId::parse(&args.next().expect("profile id")).expect("uuid");
    let resource = args.next().expect("resource, e.g. https://analysis.windows.net/powerbi/api");
    let tenant = args.next();
    let cfg = EntraConfig::from_settings(&ConnectionSettings::default()).with_tenant(tenant.as_deref()).with_resource(&resource);
    let rt = KeyringStore.get(&CredentialResolver::refresh_token_ref(&id)).expect("keyring").expect("no refresh token stored for this profile");
    let ts = refresh(&cfg, &rt).await.expect("refresh failed");
    // persist the rotated refresh token: Entra invalidates the previous one, so a helper that
    // refreshes without storing the new token signs the app out
    if let Some(new_rt) = &ts.refresh {
        KeyringStore.set(&CredentialResolver::refresh_token_ref(&id), new_rt).expect("keyring write");
    }
    eprintln!("scope: {}", ts.access.scope);
    println!("{}", ts.access.token.expose());
}
