use std::path::PathBuf;

use anyhow::Context;
use serde_json::Value;

use super::signer::ContrixKeyRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContrixAccountConfig {
    pub id: String,
    pub principal_id: String,
    pub device_id: String,
    /// Bearer-mode auth: pre-issued `cx.session.grant` access token.
    /// Either this OR `key_ref` MUST be set; if both are set, `key_ref` wins
    /// at startup (`access_token` becomes a runtime cache).
    pub access_token: String,
    /// Phase 8 (T8.A): ed25519 key location for DID-proof login + event
    /// signing. When set, savfox calls `AuthManager::login_did_proof` at
    /// boot to obtain the session grant rather than using a static token.
    pub key_ref: Option<ContrixKeyRef>,
    /// Phase 8: verification method id used by `Ed25519MoveSigner`. Defaults
    /// to `{principal_id}#key-1` when missing.
    pub verification_method: Option<String>,
    /// Phase 8: Contrix server DID used as `audience` for `login_did_proof`.
    /// Defaults to the channel-level `service_did` when missing.
    pub contrix_server_did: Option<String>,
    /// Phase 8: path to a pre-signed `cx.capability.grant` Event JSON.
    /// When set, the event_id is attached as `authorization_ref` on every
    /// outbound write.
    pub grant_event_path: Option<PathBuf>,
    pub default_realm_id: Option<String>,
    pub default_flow_id: Option<String>,
    pub agent_id: Option<String>,
    pub listen: bool,
    pub send: bool,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContrixChannelConfig {
    pub id: String,
    pub base_url: String,
    pub service_did: Option<String>,
    pub accounts: Vec<ContrixAccountConfig>,
}

impl ContrixChannelConfig {
    pub fn from_channel_config(
        config: &savfox_core::config::channel_store::ChannelConfig,
    ) -> Option<Self> {
        if !config.enabled || !config.kind.eq_ignore_ascii_case("contrix") {
            return None;
        }

        let raw = config.config.as_object()?;
        let base_url = first_non_empty(raw, &["baseUrl", "base_url", "homeserver", "url"])?;
        let service_did = first_non_empty(raw, &["serviceDid", "service_did"]);

        let accounts = match raw.get("accounts") {
            Some(value) => parse_accounts(value, raw),
            None => parse_accounts(&Value::Null, raw),
        };

        Some(Self {
            id: config.id.clone(),
            base_url,
            service_did,
            accounts,
        })
    }

    /// Validate that the channel has at least one usable account.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.base_url.trim().is_empty() {
            anyhow::bail!("Contrix channel '{}' missing base_url", self.id);
        }
        if self.accounts.is_empty() {
            anyhow::bail!(
                "Contrix channel '{}' has no accounts; configure at least one controlled account",
                self.id
            );
        }
        for account in &self.accounts {
            account.validate().with_context(|| {
                format!(
                    "Contrix channel '{}' account '{}' is invalid",
                    self.id, account.id
                )
            })?;
        }
        Ok(())
    }

    /// Find an account by id.
    #[must_use]
    pub fn account(&self, account_id: &str) -> Option<&ContrixAccountConfig> {
        self.accounts.iter().find(|a| a.id == account_id)
    }

    /// Pick the account that should send to the given realm. Preference:
    /// 1. An account whose `default_realm_id == realm_id`.
    /// 2. The first account with `send == true`.
    #[must_use]
    pub fn select_send_account(&self, realm_id: &str) -> Option<&ContrixAccountConfig> {
        if let Some(account) = self.accounts.iter().find(|a| {
            a.send
                && a.default_realm_id
                    .as_deref()
                    .is_some_and(|r| r.eq_ignore_ascii_case(realm_id))
        }) {
            return Some(account);
        }
        self.accounts.iter().find(|a| a.send)
    }
}

