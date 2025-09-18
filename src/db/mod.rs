//! Database module: entity models and SQL repositories.
//!
//! This module is split into two submodules:
//! - `model`: typed domain entities and view models returned by repositories.
//! - `repo`: SQL-only functions that map rows into entities.
//!
//! External modules should import from `tg_watchbot::db` — we re-export the
//! repository API and commonly used models for convenience.

pub mod model;
pub mod repo;

// Re-export the repository API at `crate::db::*` for backward compatibility.
pub use repo::*;

// Surface view models used by callers (e.g., outbox worker).
pub use model::{BatchForOutbox, ResourceForOutbox};
