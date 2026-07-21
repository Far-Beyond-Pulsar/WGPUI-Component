use std::{cell::OnceCell, collections::HashMap, fmt::Write as _, ops::Range, rc::Rc, sync::OnceLock};

use anyhow::Result;
use gpui::{
    actions, div, inspector_reflection::FunctionReflection, prelude::FluentBuilder, px, uniform_list,
    AnyElement, App, AppContext, Context, DivInspectorState, Entity, Inspector, InspectorElementId,
    InspectorEventListener, InspectorLayoutInfo, InspectorTab, InspectorTreeNode,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Refineable as _, Render,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Task, Window,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit, Diagnostic,
    DiagnosticSeverity, Position, TextEdit,
};
use ropey::Rope;

use crate::{
    alert::Alert,
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    description_list::DescriptionList,
    h_flex,
    input::{CompletionProvider, InputEvent, InputState, RopeExt, TabSize, TextInput},
    link::Link,
    tab::{Tab, TabBar},
    v_flex, ActiveTheme, IconName, Selectable, Sizable, TITLE_BAR_HEIGHT,
};

actions!(inspector, [ToggleInspector]);

/// Initialize the inspector and register the action to toggle it.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(vec![
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-i", ToggleInspector, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-i", ToggleInspector, None),
    ]);

    cx.on_action(|_: &ToggleInspector, cx| {
        let Some(active_window) = cx.active_window() else {
            return;
        };

        cx.defer(move |cx| {
            _ = active_window.update(cx, |_, window, cx| {
                window.toggle_inspector(cx);
            });
        });
    });

    let inspector_el = OnceCell::new();
    cx.register_inspector_element(move |id, state: &DivInspectorState, window, cx| {
        let el = inspector_el.get_or_init(|| cx.new(|cx| DivInspector::new(window, cx)));
        el.update(cx, |this, cx| {
            this.update_inspected_element(id, state.clone(), window, cx);
            this.render(window, cx).into_any_element()
        })
    });

    cx.set_inspector_renderer(Box::new(render_inspector));
}

struct EditorState {
    /// The input state for the editor.
    state: Entity<InputState>,
    /// Error to display from parsing the input, or if serialization errors somehow occur.
    error: Option<SharedString>,
    /// Whether the editor is currently being edited.
    editing: bool,
}

pub struct DivInspector {
    inspector_id: Option<InspectorElementId>,
    inspector_state: Option<DivInspectorState>,
    rust_state: EditorState,
    json_state: EditorState,
    /// Initial style before any edits
    initial_style: StyleRefinement,
    /// Part of the initial style that could not be converted to Rust code
    unconvertible_style: StyleRefinement,
    _subscriptions: Vec<Subscription>,
}

impl DivInspector {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let lsp_provider = Rc::new(LspProvider {});

