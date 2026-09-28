//! Virtual property rows backed by StyleRefinement's serde representation.
//! Edits are validated as a complete style before reaching the inspected view.
use super::*;
use crate::{
    input::Input,
    scroll::{Scrollbar, ScrollbarState},
};

struct Property {
    path: Vec<String>,
    value: serde_json::Value,
}

enum PropertyRow {
    Heading(&'static str),
    Value(Property),
}

fn group(property: &Property) -> usize {
    match property
        .path
        .first()
        .map(String::as_str)
        .unwrap_or_default()
    {
        "display" | "position" | "overflow" | "inset" => 0,
        name if name.starts_with("flex")
            || name.starts_with("align")
            || name.starts_with("justify")
            || name == "gap" =>
        {
            1
        }
        "margin" | "padding" => 2,
        "size" | "min_size" | "max_size" | "aspect_ratio" => 3,
        _ => 4,
    }
}

struct PropertyInput {
    input: Entity<InputState>,
    value: serde_json::Value,
    _subscription: Subscription,
}

fn display_value(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn collect(value: &serde_json::Value, path: &mut Vec<String>, rows: &mut Vec<Property>) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                path.push(key.clone());
                collect(value, path, rows);
                path.pop();
            }
        }
        serde_json::Value::Null => {}
        _ => rows.push(Property {
            path: path.clone(),
            value: value.clone(),
        }),
    }
}

fn edit(style: &StyleRefinement, path: &[String], text: &str) -> Result<StyleRefinement> {
    let mut json = serde_json::to_value(style)?;
    let mut value = &mut json;
    for key in path {
        value = value
            .get_mut(key)
            .ok_or_else(|| anyhow::anyhow!("Property no longer exists"))?;
    }
    *value = if value.is_string() {
        // Enum names and CSS-like lengths are strings even when the user's
        // input happens to be valid JSON (for example a unitless zero).
        serde_json::Value::String(
            serde_json::from_str::<String>(text).unwrap_or_else(|_| text.into()),
        )
    } else {
        serde_json::from_str(text)?
    };
    Ok(serde_json::from_value(json)?)
}

