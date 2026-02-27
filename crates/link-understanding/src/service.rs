use chrono::Utc;
use futures::future::join_all;
use tracing::{debug, info, warn};
use url::Url;

use crate::detector::LinkDetector;
use crate::fetcher::{ContentFetcher, FetchedContent};
use crate::opengraph::{OpenGraph, OpenGraphFetcher};
use crate::types::{LinkError, LinkInfo, LinkUnderstandingConfig, LinkUnderstandingResult};

pub struct LinkUnderstandingService {
    config: LinkUnderstandingConfig,
    detector: LinkDetector,
    opengraph_fetcher: OpenGraphFetcher,
    content_fetcher: ContentFetcher,
}

impl LinkUnderstandingService {
    pub fn new(config: LinkUnderstandingConfig) -> Self {
        let detector = LinkDetector::new(config.clone());
        let opengraph_fetcher = OpenGraphFetcher::new(config.clone());
        let content_fetcher = ContentFetcher::new(config.clone());

        Self {
            config,
            detector,
            opengraph_fetcher,
            content_fetcher,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(LinkUnderstandingConfig::default())
    }

    pub async fn understand(&self, text: &str) -> LinkUnderstandingResult {
        if !self.config.enabled {
            return LinkUnderstandingResult {
                links: Vec::new(),
                summaries: Vec::new(),
                total_urls: 0,
                processed_urls: 0,
                errors: Vec::new(),
            };
        }

        let urls = self.detector.extract(text);
        let total_urls = urls.len();

        if urls.is_empty() {
            return LinkUnderstandingResult {
                links: Vec::new(),
                summaries: Vec::new(),
                total_urls: 0,
                processed_urls: 0,
                errors: Vec::new(),
            };
        }

        info!("Processing {} URLs for link understanding", urls.len());

        let futures: Vec<_> = urls.iter().map(|url| self.process_url(url)).collect();

        let results = join_all(futures).await;

        let mut links = Vec::new();
        let mut errors = Vec::new();
        let mut processed = 0;

        for result in results {
            match result {
                Ok(link_info) => {
                    processed += 1;
                    links.push(link_info);
                }
                Err(e) => {
                    errors.push(e);
                }
            }
        }

        let summaries: Vec<String> = links
            .iter()
            .map(|l| l.format_summary(self.config.max_summary_length))
            .collect();

        LinkUnderstandingResult {
            links,
            summaries,
            total_urls,
            processed_urls: processed,
            errors,
        }
    }

    async fn process_url(&self, url: &str) -> Result<LinkInfo, LinkError> {
        let domain = Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string());

        let mut link_info = LinkInfo {
            url: url.to_string(),
            domain,
            title: None,
            description: None,
            image: None,
            site_name: None,
            content: None,
            content_type: None,
            fetched_at: Utc::now(),
        };

        if self.config.include_opengraph {
            match self.opengraph_fetcher.fetch(url).await {
                Ok(og) => {
                    link_info.title = link_info.title.or(og.title);
                    link_info.description = link_info.description.or(og.description);
                    link_info.image = link_info.image.or(og.image);
                    link_info.site_name = link_info.site_name.or(og.site_name);
                }
                Err(e) => {
                    debug!("OpenGraph fetch failed for {}: {}", url, e);
                }
            }
        }

        if self.config.include_content {
            match self.content_fetcher.fetch(url).await {
                Ok(content) => {
                    link_info.title = link_info.title.or(content.title);
                    link_info.content = Some(content.text);
                    link_info.content_type = content.content_type;
                }
                Err(e) => {
                    debug!("Content fetch failed for {}: {}", url, e);
                }
            }
        }

        if link_info.title.is_none()
            && link_info.description.is_none()
            && link_info.content.is_none()
        {
            return Err(LinkError {
                url: url.to_string(),
                error: "No content could be extracted".to_string(),
            });
        }

        Ok(link_info)
    }

    pub fn format_link_body(&self, result: &LinkUnderstandingResult) -> String {
        result.format_body()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_disabled() {
        let mut config = LinkUnderstandingConfig::default();
        config.enabled = false;

        let service = LinkUnderstandingService::new(config);
        let result = service.understand("Check out https://example.com").await;

        assert_eq!(result.total_urls, 0);
        assert!(result.links.is_empty());
    }
}
