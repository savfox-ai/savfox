mod env_var_dependencies;
pub mod injection;
pub mod loader;
pub mod manager;
pub mod model;
pub mod render;
pub mod system;

pub(crate) use env_var_dependencies::{
    collect_env_var_dependencies, resolve_skill_dependencies_for_turn,
};
pub(crate) use injection::{
    SkillInjections, build_skill_injections, collect_explicit_skill_mentions,
};
pub use loader::load_skills;
pub use manager::SkillsManager;
pub use model::{SkillError, SkillLoadOutcome, SkillMetadata};
pub use render::render_skills_section;
