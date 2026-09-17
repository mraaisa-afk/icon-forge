//! WASM host ABI for the Icon Forge editor.
//!
//! ## Why a hand-rolled ABI instead of `wasm-bindgen`
//!
//! * This workspace is deliberately near-zero-dependency (§1.2 freezes
//!   `isg-core` with no dependencies at all); `wasm-bindgen` would bring a
//!   proc-macro and a `serde`-sized tree along for one boundary.
//! * A plain `cdylib` with `extern "C"` exports builds with nothing but
//!   `rustc --target wasm32-unknown-unknown` — the same keystone check that
//!   already guards `isg-core` (§1.2) — so the exact bytes that ship can also be
//!   built and **run under Node** during local verification.
//! * No `unsafe`, no `js_sys`: the module owns its two scratch tables in its own
//!   linear memory and the host reads them through exported addresses
//!   ([`editor_in_ptr`], [`editor_out_ptr`]).
//!
//! ## Boundary
//!
//! The host calls [`editor_call`] with a feature number and two scalar
//! arguments; every feature is documented in [`abi::feature`]. Floats cross as
//! `f32::to_bits`, geometry crosses as the self-describing word blob in
//! [`doc_blob`], and text crosses as UTF-8 bytes packed into the output table.
//! Bad input never panics — a trap would lose the user's unsaved edits — it
//! comes back as `0` plus an error code ([`abi::ERR_*`]).
//!
//! The crate denies `unsafe_code` everywhere except [`ffi`], which declares the
//! exported symbols (naming a symbol for the linker is inherently an ABI
//! commitment) and forwards to this module without any `unsafe` block.
//!
//! ARCHITECTURE.md §2 sets the rule this crate exists to satisfy: interactive,
//! per-frame work never crosses the IPC boundary, so selection, hit-testing,
//! dragging and undo all run here, inside the webview.

#![deny(unsafe_code)]
#![deny(missing_docs)]

pub mod abi;
pub mod doc_blob;
pub mod ffi;

pub use abi::{feature, Abi, ABI_VERSION};
pub use doc_blob::{decode_doc, encode_doc, encode_path};
// The engine itself, re-exported so the host-side tests and the native tooling
// can talk about documents without a second dependency edge.
pub use isg_core::editor;
pub use isg_core::editor::{Command, CommandError, Doc, Editor, Node, NodeId, Point, Subpath};
