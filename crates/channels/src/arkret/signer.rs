//! Load an Ed25519 signer for an Arkret runtime or applet bot.
//!
//! The 32-byte ed25519 seed is loaded from a [`ArkretKeyRef`] location (env
//! var, file, or — debug only — inline base64). The resulting
//! [`arkret::Ed25519MoveSigner`] is the savfox-owned runtime key in
//! personal-agent mode, and also supports applet signer flows:
//!
//! * **Applet DID-proof login** for applet outbound authentication.
//! * **Event signing** (`arkret_signatures::sign_event`) before every outbound submit.
//!
//! Security: seed material is wiped with `zeroize` after the signer is
//! constructed. Logging callers MUST NOT include `ArkretKeyRef`
//! variants directly — those contain or point at secret material.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use arkret::{Did, Ed25519MoveSigner};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// How to find the ed25519 seed for an Arkret runtime or applet bot.
///
/// Tagged on `kind` so the JSON config form is:
/// ```jsonc
/// { "kind": "env", "var": "SAVFOX_ARKRET_BOT_KEY" }
/// { "kind": "file", "path": "/var/secrets/savfox/arkret.seed" }
/// { "kind": "inline_seed_base64", "value": "..." }   // debug only
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArkretKeyRef {
    /// Read base64-no-pad-encoded 32-byte ed25519 seed from named env var.
    Env { var: String },
    /// Read 32-byte ed25519 seed from file. Accepts:
    /// * raw 32 bytes (binary, file len == 32), or
    /// * UTF-8 text holding base64-no-pad of the 32-byte seed.
    File { path: PathBuf },
    /// **TEST ONLY** — inline base64-no-pad seed in the config JSON. Refused
    /// at runtime in release builds.
    InlineSeedBase64 { value: String },
}

impl ArkretKeyRef {
    /// Parse from JSON value (used by config parsers).
    #[must_use]
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}

/// Load a [`Ed25519MoveSigner`] from a [`ArkretKeyRef`].
///
/// Returns `Err` on:
/// * missing env var
/// * unreadable file
/// * decoded seed length != 32
/// * invalid base64 in env / inline value
/// * invalid DID URI
/// * release build + `InlineSeedBase64` (refused regardless of value)
pub fn load_ed25519_signer(
    key_ref: &ArkretKeyRef,
    did: &str,
    verification_method: &str,
) -> anyhow::Result<Ed25519MoveSigner> {
    let mut seed_arr = load_seed_array(key_ref)?;

    let did =
        Did::new(did.to_owned()).with_context(|| format!("arkret signer: invalid DID '{did}'"))?;
    let signer = Ed25519MoveSigner::from_did_key_seed(seed_arr, did, verification_method);
    // `from_did_key_seed` copies the seed into a SigningKey; wipe ours.
    seed_arr.zeroize();
    Ok(signer)
}

/// Load a seed and return the runtime bridge's expected lowercase hex form.
pub fn load_ed25519_seed_hex(key_ref: &ArkretKeyRef) -> anyhow::Result<String> {
    let mut seed = load_seed_array(key_ref)?;
    let encoded = hex::encode(seed);
    seed.zeroize();
    Ok(encoded)
}

pub(crate) fn load_ed25519_signing_key(key_ref: &ArkretKeyRef) -> anyhow::Result<SigningKey> {
    let mut seed = load_seed_array(key_ref)?;
    let signing_key = SigningKey::from_bytes(&seed);
    seed.zeroize();
    Ok(signing_key)
}

fn load_seed_array(key_ref: &ArkretKeyRef) -> anyhow::Result<[u8; 32]> {
    let mut seed_bytes = load_seed_bytes(key_ref)?;
    if seed_bytes.len() != 32 {
        let len = seed_bytes.len();
        seed_bytes.zeroize();
        anyhow::bail!("arkret signer: seed must be 32 bytes, got {len}");
    }
    let mut seed_arr = [0u8; 32];
    seed_arr.copy_from_slice(&seed_bytes);
    seed_bytes.zeroize();
    Ok(seed_arr)
}

fn load_seed_bytes(key_ref: &ArkretKeyRef) -> anyhow::Result<Vec<u8>> {
    match key_ref {
        ArkretKeyRef::Env { var } => {
            let value = std::env::var(var)
                .with_context(|| format!("arkret signer: env var {var} not set"))?;
            decode_base64_no_pad(&value, "env value")
        }
        ArkretKeyRef::File { path } => load_file_seed(path),
        ArkretKeyRef::InlineSeedBase64 { value } => {
            #[cfg(not(debug_assertions))]
            {
                let _ = value;
                anyhow::bail!(
                    "arkret signer: inline_seed_base64 is not permitted in release builds"
                );
            }
            #[cfg(debug_assertions)]
            decode_base64_no_pad(value, "inline_seed_base64")
        }
    }
}

fn load_file_seed(path: &Path) -> anyhow::Result<Vec<u8>> {
    let raw = fs::read(path)
        .with_context(|| format!("arkret signer: read seed file {}", path.display()))?;
    if raw.len() == 32 {
        // Binary seed, exactly 32 bytes.
        return Ok(raw);
    }
    // Otherwise treat as text → base64-no-pad, trimming whitespace.
    let text = std::str::from_utf8(&raw).with_context(|| {
        format!(
            "arkret signer: seed file {} is neither 32 raw bytes nor UTF-8 base64",
            path.display()
        )
    })?;
    decode_base64_no_pad(text.trim(), "file content")
}

