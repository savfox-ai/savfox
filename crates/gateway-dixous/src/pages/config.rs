use std::collections::HashMap;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::ws::WsRpc;

// ============== Diff Types ==============

#[derive(Clone, Debug)]
struct DiffEntry {
    path: String,
    kind: DiffKind,
    old_value: Option<serde_json::Value>,
    new_value: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
enum DiffKind {
    Added,
    Removed,
    Changed,
}

fn compute_deep_diff(
    original: &HashMap<String, serde_json::Value>,
    current: &HashMap<String, serde_json::Value>,
) -> Vec<DiffEntry> {
    let mut entries = Vec::new();
    let mut all_keys: Vec<&String> = original.keys().chain(current.keys()).collect();
    all_keys.sort();
    all_keys.dedup();

    for key in all_keys {
        let orig = original.get(key);
        let curr = current.get(key);
        match (orig, curr) {
            (Some(o), Some(c)) => {
                if o != c {
                    diff_values(key, o, c, &mut entries);
                }
            }
            (Some(o), None) => {
                entries.push(DiffEntry {
                    path: key.clone(),
                    kind: DiffKind::Removed,
                    old_value: Some(o.clone()),
                    new_value: None,
                });
            }
            (None, Some(c)) => {
                entries.push(DiffEntry {
                    path: key.clone(),
                    kind: DiffKind::Added,
                    old_value: None,
                    new_value: Some(c.clone()),
                });
            }
            (None, None) => {}
        }
    }
    entries
}

fn diff_values(
    prefix: &str,
    old: &serde_json::Value,
    new: &serde_json::Value,
    entries: &mut Vec<DiffEntry>,
) {
    if old == new {
        return;
    }
    match (old, new) {
        (serde_json::Value::Object(old_map), serde_json::Value::Object(new_map)) => {
            let mut all_keys: Vec<&String> = old_map.keys().chain(new_map.keys()).collect();
            all_keys.sort();
            all_keys.dedup();

            for key in all_keys {
                let child_path = format!("{}.{}", prefix, key);
                match (old_map.get(key), new_map.get(key)) {
                    (Some(ov), Some(nv)) => {
                        diff_values(&child_path, ov, nv, entries);
                    }
                    (Some(ov), None) => {
                        entries.push(DiffEntry {
                            path: child_path,
                            kind: DiffKind::Removed,
                            old_value: Some(ov.clone()),
                            new_value: None,
                        });
                    }
                    (None, Some(nv)) => {
                        entries.push(DiffEntry {
                            path: child_path,
                            kind: DiffKind::Added,
                            old_value: None,
                            new_value: Some(nv.clone()),
                        });
                    }
                    (None, None) => {}
                }
            }
        }
        _ => {
            entries.push(DiffEntry {
                path: prefix.to_string(),
                kind: DiffKind::Changed,
                old_value: Some(old.clone()),
                new_value: Some(new.clone()),
            });
        }
    }
}

// ============== Validation ==============

#[derive(Clone, Debug, PartialEq)]
enum ValidityState {
    Valid,
    Invalid(usize),
    Unknown,
}

fn validate_config(
    form_value: &HashMap<String, serde_json::Value>,
    schema_props: &HashMap<String, serde_json::Value>,
) -> ValidityState {
    if schema_props.is_empty() {
        return ValidityState::Unknown;
    }
    let mut error_count = 0;
    for (key, schema) in schema_props {
        let value = form_value.get(key);
        error_count += validate_value_against_schema(value, schema);
    }
    if error_count == 0 {
        ValidityState::Valid
    } else {
        ValidityState::Invalid(error_count)
    }
}

fn validate_value_against_schema(
    value: Option<&serde_json::Value>,
    schema: &serde_json::Value,
) -> usize {
    let resolved_schema = resolve_schema_variant(schema, value);
    let field_type = schema_primary_type(&resolved_schema).unwrap_or_else(|| "object".to_string());
    let required_fields: Vec<String> = resolved_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut errors = 0;

    if value.is_some_and(|v| v.is_null()) {
        if !schema_is_nullable(&resolved_schema) {
            errors += 1;
        }
        return errors;
    }

    match field_type.as_str() {
        "object" => {
            if let Some(props) = resolved_schema
                .get("properties")
                .and_then(|p| p.as_object())
            {
                let val_obj = value.and_then(|v| v.as_object());
                for req in &required_fields {
                    let field_val = val_obj.and_then(|o| o.get(req));
                    if is_empty_value(field_val) {
                        errors += 1;
                    }
                }
                // Recurse into nested properties
                if let Some(obj) = val_obj {
                    for (prop_key, prop_schema) in props {
                        let child_val = obj.get(prop_key);
                        errors += validate_value_against_schema(child_val, prop_schema);
                    }
                }
            }
        }
        "array" => {
            if let Some(v) = value {
                if !v.is_array() {
                    errors += 1;
                }
            }
        }
        "number" | "integer" => {
            if let Some(v) = value {
                match v {
                    serde_json::Value::Number(_) => {}
                    serde_json::Value::String(s) => {
                        if !s.is_empty() && s.parse::<f64>().is_err() {
                            errors += 1;
                        }
                    }
                    _ => {
                        errors += 1;
                    }
                }
            }
        }
        "boolean" => {
            if let Some(v) = value {
                match v {
                    serde_json::Value::Bool(_) => {}
                    _ => {
                        errors += 1;
                    }
                }
            }
        }
        "string" => {
            if let Some(v) = value {
                if !v.is_string() {
                    errors += 1;
                }
            }
        }
        _ => {}
    }

    errors
}

fn is_empty_value(value: Option<&serde_json::Value>) -> bool {
    match value {
        None => true,
        Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.is_empty(),
        Some(serde_json::Value::Array(a)) => a.is_empty(),
        Some(serde_json::Value::Object(o)) => o.is_empty(),
        _ => false,
    }
}

// ============== Types ==============

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JsonSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    schema_type: Option<SchemaType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    properties: Option<HashMap<String, JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<Box<JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enum_values: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_properties: Option<Box<JsonSchema>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum SchemaType {
    Single(String),
    Multiple(Vec<String>),
}

impl JsonSchema {
    fn schema_type(&self) -> Option<&str> {
        match &self.schema_type {
            Some(SchemaType::Single(t)) => Some(t.as_str()),
            Some(SchemaType::Multiple(types)) => types.first().map(|s| s.as_str()),
            None => None,
        }
    }
}

#[derive(Clone, Copy)]
struct SectionMeta {
    key: &'static str,
    label: &'static str,
    description: &'static str,
    icon: &'static str,
}

const SECTIONS: &[SectionMeta] = &[
    SectionMeta {
        key: "env",
        label: "Environment",
        description: "Environment variables passed to the gateway process",
        icon: "⚙",
    },
    SectionMeta {
        key: "update",
        label: "Updates",
        description: "Auto-update settings and release channel",
        icon: "⬇",
    },
    SectionMeta {
        key: "agents",
        label: "Agents",
        description: "Agent configurations, models, and identities",
        icon: "🤖",
    },
    SectionMeta {
        key: "auth",
        label: "Authentication",
        description: "API keys and authentication profiles",
        icon: "🔐",
    },
    SectionMeta {
        key: "channels",
        label: "Channels",
        description: "Messaging channels (Telegram, Discord, etc.)",
        icon: "💬",
    },
    SectionMeta {
        key: "messages",
        label: "Messages",
        description: "Message handling and routing settings",
        icon: "✉",
    },
    SectionMeta {
        key: "commands",
        label: "Commands",
        description: "Custom slash commands",
        icon: "⌘",
    },
    SectionMeta {
        key: "hooks",
        label: "Hooks",
        description: "Webhooks and event hooks",
        icon: "🔗",
    },
    SectionMeta {
        key: "skills",
        label: "Skills",
        description: "Skill packs and capabilities",
        icon: "★",
    },
    SectionMeta {
        key: "tools",
        label: "Tools",
        description: "Tool configurations (browser, search, etc.)",
        icon: "🔧",
    },
    SectionMeta {
        key: "gateway",
        label: "Gateway",
        description: "Gateway server settings (port, auth, binding)",
        icon: "🌐",
    },
    SectionMeta {
        key: "wizard",
        label: "Connect Provider",
        description: "Setup wizard state and history",
        icon: "🧙",
    },
    SectionMeta {
        key: "logging",
        label: "Logging",
        description: "Log levels and output configuration",
        icon: "📋",
    },
    SectionMeta {
        key: "browser",
        label: "Browser",
        description: "Browser automation settings",
        icon: "🔍",
    },
    SectionMeta {
        key: "models",
        label: "Models",
        description: "AI model configurations and providers",
        icon: "🧠",
    },
    SectionMeta {
        key: "audio",
        label: "Audio",
        description: "Audio input/output settings",
        icon: "🔊",
    },
    SectionMeta {
        key: "cron",
        label: "Cron",
        description: "Scheduled tasks and automation",
        icon: "⏰",
    },
    SectionMeta {
        key: "session",
        label: "Session",
        description: "Session management and persistence",
        icon: "💾",
    },
    SectionMeta {
        key: "canvasHost",
        label: "Canvas Host",
        description: "Canvas rendering and display",
        icon: "🖼",
    },
    SectionMeta {
        key: "talk",
        label: "Talk",
        description: "Voice and speech settings",
        icon: "🎤",
    },
    SectionMeta {
        key: "plugins",
        label: "Plugins",
        description: "Plugin management and extensions",
        icon: "🔌",
    },
    SectionMeta {
        key: "memory",
        label: "Memory",
        description: "Memory and knowledge storage",
        icon: "📚",
    },
];

// ============== Main Component ==============

/// Config page with deep-link to a specific section: /config/:section
#[component]
pub fn ConfigSection(section: String) -> Element {
    config_inner(Some(section))
}

#[component]
pub fn Config() -> Element {
    config_inner(None)
}

fn config_inner(active_section: Option<String>) -> Element {
    let ws = use_context::<WsRpc>();
    let ws_connected = use_context::<Signal<bool>>();
    let mut refresh_tick = use_signal(|| 0u32);
    let mut search_query = use_signal(String::new);
    let mut mode = use_signal(|| "form");
    let mut edit_json = use_signal(String::new);
    let mut original_json = use_signal(String::new);
    let mut error_msg = use_signal(|| Option::<String>::None);
    let mut success_msg = use_signal(|| Option::<String>::None);
    let mut form_value = use_signal(|| HashMap::<String, serde_json::Value>::new());
    let mut original_value = use_signal(|| HashMap::<String, serde_json::Value>::new());
    let mut saving = use_signal(|| false);
    let mut applying = use_signal(|| false);

    // Track whether we've initialized the form from config data
    let mut form_initialized = use_signal(|| false);

    // Schema and config data as signals, populated by effects that
    // spawn async fetches. `use_effect` reliably re-runs when its
    // reactive dependencies (`ws_connected`, `refresh_tick`) change,
    // whereas `use_resource` may not re-trigger after the first run.
    let mut schema_props = use_signal(|| HashMap::<String, serde_json::Value>::new());
    let mut ui_hints = use_signal(|| HashMap::<String, serde_json::Value>::new());

    // Fetch schema and config data. Spawns an async task that retries
    // until the WebSocket is ready, because there is a timing gap between
    // `ws_connected` becoming true and the WS actually being OPEN.
    {
        let ws = ws.clone();
        use_effect(move || {
            let _c = ws_connected();
            let _t = refresh_tick();
            let ws = ws.clone();
            spawn(async move {
                // Retry up to 20 times (10 seconds total) waiting for WS
                for attempt in 0u32..20 {
                    if attempt > 0 {
                        sleep_ms(500).await;
                    }
                    match ws.call::<serde_json::Value>("config.schema", None).await {
                        Ok(val) => {
                            let props: HashMap<String, serde_json::Value> = val
                                .get("schema")
                                .and_then(|s| s.get("properties"))
                                .and_then(|p| serde_json::from_value(p.clone()).ok())
                                .unwrap_or_default();
                            let hints: HashMap<String, serde_json::Value> = val
                                .get("uiHints")
                                .and_then(|h| serde_json::from_value(h.clone()).ok())
                                .unwrap_or_default();
                            schema_props.set(props);
                            ui_hints.set(hints);
                            break;
                        }
                        Err(_) => {}
                    }
                }
            });
        });
    }

    {
        let ws = ws.clone();
        use_effect(move || {
            let _c = ws_connected();
            let _t = refresh_tick();
            let ws = ws.clone();
            spawn(async move {
                for attempt in 0u32..20 {
                    if attempt > 0 {
                        sleep_ms(500).await;
                    }
                    match ws.call::<serde_json::Value>("config.get", None).await {
                        Ok(val) => {
                            let config_map: HashMap<String, serde_json::Value> = val
                                .get("config")
                                .and_then(|c| serde_json::from_value(c.clone()).ok())
                                .unwrap_or_default();
                            if !config_map.is_empty() && !form_initialized() {
                                form_value.set(config_map.clone());
                                original_value.set(config_map.clone());
                                let json_str =
                                    serde_json::to_string_pretty(&config_map).unwrap_or_default();
                                edit_json.set(json_str.clone());
                                original_json.set(json_str);
                                form_initialized.set(true);
                            }
                            break;
                        }
                        Err(_) => {}
                    }
                }
            });
        });
    }

    // Always show all sections — schema data enhances but doesn't gate display
    let available_sections: Vec<SectionMeta> = SECTIONS.to_vec();

    let get_diff_entries = || -> Vec<DiffEntry> {
        let current = form_value.read();
        let original = original_value.read();
        compute_deep_diff(&original, &current)
    };

    let diff_entries = get_diff_entries();
    let has_changes = !diff_entries.is_empty();

    let validity = validate_config(&form_value.read(), &schema_props.read());

    rsx! {
        div { class: "config-layout",
            // Sidebar
            aside { class: "config-sidebar",
                div { class: "config-sidebar__header",
                    div { class: "config-sidebar__title", "Settings" }
                    {match &validity {
                        ValidityState::Valid => rsx! {
                            span { class: "pill pill--sm pill--ok", "Valid" }
                        },
                        ValidityState::Invalid(count) => rsx! {
                            span { class: "pill pill--sm pill--danger", "{count} errors" }
                        },
                        ValidityState::Unknown => rsx! {
                            span { class: "pill pill--sm pill--warn", "Unknown" }
                        },
                    }}
                }

                // Search
                div { class: "config-search",
                    span { class: "config-search__icon", "🔍" }
                    input {
                        r#type: "text",
                        class: "config-search__input",
                        placeholder: "Search settings...",
                        value: "{search_query}",
                        oninput: move |e| search_query.set(e.value()),
                    }
                }

                // Section nav with deep links
                nav { class: "config-nav",
                    Link {
                        to: crate::route::Route::Config {},
                        class: if active_section.is_none() { "config-nav__item active" } else { "config-nav__item" },
                        span { class: "config-nav__icon", "⊞" }
                        span { class: "config-nav__label", "All Settings" }
                    }
                    for section in available_sections.iter() {
                        {
                            let is_active = active_section == Some(section.key.to_string());
                            let q = search_query().to_lowercase();
                            let matches = q.is_empty() ||
                                section.key.to_lowercase().contains(&q) ||
                                section.label.to_lowercase().contains(&q);
                            if matches {
                                let key = section.key.to_string();
                                rsx! {
                                    Link {
                                        key: "{section.key}",
                                        to: crate::route::Route::ConfigSection { section: key },
                                        class: if is_active { "config-nav__item active" } else { "config-nav__item" },
                                        span { class: "config-nav__icon", "{section.icon}" }
                                        span { class: "config-nav__label", "{section.label}" }
                                    }
                                }
                            } else {
                                rsx! { div { style: "display:none;", } }
                            }
                        }
                    }
                }

                // Mode toggle
                div { class: "config-sidebar__footer",
                    div { class: "config-mode-toggle",
                        button {
                            class: if mode() == "form" { "config-mode-toggle__btn active" } else { "config-mode-toggle__btn" },
                            onclick: move |_| mode.set("form"),
                            "Form"
                        }
                        button {
                            class: if mode() == "json" { "config-mode-toggle__btn active" } else { "config-mode-toggle__btn" },
                            onclick: move |_| mode.set("json"),
                            "Raw"
                        }
                    }
                }
            }

            // Main content
            main { class: "config-main",
                // Action bar
                div { class: "config-actions",
                    div { class: "config-actions__left",
                        if has_changes {
                            span { class: "config-changes-badge", "{diff_entries.len()} unsaved changes" }
                        } else {
                            span { class: "config-status muted", "No changes" }
                        }
                    }
                    div { class: "config-actions__right",
                        button {
                            class: "btn btn--sm",
                            title: "Export configuration as JSON",
                            onclick: move |_| {
                                let config = form_value.read().clone();
                                let json_str = serde_json::to_string_pretty(&config).unwrap_or_default();
                                export_config_json(&json_str);
                            },
                            "Export"
                        }
                        button {
                            class: "btn btn--sm",
                            title: "Import configuration from JSON file",
                            onclick: move |_| {
                                import_config_json(form_value, error_msg);
                            },
                            "Import"
                        }
                        button {
                            class: "btn btn--sm",
                            onclick: move |_| { refresh_tick += 1; },
                            "Reload"
                        }
                        button {
                            class: "btn btn--sm primary",
                            disabled: !has_changes || saving(),
                            onclick: {
                                let ws = ws.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let val = form_value.read().clone();
                                    saving.set(true);
                                    spawn(async move {
                                        let result = ws.call::<serde_json::Value>("config.patch", Some(json!({ "patch": val }))).await;
                                        saving.set(false);
                                        match result {
                                            Ok(_) => {
                                                success_msg.set(Some("Configuration saved".to_string()));
                                                original_value.set(form_value.read().clone());
                                                refresh_tick += 1;
                                            }
                                            Err(e) => {
                                                error_msg.set(Some(format!("Failed to save: {:?}", e)));
                                            }
                                        }
                                    });
                                }
                            },
                            if saving() { "Saving..." } else { "Save" }
                        }
                        button {
                            class: "btn btn--sm",
                            disabled: !has_changes || applying(),
                            onclick: {
                                let ws = ws.clone();
                                move |_| {
                                    let ws = ws.clone();
                                    let val = form_value.read().clone();
                                    applying.set(true);
                                    spawn(async move {
                                        let _ = ws.call::<serde_json::Value>("config.patch", Some(json!({ "patch": val }))).await;
                                        let result = ws.call::<serde_json::Value>("config.apply", Some(json!({ "config": val }))).await;
                                        applying.set(false);
                                        match result {
                                            Ok(_) => {
                                                success_msg.set(Some("Configuration applied".to_string()));
                                                original_value.set(form_value.read().clone());
                                                refresh_tick += 1;
                                            }
                                            Err(e) => {
                                                error_msg.set(Some(format!("Failed to apply: {:?}", e)));
                                            }
                                        }
                                    });
                                }
                            },
                            if applying() { "Applying..." } else { "Apply" }
                        }
                    }
                }

                // Messages
                if let Some(ref msg) = error_msg() {
                    div { class: "callout danger", "{msg}" }
                }
                if let Some(ref msg) = success_msg() {
                    div { class: "callout success", "{msg}" }
                }

                // Diff panel
                if has_changes {
                    details { class: "config-diff",
                        summary { class: "config-diff__summary",
                            "View {diff_entries.len()} pending changes"
                        }
                        div { class: "config-diff__content",
                            for entry in diff_entries.iter() {
                                {
                                    let diff_class = match entry.kind {
                                        DiffKind::Added => "config-diff__item config-diff__added",
                                        DiffKind::Removed => "config-diff__item config-diff__removed",
                                        DiffKind::Changed => "config-diff__item config-diff__changed",
                                    };
                                    let revert_path = entry.path.clone();
                                    let revert_old = entry.old_value.clone();
                                    rsx! {
                                        div { class: "{diff_class}",
                                            div { class: "config-diff__path", "{entry.path}" }
                                            div { class: "config-diff__values",
                                                match &entry.kind {
                                                    DiffKind::Added => rsx! {
                                                        span { class: "config-diff__to", "+ {truncate_value(&entry.new_value)}" }
                                                    },
                                                    DiffKind::Removed => rsx! {
                                                        span { class: "config-diff__from", "- {truncate_value(&entry.old_value)}" }
                                                    },
                                                    DiffKind::Changed => rsx! {
                                                        span { class: "config-diff__from", "{truncate_value(&entry.old_value)}" }
                                                        span { class: "config-diff__arrow", " -> " }
                                                        span { class: "config-diff__to", "{truncate_value(&entry.new_value)}" }
                                                    },
                                                }
                                                button {
                                                    class: "config-diff__revert",
                                                    title: "Revert this change",
                                                    onclick: move |_| {
                                                        revert_field(&revert_path, &revert_old, form_value);
                                                    },
                                                    "Revert"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Content
                div { class: "config-content",
                    if mode() == "form" {
                        {render_form(&schema_props.read(), &form_value.read(), active_section.clone(), search_query(), form_value, &ui_hints.read())}
                    } else {
                        div { class: "config-raw",
                            textarea {
                                class: "config-raw__textarea",
                                value: "{edit_json}",
                                oninput: move |e| edit_json.set(e.value()),
                                rows: 25,
                            }
                            div { class: "config-raw__actions",
                                button {
                                    class: "btn primary",
                                    onclick: {
                                        let ws = ws.clone();
                                        move |_| {
                                            let raw = edit_json();
                                            match serde_json::from_str::<serde_json::Value>(&raw) {
                                                Ok(val) => {
                                                    error_msg.set(None);
                                                    let ws = ws.clone();
                                                    spawn(async move {
                                                        let _ = ws.call::<serde_json::Value>("config.patch", Some(json!({ "patch": val.clone() }))).await;
                                                        let _ = ws.call::<serde_json::Value>("config.apply", Some(json!({ "config": val }))).await;
                                                        refresh_tick += 1;
                                                    });
                                                }
                                                Err(e) => {
                                                    error_msg.set(Some(format!("Invalid JSON: {e}")));
                                                }
                                            }
                                        }
                                    },
                                    "Save & Apply"
                                }
                                button {
                                    class: "btn",
                                    onclick: move |_| {
                                        edit_json.set(original_json());
                                        error_msg.set(None);
                                    },
                                    "Reset"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn truncate_value(value: &Option<serde_json::Value>) -> String {
    match value {
        None => "null".to_string(),
        Some(v) => {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Null => "null".to_string(),
                _ => serde_json::to_string(v).unwrap_or_default(),
            };
            if s.chars().count() > 40 {
                let truncated: String = s.chars().take(37).collect();
                format!("{truncated}...")
            } else {
                s
            }
        }
    }
}

fn render_form(
    schema_props: &HashMap<String, serde_json::Value>,
    form_value: &HashMap<String, serde_json::Value>,
    active_section: Option<String>,
    search_query: String,
    form_signal: Signal<HashMap<String, serde_json::Value>>,
    ui_hints: &HashMap<String, serde_json::Value>,
) -> Element {
    let sections: Vec<SectionMeta> = SECTIONS
        .iter()
        .filter(|s| {
            let matches_section =
                active_section.is_none() || active_section == Some(s.key.to_string());
            // When a specific section is targeted (deep-link), always show it
            // even if schema hasn't loaded yet. For "All Settings" view,
            // show sections that are in schema or present in current config.
            let schema_ok = active_section.is_some()
                || schema_props.contains_key(s.key)
                || form_value.contains_key(s.key);
            schema_ok && matches_section
        })
        .copied()
        .collect();

    rsx! {
        div { class: "config-form",
            for section in sections {
                {
                    let section_key = section.key.to_string();
                    let section_val = form_value.get(section.key).cloned();
                    let section_schema = schema_props.get(section.key).cloned();
                    let hints = ui_hints.clone();

                    rsx! {
                        SectionCard {
                            key: "{section_key}",
                            section_key: section_key,
                            section_label: section.label.to_string(),
                            section_desc: section.description.to_string(),
                            section_icon: section.icon.to_string(),
                            section_schema: section_schema.unwrap_or(json!({})),
                            section_val: section_val.unwrap_or(json!({})),
                            search_query: search_query.clone(),
                            form_signal: form_signal,
                            ui_hints: hints,
                        }
                    }
                }
            }
        }
    }
}

// ============== Section Card Component ==============

#[component]
fn SectionCard(
    section_key: String,
    section_label: String,
    section_desc: String,
    section_icon: String,
    section_schema: serde_json::Value,
    section_val: serde_json::Value,
    search_query: String,
    form_signal: Signal<HashMap<String, serde_json::Value>>,
    ui_hints: HashMap<String, serde_json::Value>,
) -> Element {
    let active_sub_tab = use_signal(|| Option::<String>::None);
    let new_entry_key = use_signal(String::new);
    let effective_schema = merge_schema_with_value(&section_schema, &section_val);

    let has_additional_props = effective_schema.get("additionalProperties").is_some();
    let has_properties = effective_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .is_some();

    // Check if all properties are objects (for sub-tab rendering)
    let all_props_are_objects = effective_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|props| {
            props.len() >= 3
                && props
                    .values()
                    .all(|v| v.get("type").and_then(|t| t.as_str()) == Some("object"))
        })
        .unwrap_or(false);

    rsx! {
        section { class: "config-section-card",
            div { class: "config-section-card__header",
                span { class: "config-section-card__icon", "{section_icon}" }
                div { class: "config-section-card__titles",
                    h3 { class: "config-section-card__title", "{section_label}" }
                    p { class: "config-section-card__desc", "{section_desc}" }
                }
            }
            div { class: "config-section-card__content",
                if has_additional_props && !has_properties {
                    // Dynamic-key section (agents, models, cron, plugins)
                    {render_dynamic_entries(
                        &section_key,
                        &effective_schema,
                        &section_val,
                        form_signal,
                        &ui_hints,
                        new_entry_key,
                    )}
                } else if all_props_are_objects {
                    // Sub-tabbed section (channels)
                    {render_sub_tabbed_section(
                        &section_key,
                        &effective_schema,
                        &section_val,
                        form_signal,
                        &ui_hints,
                        active_sub_tab,
                        &search_query,
                    )}
                } else {
                    // Regular flat fields
                    {render_section_fields(
                        &section_key,
                        &effective_schema,
                        &section_val,
                        &search_query,
                        form_signal,
                        &ui_hints,
                    )}
                }
            }
        }
    }
}

// ============== Field Rendering ==============

fn schema_primary_type(schema: &serde_json::Value) -> Option<String> {
    match schema.get("type") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(types)) => {
            let non_null = types
                .iter()
                .filter_map(|t| t.as_str())
                .find(|t| *t != "null")
                .map(ToString::to_string);
            non_null.or_else(|| {
                types
                    .iter()
                    .filter_map(|t| t.as_str())
                    .next()
                    .map(ToString::to_string)
            })
        }
        _ => {
            if schema.get("properties").is_some() {
                Some("object".to_string())
            } else if schema.get("items").is_some() {
                Some("array".to_string())
            } else {
                None
            }
        }
    }
}

fn schema_is_nullable(schema: &serde_json::Value) -> bool {
    if let Some(serde_json::Value::String(s)) = schema.get("type") {
        return s == "null";
    }
    if let Some(serde_json::Value::Array(types)) = schema.get("type") {
        return types.iter().any(|t| t.as_str() == Some("null"));
    }
    if let Some(one_of) = schema.get("oneOf").and_then(|v| v.as_array()) {
        return one_of.iter().any(schema_is_nullable);
    }
    if let Some(any_of) = schema.get("anyOf").and_then(|v| v.as_array()) {
        return any_of.iter().any(schema_is_nullable);
    }
    false
}

fn value_matches_type(value: &serde_json::Value, ty: &str) -> bool {
    match ty {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.as_f64().is_some(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn value_matches_schema(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    if value.is_null() {
        return schema_is_nullable(schema)
            || schema_primary_type(schema).as_deref() == Some("null");
    }

    if let Some(enum_vals) = schema.get("enum").and_then(|v| v.as_array()) {
        return enum_vals.iter().any(|allowed| allowed == value);
    }

    if let Some(ty) = schema_primary_type(schema) {
        return value_matches_type(value, &ty);
    }

    true
}

fn resolve_schema_variant(
    schema: &serde_json::Value,
    value: Option<&serde_json::Value>,
) -> serde_json::Value {
    let variants = schema
        .get("oneOf")
        .and_then(|v| v.as_array())
        .or_else(|| schema.get("anyOf").and_then(|v| v.as_array()));

    let Some(variants) = variants else {
        return schema.clone();
    };

    if let Some(current) = value {
        if let Some(matched) = variants
            .iter()
            .find(|candidate| value_matches_schema(current, candidate))
        {
            return matched.clone();
        }
    }

    if let Some(non_null) = variants
        .iter()
        .find(|candidate| schema_primary_type(candidate).as_deref() != Some("null"))
    {
        return non_null.clone();
    }

    variants.first().cloned().unwrap_or_else(|| schema.clone())
}

fn field_validation_error(
    schema: &serde_json::Value,
    value: Option<&serde_json::Value>,
) -> Option<String> {
    let value = value?;

    if value.is_null() {
        if !schema_is_nullable(schema) {
            return Some("This field cannot be null".to_string());
        }
        return None;
    }

    if let Some(enum_vals) = schema.get("enum").and_then(|v| v.as_array()) {
        if !enum_vals.iter().any(|allowed| allowed == value) {
            return Some("Value is not in allowed enum options".to_string());
        }
        return None;
    }

    if let Some(ty) = schema_primary_type(schema) {
        if !value_matches_type(value, &ty) {
            return Some(format!("Expected {ty} value"));
        }
    }

    None
}

fn infer_schema_from_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Null => json!({ "type": ["string", "null"] }),
        serde_json::Value::Bool(_) => json!({ "type": "boolean" }),
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                json!({ "type": "integer" })
            } else {
                json!({ "type": "number" })
            }
        }
        serde_json::Value::String(_) => json!({ "type": "string" }),
        serde_json::Value::Array(items) => {
            let item_schema = items
                .iter()
                .find(|item| !item.is_null())
                .map(infer_schema_from_value)
                .unwrap_or_else(|| json!({ "type": "string" }));
            json!({
                "type": "array",
                "items": item_schema
            })
        }
        serde_json::Value::Object(map) => {
            let mut props = serde_json::Map::new();
            for (key, val) in map {
                props.insert(key.clone(), infer_schema_from_value(val));
            }
            let mut obj = serde_json::Map::new();
            obj.insert("type".to_string(), json!("object"));
            if !props.is_empty() {
                obj.insert("properties".to_string(), serde_json::Value::Object(props));
            }
            serde_json::Value::Object(obj)
        }
    }
}

fn merge_schema_with_value(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> serde_json::Value {
    let mut merged = schema.clone();
    let Some(value_obj) = value.as_object() else {
        return merged;
    };

    if !merged.is_object() {
        merged = json!({});
    }

    let has_additional_props = merged.get("additionalProperties").is_some();
    let Some(merged_obj) = merged.as_object_mut() else {
        return merged;
    };

    if let Some(props) = merged_obj
        .get_mut("properties")
        .and_then(|p| p.as_object_mut())
    {
        for (key, val) in value_obj {
            props
                .entry(key.clone())
                .or_insert_with(|| infer_schema_from_value(val));
        }
        return merged;
    }

    if !has_additional_props && !value_obj.is_empty() {
        let mut inferred_props = serde_json::Map::new();
        for (key, val) in value_obj {
            inferred_props.insert(key.clone(), infer_schema_from_value(val));
        }
        merged_obj.insert("type".to_string(), json!("object"));
        merged_obj.insert(
            "properties".to_string(),
            serde_json::Value::Object(inferred_props),
        );
    }

    merged
}

fn render_section_fields(
    section_key: &str,
    schema: &serde_json::Value,
    value: &serde_json::Value,
    search_query: &str,
    form_signal: Signal<HashMap<String, serde_json::Value>>,
    ui_hints: &HashMap<String, serde_json::Value>,
) -> Element {
    let val_obj = value.as_object();
    let needle = search_query.trim().to_lowercase();

    let mut merged_fields = serde_json::Map::new();
    if let Some(fields) = schema.get("properties").and_then(|p| p.as_object()) {
        for (field_key, field_schema) in fields {
            merged_fields.insert(field_key.clone(), field_schema.clone());
        }
    }
    if let Some(obj) = val_obj {
        for (field_key, field_val) in obj {
            merged_fields
                .entry(field_key.clone())
                .or_insert_with(|| infer_schema_from_value(field_val));
        }
    }

    let has_fields = !merged_fields.is_empty();
    let root_schema = if !has_fields && !value.is_object() && !value.is_null() {
        let mut inferred = infer_schema_from_value(value);
        if let Some(obj) = inferred.as_object_mut() {
            obj.insert("title".to_string(), json!("Value"));
        }
        Some(inferred)
    } else {
        None
    };
    let root_matches = needle.is_empty()
        || "value".contains(&needle)
        || "root value".contains(&needle)
        || section_key.to_lowercase().contains(&needle);

    rsx! {
        div { class: "cfg-fields",
            if has_fields {
                {
                    let mut sorted_fields: Vec<(String, serde_json::Value)> = merged_fields.into_iter().collect();
                    sorted_fields.sort_by_key(|(key, _)| {
                        let hint_key = format!("{}.{}", section_key, key);
                        ui_hints.get(&hint_key)
                            .and_then(|h| h.get("order"))
                            .and_then(|o| o.as_i64())
                            .unwrap_or(999) as i32
                    });
                    if !needle.is_empty() {
                        sorted_fields.retain(|(field_key, field_schema)| {
                            let title = field_schema
                                .get("title")
                                .and_then(|t| t.as_str())
                                .unwrap_or(field_key)
                                .to_lowercase();
                            let description = field_schema
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let key = field_key.to_lowercase();
                            key.contains(&needle)
                                || title.contains(&needle)
                                || description.contains(&needle)
                        });
                    }

                    rsx! {
                        if sorted_fields.is_empty() {
                            div { class: "cfg-empty",
                                if needle.is_empty() {
                                    "No fields defined"
                                } else {
                                    "No fields match search"
                                }
                            }
                        } else {
                            for (field_key, field_schema) in sorted_fields.into_iter() {
                                {render_field(
                                    section_key,
                                    &[field_key.as_str()],
                                    &field_key,
                                    &field_schema,
                                    val_obj.and_then(|o| o.get(&field_key)).cloned(),
                                    form_signal,
                                    ui_hints,
                                )}
                            }
                        }
                    }
                }
            } else if let Some(root_field_schema) = root_schema {
                if root_matches {
                    {render_field(
                        section_key,
                        &[],
                        "value",
                        &root_field_schema,
                        Some(value.clone()),
                        form_signal,
                        ui_hints,
                    )}
                } else {
                    div { class: "cfg-empty", "No fields match search" }
                }
            } else {
                div { class: "cfg-empty", "No fields defined" }
            }
        }
    }
}

fn render_field(
    section_key: &str,
    path_parts: &[&str],
    field_key: &str,
    field_schema: &serde_json::Value,
    field_val: Option<serde_json::Value>,
    form_signal: Signal<HashMap<String, serde_json::Value>>,
    ui_hints: &HashMap<String, serde_json::Value>,
) -> Element {
    let resolved_schema = resolve_schema_variant(field_schema, field_val.as_ref());
    let field_type = schema_primary_type(&resolved_schema).unwrap_or_else(|| "string".to_string());
    let field_nullable = schema_is_nullable(&resolved_schema);
    let field_title = resolved_schema
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or(field_key);
    let field_desc = resolved_schema
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    let field_error = field_validation_error(&resolved_schema, field_val.as_ref());

    // Check if this is a nested object with its own properties
    if field_type == "object" {
        if let Some(nested_props) = resolved_schema
            .get("properties")
            .and_then(|p| p.as_object())
        {
            let nested_val = field_val.unwrap_or(json!({}));
            return rsx! {
                details { key: "{field_key}", class: "cfg-nested-group", open: true,
                    summary { class: "cfg-nested-group__header",
                        "{field_title}"
                        if !field_desc.is_empty() {
                            span { class: "cfg-nested-group__desc", " — {field_desc}" }
                        }
                    }
                    div { class: "cfg-nested-group__body",
                        for (nested_key, nested_schema) in nested_props.iter() {
                            {
                                let nested_field_val = nested_val.get(nested_key).cloned();
                                let mut new_path: Vec<&str> = path_parts.to_vec();
                                let nk: &str = nested_key;
                                new_path.push(nk);
                                render_field(
                                    section_key,
                                    &new_path,
                                    nested_key,
                                    nested_schema,
                                    nested_field_val,
                                    form_signal,
                                    ui_hints,
                                )
                            }
                        }
                    }
                }
            };
        }
    }

    let field_enum = resolved_schema
        .get("enum")
        .and_then(|e| e.as_array())
        .cloned();
    let field_default = resolved_schema.get("default").cloned();

    rsx! {
        div { key: "{field_key}", class: "cfg-field",
            label { class: "cfg-field__label", "{field_title}" }
            if !field_desc.is_empty() {
                div { class: "cfg-field__help", "{field_desc}" }
            }
            {render_field_input(
                section_key,
                path_parts,
                field_key,
                &field_type,
                &resolved_schema,
                field_nullable,
                field_enum,
                field_default,
                field_val,
                form_signal,
                ui_hints,
            )}
            if let Some(err) = field_error {
                div { class: "cfg-field__error", "{err}" }
            }
        }
    }
}

fn render_field_input(
    section_key: &str,
    path_parts: &[&str],
    _field_key: &str,
    field_type: &str,
    field_schema: &serde_json::Value,
    nullable: bool,
    field_enum: Option<Vec<serde_json::Value>>,
    field_default: Option<serde_json::Value>,
    field_val: Option<serde_json::Value>,
    mut form_signal: Signal<HashMap<String, serde_json::Value>>,
    ui_hints: &HashMap<String, serde_json::Value>,
) -> Element {
    let full_path = if path_parts.is_empty() {
        section_key.to_string()
    } else {
        format!("{}.{}", section_key, path_parts.join("."))
    };
    let section_key = section_key.to_string();
    let path_owned: Vec<String> = path_parts.iter().map(|s| s.to_string()).collect();

    // Check uiHints sensitivity, fall back to heuristic
    let is_sensitive = ui_hints
        .get(&full_path)
        .and_then(|h| h.get("sensitive"))
        .and_then(|s| s.as_bool())
        .unwrap_or_else(|| {
            let lp = full_path.to_lowercase();
            lp.contains("token")
                || lp.contains("password")
                || lp.contains("secret")
                || lp.contains("key")
        });

    // Handle enum
    if let Some(ref enum_vals) = field_enum {
        if enum_vals.len() <= 5 {
            return rsx! {
                div { class: "cfg-segmented",
                    if nullable {
                        {
                            let is_active = field_val.as_ref().is_some_and(|v| v.is_null());
                            let sk = section_key.clone();
                            let pp = path_owned.clone();
                            rsx! {
                                button {
                                    class: if is_active { "cfg-segmented__btn active" } else { "cfg-segmented__btn" },
                                    onclick: move |_| {
                                        update_nested_value(&mut form_signal, &sk, &pp, serde_json::Value::Null);
                                    },
                                    "None"
                                }
                            }
                        }
                    }
                    for opt in enum_vals.iter() {
                        {
                            let opt_str = match opt {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Bool(b) => b.to_string(),
                                serde_json::Value::Number(n) => n.to_string(),
                                _ => opt.to_string(),
                            };
                            let is_active = field_val.as_ref() == Some(opt) ||
                                (field_val.is_none() && field_default.as_ref() == Some(opt));
                            let opt_c = opt.clone();
                            let sk = section_key.clone();
                            let pp = path_owned.clone();
                            rsx! {
                                button {
                                    key: "{opt_str}",
                                    class: if is_active { "cfg-segmented__btn active" } else { "cfg-segmented__btn" },
                                    onclick: move |_| {
                                        update_nested_value(&mut form_signal, &sk, &pp, opt_c.clone());
                                    },
                                    "{opt_str}"
                                }
                            }
                        }
                    }
                }
            };
        }
    }

    match field_type {
        "boolean" => {
            let bool_val = field_val
                .as_ref()
                .and_then(|v| v.as_bool())
                .or(field_default.as_ref().and_then(|d| d.as_bool()))
                .unwrap_or(false);
            let sk = section_key.clone();
            let pp = path_owned.clone();
            rsx! {
                label { class: "cfg-toggle-row",
                    div { class: "cfg-toggle-row__content",
                        div { class: "cfg-toggle",
                            input {
                                r#type: "checkbox",
                                checked: "{bool_val}",
                                onchange: move |e| {
                                    update_nested_value(&mut form_signal, &sk, &pp, json!(e.checked()));
                                },
                            }
                            span { class: "cfg-toggle__track" }
                        }
                    }
                }
            }
        }
        "integer" | "number" => {
            let is_integer = field_type == "integer";
            let num_text = match field_val.as_ref() {
                Some(serde_json::Value::Number(n)) => n.to_string(),
                Some(serde_json::Value::Null) if nullable => String::new(),
                _ => field_default
                    .as_ref()
                    .and_then(|d| d.as_f64())
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
            };
            let sk1 = section_key.clone();
            let sk2 = section_key.clone();
            let sk3 = section_key.clone();
            let pp1 = path_owned.clone();
            let pp2 = path_owned.clone();
            let pp3 = path_owned.clone();
            rsx! {
                div { class: "cfg-number",
                    button {
                        class: "cfg-number__btn",
                        onclick: move |_| {
                            let current = get_nested_value(&form_signal.read(), &sk1, &pp1)
                                .and_then(|v| v.as_f64()).unwrap_or(0.0);
                            if is_integer {
                                update_nested_value(&mut form_signal, &sk1, &pp1, json!((current as i64) - 1));
                            } else {
                                update_nested_value(&mut form_signal, &sk1, &pp1, json!(current - 1.0));
                            }
                        },
                        "−"
                    }
                    input {
                        r#type: "number",
                        class: "cfg-number__input",
                        value: "{num_text}",
                        oninput: move |e| {
                            let raw = e.value();
                            if nullable && raw.trim().is_empty() {
                                update_nested_value(&mut form_signal, &sk2, &pp2, serde_json::Value::Null);
                                return;
                            }
                            let val: f64 = raw.parse().unwrap_or(0.0);
                            if is_integer {
                                update_nested_value(&mut form_signal, &sk2, &pp2, json!(val as i64));
                            } else {
                                update_nested_value(&mut form_signal, &sk2, &pp2, json!(val));
                            }
                        },
                    }
                    button {
                        class: "cfg-number__btn",
                        onclick: move |_| {
                            let current = get_nested_value(&form_signal.read(), &sk3, &pp3)
                                .and_then(|v| v.as_f64()).unwrap_or(0.0);
                            if is_integer {
                                update_nested_value(&mut form_signal, &sk3, &pp3, json!((current as i64) + 1));
                            } else {
                                update_nested_value(&mut form_signal, &sk3, &pp3, json!(current + 1.0));
                            }
                        },
                        "+"
                    }
                }
            }
        }
        "array" => {
            let item_schema = field_schema
                .get("items")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "string" }));
            let items = match field_val.as_ref() {
                Some(serde_json::Value::Array(arr)) => arr.clone(),
                Some(serde_json::Value::Null) if nullable => Vec::new(),
                Some(other) => vec![other.clone()],
                None => field_default
                    .as_ref()
                    .and_then(|d| d.as_array().cloned())
                    .unwrap_or_default(),
            };
            rsx! {
                div { class: "cfg-array",
                    for (idx, item) in items.iter().enumerate() {
                        {
                            let display = match item {
                                serde_json::Value::String(s) => s.clone(),
                                _ => item.to_string(),
                            };
                            let sk_edit = section_key.clone();
                            let pp_edit = path_owned.clone();
                            let sk_remove = section_key.clone();
                            let pp_remove = path_owned.clone();
                            let default_arr = field_default
                                .as_ref()
                                .and_then(|d| d.as_array().cloned())
                                .unwrap_or_default();
                            let item_schema_local = item_schema.clone();
                            rsx! {
                                div { key: "{idx}", class: "cfg-array__row",
                                    input {
                                        r#type: "text",
                                        class: "cfg-input",
                                        value: "{display}",
                                        oninput: move |e| {
                                            let mut arr = get_nested_value(&form_signal.read(), &sk_edit, &pp_edit)
                                                .and_then(|v| v.as_array().cloned())
                                                .unwrap_or_else(|| default_arr.clone());
                                            if idx >= arr.len() {
                                                arr.resize(idx + 1, json!(""));
                                            }
                                            let raw = e.value();
                                            let parsed = match schema_primary_type(&item_schema_local).as_deref() {
                                                Some("number") => raw
                                                    .parse::<f64>()
                                                    .map(|n| json!(n))
                                                    .unwrap_or_else(|_| json!(raw)),
                                                Some("integer") => raw
                                                    .parse::<i64>()
                                                    .map(|n| json!(n))
                                                    .unwrap_or_else(|_| json!(raw)),
                                                Some("boolean") => raw
                                                    .parse::<bool>()
                                                    .map(|b| json!(b))
                                                    .unwrap_or_else(|_| json!(raw)),
                                                _ => json!(raw),
                                            };
                                            arr[idx] = parsed;
                                            update_nested_value(&mut form_signal, &sk_edit, &pp_edit, json!(arr));
                                        },
                                    }
                                    button {
                                        class: "cfg-array__remove",
                                        onclick: move |_| {
                                            let mut arr = get_nested_value(&form_signal.read(), &sk_remove, &pp_remove)
                                                .and_then(|v| v.as_array().cloned())
                                                .unwrap_or_default();
                                            if idx < arr.len() {
                                                arr.remove(idx);
                                            }
                                            update_nested_value(&mut form_signal, &sk_remove, &pp_remove, json!(arr));
                                        },
                                        "Remove"
                                    }
                                }
                            }
                        }
                    }

                    button {
                        class: "cfg-array__add",
                        onclick: {
                            let sk_add = section_key.clone();
                            let pp_add = path_owned.clone();
                            move |_| {
                                let mut arr = get_nested_value(&form_signal.read(), &sk_add, &pp_add)
                                    .and_then(|v| v.as_array().cloned())
                                    .unwrap_or_default();
                                arr.push(json!(""));
                                update_nested_value(&mut form_signal, &sk_add, &pp_add, json!(arr));
                            }
                        },
                        "Add Item"
                    }
                }
            }
        }
        _ => {
            let str_val = match field_val.as_ref() {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            let placeholder = match &field_default {
                Some(d) => format!("Default: {}", d),
                None => String::new(),
            };
            let sk = section_key.clone();
            let pp = path_owned.clone();
            let sk2 = section_key.clone();
            let pp2 = path_owned.clone();
            rsx! {
                div { class: "cfg-input-wrap",
                    input {
                        r#type: if is_sensitive { "password" } else { "text" },
                        class: "cfg-input",
                        value: "{str_val}",
                        placeholder: "{placeholder}",
                        oninput: move |e| {
                            let val = e.value();
                            if nullable && val.trim().is_empty() {
                                update_nested_value(&mut form_signal, &sk, &pp, serde_json::Value::Null);
                            } else {
                                update_nested_value(&mut form_signal, &sk, &pp, json!(val));
                            }
                        },
                    }
                    if field_default.is_some() {
                        button {
                            class: "cfg-input__reset",
                            title: "Reset to default",
                            onclick: {
                                let fd = field_default.clone();
                                move |_| {
                                    update_nested_value(&mut form_signal, &sk2, &pp2, fd.clone().unwrap_or(json!(null)));
                                }
                            },
                            "↺"
                        }
                    }
                }
            }
        }
    }
}

// ============== Sub-tabbed Section (e.g. Channels) ==============

fn render_sub_tabbed_section(
    section_key: &str,
    schema: &serde_json::Value,
    value: &serde_json::Value,
    form_signal: Signal<HashMap<String, serde_json::Value>>,
    ui_hints: &HashMap<String, serde_json::Value>,
    mut active_sub_tab: Signal<Option<String>>,
    search_query: &str,
) -> Element {
    let props = schema.get("properties").and_then(|p| p.as_object());
    let Some(fields) = props else {
        return rsx! { div { class: "cfg-empty", "No fields defined" } };
    };

    let needle = search_query.trim().to_lowercase();
    let field_matches = |key: &str, schema: &serde_json::Value| -> bool {
        if needle.is_empty() {
            return true;
        }
        let title = schema
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or(key)
            .to_lowercase();
        let desc = schema
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_lowercase();
        key.to_lowercase().contains(&needle) || title.contains(&needle) || desc.contains(&needle)
    };
    let tab_keys: Vec<String> = fields
        .iter()
        .filter(|(k, s)| field_matches(k, s))
        .map(|(k, _)| k.clone())
        .collect();
    let filtered_fields: Vec<(&String, &serde_json::Value)> =
        fields.iter().filter(|(k, s)| field_matches(k, s)).collect();

    rsx! {
        div { class: "cfg-sub-tabs",
            div { class: "tab-bar",
                button {
                    class: if active_sub_tab().is_none() { "tab-bar__item active" } else { "tab-bar__item" },
                    onclick: move |_| active_sub_tab.set(None),
                    "All"
                }
                for tk in tab_keys.iter() {
                    {
                        let tab_title = fields.get(tk)
                            .and_then(|s| s.get("title"))
                            .and_then(|t| t.as_str())
                            .unwrap_or(tk);
                        let is_active = active_sub_tab() == Some(tk.clone());
                        let tk_c = tk.clone();
                        rsx! {
                            button {
                                key: "{tk}",
                                class: if is_active { "tab-bar__item active" } else { "tab-bar__item" },
                                onclick: move |_| active_sub_tab.set(Some(tk_c.clone())),
                                "{tab_title}"
                            }
                        }
                    }
                }
            }
            div { class: "cfg-fields", style: "margin-top: 16px;",
                for (sub_key, sub_schema) in filtered_fields.iter().copied() {
                    {
                        let visible = active_sub_tab().is_none() || active_sub_tab() == Some(sub_key.clone());
                        if visible {
                            let sub_val = value.get(sub_key).cloned();
                            render_field(
                                section_key,
                                &[sub_key.as_str()],
                                sub_key,
                                sub_schema,
                                sub_val,
                                form_signal,
                                ui_hints,
                            )
                        } else {
                            rsx! { div { style: "display:none;" } }
                        }
                    }
                }
            }
        }
    }
}

// ============== Dynamic Entries (additionalProperties) ==============

fn render_dynamic_entries(
    section_key: &str,
    schema: &serde_json::Value,
    value: &serde_json::Value,
    mut form_signal: Signal<HashMap<String, serde_json::Value>>,
    ui_hints: &HashMap<String, serde_json::Value>,
    mut new_entry_key: Signal<String>,
) -> Element {
    let entry_schema = schema
        .get("additionalProperties")
        .cloned()
        .unwrap_or(json!({}));
    let entries = value.as_object();

    let sk = section_key.to_string();
    let es = entry_schema.clone();

    rsx! {
        div { class: "cfg-dynamic-entries",
            // Existing entries
            if let Some(obj) = entries {
                for (entry_key, entry_val) in obj.iter() {
                    {
                        let ek = entry_key.clone();
                        let sk_del = sk.clone();
                        rsx! {
                            details { key: "{entry_key}", class: "cfg-nested-group", open: true,
                                summary { class: "cfg-nested-group__header",
                                    span { "{entry_key}" }
                                    button {
                                        class: "cfg-dynamic-entries__remove",
                                        title: "Remove entry",
                                        onclick: move |_| {
                                            let mut form = form_signal.write();
                                            let section = form.entry(sk_del.clone()).or_insert_with(|| json!({}));
                                            if let Some(obj) = section.as_object_mut() {
                                                obj.remove(&ek);
                                            }
                                        },
                                        "×"
                                    }
                                }
                                div { class: "cfg-nested-group__body",
                                    {render_entry_fields(
                                        section_key,
                                        entry_key,
                                        &entry_schema,
                                        entry_val,
                                        form_signal,
                                        ui_hints,
                                    )}
                                }
                            }
                        }
                    }
                }
            }

            // Add new entry
            div { class: "cfg-add-entry",
                input {
                    r#type: "text",
                    class: "cfg-input",
                    placeholder: "New entry name...",
                    value: "{new_entry_key}",
                    oninput: move |e| new_entry_key.set(e.value()),
                }
                button {
                    class: "btn btn--sm primary",
                    disabled: new_entry_key().trim().is_empty(),
                    onclick: move |_| {
                        let key = new_entry_key().trim().to_string();
                        if !key.is_empty() {
                            let mut form = form_signal.write();
                            let section = form.entry(sk.clone()).or_insert_with(|| json!({}));
                            if let Some(obj) = section.as_object_mut() {
                                if !obj.contains_key(&key) {
                                    // Build default from schema
                                    let default_val = build_default_from_schema(&es);
                                    obj.insert(key, default_val);
                                }
                            }
                            new_entry_key.set(String::new());
                        }
                    },
                    "+ Add"
                }
            }
        }
    }
}

fn render_entry_fields(
    section_key: &str,
    entry_key: &str,
    entry_schema: &serde_json::Value,
    entry_val: &serde_json::Value,
    mut form_signal: Signal<HashMap<String, serde_json::Value>>,
    ui_hints: &HashMap<String, serde_json::Value>,
) -> Element {
    let props = entry_schema.get("properties").and_then(|p| p.as_object());
    let val_obj = entry_val.as_object();

    rsx! {
        div { class: "cfg-fields",
            if let Some(fields) = props {
                for (field_key, field_schema) in fields.iter() {
                    {
                        let field_val = val_obj.and_then(|o| o.get(field_key)).cloned();
                        let path_parts_owned = vec![entry_key, field_key.as_str()];
                        render_field(
                            section_key,
                            &path_parts_owned,
                            field_key,
                            field_schema,
                            field_val,
                            form_signal,
                            ui_hints,
                        )
                    }
                }
            } else {
                // No schema properties — render value as JSON text
                {
                    let str_val = serde_json::to_string_pretty(entry_val).unwrap_or_default();
                    let sk = section_key.to_string();
                    let ek = entry_key.to_string();
                    rsx! {
                        div { class: "cfg-field",
                            label { class: "cfg-field__label", "Value (JSON)" }
                            div { class: "cfg-input-wrap",
                                input {
                                    r#type: "text",
                                    class: "cfg-input",
                                    value: "{str_val}",
                                    oninput: move |e| {
                                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&e.value()) {
                                            let mut form = form_signal.write();
                                            let section = form.entry(sk.clone()).or_insert_with(|| json!({}));
                                            if let Some(obj) = section.as_object_mut() {
                                                obj.insert(ek.clone(), parsed);
                                            }
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ============== Helpers ==============

/// Reverts a single diff entry by path (e.g. "channels.discord.bot_token")
/// back to its original value in the form signal.
fn revert_field(
    path: &str,
    original_value: &Option<serde_json::Value>,
    mut form_signal: Signal<HashMap<String, serde_json::Value>>,
) {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return;
    }

    let section_key = parts[0];
    let mut form = form_signal.write();

    if parts.len() == 1 {
        // Top-level key
        match original_value {
            Some(val) => {
                form.insert(section_key.to_string(), val.clone());
            }
            None => {
                form.remove(section_key);
            }
        }
        return;
    }

    // Nested path: navigate to the parent and set/remove the leaf
    let section = match form.get_mut(section_key) {
        Some(s) => s,
        None => {
            // Section doesn't exist; if we're reverting to a value, we need to rebuild
            if let Some(val) = original_value {
                let mut root = serde_json::Value::Object(serde_json::Map::new());
                set_nested_by_str_parts(&mut root, &parts[1..], val.clone());
                form.insert(section_key.to_string(), root);
            }
            return;
        }
    };

    match original_value {
        Some(val) => {
            set_nested_by_str_parts(section, &parts[1..], val.clone());
        }
        None => {
            remove_nested_by_str_parts(section, &parts[1..]);
        }
    }
}

fn set_nested_by_str_parts(
    target: &mut serde_json::Value,
    path: &[&str],
    value: serde_json::Value,
) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        if let Some(obj) = target.as_object_mut() {
            obj.insert(path[0].to_string(), value);
        }
        return;
    }
    let obj = match target.as_object_mut() {
        Some(obj) => obj,
        None => return,
    };
    let child = obj.entry(path[0].to_string()).or_insert_with(|| json!({}));
    set_nested_by_str_parts(child, &path[1..], value);
}

fn remove_nested_by_str_parts(target: &mut serde_json::Value, path: &[&str]) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        if let Some(obj) = target.as_object_mut() {
            obj.remove(path[0]);
        }
        return;
    }
    let obj = match target.as_object_mut() {
        Some(obj) => obj,
        None => return,
    };
    if let Some(child) = obj.get_mut(path[0]) {
        remove_nested_by_str_parts(child, &path[1..]);
    }
}

fn update_nested_value(
    form_signal: &mut Signal<HashMap<String, serde_json::Value>>,
    section_key: &str,
    path: &[String],
    value: serde_json::Value,
) {
    let mut form = form_signal.write();
    if path.is_empty() {
        form.insert(section_key.to_string(), value);
        return;
    }
    let section = form
        .entry(section_key.to_string())
        .or_insert_with(|| json!({}));

    if path.len() == 1 {
        if let Some(obj) = section.as_object_mut() {
            obj.insert(path[0].clone(), value);
        }
    } else {
        set_nested(section, &path, value);
    }
}

fn set_nested(target: &mut serde_json::Value, path: &[String], value: serde_json::Value) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        if let Some(obj) = target.as_object_mut() {
            obj.insert(path[0].clone(), value);
        }
        return;
    }
    let obj = match target.as_object_mut() {
        Some(obj) => obj,
        None => return,
    };
    let child = obj.entry(path[0].clone()).or_insert_with(|| json!({}));
    set_nested(child, &path[1..], value);
}

fn get_nested_value(
    form: &HashMap<String, serde_json::Value>,
    section_key: &str,
    path: &[String],
) -> Option<serde_json::Value> {
    let mut current = form.get(section_key)?;
    for key in path {
        current = current.get(key)?;
    }
    Some(current.clone())
}

fn build_default_from_schema(schema: &serde_json::Value) -> serde_json::Value {
    let resolved_schema = resolve_schema_variant(schema, None);
    let field_type = schema_primary_type(&resolved_schema).unwrap_or_else(|| "object".to_string());
    if let Some(default) = resolved_schema.get("default") {
        return default.clone();
    }
    match field_type.as_str() {
        "object" => {
            let mut obj = serde_json::Map::new();
            if let Some(props) = resolved_schema
                .get("properties")
                .and_then(|p| p.as_object())
            {
                for (key, prop_schema) in props {
                    if let Some(default) = prop_schema.get("default") {
                        obj.insert(key.clone(), default.clone());
                    }
                }
            }
            serde_json::Value::Object(obj)
        }
        "boolean" => json!(false),
        "integer" | "number" => json!(0),
        "string" => json!(""),
        "array" => json!([]),
        _ => json!({}),
    }
}

// ============== Config Import / Export ==============

fn export_config_json(json_str: &str) {
    if let Some(window) = web_sys::window() {
        let blob_opts = web_sys::BlobPropertyBag::new();
        blob_opts.set_type("application/json");
        if let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(
            &js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(json_str)),
            &blob_opts,
        ) {
            if let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) {
                if let Some(doc) = window.document() {
                    if let Ok(a) = doc.create_element("a") {
                        let _ = a.set_attribute("href", &url);
                        let _ = a.set_attribute("download", "savfox-config.json");
                        let _ = a.set_attribute("style", "display:none");
                        if let Some(body) = doc.body() {
                            let _ = body.append_child(&a);
                            if let Some(el) = a.dyn_ref::<web_sys::HtmlElement>() {
                                el.click();
                            }
                            let _ = body.remove_child(&a);
                        }
                        let _ = web_sys::Url::revoke_object_url(&url);
                    }
                }
            }
        }
    }
}

fn import_config_json(
    mut form_value: Signal<HashMap<String, serde_json::Value>>,
    mut error_msg: Signal<Option<String>>,
) {
    if let Some(window) = web_sys::window() {
        if let Some(doc) = window.document() {
            if let Ok(input) = doc.create_element("input") {
                let _ = input.set_attribute("type", "file");
                let _ = input.set_attribute("accept", ".json,application/json");
                let _ = input.set_attribute("style", "display:none");

                let cb = Closure::wrap(Box::new(move |event: web_sys::Event| {
                    let target = match event.target() {
                        Some(t) => t,
                        None => return,
                    };
                    let input: web_sys::HtmlInputElement = match target.dyn_into() {
                        Ok(i) => i,
                        Err(_) => return,
                    };
                    let files = match input.files() {
                        Some(f) => f,
                        None => return,
                    };
                    let file = match files.get(0) {
                        Some(f) => f,
                        None => return,
                    };

                    let reader = match web_sys::FileReader::new() {
                        Ok(r) => r,
                        Err(_) => return,
                    };

                    let reader_clone = reader.clone();
                    let onload = Closure::wrap(Box::new(move |_: web_sys::Event| {
                        if let Ok(result) = reader_clone.result() {
                            if let Some(text) = result.as_string() {
                                match serde_json::from_str::<HashMap<String, serde_json::Value>>(
                                    &text,
                                ) {
                                    Ok(parsed) => {
                                        form_value.set(parsed);
                                        error_msg.set(None);
                                    }
                                    Err(e) => {
                                        error_msg.set(Some(format!("Invalid JSON: {e}")));
                                    }
                                }
                            }
                        }
                    })
                        as Box<dyn FnMut(web_sys::Event)>);

                    reader.set_onload(Some(onload.as_ref().unchecked_ref()));
                    onload.forget();
                    let _ = reader.read_as_text(&file);

                    // Clean up the input element
                    if let Some(parent) = input.parent_node() {
                        let _ = parent.remove_child(&input);
                    }
                }) as Box<dyn FnMut(web_sys::Event)>);

                let _ =
                    input.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref());
                cb.forget();

                if let Some(body) = doc.body() {
                    let _ = body.append_child(&input);
                    if let Some(el) = input.dyn_ref::<web_sys::HtmlElement>() {
                        el.click();
                    }
                }
            }
        }
    }
}

/// Async sleep for WASM using `setTimeout`.
async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
