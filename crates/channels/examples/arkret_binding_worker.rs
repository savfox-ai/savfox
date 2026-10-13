//! Owned conformance worker for real platform keys and channel persistence.
//! No private runtime seed crosses this process boundary.

use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use savfox_channels::arkret::{
    ArkretChannelConfig, ArkretKeyRef, build_arkret_runtime_key_request_json,
    default_agent_runtime_scope, delete_ed25519_key_ref_from_keyring,
    get_or_generate_ed25519_key_ref_in_keyring,
};
use savfox_core::config::channel_store::{
    ChannelConfig, delete_channel_config, get_channel_config, save_channel_config,
};
use serde_json::json;

fn main() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 6,
        "expected operation/home/bootstrap/output/owned-vault-service"
    );
    let operation = &args[1];
    let home = PathBuf::from(&args[2]);
    let input: serde_json::Value = serde_json::from_slice(&std::fs::read(&args[3])?)?;
    let bootstrap: arkret::AgentPairingBootstrap =
        serde_json::from_value(input["bootstrap"].clone())?;
    let controller: arkret::AccountId = serde_json::from_value(input["controller"].clone())?;
    let output = PathBuf::from(&args[4]);
    let service = &args[5];
    ensure!(
        service.starts_with("cotest-agent-binding-"),
        "worker requires its owned vault namespace"
    );
    let channel_id = "arkret-binding-worker";
    let account = bootstrap.pairing_request_id.to_string();
    let existing = get_channel_config(&home, channel_id).await?;
    if operation == "cleanup" {
        delete_ed25519_key_ref_from_keyring(&ArkretKeyRef::Keyring {
            service: service.clone(),
            account,
        })?;
        delete_channel_config(&home, channel_id).await?;
        std::fs::write(output, b"{\"cleaned\":true}")?;
        return Ok(());
    }
    let config = match operation.as_str() {
        "create" | "recreate" => {
            if operation == "recreate" {
                ensure!(existing.is_some(), "recreate requires a saved channel");
                ensure!(
                    delete_channel_config(&home, channel_id).await?,
                    "actual channel deletion failed"
                );
                ensure!(
                    get_channel_config(&home, channel_id).await?.is_none(),
                    "deleted channel remained visible"
                );
            } else {
                ensure!(existing.is_none(), "create cannot replace a saved channel");
            }
            let method = bootstrap
                .validated_runtime_identity()
                .map_err(anyhow::Error::msg)?
                .verification_method
                .clone();
            let key = get_or_generate_ed25519_key_ref_in_keyring(service.clone(), account)?;
            let config = ChannelConfig {
                id: channel_id.into(),
                kind: "arkret".into(),
                slug: "binding-worker".into(),
                name: "Binding worker".into(),
                enabled: true,
                config: json!({"mode":"agent", "inksonBootstrap":bootstrap,
                    "keyRef":key,"verificationMethod":method,"controllerAccountId":controller,
                    "requestedScope":default_agent_runtime_scope().map_err(anyhow::Error::msg)?}),
                router: None,
                dm_policy: None,
                group_policy: None,
                created_at: None,
                updated_at: None,
            };
            save_channel_config(&home, &config).await?;
            get_channel_config(&home, channel_id)
                .await?
                .context("saved channel missing")?
        }
        "load" | "lose_key" => existing.context("worker channel missing")?,
        _ => anyhow::bail!("unknown owned binding worker operation"),
    };
    let parsed = ArkretChannelConfig::from_strict_agent_pairing_config(&config)?;
    let runtime = parsed.accounts.first().context("runtime account missing")?;
    if operation == "lose_key" {
        let key = runtime
            .key_ref
            .as_ref()
            .context("saved key reference missing")?;
        ensure!(
            delete_ed25519_key_ref_from_keyring(key)?,
            "owned key was not deleted"
        );
        ensure!(
            build_arkret_runtime_key_request_json(runtime, chrono::Utc::now()).is_err(),
            "runtime generated a replacement while the bound key was absent"
        );
        std::fs::write(output, b"{\"bound_key_missing_stopped_request\":true}")?;
        return Ok(());
    }
    let mut request = build_arkret_runtime_key_request_json(runtime, chrono::Utc::now())?;
    request
        .as_object_mut()
        .context("SDK runtime request object")?
        .insert(
            "pairing_code".into(),
            serde_json::to_value(&bootstrap.pairing_code)?,
        );
    let request: arkret::AgentRuntimeApprovalRequestBody = serde_json::from_value(request)?;
    std::fs::write(output, arkret::canonical::canonical_json_bytes(&request)?)?;
    Ok(())
}
