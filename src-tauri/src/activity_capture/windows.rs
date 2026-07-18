//! Windows foreground activity provider entry point.
//!
//! This module is reserved for Windows-only API bindings. Shared event
//! normalization and lifecycle contracts live in `super`; macOS and Linux
//! providers should be implemented as sibling modules without changing them.

pub const PROVIDER_NAME: &str = "windows_foreground_activity";
