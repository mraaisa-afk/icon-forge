//! The command dispatcher and the module's shared scratch tables.
//!
//! The module owns two flat `u32` tables in its **own** linear memory: an input
//! table (the host writes documents and command arguments into it) and an output
//! table (the module writes results, node records and path blobs into it). The
//! host adapter reaches them through the exported `editor_in_ptr` /
//! `editor_out_ptr` accessors, which keeps the boundary pointer-free from Rust's
//! point of view: no FFI slice is ever handed out, so this crate builds with
//! `#![forbid(unsafe_code)]` and the native tests exercise exactly the same code
//! path as the browser (see `tests/abi.rs`).
//!
//! Every call is validated and returns a `u32`; failures return `0` and set the
//! error code readable through [`feature::ERROR`]. Nothing panics on host input:
//! a panic would trap the `wasm32` instance and lose unsaved edits.

use core::cell::RefCell;

use isg_core::editor::{
    svg, Affine, AlignEdge, AlignFrame, ArrangeTo, BooleanOp, Command, CommandError, Editor,
    Handle, HandleRef, Node, NodeId, Point, SegKind, SegmentRef, SnapOptions, VertexRef,
    DEFAULT_PICK_TOLERANCE,
};

use crate::doc_blob::{DOC_HEADER_WORDS, NODE_HEADER_WORDS};

/// ABI level of the exported entry points.
pub const ABI_VERSION: u32 = 2;

/// Words available in the input table (4 MiB).
pub const IN_WORDS: usize = 1 << 20;

/// Words available in the output table (1 MiB).
pub const OUT_WORDS: usize = 1 << 18;

/// Largest document [`Abi::call`] will load, so a malformed blob cannot make the
/// module allocate without bound.
pub const MAX_NODES: u32 = 4096;

/// Most guides one `SNAP` answer carries (mirrors `editor::snap::MAX_GUIDES`).
pub const MAX_SNAP_GUIDES: usize = 8;

/// Words per guide record: axis, kind, position, from, to.
pub const GUIDE_WORDS: usize = 5;

/// `SNAP` flags: which target families are enabled.
pub const SNAP_CANVAS: u32 = 1 << 0;
/// `SNAP` flags: snap to other nodes' edges and centres.
pub const SNAP_NODES: u32 = 1 << 1;
/// `SNAP` flags: snap to the grid.
pub const SNAP_GRID: u32 = 1 << 2;
/// Every `SNAP` flag the ABI defines. A request that sets a bit outside this
/// mask is a caller bug and is refused rather than partially obeyed.
pub const SNAP_FLAGS: u32 = SNAP_CANVAS | SNAP_NODES | SNAP_GRID;

/// `PREVIEW` argument `a`: install a preview from the spec in the input table.
pub const PREVIEW_SET: u32 = 0;
/// `PREVIEW` argument `a`: drop the live preview.
pub const PREVIEW_CLEAR: u32 = 1;

/// Words in a document blob header — re-exported so the TypeScript blob writer
/// and the Rust parser cannot drift apart.
pub const DOC_HEADER: usize = DOC_HEADER_WORDS;

/// Words in a node record header (the path blob follows it).
pub const NODE_RECORD_HEADER: usize = NODE_HEADER_WORDS;

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

/// No error.
pub const ERR_NONE: u32 = 0;
/// The feature number is not known to this ABI level.
pub const ERR_BAD_FEATURE: u32 = 1;
/// No document is loaded.
pub const ERR_NO_DOCUMENT: u32 = 2;
/// An argument was malformed (bad offset, bad blob, unknown node id, …).
pub const ERR_BAD_ARGUMENT: u32 = 3;
/// The result does not fit the output table.
pub const ERR_CAPACITY: u32 = 4;
/// There is nothing to undo/redo.
pub const ERR_NO_HISTORY: u32 = 5;
/// The command needs a selection and there is none.
pub const ERR_NO_SELECTION: u32 = 6;
/// The transform would be degenerate (zero or non-finite scale).
pub const ERR_DEGENERATE: u32 = 7;
/// A completely transparent fill was rejected.
pub const ERR_TRANSPARENT: u32 = 8;
/// The command was valid but would not change anything.
pub const ERR_NO_OP: u32 = 9;
/// A referenced node no longer exists.
pub const ERR_MISSING_NODE: u32 = 10;
/// Imported SVG text that could not be read (`4C`).
pub const ERR_MALFORMED_SVG: u32 = 11;

/// Maps an engine failure onto its wire code.
#[must_use]
pub const fn error_code(err: CommandError) -> u32 {
    match err {
        CommandError::NoDocument => ERR_NO_DOCUMENT,
        CommandError::EmptySelection => ERR_NO_SELECTION,
        CommandError::MissingNode(_) => ERR_MISSING_NODE,
        CommandError::DegenerateTransform | CommandError::DegeneratePath => ERR_DEGENERATE,
        CommandError::Transparent => ERR_TRANSPARENT,
        CommandError::IndexOutOfRange => ERR_BAD_ARGUMENT,
        CommandError::MalformedSvg => ERR_MALFORMED_SVG,
        CommandError::BadHistory => ERR_NO_HISTORY,
        CommandError::ZeroDelta | CommandError::NoOp => ERR_NO_OP,
    }
}

// ---------------------------------------------------------------------------
// Features
// ---------------------------------------------------------------------------

