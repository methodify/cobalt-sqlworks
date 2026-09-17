//! Dev helper: print a fresh SQL access token for a profile whose refresh token is in the
//! OS credential store. Usage: cargo run -p cobalt-auth --example print_token -- <profile-id> [tenant]
use cobalt_auth::{entra::refresh, CredentialResolver, EntraConfig, KeyringStore, SecretStore};
use cobalt_core::{ConnectionSettings, ProfileId};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let id = ProfileId::parse(&args.next().expect("profile id")).expect("uuid");
    let tenant = args.next();
    let cfg = EntraConfig::from_settings(&ConnectionSettings::default()).with_tenant(tenant.as_deref());
    let rt = KeyringStore
        .get(&CredentialResolver::refresh_token_ref(&id))
        .expect("keyring")
        .expect("no refresh token stored for this profile");
    let ts = refresh(&cfg, &rt).await.expect("refresh failed");
    println!("{}", ts.access.token.expose());
}
