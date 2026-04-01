use dioxus::prelude::*;

/// Artifact types that can be rendered in the canvas.
#[derive(Clone, Debug, PartialEq)]
pub enum ArtifactKind {
    Html,
    Svg,
    Mermaid,
    Code { language: String },
}

/// A parsed artifact from chat message content.
#[derive(Clone, Debug, PartialEq)]
pub struct Artifact {
    pub id: Option<String>,
    pub kind: ArtifactKind,
    pub title: Option<String>,
    pub source: String,
}

/// Parse artifacts from message content.
/// Supports:
///   <artifact type="html" id="..." title="...">...</artifact>
///   <canvas type="html">...</canvas>
///   ```html ... ``` (when long enough to be an artifact)
pub fn parse_artifacts(content: &str) -> Vec<Artifact> {
    let mut artifacts = Vec::new();

    // Parse <artifact ...>...</artifact> blocks
    let mut pos = 0;
    while let Some(tag_start) = content[pos..].find("<artifact") {
        let abs_start = pos + tag_start;
        if let Some(tag_end) = content[abs_start..].find('>') {
            let attrs_str = &content[abs_start + 9..abs_start + tag_end];
            if let Some(close) = content[abs_start + tag_end + 1..].find("</artifact>") {
                let inner = &content[abs_start + tag_end + 1..abs_start + tag_end + 1 + close];
                let kind = parse_type_attr(attrs_str);
                let id = parse_attr(attrs_str, "id");
                let title = parse_attr(attrs_str, "title");
                artifacts.push(Artifact {
                    id,
                    kind,
                    title,
                    source: inner.to_string(),
                });
                pos = abs_start + tag_end + 1 + close + 11;
                continue;
            }
        }
        pos = abs_start + 9;
    }

    // Parse <canvas ...>...</canvas> blocks
    pos = 0;
    while let Some(tag_start) = content[pos..].find("<canvas") {
        let abs_start = pos + tag_start;
        // Ensure it's our canvas tag, not HTML5 <canvas>
        if let Some(tag_end) = content[abs_start..].find('>') {
            let attrs_str = &content[abs_start + 7..abs_start + tag_end];
            if attrs_str.contains("type=") {
                if let Some(close) = content[abs_start + tag_end + 1..].find("</canvas>") {
                    let inner = &content[abs_start + tag_end + 1..abs_start + tag_end + 1 + close];
                    let kind = parse_type_attr(attrs_str);
                    let id = parse_attr(attrs_str, "id");
                    let title = parse_attr(attrs_str, "title");
                    artifacts.push(Artifact {
                        id,
                        kind,
                        title,
                        source: inner.to_string(),
                    });
                    pos = abs_start + tag_end + 1 + close + 9;
                    continue;
                }
            }
        }
        pos = abs_start + 7;
    }

    artifacts
}

/// Strip artifact/canvas blocks from content.
pub fn strip_artifacts(content: &str) -> String {
    let mut result = content.to_string();

    // Remove <artifact>...</artifact>
    while let Some(start) = result.find("<artifact") {
        if let Some(end) = result[start..].find("</artifact>") {
            result.replace_range(start..start + end + 11, "");
        } else {
            break;
        }
    }

    // Remove <canvas type=...>...</canvas>
    while let Some(start) = result.find("<canvas") {
        let after = &result[start + 7..];
        if let Some(tag_end) = after.find('>') {
            let attrs = &after[..tag_end];
            if attrs.contains("type=") {
                if let Some(close) = result[start + 7 + tag_end + 1..].find("</canvas>") {
                    let end_pos = start + 7 + tag_end + 1 + close + 9;
                    result.replace_range(start..end_pos, "");
                    continue;
                }
            }
        }
        break;
    }

    result.trim().to_string()
}

fn parse_type_attr(attrs: &str) -> ArtifactKind {
    let type_val = parse_attr(attrs, "type").unwrap_or_default();
    match type_val.as_str() {
        "html" => ArtifactKind::Html,
        "svg" => ArtifactKind::Svg,
        "mermaid" => ArtifactKind::Mermaid,
        other if !other.is_empty() => ArtifactKind::Code {
            language: other.to_string(),
        },
        _ => ArtifactKind::Html,
    }
}

