use palette::Lab;

/// Configuration for GPU colorization.
///
/// Colors are expected to be in CIE Lab space. The CLI resolves hex colorscheme
/// files into this form before constructing a [`crate::GpuColorizer`].
#[derive(Debug, Clone)]
pub struct ColorizerConfig {
    /// Blend factor from `0.0` to `1.0`.
    ///
    /// `0.0` preserves the original image; `1.0` uses only the colorized result.
    pub blend_factor: f32,

    /// Palette colors in CIE Lab space.
    pub colors: Vec<Lab>,

    /// Dithering amount from `0.0` to `1.0`.
    pub dither_amount: f32,

    /// Radius, in pixels, used by the separable spatial average.
    pub spatial_averaging_radius: u32,
}
