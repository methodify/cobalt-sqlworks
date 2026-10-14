//! Semantic models and Analysis Services for Cobalt: run DAX, read the model's catalog.
//!
//! Two transports behind one facade (`transport::Transport`): the Power BI **Arrow query API**
//! (`rest`, `executeDaxQueries`: a bearer token in, a stream of result sets out, each batch
//! already cast to the schema `cobalt_results::ResultSet` derives from its `ColumnInfo`s) and
//! **XMLA over HTTPS** (`xmla`: plain SOAP for Azure Analysis Services, `msmdpump.dll` and, once
//! the routing capture exists, Power BI; DAX, MDX and the `$SYSTEM` DMVs; sessions with a real
//! `Cancel`). Both yield the same `DaxItem` sequence. The model tree (`cobalt_core::ModelCatalog`)
//! comes from the `INFO.VIEW.*` DAX functions, or from the TMSCHEMA DMVs over XMLA. No egui here;
//! the only async runtime type in the public API is `async fn`.
//!
//! Design: `docs/design/semantic_models.md`.

pub mod catalog;
pub mod decode;
pub mod explore;
pub mod rest;
pub mod transport;
pub mod xmla;

pub use catalog::{load_catalog, load_functions, probe, DaxFunction};
pub use decode::{DaxItem, DaxResult, DaxTable};
pub use explore::{describe as describe_explore, to_dax, DaxQuery};
pub use rest::{DaxOptions, DaxStream, PowerBiClient, POWERBI_RESOURCE};
pub use transport::{credential_resource, Transport};
pub use xmla::{is_mdx_or_dmv, Credential, XmlaClient, AZURE_AS_RESOURCE};

use cobalt_core::tabular::TabularTarget;

/// What went wrong talking to a model. `Display` is a sentence for the Messages pane;
/// `hint()` adds the fix when one is known.
#[derive(Debug, thiserror::Error)]
pub enum TabularError {
    /// 401 / 403: the token is missing, expired, for another audience, or the user lacks
    /// permission on the model.
    #[error("not authorized on the model (HTTP {status}){detail}", status = .0, detail = fmt_detail(.1))]
    Unauthorized(u16, String),
    /// 429: the per-user request budget is spent.
    #[error("Power BI asked us to slow down; retry in {retry_after}s")]
    Throttled { retry_after: u64 },
    /// Any other non-2xx answer, with the service's error code and message.
    #[error("the service returned {status}{}{}", fmt_code(.code), fmt_detail(.message))]
    Api { status: u16, code: String, message: String },
    /// The query ran and the engine answered with an error rowset (HTTP 200) or a SOAP fault.
    #[error("{message}{}", fmt_paren(.code))]
    Query { code: Option<String>, message: String },
    /// The connection failed or dropped.
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    /// The response body was not the Arrow stream or the XML we expected.
    #[error("unexpected response: {0}")]
    Decode(String),
    /// The request is not possible on this transport (an Azure AS or HTTP target on the Arrow
    /// query API, MDX on the Arrow query API, a Power BI endpoint that could not be resolved,
    /// a binary XML answer).
    #[error("{0}")]
    Unsupported(String),
}

fn fmt_detail(s: &str) -> String {
    if s.trim().is_empty() {
        String::new()
    } else {
        format!(": {}", s.trim())
    }
}

fn fmt_code(s: &str) -> String {
    if s.trim().is_empty() {
        String::new()
    } else {
        format!(" {}", s.trim())
    }
}

fn fmt_paren(s: &Option<String>) -> String {
    match s {
        Some(c) if !c.trim().is_empty() => format!(" ({})", c.trim()),
        _ => String::new(),
    }
}

