pub mod index;
pub mod installer;
pub mod manifest;
pub mod package;
pub mod registry;

pub use index::RemoteIndex;
pub use installer::{InstallProgress, InstallResult, SkillInstaller};
pub use manifest::{
    InstallKind, ManifestError, SavfoxMetadata, SkillInstallMethod,
    SkillManifest as SkillFileManifest, SkillMetadata, SkillRequirements, load_skill_manifest,
    load_skill_manifest_async, parse_skill_manifest,
};
pub use package::{SkillManifest, SkillPackage, SkillSource};
pub use registry::SkillRegistry;
