use std::path::PathBuf;

use semver::Version;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillPackage {
    pub manifest: SkillManifest,
    pub source: SkillSource,
    pub installed: bool,
    pub installed_version: Option<Version>,
    pub install_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_version")]
    pub version: Version,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub short_description: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<SkillDependency>,
    #[serde(default)]
    pub compatible_hosts: Vec<String>,
    #[serde(default)]
    pub min_host_version: Option<Version>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub brand_color: Option<String>,
    #[serde(default)]
    pub default_prompt: Option<String>,
    #[serde(default)]
    pub readme: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
}

fn default_version() -> Version {
    Version::new(0, 0, 0)
}

impl Default for SkillManifest {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: default_version(),
            description: String::new(),
            short_description: None,
            author: None,
            license: None,
            repository: None,
            homepage: None,
            keywords: Vec::new(),
            categories: Vec::new(),
            dependencies: Vec::new(),
            compatible_hosts: Vec::new(),
            min_host_version: None,
            icon: None,
            brand_color: None,
            default_prompt: None,
            readme: None,
            changelog: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDependency {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSource {
    #[serde(rename = "type")]
    pub source_type: SkillSourceType,
    pub url: Option<String>,
    pub path: Option<PathBuf>,
    pub registry: Option<String>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillSourceType {
    Registry,
    Git,
    Local,
    Http,
    Embedded,
}

impl SkillManifest {
    pub fn parse(content: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(content)
    }

    pub fn to_skill_md(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("name: {}\n", self.name));
        md.push_str(&format!("description: {}\n", self.description));
        if let Some(ref short) = self.short_description {
            md.push_str(&format!("short-description: {}\n", short));
        }
        md.push_str("---\n\n");
        if let Some(ref readme) = self.readme {
            md.push_str(readme);
        }
        md
    }
}

impl SkillPackage {
    pub fn is_update_available(&self) -> bool {
        if !self.installed {
            return false;
        }
        if let Some(ref installed) = self.installed_version {
            self.manifest.version > *installed
        } else {
            false
        }
    }

    pub fn status_label(&self) -> &'static str {
        if !self.installed {
            "Not installed"
        } else if self.is_update_available() {
            "Update available"
        } else {
            "Installed"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manifest() {
        let yaml = r#"
name: github
version: "1.0.0"
description: GitHub integration skill
short-description: Work with GitHub repos
author: Savfox Team
license: MIT
keywords:
  - github
  - git
categories:
  - developer-tools
"#;
        let manifest: SkillManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.name, "github");
        assert_eq!(manifest.keywords.len(), 2);
    }
}