/// The sentences a status bar shows under a refused request.
pub const HINT_TENANT_SETTINGS: &str = "The Arrow query API needs the model on a Fabric or Premium capacity and the tenant settings 'Dataset Execute Queries REST API' and 'Allow XMLA endpoints and Analyze in Excel with on-premises semantic models' enabled.";
pub const HINT_CAPACITY: &str = "The Arrow query API needs the model on a Fabric or Premium capacity (Fabric F SKUs including trial, Premium, Embedded); Pro workspaces are not reachable.";
pub const HINT_BUILD: &str = "Build permission on the model is required (and Dataset.Read.All on the app registration).";
pub const HINT_SIGN_IN: &str = "The Power BI service refused the token: sign in again, or the sign-in lacks the Dataset.Read.All permission (the app registration needs it; then Grant permissions on the Fabric panel).";
pub const HINT_THROTTLED: &str = "The Arrow query API allows 120 requests per minute per user (40 on Pro / PPU licences); wait and retry.";

impl TabularError {
    /// A one-sentence fix for the common refusals, when the message identifies one.
    pub fn hint(&self) -> Option<&'static str> {
        let (status, text) = match self {
            TabularError::Unauthorized(status, m) => (*status, m.to_lowercase()),
            TabularError::Api { status, code, message } => (*status, format!("{code} {message}").to_lowercase()),
            TabularError::Query { code, message } => (200, format!("{} {message}", code.as_deref().unwrap_or("")).to_lowercase()),
            TabularError::Throttled { .. } => return Some(HINT_THROTTLED),
            _ => return None,
        };
        if text.contains("xmla endpoint") || text.contains("execute queries") || text.contains("executequeries") || text.contains("tenant setting") {
            return Some(HINT_TENANT_SETTINGS);
        }
        if text.contains("premium") || text.contains("capacity") || text.contains("sku") {
            return Some(HINT_CAPACITY);
        }
        let bad_token = text.contains("token") && (text.contains("expired") || text.contains("invalid") || text.contains("audience"));
        if status == 401 || bad_token {
            return Some(HINT_SIGN_IN);
        }
        if status == 403 || text.contains("build") || text.contains("permission") || text.contains("forbidden") || text.contains("unauthorized") {
            return Some(HINT_BUILD);
        }
        None
    }

    /// True for the refusals a sign-in would fix.
    pub fn is_auth(&self) -> bool {
        matches!(self, TabularError::Unauthorized(..))
    }
}

/// The target as a sentence for titles and messages: what and where.
pub fn describe_target(target: &TabularTarget) -> String {
    match target {
        TabularTarget::PowerBi { workspace_name, model_name, .. } => format!("semantic model '{model_name}' in workspace '{workspace_name}'"),
        TabularTarget::AzureAs { region, server, model } => format!("Analysis Services model '{model}' on {server} ({region})"),
        TabularTarget::Http { url, model } => format!("model '{model}' at {url}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_and_display() {
        let e = TabularError::Api { status: 400, code: "InvalidRequest".into(), message: "XMLA endpoint feature is disabled. Turn on the tenant setting 'Allow XMLA endpoints and Analyze in Excel with on-premises semantic models' to enable this feature.".into() };
        assert_eq!(e.hint(), Some(HINT_TENANT_SETTINGS));
        assert!(e.to_string().starts_with("the service returned 400 InvalidRequest: XMLA endpoint"));
        let e = TabularError::Api { status: 400, code: String::new(), message: "This operation is only supported for datasets on a Premium capacity".into() };
        assert_eq!(e.hint(), Some(HINT_CAPACITY));
        assert_eq!(TabularError::Unauthorized(401, String::new()).hint(), Some(HINT_SIGN_IN));
        assert_eq!(TabularError::Unauthorized(401, String::new()).to_string(), "not authorized on the model (HTTP 401)");
        assert_eq!(TabularError::Unauthorized(403, "PowerBINotAuthorizedException".into()).hint(), Some(HINT_BUILD));
        assert_eq!(TabularError::Query { code: Some("0xC1140001".into()), message: "Query (1, 10) The syntax is wrong.".into() }.to_string(), "Query (1, 10) The syntax is wrong. (0xC1140001)");
        assert_eq!(TabularError::Decode("x".into()).hint(), None);
        let t = TabularTarget::PowerBi { workspace_id: "w".into(), workspace_name: "Fabric test".into(), dataset_id: "d".into(), model_name: "Sales".into() };
        assert_eq!(describe_target(&t), "semantic model 'Sales' in workspace 'Fabric test'");
    }
}
