use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub type IdentityLinks = HashMap<String, Vec<String>>;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct IdentityLinkUpdateResult {
    pub canonical: String,
    pub added: usize,
    pub moved_from: Vec<String>,
    pub total: usize,
}

fn identity_links_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("identity-links.json")
}

#[must_use]
pub fn normalize_identity(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

#[must_use]
pub fn normalize_peer(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

#[must_use]
pub fn platform_peer(platform: &str, peer_id: &str) -> Option<String> {
    let platform = platform.trim();
    let peer_id = peer_id.trim();
    if platform.is_empty() || peer_id.is_empty() {
        None
    } else {
        normalize_peer(&format!("{platform}:{peer_id}"))
    }
}

fn dedupe_sorted(values: &mut Vec<String>) {
    values.retain(|v| !v.trim().is_empty());
    values.sort();
    values.dedup();
}

#[must_use]
pub fn normalize_links(raw: IdentityLinks) -> IdentityLinks {
    let mut normalized = IdentityLinks::new();
    for (canonical, peers) in raw {
        let Some(canonical) = normalize_identity(&canonical) else {
            continue;
        };
        let mut cleaned: Vec<String> = peers
            .into_iter()
            .filter_map(|peer| normalize_peer(&peer))
            .collect();
        dedupe_sorted(&mut cleaned);
        if cleaned.is_empty() {
            continue;
        }
        normalized
            .entry(canonical)
            .or_default()
            .extend(cleaned.into_iter());
    }

    for peers in normalized.values_mut() {
        dedupe_sorted(peers);
    }
    normalized.retain(|_, peers| !peers.is_empty());
    normalized
}

#[must_use]
pub fn canonical_for_peer(links: &IdentityLinks, peer: &str) -> Option<String> {
    let peer = normalize_peer(peer)?;
    links
        .iter()
        .find(|(_, peers)| peers.iter().any(|candidate| candidate == &peer))
        .map(|(identity, _)| identity.clone())
}

#[must_use]
pub fn peers_for_identity(links: &IdentityLinks, identity: &str) -> Vec<String> {
    let Some(identity) = normalize_identity(identity) else {
        return Vec::new();
    };
    links.get(&identity).cloned().unwrap_or_default()
}

pub fn upsert_link(
    links: &mut IdentityLinks,
    canonical: &str,
    peers: &[String],
) -> Option<IdentityLinkUpdateResult> {
    let canonical = normalize_identity(canonical)?;
    let mut requested: HashSet<String> = peers
        .iter()
        .filter_map(|peer| normalize_peer(peer))
        .collect();
    if requested.is_empty() {
        return None;
    }

    let mut moved_from = Vec::new();
    let mut empty_keys = Vec::new();
    for (identity, existing_peers) in links.iter_mut() {
        if identity == &canonical {
            continue;
        }
        let before = existing_peers.len();
        existing_peers.retain(|peer| !requested.contains(peer));
        if existing_peers.len() < before {
            moved_from.push(identity.clone());
        }
        if existing_peers.is_empty() {
            empty_keys.push(identity.clone());
        }
    }
    for key in empty_keys {
        links.remove(&key);
    }

    let entry = links.entry(canonical.clone()).or_default();
    let before = entry.len();
    entry.extend(requested.drain());
    dedupe_sorted(entry);
    let added = entry.len().saturating_sub(before);

    Some(IdentityLinkUpdateResult {
        canonical,
        added,
        moved_from,
        total: entry.len(),
    })
}

pub async fn load_identity_links(savfox_home: &Path) -> IdentityLinks {
    let path = identity_links_path(savfox_home);
    let Ok(content) = tokio::fs::read_to_string(path).await else {
        return IdentityLinks::new();
    };
    let parsed = serde_json::from_str::<IdentityLinks>(&content).unwrap_or_default();
    normalize_links(parsed)
}

pub async fn save_identity_links(savfox_home: &Path, links: &IdentityLinks) -> anyhow::Result<()> {
    let path = identity_links_path(savfox_home);
    let normalized = normalize_links(links.clone());
    let content = serde_json::to_string_pretty(&normalized)?;
    tokio::fs::write(path, content).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{IdentityLinks, canonical_for_peer, normalize_links, upsert_link};

    #[test]
    fn upsert_moves_peer_from_other_identity() {
        let mut links: IdentityLinks = serde_json::from_value(serde_json::json!({
            "alice": ["discord:111", "slack:u1"],
            "bob": ["discord:222"]
        }))
        .expect("parse");

        let summary = upsert_link(
            &mut links,
            "bob",
            &[String::from("discord:111"), String::from("telegram:333")],
        )
        .expect("summary");

        assert_eq!(summary.canonical, "bob");
        assert_eq!(summary.total, 3);
        assert!(summary.moved_from.contains(&String::from("alice")));
        assert_eq!(
            canonical_for_peer(&links, "discord:111").as_deref(),
            Some("bob")
        );
    }

    #[test]
    fn normalize_links_dedupes_and_lowercases() {
        let links: IdentityLinks = serde_json::from_value(serde_json::json!({
            "Chris": ["Discord:ABC", "discord:abc", "  "]
        }))
        .expect("parse");

        let normalized = normalize_links(links);
        let peers = normalized.get("chris").expect("identity exists");
        assert_eq!(peers, &vec![String::from("discord:abc")]);
    }
}
