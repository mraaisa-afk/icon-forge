//! User-level edit commands.
//!
//! A [`Command`] is what the UI produces; it is expressed relative to the
//! editor's **current selection** (drag the selection, scale the selection,
//! recolour the selection). The editor turns it into a concrete, value-carrying
//! operation list ([`super::editor::HistoryEntry`]) so that undoing restores the
//! exact previous state.

use super::affine::Affine;
use super::doc::NodeId;

/// Why a command could not be applied. A rejected command must leave the
/// document **and** the history untouched (the editor guarantees this by
/// validating before it records anything).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The editor has no editable document loaded.
    NoDocument,
    /// The selection is empty (nothing to transform or recolour).
    EmptySelection,
    /// One or more selected nodes no longer exist.
    MissingNode(NodeId),
    /// The transform is degenerate or non-finite (e.g. a zero scale factor).
    DegenerateTransform,
    /// The colour has no visible contribution (`alpha == 0`).
    Transparent,
    /// The target index is outside the document.
    IndexOutOfRange,
    /// A history cursor operation was impossible (nothing to undo/redo, or the
    /// transaction depth is wrong).
    BadHistory,
    /// An arrow-key nudge of `0` would otherwise record a no-op history step.
    ZeroDelta,
    /// The command was valid but nothing would change (e.g. reordering a node
    /// that is already on top, or re-applying the current fill). No history step
    /// is recorded; the UI reports it silently.
    NoOp,
}

impl CommandError {
    /// Stable machine-readable code, carried across the WASM wire so the UI can
    /// branch on it without string matching.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoDocument => "no_document",
            Self::EmptySelection => "empty_selection",
            Self::MissingNode(_) => "missing_node",
            Self::DegenerateTransform => "degenerate_transform",
            Self::Transparent => "transparent",
            Self::IndexOutOfRange => "index_out_of_range",
            Self::BadHistory => "bad_history",
            Self::ZeroDelta => "zero_delta",
            Self::NoOp => "no_op",
        }
    }
}

/// A local, invertible node mutation — the unit the history stores.
///
/// The editor applies exactly these; undo applies the stored inverse value
/// instead of recomputing an arithmetic reverse (which is not exact in `f32`).
#[derive(Clone, Debug, PartialEq)]
pub enum NodeOp {
    /// Replace the node's local→document transform.
    SetTransform {
        /// Which node.
        id: NodeId,
        /// New transform.
        to: Affine,
    },
    /// Replace the node's fill colour.
    SetFill {
        /// Which node.
        id: NodeId,
        /// New RGBA fill.
        to: [u8; 4],
    },
    /// Replace the node's visibility.
    SetVisible {
        /// Which node.
        id: NodeId,
        /// New visibility.
        to: bool,
    },
    /// Insert a node at a z index (the exact [`super::doc::Node`] value, so a
    /// restored node keeps its id and its geometry bits).
    Insert {
        /// Target z index.
        index: usize,
        /// The node to insert.
        node: super::doc::Node,
    },
    /// Remove the node at a z index.
    Remove {
        /// Source z index.
        index: usize,
    },
}

/// A user-level editing command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Move the selection by a delta in document units.
    Translate {
        /// Horizontal delta.
        dx: f32,
        /// Vertical delta.
        dy: f32,
    },
    /// Scale the selection about a document-space pivot.
    Scale {
        /// Uniform factor applied on both axes (Phase 4A exposes uniform scale;
        /// non-uniform scaling is a 4B handle edit).
        factor: f32,
        /// Pivot that stays fixed.
        pivot: (f32, f32),
    },
    /// Rotate the selection about a document-space pivot (degrees, clockwise in
    /// the y-down document space).
    Rotate {
        /// Angle in degrees.
        degrees: f32,
        /// Pivot that stays fixed.
        pivot: (f32, f32),
    },
    /// Snap the selection's bounds back to the canvas centre (a UI convenience
    /// that exercises the same transform path as a drag).
    CenterOnCanvas,
    /// Replace the selection's fill colour.
    SetFill {
        /// New RGBA fill.
        to: [u8; 4],
    },
    /// Show or hide the selection. Hidden nodes stay in the document, so this is
    /// exactly invertible.
    SetVisible {
        /// New visibility.
        to: bool,
    },
    /// Move the selection one z step (clamped at the ends; a move that would not
    /// change the order is rejected rather than recorded as a no-op).
    Reorder {
        /// `true` moves each node one step up (later in the draw order).
        up: bool,
    },
    /// Copy the selection on top of itself with a small offset, selecting the
    /// copies (the usual editing idiom).
    Duplicate {
        /// Offset applied to each copy.
        dx: f32,
        /// Offset applied to each copy.
        dy: f32,
    },
    /// Delete the selection.
    Delete,
}

impl Command {
    /// True when the command shows the selection in the UI, i.e. it needs a
    /// non-empty selection to be meaningful.
    #[must_use]
    pub const fn needs_selection(&self) -> bool {
        matches!(
            self,
            Self::Translate { .. }
                | Self::Scale { .. }
                | Self::Rotate { .. }
                | Self::CenterOnCanvas
                | Self::SetFill { .. }
                | Self::SetVisible { .. }
                | Self::Reorder { .. }
                | Self::Duplicate { .. }
                | Self::Delete
        )
    }

    /// Short label used in the editor's status line and the history tooltip.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Translate { .. } => "move",
            Self::Scale { .. } => "scale",
            Self::Rotate { .. } => "rotate",
            Self::CenterOnCanvas => "centre",
            Self::SetFill { .. } => "fill",
            Self::SetVisible { .. } => "visibility",
            Self::Reorder { .. } => "reorder",
            Self::Duplicate { .. } => "duplicate",
            Self::Delete => "delete",
        }
    }
}
