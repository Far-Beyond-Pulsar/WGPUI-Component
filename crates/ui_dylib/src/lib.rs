//! The WGPUI component library as one shared library.
//!
//! This crate contains `ui` and every crate it links that no other shared library holds
//! (tree-sitter and its languages, markdown, text shaping). A binary or plugin library that
//! links it (`use ui_dylib as _;`) gets those crates from this library, so the components'
//! process-wide state exists once per process. The framework (and wgpu) come from
//! `wgpui_dylib`.

// Linked, not just depended on: a crate this library shares with another shared library must
// come from that library, or it would be in both (rustc: "only shows up once").
use wgpui_dylib as _;

pub use ui;

// tree-sitter's C API, exported for the crates outside this library that call it (build.rs).
include!(concat!(env!("OUT_DIR"), "/native_exports.rs"));