pub(in crate::inspector) fn render(
    this: &mut DivInspector,
    window: &mut Window,
    cx: &mut Context<DivInspector>,
) -> AnyElement {
    let Some(state) = this.inspector_state.as_ref() else {
        return div().into_any_element();
    };
    let mut rows = Vec::new();
    if let Ok(json) = serde_json::to_value(state.base_style.as_ref()) {
        collect(&json, &mut Vec::new(), &mut rows);
    }
    rows.sort_by_key(group);
    let mut grouped = Vec::new();
    let mut previous = None;
    for property in rows {
        let category = group(&property);
        if previous != Some(category) {
            grouped.push(PropertyRow::Heading(
                ["Layout", "Flex", "Spacing", "Size", "Visual"][category],
            ));
            previous = Some(category);
        }
        grouped.push(PropertyRow::Value(property));
    }
    let rows = grouped;
    let count = rows.len();
    let owner = cx.entity().downgrade();
    let selected = this.inspector_id.clone();
    let scroll = this.property_scroll.clone();
    let scrollbar = window
        .use_keyed_state("style-property-scrollbar", cx, |_, _| {
            ScrollbarState::default()
        })
        .read(cx)
        .clone();
    let list = uniform_list("style-properties", count, move |range, window, cx| {
        range
            .map(|index| {
                let row = match &rows[index] {
                    PropertyRow::Value(row) => row,
                    PropertyRow::Heading(title) => {
                        return div()
                            .h(px(34.))
                            .pt_3()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(*title)
                            .into_any_element()
                    }
                };
                let path = row.path.clone();
                let label = path.join(".");
                let value = row.value.clone();
                let owner = owner.clone();
                let selected = selected.clone();
                let editor = window.use_keyed_state(
                    SharedString::from(format!("style-property-{selected:?}-{label}")),
                    cx,
                    |window, cx| {
                        let input = cx.new(|cx| {
                            InputState::new(window, cx).default_value(display_value(&value))
                        });
                        let subscription =
                            cx.subscribe_in(&input, window, move |_, input, event, window, cx| {
                                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur)
                                {
                                    let text = input.read(cx).value();
                                    let _ = owner.update(cx, |inspector, cx| {
                                        // A focused input can blur after the selection has moved.
                                        if inspector.inspector_id != selected {
                                            return;
                                        }
                                        let Some(state) = inspector.inspector_state.as_ref() else {
                                            return;
                                        };
                                        match edit(&state.base_style, &path, text.as_str()) {
                                            Ok(style) => {
                                                inspector.property_error = None;
                                                inspector.json_state.editing = false;
                                                inspector.rust_state.editing = false;
                                                inspector
                                                    .update_json_from_style(&style, window, cx);
                                                let converted = inspector
                                                    .update_rust_from_style(&style, window, cx);
                                                inspector.unconvertible_style =
                                                    style.subtract(&converted);
                                                inspector.update_element_style(style, window, cx);
                                            }
                                            Err(error) => {
                                                inspector.property_error =
                                                    Some(error.to_string().into());
                                            }
                                        }
                                        cx.notify();
                                    });
                                }
                            });
                        PropertyInput {
                            input,
                            value: value.clone(),
                            _subscription: subscription,
                        }
                    },
                );
                editor.update(cx, |editor, cx| {
                    if editor.value != row.value {
                        editor.value = row.value.clone();
                        editor.input.update(cx, |input, cx| {
                            input.set_value(display_value(&row.value), window, cx)
                        });
                    }
                });
                h_flex()
                    .h(px(34.))
                    .gap_2()
                    .text_xs()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .child(Input::new(&editor.read(cx).input).small().w(px(126.)))
                    .into_any_element()
            })
            .collect()
    })
    .track_scroll(&scroll)
    .w_full()
    .h_full();
    let rust = style_to_rust(&state.base_style).0;
    v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Edit a value and press Enter to apply."),
        )
        .when_some(this.property_error.clone(), |el, error| {
            el.child(Alert::error("property-error", error).text_xs())
        })
        .child(
            div()
                .relative()
                .h(px((count.clamp(1, 10) * 34) as f32))
                .overflow_hidden()
                .when(count == 0, |el| {
                    el.child(div().text_sm().child("No explicit style properties."))
                })
                .when(count > 0, |el| {
                    el.child(list)
                        .child(Scrollbar::vertical(&scrollbar, &scroll))
                }),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .child(
                    Button::new("style-add-property")
                        .small()
                        .ghost()
                        .label(if this.show_code {
                            "Hide code"
                        } else {
                            "+ Add property / Code"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_code = !this.show_code;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("style-copy-rust")
                        .small()
                        .label("Copy Rust")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                rust.to_string(),
                            ));
                        }),
                )
                .child(
                    Button::new("style-reset")
                        .small()
                        .ghost()
                        .label("Reset")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reset_style(window, cx);
                            cx.notify();
                        })),
                ),
        )
        .when(this.show_code, |el| {
            el.child(this.render_code_editors(window, cx))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_nested_properties_and_skips_unset_values() {
        let json = serde_json::json!({"size": {"width": 12, "height": null}, "display": "Flex"});
        let mut rows = Vec::new();
        collect(&json, &mut Vec::new(), &mut rows);
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|row| row.path == ["size", "width"] && row.value == 12));
    }

    #[test]
    fn rejects_missing_property_without_changing_style() {
        assert!(edit(&StyleRefinement::default(), &["not_a_style".into()], "12").is_err());
    }

    #[test]
    fn edits_a_dimension_and_preserves_other_properties() {
        let style = div().w(px(64.)).h(px(20.)).style().clone();
        let updated = edit(&style, &["size".into(), "width".into()], "128px").unwrap();
        assert_eq!(updated.size.width, Some(px(128.).into()));
        assert_eq!(updated.size.height, style.size.height);
        assert_eq!(style.size.width, Some(px(64.).into()));
        assert!(edit(&style, &["size".into(), "width".into()], "not-a-length").is_err());
    }
}