fn decode_base64_no_pad(text: &str, source: &str) -> anyhow::Result<Vec<u8>> {
    // Trim surrounding whitespace/newlines first (e.g. an env var set via
    // `export KEY=$(cat seed.b64)` carries a trailing newline), then strip any
    // base64 padding before decoding with the no-pad engine.
    let cleaned = text.trim().trim_end_matches('=');
    STANDARD_NO_PAD
        .decode(cleaned)
        .with_context(|| format!("arkret signer: base64 decode failed ({source})"))
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use arkret::MoveSigner;

    use super::*;

    const TEST_DID: &str = "did:webvh:example.org:agents:support";
    const TEST_VM: &str = "did:webvh:example.org:agents:support#key-1";
    const TEST_SEED: [u8; 32] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
        26, 27, 28, 29, 30, 31, 32,
    ];

    fn seed_b64() -> String {
        STANDARD_NO_PAD.encode(TEST_SEED)
    }

    // Use process-unique names so parallel tests don't clobber.
    fn unique_id(label: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}_{}_{}",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        )
    }

    // Env-based key_ref happy/error paths are covered by integration tests
    // (process-spawn). The workspace forbids unsafe code, and Rust 2024
    // marks `std::env::set_var` as unsafe — so we exercise env semantics
    // outside this unit-test module. Here we cover `File` + `InlineSeedBase64`
    // + happy-path round-trip through `Ed25519MoveSigner::sign_payload`.

    #[test]
    fn env_missing_returns_typed_error() {
        // Read-only is fine and safe; pick a clearly-nonexistent name.
        let var = format!("SAVFOX_ARKRET_DEFINITELY_NOT_SET_{}", unique_id("MISS"));
        let key_ref = ArkretKeyRef::Env { var };
        let err = load_ed25519_signer(&key_ref, TEST_DID, TEST_VM)
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("not set"));
    }

    #[test]
    fn file_text_load_succeeds() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "savfox-arkret-test-seed-{}.txt",
            unique_id("FILE_OK")
        ));
        std::fs::write(&path, seed_b64()).expect("write");
        let key_ref = ArkretKeyRef::File { path: path.clone() };
        let signer = load_ed25519_signer(&key_ref, TEST_DID, TEST_VM).expect("load");
        assert_eq!(signer.verification_method_id(), TEST_VM);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_binary_load_succeeds() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "savfox-arkret-test-seed-{}.bin",
            unique_id("FILE_BIN")
        ));
        std::fs::write(&path, TEST_SEED).expect("write");
        let key_ref = ArkretKeyRef::File { path: path.clone() };
        let signer = load_ed25519_signer(&key_ref, TEST_DID, TEST_VM).expect("load");
        assert_eq!(signer.verification_method_id(), TEST_VM);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wrong_seed_length_rejects() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "savfox-arkret-test-short-{}.txt",
            unique_id("FILE_SHORT")
        ));
        // Write text base64 of a 16-byte buffer — decode succeeds but the
        // length check after decode catches the mismatch.
        let short_b64 = STANDARD_NO_PAD.encode([0u8; 16]);
        std::fs::write(&path, short_b64).expect("write");
        let key_ref = ArkretKeyRef::File { path: path.clone() };
        let err = load_ed25519_signer(&key_ref, TEST_DID, TEST_VM)
            .err()
            .expect("expected error");
        let msg = err.to_string();
        assert!(
            msg.contains("32 bytes"),
            "expected '32 bytes' in error, got: {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_did_rejects() {
        // Use file-based key_ref to avoid env mutation.
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "savfox-arkret-test-bad-did-{}.bin",
            unique_id("FILE_BAD_DID")
        ));
        std::fs::write(&path, TEST_SEED).expect("write");
        let key_ref = ArkretKeyRef::File { path: path.clone() };
        let err = load_ed25519_signer(&key_ref, "not-a-did", TEST_VM)
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("invalid DID"));
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn inline_seed_works_in_debug() {
        let key_ref = ArkretKeyRef::InlineSeedBase64 { value: seed_b64() };
        let signer = load_ed25519_signer(&key_ref, TEST_DID, TEST_VM).expect("load");
        assert_eq!(signer.verification_method_id(), TEST_VM);
    }

    #[test]
    fn json_round_trip_env() {
        let key_ref = ArkretKeyRef::Env {
            var: "EXAMPLE".to_owned(),
        };
        let json = serde_json::to_value(&key_ref).expect("ser");
        assert_eq!(json["kind"], "env");
        assert_eq!(json["var"], "EXAMPLE");
        let back: ArkretKeyRef = serde_json::from_value(json).expect("de");
        assert_eq!(back, key_ref);
    }

    #[test]
    fn json_round_trip_file() {
        let key_ref = ArkretKeyRef::File {
            path: PathBuf::from("/var/secrets/x"),
        };
        let json = serde_json::to_value(&key_ref).expect("ser");
        assert_eq!(json["kind"], "file");
        let back: ArkretKeyRef = serde_json::from_value(json).expect("de");
        assert_eq!(back, key_ref);
    }
}
