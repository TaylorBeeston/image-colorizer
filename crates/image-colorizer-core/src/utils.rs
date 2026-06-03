use palette::{Lab, Srgb};

pub fn hex_to_rgb(input: &str) -> Result<Srgb<f32>, String> {
    let cleaned = input.trim_start_matches('#');

    match cleaned.len() {
        3 => {
            // Three-character hex code
            let r = u8::from_str_radix(&cleaned[0..1].repeat(2), 16).map_err(|e| e.to_string())?
                as f32
                / 255.0;
            let g = u8::from_str_radix(&cleaned[1..2].repeat(2), 16).map_err(|e| e.to_string())?
                as f32
                / 255.0;
            let b = u8::from_str_radix(&cleaned[2..3].repeat(2), 16).map_err(|e| e.to_string())?
                as f32
                / 255.0;
            Ok(Srgb::new(r, g, b))
        }
        6 => {
            // Six-character hex code
            let r =
                u8::from_str_radix(&cleaned[0..2], 16).map_err(|e| e.to_string())? as f32 / 255.0;
            let g =
                u8::from_str_radix(&cleaned[2..4], 16).map_err(|e| e.to_string())? as f32 / 255.0;
            let b =
                u8::from_str_radix(&cleaned[4..6], 16).map_err(|e| e.to_string())? as f32 / 255.0;
            Ok(Srgb::new(r, g, b))
        }
        _ => Err(format!(
            "Invalid input: '{}'. Expected a 3 or 6-digit hex code.",
            input
        )),
    }
}

pub fn interpolate_color(color1: &Lab, color2: &Lab, t: f32) -> Lab {
    Lab::new(
        color1.l + (color2.l - color1.l) * t,
        color1.a + (color2.a - color1.a) * t,
        color1.b + (color2.b - color1.b) * t,
    )
}
