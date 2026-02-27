use regex::Regex;
use url::Url;

use crate::types::{DEFAULT_MAX_LINKS, LinkUnderstandingConfig};

pub struct LinkDetector {
    bare_link_re: Regex,
    markdown_link_re: Regex,
    config: LinkUnderstandingConfig,
}

impl LinkDetector {
    pub fn new(config: LinkUnderstandingConfig) -> Self {
        Self {
            bare_link_re: Regex::new(r"https?://\S+").expect("invalid regex"),
            markdown_link_re: Regex::new(r"\[[^\]]*\]\((https?://\S+?)\)").expect("invalid regex"),
            config,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(LinkUnderstandingConfig::default())
    }

    pub fn extract(&self, text: &str) -> Vec<String> {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }

        let sanitized = self.strip_markdown_links(text);

        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();

        for cap in self.bare_link_re.captures_iter(&sanitized) {
            if let Some(m) = cap.get(0) {
                let raw = m.as_str().trim();

                if !self.is_allowed_url(raw) {
                    continue;
                }

                if seen.contains(raw) {
                    continue;
                }

                seen.insert(raw.to_string());
                results.push(raw.to_string());

                if results.len() >= self.config.max_links {
                    break;
                }
            }
        }

        results
    }

    fn strip_markdown_links(&self, text: &str) -> String {
        self.markdown_link_re.replace_all(text, " ").to_string()
    }

    fn is_allowed_url(&self, raw: &str) -> bool {
        let parsed = match Url::parse(raw) {
            Ok(u) => u,
            Err(_) => return false,
        };

        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return false;
        }

        if let Some(host) = parsed.host_str() {
            if self
                .config
                .blocked_domains
                .iter()
                .any(|d| host == d || host.ends_with(&format!(".{}", d)))
            {
                return false;
            }

            if let Some(ref allowed) = self.config.allowed_domains {
                if !allowed
                    .iter()
                    .any(|d| host == d || host.ends_with(&format!(".{}", d)))
                {
                    return false;
                }
            }
        }

        true
    }
}

pub fn extract_links(text: &str) -> Vec<String> {
    LinkDetector::with_defaults().extract(text)
}

pub fn extract_links_with_config(text: &str, config: LinkUnderstandingConfig) -> Vec<String> {
    LinkDetector::new(config).extract(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_bare_links() {
        let text = "Check out https://example.com and http://test.org/path";
        let links = extract_links(text);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0], "https://example.com");
    }

    #[test]
    fn test_strip_markdown_links() {
        let text = "See [this link](https://example.com) and https://other.com";
        let links = extract_links(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0], "https://other.com");
    }

    #[test]
    fn test_blocked_domains() {
        let text = "Visit https://localhost/test and https://example.com";
        let links = extract_links(text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0], "https://example.com");
    }

    #[test]
    fn test_max_links() {
        let text = "Links: https://a.com https://b.com https://c.com https://d.com https://e.com https://f.com";
        let links = extract_links(text);
        assert_eq!(links.len(), DEFAULT_MAX_LINKS);
    }
}
