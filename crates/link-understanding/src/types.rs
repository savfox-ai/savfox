use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkUnderstandingConfig {
    pub enabled: bool,
    pub max_links: usize,
    pub timeout_seconds: u64,
    pub max_content_length: usize,
    pub user_agent: String,
    pub include_opengraph: bool,
    pub include_content: bool,
    pub max_summary_length: usize,
    pub blocked_domains: Vec<String>,
    pub allowed_domains: Option<Vec<String>>,
}

impl Default for LinkUnderstandingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_links: 5,
            timeout_seconds: 30,
            max_content_length: 10000,
            user_agent: "Mozilla/5.0 (compatible; SavfoxBot/1.0)".to_string(),
            include_opengraph: true,
            include_content: true,
            max_summary_length: 500,
            blocked_domains: vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "0.0.0.0".to_string(),
            ],
            allowed_domains: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkInfo {
    pub url: String,
    pub domain: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub site_name: Option<String>,
    pub content: Option<String>,
    pub content_type: Option<String>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

impl LinkInfo {
    pub fn format_summary(&self, max_length: usize) -> String {
        let mut parts = Vec::new();

        if let Some(ref title) = self.title {
            parts.push(format!("**{}**", title));
        }

        if let Some(ref description) = self.description {
            parts.push(description.clone());
        }

        if let Some(ref site_name) = self.site_name {
            parts.push(format!("_{}_ ({})", site_name, self.domain));
        }

        let summary = parts.join("\n");

        if summary.len() > max_length {
            format!("{}...", &summary[..max_length.saturating_sub(3)])
        } else {
            summary
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkUnderstandingResult {
    pub links: Vec<LinkInfo>,
    pub summaries: Vec<String>,
    pub total_urls: usize,
    pub processed_urls: usize,
    pub errors: Vec<LinkError>,
}

impl LinkUnderstandingResult {
    pub fn format_body(&self) -> String {
        if self.summaries.is_empty() {
            return String::new();
        }

        let header = if self.links.len() == 1 {
            "📎 Link Preview:\n"
        } else {
            &format!("📎 {} Link Previews:\n", self.links.len())
        };

        let mut body = header.to_string();
        for (i, summary) in self.summaries.iter().enumerate() {
            if i > 0 {
                body.push_str("\n---\n");
            }
            body.push_str(summary);
        }

        body
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkError {
    pub url: String,
    pub error: String,
}

pub const DEFAULT_MAX_LINKS: usize = 5;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
pub const DEFAULT_MAX_CONTENT_LENGTH: usize = 10000;
pub const DEFAULT_MAX_SUMMARY_LENGTH: usize = 500;