fn parse_attr(attrs: &str, name: &str) -> Option<String> {
    let patterns = [format!("{name}=\""), format!("{name}='")];
    for pattern in &patterns {
        if let Some(start) = attrs.find(pattern.as_str()) {
            let val_start = start + pattern.len();
            let quote = pattern.chars().last().unwrap();
            if let Some(end) = attrs[val_start..].find(quote) {
                return Some(attrs[val_start..val_start + end].to_string());
            }
        }
    }
    None
}

/// Render an artifact in a sandboxed iframe.
#[component]
pub fn CanvasRenderer(artifact: Artifact, on_expand: Option<EventHandler<String>>) -> Element {
    let mut is_expanded = use_signal(|| false);
    let kind_label = match &artifact.kind {
        ArtifactKind::Html => "HTML",
        ArtifactKind::Svg => "SVG",
        ArtifactKind::Mermaid => "Mermaid",
        ArtifactKind::Code { language } => language.as_str(),
    };
    let title = artifact.title.as_deref().unwrap_or(kind_label).to_string();

    let source = artifact.source.clone();
    let source_copy = artifact.source.clone();
    let source_open = artifact.source.clone();

    // Build the HTML for the iframe
    let iframe_html = build_iframe_content(&artifact);
    let iframe_src = format!(
        "data:text/html;charset=utf-8,{}",
        js_sys::encode_uri_component(&iframe_html)
    );

    let container_style = if is_expanded() {
        "position:fixed;inset:0;z-index:100;background:linear-gradient(180deg,color-mix(in srgb,var(--bg-secondary) 88%, black 12%) 0%,var(--bg-primary) 100%);display:flex;flex-direction:column;padding:12px;"
    } else {
        ""
    };

    rsx! {
        div { class: "canvas-artifact", style: "{container_style}",
            // Toolbar
            div { class: "canvas-artifact__toolbar",
                div { class: "canvas-artifact__info",
                    span { class: "canvas-artifact__type-badge", "{kind_label}" }
                    span { class: "canvas-artifact__title", "{title}" }
                }
                div { class: "canvas-artifact__actions",
                    button {
                        class: "canvas-artifact__btn",
                        title: "Copy source",
                        onclick: move |_| {
                            copy_to_clipboard(&source_copy);
                        },
                        "Copy"
                    }
                    button {
                        class: "canvas-artifact__btn",
                        title: "Open in new tab",
                        onclick: move |_| {
                            open_in_new_tab(&source_open);
                        },
                        "Open"
                    }
                    if let Some(ref handler) = on_expand {
                        {
                            let h = handler.clone();
                            let s = source.clone();
                            rsx! {
                                button {
                                    class: "canvas-artifact__btn",
                                    title: "Expand in sidebar",
                                    onclick: move |_| h.call(s.clone()),
                                    "Sidebar"
                                }
                            }
                        }
                    }
                    button {
                        class: "canvas-artifact__btn",
                        title: if is_expanded() { "Minimize" } else { "Fullscreen" },
                        onclick: move |_| is_expanded.toggle(),
                        if is_expanded() { "Exit" } else { "Full" }
                    }
                }
            }

            // Sandboxed iframe
            iframe {
                class: "canvas-artifact__frame",
                src: "{iframe_src}",
                style: if is_expanded() { "flex:1;" } else { "" },
            }
        }
    }
}

