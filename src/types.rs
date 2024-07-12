use palette::Lab;

#[derive(Debug)]
pub struct AppConfig {
    pub input_path: String,
    pub blend_factor: f32,
    pub colorscheme: String,
    pub colors: Vec<Lab>,
    pub interpolation_threshold: f32,
    pub dither_amount: f32,
    pub spatial_averaging_radius: u32,
}
