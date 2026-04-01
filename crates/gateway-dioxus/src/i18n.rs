use std::collections::HashMap;
use std::sync::LazyLock;

use dioxus::prelude::*;

/// Supported locales.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Locale {
    #[default]
    En,
    ZhCn,
}

impl Locale {
    /// All supported locales.
    pub const ALL: &[Locale] = &[Locale::En, Locale::ZhCn];

    /// BCP 47 language tag.
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }

    /// Human-readable label in the locale's own language.
    pub fn label(self) -> &'static str {
        match self {
            Self::En => "EN",
            Self::ZhCn => "中文",
        }
    }

    /// Parse a BCP 47 tag (or prefix) into a [`Locale`].
    pub fn from_code(code: &str) -> Self {
        let lower = code.to_lowercase();
        if lower.starts_with("zh") {
            Self::ZhCn
        } else {
            Self::En
        }
    }
}

// ── Translation storage ─────────────────────────────────────────────────

type TranslationMap = HashMap<String, String>;

static EN: LazyLock<TranslationMap> =
    LazyLock::new(|| parse_flat_toml(include_str!("../locales/en.toml")));

static ZH_CN: LazyLock<TranslationMap> =
    LazyLock::new(|| parse_flat_toml(include_str!("../locales/zh-CN.toml")));

fn parse_flat_toml(content: &str) -> TranslationMap {
    let table: toml::Table = content.parse().expect("invalid translations TOML");
    let mut map = HashMap::new();
    flatten("", &table, &mut map);
    map
}

fn flatten(prefix: &str, table: &toml::Table, out: &mut TranslationMap) {
    for (key, value) in table {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::String(s) => {
                out.insert(full, s.clone());
            }
            toml::Value::Table(t) => {
                flatten(&full, t, out);
            }
            _ => {}
        }
    }
}

fn translations(locale: Locale) -> &'static TranslationMap {
    match locale {
        Locale::En => &EN,
        Locale::ZhCn => &ZH_CN,
    }
}

/// Look up a translation key for the given locale.
/// Returns the key itself if no translation is found.
pub fn t(locale: Locale, key: &str) -> String {
    translations(locale)
        .get(key)
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// Look up a translation key and replace `{name}` placeholders.
pub fn t_args(locale: Locale, key: &str, args: &[(&str, &str)]) -> String {
    let mut result = t(locale, key);
    for (name, value) in args {
        result = result.replace(&format!("{{{name}}}"), value);
    }
    result
}

// ── Dioxus integration ──────────────────────────────────────────────────

/// Dioxus hook: returns the current [`Locale`] signal and a translator closure.
///
/// Components that call this hook will re-render when the locale changes.
pub fn use_i18n() -> (Signal<Locale>, impl Fn(&str) -> String) {
    let locale_sig = use_context::<Signal<Locale>>();
    let locale = locale_sig();
    let tr = move |key: &str| -> String { t(locale, key) };
    (locale_sig, tr)
}

// ── Locale persistence (localStorage) ───────────────────────────────────

const STORAGE_KEY: &str = "savfox_locale";

/// Detect the initial locale: localStorage → browser language → English.
pub fn detect_locale() -> Locale {
    if let Some(saved) = read_saved_locale() {
        return Locale::from_code(&saved);
    }
    if let Some(lang) = browser_language() {
        return Locale::from_code(&lang);
    }
    Locale::default()
}

/// Persist the locale choice to localStorage.
pub fn save_locale(locale: Locale) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(storage) = web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
        {
            let _ = storage.set_item(STORAGE_KEY, locale.code());
        }
    }
}

fn read_saved_locale() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        return web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
            .and_then(|s| s.get_item(STORAGE_KEY).ok())
            .flatten()
            .filter(|v| !v.is_empty());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

fn browser_language() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        return web_sys::window().and_then(|w| w.navigator().language());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}
