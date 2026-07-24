use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorBlindMode {
    #[default]
    None,
    Protanopia,
    Deuteranopia,
    Tritanopia,
    Achromatopsia,
}

pub fn apply_color_blind_remap(colors: &mut crate::theme::ThemeColor, mode: ColorBlindMode) {
    match mode {
        ColorBlindMode::None => {}
        ColorBlindMode::Protanopia | ColorBlindMode::Deuteranopia => {
            remap_color(&mut colors.red, 0.03, 0.8);
            remap_color(&mut colors.red_light, 0.03, 0.7);
            remap_color(&mut colors.green, 0.55, 0.5);
            remap_color(&mut colors.green_light, 0.55, 0.4);
            remap_color(&mut colors.danger, 0.03, 0.8);
            remap_color(&mut colors.success, 0.55, 0.5);
        }
        ColorBlindMode::Tritanopia => {
            remap_color(&mut colors.blue, 0.55, 0.7);
            remap_color(&mut colors.blue_light, 0.55, 0.6);
            remap_color(&mut colors.yellow, 0.10, 1.0);
            remap_color(&mut colors.yellow_light, 0.10, 0.9);
        }
        ColorBlindMode::Achromatopsia => {
            desaturate_color(&mut colors.red);
            desaturate_color(&mut colors.red_light);
            desaturate_color(&mut colors.green);
            desaturate_color(&mut colors.green_light);
            desaturate_color(&mut colors.blue);
            desaturate_color(&mut colors.blue_light);
            desaturate_color(&mut colors.yellow);
            desaturate_color(&mut colors.yellow_light);
            desaturate_color(&mut colors.magenta);
            desaturate_color(&mut colors.magenta_light);
            desaturate_color(&mut colors.cyan);
            desaturate_color(&mut colors.cyan_light);
            desaturate_color(&mut colors.danger);
            desaturate_color(&mut colors.success);
            desaturate_color(&mut colors.warning);
            desaturate_color(&mut colors.info);
            desaturate_color(&mut colors.primary);
            desaturate_color(&mut colors.accent);
        }
    }
}

fn remap_color(color: &mut gpui::Hsla, target_hue: f32, target_saturation: f32) {
    color.h = target_hue;
    color.s = target_saturation;
}

fn desaturate_color(color: &mut gpui::Hsla) {
    color.s = 0.0;
}
