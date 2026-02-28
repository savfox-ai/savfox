use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::debug;

/// A parsed SKILL.md file consisting of YAML frontmatter and a Markdown body.
///
/// The YAML frontmatter is delimited by `---` lines at the top of the file.
/// Everything after the closing `---` is the Markdown instruction body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    /// Unique skill identifier (e.g. "github", "weather").
    pub name: String,

    /// Human-readable description of what the skill does.
    #[serde(default)]
    pub description: String,

    /// Optional version string.
    #[serde(default)]
    pub version: Option<String>,

    /// Nested metadata bag; the `savfox` key carries Savfox-specific fields.
    #[serde(default)]
    pub metadata: SkillMetadata,

    /// The raw Markdown body that follows the YAML frontmatter.
    /// This is populated by `load_skill_manifest`, not from YAML itself.
    #[serde(skip)]
    pub instructions: String,
}

/// Top-level `metadata` object inside the YAML frontmatter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillMetadata {
    /// Savfox-specific metadata.
    #[serde(default)]
    pub savfox: SavfoxMetadata,
}

/// Savfox-specific metadata for a skill.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavfoxMetadata {
    /// Display emoji for the skill (e.g. "\u{1F419}").
    #[serde(default)]
    pub emoji: Option<String>,

    /// Runtime requirements the skill needs on the host.
    #[serde(default)]
    pub requires: SkillRequirements,

    /// Ordered list of install methods that can satisfy the requirements.
    #[serde(default)]
    pub install: Vec<SkillInstallMethod>,
}

/// Declares what the skill needs to be present on the host system.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillRequirements {
    /// Binary executables that must be on `$PATH`.
    #[serde(default)]
    pub bins: Vec<String>,

    /// Environment variables that must be set.
    #[serde(default)]
    pub env: Vec<String>,
}

/// A single method for installing a skill's requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInstallMethod {
    /// Unique id for this install method (e.g. "brew", "apt-gh").
    pub id: String,

    /// The kind of package manager or installer.
    pub kind: InstallKind,

    /// The formula / package / crate / npm-package name.
    #[serde(default)]
    pub formula: Option<String>,

    /// Alias — some kinds use `package` instead of `formula`.
    #[serde(default)]
    pub package: Option<String>,

    /// Crate name (for `cargo` kind).
    #[serde(default)]
    pub crate_name: Option<String>,

    /// npm package name (for `npm` kind).
    #[serde(default)]
    pub npm_package: Option<String>,

    /// After installation, these binaries should be available.
    #[serde(default)]
    pub bins: Vec<String>,

    /// Human-readable label shown in the UI.
    #[serde(default)]
    pub label: Option<String>,
}