/// The feature numbers [`Abi::call`] understands.
///
/// Arguments: `off` means "word offset into the input table", `id` a node id,
/// `bits` an `f32` as `to_bits`, and "out" means the result lands in the output
/// table.
#[allow(missing_docs)]
pub mod feature {
    pub const VERSION: u32 = 0;
    pub const DOC_LOAD: u32 = 1;
    pub const NODE_COUNT: u32 = 2;
    pub const REVISION: u32 = 3;
    pub const NODE_SYNC: u32 = 4;
    pub const NODE_BOUNDS: u32 = 5;
    pub const PATH_FLUSH: u32 = 6;
    pub const SELECTION_COUNT: u32 = 7;
    pub const SELECTION_AT: u32 = 8;
    pub const SELECTION_IDS: u32 = 9;
    pub const SELECTION_BOUNDS: u32 = 10;
    pub const SELECT_ALL: u32 = 11;
    pub const SELECT_CLEAR: u32 = 12;
    pub const SELECT_ONLY: u32 = 13;
    pub const SELECT_ADD: u32 = 14;
    pub const SELECT_TOGGLE: u32 = 15;
    pub const PICK: u32 = 16;
    pub const MARQUEE: u32 = 17;
    pub const GET_TOLERANCE: u32 = 18;
    pub const SET_TOLERANCE: u32 = 19;
    pub const APPLY_SPEC: u32 = 20;
    pub const UNDO: u32 = 21;
    pub const REDO: u32 = 22;
    pub const CAN_UNDO: u32 = 23;
    pub const CAN_REDO: u32 = 24;
    pub const HISTORY_LEN: u32 = 25;
    pub const HISTORY_CURSOR: u32 = 26;
    pub const HISTORY_REDO_DEPTH: u32 = 27;
    pub const HISTORY_DROPPED: u32 = 28;
    pub const UNDO_LABEL: u32 = 29;
    pub const REDO_LABEL: u32 = 30;
    pub const LAST_LABEL: u32 = 31;
    pub const ERROR: u32 = 32;
    pub const DOC_SIZE: u32 = 33;
    pub const SEGMENT_COUNT: u32 = 34;
    pub const NODE_AT: u32 = 35;
    pub const CLOSE: u32 = 36;
    /// Snaps a proposed move of the selection (input: dx, dy, tolerance, flags,
    /// grid step; output: dx, dy, guide count, guide records, terminator).
    ///
    /// `flags` selects the target families. With none selected the delta comes
    /// back unchanged and the guide list is empty — snapping is simply off. A
    /// non-finite delta or tolerance, or a flag bit outside [`SNAP_FLAGS`], is
    /// refused with [`ERR_BAD_ARGUMENT`].
    pub const SNAP: u32 = 37;
    /// Installs (`a = 0`) or drops (`a = 1`) a live transform preview.
    pub const PREVIEW: u32 = 38;

    /// Parses an SVG and reports its shapes as node records, without touching
    /// the document (`4C`). This is how an icon becomes geometry: the host keeps
    /// no parser of its own, and a file can be read before any document exists.
    /// Input: a spec at `a` (see [`SVG_SPEC_WORDS`]). Output: node records,
    /// written exactly as `NODE_SYNC` writes them.
    pub const SVG_NODES: u32 = 39;

    /// Imports an SVG into the document, one node per `<path>` (`4C`).
    /// Input: the same spec at `a`. Requires a document; returns the number of
    /// nodes added and makes them the selection.
    pub const IMPORT_SVG: u32 = 40;
}

/// The highest feature number this ABI level defines.
pub const MAX_FEATURE: u32 = feature::IMPORT_SVG;

/// Words in the spec both SVG features read: the first node id (used by
/// `SVG_NODES`, ignored by `IMPORT_SVG`), the placement matrix, the default
/// fill, the text's byte length, and then the text itself, packed
/// little-endian into as many words as it needs.
pub const SVG_SPEC_WORDS: usize = 9;

// Command spec opcodes (see [`feature::APPLY_SPEC`]).
/// `Translate`: `dx`, `dy`.
pub const OP_TRANSLATE: u32 = 1;
/// `Scale`: `factor`, pivot x, pivot y.
pub const OP_SCALE: u32 = 2;
/// `Rotate`: degrees, pivot x, pivot y.
pub const OP_ROTATE: u32 = 3;
/// `CenterOnCanvas`: no arguments.
pub const OP_CENTER: u32 = 4;
/// `SetFill`: RGBA as `0xRRGGBBAA`.
pub const OP_SET_FILL: u32 = 5;
/// `SetVisible`: 0 or 1.
pub const OP_SET_VISIBLE: u32 = 6;
/// `Reorder`: 1 to move up, 0 to move down.
pub const OP_REORDER: u32 = 7;
/// `Duplicate`: `dx`, `dy`.
pub const OP_DUPLICATE: u32 = 8;
/// `Delete`: no arguments.
pub const OP_DELETE: u32 = 9;
/// Group the selection (`4B`).
pub const OP_GROUP: u32 = 10;
/// Dissolve the selection's groups (`4B`).
pub const OP_UNGROUP: u32 = 11;
/// Move the selection to the front (`param = 0`) or back (`param = 1`) of the
/// z-order (`4B`).
pub const OP_ARRANGE: u32 = 12;
/// Align the selection: `a = frame` (0 = selection box, 1 = canvas),
/// `b = edge` (0–5, see `AlignEdge`) (`4B`).
pub const OP_ALIGN: u32 = 13;
/// Non-uniform scale about a pivot: `sx, sy, px, py` (`4B`).
pub const OP_SCALE_XY: u32 = 14;
/// Move one vertex: `id, subpath, vertex, x, y` (`4C`).
pub const OP_MOVE_POINT: u32 = 15;
/// Move one control handle: `id, subpath, segment, handle (0 = c1, 1 = c2),
/// x, y` (`4C`).
pub const OP_MOVE_HANDLE: u32 = 16;
/// Insert a vertex on a segment at `t`: `id, subpath, segment, t` (`4C`).
pub const OP_INSERT_POINT: u32 = 17;
/// Delete a vertex, joining its neighbours: `id, subpath, vertex` (`4C`).
pub const OP_DELETE_POINT: u32 = 18;
/// Convert a segment: `id, subpath, segment, kind` (`0` = line, `1` = cubic)
/// (`4C`).
pub const OP_SET_SEGMENT: u32 = 19;
/// Combine the selection: `op` (see [`BooleanOp`]) (`4C`).
pub const OP_BOOLEAN: u32 = 20;

