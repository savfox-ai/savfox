#![allow(unreachable_pub)]

#[cfg(not(target_arch = "wasm32"))]
pub mod absolute_path;
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
#[cfg(not(target_arch = "wasm32"))]
pub mod cargo_bin;
#[cfg(not(target_arch = "wasm32"))]
pub mod git;
#[cfg(not(target_arch = "wasm32"))]
pub mod home_dir;
#[cfg(not(target_arch = "wasm32"))]
pub mod image;
#[cfg(not(target_arch = "wasm32"))]
pub mod json_to_toml;
#[cfg(not(target_arch = "wasm32"))]
pub mod pty;
#[cfg(not(target_arch = "wasm32"))]
pub mod readiness;
pub mod string;

#[cfg(not(target_arch = "wasm32"))]
pub use cargo_bin::{resolve_bazel_runfile, runfiles_available};
