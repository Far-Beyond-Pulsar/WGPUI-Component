use std::ops::Range;

use gpui::{App, SharedString};
use ropey::Rope;

use crate::input::RopeExt as _;

use super::text_wrapper::TextWrapper;
use crate::highlighter::DiagnosticSet;
#[cfg(not(target_family = "wasm"))]
use crate::highlighter::SyntaxHighlighter;
#[cfg(not(target_family = "wasm"))]
use std::cell::RefCell;
#[cfg(not(target_family = "wasm"))]
use std::rc::Rc;
#[cfg(not(target_family = "wasm"))]
use tree_sitter::InputEdit;

#[derive(Debug, Copy, Clone)]
pub struct TabSize {
    /// Default is 2
    pub tab_size: usize,
    /// Set true to use `\t` as tab indent, default is false
    pub hard_tabs: bool,
}

impl Default for TabSize {
    fn default() -> Self {
        Self {
            tab_size: 2,
            hard_tabs: false,
        }
    }
}

impl TabSize {
    pub(super) fn to_string(&self) -> SharedString {
        if self.hard_tabs {
            "\t".into()
        } else {
            " ".repeat(self.tab_size).into()
        }
    }
}

#[derive(Default, Clone)]
pub enum InputMode {
    #[default]
    SingleLine,
    MultiLine {
        tab: TabSize,
        rows: usize,
    },
    AutoGrow {
        rows: usize,
        min_rows: usize,
        max_rows: usize,
    },
    #[cfg(not(target_family = "wasm"))]
    CodeEditor {
        tab: TabSize,
        rows: usize,
        /// Show line number
        line_number: bool,
        language: SharedString,
        highlighter: Rc<RefCell<Option<SyntaxHighlighter>>>,
        diagnostics: DiagnosticSet,
    },
}

#[allow(unused)]
impl InputMode {
    #[inline]
    pub(super) fn is_single_line(&self) -> bool {
        matches!(self, InputMode::SingleLine)
    }

    #[inline]
    pub(super) fn is_code_editor(&self) -> bool {
        #[cfg(target_family = "wasm")]
        return false;
        #[cfg(not(target_family = "wasm"))]
        matches!(self, InputMode::CodeEditor { .. })
    }

    #[inline]
    pub(super) fn is_auto_grow(&self) -> bool {
        matches!(self, InputMode::AutoGrow { .. })
    }

    #[inline]
    pub(super) fn is_multi_line(&self) -> bool {
        #[cfg(not(target_family = "wasm"))]
        return matches!(
            self,
            InputMode::MultiLine { .. } | InputMode::AutoGrow { .. } | InputMode::CodeEditor { .. }
        );
        #[cfg(target_family = "wasm")]
        matches!(
            self,
            InputMode::MultiLine { .. } | InputMode::AutoGrow { .. }
        )
    }

    pub(super) fn set_rows(&mut self, new_rows: usize) {
        match self {
            InputMode::MultiLine { rows, .. } => {
                *rows = new_rows;
            }
            #[cfg(not(target_family = "wasm"))]
            InputMode::CodeEditor { rows, .. } => {
                *rows = new_rows;
            }
            InputMode::AutoGrow {
                rows,
                min_rows,
                max_rows,
            } => {
                *rows = new_rows.clamp(*min_rows, *max_rows);
            }
            _ => {}
        }
    }

    pub(super) fn update_auto_grow(&mut self, text_wrapper: &TextWrapper) {
        if self.is_single_line() {
            return;
        }

        let wrapped_lines = text_wrapper.len();
        self.set_rows(wrapped_lines);
    }

    /// At least 1 row be return.
    pub(super) fn rows(&self) -> usize {
        let base = match self {
            InputMode::MultiLine { rows, .. } => *rows,
            #[cfg(not(target_family = "wasm"))]
            InputMode::CodeEditor { rows, .. } => *rows,
            InputMode::AutoGrow { rows, .. } => *rows,
            _ => 1,
        };
        base.max(1)
    }

    /// At least 1 row be return.
    #[allow(unused)]
    pub(super) fn min_rows(&self) -> usize {
        let base = match self {
            InputMode::MultiLine { .. } => 1,
            #[cfg(not(target_family = "wasm"))]
            InputMode::CodeEditor { .. } => 1,
            InputMode::AutoGrow { min_rows, .. } => *min_rows,
            _ => 1,
        };
        base.max(1)
    }

