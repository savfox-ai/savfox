//! Static guard for the Arkret channel's shared SDK model usage.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_DEFINITION_PREFIXES: &[&str] = &["struct ", "enum ", "type "];
const SDK_MODEL_NAME: &str = "AgentPairingBootstrap";

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn savfox_does_not_reintroduce_agent_pairing_bootstrap_shadow_type() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("channels crate should live under crates/channels");
    let crates = workspace.join("crates");
    assert!(
        crates.is_dir(),
        "missing crates directory: {}",
        crates.display()
    );

    let mut files = Vec::new();
    collect_rs_files(&crates, &mut files);
    assert!(
        !files.is_empty(),
        "expected Rust source files under {}",
        crates.display()
    );

    let mut violations = Vec::new();
    for file in files {
        let contents = match fs::read_to_string(&file) {
            Ok(contents) => contents,
            Err(_) => continue,
        };
        for prefix in FORBIDDEN_DEFINITION_PREFIXES {
            let definition = format!("{prefix}{SDK_MODEL_NAME}");
            if contents.contains(&definition) {
                violations.push(format!("{} contains `{definition}`", file.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Savfox must use the SDK AgentPairingBootstrap model instead of a local \
         shadow DTO. Violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn savfox_listener_does_not_reintroduce_session_outer_loop() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("channels crate should live under crates/channels");
    let listener = workspace.join("crates/gateway-server/src/channels/arkret.rs");
    let contents = fs::read_to_string(&listener)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", listener.display()));
    let forbidden = [
        "SESSION_REFRESH_SKEW_SECS",
        "AccountEngineOutcome::RefreshSession",
        "fn session_refresh_delay(",
        "fn session_grant_needs_refresh(",
        "struct FixedAccountTransport",
        "fn sleep_with_backoff(",
    ];
    let violations = forbidden
        .iter()
        .filter(|needle| contents.contains(**needle))
        .copied()
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "Savfox account subscription must delegate session refresh and transport rebuild to \
         garth::SessionTransportProvider. Forbidden listener surfaces: {violations:?}"
    );
    assert!(
        contents.contains("construct_account_provider")
            && contents.contains("ArkretAgentSessionProvider"),
        "Savfox account listener must remain wired to the shared session provider"
    );
}

#[test]
fn savfox_account_outbound_uses_garth_durable_queue() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("channels crate should live under crates/channels");
    let source_path = workspace.join("crates/gateway-server/src/channels/arkret.rs");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source_path.display()));
    let send_start = source
        .find("pub(crate) async fn send_to_arkret_account")
        .expect("missing account outbound entrypoint");
    let send_source = &source[send_start..];
    assert!(
        send_source.contains("OutboundEngine::new(outbound_store)")
            && send_source.contains(".enqueue(")
            && send_source.contains(".submit_next("),
        "Savfox account outbound must enqueue and submit through Garth's durable engine"
    );
    assert!(
        source.contains("drain_pending_account_outbound")
            && source.contains("durable outbound worker completed queued event"),
        "Savfox listener must resume pending outbound items after process restart"
    );
    assert!(
        !send_source.contains("let response = client.submit_event(&event).await?"),
        "Savfox account outbound must not bypass the durable queue with direct submit"
    );
}

#[test]
fn savfox_account_runtime_does_not_restore_legacy_store_or_channel_drain() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("channels crate should live under crates/channels");
    let account_store = workspace.join("crates/channels/src/arkret/account_store.rs");
    let listener = workspace.join("crates/gateway-server/src/channels/arkret.rs");
    let sources = [account_store, listener]
        .map(|path| {
            fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        })
        .join("\n");
    for forbidden in [
        "FileArkretAccountStore",
        "drain_pending_account_events",
        "unbounded_channel",
    ] {
        assert!(
            !sources.contains(forbidden),
            "Savfox must use Garth FileStore and durable inbox directly; found legacy surface `{forbidden}`"
        );
    }
    assert!(
        sources.contains("process_durable_account_inbox") && sources.contains("garth::FileStore"),
        "Savfox listener must keep the durable Garth inbox worker wired"
    );
}
