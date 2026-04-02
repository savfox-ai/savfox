use std::collections::HashMap;

/// Advanced template engine for auto-reply messages.
///
/// Supports:
/// - Variable substitution: `{{var}}`
/// - Conditionals: `{{#if var}}...{{/if}}`
/// - Defaults: `{{var|default}}`
/// - Loops: `{{#each items}}...{{/each}}`
/// - Escaping: `\{{literal}}`
#[derive(Debug)]
pub(crate) struct TemplateEngine {
    helpers: HashMap<String, Box<dyn TemplateHelper>>,
}

/// A template helper function.
pub(crate) trait TemplateHelper: std::fmt::Debug + Send + Sync {
    fn render(&self, args: &[&str], context: &TemplateContext) -> String;
}

/// Context variables available during template rendering.
#[derive(Debug, Clone)]
pub(crate) struct TemplateContext {
    /// Variable values.
    pub(crate) vars: HashMap<String, String>,
    /// List variables (for {{#each}}).
    pub(crate) lists: HashMap<String, Vec<String>>,
}

impl TemplateContext {
    pub(crate) fn new() -> Self {
        Self {
            vars: HashMap::new(),
            lists: HashMap::new(),
        }
    }

    /// Create a context from message metadata.
    pub(crate) fn from_message(user_id: &str, channel: &str, message: &str) -> Self {
        let mut vars = HashMap::new();
        vars.insert("user".to_owned(), user_id.to_owned());
        vars.insert("channel".to_owned(), channel.to_owned());
        vars.insert("message".to_owned(), message.to_owned());
        vars.insert("timestamp".to_owned(), chrono::Utc::now().to_rfc3339());
        vars.insert(
            "date".to_owned(),
            chrono::Utc::now().format("%Y-%m-%d").to_string(),
        );
        vars.insert(
            "time".to_owned(),
            chrono::Utc::now().format("%H:%M:%S").to_string(),
        );
        Self {
            vars,
            lists: HashMap::new(),
        }
    }

    /// Set a variable.
    pub(crate) fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    /// Set a list variable.
    pub(crate) fn set_list(&mut self, key: impl Into<String>, values: Vec<String>) {
        self.lists.insert(key.into(), values);
    }

    /// Get a variable value.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }
}

impl TemplateEngine {
    pub(crate) fn new() -> Self {
        Self {
            helpers: HashMap::new(),
        }
    }

    /// Render a template string with the given context.
    pub(crate) fn render(&self, template: &str, ctx: &TemplateContext) -> String {
        let mut result = String::with_capacity(template.len());
        let mut chars = template.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '\\' && chars.peek() == Some(&'{') {
                // Escaped brace
                if let Some(next) = chars.next() {
                    result.push(next);
                }
                continue;
            }

            if ch == '{' && chars.peek() == Some(&'{') {
                chars.next(); // consume second {
                let mut tag = String::new();
                let mut depth = 1;

                while let Some(c) = chars.next() {
                    if c == '{' && chars.peek() == Some(&'{') {
                        depth += 1;
                        tag.push(c);
                        if let Some(n) = chars.next() {
                            tag.push(n);
                        }
                    } else if c == '}' && chars.peek() == Some(&'}') {
                        depth -= 1;
                        if depth == 0 {
                            chars.next(); // consume second }
                            break;
                        }
                        tag.push(c);
                        if let Some(n) = chars.next() {
                            tag.push(n);
                        }
                    } else {
                        tag.push(c);
                    }
                }

                let tag = tag.trim();

                if let Some(stripped) = tag.strip_prefix("#if ") {
                    // Conditional block
                    let var_name = stripped.trim();
                    let end_tag = "{{/if}}".to_owned();
                    let block_content = extract_until(&mut chars, &end_tag);
                    let has_value = ctx.get(var_name).is_some_and(|v| !v.is_empty());
                    if has_value {
                        result.push_str(&self.render(&block_content, ctx));
                    }
                } else if let Some(stripped) = tag.strip_prefix("#each ") {
                    // Loop block
                    let list_name = stripped.trim();
                    let end_tag = "{{/each}}".to_owned();
                    let block_content = extract_until(&mut chars, &end_tag);
                    if let Some(items) = ctx.lists.get(list_name) {
                        for (i, item) in items.iter().enumerate() {
                            let mut inner_ctx = ctx.clone();
                            inner_ctx.set("item", item.as_str());
                            inner_ctx.set("index", i.to_string());
                            result.push_str(&self.render(&block_content, &inner_ctx));
                        }
                    }
                } else if tag.contains('|') {
                    // Variable with default
                    let parts: Vec<&str> = tag.splitn(2, '|').collect();
                    let var_name = parts[0].trim();
                    let default = parts.get(1).map(|s| s.trim()).unwrap_or("");
                    result.push_str(ctx.get(var_name).unwrap_or(default));
                } else {
                    // Simple variable substitution
                    if let Some(helper) = self.helpers.get(tag) {
                        result.push_str(&helper.render(&[], ctx));
                    } else {
                        result.push_str(ctx.get(tag).unwrap_or(""));
                    }
                }
            } else {
                result.push(ch);
            }
        }

