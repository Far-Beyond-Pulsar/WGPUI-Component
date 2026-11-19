use super::*;
use gpui::{
    AppContext, EntityInputHandler, KeyDownEvent, Keystroke, Modifiers, TestAppContext,
    WindowHandle,
};

fn input_with_value(cx: &mut TestAppContext) -> WindowHandle<InputState> {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            cx.new(|cx| InputState::new(window, cx))
        })
        .unwrap()
    });

    window
        .update(cx, |input, window, cx| {
            input.set_value("abcd", window, cx);
        })
        .unwrap();

    window
}

fn select(
    cx: &mut TestAppContext,
    window: &WindowHandle<InputState>,
    range: std::ops::Range<usize>,
) {
    window
        .update(cx, |input, _, _| {
            input.selected_range = range.into();
        })
        .unwrap();
}

#[gpui::test]
fn typing_with_selection_replaces_selection(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);
    select(cx, &window, 1..3);

    // This is the path used by both the platform's input-handler routing
    // and the IME commit flow.
    window
        .update(cx, |input, window, cx| {
            input.replace_text_in_range(None, "e", window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "aed");
        })
        .unwrap();
}

#[gpui::test]
fn insert_replaces_selection(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);
    select(cx, &window, 1..3);

    window
        .update(cx, |input, window, cx| {
            input.insert("e", window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "aed");
        })
        .unwrap();
}

#[gpui::test]
fn replace_replaces_selection(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);
    select(cx, &window, 1..3);

    window
        .update(cx, |input, window, cx| {
            input.replace("e", window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "aed");
        })
        .unwrap();
}

#[gpui::test]
fn typing_without_selection_inserts_at_cursor(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);

    window
        .update(cx, |input, window, cx| {
            input.replace_text_in_range(None, "e", window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "abcde");
        })
        .unwrap();
}

#[gpui::test]
fn non_character_key_does_not_delete_selection(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);
    select(cx, &window, 1..3);

    window
        .update(cx, |input, window, cx| {
            let event = KeyDownEvent {
                keystroke: Keystroke::parse("right").unwrap(),
                is_held: false,
                prefer_character_input: false,
            };
            input.on_key_down(&event, window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "abcd");
        })
        .unwrap();
}

#[gpui::test]
fn typing_with_reversed_selection_replaces_selection(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);
    select(cx, &window, 1..3);

    // A selection dragged from right to left is stored as (start..end)
    // with `selection_reversed` set; typing must still replace the whole
    // selected range rather than inserting at the range start.
    window
        .update(cx, |input, _, _| {
            input.selection_reversed = true;
        })
        .unwrap();

    window
        .update(cx, |input, window, cx| {
            input.replace_text_in_range(None, "e", window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "aed");
        })
        .unwrap();
}

#[gpui::test]
fn cut_is_not_blocked_by_held_shortcut_modifier(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);
    select(cx, &window, 1..3);

    window
        .update(cx, |input, window, cx| {
            window.set_modifiers(Modifiers {
                control: true,
                ..Default::default()
            });
            input.cut(&Cut, window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, cx| {
            assert_eq!(input.value().as_ref(), "ad");
            assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "bc");
        })
        .unwrap();
}

#[gpui::test]
fn shortcut_key_text_is_not_inserted(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);
    select(cx, &window, 1..3);

    window
        .update(cx, |input, window, cx| {
            window.set_modifiers(Modifiers {
                control: true,
                ..Default::default()
            });
            input.replace_text_in_range(None, "x", window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "abcd");
        })
        .unwrap();
}

#[gpui::test]
fn paste_is_not_blocked_by_held_shortcut_modifier(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);
    select(cx, &window, 1..3);

    window
        .update(cx, |input, window, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("XY".to_owned()));
            window.set_modifiers(Modifiers {
                control: true,
                ..Default::default()
            });
            input.paste(&Paste, window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "aXYd");
        })
        .unwrap();
}

#[gpui::test]
fn space_is_inserted_through_input_handler(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx));
    let window = input_with_value(cx);

    window
        .update(cx, |input, window, cx| {
            input.replace_text_in_range(None, " ", window, cx);
        })
        .unwrap();

    window
        .update(cx, |input, _, _| {
            assert_eq!(input.value().as_ref(), "abcd ");
        })
        .unwrap();
}