fn build_iframe_content(artifact: &Artifact) -> String {
    match &artifact.kind {
        ArtifactKind::Html => {
            format!(
                r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
  :root {{
    color-scheme: dark;
    --bg: #14090d;
    --panel: rgba(38, 20, 27, 0.88);
    --panel-strong: rgba(54, 30, 38, 0.92);
    --text: #fff5ee;
    --muted: #e7c5ba;
    --accent: #ef6b6f;
    --ember: #ff9a6c;
    --ornament: #f6c58d;
    --stroke: rgba(255, 239, 233, 0.12);
  }}
  * {{ box-sizing: border-box; }}
  html, body {{ min-height: 100%; }}
  body {{
    margin: 0;
    padding: 20px;
    font-family: "Manrope", "Noto Sans SC", system-ui, sans-serif;
    color: var(--text);
    background:
      radial-gradient(circle at 14% 12%, rgba(239, 107, 111, 0.18) 0%, transparent 34%),
      radial-gradient(circle at 82% 16%, rgba(246, 197, 141, 0.18) 0%, transparent 28%),
      linear-gradient(180deg, #1a0d13 0%, var(--bg) 100%);
  }}
  .canvas-shell {{
    min-height: calc(100vh - 40px);
    padding: 24px;
    border-radius: 24px;
    border: 1px solid var(--stroke);
    background:
      linear-gradient(180deg, rgba(60, 34, 43, 0.12) 0%, transparent 44%),
      linear-gradient(180deg, var(--panel-strong) 0%, var(--panel) 100%);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), 0 28px 64px rgba(9, 3, 5, 0.28);
  }}
  a {{ color: var(--ornament); }}
  pre, code {{ font-family: "JetBrains Mono", "SF Mono", monospace; }}
  pre {{
    margin: 0;
    padding: 18px;
    border-radius: 18px;
    border: 1px solid var(--stroke);
    background: rgba(24, 12, 17, 0.82);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.04);
  }}
</style>
</head>
<body><div class="canvas-shell">{}</div></body>
</html>"#,
                artifact.source
            )
        }
        ArtifactKind::Svg => {
            format!(
                r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  body {{
    margin: 0;
    padding: 20px;
    display: flex;
    justify-content: center;
    align-items: center;
    min-height: 100vh;
    background:
      radial-gradient(circle at 20% 14%, rgba(239, 107, 111, 0.18) 0%, transparent 34%),
      radial-gradient(circle at 80% 18%, rgba(246, 197, 141, 0.18) 0%, transparent 28%),
      linear-gradient(180deg, #1a0d13 0%, #14090d 100%);
  }}
  .canvas-shell {{
    width: min(100%, 1040px);
    padding: 24px;
    border-radius: 24px;
    border: 1px solid rgba(255, 239, 233, 0.12);
    background: linear-gradient(180deg, rgba(56, 32, 40, 0.88) 0%, rgba(30, 17, 23, 0.84) 100%);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), 0 28px 64px rgba(9, 3, 5, 0.28);
  }}
  svg {{ max-width: 100%; height: auto; }}
</style>
</head>
<body><div class="canvas-shell">{}</div></body>
</html>"#,
                artifact.source
            )
        }
        ArtifactKind::Mermaid => {
            format!(
                r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<script src="https://cdn.jsdelivr.net/npm/mermaid/dist/mermaid.min.js"></script>
<style>
  body {{
    margin: 0;
    padding: 20px;
    background:
      radial-gradient(circle at 20% 14%, rgba(239, 107, 111, 0.18) 0%, transparent 34%),
      radial-gradient(circle at 80% 18%, rgba(246, 197, 141, 0.18) 0%, transparent 28%),
      linear-gradient(180deg, #1a0d13 0%, #14090d 100%);
    color: #fff5ee;
    font-family: "Manrope", "Noto Sans SC", system-ui, sans-serif;
  }}
  .canvas-shell {{
    min-height: calc(100vh - 40px);
    padding: 24px;
    border-radius: 24px;
    border: 1px solid rgba(255, 239, 233, 0.12);
    background: linear-gradient(180deg, rgba(56, 32, 40, 0.88) 0%, rgba(30, 17, 23, 0.84) 100%);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), 0 28px 64px rgba(9, 3, 5, 0.28);
  }}
  .mermaid {{
    border-radius: 18px;
    border: 1px solid rgba(255, 239, 233, 0.10);
    background: rgba(25, 12, 17, 0.78);
    padding: 18px;
  }}
</style>
</head>
<body>
<div class="canvas-shell">
<pre class="mermaid">{}</pre>
</div>
<script>
mermaid.initialize({{
  startOnLoad: true,
  theme: 'base',
  themeVariables: {{
    background: '#14090d',
    primaryColor: '#2b1820',
    primaryBorderColor: '#ef6b6f',
    primaryTextColor: '#fff5ee',
    lineColor: '#f6c58d',
    secondaryColor: '#341e27',
    tertiaryColor: '#2a171e'
  }}
}});
</script>
</body>
</html>"#,
                artifact.source
            )
        }
        ArtifactKind::Code { language } => {
            let escaped = artifact
                .source
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            format!(
                r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  body {{
    margin: 0;
    padding: 20px;
    background:
      radial-gradient(circle at 20% 14%, rgba(239, 107, 111, 0.18) 0%, transparent 34%),
      radial-gradient(circle at 80% 18%, rgba(246, 197, 141, 0.18) 0%, transparent 28%),
      linear-gradient(180deg, #1a0d13 0%, #14090d 100%);
    color: #fff5ee;
    font-family: "Manrope", "Noto Sans SC", system-ui, sans-serif;
  }}
  .canvas-shell {{
    min-height: calc(100vh - 40px);
    padding: 24px;
    border-radius: 24px;
    border: 1px solid rgba(255, 239, 233, 0.12);
    background: linear-gradient(180deg, rgba(56, 32, 40, 0.88) 0%, rgba(30, 17, 23, 0.84) 100%);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), 0 28px 64px rgba(9, 3, 5, 0.28);
  }}
  pre {{
    white-space: pre-wrap;
    word-break: break-word;
    font-family: "JetBrains Mono", "SF Mono", monospace;
    font-size: 13px;
    line-height: 1.7;
    margin: 0;
    padding: 18px;
    border-radius: 18px;
    border: 1px solid rgba(255, 239, 233, 0.10);
    background: rgba(25, 12, 17, 0.78);
  }}
  .lang {{
    color: #f6c58d;
    font-size: 11px;
    margin-bottom: 10px;
    text-transform: uppercase;
    letter-spacing: 0.16em;
  }}
</style>
</head>
<body>
<div class="canvas-shell">
<div class="lang">{language}</div>
<pre><code>{escaped}</code></pre>
</div>
</body>
</html>"#
            )
        }
    }
}

