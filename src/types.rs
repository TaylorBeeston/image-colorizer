use palette::Lab;

#[derive(Debug)]
pub struct AppConfig {
    pub input_output_pairs: Vec<(String, String)>,
    pub blend_factor: f32,
    pub colors: Vec<Lab>,
    pub dither_amount: f32,
    pub spatial_averaging_radius: u32,
}