        result
    }
}

/// Extract content from chars until the end tag is found.
fn extract_until(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, end_tag: &str) -> String {
    let mut content = String::new();
    let end_bytes = end_tag.as_bytes();

    loop {
        if content.ends_with(end_tag) {
            content.truncate(content.len() - end_tag.len());
            return content;
        }

        match chars.next() {
            Some(c) => content.push(c),
            None => return content,
        }

        // Quick check: if content is long enough and ends match
        if content.len() >= end_tag.len() {
            let tail = &content.as_bytes()[content.len() - end_bytes.len()..];
            if tail == end_bytes {
                content.truncate(content.len() - end_tag.len());
                return content;
            }
        }
    }
}

/// Prefix injection: prepend a system message before user content.
#[derive(Debug, Clone)]
pub(crate) struct PrefixInjection {
    pub(crate) prefix: String,
    pub(crate) separator: String,
}

impl PrefixInjection {
    pub(crate) fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            separator: "\n\n".to_owned(),
        }
    }

    pub(crate) fn with_separator(mut self, sep: impl Into<String>) -> Self {
        self.separator = sep.into();
        self
    }

    pub(crate) fn apply(&self, message: &str) -> String {
        format!("{}{}{}", self.prefix, self.separator, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_variable() {
        let engine = TemplateEngine::new();
        let mut ctx = TemplateContext::new();
        ctx.set("name", "World");
        assert_eq!(engine.render("Hello {{name}}!", &ctx), "Hello World!");
    }

    #[test]
    fn test_missing_variable() {
        let engine = TemplateEngine::new();
        let ctx = TemplateContext::new();
        assert_eq!(engine.render("Hello {{name}}!", &ctx), "Hello !");
    }

    #[test]
    fn test_default_value() {
        let engine = TemplateEngine::new();
        let ctx = TemplateContext::new();
        assert_eq!(
            engine.render("Hello {{name|stranger}}!", &ctx),
            "Hello stranger!"
        );
    }

    #[test]
    fn test_default_with_existing_value() {
        let engine = TemplateEngine::new();
        let mut ctx = TemplateContext::new();
        ctx.set("name", "Alice");
        assert_eq!(
            engine.render("Hello {{name|stranger}}!", &ctx),
            "Hello Alice!"
        );
    }

    #[test]
    fn test_conditional_true() {
        let engine = TemplateEngine::new();
        let mut ctx = TemplateContext::new();
        ctx.set("premium", "yes");
        assert_eq!(
            engine.render("{{#if premium}}VIP access{{/if}}", &ctx),
            "VIP access"
        );
    }

    #[test]
    fn test_conditional_false() {
        let engine = TemplateEngine::new();
        let ctx = TemplateContext::new();
        assert_eq!(engine.render("{{#if premium}}VIP access{{/if}}", &ctx), "");
    }

    #[test]
    fn test_each_loop() {
        let engine = TemplateEngine::new();
        let mut ctx = TemplateContext::new();
        ctx.set_list("items", vec!["apple".to_owned(), "banana".to_owned()]);
        assert_eq!(
            engine.render("{{#each items}}- {{item}}\n{{/each}}", &ctx),
            "- apple\n- banana\n"
        );
    }

    #[test]
    fn test_from_message() {
        let engine = TemplateEngine::new();
        let ctx = TemplateContext::from_message("user123", "general", "hello");
        let result = engine.render("{{user}} in {{channel}}: {{message}}", &ctx);
        assert_eq!(result, "user123 in general: hello");
    }

    #[test]
    fn test_prefix_injection() {
        let prefix = PrefixInjection::new("[System] You are a helpful bot.");
        assert_eq!(
            prefix.apply("Hello!"),
            "[System] You are a helpful bot.\n\nHello!"
        );
    }

    #[test]
    fn test_prefix_injection_custom_separator() {
        let prefix = PrefixInjection::new("[Bot]").with_separator(" | ");
        assert_eq!(prefix.apply("Hello!"), "[Bot] | Hello!");
    }
}