fn copy_to_clipboard(text: &str) {
    if let Some(window) = web_sys::window() {
        let clipboard = window.navigator().clipboard();
        let _ = clipboard.write_text(text);
    }
}

fn open_in_new_tab(source: &str) {
    if let Some(window) = web_sys::window() {
        let html = format!(
            r#"<!DOCTYPE html><html><head><meta charset='utf-8'><style>body{{margin:0;padding:20px;font-family:"Manrope","Noto Sans SC",system-ui,sans-serif;background:radial-gradient(circle at 20% 14%,rgba(239,107,111,.18) 0%,transparent 34%),radial-gradient(circle at 80% 18%,rgba(246,197,141,.18) 0%,transparent 28%),linear-gradient(180deg,#1a0d13 0%,#14090d 100%);color:#fff5ee}}.canvas-shell{{min-height:calc(100vh - 40px);padding:24px;border-radius:24px;border:1px solid rgba(255,239,233,.12);background:linear-gradient(180deg,rgba(56,32,40,.88) 0%,rgba(30,17,23,.84) 100%);box-shadow:inset 0 1px 0 rgba(255,255,255,.08),0 28px 64px rgba(9,3,5,.28)}}</style></head><body><div class='canvas-shell'>{source}</div></body></html>"#
        );
        let blob_opts = web_sys::BlobPropertyBag::new();
        blob_opts.set_type("text/html");
        if let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(
            &js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(&html)),
            &blob_opts,
        ) {
            if let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) {
                let _ = window.open_with_url_and_target(&url, "_blank");
            }
        }
    }
}
