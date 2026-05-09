use std::collections::{BTreeMap, HashMap};

use crate::api::types::AvailableModel;
use crate::utils::provider_registry::{
    canonical_provider_id, known_provider_ids,
    provider_display_name as registry_provider_display_name,
    provider_sort_rank as registry_provider_sort_rank,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderModelItem {
    pub model_id: String,
    pub full_id: String,
    pub name: String,
    pub model_slug: Option<String>,
    pub base_url: Option<String>,
    pub builtin: bool,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderItem {
    pub id: String,
    pub name: String,
    /// Account slug for multi-account display (e.g. "work-account").
    /// Empty for single-account / legacy entries.
    pub account_slug: String,
    pub source: String,
    pub env: Vec<String>,
    pub key: Option<String>,
    pub options: HashMap<String, String>,
    pub models: BTreeMap<String, ProviderModelItem>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProviderCatalog {
    pub all: Vec<ProviderItem>,
    pub default: HashMap<String, String>,
    pub connected: Vec<String>,
}

pub fn provider_display_name(provider_id: &str) -> String {
    registry_provider_display_name(provider_id)
}

pub fn provider_sort_rank(provider_id: &str) -> usize {
    registry_provider_sort_rank(provider_id)
}

pub fn model_display_name(model: &ProviderModelItem) -> String {
    model.name.trim().to_string()
}

pub fn normalize_provider_id(provider: Option<&str>, full_model_id: &str) -> String {
    let from_provider = provider.unwrap_or("").trim();
    if !from_provider.is_empty() {
        return canonical_provider_id(from_provider);
    }
    if let Some((prefix, _)) = full_model_id.split_once('/') {
        let normalized = prefix.trim();
        if !normalized.is_empty() {
            return canonical_provider_id(normalized);
        }
    }
    "other".to_string()
}

fn base_provider_id(account_id: &str, account_slug: &str) -> String {
    let trimmed_account_id = account_id.trim();
    let trimmed_slug = account_slug.trim();
    if !trimmed_slug.is_empty()
        && let Some(base) = trimmed_account_id.strip_suffix(&format!("-{trimmed_slug}"))
    {
        return canonical_provider_id(base);
    }
    canonical_provider_id(trimmed_account_id)
}

fn normalize_model_id(model: &AvailableModel, provider_id: &str) -> String {
    if let Some(model_slug) = model
        .model_slug
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return model_slug.to_string();
    }

    let id = model.id.trim();
    if let Some((prefix, rest)) = id.split_once('/') {
        if canonical_provider_id(prefix) == provider_id {
            let rest = rest.trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }

    if let Some(rest) = id.strip_prefix(&format!("{provider_id}/")) {
        let rest = rest.trim();
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    id.to_string()
}

fn infer_provider_source(provider: &ProviderItem) -> String {
    let has_non_builtin = provider.models.values().any(|model| !model.builtin);
    if has_non_builtin {
        "config".to_string()
    } else {
        "custom".to_string()
    }
}

pub fn build_provider_catalog(models: &[AvailableModel]) -> ProviderCatalog {
    let mut providers_map: BTreeMap<String, ProviderItem> = BTreeMap::new();

    for provider_id in known_provider_ids().iter() {
        providers_map
            .entry((*provider_id).to_string())
            .or_insert_with(|| ProviderItem {
                id: (*provider_id).to_string(),
                name: provider_display_name(provider_id),
                account_slug: String::new(),
                source: "custom".to_string(),
                env: vec![],
                key: None,
                options: HashMap::new(),
                models: BTreeMap::new(),
            });
    }

    for model in models.iter() {
        // Use the raw account_id (model.provider) as the grouping key so that
        // multiple accounts for the same provider appear as separate groups.
        let account_id = model
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| normalize_provider_id(None, &model.id));
        let account_slug = model
            .account_slug
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("")
            .to_string();
        let base_provider_id = base_provider_id(&account_id, &account_slug);
        let base_name = model
            .provider_name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .filter(|_| model.account_name.is_some() || account_slug.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| provider_display_name(&base_provider_id));
        let account_name = model
            .account_name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                if account_slug.is_empty() {
                    None
                } else {
                    model
                        .provider_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .filter(|legacy| !legacy.eq_ignore_ascii_case(base_name.as_str()))
                        .map(ToString::to_string)
                }
            })
            .unwrap_or_else(|| account_slug.clone());
        let display_name = if account_name.is_empty() {
            base_name.clone()
        } else {
            format!("{base_name} / {account_name}")
        };

        let provider = providers_map
            .entry(account_id.clone())
            .or_insert_with(|| ProviderItem {
                id: account_id.clone(),
                name: display_name.clone(),
                account_slug: account_slug.clone(),
                source: "custom".to_string(),
                env: vec![],
                key: None,
                options: HashMap::new(),
                models: BTreeMap::new(),
            });
        // Pre-populated known-provider entries have empty account_slug / generic
        // name.  Update them with the richer data from the model config file.
        if provider.account_slug.is_empty() && !account_slug.is_empty() {
            provider.account_slug = account_slug.clone();
            provider.name = display_name;
        }

        let model_id = normalize_model_id(model, &account_id);
        let model_name = model
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(model_id.as_str())
            .to_string();

        let mut key = model_id.clone();
        if provider.models.contains_key(&key) {
            key = model.id.clone();
        }

        provider.models.insert(
            key.clone(),
            ProviderModelItem {
                model_id: key,
                full_id: model.id.clone(),
                name: model_name,
                model_slug: model.model_slug.clone(),
                base_url: model.base_url.clone(),
                builtin: model.builtin.unwrap_or(false),
                is_default: model.is_default.unwrap_or(false),
            },
        );
    }

    let mut all: Vec<ProviderItem> = providers_map.into_values().collect();
    for provider in all.iter_mut() {
        provider.source = infer_provider_source(provider);
    }
    all.sort_by(|a, b| {
        let a_base = base_provider_id(&a.id, &a.account_slug);
        let b_base = base_provider_id(&b.id, &b.account_slug);
        provider_sort_rank(&a_base)
            .cmp(&provider_sort_rank(&b_base))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut default_map = HashMap::new();
    for provider in all.iter() {
        let mut entries = provider.models.values().collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            model_display_name(a)
                .cmp(&model_display_name(b))
                .then_with(|| a.full_id.cmp(&b.full_id))
        });
        let selected = entries
            .iter()
            .find(|entry| entry.is_default)
            .copied()
            .or_else(|| entries.first().copied());
        if let Some(model) = selected {
            default_map.insert(provider.id.clone(), model.model_id.clone());
        }
    }

    let connected = all
        .iter()
        .filter(|provider| !provider.models.is_empty())
        .map(|provider| provider.id.clone())
        .collect::<Vec<_>>();

    ProviderCatalog {
        all,
        default: default_map,
        connected,
    }
}

pub fn find_model_by_full_id<'a>(
    catalog: &'a ProviderCatalog,
    full_id: &str,
) -> Option<(&'a ProviderItem, &'a ProviderModelItem)> {
    for provider in catalog.all.iter() {
        for model in provider.models.values() {
            if model.full_id == full_id {
                return Some((provider, model));
            }
        }
    }
    None
}

