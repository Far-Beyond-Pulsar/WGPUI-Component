//! Diagnostic types for LSP and code analysis.
//!
//! Re-exports the canonical types from the UI-local `diagnostics` module.

pub use crate::diagnostics::{CodeAction, Diagnostic, DiagnosticSeverity, TextEdit};