/// Packs an RGBA fill into the `0xRRGGBBAA` word the ABI uses.
#[must_use]
pub const fn pack_rgba(rgba: [u8; 4]) -> u32 {
    (rgba[0] as u32) << 24 | (rgba[1] as u32) << 16 | (rgba[2] as u32) << 8 | rgba[3] as u32
}

/// Unpacks a `0xRRGGBBAA` word.
#[must_use]
pub const fn unpack_rgba(word: u32) -> [u8; 4] {
    [
        (word >> 24) as u8,
        (word >> 16) as u8,
        (word >> 8) as u8,
        word as u8,
    ]
}

/// Encodes a command as the word spec [`feature::APPLY_SPEC`] expects.
#[must_use]
pub fn encode_command(command: &Command) -> Vec<u32> {
    let b = f32::to_bits;
    match command {
        Command::Translate { dx, dy } => vec![OP_TRANSLATE, b(*dx), b(*dy)],
        Command::Scale { factor, pivot } => vec![OP_SCALE, b(*factor), b(pivot.0), b(pivot.1)],
        Command::Rotate { degrees, pivot } => vec![OP_ROTATE, b(*degrees), b(pivot.0), b(pivot.1)],
        Command::CenterOnCanvas => vec![OP_CENTER],
        Command::SetFill { to } => vec![OP_SET_FILL, pack_rgba(*to)],
        Command::SetVisible { to } => vec![OP_SET_VISIBLE, u32::from(*to)],
        Command::Reorder { up } => vec![OP_REORDER, u32::from(*up)],
        Command::Duplicate { dx, dy } => vec![OP_DUPLICATE, b(*dx), b(*dy)],
        Command::Delete => vec![OP_DELETE],
        Command::ScaleXY { sx, sy, pivot } => {
            vec![OP_SCALE_XY, b(*sx), b(*sy), b(pivot.0), b(pivot.1)]
        }
        Command::Arrange { to } => vec![OP_ARRANGE, to.raw()],
        Command::Align { frame, edge } => vec![OP_ALIGN, frame.raw(), edge.raw()],
        Command::Group => vec![OP_GROUP],
        Command::Ungroup => vec![OP_UNGROUP],
        Command::MovePoint { id, at, to } => vec![
            OP_MOVE_POINT,
            id.get(),
            at.subpath as u32,
            at.vertex as u32,
            b(to.0),
            b(to.1),
        ],
        Command::MoveHandle { id, at, to } => vec![
            OP_MOVE_HANDLE,
            id.get(),
            at.subpath as u32,
            at.segment as u32,
            match at.handle {
                Handle::C1 => 0,
                Handle::C2 => 1,
            },
            b(to.0),
            b(to.1),
        ],
        Command::InsertPoint { id, at, t } => vec![
            OP_INSERT_POINT,
            id.get(),
            at.subpath as u32,
            at.segment as u32,
            b(*t),
        ],
        Command::DeletePoint { id, at } => vec![
            OP_DELETE_POINT,
            id.get(),
            at.subpath as u32,
            at.vertex as u32,
        ],
        Command::SetSegment { id, at, to } => vec![
            OP_SET_SEGMENT,
            id.get(),
            at.subpath as u32,
            at.segment as u32,
            match to {
                SegKind::Line => 0,
                SegKind::Cubic => 1,
            },
        ],
        Command::Boolean { op } => vec![OP_BOOLEAN, op.raw()],
    }
}

