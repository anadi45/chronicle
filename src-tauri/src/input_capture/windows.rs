//! Windows keyboard and mouse provider entry point.
//!
//! Low-level Windows hook implementations belong in sibling modules under
//! this folder. Shared privacy settings and event normalization live in the
//! parent module.

pub const PROVIDER_NAME: &str = "windows_global_input";
