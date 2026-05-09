#![allow(unreachable_pub)]
#![allow(
    clippy::manual_let_else,
    clippy::module_inception,
    clippy::needless_continue,
    clippy::needless_pass_by_ref_mut,
    clippy::ref_option,
    clippy::return_self_not_must_use
)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(not(target_arch = "wasm32"))]
pub mod absolute_path;
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
#[cfg(not(target_arch = "wasm32"))]
pub mod cargo_bin;
#[cfg(not(target_arch = "wasm32"))]
pub mod fs;
#[cfg(not(target_arch = "wasm32"))]
pub mod git;
#[cfg(not(target_arch = "wasm32"))]
pub mod home_dir;
#[cfg(not(target_arch = "wasm32"))]
pub mod image;
#[cfg(not(target_arch = "wasm32"))]
pub mod json_to_toml;
pub mod provider_id;
#[cfg(not(target_arch = "wasm32"))]
pub mod pty;
#[cfg(not(target_arch = "wasm32"))]
pub mod readiness;
pub mod string;

#[cfg(not(target_arch = "wasm32"))]
pub use cargo_bin::{resolve_bazel_runfile, runfiles_available};