/// Decodes a command spec written by the host.
///
/// # Errors
///
/// Returns [`ERR_BAD_ARGUMENT`] for an unknown opcode or a truncated spec.
pub fn decode_command(words: &[u32]) -> Result<Command, u32> {
    let op = *words.first().ok_or(ERR_BAD_ARGUMENT)?;
    let f = |i: usize| words.get(i).map(|w| f32::from_bits(*w));
    let float = |i: usize| f(i).ok_or(ERR_BAD_ARGUMENT);
    // Subpath/vertex/segment addresses cross as plain word indices. They are
    // `usize` in the engine, and an address that does not exist is refused by
    // the engine itself rather than here.
    let index =
        |i: usize| -> Result<usize, u32> { Ok(*words.get(i).ok_or(ERR_BAD_ARGUMENT)? as usize) };
    let flag = |i: usize| -> Result<bool, u32> { Ok(*words.get(i).ok_or(ERR_BAD_ARGUMENT)? != 0) };
    match op {
        OP_TRANSLATE => Ok(Command::Translate {
            dx: float(1)?,
            dy: float(2)?,
        }),
        OP_SCALE => Ok(Command::Scale {
            factor: float(1)?,
            pivot: (float(2)?, float(3)?),
        }),
        OP_ROTATE => Ok(Command::Rotate {
            degrees: float(1)?,
            pivot: (float(2)?, float(3)?),
        }),
        OP_CENTER => Ok(Command::CenterOnCanvas),
        OP_SET_FILL => Ok(Command::SetFill {
            to: unpack_rgba(*words.get(1).ok_or(ERR_BAD_ARGUMENT)?),
        }),
        OP_SET_VISIBLE => Ok(Command::SetVisible { to: flag(1)? }),
        OP_REORDER => Ok(Command::Reorder { up: flag(1)? }),
        OP_DUPLICATE => Ok(Command::Duplicate {
            dx: float(1)?,
            dy: float(2)?,
        }),
        OP_DELETE => Ok(Command::Delete),
        OP_SCALE_XY => Ok(Command::ScaleXY {
            sx: float(1)?,
            sy: float(2)?,
            pivot: (float(3)?, float(4)?),
        }),
        OP_ARRANGE => Ok(Command::Arrange {
            to: ArrangeTo::from_raw(*words.get(1).ok_or(ERR_BAD_ARGUMENT)?)
                .ok_or(ERR_BAD_ARGUMENT)?,
        }),
        OP_ALIGN => Ok(Command::Align {
            frame: AlignFrame::from_raw(*words.get(1).ok_or(ERR_BAD_ARGUMENT)?)
                .ok_or(ERR_BAD_ARGUMENT)?,
            edge: AlignEdge::from_raw(*words.get(2).ok_or(ERR_BAD_ARGUMENT)?)
                .ok_or(ERR_BAD_ARGUMENT)?,
        }),
        OP_GROUP => Ok(Command::Group),
        OP_UNGROUP => Ok(Command::Ungroup),
        OP_MOVE_POINT => Ok(Command::MovePoint {
            id: NodeId::new(index(1)? as u32),
            at: VertexRef::new(index(2)?, index(3)?),
            to: (float(4)?, float(5)?),
        }),
        OP_MOVE_HANDLE => Ok(Command::MoveHandle {
            id: NodeId::new(index(1)? as u32),
            at: HandleRef::new(
                index(2)?,
                index(3)?,
                match *words.get(4).ok_or(ERR_BAD_ARGUMENT)? {
                    0 => Handle::C1,
                    1 => Handle::C2,
                    _ => return Err(ERR_BAD_ARGUMENT),
                },
            ),
            to: (float(5)?, float(6)?),
        }),
        OP_INSERT_POINT => Ok(Command::InsertPoint {
            id: NodeId::new(index(1)? as u32),
            at: SegmentRef::new(index(2)?, index(3)?),
            t: float(4)?,
        }),
        OP_DELETE_POINT => Ok(Command::DeletePoint {
            id: NodeId::new(index(1)? as u32),
            at: VertexRef::new(index(2)?, index(3)?),
        }),
        OP_SET_SEGMENT => Ok(Command::SetSegment {
            id: NodeId::new(index(1)? as u32),
            at: SegmentRef::new(index(2)?, index(3)?),
            to: match *words.get(4).ok_or(ERR_BAD_ARGUMENT)? {
                0 => SegKind::Line,
                1 => SegKind::Cubic,
                _ => return Err(ERR_BAD_ARGUMENT),
            },
        }),
        OP_BOOLEAN => Ok(Command::Boolean {
            op: BooleanOp::from_raw(*words.get(1).ok_or(ERR_BAD_ARGUMENT)?)
                .ok_or(ERR_BAD_ARGUMENT)?,
        }),
        _ => Err(ERR_BAD_ARGUMENT),
    }
}

// ---------------------------------------------------------------------------
// The dispatcher
// ---------------------------------------------------------------------------

/// Editor state plus the per-call bookkeeping the host polls.
#[derive(Clone, Debug)]
pub struct Abi {
    editor: Editor,
    error: u32,
    revision: u32,
    tolerance: f32,
    pending_text: Vec<u8>,
}

impl Default for Abi {
    fn default() -> Self {
        Self::new()
    }
}

impl Abi {
    /// A fresh session with no document.
    #[must_use]
    pub fn new() -> Self {
        Self {
            editor: Editor::new(),
            error: ERR_NONE,
            revision: 0,
            tolerance: DEFAULT_PICK_TOLERANCE,
            pending_text: Vec::new(),
        }
    }

    /// The engine underneath (native tooling and tests use this; the browser
    /// only ever sees the word-level API).
    #[must_use]
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// Mutable engine access for native tooling.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The error code of the most recent call.
    #[must_use]
    pub fn error(&self) -> u32 {
        self.error
    }

    /// The state revision: bumped by every call that changed the document, the
    /// selection or the history. The host rebuilds its canvas when it changes.
    #[must_use]
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// The hit-test tolerance in force, in document units.
    #[must_use]
    pub fn tolerance(&self) -> f32 {
        self.tolerance
    }