    #[allow(unused)]
    pub(super) fn max_rows(&self) -> usize {
        let base = match self {
            InputMode::MultiLine { .. } => usize::MAX,
            #[cfg(not(target_family = "wasm"))]
            InputMode::CodeEditor { .. } => usize::MAX,
            InputMode::AutoGrow { max_rows, .. } => *max_rows,
            _ => 1,
        };
        base
    }

    /// Return false if the mode is not [`InputMode::CodeEditor`].
    #[allow(unused)]
    #[inline]
    pub(super) fn line_number(&self) -> bool {
        #[cfg(target_family = "wasm")]
        return false;
        #[cfg(not(target_family = "wasm"))]
        match self {
            InputMode::CodeEditor { line_number, .. } => *line_number,
            _ => false,
        }
    }

    #[inline]
    pub(super) fn tab_size(&self) -> Option<&TabSize> {
        match self {
            InputMode::MultiLine { tab, .. } => Some(tab),
            #[cfg(not(target_family = "wasm"))]
            InputMode::CodeEditor { tab, .. } => Some(tab),
            _ => None,
        }
    }

    #[cfg(not(target_family = "wasm"))]
    pub(super) fn update_highlighter(
        &mut self,
        selected_range: &Range<usize>,
        text: &Rope,
        new_text: &str,
        force: bool,
        cx: &mut App,
    ) {
        match &self {
            InputMode::CodeEditor {
                language,
                highlighter,
                ..
            } => {
                if !force && highlighter.borrow().is_some() {
                    return;
                }

                // When full text changed, the selected_range may be out of bound (The before version).
                let mut selected_range = selected_range.clone();
                selected_range.end = selected_range.end.min(text.len());

                let changed_len = new_text.len() as isize - selected_range.len() as isize;
                let new_end = (selected_range.end as isize + changed_len) as usize;

                // If the highlighter was just nulled (e.g. by set_highlighter after a
                // language change), the no-op edit (0..0 → 0) from render() would leave
                // the tree-sitter tree empty and no syntax colours would be applied.
                // A None edit triggers a full re-parse of the buffer.
                let was_null = highlighter.borrow().is_none();

                let mut highlighter = highlighter.borrow_mut();
                if highlighter.is_none() {
                    let new_highlighter = SyntaxHighlighter::new(language);
                    highlighter.replace(new_highlighter);
                }

                let Some(highlighter) = highlighter.as_mut() else {
                    return;
                };

                if was_null {
                    highlighter.update(None, text);
                } else {
                    let start_pos = text.offset_to_point(selected_range.start);
                    let old_end_pos = text.offset_to_point(selected_range.end);
                    let new_end_pos = text.offset_to_point(new_end);

                    let edit = InputEdit {
                        start_byte: selected_range.start,
                        old_end_byte: selected_range.end,
                        new_end_byte: new_end,
                        start_position: start_pos,
                        old_end_position: old_end_pos,
                        new_end_position: new_end_pos,
                    };

                    highlighter.update(Some(edit), text);
                }
            }
            _ => {}
        }
    }

    #[allow(unused)]
    pub(super) fn diagnostics(&self) -> Option<&DiagnosticSet> {
        #[cfg(target_family = "wasm")]
        return None;
        #[cfg(not(target_family = "wasm"))]
        match self {
            InputMode::CodeEditor { diagnostics, .. } => Some(diagnostics),
            _ => None,
        }
    }

    pub(super) fn diagnostics_mut(&mut self) -> Option<&mut DiagnosticSet> {
        #[cfg(target_family = "wasm")]
        return None;
        #[cfg(not(target_family = "wasm"))]
        match self {
            InputMode::CodeEditor { diagnostics, .. } => Some(diagnostics),
            _ => None,
        }
    }

    /// Returns a clone of the syntax highlighter handle for code-editor mode.
    #[cfg(not(target_family = "wasm"))]
    pub(in crate::input) fn highlighter_ref(
        &self,
    ) -> Option<Rc<RefCell<Option<SyntaxHighlighter>>>> {
        match self {
            InputMode::CodeEditor { highlighter, .. } => Some(highlighter.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TabSize;

    #[test]
    fn test_tab_size() {
        let tab = TabSize {
            tab_size: 2,
            hard_tabs: false,
        };
        assert_eq!(tab.to_string(), "  ");
        let tab = TabSize {
            tab_size: 4,
            hard_tabs: false,
        };
        assert_eq!(tab.to_string(), "    ");

        let tab = TabSize {
            tab_size: 2,
            hard_tabs: true,
        };
        assert_eq!(tab.to_string(), "\t");
        let tab = TabSize {
            tab_size: 4,
            hard_tabs: true,
        };
        assert_eq!(tab.to_string(), "\t");
    }
}
