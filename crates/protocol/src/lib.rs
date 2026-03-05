//! Protocol types and definitions for the Savfox AI platform.
//!
//! This crate contains the core protocol definitions, data types, and models
//! used across the Savfox ecosystem for communication between different components.
//!
//! # Modules
//!
//! - [`protocol`] - Core protocol definitions and types
//! - [`openai_models`] - OpenAI API compatible models
//! - [`models`] - Savfox-specific model definitions
//! - [`session_id`] - Session identification types
//! - [`approvals`] - Approval workflow types
//! - [`config_types`] - Configuration type definitions
//! - [`message_history`] - Message history types
//! - [`items`] - Protocol item definitions
//! - [`mcp`] - Model Context Protocol types
//! - [`custom_prompts`] - Custom prompt types
//! - [`dynamic_tools`] - Dynamic tool definitions
//! - [`request_user_input`] - User input request types
//! - [`user_input`] - User input types
//! - [`account`] - Account-related types
//! - [`num_format`] - Number formatting utilities
//! - [`parse_command`] - Command parsing utilities
//! - [`plan_tool`] - Planning tool types

#![allow(missing_debug_implementations)]

pub mod account;
mod session_id;
pub use session_id::SessionId;
pub mod approvals;
pub mod config_types;
pub mod custom_prompts;
pub mod dynamic_tools;
pub mod items;
pub mod mcp;
pub mod message_history;
pub mod models;
pub mod num_format;
pub mod openai_models;
pub mod parse_command;
pub mod plan_tool;
pub mod protocol;
pub mod request_user_input;
pub mod user_input;