    /// Runs one feature against the two tables. Returns the feature's value, or
    /// `0` with [`Abi::error`] set.
    #[must_use]
    pub fn call(&mut self, feature: u32, a: u32, b: u32, input: &[u32], out: &mut [u32]) -> u32 {
        // `ERROR` reports the *previous* call's code, so snapshot before the
        // reset: the host's first move after a `0` result is always `ERROR`.
        let previous = self.error;
        self.error = ERR_NONE;
        self.pending_text.clear();
        if feature > MAX_FEATURE {
            self.error = ERR_BAD_FEATURE;
            return 0;
        }
        let deep_match = match feature {
            feature::VERSION => Some(ABI_VERSION),
            feature::DOC_LOAD => Some(self.load(a, input)),
            feature::SVG_NODES => Some(self.svg_nodes(a, input, out)),
            feature::CLOSE => {
                self.editor.close();
                self.bump();
                Some(0)
            }
            feature::ERROR => Some(previous),
            _ => None,
        };
        let value = match deep_match {
            Some(value) => value,
            None => {
                if !self.editor.has_document() {
                    self.error = ERR_NO_DOCUMENT;
                    return 0;
                }
                match feature {
                    feature::NODE_COUNT => self.node_count(),
                    feature::REVISION => self.revision,
                    feature::NODE_SYNC => self.node_sync(out),
                    feature::NODE_BOUNDS => self.node_bounds(a, out),
                    feature::PATH_FLUSH => self.path_flush(a, out),
                    feature::NODE_AT => self.node_at(a),
                    feature::SEGMENT_COUNT => self.segment_count(),
                    feature::DOC_SIZE => self.doc_size(out),
                    feature::SELECTION_COUNT => self.editor.selection().len() as u32,
                    feature::SELECTION_AT => self
                        .editor
                        .selection()
                        .get(a as usize)
                        .map_or(0, |id| id.get()),
                    feature::SELECTION_IDS => self.selection_ids(out),
                    feature::SELECTION_BOUNDS => self.selection_bounds(out),
                    feature::SELECT_ALL => {
                        self.editor.select_all();
                        self.editor.selection().len() as u32
                    }
                    feature::SELECT_CLEAR => {
                        self.editor.select_clear();
                        0
                    }
                    feature::SELECT_ONLY => self.select_only(a),
                    feature::SELECT_ADD => self.select_add(a),
                    feature::SELECT_TOGGLE => self.select_toggle(a),
                    feature::PICK => self.pick(a, b),
                    feature::MARQUEE => self.marquee(a, input, out),
                    feature::GET_TOLERANCE => self.tolerance.to_bits(),
                    feature::SET_TOLERANCE => self.set_tolerance(a),
                    feature::APPLY_SPEC => self.apply_spec(a, input),
                    feature::IMPORT_SVG => self.import_svg(a, input),
                    feature::PREVIEW => self.preview(a, input),
                    feature::SNAP => self.snap(input, out),
                    feature::UNDO => self.undo(),
                    feature::REDO => self.redo(),
                    feature::CAN_UNDO => u32::from(self.editor.history().can_undo()),
                    feature::CAN_REDO => u32::from(self.editor.history().can_redo()),
                    feature::HISTORY_LEN | feature::HISTORY_CURSOR => {
                        self.editor.history().cursor() as u32
                    }
                    feature::HISTORY_REDO_DEPTH => self.editor.history().redo_depth() as u32,
                    feature::HISTORY_DROPPED => self.editor.history().dropped() as u32,
                    feature::UNDO_LABEL => match self.editor.history().undo_label() {
                        Some(label) => self.write_text(label),
                        None => {
                            self.error = ERR_NO_HISTORY;
                            0
                        }
                    },
                    feature::REDO_LABEL => match self.editor.history().redo_label() {
                        Some(label) => self.write_text(label),
                        None => {
                            self.error = ERR_NO_HISTORY;
                            0
                        }
                    },
                    feature::LAST_LABEL => {
                        let index = self.editor.history().cursor().saturating_sub(1);
                        match self.editor.history().entries().get(index) {
                            Some(entry) => self.write_text(entry.label),
                            None => {
                                self.error = ERR_NO_HISTORY;
                                0
                            }
                        }
                    }
                    _ => {
                        self.error = ERR_BAD_FEATURE;
                        0
                    }
                }
            }
        };
        // Text features stash their bytes; copy them into the output table now
        // so a single call can both answer and deliver its payload.
        if !self.pending_text.is_empty() && self.take_pending_text(out) == 0 {
            return 0;
        }
        value
    }

    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    // -- document ------------------------------------------------------------

    fn load(&mut self, off: u32, input: &[u32]) -> u32 {
        let start = off as usize;
        let Some(blob) = input.get(start..) else {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        };
        match crate::doc_blob::decode_doc(blob) {
            Ok(doc) if doc.node_count() as u32 > MAX_NODES => {
                self.error = ERR_CAPACITY;
                0
            }
            Ok(doc) => {
                let count = doc.node_count() as u32;
                self.editor.load(doc);
                self.bump();
                count
            }
            Err(code) => {
                self.error = code;
                0
            }
        }
    }

    fn node_count(&mut self) -> u32 {
        self.editor.doc().map_or(0, |d| d.node_count() as u32)
    }

    fn node_at(&mut self, index: u32) -> u32 {
        self.editor
            .doc()
            .and_then(|d| d.nodes().get(index as usize))
            .map_or(0, |n| n.id.get())
    }

    fn segment_count(&mut self) -> u32 {
        self.editor.doc().map_or(0, |d| d.segment_count() as u32)
    }

    fn doc_size(&mut self, out: &mut [u32]) -> u32 {
        let Some(doc) = self.editor.doc() else {
            return 0;
        };
        if out.len() < 2 {
            self.error = ERR_CAPACITY;
            return 0;
        }
        out[0] = doc.width().to_bits();
        out[1] = doc.height().to_bits();
        2
    }

    /// Writes every node as a self-describing record (header + path blob) and
    /// returns how many records fit. Records are walked by the host using the
    /// path length stored in each header.
    fn node_sync(&mut self, out: &mut [u32]) -> u32 {
        let Some(doc) = self.editor.doc() else {
            return 0;
        };
        let mut cursor = 0;
        let mut written = 0;
        for node in doc.nodes() {
            let record = match self.editor.preview_transform(node.id) {
                Some(transform) => {
                    let mut previewed = node.clone();
                    previewed.transform = transform;
                    crate::doc_blob::write_node_record(&mut out[cursor..], &previewed)
                }
                None => crate::doc_blob::write_node_record(&mut out[cursor..], node),
            };
            match record {
                Some(words) => {
                    cursor += words;
                    written += 1;
                }
                None => {
                    self.error = ERR_CAPACITY;
                    break;
                }
            }
        }
        self.terminate(out, cursor);
        written
    }

