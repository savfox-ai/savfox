//! Static guard for the Cokret channel's shared SDK model usage.

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
