//! Reusable GPU image colorization library.
//!
//! The main entry point is [`GpuColorizer`]. Build a [`ColorizerConfig`] with a
//! Lab colorscheme, create one colorizer, then reuse it for any number of images.
//! Reusing the colorizer preserves GPU device/pipeline state and scratch buffers
//! across calls.

mod colorize;
pub mod colors;
mod types;
pub mod utils;

pub use colorize::{ColorizeStage, GpuColorizer, RenderedImage};
pub use types::ColorizerConfig;

#[cfg(test)]
mod tests;