    /// Zeroes the word after a list of ids or records.
    ///
    /// Ids are never 0, so a walking host can stop at the terminator instead of
    /// trusting a count — which matters because the output table keeps its
    /// contents between calls and stale words are otherwise indistinguishable
    /// from real data.
    fn terminate(&self, out: &mut [u32], at: usize) {
        if let Some(slot) = out.get_mut(at) {
            *slot = 0;
        }
    }

    fn node_bounds(&mut self, id: u32, out: &mut [u32]) -> u32 {
        if self
            .editor
            .doc()
            .and_then(|d| d.node(NodeId::new(id)))
            .is_none()
        {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        }
        // As displayed: a live drag preview has to move the box the canvas draws
        // (and the marquee that follows it), not just the node records.
        let Some((lo, hi)) = self.editor.display_bounds(NodeId::new(id)) else {
            return 0;
        };
        self.write_bounds(lo, hi, out)
    }

    fn path_flush(&mut self, id: u32, out: &mut [u32]) -> u32 {
        let Some(node) = self.editor.doc().and_then(|d| d.node(NodeId::new(id))) else {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        };
        // The placed path already has the node transform folded in, so the
        // canvas never has to replay a matrix.
        let blob = crate::doc_blob::encode_path(&node.placed_path());
        if blob.len() > out.len() {
            self.error = ERR_CAPACITY;
            return 0;
        }
        out[..blob.len()].copy_from_slice(&blob);
        blob.len() as u32
    }

    // -- selection -----------------------------------------------------------

    fn select_only(&mut self, id: u32) -> u32 {
        self.editor.select_only(&[NodeId::new(id)]);
        self.editor.selection().len() as u32
    }

    fn select_add(&mut self, id: u32) -> u32 {
        self.editor.select_add(NodeId::new(id));
        self.editor.selection().len() as u32
    }

    fn select_toggle(&mut self, id: u32) -> u32 {
        self.editor.select_toggle(NodeId::new(id));
        self.editor.selection().len() as u32
    }

    fn selection_ids(&mut self, out: &mut [u32]) -> u32 {
        let ids = self.editor.selection();
        if ids.len() > out.len() {
            self.error = ERR_CAPACITY;
            return 0;
        }
        for (slot, id) in out.iter_mut().zip(ids) {
            *slot = id.get();
        }
        let len = ids.len();
        self.terminate(out, len);
        len as u32
    }

    fn selection_bounds(&mut self, out: &mut [u32]) -> u32 {
        let Some((lo, hi)) = self.editor.display_selection_bounds() else {
            return 0;
        };
        self.write_bounds(lo, hi, out)
    }

    fn write_bounds(&mut self, lo: Point, hi: Point, out: &mut [u32]) -> u32 {
        if out.len() < 4 {
            self.error = ERR_CAPACITY;
            return 0;
        }
        out[0] = lo.x.to_bits();
        out[1] = lo.y.to_bits();
        out[2] = hi.x.to_bits();
        out[3] = hi.y.to_bits();
        1
    }

    fn pick(&mut self, x: u32, y: u32) -> u32 {
        let (x, y) = (f32::from_bits(x), f32::from_bits(y));
        if !x.is_finite() || !y.is_finite() {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        }
        self.editor
            .pick(x, y, self.tolerance)
            .map_or(0, |id| id.get())
    }