        let json_input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("json")
                .line_number(false)
        });

        let rust_input_state = cx.new(|cx| {
            let mut editor = InputState::new(window, cx)
                .code_editor("rust")
                .line_number(false)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                });

            editor.lsp.completion_provider = Some(lsp_provider.clone());
            editor
        });

        let _subscriptions = vec![
            cx.subscribe_in(
                &json_input_state,
                window,
                |this: &mut DivInspector, state, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        let new_style = state.read(cx).value();
                        this.edit_json(new_style.as_str(), window, cx);
                    }
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &rust_input_state,
                window,
                |this: &mut DivInspector, state, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        let new_style = state.read(cx).value();
                        this.edit_rust(new_style.as_str(), window, cx);
                    }
                    _ => {}
                },
            ),
        ];

        let rust_state = EditorState {
            state: rust_input_state,
            error: None,
            editing: false,
        };

        let json_state = EditorState {
            state: json_input_state,
            error: None,
            editing: false,
        };

        Self {
            inspector_id: None,
            inspector_state: None,
            rust_state,
            json_state,
            initial_style: Default::default(),
            unconvertible_style: Default::default(),
            _subscriptions,
        }
    }

    pub fn update_inspected_element(
        &mut self,
        inspector_id: InspectorElementId,
        state: DivInspectorState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Skip updating if the inspector ID hasn't changed
        if self.inspector_id.as_ref() == Some(&inspector_id) {
            return;
        }

        let initial_style = state.base_style.as_ref();
        self.initial_style = initial_style.clone();
        self.json_state.editing = false;
        self.update_json_from_style(initial_style, window, cx);
        self.rust_state.editing = false;
        let rust_style = self.update_rust_from_style(initial_style, window, cx);
        self.unconvertible_style = initial_style.subtract(&rust_style);
        self.inspector_id = Some(inspector_id);
        self.inspector_state = Some(state);
        cx.notify();
    }

    fn edit_json(&mut self, code: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.json_state.editing {
            self.json_state.editing = true;
            return;
        }

        match serde_json::from_str::<StyleRefinement>(code) {
            Ok(new_style) => {
                self.json_state.error = None;
                self.rust_state.error = None;
                self.rust_state.editing = false;
                let rust_style = self.update_rust_from_style(&new_style, window, cx);
                self.unconvertible_style = new_style.subtract(&rust_style);
                self.update_element_style(new_style, window, cx);
            }
            Err(e) => {
                self.json_state.error = Some(e.to_string().trim_end().to_string().into());
                window.refresh();
            }
        }
    }

    fn edit_rust(&mut self, code: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rust_state.editing {
            self.rust_state.editing = true;
            return;
        }

        let (new_style, diagnostics) = rust_to_style(self.unconvertible_style.clone(), code);
        self.rust_state.state.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.clear();
                set.extend(diagnostics);
            }
            cx.notify();
        });
        self.json_state.error = None;
        self.json_state.editing = false;
        self.update_json_from_style(&new_style, window, cx);
        self.update_element_style(new_style, window, cx);
    }

    fn update_element_style(
        &self,
        style: StyleRefinement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.with_inspector_state::<DivInspectorState, _>(
            self.inspector_id.as_ref(),
            cx,
            |state, _window| {
                if let Some(state) = state {
                    *state.base_style = style;
                }
            },
        );
        window.refresh();
    }

    fn reset_style(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rust_state.editing = false;
        let rust_style = self.update_rust_from_style(&self.initial_style, window, cx);
        self.unconvertible_style = self.initial_style.subtract(&rust_style);
        self.json_state.editing = false;
        self.update_json_from_style(&self.initial_style, window, cx);
        if let Some(state) = self.inspector_state.as_mut() {
            *state.base_style = self.initial_style.clone();
        }
    }

    fn update_json_from_style(
        &self,
        style: &StyleRefinement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.json_state.state.update(cx, |state, cx| {
            state.set_value(style_to_json(style), window, cx);
        });
    }

    fn update_rust_from_style(
        &self,
        style: &StyleRefinement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> StyleRefinement {
        self.rust_state.state.update(cx, |state, cx| {
            let (rust_code, rust_style) = style_to_rust(style);
            state.set_value(rust_code, window, cx);
            rust_style
        })
    }
}

fn style_to_json(style: &StyleRefinement) -> String {
    serde_json::to_string_pretty(style).unwrap_or_else(|e| format!("{{ \"error\": \"{}\" }}", e))
}

struct StyleMethods {
    table: Vec<(Box<StyleRefinement>, FunctionReflection<StyleRefinement>)>,
    map: HashMap<&'static str, FunctionReflection<StyleRefinement>>,
}

impl StyleMethods {
    fn get() -> &'static Self {
        static STYLE_METHODS: OnceLock<StyleMethods> = OnceLock::new();
        STYLE_METHODS.get_or_init(|| {
            let table: Vec<_> = gpui::styled_reflection::methods::<StyleRefinement>()
                .into_iter()
                .map(|method| (Box::new(method.invoke(StyleRefinement::default())), method))
                .collect();
            let map = table
                .iter()
                .map(|(_, method)| (method.name, method.clone()))
                .collect();

            Self { table, map }
        })
    }
}