/// Supported installer back-ends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InstallKind {
    Brew,
    Apt,
    Cargo,
    Npm,
    Winget,
    Scoop,
    Choco,
    Manual,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Errors returned when parsing a SKILL.md file.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("IO error reading manifest: {0}")]
    Io(#[from] std::io::Error),

    #[error("missing YAML frontmatter delimiters (expected `---`)")]
    MissingFrontmatter,

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Split raw file content into (yaml_str, markdown_body).
fn split_frontmatter(content: &str) -> Result<(&str, &str), ManifestError> {
    let trimmed = content.trim_start();

    // Must start with `---`
    let rest = trimmed
        .strip_prefix("---")
        .ok_or(ManifestError::MissingFrontmatter)?;

    // Find the closing `---`
    let closing = rest
        .find("\n---")
        .ok_or(ManifestError::MissingFrontmatter)?;

    let yaml = &rest[..closing];
    let after_closing = &rest[closing + 4..]; // skip "\n---"

    // Skip the rest of the closing delimiter line (could be "\n" or "\r\n")
    let body = after_closing
        .strip_prefix('\n')
        .or_else(|| after_closing.strip_prefix("\r\n"))
        .unwrap_or(after_closing);

    Ok((yaml, body))
}

/// Load and parse a `SKILL.md` file from `path`.
///
/// The file must contain YAML frontmatter between `---` delimiters followed
/// by Markdown content.  Returns a [`SkillManifest`] with the `instructions`
/// field populated from the Markdown body.
pub fn load_skill_manifest(path: &Path) -> Result<SkillManifest, ManifestError> {
    let content = std::fs::read_to_string(path)?;
    parse_skill_manifest(&content)
}

/// Async variant of [`load_skill_manifest`].
pub async fn load_skill_manifest_async(path: &Path) -> Result<SkillManifest, ManifestError> {
    let content = tokio::fs::read_to_string(path).await?;
    parse_skill_manifest(&content)
}

/// Parse a SKILL.md string (frontmatter + body) into a [`SkillManifest`].
pub fn parse_skill_manifest(content: &str) -> Result<SkillManifest, ManifestError> {
    let (yaml_str, body) = split_frontmatter(content)?;

    debug!(
        yaml_len = yaml_str.len(),
        body_len = body.len(),
        "parsing skill manifest"
    );

    let mut manifest: SkillManifest = serde_yaml::from_str(yaml_str)?;
    manifest.instructions = body.to_string();
    Ok(manifest)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
name: github
description: "Interact with GitHub using the gh CLI"
version: "1.0.0"
metadata:
  savfox:
    emoji: "\U0001F419"
    requires:
      bins: ["gh"]
    install:
      - id: brew
        kind: brew
        formula: gh
        bins: [gh]
        label: "Install GitHub CLI (brew)"
      - id: apt
        kind: apt
        package: gh
        bins: [gh]
        label: "Install GitHub CLI (apt)"
---
# GitHub Skill

Use the `gh` CLI to interact with GitHub.
"#;

    #[test]
    fn parse_sample_manifest() {
        let manifest = parse_skill_manifest(SAMPLE).expect("should parse");
        assert_eq!(manifest.name, "github");
        assert_eq!(
            manifest.description,
            "Interact with GitHub using the gh CLI"
        );
        assert_eq!(manifest.version.as_deref(), Some("1.0.0"));
        assert_eq!(manifest.metadata.savfox.emoji.as_deref(), Some("\u{1F419}"));
        assert_eq!(manifest.metadata.savfox.requires.bins, vec!["gh"]);
        assert_eq!(manifest.metadata.savfox.install.len(), 2);
        assert_eq!(manifest.metadata.savfox.install[0].kind, InstallKind::Brew);
        assert_eq!(manifest.metadata.savfox.install[1].kind, InstallKind::Apt);
        assert!(manifest.instructions.contains("# GitHub Skill"));
    }

    #[test]
    fn missing_frontmatter_returns_error() {
        let bad = "# No frontmatter here\nJust markdown.";
        let err = parse_skill_manifest(bad).unwrap_err();
        assert!(matches!(err, ManifestError::MissingFrontmatter));
    }

    #[test]
    fn minimal_frontmatter() {
        let minimal = "---\nname: test\n---\nBody text.";
        let manifest = parse_skill_manifest(minimal).expect("should parse");
        assert_eq!(manifest.name, "test");
        assert_eq!(manifest.instructions, "Body text.");
    }

    #[test]
    fn empty_body() {
        let no_body = "---\nname: empty\ndescription: nothing\n---\n";
        let manifest = parse_skill_manifest(no_body).expect("should parse");
        assert_eq!(manifest.name, "empty");
        assert!(manifest.instructions.is_empty());
    }

    #[test]
    fn split_preserves_body_content() {
        let content = "---\nname: x\n---\nLine 1\nLine 2\n";
        let (yaml, body) = split_frontmatter(content).expect("should split");
        assert_eq!(yaml.trim(), "name: x");
        assert_eq!(body, "Line 1\nLine 2\n");
    }
}