    fn marquee(&mut self, off: u32, input: &[u32], out: &mut [u32]) -> u32 {
        let args = input.get(off as usize..).and_then(|rest| rest.get(..4));
        let Some(args) = args else {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        };
        let f = |i: usize| f32::from_bits(args[i]);
        let (x0, y0, x1, y1) = (f(0), f(1), f(2), f(3));
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        }
        let Some(doc) = self.editor.doc() else {
            return 0;
        };
        let ids = doc.marquee(x0, y0, x1, y1);
        if ids.len() > out.len() {
            self.error = ERR_CAPACITY;
            return 0;
        }
        for (slot, id) in out.iter_mut().zip(&ids) {
            *slot = id.get();
        }
        let len = ids.len();
        self.terminate(out, len);
        len as u32
    }

    fn set_tolerance(&mut self, bits: u32) -> u32 {
        let value = f32::from_bits(bits);
        if !value.is_finite() || value < 0.0 {
            self.error = ERR_BAD_ARGUMENT;
            return self.tolerance.to_bits();
        }
        self.tolerance = value;
        value.to_bits()
    }

    // -- edits and history ---------------------------------------------------

    fn apply_spec(&mut self, off: u32, input: &[u32]) -> u32 {
        let Some(spec) = input.get(off as usize..) else {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        };
        match decode_command(spec) {
            Ok(command) => match self.editor.apply(&command) {
                Ok(ops) => {
                    self.bump();
                    ops as u32
                }
                Err(err) => {
                    self.error = error_code(err);
                    0
                }
            },
            Err(code) => {
                self.error = code;
                0
            }
        }
    }

    /// Reads the spec both SVG features take: where it is, what to place it
    /// with, and the text itself.
    ///
    /// Returns the first node id, the placement, the default fill and the text.
    fn svg_spec(&mut self, at: u32, input: &[u32]) -> Result<(u32, Affine, [u8; 4], String), ()> {
        let base = at as usize;
        let Some(header) = input.get(base..base + SVG_SPEC_WORDS) else {
            self.error = ERR_BAD_ARGUMENT;
            return Err(());
        };
        let first_id = header[0];
        let mut m = [0.0f32; 6];
        for (i, slot) in m.iter_mut().enumerate() {
            *slot = f32::from_bits(header[1 + i]);
        }
        let fill = unpack_rgba(header[7]);
        let bytes = header[8] as usize;
        let words = bytes.div_ceil(4);
        let Some(text_words) = input.get(base + SVG_SPEC_WORDS..base + SVG_SPEC_WORDS + words)
        else {
            self.error = ERR_BAD_ARGUMENT;
            return Err(());
        };
        // The text is UTF-8 that the host packed four bytes to a word; only the
        // bytes it declared are read, so the padding is never part of the text.
        let mut raw = Vec::with_capacity(words * 4);
        for word in text_words {
            raw.extend_from_slice(&word.to_le_bytes());
        }
        raw.truncate(bytes);
        match core::str::from_utf8(&raw) {
            // Owned: the spec is decoded from a word table, so the text has to
            // outlive the borrow of it.
            Ok(text) => Ok((first_id, Affine::new(m), fill, text.to_owned())),
            Err(_) => {
                self.error = ERR_BAD_ARGUMENT;
                Err(())
            }
        }
    }

    /// Parses an SVG into node records. No document is needed, and none is
    /// touched: this is the read half of the import.
    fn svg_nodes(&mut self, at: u32, input: &[u32], out: &mut [u32]) -> u32 {
        let Ok((first_id, placement, fill, text)) = self.svg_spec(at, input) else {
            return 0;
        };
        if !placement.is_finite() || placement.invert().is_none() {
            self.error = ERR_DEGENERATE;
            return 0;
        }
        let parsed = match svg::parse(&text) {
            Ok(parsed) => parsed,
            Err(_) => {
                self.error = ERR_MALFORMED_SVG;
                return 0;
            }
        };
        let mut cursor = 0;
        let mut written = 0;
        for (offset, shape) in parsed.shapes.iter().enumerate() {
            let transform = shape.transform.then(placement);
            if !transform.is_finite() || transform.invert().is_none() {
                self.error = ERR_DEGENERATE;
                return 0;
            }
            let mut node = Node::new(
                NodeId::new(first_id.saturating_add(offset as u32)),
                shape.path.clone(),
                shape.fill.unwrap_or(fill),
            );
            node.transform = transform;
            match crate::doc_blob::write_node_record(&mut out[cursor..], &node) {
                Some(words) => {
                    cursor += words;
                    written += 1;
                }
                None => {
                    self.error = ERR_CAPACITY;
                    break;
                }
            }
        }
        self.terminate(out, cursor);
        written
    }

    /// Imports an SVG as one node per `<path>`, as one history step.
    fn import_svg(&mut self, at: u32, input: &[u32]) -> u32 {
        let Ok((_, placement, fill, text)) = self.svg_spec(at, input) else {
            return 0;
        };
        let current = self.editor.doc().map_or(0, |doc| doc.node_count()) as u32;
        // The node cap is a host-side promise about the canvas, so it is checked
        // here and *before* anything is applied. The text is parsed once; the
        // engine then takes the shapes rather than re-reading the file.
        let parsed = match svg::parse(&text) {
            Ok(parsed) => parsed,
            Err(_) => {
                self.error = ERR_MALFORMED_SVG;
                return 0;
            }
        };
        if current + parsed.shapes.len() as u32 > MAX_NODES {
            self.error = ERR_CAPACITY;
            return 0;
        }
        match self.editor.add_shapes(&parsed.shapes, placement, fill) {
            Ok(added) => {
                self.bump();
                added.len() as u32
            }
            Err(err) => {
                self.error = error_code(err);
                0
            }
        }
    }

    /// Installs (`a = PREVIEW_SET`) or drops (`a = PREVIEW_CLEAR`) a live
    /// transform preview; the returned value is the number of previewed nodes.
    ///
    /// The preview is what makes a drag feel live: the canvas re-syncs with no
    /// document mutation and no history step, and the eventual commit is an
    /// ordinary `APPLY_SPEC` of the same command.
    fn preview(&mut self, a: u32, input: &[u32]) -> u32 {
        if a == PREVIEW_CLEAR {
            self.editor.preview_clear();
            self.bump();
            return 0;
        }
        let spec = match input.get(a as usize..) {
            Some(spec) => spec,
            None => {
                self.error = ERR_BAD_ARGUMENT;
                return 0;
            }
        };
        let command = match decode_command(spec) {
            Ok(command) => command,
            Err(code) => {
                self.error = code;
                return 0;
            }
        };
        match self.editor.preview(&command) {
            Ok(ops) => {
                self.bump();
                ops as u32
            }
            Err(err) => {
                self.error = error_code(err);
                0
            }
        }
    }

    /// Snaps a proposed move of the selection.
    ///
    /// Input words: dx, dy, tolerance, flags, grid step. Output: the snapped
    /// dx, dy, the guide count, one record per guide
    /// (`axis, kind, position, from, to`), then the zero terminator.
    fn snap(&mut self, input: &[u32], out: &mut [u32]) -> u32 {
        let Some(words) = input.get(..5) else {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        };
        let f = |i: usize| f32::from_bits(words[i]);
        let flags = words[3];
        if flags & !SNAP_FLAGS != 0 {
            self.error = ERR_BAD_ARGUMENT;
            return 0;
        }
        let options = SnapOptions {
            tolerance: f(2),
            grid_step: f(4),
            canvas: flags & SNAP_CANVAS != 0,
            nodes: flags & SNAP_NODES != 0,
            grid: flags & SNAP_GRID != 0,
        };
        match self.editor.snap_move(f(0), f(1), &options) {
            Ok(result) => {
                let guides = result.guides.len().min(MAX_SNAP_GUIDES);
                if out.len() < 3 + guides * GUIDE_WORDS + 1 {
                    self.error = ERR_CAPACITY;
                    return 0;
                }
                out[0] = result.dx.to_bits();
                out[1] = result.dy.to_bits();
                out[2] = guides as u32;
                for (index, guide) in result.guides.iter().take(guides).enumerate() {
                    let at = 3 + index * GUIDE_WORDS;
                    out[at] = guide.axis.raw();
                    out[at + 1] = guide.kind.raw();
                    out[at + 2] = guide.position.to_bits();
                    out[at + 3] = guide.from.to_bits();
                    out[at + 4] = guide.to.to_bits();
                }
                self.terminate(out, 3 + guides * GUIDE_WORDS);
                guides as u32
            }
            Err(err) => {
                self.error = error_code(err);
                0
            }
        }
    }

    fn undo(&mut self) -> u32 {
        match self.editor.undo() {
            Ok(label) => {
                self.bump();
                self.write_text(label)
            }
            Err(err) => {
                self.error = error_code(err);
                0
            }
        }
    }

    fn redo(&mut self) -> u32 {
        match self.editor.redo() {
            Ok(label) => {
                self.bump();
                self.write_text(label)
            }
            Err(err) => {
                self.error = error_code(err);
                0
            }
        }
    }

    /// Stashes a label for the end of the current call and returns its byte
    /// length (the value the text features report).
    fn write_text(&mut self, text: &str) -> u32 {
        self.pending_text.clear();
        self.pending_text.extend_from_slice(text.as_bytes());
        text.len() as u32
    }

    /// The bytes most recently produced by a text feature.
    #[must_use]
    pub fn pending_text(&self) -> &[u8] {
        &self.pending_text
    }

    /// Packs [`Abi::pending_text`] into `out` as little-endian words and returns
    /// the word count (0, with [`ERR_CAPACITY`], when it does not fit).
    pub fn take_pending_text(&mut self, out: &mut [u32]) -> u32 {
        let bytes = core::mem::take(&mut self.pending_text);
        let words = bytes.len().div_ceil(4);
        if words > out.len() {
            self.error = ERR_CAPACITY;
            return 0;
        }
        for (i, chunk) in bytes.chunks(4).enumerate() {
            let mut word = 0u32;
            for (j, byte) in chunk.iter().enumerate() {
                word |= u32::from(*byte) << (8 * j);
            }
            out[i] = word;
        }
        words as u32
    }
}