fn style_to_rust(input_style: &StyleRefinement) -> (String, StyleRefinement) {
    let methods: Vec<_> = StyleMethods::get()
        .table
        .iter()
        .filter_map(|(style, method)| {
            if input_style.is_superset_of(style) {
                Some(method)
            } else {
                None
            }
        })
        .collect();
    let mut code = "fn build() -> Div {\n    div()\n".to_string();
    let mut style = StyleRefinement::default();
    for method in methods {
        let before_invoke = style.clone();
        style = method.invoke(style);
        if style != before_invoke {
            _ = write!(code, "        .{}()\n", method.name);
        }
    }
    code.push_str("}");
    (code, style)
}

fn rust_to_style(mut style: StyleRefinement, source: &str) -> (StyleRefinement, Vec<Diagnostic>) {
    let rope = Rope::from(source);
    let Some(begin) = source.find("div()").map(|i| i + "div()".len()) else {
        let start_pos = Position::new(0, 0);
        let end_pos = rope.offset_to_position(rope.len());

        return (
            style,
            vec![Diagnostic {
                range: lsp_types::Range::new(start_pos, end_pos),
                severity: Some(DiagnosticSeverity::ERROR),
                message: "expected `div()`".into(),
                ..Default::default()
            }],
        );
    };

    let mut methods = vec![];
    let mut offset = 0;
    let mut method_offset = 0;
    let mut method = String::new();
    for line in rope.iter_lines() {
        if line.to_string().trim().starts_with("//") {
            offset += line.len() + 1;
            continue;
        }

        for c in line.chars() {
            offset += c.len_utf8();
            if offset < begin {
                continue;
            }

            if c.is_ascii_alphanumeric() || c == '_' {
                method.push(c);
                method_offset = offset;
            } else {
                if !method.is_empty() {
                    methods.push((method_offset, method.clone()));
                }
                method.clear();
            }
        }

        // +1 \n
        offset += 1;
    }

    let mut diagnostics = vec![];
    let style_methods = StyleMethods::get();

    for (offset, method) in methods {
        match style_methods.map.get(method.as_str()) {
            Some(method_reflection) => style = method_reflection.invoke(style),
            None => {
                let message = format!("unknown method `{}`", method);
                let start = rope.offset_to_position(offset.saturating_sub(method.len()));
                let end = rope.offset_to_position(offset);
                let diagnostic = lsp_types::Diagnostic {
                    range: lsp_types::Range::new(start, end),
                    severity: Some(DiagnosticSeverity::ERROR),
                    message,
                    ..Default::default()
                };

                diagnostics.push(diagnostic);
            }
        }
    }

    (style, diagnostics)
}

impl Render for DivInspector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().gap_y_4().text_sm().when_some(
            self.inspector_state.as_ref(),
            |this, state| {
                this.child(
                    DescriptionList::new()
                        .columns(1)
                        .label_width(px(110.))
                        .bordered(false)
                        .child("Origin", format!("{}", state.bounds.origin), 1)
                        .child("Size", format!("{}", state.bounds.size), 1)
                        .child("Content Size", format!("{}", state.content_size), 1),
                )
                .child(
                    v_flex()
                        .h(px(140.))
                        .gap_y_3()
                        .child(
                            h_flex()
                                .justify_between()
                                .gap_x_2()
                                .child("Rust Styles")
                                .child(Button::new("rust-reset").label("Reset").small().on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.reset_style(window, cx);
                                    }),
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_y_1()
                                .font_family("JetBrainsMono-Regular")
                                .text_size(px(12.))
                                .child(TextInput::new(&self.rust_state.state).h(px(100.)))
                                .when_some(self.rust_state.error.clone(), |this, err| {
                                    this.child(Alert::error("rust-error", err).text_xs())
                                }),
                        ),
                )
                .child(
                    v_flex()
                        .h(px(140.))
                        .gap_y_3()
                        .flex_shrink_0()
                        .child(
                            h_flex()
                                .gap_x_2()
                                .child(div().flex_1().child("JSON Styles"))
                                .child(Button::new("json-reset").label("Reset").small().on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.reset_style(window, cx);
                                    }),
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_y_1()
                                .font_family("JetBrainsMono-Regular")
                                .text_size(px(12.))
                                .child(TextInput::new(&self.json_state.state).h(px(100.)))
                                .when_some(self.json_state.error.clone(), |this, err| {
                                    this.child(Alert::error("json-error", err).text_xs())
                                }),
                        ),
                )
            },
        )
    }
}

