use chrono::Utc;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::types::LinkUnderstandingConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedContent {
    pub url: String,
    pub title: Option<String>,
    pub text: String,
    pub content_type: Option<String>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

pub struct ContentFetcher {
    client: Client,
    config: LinkUnderstandingConfig,
}

impl ContentFetcher {
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
    ) -> Result<FetchedContent, Box<dyn std::error::Error + Send + Sync>> {
        let response = self.client.get(url).send().await?;

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| {
                let parts: Vec<&str> = s.split(';').collect();
                parts
                    .first()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            });

        let final_url = response.url().to_string();

        let html = response.text().await?;

        let truncated = if html.len() > self.config.max_content_length * 10 {
            &html[..self.config.max_content_length * 10]
        } else {
            &html
        };

        let (title, text) = self.extract_text(truncated)?;

        let text = if text.len() > self.config.max_content_length {
            text[..self.config.max_content_length].to_string()
        } else {
            text
        };

        Ok(FetchedContent {
            url: final_url,
            title,
            text,
            content_type,
            fetched_at: Utc::now(),
        })
    }

    fn extract_text(
        &self,
        html: &str,
    ) -> Result<(Option<String>, String), Box<dyn std::error::Error + Send + Sync>> {
        let document = Html::parse_document(html);

        let title = {
            let title_selector =
                Selector::parse("title").map_err(|e| format!("selector error: {:?}", e))?;
            document
                .select(&title_selector)
                .next()
                .map(|el| el.text().collect::<String>().trim().to_string())
        };

        let body_selector =
            Selector::parse("body").map_err(|e| format!("selector error: {:?}", e))?;

        let mut text = String::new();

        if let Some(body) = document.select(&body_selector).next() {
            for node_text in body.text() {
                let t = node_text.trim();
                if !t.is_empty() {
                    if !text.is_empty() && !text.ends_with(' ') && !text.ends_with('\n') {
                        text.push(' ');
                    }
                    text.push_str(t);
                }
            }
        }

        let text = self.normalize_whitespace(&text);

        Ok((title, text))
    }

    fn normalize_whitespace(&self, text: &str) -> String {
        let re = regex::Regex::new(r"\s+").unwrap();
        re.replace_all(text, " ").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text() {
        let html = r#"
        <html>
            <head><title>Test Page</title></head>
            <body>
                <nav>Navigation</nav>
                <main>
                    <h1>Hello World</h1>
                    <p>This is a test paragraph.</p>
                </main>
                <footer>Footer</footer>
            </body>
        </html>
        "#;

        let fetcher = ContentFetcher::with_defaults();
        let (title, text) = fetcher.extract_text(html).unwrap();

        assert_eq!(title, Some("Test Page".to_string()));
        assert!(text.contains("Hello World"));
        assert!(text.contains("test paragraph"));
    }
}