impl ContrixAccountConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.id.trim().is_empty() {
            anyhow::bail!("Contrix account missing id");
        }
        if self.principal_id.trim().is_empty() {
            anyhow::bail!("Contrix account '{}' missing principal_id (DID)", self.id);
        }
        if self.device_id.trim().is_empty() {
            anyhow::bail!("Contrix account '{}' missing device_id", self.id);
        }
        // Phase 8: either a static access_token OR a key_ref MUST be set.
        // Both is allowed; key_ref takes precedence at startup.
        if self.access_token.trim().is_empty() && self.key_ref.is_none() {
            anyhow::bail!(
                "Contrix account '{}' missing both access_token and key_ref — \
                 set one of them",
                self.id
            );
        }
        if self.send && self.default_realm_id.is_none() {
            anyhow::bail!(
                "Contrix account '{}' has send=true but no default_realm_id",
                self.id
            );
        }
        Ok(())
    }
}

fn parse_accounts(
    accounts_value: &Value,
    parent_raw: &serde_json::Map<String, Value>,
) -> Vec<ContrixAccountConfig> {
    match accounts_value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| parse_account_entry(item.as_object()?))
            .collect(),
        Value::Object(_) | Value::Null => {
            // Allow single-account flat form where principal_id / access_token live at
            // the top of the channel config object — convenient for one-account setups.
            if let Some(account) = parse_account_entry(parent_raw) {
                vec![account]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn parse_account_entry(map: &serde_json::Map<String, Value>) -> Option<ContrixAccountConfig> {
    let principal_id = first_non_empty(map, &["principalId", "principal_id", "did", "user_id"])?;
    let access_token = first_non_empty(
        map,
        &["accessToken", "access_token", "token", "sessionGrant"],
    )
    .unwrap_or_default();
    let key_ref = map
        .get("keyRef")
        .or_else(|| map.get("key_ref"))
        .and_then(ContrixKeyRef::from_value);
    // Caller must have at least one auth path. Reject parse for accounts
    // with neither token nor key_ref; `validate()` reports the precise error.
    if access_token.is_empty() && key_ref.is_none() {
        return None;
    }
    let id = first_non_empty(map, &["id", "accountId", "account_id"])
        .unwrap_or_else(|| principal_id.clone());
    let device_id = first_non_empty(map, &["deviceId", "device_id"]).unwrap_or_default();
    let default_realm_id = first_non_empty(map, &["defaultRealmId", "default_realm_id", "realmId"]);
    let default_flow_id = first_non_empty(map, &["defaultFlowId", "default_flow_id", "flowId"]);
    let agent_id = first_non_empty(map, &["agentId", "agent_id", "agent"]);
    let verification_method = first_non_empty(
        map,
        &[
            "verificationMethod",
            "verification_method",
            "verificationMethodId",
        ],
    );
    let contrix_server_did = first_non_empty(map, &["contrixServerDid", "contrix_server_did"]);
    let grant_event_path =
        first_non_empty(map, &["grantEventPath", "grant_event_path"]).map(PathBuf::from);

    let listen = map.get("listen").and_then(Value::as_bool).unwrap_or(true);
    let send = map.get("send").and_then(Value::as_bool).unwrap_or(true);
    let scopes = parse_string_list(map.get("scopes"));

    Some(ContrixAccountConfig {
        id,
        principal_id,
        device_id,
        access_token,
        key_ref,
        verification_method,
        contrix_server_did,
        grant_event_path,
        default_realm_id,
        default_flow_id,
        agent_id,
        listen,
        send,
        scopes,
    })
}

fn first_non_empty(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| {
            let text = value.as_str()?.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_owned())
            }
        })
    })
}