fn render_inspector(
    inspector: &mut Inspector,
    window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let tab = inspector.active_tab();
    let inspector_element_id = inspector.active_element_id().cloned();

    v_flex()
        .id("inspector")
        .font_family(".SystemUIFont")
        .size_full()
        .bg(cx.theme().background)
        .border_l_1()
        .border_color(cx.theme().border)
        .text_color(cx.theme().foreground)
        .child(render_inspector_title_bar(inspector, window, cx))
        .child(render_inspector_tabs(inspector, window, cx))
        .child(render_tab_content(tab, inspector, window, cx))
        .into_any_element()
}

fn render_inspector_title_bar(
    inspector: &mut Inspector,
    window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    h_flex()
        .w_full()
        .justify_between()
        .gap_2()
        .h(TITLE_BAR_HEIGHT)
        .line_height(TITLE_BAR_HEIGHT)
        .overflow_x_hidden()
        .px_2()
        .border_b_1()
        .border_color(cx.theme().title_bar_border)
        .bg(cx.theme().title_bar)
        .child(
            h_flex()
                .gap_2()
                .text_sm()
                .child(
                    Button::new("inspect")
                        .icon(IconName::Inspector)
                        .selected(inspector.is_picking())
                        .small()
                        .ghost()
                        .on_click(cx.listener(|this, _, window, _| {
                            this.start_picking();
                            window.refresh();
                        })),
                )
                .child("Inspector"),
        )
        .child(
            Button::new("close")
                .icon(IconName::Close)
                .small()
                .ghost()
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleInspector), cx);
                }),
        )
        .into_any_element()
}

fn render_inspector_tabs(
    inspector: &mut Inspector,
    _window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let active = inspector.active_tab();
    let tabs = InspectorTab::all();
    let active_idx = tabs.iter().position(|t| *t == active).unwrap_or(0);

    TabBar::new("inspector-tabs")
        .underline()
        .selected_index(active_idx)
        .on_click({
            let entity = cx.entity().clone();
            move |idx, _, cx: &mut App| {
                entity.update(cx, |inspector, cx| {
                    inspector.set_tab(tabs[*idx]);
                    cx.notify();
                });
            }
        })
        .children(tabs.iter().map(|tab| Tab::new(tab.label())))
        .into_any_element()
}

fn render_tab_content(
    tab: InspectorTab,
    inspector: &mut Inspector,
    window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let content = match tab {
        InspectorTab::Elements => render_elements_tab(inspector, window, cx),
        InspectorTab::Styles => render_styles_tab(inspector, window, cx),
        InspectorTab::Layout => render_layout_tab(inspector, window, cx),
        InspectorTab::EventListeners => render_listeners_tab(inspector, window, cx),
    };

    if tab == InspectorTab::Elements {
        div().flex_1().child(content).into_any_element()
    } else {
        div().flex_1().overflow_y_scroll().child(content).into_any_element()
    }
}

