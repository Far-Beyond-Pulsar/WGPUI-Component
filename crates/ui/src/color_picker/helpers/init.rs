use super::*;

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Cancel, Some(CONTEXT))])
}

struct ColorPickerInit;
#[cfg(not(target_family = "wasm"))]
impl crate::registry::UiComponentInit for ColorPickerInit {
    fn init(&self, cx: &mut App) {
        init(cx);
    }
}
#[cfg(not(target_family = "wasm"))]
crate::register_ui_component!(ColorPickerInit);