fn parse_string_list(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::String(text) => text
            .split([',', '\n'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Load all configured Contrix channels from `savfox_home/channels/*.json`.
pub async fn load_contrix_channel_configs(
    savfox_home: &PathBuf,
) -> anyhow::Result<Vec<ContrixChannelConfig>> {
    let all_configs = savfox_core::config::channel_store::list_channel_configs(savfox_home)
        .await
        .context("failed to load channel configs for contrix")?;
    Ok(all_configs
        .iter()
        .filter_map(ContrixChannelConfig::from_channel_config)
        .collect())
}

/// Resolve an outbound (send) account for a realm.
///
/// Iterates configured Contrix channels and returns the first
/// `(channel, account)` pair whose [`ContrixChannelConfig::select_send_account`]
/// matches the realm.
pub async fn resolve_contrix_outbound_account(
    savfox_home: &PathBuf,
    realm_id: &str,
) -> anyhow::Result<Option<(ContrixChannelConfig, ContrixAccountConfig)>> {
    let channels = load_contrix_channel_configs(savfox_home).await?;
    for channel in channels {
        if channel.validate().is_err() {
            continue;
        }
        if let Some(account) = channel.select_send_account(realm_id) {
            let account = account.clone();
            return Ok(Some((channel, account)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use savfox_core::config::channel_store::ChannelConfig;
    use serde_json::json;

    fn make_channel_config(body: Value) -> ChannelConfig {
        ChannelConfig {
            id: "contrix-test".into(),
            kind: "contrix".into(),
            slug: "test".into(),
            name: "Test".into(),
            enabled: true,
            config: body,
            router: None,
            dm_policy: None,
            group_policy: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn parses_multi_account_array() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://contrix.example.org",
            "serviceDid": "did:webvh:contrix.example.org",
            "accounts": [
                {
                    "id": "support",
                    "principalId": "did:webvh:example.org:agents:support-1",
                    "deviceId": "cx:device:01904100-0000-7000-8000-000000000001",
                    "accessToken": "tok-1",
                    "defaultRealmId": "cx:realm:abc",
                    "defaultFlowId": "cx:flow:def",
                    "agentId": "support",
                    "listen": true,
                    "send": true
                },
                {
                    "id": "billing",
                    "principalId": "did:webvh:example.org:agents:billing-1",
                    "deviceId": "cx:device:01904100-0000-7000-8000-000000000002",
                    "accessToken": "tok-2",
                    "defaultRealmId": "cx:realm:billing",
                    "listen": true,
                    "send": false
                }
            ]
        }));
        let parsed = ContrixChannelConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(parsed.base_url, "https://contrix.example.org");
        assert_eq!(
            parsed.service_did.as_deref(),
            Some("did:webvh:contrix.example.org")
        );
        assert_eq!(parsed.accounts.len(), 2);
        assert_eq!(parsed.accounts[0].id, "support");
        assert_eq!(parsed.accounts[1].send, false);
        parsed.validate().expect("validation");
    }

    #[test]
    fn parses_single_account_flat_form() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://contrix.example.org",
            "principalId": "did:webvh:example.org:bot",
            "deviceId": "cx:device:bot-1",
            "accessToken": "tok",
            "defaultRealmId": "cx:realm:r1"
        }));
        let parsed = ContrixChannelConfig::from_channel_config(&cfg).expect("parse");
        assert_eq!(parsed.accounts.len(), 1);
        assert_eq!(parsed.accounts[0].principal_id, "did:webvh:example.org:bot");
        parsed.validate().expect("validate");
    }

    #[test]
    fn disabled_returns_none() {
        let mut cfg = make_channel_config(json!({
            "baseUrl": "https://contrix.example.org",
            "principalId": "did:webvh:example.org:bot",
            "accessToken": "tok"
        }));
        cfg.enabled = false;
        assert!(ContrixChannelConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn missing_access_token_rejects_validation() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://contrix.example.org",
            "accounts": [{
                "id": "x",
                "principalId": "did:webvh:example.org:x",
                "deviceId": "cx:device:x",
                "accessToken": ""
            }]
        }));
        // accessToken empty → parse_account_entry returns None → accounts empty.
        let parsed = ContrixChannelConfig::from_channel_config(&cfg).expect("parse");
        assert!(parsed.accounts.is_empty());
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn missing_base_url_rejects_parse() {
        let cfg = make_channel_config(json!({
            "accounts": [{
                "principalId": "did:webvh:example.org:x",
                "accessToken": "tok"
            }]
        }));
        assert!(ContrixChannelConfig::from_channel_config(&cfg).is_none());
    }

    #[test]
    fn select_send_account_prefers_default_realm_match() {
        let cfg = make_channel_config(json!({
            "baseUrl": "https://x.example",
            "accounts": [
                {"id":"a","principalId":"did:webvh:a","deviceId":"d","accessToken":"t","defaultRealmId":"cx:realm:1"},
                {"id":"b","principalId":"did:webvh:b","deviceId":"d","accessToken":"t","defaultRealmId":"cx:realm:2"}
            ]
        }));
        let parsed = ContrixChannelConfig::from_channel_config(&cfg).expect("parse");
        let chosen = parsed.select_send_account("cx:realm:2").expect("match");
        assert_eq!(chosen.id, "b");
    }
}
