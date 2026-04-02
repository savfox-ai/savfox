#![allow(clippy::manual_let_else, clippy::ref_option)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod config;
pub mod git_registry;
pub mod installer;
pub mod manifest;
pub mod package;
pub mod registry;
pub mod zip_installer;

pub use config::RegistryConfig;
pub use git_registry::{GitRegistry, RegistrySearchResult, SkillEntry, SkillsIndex};
pub use installer::{InstallProgress, InstallResult, SkillInstaller};
pub use manifest::{
    InstallKind, ManifestError, SavfoxMetadata, SkillInstallMethod,
    SkillManifest as SkillFileManifest, SkillMetadata, SkillRequirements, load_skill_manifest,
    load_skill_manifest_async, parse_skill_manifest,
};
pub use package::{SkillManifest, SkillPackage, SkillSource};
pub use registry::SkillRegistry;
pub use zip_installer::{ConflictStrategy, ZipConflict, ZipInstallResult};
