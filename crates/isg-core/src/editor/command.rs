//! User-level edit commands.
//!
//! A [`Command`] is what the UI produces; it is expressed relative to the
//! editor's **current selection** (drag the selection, scale the selection,
//! recolour the selection). The editor turns it into a concrete, value-carrying
//! operation list ([`super::editor::HistoryEntry`]) so that undoing restores the
//! exact previous state.

use super::affine::Affine;
use super::doc::{GroupId, NodeId};

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
    /// Move a node into a group (or out of one, with `to: None`).
    ///
    /// This is the only op that touches group membership, and it carries the
    /// previous value, so grouping and ungrouping undo exactly like every other
    /// edit — a stored value rather than a recomputed one.
    SetGroup {
        /// Which node.
        id: NodeId,
        /// The group it belongs to afterwards.
        to: Option<GroupId>,
    },
}

/// Where a z-order command puts the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrangeTo {
    /// On top of everything else.
    Front,
    /// Under everything else.
    Back,
}

impl ArrangeTo {
    /// Raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Front => 0,
            Self::Back => 1,
        }
    }

    /// Decodes a wire value.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Front),
            1 => Some(Self::Back),
            _ => None,
        }
    }
}

/// What an alignment lines the selection up against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignFrame {
    /// The selection's own bounding box (aligning several objects to each
    /// other). With a single node selected this is a no-op by construction.
    Selection,
    /// The canvas: its edges and its centre lines.
    Canvas,
}

impl AlignFrame {
    /// Raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Selection => 0,
            Self::Canvas => 1,
        }
    }

    /// Decodes a wire value.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Selection),
            1 => Some(Self::Canvas),
            _ => None,
        }
    }
}

/// Which reference line of the frame an alignment uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignEdge {
    /// Left edges.
    Left,
    /// Horizontal centres.
    HCenter,
    /// Right edges.
    Right,
    /// Top edges.
    Top,
    /// Vertical centres.
    VCenter,
    /// Bottom edges.
    Bottom,
}

impl AlignEdge {
    /// Every edge, in the order the UI lays its buttons out.
    pub const ALL: [Self; 6] = [
        Self::Left,
        Self::HCenter,
        Self::Right,
        Self::Top,
        Self::VCenter,
        Self::Bottom,
    ];

    /// Raw wire value.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::HCenter => 1,
            Self::Right => 2,
            Self::Top => 3,
            Self::VCenter => 4,
            Self::Bottom => 5,
        }
    }

    /// Decodes a wire value.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Left),
            1 => Some(Self::HCenter),
            2 => Some(Self::Right),
            3 => Some(Self::Top),
            4 => Some(Self::VCenter),
            5 => Some(Self::Bottom),
            _ => None,
        }
    }

    /// True when the edge moves the selection along x.
    #[must_use]
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Left | Self::HCenter | Self::Right)
    }
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
    /// Scale the selection uniformly about a document-space pivot.
    Scale {
        /// Uniform factor applied on both axes.
        factor: f32,
        /// Pivot that stays fixed.
        pivot: (f32, f32),
    },
    /// Scale the selection by a factor per axis about a document-space pivot —
    /// what dragging a corner handle produces (4B). The factors apply in
    /// document space, exactly as the drag on screen reads.
    ScaleXY {
        /// Horizontal factor.
        sx: f32,
        /// Vertical factor.
        sy: f32,
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
    /// Put the selection on top of (or under) everything else, keeping the
    /// selection's own relative order.
    Arrange {
        /// Which end of the stack.
        to: ArrangeTo,
    },
    /// Line the selection up against its own box or against the canvas.
    Align {
        /// What to line up against.
        frame: AlignFrame,
        /// Which reference line.
        edge: AlignEdge,
    },
    /// Group the selection: picking one member afterwards selects them all.
    Group,
    /// Dissolve the groups the selection belongs to.
    Ungroup,
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
                | Self::ScaleXY { .. }
                | Self::Rotate { .. }
                | Self::Arrange { .. }
                | Self::Align { .. }
                | Self::Group
                | Self::Ungroup
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
            Self::ScaleXY { .. } => "resize",
            Self::Rotate { .. } => "rotate",
            Self::Arrange { .. } => "arrange",
            Self::Align { .. } => "align",
            Self::Group => "group",
            Self::Ungroup => "ungroup",
            Self::CenterOnCanvas => "centre",
            Self::SetFill { .. } => "fill",
            Self::SetVisible { .. } => "visibility",
            Self::Reorder { .. } => "reorder",
            Self::Duplicate { .. } => "duplicate",
            Self::Delete => "delete",
        }
    }
}
