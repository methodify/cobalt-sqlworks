//! One facade over the two transports: the Arrow query API (`rest`) for Power BI, XMLA for
//! Azure Analysis Services and HTTP endpoints (and Power BI when forced). The app picks the
//! credential with [`credential_resource`], connects once, then runs, cancels and loads the
//! catalog without knowing which wire it is on.

use crate::catalog::{self, DaxFunction};
use crate::rest::{DaxOptions, DaxStream, PowerBiClient, POWERBI_RESOURCE};
use crate::xmla::{is_mdx_or_dmv, Credential, XmlaClient, AZURE_AS_RESOURCE};
use crate::TabularError;
use cobalt_core::tabular::{ModelCatalog, TabularTarget};

/// A connected transport.
pub enum Transport {
    Rest(PowerBiClient),
    Xmla(XmlaClient),
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Rest(_) => f.write_str("Transport::Rest"),
            Transport::Xmla(x) => f.debug_tuple("Transport::Xmla").field(x).finish(),
        }
    }
}

/// The Entra resource a bearer token for the target needs: Power BI's API audience for Power
/// BI (both transports), Azure AS's for Azure AS; none for an HTTP endpoint (Basic, anonymous,
/// or a bearer the caller already has).
pub fn credential_resource(target: &TabularTarget, force_xmla: bool) -> Option<&'static str> {
    let _ = force_xmla;
    match target {
        TabularTarget::PowerBi { .. } => Some(POWERBI_RESOURCE),
        TabularTarget::AzureAs { .. } => Some(AZURE_AS_RESOURCE),
        TabularTarget::Http { .. } => None,
    }
}

impl Transport {
    /// Rest for Power BI unless `force_xmla`; Xmla for Azure AS and Http. Rest needs no network
    /// here (the first query proves the token); Xmla resolves the endpoint and opens a session.
    pub async fn connect(target: &TabularTarget, cred: Credential, force_xmla: bool) -> Result<Transport, TabularError> {
        match target {
            TabularTarget::PowerBi { .. } if !force_xmla => match cred {
                Credential::Bearer(token) => Ok(Transport::Rest(PowerBiClient::new(token))),
                other => Err(TabularError::Unsupported(format!("the Arrow query API needs a bearer token, not {other}"))),
            },
            _ => Ok(Transport::Xmla(XmlaClient::connect(target, cred).await?)),
        }
    }

    /// `"Arrow query API"` or `"XMLA"`.
    pub fn name(&self) -> &'static str {
        match self {
            Transport::Rest(_) => "Arrow query API",
            Transport::Xmla(_) => "XMLA",
        }
    }

    /// Run query text. The Arrow query API takes DAX only; MDX and DMV text is refused there.
    pub async fn execute(&self, target: &TabularTarget, text: &str, opts: &DaxOptions) -> Result<DaxStream, TabularError> {
        match self {
            Transport::Rest(c) => {
                if is_mdx_or_dmv(text) {
                    return Err(TabularError::Unsupported("MDX and DMV queries need the XMLA transport".into()));
                }
                c.execute(target, text, opts).await
            }
            Transport::Xmla(x) => x.execute_in(target.model(), text, opts).await,
        }
    }

    /// Stop the running query. The Arrow query API has no cancel (the caller drops the
    /// connection); XMLA sends `<Cancel/>`.
    pub async fn cancel(&self) -> Result<(), TabularError> {
        match self {
            Transport::Rest(_) => Ok(()),
            Transport::Xmla(x) => x.cancel().await,
        }
    }

    pub async fn load_catalog(&self, target: &TabularTarget) -> Result<ModelCatalog, TabularError> {
        match self {
            Transport::Rest(c) => catalog::load_catalog(c, target).await,
            Transport::Xmla(x) => x.load_catalog(target.model()).await,
        }
    }

    pub async fn load_functions(&self, target: &TabularTarget) -> Result<Vec<DaxFunction>, TabularError> {
        match self {
            Transport::Rest(c) => catalog::load_functions(c, target).await,
            Transport::Xmla(x) => x.load_functions().await,
        }
    }

    /// The models on the server (XMLA only).
    pub async fn list_catalogs(&self) -> Result<Vec<String>, TabularError> {
        match self {
            Transport::Rest(_) => Err(TabularError::Unsupported("listing catalogs needs the XMLA transport".into())),
            Transport::Xmla(x) => x.list_catalogs().await,
        }
    }

    /// The server version, when the transport learned it.
    pub fn server_version(&self) -> Option<String> {
        match self {
            Transport::Rest(_) => None,
            Transport::Xmla(x) => x.server_version(),
        }
    }

    /// Close the session (XMLA); nothing to do for Rest.
    pub async fn close(&self) {
        if let Transport::Xmla(x) = self {
            x.end_session().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pbi() -> TabularTarget {
        TabularTarget::PowerBi { workspace_id: "w".into(), workspace_name: "W".into(), dataset_id: "d".into(), model_name: "M".into() }
    }

    #[test]
    fn resources() {
        assert_eq!(credential_resource(&pbi(), false), Some("https://analysis.windows.net/powerbi/api"));
        assert_eq!(credential_resource(&pbi(), true), Some("https://analysis.windows.net/powerbi/api"));
        assert_eq!(credential_resource(&TabularTarget::AzureAs { region: "r".into(), server: "s".into(), model: "m".into() }, false), Some("https://*.asazure.windows.net"));
        assert_eq!(credential_resource(&TabularTarget::Http { url: "u".into(), model: "m".into() }, false), None);
    }

    #[tokio::test]
    async fn rest_needs_no_network_and_refuses_mdx() {
        let t = Transport::connect(&pbi(), Credential::Bearer("tok".into()), false).await.unwrap();
        assert_eq!(t.name(), "Arrow query API");
        assert!(matches!(t, Transport::Rest(_)));
        let e = match t.execute(&pbi(), "SELECT [Measures].[x] ON COLUMNS FROM [M]", &DaxOptions::default()).await {
            Err(e) => e,
            Ok(_) => panic!("MDX ran on the Arrow query API"),
        };
        assert!(matches!(&e, TabularError::Unsupported(m) if m == "MDX and DMV queries need the XMLA transport"));
        assert!(t.cancel().await.is_ok());
        assert!(matches!(t.list_catalogs().await, Err(TabularError::Unsupported(_))));
        assert_eq!(t.server_version(), None);
        let e = Transport::connect(&pbi(), Credential::Anonymous, false).await.unwrap_err();
        assert!(matches!(e, TabularError::Unsupported(_)));
    }
}
