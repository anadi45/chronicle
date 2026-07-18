//! Activity-capture module shim.
pub mod shared;
#[cfg(windows)]
pub mod windows;
pub use shared::*;
