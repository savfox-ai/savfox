use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Once};

use dioxus::prelude::*;
use pulldown_cmark::html::push_html;
use pulldown_cmark::{Event, Options, Parser};
use regex_lite::Regex;

const MAX_MARKDOWN_INPUT_CHARS: usize = 140_000;
const MAX_MARKDOWN_CACHE_ENTRIES: usize = 200;
const TRUNCATION_NOTICE: &str = "\n\n> _Message truncated at 140000 chars._";

thread_local! {
    static MARKDOWN_CACHE: RefCell<MarkdownLruCache> =
        RefCell::new(MarkdownLruCache::new(MAX_MARKDOWN_CACHE_ENTRIES));
}

#[derive(Debug, Clone)]
struct CacheEntry {
    source: String,
    html: String,
}

#[derive(Debug)]
struct MarkdownLruCache {
    capacity: usize,
    order: VecDeque<u64>,
    entries: HashMap<u64, CacheEntry>,
}

impl MarkdownLruCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::with_capacity(capacity),
            entries: HashMap::with_capacity(capacity),
        }
    }

    fn get(&mut self, source: &str) -> Option<String> {
        let key = cache_hash(source);
        let html = match self.entries.get(&key) {
            Some(entry) if entry.source == source => entry.html.clone(),
            _ => return None,
        };
        self.touch(key);
        Some(html)
    }

    fn put(&mut self, source: String, html: String) {
        let key = cache_hash(&source);
        self.entries.insert(key, CacheEntry { source, html });
        self.touch(key);
        while self.order.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            }
        }
    }

    fn touch(&mut self, key: u64) {
        if let Some(pos) = self.order.iter().position(|k| *k == key) {
            let _ = self.order.remove(pos);
        }
        self.order.push_back(key);
    }
}

/// Renders markdown content as sanitized HTML.
///
/// Includes:
/// - 200-entry LRU cache to avoid repeat render work
/// - truncation for very large messages (>140k chars)
/// - sanitization pass to strip script tags and event handlers
/// - markdown extensions (tables, task lists, footnotes)
#[component]
pub fn MarkdownRenderer(content: String) -> Element {
    inject_markdown_styles_once();
    let rendered_html = render_markdown_cached(&content);

    rsx! {
        div {
            class: "markdown-content",
            dangerous_inner_html: rendered_html
        }
    }
}

/// Injects the markdown stylesheet into the document head exactly once,
/// instead of emitting an inline `<style>` per rendered message.
fn inject_markdown_styles_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Ok(el) = doc.create_element("style") {
                el.set_inner_html(MARKDOWN_STYLES);
                if let Some(head) = doc.head() {
                    let _ = head.append_child(&el);
                }
            }
        }
    });
}

fn render_markdown_cached(content: &str) -> String {
    let normalized = truncate_markdown_input(content);
    MARKDOWN_CACHE.with(|cache_cell| {
        let mut cache = cache_cell.borrow_mut();
        if let Some(hit) = cache.get(&normalized) {
            return hit;
        }
        let html = render_markdown_html(&normalized);
        cache.put(normalized, html.clone());
        html
    })
}

fn truncate_markdown_input(content: &str) -> String {
    let char_count = content.chars().count();
    if char_count <= MAX_MARKDOWN_INPUT_CHARS {
        return content.to_string();
    }

    let mut truncated = String::with_capacity(MAX_MARKDOWN_INPUT_CHARS + TRUNCATION_NOTICE.len());
    truncated.extend(content.chars().take(MAX_MARKDOWN_INPUT_CHARS));
    truncated.push_str(TRUNCATION_NOTICE);
    truncated
}

fn render_markdown_html(content: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(content, options)
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)));

    let mut html = String::new();
    push_html(&mut html, parser);
    sanitize_html_output(&html)
}

static SCRIPT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").expect("script regex should compile")
});

static EVENT_HANDLER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)\son[a-z0-9_-]+\s*=\s*(".*?"|'.*?'|[^\s>]+)"#)
        .expect("event handler regex should compile")
});

fn sanitize_html_output(html: &str) -> String {
    let no_script = SCRIPT_RE.replace_all(html, "");
    EVENT_HANDLER_RE.replace_all(&no_script, "").into_owned()
}

fn cache_hash(input: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish()
}

const MARKDOWN_STYLES: &str = r#"
    .markdown-content {
        line-height: 1.6;
        word-wrap: break-word;
        overflow-wrap: anywhere;
    }

    .markdown-content p {
        margin: 0 0 0.6em;
    }

    .markdown-content > *:last-child {
        margin-bottom: 0;
    }

    .markdown-content table {
        width: 100%;
        border-collapse: collapse;
        margin: 8px 0;
        display: block;
        overflow-x: auto;
    }

    .markdown-content th,
    .markdown-content td {
        border: 1px solid var(--border);
        padding: 6px 8px;
        text-align: left;
    }

    .markdown-content thead {
        background: var(--bg-secondary);
    }

    .markdown-content ul,
    .markdown-content ol {
        margin: 0 0 0.6em;
        padding-left: 1.2em;
    }

    .markdown-content .task-list-item {
        list-style: none;
    }

    .markdown-content .task-list-item input[type="checkbox"] {
        margin-right: 0.4em;
        vertical-align: middle;
    }

    .markdown-content .footnote-definition {
        margin: 0.4em 0;
        padding: 0.4em 0.6em;
        background: var(--bg-secondary);
        border-radius: 6px;
    }
"#;

#[cfg(test)]
mod tests {
    use super::{
        MAX_MARKDOWN_INPUT_CHARS, MarkdownLruCache, render_markdown_html, sanitize_html_output,
        truncate_markdown_input,
    };

    #[test]
    fn truncates_very_large_input() {
        let long = "a".repeat(MAX_MARKDOWN_INPUT_CHARS + 16);
        let truncated = truncate_markdown_input(&long);
        assert!(truncated.starts_with(&"a".repeat(MAX_MARKDOWN_INPUT_CHARS)));
        assert!(truncated.contains("Message truncated"));
    }

    #[test]
    fn supports_tables_task_lists_and_footnotes() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n\nnote[^1]\n\n[^1]: footnote";
        let html = render_markdown_html(md);
        assert!(html.contains("<table>"));
        assert!(html.contains("type=\"checkbox\""));
        assert!(html.contains("footnote-reference"));
    }

    #[test]
    fn sanitizer_strips_script_tags_and_event_handlers() {
        let html = r#"<p onclick="evil()">ok</p><script>alert(1)</script><div onload='x'>x</div>"#;
        let sanitized = sanitize_html_output(html);
        assert!(!sanitized.contains("<script"));
        assert!(!sanitized.contains("onclick="));
        assert!(!sanitized.contains("onload="));
    }

    #[test]
    fn lru_cache_evicts_oldest_entry() {
        let mut cache = MarkdownLruCache::new(2);
        cache.put("a".to_string(), "1".to_string());
        cache.put("b".to_string(), "2".to_string());
        assert_eq!(cache.get("a").as_deref(), Some("1"));
        cache.put("c".to_string(), "3".to_string());
        assert!(cache.get("b").is_none());
        assert_eq!(cache.get("a").as_deref(), Some("1"));
        assert_eq!(cache.get("c").as_deref(), Some("3"));
    }
}