// ---------------------------------------------------------------------------
// The module-owned tables
// ---------------------------------------------------------------------------

thread_local! {
    static STATE: RefCell<Abi> = RefCell::new(Abi::new());
    static INPUT: RefCell<Box<[u32]>> = RefCell::new(vec![0u32; IN_WORDS].into_boxed_slice());
    static OUTPUT: RefCell<Box<[u32]>> = RefCell::new(vec![0u32; OUT_WORDS].into_boxed_slice());
}

/// Runs `f` with the session and both tables.
///
/// # Panics
///
/// Panics only if a previous call left a borrow alive, which would be a bug in
/// this module — no call re-enters.
pub fn with_tables<R>(f: impl FnOnce(&mut Abi, &[u32], &mut [u32]) -> R) -> R {
    INPUT.with(|input| {
        OUTPUT.with(|output| {
            STATE.with(|state| {
                let input = input.borrow();
                let mut output = output.borrow_mut();
                let mut state = state.borrow_mut();
                f(&mut state, &input, &mut output)
            })
        })
    })
}

/// Address of the input table inside the module's memory (the browser writes
/// through this; native callers use [`put_input`]).
#[must_use]
pub fn input_ptr() -> u32 {
    INPUT.with(|input| input.borrow().as_ptr() as u32)
}

/// Capacity of the input table in words.
#[must_use]
pub fn input_capacity() -> u32 {
    IN_WORDS as u32
}

/// Address of the output table inside the module's memory.
#[must_use]
pub fn output_ptr() -> u32 {
    OUTPUT.with(|output| output.borrow().as_ptr() as u32)
}

/// Capacity of the output table in words.
#[must_use]
pub fn output_capacity() -> u32 {
    OUT_WORDS as u32
}

/// Writes `words` into the input table at word offset `at`.
#[must_use]
pub fn put_input(words: &[u32], at: usize) -> bool {
    INPUT.with(|input| {
        let mut input = input.borrow_mut();
        match at
            .checked_add(words.len())
            .and_then(|end| input.get_mut(at..end))
        {
            Some(slice) => {
                slice.copy_from_slice(words);
                true
            }
            None => false,
        }
    })
}

/// Reads `len` words from the output table.
#[must_use]
pub fn get_output(len: usize) -> Vec<u32> {
    OUTPUT.with(|output| output.borrow()[..len.min(OUT_WORDS)].to_vec())
}

/// Runs one feature against the module's own tables — the single entry point
/// shared by the exported function and the native tests.
#[must_use]
pub fn call(feature: u32, a: u32, b: u32) -> u32 {
    with_tables(|state, input, output| state.call(feature, a, b, input, output))
}

/// The error code of the most recent [`call`].
#[must_use]
pub fn last_error() -> u32 {
    STATE.with(|state| state.borrow().error())
}

/// The current state revision.
#[must_use]
pub fn revision() -> u32 {
    STATE.with(|state| state.borrow().revision())
}