fn render_elements_tab(
    inspector: &mut Inspector,
    _window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let tree = inspector.element_tree().to_vec();
    if tree.is_empty() {
        return div()
            .p(px(8.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No elements available.")
            .into_any_element();
    }

    let mut rows: Vec<FlattenedRow> = Vec::new();
    flatten_rows(&tree, 0, inspector, &mut rows);
    let item_count = rows.len();

    let entity = cx.entity().clone();
    let rows_rc = Rc::new(rows);

    uniform_list("inspector-tree", item_count, {
        let rows = rows_rc;
        let entity = entity.clone();
        move |range: Range<usize>, _window: &mut Window, cx: &mut App| {
            range.map(|i| {
                let row = &rows[i];
                let indent = row.depth * 14;
                let entity = entity.clone();
                let mut el = div()
                    .id(SharedString::from(format!("tree-{}", i)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(22.))
                    .pl(px(indent as f32))
                    .cursor_pointer()
                    .rounded_sm()
                    .text_xs()
                    .when(row.is_selected, |s| s.bg(gpui::rgba(0x00000033)));

                if row.has_children {
                    let entity_for_toggle = entity.clone();
                    let nid = row.node_id.clone();
                    el = el.child(
                        div()
                            .w(px(14.))
                            .text_color(gpui::rgba(0x888888ff))
                            .child(if row.is_expanded { "▼" } else { "▶" })
                            .on_mouse_down(gpui::MouseButton::Left, move |_, _window, cx| {
                                if let Some(ref id) = nid {
                                    entity_for_toggle.update(cx, |i, cx| {
                                        i.toggle_collapsed(id.clone());
                                        cx.notify();
                                    });
                                }
                            }),
                    );
                } else {
                    el = el.child(div().w(px(14.)));
                }

                el = el.child(
                    div()
                        .text_color(gpui::rgba(0x8888ffff))
                        .child(row.element_type.clone()),
                );
                if !row.display_label.is_empty() {
                    el = el.child(
                        div()
                            .text_color(gpui::rgba(0x888888ff))
                            .ml(px(4.))
                            .child(row.display_label.clone()),
                    );
                }

                if row.node_id.is_some() {
                    let entity_for_click = entity.clone();
                    let nid = row.node_id.clone();
                    let has_ch = row.has_children;
                    el = el.on_click(move |_, window, cx| {
                        if let Some(ref id) = nid {
                            entity_for_click.update(cx, |i, cx| {
                                if i.active_element_id() == Some(id) && has_ch {
                                    i.toggle_collapsed(id.clone());
                                } else {
                                    i.set_active_element_id(id.clone(), window);
                                }
                                cx.notify();
                            });
                        }
                    });
                }

                el.into_any_element()
            }).collect::<Vec<_>>()
        }
    })
    .w_full()
    .h_full()
    .into_any_element()
}

struct FlattenedRow {
    depth: usize,
    has_children: bool,
    is_expanded: bool,
    is_selected: bool,
    node_id: Option<InspectorElementId>,
    element_type: SharedString,
    display_label: SharedString,
}

fn flatten_rows(
    nodes: &[InspectorTreeNode],
    depth: usize,
    inspector: &mut Inspector,
    out: &mut Vec<FlattenedRow>,
) {
    for node in nodes {
        let is_collapsed = node
            .inspector_id
            .as_ref()
            .map_or(false, |id| inspector.is_collapsed(id));

        out.push(FlattenedRow {
            depth,
            has_children: !node.children.is_empty(),
            is_expanded: !is_collapsed,
            is_selected: node.is_selected,
            node_id: node.inspector_id.clone(),
            element_type: node.element_type.clone(),
            display_label: node.display_label.clone(),
        });

        if !is_collapsed && !node.children.is_empty() {
            flatten_rows(&node.children, depth + 1, inspector, out);
        }
    }
}

// ── Styles tab ─────────────────────────────────────────────────────────────

fn render_styles_tab(
    inspector: &mut Inspector,
    window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let has_selection = inspector.active_element_id().is_some();
    let mut elements: Vec<AnyElement> = Vec::new();

    if has_selection {
        let source_location = inspector
            .active_element_id()
            .map(|id| format!("{}", id.path.source_location));

        elements.push(
            v_flex()
                .px(px(8.))
                .py(px(6.))
                .gap_y(px(6.))
                .when_some(source_location, |this, location| {
                    this.child(
                        h_flex()
                            .gap_x_2()
                            .text_xs()
                            .child(
                                Link::new("source-location")
                                    .href(format!("file://{}", location))
                                    .child(location.clone())
                                    .flex_1()
                                    .overflow_x_hidden(),
                            )
                            .child(Clipboard::new("copy-source-location").value(location)),
                    )
                })
                .children(inspector.render_inspector_states(window, cx))
                .into_any_element(),
        );

        if elements.is_empty() {
            elements.push(
                div()
                    .p(px(8.))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No style editors registered for this element type.")
                    .into_any_element(),
            );
        }

        // Computed layout info
        if let Some(layout) = inspector.active_layout() {
            elements.push(
                DescriptionList::new()
                    .columns(1)
                    .label_width(px(100.))
                    .bordered(false)
                    .child("Width", format!("{:.1}px", layout.bounds.size.width.value()), 1)
                    .child("Height", format!("{:.1}px", layout.bounds.size.height.value()), 1)
                    .child("X", format!("{:.1}px", layout.bounds.origin.x.value()), 1)
                    .child("Y", format!("{:.1}px", layout.bounds.origin.y.value()), 1)
                    .child(
                        "Margin",
                        format!(
                            "{:.1} {:.1} {:.1} {:.1}",
                            layout.margin.top.value(),
                            layout.margin.right.value(),
                            layout.margin.bottom.value(),
                            layout.margin.left.value(),
                        ),
                        1,
                    )
                    .child(
                        "Border",
                        format!(
                            "{:.1} {:.1} {:.1} {:.1}",
                            layout.border.top.value(),
                            layout.border.right.value(),
                            layout.border.bottom.value(),
                            layout.border.left.value(),
                        ),
                        1,
                    )
                    .child(
                        "Padding",
                        format!(
                            "{:.1} {:.1} {:.1} {:.1}",
                            layout.padding.top.value(),
                            layout.padding.right.value(),
                            layout.padding.bottom.value(),
                            layout.padding.left.value(),
                        ),
                        1,
                    )
                    .child(
                        "Content",
                        format!(
                            "{:.1} x {:.1}",
                            layout.content_size.width.value(),
                            layout.content_size.height.value(),
                        ),
                        1,
                    )
                    .into_any_element(),
            );
        }
    } else {
        elements.push(
            div()
                .p(px(8.))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Select an element to view its styles.")
                .into_any_element(),
        );
    }

    v_flex().children(elements).into_any_element()
}

// ── Layout tab ─────────────────────────────────────────────────────────────

fn render_layout_tab(
    inspector: &mut Inspector,
    _window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let has_selection = inspector.active_element_id().is_some();

    if !has_selection {
        return div()
            .p(px(8.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Select an element to view its layout.")
            .into_any_element();
    }

    let layout = inspector.active_layout().cloned();

    v_flex()
        .p(px(8.))
        .gap_y(px(6.))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(cx.theme().foreground)
                .child("Box Model"),
        )
        .child(if let Some(ref layout) = layout {
            render_box_model(layout, cx)
        } else {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Layout data not yet available.")
                .into_any_element()
        })
        .child(if let Some(ref layout) = layout {
            render_layout_details(layout, cx)
        } else {
            div().into_any_element()
        })
        .into_any_element()
}

fn render_box_model(layout: &InspectorLayoutInfo, cx: &Context<Inspector>) -> AnyElement {
    let scale = 0.3;
    let margin_w = layout.margin;
    let border_w = layout.border;
    let padding_w = layout.padding;
    let content = layout.content_size;

    let total_width = margin_w.left
        + border_w.left
        + padding_w.left
        + content.width
        + padding_w.right
        + border_w.right
        + margin_w.right;
    let total_height = margin_w.top
        + border_w.top
        + padding_w.top
        + content.height
        + padding_w.bottom
        + border_w.bottom
        + margin_w.bottom;

    let scaled_w = total_width * scale;
    let scaled_h = total_height * scale;

    div()
        .w(scaled_w.max(px(100.)))
        .h(scaled_h.max(px(60.)))
        .relative()
        .child(
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .w(scaled_w)
                .h(scaled_h)
                .bg(cx.theme().chart_1)
                .opacity(0.2),
        )
        .child(
            div()
                .absolute()
                .top(margin_w.top * scale)
                .left(margin_w.left * scale)
                .w((border_w.left + padding_w.left + content.width + padding_w.right + border_w.right) * scale)
                .h((border_w.top + padding_w.top + content.height + padding_w.bottom + border_w.bottom) * scale)
                .bg(cx.theme().chart_2)
                .opacity(0.2),
        )
        .child(
            div()
                .absolute()
                .top((margin_w.top + border_w.top) * scale)
                .left((margin_w.left + border_w.left) * scale)
                .w((padding_w.left + content.width + padding_w.right) * scale)
                .h((padding_w.top + content.height + padding_w.bottom) * scale)
                .bg(cx.theme().chart_3)
                .opacity(0.2),
        )
        .child(
            div()
                .absolute()
                .top((margin_w.top + border_w.top + padding_w.top) * scale)
                .left((margin_w.left + border_w.left + padding_w.left) * scale)
                .w(content.width * scale)
                .h(content.height * scale)
                .bg(cx.theme().chart_4)
                .opacity(0.3),
        )
        .into_any_element()
}

fn render_layout_details(layout: &InspectorLayoutInfo, cx: &Context<Inspector>) -> AnyElement {
    v_flex()
        .gap_y(px(2.))
        .child(layout_section(
            "Position",
            &[
                ("x", layout.bounds.origin.x.value()),
                ("y", layout.bounds.origin.y.value()),
                ("width", layout.bounds.size.width.value()),
                ("height", layout.bounds.size.height.value()),
            ],
            cx,
        ))
        .child(layout_section(
            "Margin",
            &[
                ("top", layout.margin.top.value()),
                ("right", layout.margin.right.value()),
                ("bottom", layout.margin.bottom.value()),
                ("left", layout.margin.left.value()),
            ],
            cx,
        ))
        .child(layout_section(
            "Border",
            &[
                ("top", layout.border.top.value()),
                ("right", layout.border.right.value()),
                ("bottom", layout.border.bottom.value()),
                ("left", layout.border.left.value()),
            ],
            cx,
        ))
        .child(layout_section(
            "Padding",
            &[
                ("top", layout.padding.top.value()),
                ("right", layout.padding.right.value()),
                ("bottom", layout.padding.bottom.value()),
                ("left", layout.padding.left.value()),
            ],
            cx,
        ))
        .child(layout_section(
            "Content",
            &[("width", layout.content_size.width.value()), ("height", layout.content_size.height.value())],
            cx,
        ))
        .into_any_element()
}

fn layout_section(title: &str, values: &[(&str, f32)], cx: &Context<Inspector>) -> AnyElement {
    let title = SharedString::from(title);
    v_flex()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().foreground)
                .font_weight(gpui::FontWeight::BOLD)
                .mb(px(2.))
                .child(title.clone()),
        )
        .children(values.iter().map(|(name, value)| {
            h_flex()
                .px(px(4.))
                .child(
                    div()
                        .w(px(60.))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(*name)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .child(SharedString::from(format!("{:.1}px", value))),
                )
                .into_any_element()
        }))
        .into_any_element()
}

// ── Event Listeners tab ────────────────────────────────────────────────────

fn render_listeners_tab(
    inspector: &mut Inspector,
    _window: &mut Window,
    cx: &mut Context<Inspector>,
) -> AnyElement {
    let has_selection = inspector.active_element_id().is_some();

    if !has_selection {
        return div()
            .p(px(8.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Select an element to view its event listeners.")
            .into_any_element();
    }

    let selected_id = inspector.active_element_id().cloned();
    let mut listeners: Vec<InspectorEventListener> = Vec::new();

    for info in inspector.element_infos() {
        if Some(&info.inspector_id) == selected_id.as_ref() {
            listeners.extend(info.event_listeners.clone());
            break;
        }
    }

    if listeners.is_empty() {
        return div()
            .p(px(8.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No event listeners registered on this element.")
            .into_any_element();
    }

    v_flex()
        .children(listeners.iter().map(|listener| {
            h_flex()
                .px(px(8.))
                .py(px(3.))
                .border_b_1()
                .border_color(cx.theme().border)
                .hover(|style| style.bg(cx.theme().list_hover))
                .child(
                    div()
                        .px(px(4.))
                        .py(px(1.))
                        .rounded_sm()
                        .bg(cx.theme().primary)
                        .text_xs()
                        .text_color(cx.theme().primary_foreground)
                        .child(listener.event_type.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .ml(px(8.))
                        .child(listener.location.clone()),
                )
                .into_any_element()
        }))
        .into_any_element()
}

struct LspProvider {}

impl CompletionProvider for LspProvider {
    fn completions(
        &self,
        rope: &ropey::Rope,
        offset: usize,
        _: lsp_types::CompletionContext,
        _: &mut Window,
        cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        let mut left_offset = 0;
        while left_offset < 100 {
            match rope.char_at(offset.saturating_sub(left_offset)) {
                Some('.') => {
                    break;
                }
                None => break,
                _ => {}
            }
            left_offset += 1;
        }
        let start = offset.saturating_sub(left_offset);
        let trigger_character = rope.slice(start..offset).to_string();
        if !trigger_character.starts_with('.') {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }

        let start_pos = rope.offset_to_position(start);
        let end_pos = rope.offset_to_position(offset);

        cx.background_spawn(async move {
            let styles = StyleMethods::get()
                .map
                .iter()
                .filter_map(|(name, method)| {
                    let prefix = &trigger_character[1..];
                    if name.starts_with(&prefix) {
                        Some(CompletionItem {
                            label: name.to_string(),
                            filter_text: Some(prefix.to_string()),
                            kind: Some(CompletionItemKind::METHOD),
                            detail: Some("()".to_string()),
                            documentation: method
                                .documentation
                                .as_ref()
                                .map(|doc| lsp_types::Documentation::String(doc.to_string())),
                            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                                range: lsp_types::Range {
                                    start: start_pos,
                                    end: end_pos,
                                },
                                new_text: format!(".{}()", name),
                            })),
                            ..Default::default()
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();

            Ok(CompletionResponse::Array(styles))
        })
    }

    fn is_completion_trigger(&self, _: usize, _: &str, _: &mut Context<InputState>) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use gpui::{rems, AbsoluteLength, DefiniteLength, Length};
    use indoc::indoc;
    use lsp_types::Position;

    #[test]
    fn test_rust_to_style() {
        let (style, diagnostics) = super::rust_to_style(
            Default::default(),
            indoc! {r#"
            fn build() -> Div {
                div()
                    .p_1()
                    // This is a comment
                    .mx_2()
            }
            "#},
        );
        assert_eq!(diagnostics, vec![]);
        assert_eq!(
            style.padding.left,
            Some(DefiniteLength::Absolute(AbsoluteLength::Rems(rems(0.25))))
        );
        assert_eq!(
            style.margin.left,
            Some(Length::Definite(DefiniteLength::Absolute(
                AbsoluteLength::Rems(rems(0.5))
            )))
        );

        let (_, diagnostics) = super::rust_to_style(
            Default::default(),
            indoc! {r#"
            fn build() -> Div {
                div()
                    .p_1()
                    // This is a comment
                    .unknown_method
                    .bad_method()
            }
            "#},
        );

        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].message, "unknown method `unknown_method`");
        assert_eq!(diagnostics[0].range.start, Position::new(4, 9));
        assert_eq!(diagnostics[0].range.end, Position::new(4, 23));
        assert_eq!(diagnostics[1].message, "unknown method `bad_method`");
        assert_eq!(diagnostics[1].range.start, Position::new(5, 9));
        assert_eq!(diagnostics[1].range.end, Position::new(5, 19));
    }
}
