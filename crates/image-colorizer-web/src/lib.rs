//! Static client-only WebGPU app assets.
//!
//! This crate is a workspace boundary for the hostable browser app. The files in
//! `static/` are served directly by static hosts such as Netlify and intentionally
//! do not replace the CLI's native-GPU `image-colorizer serve` UI.

/// Relative path to the static site assets from the repository root.
pub const STATIC_DIR: &str = "crates/image-colorizer-web/static";
