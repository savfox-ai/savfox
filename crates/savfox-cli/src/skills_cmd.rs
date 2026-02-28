//! `savfox skills` -- manage installed skill toggles.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
pub struct SkillsCommand {
    #[command(subcommand)]
    pub command: SkillsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SkillsSubcommand {
    /// List installed skills and enabled state.
    List {
        /// Print JSON output.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Enable an installed skill.
    Enable {
        /// Skill name.
        name: String,
    },
    /// Disable an installed skill.
    Disable {
        /// Skill name.
        name: String,
        /// Optional disable reason.
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstalledSkill {
    name: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    disabled_reason: Option<String>,
    installed_at_ms: u64,
    updated_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillsState {
    #[serde(default)]
    installed: Vec<InstalledSkill>,
}

fn default_true() -> bool {
    true
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn state_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("gateway").join("skills-state.json")
}

fn load_state(path: &Path) -> Result<SkillsState> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(SkillsState::default()),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse skills state {}", path.display()))
}

fn save_state(path: &Path, state: &SkillsState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(state).context("failed to serialize skills state")?;
    std::fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn print_table(skills: &[InstalledSkill]) {
    println!("{:<24} {:<8} {:<12} REASON", "NAME", "ENABLED", "VERSION");
    for skill in skills {
        let version = skill.version.as_deref().unwrap_or("-");
        let reason = skill.disabled_reason.as_deref().unwrap_or("-");
        println!(
            "{:<24} {:<8} {:<12} {}",
            skill.name, skill.enabled, version, reason
        );
    }
}

pub fn run(cmd: SkillsCommand) -> Result<()> {
    let savfox_home = savfox_core::config::find_savfox_home()?;
    let path = state_path(&savfox_home);
    let mut state = load_state(&path)?;

    match cmd.command {
        SkillsSubcommand::List { json } => {
            state.installed.sort_by(|a, b| a.name.cmp(&b.name));
            if json {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                print_table(&state.installed);
            }
        }
        SkillsSubcommand::Enable { name } => {
            let skill = state
                .installed
                .iter_mut()
                .find(|s| s.name == name)
                .ok_or_else(|| anyhow::anyhow!("skill not installed: {name}"))?;
            skill.enabled = true;
            skill.disabled_reason = None;
            skill.updated_at_ms = now_ms();
            save_state(&path, &state)?;
            println!("Enabled skill '{}'", name);
        }
        SkillsSubcommand::Disable { name, reason } => {
            let skill = state
                .installed
                .iter_mut()
                .find(|s| s.name == name)
                .ok_or_else(|| anyhow::anyhow!("skill not installed: {name}"))?;
            skill.enabled = false;
            skill.disabled_reason = Some(
                reason
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| "disabled from CLI".to_string()),
            );
            skill.updated_at_ms = now_ms();
            save_state(&path, &state)?;
            println!("Disabled skill '{}'", name);
        }
    }

    Ok(())
}
