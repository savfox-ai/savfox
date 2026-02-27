use chrono::Utc;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::types::LinkUnderstandingConfig;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenGraph {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub url: Option<String>,
    pub site_name: Option<String>,
    pub r#type: Option<String>,
    pub locale: Option<String>,
}

pub struct OpenGraphFetcher {
    client: Client,
    config: LinkUnderstandingConfig,
}

impl OpenGraphFetcher {
    pub fn new(config: LinkUnderstandingConfig) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_seconds))
            .user_agent(&config.user_agent)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("failed to create HTTP client");

        Self { client, config }
    }

    pub fn with_defaults() -> Self {
        Self::new(LinkUnderstandingConfig::default())
    }

    pub async fn fetch(
        &self,
        url: &str,
    ) -> Result<OpenGraph, Box<dyn std::error::Error + Send + Sync>> {
        let response = self.client.get(url).send().await?;

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if let Some(ref ct) = content_type {
            if !ct.contains("text/html") {
                debug!("Skipping non-HTML content: {}", ct);
                return Ok(OpenGraph::default());
            }
        }

        let html = response.text().await?;

        if html.len() > self.config.max_content_length * 10 {
            debug!("HTML too large, truncating");
            return self.parse_html(&html[..self.config.max_content_length * 10]);
        }

        self.parse_html(&html)
    }

    fn parse_html(
        &self,
        html: &str,
    ) -> Result<OpenGraph, Box<dyn std::error::Error + Send + Sync>> {
        let document = Html::parse_document(html);
        let mut og = OpenGraph::default();

        let meta_selector =
            Selector::parse("meta").map_err(|e| format!("selector error: {:?}", e))?;

        for meta in document.select(&meta_selector) {
            let property = meta.value().attr("property");
            let name = meta.value().attr("name");
            let content = meta.value().attr("content");

            if let (Some(prop), Some(content)) = (property, content) {
                match prop {
                    "og:title" => og.title = Some(content.to_string()),
                    "og:description" => og.description = Some(content.to_string()),
                    "og:image" => og.image = Some(content.to_string()),
                    "og:url" => og.url = Some(content.to_string()),
                    "og:site_name" => og.site_name = Some(content.to_string()),
                    "og:type" => og.r#type = Some(content.to_string()),
                    "og:locale" => og.locale = Some(content.to_string()),
                    _ => {}
                }
            } else if let (Some(name), Some(content)) = (name, content) {
                match name {
                    "twitter:title" if og.title.is_none() => og.title = Some(content.to_string()),
                    "twitter:description" if og.description.is_none() => {
                        og.description = Some(content.to_string())
                    }
                    "twitter:image" if og.image.is_none() => og.image = Some(content.to_string()),
                    "description" if og.description.is_none() => {
                        og.description = Some(content.to_string())
                    }
                    _ => {}
                }
            }
        }

        if og.title.is_none() {
            let title_selector =
                Selector::parse("title").map_err(|e| format!("selector error: {:?}", e))?;
            if let Some(title_el) = document.select(&title_selector).next() {
                og.title = Some(title_el.text().collect::<String>().trim().to_string());
            }
        }

        Ok(og)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_html_basic() {
        let html = r#"
        <html>
            <head>
                <meta property="og:title" content="Test Title">
                <meta property="og:description" content="Test Description">
                <meta name="twitter:image" content="https://example.com/image.png">
            </head>
        </html>
        "#;

        let fetcher = OpenGraphFetcher::with_defaults();
        let result = fetcher.parse_html(html).unwrap();

        assert_eq!(result.title, Some("Test Title".to_string()));
        assert_eq!(result.description, Some("Test Description".to_string()));
        assert_eq!(
            result.image,
            Some("https://example.com/image.png".to_string())
        );
    }
}
