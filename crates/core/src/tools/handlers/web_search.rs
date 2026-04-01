use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::parse_arguments;
use crate::function_tool::{FunctionCallError, model_err};
use crate::tools::context::{ToolInvocation, ToolOutput, ToolPayload};
use crate::tools::registry::{ToolHandler, ToolKind};

const BRAVE_SEARCH_API_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Environment variable for the Brave Search API key.
const API_KEY_ENV: &str = "SAVFOX_WEB_SEARCH_API_KEY";

#[derive(Deserialize)]
struct WebSearchArgs {
    /// The search query.
    query: String,
    /// Maximum number of results to return (default 5).
    #[serde(default = "defaults::limit")]
    limit: usize,
    /// Optional domain filter to restrict results.
    #[serde(default)]
    site: Option<String>,
}

mod defaults {
    pub fn limit() -> usize {
        5
    }
}

#[derive(Deserialize)]
struct BraveSearchResponse {
    web: Option<BraveWebResults>,
}

#[derive(Deserialize)]
struct BraveWebResults {
    results: Vec<BraveWebResult>,
}

#[derive(Deserialize)]
struct BraveWebResult {
    title: String,
    url: String,
    description: Option<String>,
}

pub struct WebSearchHandler;

#[async_trait::async_trait]
impl ToolHandler for WebSearchHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match &_invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => return model_err("WebSearchHandler received unsupported payload"),
        };
        let args: WebSearchArgs = parse_arguments(&arguments)?;
        // Look up the API key from the environment.
        let api_key = match std::env::var(API_KEY_ENV) {
            Ok(key) if !key.is_empty() => key,
            _ => {
                return Ok(ToolOutput::fail(format!(
                    "Web search is not configured. Set the {API_KEY_ENV} environment variable \
                     to a Brave Search API key to enable web search."
                )));
            }
        };

        // Build the search query, optionally scoped to a site.
        let query = if let Some(ref site) = args.site {
            format!("site:{site} {}", args.query)
        } else {
            args.query.clone()
        };

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to build HTTP client: {err}"))
            })?;

        // Build the URL with query parameters manually since reqwest may not
        // have the `query` feature enabled in this workspace configuration.
        let count = args.limit.min(20);
        let search_url = format!(
            "{}?q={}&count={}",
            BRAVE_SEARCH_API_URL,
            urlencoding::encode(&query),
            count
        );

        let response = client
            .get(&search_url)
            .header("X-Subscription-Token", &api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("search request failed: {err}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let body_bytes = response.bytes().await.unwrap_or_default();
            let body = String::from_utf8_lossy(&body_bytes);
            return model_err(format!("search API returned HTTP {status}: {body}"));
        }

        let body_bytes = response.bytes().await.map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to read search response: {err}"))
        })?;
        let brave_response: BraveSearchResponse =
            serde_json::from_slice(&body_bytes).map_err(|err| {
                FunctionCallError::RespondToModel(format!("failed to parse search response: {err}"))
            })?;

        let results: Vec<serde_json::Value> = brave_response
            .web
            .map(|web| {
                web.results
                    .into_iter()
                    .take(args.limit)
                    .map(|r| {
                        json!({
                            "title": r.title,
                            "url": r.url,
                            "snippet": r.description.unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let output = serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".to_string());

        Ok(ToolOutput::ok(output))
    }
}