pub fn first_default_full_id(catalog: &ProviderCatalog) -> Option<String> {
    for provider in catalog.all.iter() {
        if let Some(default_model_id) = catalog.default.get(&provider.id) {
            if let Some(model) = provider.models.get(default_model_id) {
                return Some(model.full_id.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::build_provider_catalog;
    use crate::api::types::AvailableModel;

    fn test_model(
        provider: &str,
        provider_name: Option<&str>,
        account_name: Option<&str>,
        account_slug: Option<&str>,
    ) -> AvailableModel {
        AvailableModel {
            id: format!("{provider}/gpt-5"),
            name: Some("GPT-5".to_string()),
            provider: Some(provider.to_string()),
            model_slug: Some("gpt-5".to_string()),
            api_key: None,
            base_url: None,
            max_tokens: None,
            temperature: None,
            is_default: Some(true),
            builtin: Some(false),
            default_reasoning_level: None,
            supported_reasoning_levels: None,
            account_slug: account_slug.map(ToString::to_string),
            provider_name: provider_name.map(ToString::to_string),
            account_name: account_name.map(ToString::to_string),
        }
    }

    #[test]
    fn build_provider_catalog_uses_provider_and_account_names_for_multi_account_entries() {
        let catalog = build_provider_catalog(&[test_model(
            "openai-jalen",
            Some("OpenAI"),
            Some("jalen"),
            Some("jalen"),
        )]);

        let provider = catalog
            .all
            .iter()
            .find(|provider| provider.id == "openai-jalen")
            .expect("provider should exist");
        assert_eq!(provider.name, "OpenAI / jalen");
    }

    #[test]
    fn build_provider_catalog_preserves_legacy_account_name_payloads() {
        let catalog = build_provider_catalog(&[test_model(
            "openai-jalen",
            Some("jalen"),
            None,
            Some("jalen"),
        )]);

        let provider = catalog
            .all
            .iter()
            .find(|provider| provider.id == "openai-jalen")
            .expect("provider should exist");
        assert_eq!(provider.name, "OpenAI / jalen");
    }
}
