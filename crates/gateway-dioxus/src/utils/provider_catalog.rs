use std::collections::{BTreeMap, HashMap};

use crate::api::types::ModelInfo;
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

fn normalize_model_id(model: &ModelInfo, provider_id: &str) -> String {
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

pub fn build_provider_catalog(models: &[ModelInfo]) -> ProviderCatalog {
    let mut providers_map: BTreeMap<String, ProviderItem> = BTreeMap::new();

    for provider_id in known_provider_ids().iter() {
        providers_map
            .entry((*provider_id).to_string())
            .or_insert_with(|| ProviderItem {
                id: (*provider_id).to_string(),
                name: provider_display_name(provider_id),
                source: "custom".to_string(),
                env: vec![],
                key: None,
                options: HashMap::new(),
                models: BTreeMap::new(),
            });
    }

    for model in models.iter() {
        let provider_id = normalize_provider_id(model.provider.as_deref(), &model.id);
        let provider_name = provider_display_name(&provider_id);

        let provider = providers_map
            .entry(provider_id.clone())
            .or_insert_with(|| ProviderItem {
                id: provider_id.clone(),
                name: provider_name,
                source: "custom".to_string(),
                env: vec![],
                key: None,
                options: HashMap::new(),
                models: BTreeMap::new(),
            });

        let model_id = normalize_model_id(model, &provider_id);
        let model_name = model
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| model_id.as_str())
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
        provider_sort_rank(&a.id)
            .cmp(&provider_sort_rank(&b.id))
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
