//! `savfox skills` -- list available skills.
//!
//! Skill enable/disable is now managed per-agent in each agent's config file
//! (`~/.savfox/agents/*.json`).  This CLI lists the skills discovered from
//! manifest directories.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Debug, Parser)]
pub struct SkillsCommand {
    #[command(subcommand)]
    pub command: SkillsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SkillsSubcommand {
    /// List available skills discovered from manifest directories.
    List {
        /// Print JSON output.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

pub fn run(cmd: SkillsCommand) -> Result<()> {
    match cmd.command {
        SkillsSubcommand::List { json: json_output } => {
            let savfox_home = savfox_core::config::find_savfox_home()?;
            let skills = discover_skills(&savfox_home);
            if json_output {
                println!("{}", serde_json::to_string_pretty(&skills)?);
            } else {
                println!("{:<28} {:<10} CATEGORY", "NAME", "INSTALLED",);
                for skill in &skills {
                    let name = skill.get("name").and_then(|v| v.as_str()).unwrap_or("-");
                    let installed = skill
                        .get("installed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let category = skill
                        .get("category")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-");
                    println!("{:<28} {:<10} {}", name, installed, category);
                }
                println!("\nSkill enable/disable is managed per-agent in agent config files.");
            }
            Ok(())
        }
    }
}

fn discover_skills(savfox_home: &Path) -> Vec<serde_json::Value> {
    let skill_file = "SKILL.md";
    let mut results = Vec::new();

    // Built-in skills
    for path in scan_manifests(
        &savfox_home.join("skills").join(".system"),
        skill_file,
        false,
    ) {
        if let Some(name) = extract_skill_name(&path) {
            results.push(json!({
                "name": name,
                "installed": true,
                "category": "built-in",
            }));
        }
    }

    // User-installed skills (skip .system)
    for path in scan_manifests(&savfox_home.join("skills"), skill_file, true) {
        if let Some(name) = extract_skill_name(&path) {
            results.push(json!({
                "name": name,
                "installed": true,
                "category": "installed",
            }));
        }
    }

    results
}

fn scan_manifests(root: &Path, skill_file: &str, skip_system: bool) -> Vec<PathBuf> {
    let mut manifests = Vec::new();
    if !root.is_dir() {
        return manifests;
    }
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 4 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if skip_system
                    && p.file_name()
                        .and_then(|v| v.to_str())
                        .is_some_and(|n| n == ".system")
                {
                    continue;
                }
                stack.push((p, depth + 1));
            } else if p
                .file_name()
                .and_then(|v| v.to_str())
                .is_some_and(|n| n == skill_file)
            {
                manifests.push(p);
            }
        }
    }
    manifests
}

/// Extract the skill name from the SKILL.md frontmatter (first `name:` field).
fn extract_skill_name(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim_start();
    let rest = trimmed.strip_prefix("---")?;
    let closing = rest.find("\n---")?;
    let yaml = &rest[..closing];
    for line in yaml.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("name:") {
            let name = value.trim().trim_matches('"').trim_matches('\'').trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}
