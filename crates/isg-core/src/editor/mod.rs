//! Interactive editor engine — the document model, the edit commands and the
//! exact undo/redo history behind the Phase 4 canvas editor (ARCHITECTURE.md §8).
//!
//! ## Why it lives in `isg-core`
//!
//! §1.2's keystone decision compiles this crate **twice**: natively (batch work)
//! and for `wasm32-unknown-unknown` (interactive geometry). §2's golden rule —
//! *interactive, per-frame work never crosses the IPC boundary* — is what puts
//! the editor engine here rather than in the Tauri shell: hit-testing, selection
//! and drag commits run in the webview.
//!
//! The module is dependency-free and platform-free like the rest of the crate
//! (no `std::fs`, no threads, no `unsafe`), so the WASM keystone check covers it
//! automatically.
//!
//! ## Model
//!
//! * [`Doc`] — a flat, z-ordered list of [`Node`]s. Each node owns a path in
//!   **local** coordinates plus an [`Affine`] local→document transform, a fill
//!   colour and a visibility flag. Flat nodes with an explicit z-order keep the
//!   command set small in 4A; grouping arrives in 4B.
//! * [`Command`] — the user-level edits the UI produces (`Translate`, `Scale`,
//!   `Reorder`, `Duplicate`, …), expressed *relative to the current selection*.
//! * [`Editor`] — document + selection + [`History`]. Every edit is recorded as
//!   a pair of **value-carrying** operation lists, so `undo` restores the exact
//!   previous bits: dragging a node and undoing it reproduces the original
//!   transform bit-for-bit. That is what makes the Phase 4 exit criterion
//!   (`undo(do(x)) == x` over 10k random sequences) provable rather than
//!   approximate — inverse *arithmetic* on `f32` is not exact, stored values are.
//!
//! Selection is view state: `Select*` commands change it without touching the
//! history, so undoing a drag never rewinds the user's selection clicks. Each
//! history entry does carry the selection on both sides, so undo/redo restore it
//! along with the document — the invariant the property test checks is editor
//! state equality, not just document equality.

pub mod affine;
pub mod command;
pub mod doc;
pub mod geom;
pub mod snap;

pub use affine::Affine;
pub use command::{AlignEdge, AlignFrame, ArrangeTo, Command, CommandError, NodeOp};
pub use doc::{Doc, GroupId, Node, NodeId, DEFAULT_PICK_TOLERANCE};
pub use geom::{Point, Seg, Subpath};
pub use snap::{Axis, Guide, GuideKind, SnapOptions, SnapResult};

/// Wire/behaviour version of the editor engine.
///
/// Bumped whenever the document model, the command set or the WASM byte
/// protocol changes in a way the TypeScript side must notice (it is part of the
/// `state` response and of the editor's status line). 4B added groups (a group
/// word in every node record), the arrange/align commands, non-uniform scale
/// and the move-preview/snap features.
pub const EDITOR_VERSION: u32 = 2;

/// Maximum number of undo steps kept per editor (older entries are dropped).
///
/// A step holds only the *touched* nodes, so this is cheap for the normal case
/// (drag, recolour, reorder) and bounded for the pathological one (a 5000-node
/// document, 256 deletes in a row).
pub const MAX_HISTORY: usize = 256;

// ---------------------------------------------------------------------------
// Editor state: the document slot, the selection and the undo history.
//
// `Editor` and `History` live in this facade module rather than in a submodule
// because they are the primary types of `isg_core::editor`.
// ---------------------------------------------------------------------------

/// One undo step: the operations that were applied, plus the inverse
/// operations that restore the previous state, plus the selection on both
/// sides.
///
/// Storing both directions (rather than recomputing the inverse) is what makes
/// the Phase 4 exit criterion exact: `undo(do(x)) == x` compares editor state,
/// including the `f32` bits of every transform.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// Human label of the command that produced the entry.
    pub label: &'static str,
    /// Operations applied, in order.
    pub forward: Vec<NodeOp>,
    /// Operations that undo [`HistoryEntry::forward`], in reverse order.
    pub inverse: Vec<NodeOp>,
    /// Selection before the command.
    pub selection_before: Vec<NodeId>,
    /// Selection after the command.
    pub selection_after: Vec<NodeId>,
}

impl HistoryEntry {
    /// Number of node operations in the step.
    #[must_use]
    pub fn len(&self) -> usize {
        self.forward.len()
    }

    /// True when the step touches no node (never recorded).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }
}

/// A bounded undo/redo stack with a cursor.
///
/// Entries before `cursor` are undoable, entries from `cursor` on are redoable.
/// Pushing a new entry drops the redo tail and, when the stack exceeds
/// [`MAX_HISTORY`], the oldest entries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    entries: Vec<HistoryEntry>,
    cursor: usize,
    dropped: u64,
}

impl History {
    /// An empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Undoable entries, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[HistoryEntry] {
        // Only the undoable prefix is meaningful to callers.
        &self.entries[..self.cursor]
    }

    /// The undo cursor: the number of undoable steps.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Redoable steps remaining.
    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.entries.len() - self.cursor
    }

    /// True when there is nothing to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    /// True when there is something to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    /// Steps dropped by the [`MAX_HISTORY`] cap (reported in the status line so
    /// a clipped history is visible, never silent).
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The label of the step an undo would revert.
    #[must_use]
    pub fn undo_label(&self) -> Option<&'static str> {
        self.can_undo().then(|| self.entries[self.cursor - 1].label)
    }

    /// The label of the step a redo would re-apply.
    #[must_use]
    pub fn redo_label(&self) -> Option<&'static str> {
        self.can_redo().then(|| self.entries[self.cursor].label)
    }

    /// Records a new step, discarding the redo tail.
    pub fn push(&mut self, entry: HistoryEntry) {
        self.entries.truncate(self.cursor);
        self.entries.push(entry);
        self.cursor = self.entries.len();
        while self.entries.len() > MAX_HISTORY {
            self.entries.remove(0);
            self.cursor -= 1;
            self.dropped += 1;
        }
    }

    /// Applies the entry at the cursor to `doc` and moves the cursor.
    ///
    /// `forward` selects which direction to run: `true` = redo, `false` = undo.
    fn step(&mut self, doc: &mut Doc, forward: bool) -> Result<Vec<NodeId>, CommandError> {
        if forward {
            if !self.can_redo() {
                return Err(CommandError::BadHistory);
            }
            let entry = self.entries[self.cursor].clone();
            apply_ops(doc, &entry.forward)?;
            self.cursor += 1;
            Ok(entry.selection_after.clone())
        } else {
            if !self.can_undo() {
                return Err(CommandError::BadHistory);
            }
            let entry = self.entries[self.cursor - 1].clone();
            apply_ops(doc, &entry.inverse)?;
            self.cursor -= 1;
            Ok(entry.selection_before.clone())
        }
    }
}

/// Applies a list of ops to the document.
fn apply_ops(doc: &mut Doc, ops: &[NodeOp]) -> Result<(), CommandError> {
    for op in ops {
        match op {
            NodeOp::SetTransform { id, to } => {
                let index = doc.index_of(*id).ok_or(CommandError::MissingNode(*id))?;
                let mut node = doc.node(*id).expect("index checked").clone();
                node.transform = *to;
                doc.set_at(index, node);
            }
            NodeOp::SetFill { id, to } => {
                let index = doc.index_of(*id).ok_or(CommandError::MissingNode(*id))?;
                let mut node = doc.node(*id).expect("index checked").clone();
                node.fill = *to;
                doc.set_at(index, node);
            }
            NodeOp::SetVisible { id, to } => {
                let index = doc.index_of(*id).ok_or(CommandError::MissingNode(*id))?;
                let mut node = doc.node(*id).expect("index checked").clone();
                node.visible = *to;
                doc.set_at(index, node);
            }
            NodeOp::Insert { index, node } => {
                for group in node.group.iter() {
                    doc.note_group(*group);
                }
                doc.insert_at(*index, node.clone());
            }
            NodeOp::Remove { index } => {
                if *index >= doc.node_count() {
                    return Err(CommandError::IndexOutOfRange);
                }
                doc.remove_at(*index);
            }
            NodeOp::SetGroup { id, to } => {
                let index = doc.index_of(*id).ok_or(CommandError::MissingNode(*id))?;
                let mut node = doc.node(*id).expect("index checked").clone();
                node.group = *to;
                if let Some(group) = to {
                    doc.note_group(*group);
                }
                doc.set_at(index, node);
            }
        }
    }
    Ok(())
}

/// Interactive editor state: the document, the selection and the history.
///
/// A selection is view state: [`Editor::select_only`] and friends change it
/// without touching the history, so undoing a drag never rewinds the user's
/// clicks. Each history step does carry the selection on both sides, so undo
/// and redo restore it together with the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    doc: Option<Doc>,
    selection: Vec<NodeId>,
    history: History,
    /// Live transform overrides for a drag in progress (see [`Editor::preview`]).
    preview: Option<Vec<(NodeId, Affine)>>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// An editor with no document loaded.
    #[must_use]
    pub fn new() -> Self {
        Self {
            doc: None,
            selection: Vec::new(),
            history: History::new(),
            preview: None,
        }
    }

    /// True when a document is loaded.
    #[must_use]
    pub fn has_document(&self) -> bool {
        self.doc.is_some()
    }

    /// The document, if any.
    #[must_use]
    pub fn doc(&self) -> Option<&Doc> {
        self.doc.as_ref()
    }

    /// Mutable access for value-preserving bulk work (a fresh trace, a test
    /// fixture). Interactive edits go through [`Editor::apply`].
    pub fn doc_mut(&mut self) -> Option<&mut Doc> {
        self.doc.as_mut()
    }

    /// The selection, bottom-first.
    #[must_use]
    pub fn selection(&self) -> &[NodeId] {
        &self.selection
    }

    /// The history.
    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Installs a document, clearing the selection and the history (loading a
    /// sheet is not an undoable edit — it *is* the new baseline).
    pub fn load(&mut self, doc: Doc) {
        self.doc = Some(doc);
        self.selection.clear();
        self.history = History::new();
        self.preview = None;
    }

    /// Drops the document and everything derived from it.
    pub fn close(&mut self) {
        self.doc = None;
        self.selection.clear();
        self.history = History::new();
        self.preview = None;
    }

    /// The ids a click on `id` selects (its whole group, when it has one).
    #[must_use]
    pub fn selection_target(&self, id: NodeId) -> Vec<NodeId> {
        self.doc
            .as_ref()
            .map_or_else(|| vec![id], |doc| doc.selection_target(id))
    }

    /// Replaces the selection.
    pub fn select_only(&mut self, ids: &[NodeId]) {
        let mut expanded = Vec::with_capacity(ids.len());
        for id in ids {
            for member in self.selection_target(*id) {
                if !expanded.contains(&member) {
                    expanded.push(member);
                }
            }
        }
        self.selection = expanded;
        self.normalize_selection();
    }

    /// Adds a node to the selection (no-op when already selected).
    pub fn select_add(&mut self, id: NodeId) {
        for member in self.selection_target(id) {
            if !self.selection.contains(&member) {
                self.selection.push(member);
            }
        }
        self.normalize_selection();
    }

    /// Removes a node from the selection (its whole group goes with it).
    pub fn select_remove(&mut self, id: NodeId) {
        let drop = self.selection_target(id);
        self.selection.retain(|s| !drop.contains(s));
    }

    /// Clears the selection.
    pub fn select_clear(&mut self) {
        self.selection.clear();
    }

    /// Toggles a node in the selection — or, for a grouped node, its group.
    pub fn select_toggle(&mut self, id: NodeId) {
        if self.selection.contains(&id) {
            self.select_remove(id);
        } else {
            self.select_add(id);
        }
    }

    /// Selects everything.
    pub fn select_all(&mut self) {
        self.selection = self.doc.as_ref().map_or(Vec::new(), Doc::ids);
    }

    /// Drops ids that are not (or no longer) visible in the document and sorts
    /// the rest by z order, so the selection is always in a canonical order.
    fn normalize_selection(&mut self) {
        let Some(doc) = &self.doc else {
            self.selection.clear();
            return;
        };
        let mut keep: Vec<NodeId> = doc
            .ids()
            .into_iter()
            .filter(|id| self.selection.contains(id))
            .collect();
        keep.dedup();
        self.selection = keep;
    }

    /// The topmost node under a document-space point.
    #[must_use]
    pub fn pick(&self, x: f32, y: f32, tolerance: f32) -> Option<NodeId> {
        self.doc.as_ref()?.hit_test(x, y, tolerance)
    }

    /// The topmost node under a point, using [`DEFAULT_PICK_TOLERANCE`].
    #[must_use]
    pub fn pick_default(&self, x: f32, y: f32) -> Option<NodeId> {
        self.pick(x, y, DEFAULT_PICK_TOLERANCE)
    }

    /// Selection bounds in document space.
    #[must_use]
    pub fn selection_bounds(&self) -> Option<(Point, Point)> {
        let doc = self.doc.as_ref()?;
        if self.selection.is_empty() {
            return None;
        }
        let ids = self.selection.clone();
        doc.bounds(&ids)
    }

    /// A node's transform as displayed, with any live preview applied.
    #[must_use]
    pub fn display_transform(&self, id: NodeId) -> Option<Affine> {
        if let Some(preview) = self.preview_transform(id) {
            return Some(preview);
        }
        self.doc.as_ref()?.node(id).map(|n| n.transform)
    }

    /// A node's bounding box as displayed (preview included).
    #[must_use]
    pub fn display_bounds(&self, id: NodeId) -> Option<(Point, Point)> {
        let node = self.doc.as_ref()?.node(id)?;
        node.bounds_with(self.display_transform(id)?)
    }

    /// A node's placed path as displayed (preview included).
    #[must_use]
    pub fn display_path(&self, id: NodeId) -> Option<Vec<Subpath>> {
        let node = self.doc.as_ref()?.node(id)?;
        let transform = self.display_transform(id)?;
        Some(node.path.iter().map(|s| s.transformed(transform)).collect())
    }

    /// Selection bounds as displayed (preview included).
    #[must_use]
    pub fn display_selection_bounds(&self) -> Option<(Point, Point)> {
        self.doc.as_ref()?;
        let mut min = Point::new(f32::INFINITY, f32::INFINITY);
        let mut max = Point::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut any = false;
        for id in &self.selection {
            let Some((lo, hi)) = self.display_bounds(*id) else {
                continue;
            };
            min.x = min.x.min(lo.x);
            min.y = min.y.min(lo.y);
            max.x = max.x.max(hi.x);
            max.y = max.y.max(hi.y);
            any = true;
        }
        any.then_some((min, max))
    }

    /// Shows what a command *would* do without recording it — the live half of
    /// a drag.
    ///
    /// Only transforms are previewed: the canvas needs those on every pointer
    /// move, while a fill, a visibility or a z-order change has nothing to
    /// interpolate. The history is untouched, [`Editor::preview_clear`] restores
    /// exactly what was on screen before the drag began, and a commit is an
    /// ordinary [`Editor::apply`] of the *same* command — which is what keeps
    /// one drag equal to one undo step.
    pub fn preview(&mut self, command: &Command) -> Result<usize, CommandError> {
        let entry = self.build_entry(command)?;
        if entry.forward.is_empty() {
            return Err(CommandError::NoOp);
        }
        let overrides: Vec<(NodeId, Affine)> = entry
            .forward
            .iter()
            .filter_map(|op| match op {
                NodeOp::SetTransform { id, to } => Some((*id, *to)),
                _ => None,
            })
            .collect();
        self.preview = (!overrides.is_empty()).then_some(overrides);
        Ok(entry.forward.len())
    }

    /// Drops any live preview.
    pub fn preview_clear(&mut self) {
        self.preview = None;
    }

    /// True while a preview is live.
    #[must_use]
    pub fn preview_active(&self) -> bool {
        self.preview.is_some()
    }

    /// The previewed transform of one node, if it has one.
    #[must_use]
    pub fn preview_transform(&self, id: NodeId) -> Option<Affine> {
        self.preview
            .as_ref()?
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, transform)| *transform)
    }

    /// Snaps a proposed move of the current selection.
    ///
    /// The delta passed in is the *proposed* one and the base is the stored
    /// (un-previewed) geometry, so the UI can ask for the snapped delta first
    /// and then preview or apply exactly that — the two can never drift.
    pub fn snap_move(
        &self,
        dx: f32,
        dy: f32,
        options: &SnapOptions,
    ) -> Result<SnapResult, CommandError> {
        let doc = self.doc.as_ref().ok_or(CommandError::NoDocument)?;
        if self.selection.is_empty() {
            return Err(CommandError::EmptySelection);
        }
        if !dx.is_finite()
            || !dy.is_finite()
            || !options.tolerance.is_finite()
            || options.tolerance < 0.0
        {
            return Err(CommandError::DegenerateTransform);
        }
        Ok(
            snap::snap_move(doc, &self.selection, dx, dy, options).unwrap_or(SnapResult {
                dx,
                dy,
                guides: Vec::new(),
            }),
        )
    }

    /// Applies a command, recording an exact inverse.
    ///
    /// Validates first: a rejected command leaves the document and the history
    /// untouched and returns the reason. A successful edit also drops any live
    /// preview, so a preview can never outlive the state it was drawn against.
    pub fn apply(&mut self, command: &Command) -> Result<usize, CommandError> {
        let entry = self.build_entry(command)?;
        if entry.forward.is_empty() {
            return Err(CommandError::NoOp);
        }
        let doc = self.doc.as_mut().ok_or(CommandError::NoDocument)?;
        apply_ops(doc, &entry.forward)?;
        self.preview = None;
        self.selection = entry.selection_after.clone();
        // Canonicalise immediately: `undo`/`redo` restore the recorded
        // selections through `normalize_selection`, so an edit must leave the
        // selection in exactly that form too — otherwise "apply an edit" and
        // "redo the same edit" would expose the same nodes in a different order
        // (the Phase 4 exit criterion compares editor state, not just the node
        // list).
        self.normalize_selection();
        self.history.push(entry.clone());
        Ok(entry.forward.len())
    }

    /// Undoes one step.
    pub fn undo(&mut self) -> Result<&'static str, CommandError> {
        let doc = self.doc.as_mut().ok_or(CommandError::NoDocument)?;
        self.preview = None;
        let selection = self.history.step(doc, false)?;
        let label = self.history.redo_label().unwrap_or("edit");
        self.selection = selection;
        self.normalize_selection();
        Ok(label)
    }

    /// Redoes one step.
    pub fn redo(&mut self) -> Result<&'static str, CommandError> {
        let doc = self.doc.as_mut().ok_or(CommandError::NoDocument)?;
        self.preview = None;
        let cursor = self.history.cursor();
        let selection = self.history.step(doc, true)?;
        let label = self
            .history
            .entries()
            .get(cursor)
            .map_or("edit", |e| e.label);
        self.selection = selection;
        self.normalize_selection();
        Ok(label)
    }

    /// Builds the forward/inverse op lists for a command without applying it.
    fn build_entry(&self, command: &Command) -> Result<HistoryEntry, CommandError> {
        let doc = self.doc.as_ref().ok_or(CommandError::NoDocument)?;
        if command.needs_selection() && self.selection.is_empty() {
            return Err(CommandError::EmptySelection);
        }
        let selection_before = self.selection.clone();
        let mut forward = Vec::new();
        // Pointwise inverses in forward order; reversed into `inverse` at the
        // end. Reversing is what makes index-based ops exact: each inverse is
        // applied to exactly the state its forward op produced.
        let mut raw_inverse = Vec::new();
        let mut selection_after = selection_before.clone();

        // Resolve the selection once, in z order, so a delete/duplicate can
        // build index-correct ops for every affected node.
        let mut targets: Vec<(usize, Node)> = Vec::with_capacity(self.selection.len());
        for id in &self.selection {
            let index = doc.index_of(*id).ok_or(CommandError::MissingNode(*id))?;
            targets.push((index, doc.node(*id).expect("index checked").clone()));
        }

        match command {
            Command::Translate { dx, dy } => {
                if *dx == 0.0 && *dy == 0.0 {
                    return Err(CommandError::ZeroDelta);
                }
                if !dx.is_finite() || !dy.is_finite() {
                    return Err(CommandError::DegenerateTransform);
                }
                for (_, node) in &targets {
                    let to = node.transform.then(Affine::translate(*dx, *dy));
                    push_transform(&mut forward, &mut raw_inverse, node, to)?;
                }
            }
            Command::Scale { factor, pivot } => {
                if !factor.is_finite() || *factor <= 0.0 {
                    return Err(CommandError::DegenerateTransform);
                }
                let (px, py) = *pivot;
                if !px.is_finite() || !py.is_finite() {
                    return Err(CommandError::DegenerateTransform);
                }
                let about = Affine::translate(-px, -py)
                    .then(Affine::scale(*factor, *factor))
                    .then(Affine::translate(px, py));
                for (_, node) in &targets {
                    let to = node.transform.then(about);
                    push_transform(&mut forward, &mut raw_inverse, node, to)?;
                }
            }
            Command::Rotate { degrees, pivot } => {
                if !degrees.is_finite() {
                    return Err(CommandError::DegenerateTransform);
                }
                let (px, py) = *pivot;
                if !px.is_finite() || !py.is_finite() {
                    return Err(CommandError::DegenerateTransform);
                }
                let about = Affine::translate(-px, -py)
                    .then(Affine::rotate(*degrees))
                    .then(Affine::translate(px, py));
                for (_, node) in &targets {
                    let to = node.transform.then(about);
                    push_transform(&mut forward, &mut raw_inverse, node, to)?;
                }
            }
            Command::ScaleXY { sx, sy, pivot } => {
                if !sx.is_finite() || !sy.is_finite() || *sx <= 0.0 || *sy <= 0.0 {
                    return Err(CommandError::DegenerateTransform);
                }
                let (px, py) = *pivot;
                if !px.is_finite() || !py.is_finite() {
                    return Err(CommandError::DegenerateTransform);
                }
                let about = Affine::translate(-px, -py)
                    .then(Affine::scale(*sx, *sy))
                    .then(Affine::translate(px, py));
                for (_, node) in &targets {
                    let to = node.transform.then(about);
                    push_transform(&mut forward, &mut raw_inverse, node, to)?;
                }
            }
            Command::Align { frame, edge } => {
                let reference = match frame {
                    AlignFrame::Selection => doc.bounds(&selection_before),
                    AlignFrame::Canvas => {
                        Some((Point::new(0.0, 0.0), Point::new(doc.width(), doc.height())))
                    }
                };
                let Some((reference_lo, reference_hi)) = reference else {
                    return Err(CommandError::EmptySelection);
                };
                for (_, node) in &targets {
                    let Some((lo, hi)) = node.bounds() else {
                        continue;
                    };
                    let (dx, dy) = match edge {
                        AlignEdge::Left => (reference_lo.x - lo.x, 0.0),
                        AlignEdge::HCenter => (
                            (reference_lo.x + reference_hi.x) / 2.0 - (lo.x + hi.x) / 2.0,
                            0.0,
                        ),
                        AlignEdge::Right => (reference_hi.x - hi.x, 0.0),
                        AlignEdge::Top => (0.0, reference_lo.y - lo.y),
                        AlignEdge::VCenter => (
                            0.0,
                            (reference_lo.y + reference_hi.y) / 2.0 - (lo.y + hi.y) / 2.0,
                        ),
                        AlignEdge::Bottom => (0.0, reference_hi.y - hi.y),
                    };
                    if dx == 0.0 && dy == 0.0 {
                        continue;
                    }
                    let to = node.transform.then(Affine::translate(dx, dy));
                    push_transform(&mut forward, &mut raw_inverse, node, to)?;
                }
            }
            Command::Arrange { to } => {
                // Simulated so every index is exact: `front` walks the selection
                // top-down (each node lands just above the block already moved)
                // and `back` bottom-up, which keeps the selection's own order.
                let mut order: Vec<NodeId> = selection_before.clone();
                if *to == ArrangeTo::Front {
                    order.reverse();
                }
                let count = doc.node_count();
                let mut current: Vec<NodeId> = doc.ids();
                for (placed, id) in order.iter().enumerate() {
                    let Some(from) = current.iter().position(|candidate| candidate == id) else {
                        continue;
                    };
                    let at = match to {
                        ArrangeTo::Front => (count - 1).saturating_sub(placed),
                        ArrangeTo::Back => placed,
                    }
                    .min(count - 1);
                    if from == at {
                        continue;
                    }
                    current.remove(from);
                    current.insert(at, *id);
                    let node = doc.node(*id).expect("id came from the selection").clone();
                    forward.push(NodeOp::Remove { index: from });
                    forward.push(NodeOp::Insert {
                        index: at,
                        node: node.clone(),
                    });
                    raw_inverse.push(NodeOp::Insert { index: from, node });
                    raw_inverse.push(NodeOp::Remove { index: at });
                }
            }
            Command::Group => {
                if targets.len() < 2 {
                    return Err(CommandError::NoOp);
                }
                let mut existing: Vec<GroupId> =
                    targets.iter().filter_map(|(_, node)| node.group).collect();
                existing.sort_unstable();
                existing.dedup();
                if existing.len() == 1
                    && targets
                        .iter()
                        .all(|(_, node)| node.group == existing.first().copied())
                {
                    // Every target is already in the same group: nothing to do.
                    return Err(CommandError::NoOp);
                }
                let group = GroupId::new(doc.next_group());
                for (_, node) in &targets {
                    if node.group == Some(group) {
                        continue;
                    }
                    forward.push(NodeOp::SetGroup {
                        id: node.id,
                        to: Some(group),
                    });
                    raw_inverse.push(NodeOp::SetGroup {
                        id: node.id,
                        to: node.group,
                    });
                }
            }
            Command::Ungroup => {
                for (_, node) in &targets {
                    let Some(group) = node.group else {
                        continue;
                    };
                    forward.push(NodeOp::SetGroup {
                        id: node.id,
                        to: None,
                    });
                    raw_inverse.push(NodeOp::SetGroup {
                        id: node.id,
                        to: Some(group),
                    });
                }
            }
            Command::CenterOnCanvas => {
                let (lo, hi) = doc
                    .bounds(&selection_before)
                    .ok_or(CommandError::EmptySelection)?;
                let dx = doc.width() / 2.0 - (lo.x + hi.x) / 2.0;
                let dy = doc.height() / 2.0 - (lo.y + hi.y) / 2.0;
                for (_, node) in &targets {
                    let to = node.transform.then(Affine::translate(dx, dy));
                    push_transform(&mut forward, &mut raw_inverse, node, to)?;
                }
            }
            Command::SetFill { to } => {
                if to[3] == 0 {
                    return Err(CommandError::Transparent);
                }
                for (_, node) in &targets {
                    let before = node.fill;
                    // Only colour changes are recorded; a fill that already
                    // matches is not a history step (the `empty` check below
                    // rejects an all-no-op command).
                    if before != *to {
                        forward.push(NodeOp::SetFill {
                            id: node.id,
                            to: *to,
                        });
                        raw_inverse.push(NodeOp::SetFill {
                            id: node.id,
                            to: before,
                        });
                    }
                }
            }
            Command::SetVisible { to } => {
                for (_, node) in &targets {
                    if node.visible != *to {
                        forward.push(NodeOp::SetVisible {
                            id: node.id,
                            to: *to,
                        });
                        raw_inverse.push(NodeOp::SetVisible {
                            id: node.id,
                            to: node.visible,
                        });
                        if !*to {
                            selection_after.retain(|s| *s != node.id);
                        }
                    }
                }
            }
            Command::Reorder { up } => {
                // Reorder from the top down when moving up (and bottom up when
                // moving down) so a multi-node selection keeps its ordering and
                // never collides with itself as indices shift.
                let mut order: Vec<usize> = (0..targets.len()).collect();
                if *up {
                    order.reverse();
                }
                for i in order {
                    let (index, node) = &targets[i];
                    let to = if *up {
                        (*index + 1).min(doc.node_count() - 1)
                    } else {
                        index.saturating_sub(1)
                    };
                    if to == *index {
                        continue;
                    }
                    forward.push(NodeOp::Remove { index: *index });
                    forward.push(NodeOp::Insert {
                        index: to,
                        node: node.clone(),
                    });
                    raw_inverse.push(NodeOp::Insert {
                        index: *index,
                        node: node.clone(),
                    });
                    raw_inverse.push(NodeOp::Remove { index: to });
                }
            }
            Command::Duplicate { dx, dy } => {
                if !dx.is_finite() || !dy.is_finite() {
                    return Err(CommandError::DegenerateTransform);
                }
                selection_after.clear();
                for (k, (_, node)) in targets.iter().enumerate() {
                    // The id is minted here (and stored in the op), so a redo
                    // re-inserts the same id and the same geometry bits.
                    let mut copy = node.clone();
                    copy.id = NodeId::new(doc.next_id() + k as u32);
                    copy.transform = node.transform.then(Affine::translate(*dx, *dy));
                    let index = doc.node_count() + k;
                    selection_after.push(copy.id);
                    forward.push(NodeOp::Insert { index, node: copy });
                    raw_inverse.push(NodeOp::Remove { index });
                }
            }
            Command::Delete => {
                // Forward removes from the top down; each op's inverse is
                // pushed alongside it, so the final reversal re-inserts from
                // the bottom up — every index is exact and never merely clamped
                // (a clamped insert would silently scramble the z-order).
                for (index, node) in targets.iter().rev() {
                    forward.push(NodeOp::Remove { index: *index });
                    raw_inverse.push(NodeOp::Insert {
                        index: *index,
                        node: node.clone(),
                    });
                }
                selection_after.clear();
            }
        }

        raw_inverse.reverse();
        Ok(HistoryEntry {
            label: command.label(),
            forward,
            inverse: raw_inverse,
            selection_before,
            selection_after,
        })
    }
}

/// Pushes a transform change (and its exact inverse) when it actually changes
/// the node.
fn push_transform(
    forward: &mut Vec<NodeOp>,
    raw_inverse: &mut Vec<NodeOp>,
    node: &Node,
    to: Affine,
) -> Result<(), CommandError> {
    if !to.is_finite() || to.invert().is_none() {
        return Err(CommandError::DegenerateTransform);
    }
    if to == node.transform {
        return Ok(());
    }
    forward.push(NodeOp::SetTransform { id: node.id, to });
    raw_inverse.push(NodeOp::SetTransform {
        id: node.id,
        to: node.transform,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f32, y0: f32, size: f32) -> Vec<Subpath> {
        vec![Subpath {
            start: Point::new(x0, y0),
            segs: vec![
                Seg::Line(Point::new(x0 + size, y0)),
                Seg::Line(Point::new(x0 + size, y0 + size)),
                Seg::Line(Point::new(x0, y0 + size)),
            ],
            closed: true,
        }]
    }

    /// The part of an editor's state a user can see: the nodes and the
    /// selection.
    ///
    /// Two things are deliberately excluded. History internals: after an undo
    /// the redo tail is still stored, so only the visible state returns to its
    /// previous value. The id counter: ids are never reused, so undoing a
    /// duplicate or a delete leaves the counter advanced — that is what makes a
    /// later edit unable to resurrect a stale id.
    fn view(ed: &Editor) -> (Option<Vec<Node>>, Vec<NodeId>) {
        (
            ed.doc().map(|d| d.nodes().to_vec()),
            ed.selection().to_vec(),
        )
    }

    fn editor_with_two_nodes() -> Editor {
        let mut doc = Doc::new(100.0, 100.0);
        doc.add(square(10.0, 10.0, 20.0), [255, 0, 0, 255]);
        doc.add(square(50.0, 50.0, 20.0), [0, 255, 0, 255]);
        let mut ed = Editor::new();
        ed.load(doc);
        ed
    }

    #[test]
    fn translate_then_undo_restores_exact_bits() {
        let mut ed = editor_with_two_nodes();
        ed.select_all();
        let id = ed.selection()[0];
        ed.select_only(&[id]);
        let before = ed.clone();
        let m0 = ed.doc().unwrap().node(id).unwrap().transform;
        ed.apply(&Command::Translate { dx: 3.5, dy: -1.25 })
            .unwrap();
        assert_ne!(ed.doc().unwrap().node(id).unwrap().transform, m0);
        ed.undo().unwrap();
        assert_eq!(ed.doc().unwrap().node(id).unwrap().transform, m0);
        assert_eq!(ed.doc(), before.doc());
        assert_eq!(ed.selection(), before.selection());
        assert!(!ed.history().can_undo());
    }

    #[test]
    fn undo_redo_cycles_are_stable_over_many_steps() {
        let mut ed = editor_with_two_nodes();
        ed.select_all();
        let ids = ed.selection().to_vec();
        let mut states = Vec::new();
        for i in 0..40 {
            let dx = (i as f32 % 7.0) - 3.0;
            ed.apply(&Command::Translate { dx, dy: 0.5 }).unwrap();
            states.push(view(&ed));
        }
        // Undo walks the recorded states backwards, bit-for-bit.
        for (step, expect) in states.iter().rev().skip(1).enumerate() {
            ed.undo().unwrap();
            assert!(
                view(&ed) == *expect,
                "state diverged {step} steps into undo"
            );
        }
        // Redo walks the same path forward again.
        for (step, expect) in states.iter().skip(1).enumerate() {
            ed.redo().unwrap();
            assert!(
                view(&ed) == *expect,
                "state diverged {step} steps into redo"
            );
        }
        assert_eq!(ed.selection(), ids.as_slice());
    }

    #[test]
    fn commands_validate_before_recording() {
        let mut ed = editor_with_two_nodes();
        // Nothing selected: every selection command is rejected.
        assert_eq!(
            ed.apply(&Command::Translate { dx: 1.0, dy: 1.0 }),
            Err(CommandError::EmptySelection)
        );
        ed.select_all();
        assert_eq!(
            ed.apply(&Command::Translate { dx: 0.0, dy: 0.0 }),
            Err(CommandError::ZeroDelta)
        );
        assert_eq!(
            ed.apply(&Command::Scale {
                factor: 0.0,
                pivot: (0.0, 0.0)
            }),
            Err(CommandError::DegenerateTransform)
        );
        assert_eq!(
            ed.apply(&Command::SetFill { to: [1, 2, 3, 0] }),
            Err(CommandError::Transparent)
        );
        // A rejected command leaves no history behind.
        assert!(!ed.history().can_undo());
        // Outside a document there is nothing to edit at all.
        let mut empty = Editor::new();
        assert_eq!(empty.apply(&Command::Delete), Err(CommandError::NoDocument));
        assert_eq!(empty.undo(), Err(CommandError::NoDocument));
    }

    #[test]
    fn a_no_op_fill_is_not_a_history_step() {
        let mut ed = editor_with_two_nodes();
        let first = ed.doc().unwrap().ids()[0];
        ed.select_only(&[first]);
        let fill = ed.doc().unwrap().node(first).unwrap().fill;
        assert_eq!(
            ed.apply(&Command::SetFill { to: fill }),
            Err(CommandError::NoOp)
        );
        assert!(!ed.history().can_undo());
    }

    #[test]
    fn delete_restores_exact_geometry_and_z_order() {
        let mut ed = editor_with_two_nodes();
        let before = ed.doc().unwrap().clone();
        ed.select_only(&[before.ids()[0]]);
        ed.apply(&Command::Delete).unwrap();
        assert_eq!(ed.doc().unwrap().node_count(), 1);
        assert!(ed.selection().is_empty());
        ed.undo().unwrap();
        assert_eq!(
            view(&ed),
            (Some(before.nodes().to_vec()), vec![before.ids()[0]])
        );
    }

    #[test]
    fn deleting_every_node_and_undoing_restores_z_order() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        let before = ed.doc().unwrap().clone();
        ed.select_all();
        ed.apply(&Command::Delete).unwrap();
        assert_eq!(ed.doc().unwrap().node_count(), 0);
        assert!(ed.selection().is_empty());
        ed.undo().unwrap();
        // A clamped insert would land the nodes in the wrong order here.
        assert_eq!(ed.doc().unwrap().ids(), ids);
        assert_eq!(view(&ed), (Some(before.nodes().to_vec()), ids.clone()));
    }

    #[test]
    fn duplicate_selects_the_copies_and_is_exactly_invertible() {
        let mut ed = editor_with_two_nodes();
        let before = ed.doc().unwrap().clone();
        ed.select_only(&before.ids()[1..2]);
        let source = ed.doc().unwrap().node(before.ids()[1]).unwrap().clone();
        ed.apply(&Command::Duplicate { dx: 4.0, dy: 6.0 }).unwrap();
        assert_eq!(ed.doc().unwrap().node_count(), 3);
        assert_eq!(ed.selection().len(), 1);
        let copy = ed.doc().unwrap().node(ed.selection()[0]).unwrap();
        assert_ne!(copy.id, source.id, "copies get fresh ids");
        // The offset is baked into the copy's own transform, so the copy is a
        // real, independently-editable node.
        assert_eq!(
            copy.transform,
            source.transform.then(Affine::translate(4.0, 6.0))
        );
        let (lo, hi) = ed
            .doc()
            .unwrap()
            .node(ed.selection()[0])
            .unwrap()
            .bounds()
            .unwrap();
        assert_eq!((lo.x, lo.y), (54.0, 56.0));
        assert_eq!((hi.x - lo.x, hi.y - lo.y), (20.0, 20.0));
        ed.undo().unwrap();
        assert_eq!(
            view(&ed),
            (Some(before.nodes().to_vec()), vec![before.ids()[1]])
        );
    }

    #[test]
    fn reorder_is_clamped_and_invertible() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_only(&ids[0..1]);
        ed.apply(&Command::Reorder { up: true }).unwrap();
        assert_eq!(ed.doc().unwrap().ids(), vec![ids[1], ids[0]]);
        ed.undo().unwrap();
        assert_eq!(ed.doc().unwrap().ids(), ids);
        // Already on top: nothing to do, and no history step is recorded.
        ed.select_only(&ids[1..2]);
        assert_eq!(
            ed.apply(&Command::Reorder { up: true }),
            Err(CommandError::NoOp)
        );
        assert!(!ed.history().can_undo());
    }

    #[test]
    fn later_edits_drop_the_redo_tail() {
        let mut ed = editor_with_two_nodes();
        ed.select_all();
        ed.apply(&Command::Translate { dx: 1.0, dy: 0.0 }).unwrap();
        ed.apply(&Command::Translate { dx: 1.0, dy: 0.0 }).unwrap();
        ed.undo().unwrap();
        assert!(ed.history().can_redo());
        ed.apply(&Command::Translate { dx: 5.0, dy: 0.0 }).unwrap();
        assert!(!ed.history().can_redo());
        assert_eq!(ed.history().cursor(), 2);
    }

    #[test]
    fn history_is_bounded_and_reports_drops() {
        let mut ed = editor_with_two_nodes();
        ed.select_all();
        for i in 0..(MAX_HISTORY + 20) {
            ed.apply(&Command::Translate {
                dx: 1.0,
                dy: if i % 2 == 0 { 0.0 } else { 1.0 },
            })
            .unwrap();
        }
        assert_eq!(ed.history().entries().len(), MAX_HISTORY);
        assert_eq!(ed.history().dropped(), 20);
        // Undo still walks back exactly as far as the retained history allows.
        for _ in 0..MAX_HISTORY {
            ed.undo().unwrap();
        }
        assert!(!ed.history().can_undo());
    }

    #[test]
    fn selection_helpers_are_view_state_only() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_all();
        assert_eq!(ed.selection(), ids.as_slice());
        ed.select_toggle(ids[0]);
        assert_eq!(ed.selection(), &ids[1..]);
        ed.select_add(ids[0]);
        assert_eq!(ed.selection(), ids.as_slice());
        ed.select_clear();
        assert!(ed.selection().is_empty());
        // None of that touched the history.
        assert!(!ed.history().can_undo());
        // Hidden nodes leave the selection; a deleted node cannot be selected.
        ed.select_all();
        ed.apply(&Command::SetVisible { to: false }).unwrap();
        assert!(ed.selection().is_empty());
        ed.undo().unwrap();
        assert_eq!(ed.selection(), ids.as_slice());
    }

    #[test]
    fn apply_leaves_the_selection_in_canonical_z_order() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_only(&ids);
        ed.apply(&Command::Reorder { up: true }).unwrap();
        // The reorder put the last node first; the selection follows the new
        // paint order instead of the order the nodes were clicked in.
        assert_eq!(ed.doc().unwrap().ids(), vec![ids[1], ids[0]]);
        assert_eq!(ed.selection(), &[ids[1], ids[0]]);
        let after_apply = view(&ed);
        ed.undo().unwrap();
        ed.redo().unwrap();
        assert_eq!(view(&ed), after_apply);
    }

    #[test]
    fn selection_is_normalised_against_the_document() {
        let mut ed = editor_with_two_nodes();
        let ghost = NodeId::new(999);
        ed.select_only(&[ghost]);
        assert!(ed.selection().is_empty());
        assert_eq!(ed.pick(90.0, 90.0, 1.0), None);
        assert_eq!(ed.pick(15.0, 15.0, 1.0), Some(ed.doc().unwrap().ids()[0]));
    }

    #[test]
    fn center_on_canvas_uses_the_selection_bounds() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_only(&ids[0..1]);
        ed.apply(&Command::CenterOnCanvas).unwrap();
        let (lo, hi) = ed
            .doc()
            .unwrap()
            .node(ids[0])
            .unwrap()
            .bounds()
            .expect("bounds");
        assert!((lo.x + hi.x) / 2.0 == 50.0);
        assert!((lo.y + hi.y) / 2.0 == 50.0);
        ed.undo().unwrap();
        let (lo, _hi) = ed.doc().unwrap().node(ids[0]).unwrap().bounds().unwrap();
        assert_eq!(lo.x, 10.0);
    }

    #[test]
    fn scale_and_rotate_keep_the_pivot_fixed() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_only(&ids[0..1]);
        let pivot = (20.0, 20.0);
        ed.apply(&Command::Scale { factor: 2.0, pivot }).unwrap();
        let (lo, hi) = ed.doc().unwrap().node(ids[0]).unwrap().bounds().unwrap();
        assert_eq!((lo.x, lo.y), (0.0, 0.0));
        assert_eq!((hi.x, hi.y), (40.0, 40.0));
        ed.apply(&Command::Rotate {
            degrees: 90.0,
            pivot,
        })
        .unwrap();
        let (lo, hi) = ed.doc().unwrap().node(ids[0]).unwrap().bounds().unwrap();
        assert!((lo.x - 0.0).abs() < 1e-3 && (hi.x - 40.0).abs() < 1e-3);
        ed.undo().unwrap();
        ed.undo().unwrap();
        let (lo, hi) = ed.doc().unwrap().node(ids[0]).unwrap().bounds().unwrap();
        assert_eq!((lo.x, lo.y, hi.x, hi.y), (10.0, 10.0, 30.0, 30.0));
    }
    // -- 4B: groups, arrange, align, non-uniform scale, preview ---------------

    #[test]
    fn grouping_makes_the_members_select_as_one() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_all();
        let before = ed.clone();
        assert_eq!(ed.apply(&Command::Group).unwrap(), 2);
        let group = ed.doc().unwrap().group_of(ids[0]).expect("grouped");
        assert_eq!(ed.doc().unwrap().group_of(ids[1]), Some(group));
        assert_eq!(ed.doc().unwrap().members(group), ids);

        // Any member now selects the whole group, from any entry point.
        ed.select_clear();
        ed.select_only(&[ids[1]]);
        assert_eq!(ed.selection(), ids.as_slice());
        ed.select_clear();
        ed.select_add(ids[0]);
        assert_eq!(ed.selection(), ids.as_slice());
        ed.select_toggle(ids[1]);
        assert!(
            ed.selection().is_empty(),
            "toggling a member drops the group"
        );

        // A grouped click target is the group, not the node.
        assert_eq!(ed.selection_target(ids[0]), ids);
        ed.undo().unwrap();
        assert_eq!(ed.doc().unwrap().group_count(), 0);
        // The view, not the whole `Doc`: like node ids, a group id is never
        // reused, so the counter legitimately stays ahead after an undo.
        assert_eq!(view(&ed), view(&before), "ungroup restores the nodes");
    }

    #[test]
    fn ungroup_leaves_the_members_alone_and_is_invertible() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_all();
        ed.apply(&Command::Group).unwrap();
        assert_eq!(ed.apply(&Command::Ungroup).unwrap(), 2);
        assert_eq!(ed.doc().unwrap().group_count(), 0);
        ed.undo().unwrap();
        assert!(ed.doc().unwrap().group_of(ids[0]).is_some());
        ed.redo().unwrap();
        assert!(ed.doc().unwrap().group_of(ids[0]).is_none());
    }

    #[test]
    fn grouping_validates_before_recording() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_only(&ids[0..1]);
        assert_eq!(ed.apply(&Command::Group), Err(CommandError::NoOp));
        assert!(!ed.history().can_undo());
        ed.select_all();
        ed.apply(&Command::Group).unwrap();
        // Selecting a group and grouping it again changes nothing.
        assert_eq!(ed.apply(&Command::Group), Err(CommandError::NoOp));
        assert_eq!(ed.history().cursor(), 1);
        ed.select_only(&ids[0..1]);
        assert_eq!(ed.apply(&Command::Ungroup).unwrap(), 2);
    }

    #[test]
    fn arrange_moves_the_selection_to_the_ends_and_keeps_its_order() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        // Bottom two of three selected, moved to the front: relative order kept.
        ed.select_all();
        ed.apply(&Command::Duplicate { dx: 0.0, dy: 0.0 }).unwrap();
        let all = ed.doc().unwrap().ids();
        ed.select_only(&[ids[0], ids[1]]);
        ed.apply(&Command::Arrange {
            to: ArrangeTo::Front,
        })
        .unwrap();
        let after = ed.doc().unwrap().ids();
        assert_eq!(after[after.len() - 2..], [ids[0], ids[1]]);
        assert_eq!(after.len(), all.len());
        ed.undo().unwrap();
        assert_eq!(ed.doc().unwrap().ids(), all, "arrange undoes exactly");

        // Back to where they started, which is already the back of the stack.
        ed.select_only(&[ids[0], ids[1]]);
        assert_eq!(
            ed.apply(&Command::Arrange {
                to: ArrangeTo::Back
            }),
            Err(CommandError::NoOp)
        );
        ed.apply(&Command::Arrange {
            to: ArrangeTo::Front,
        })
        .unwrap();
        ed.apply(&Command::Arrange {
            to: ArrangeTo::Back,
        })
        .unwrap();
        assert_eq!(ed.doc().unwrap().ids()[..2], [ids[0], ids[1]]);
        assert_eq!(
            ed.apply(&Command::Arrange {
                to: ArrangeTo::Back
            }),
            Err(CommandError::NoOp),
            "already at the back"
        );
    }

    #[test]
    fn align_lines_the_selection_up_against_its_own_box_and_the_canvas() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_all();
        ed.apply(&Command::Align {
            frame: AlignFrame::Selection,
            edge: AlignEdge::Left,
        })
        .unwrap();
        let lefts: Vec<f32> = ids
            .iter()
            .map(|id| ed.doc().unwrap().node(*id).unwrap().bounds().unwrap().0.x)
            .collect();
        assert_eq!(lefts, vec![10.0, 10.0]);
        ed.undo().unwrap();
        assert_eq!(
            ed.doc().unwrap().node(ids[1]).unwrap().transform,
            Affine::IDENTITY
        );

        // Against the canvas: the selection's centre lands on the canvas centre.
        ed.select_all();
        ed.apply(&Command::Align {
            frame: AlignFrame::Canvas,
            edge: AlignEdge::HCenter,
        })
        .unwrap();
        let (lo, hi) = ed.selection_bounds().unwrap();
        assert_eq!((lo.x + hi.x) / 2.0, 50.0);
        ed.apply(&Command::Align {
            frame: AlignFrame::Canvas,
            edge: AlignEdge::Right,
        })
        .unwrap();
        let (_lo, hi) = ed.selection_bounds().unwrap();
        assert_eq!(hi.x, 100.0);
        // Aligned already: not a history step.
        assert_eq!(
            ed.apply(&Command::Align {
                frame: AlignFrame::Canvas,
                edge: AlignEdge::Right,
            }),
            Err(CommandError::NoOp)
        );
    }

    #[test]
    fn non_uniform_scale_stretches_and_undoes_exactly() {
        let mut ed = editor_with_two_nodes();
        let id = ed.doc().unwrap().ids()[0];
        ed.select_only(&[id]);
        let before = ed.doc().unwrap().node(id).unwrap().transform;
        ed.apply(&Command::ScaleXY {
            sx: 2.0,
            sy: 0.5,
            pivot: (10.0, 10.0),
        })
        .unwrap();
        let (lo, hi) = ed.doc().unwrap().node(id).unwrap().bounds().unwrap();
        assert_eq!((lo.x, lo.y), (10.0, 10.0));
        assert_eq!((hi.x, hi.y), (50.0, 20.0));
        ed.undo().unwrap();
        assert_eq!(ed.doc().unwrap().node(id).unwrap().transform, before);
        ed.redo().unwrap();
        let (_, hi) = ed.doc().unwrap().node(id).unwrap().bounds().unwrap();
        assert_eq!((hi.x, hi.y), (50.0, 20.0));
        ed.undo().unwrap();
    }

    #[test]
    fn a_preview_shows_without_recording_and_commits_identically() {
        let mut ed = editor_with_two_nodes();
        let id = ed.doc().unwrap().ids()[0];
        ed.select_only(&[id]);
        let stored = ed.doc().unwrap().node(id).unwrap().transform;
        let command = Command::Translate { dx: 12.0, dy: -4.0 };
        ed.preview(&command).unwrap();
        assert!(ed.preview_active());
        assert_ne!(ed.display_transform(id).unwrap(), stored);
        assert_eq!(
            ed.doc().unwrap().node(id).unwrap().transform,
            stored,
            "a preview never mutates the document"
        );
        assert!(!ed.history().can_undo(), "a preview is not a history step");
        let previewed = ed.display_bounds(id).unwrap();

        ed.preview_clear();
        assert!(!ed.preview_active());
        assert_eq!(ed.display_transform(id).unwrap(), stored);

        // Committing the same command reproduces exactly what was previewed.
        ed.preview(&command).unwrap();
        ed.apply(&command).unwrap();
        assert!(!ed.preview_active(), "a commit drops the live preview");
        assert_eq!(ed.display_bounds(id).unwrap(), previewed);
        assert_eq!(ed.history().cursor(), 1, "one drag is one undo step");
    }

    #[test]
    fn previews_validate_like_commands_and_disappear_with_the_document() {
        let mut ed = editor_with_two_nodes();
        assert_eq!(
            ed.preview(&Command::Translate { dx: 1.0, dy: 1.0 }),
            Err(CommandError::EmptySelection)
        );
        let id = ed.doc().unwrap().ids()[0];
        ed.select_only(&[id]);
        assert_eq!(
            ed.preview(&Command::Translate { dx: 0.0, dy: 0.0 }),
            Err(CommandError::ZeroDelta)
        );
        ed.preview(&Command::Translate { dx: 1.0, dy: 1.0 })
            .unwrap();
        ed.undo()
            .expect_err("nothing was recorded, so there is nothing to undo");
        ed.close();
        assert!(!ed.preview_active());
    }

    #[test]
    fn snap_reports_the_delta_it_would_apply_and_the_guides_that_explain_it() {
        let mut ed = editor_with_two_nodes();
        let id = ed.doc().unwrap().ids()[1]; // square at (50, 50)-(70, 70)
        ed.select_only(&[id]);
        // A drag that lands the left edge 2 px short of the canvas centre line.
        let options = SnapOptions {
            tolerance: 6.0,
            ..SnapOptions::default()
        };
        let snapped = ed.snap_move(48.0 - 50.0, 20.0, &options).unwrap();
        assert_eq!((snapped.dx, snapped.dy), (0.0, 20.0), "left edge → centre");
        assert!(snapped
            .guides
            .iter()
            .any(|g| g.axis == Axis::X && g.kind == GuideKind::CanvasCenter && g.position == 50.0));
        // Far from every target: the proposed delta passes through untouched.
        let free = ed.snap_move(13.0, 20.0, &options).unwrap();
        assert_eq!((free.dx, free.dy), (13.0, 20.0));
        assert!(free.guides.is_empty());
        // With every family off there is nothing to snap to at all.
        let off = SnapOptions {
            canvas: false,
            nodes: false,
            grid: false,
            ..options
        };
        assert!(!off.is_active());
        let raw = ed.snap_move(48.0 - 50.0, 0.0, &off).unwrap();
        assert_eq!(raw.dx, -2.0);
    }

    #[test]
    fn snapping_prefers_the_nearest_target_and_stays_inside_the_tolerance() {
        let mut ed = editor_with_two_nodes();
        let ids = ed.doc().unwrap().ids();
        ed.select_only(&ids[1..2]);
        // The other node spans x 10..30. The proposed box lands at x 35..55, so
        // its left edge is 5 px from that node's right edge (30) *and* its centre
        // is 5 px from the canvas centre line (50): a tie, which the node wins.
        let options = SnapOptions {
            tolerance: 6.0,
            ..SnapOptions::default()
        };
        let snapped = ed.snap_move(35.0 - 50.0, 0.0, &options).unwrap();
        assert_eq!(snapped.dx, 30.0 - 50.0);
        assert!(snapped.guides.iter().any(|g| g.kind == GuideKind::NodeEdge));
        // A tolerance of zero disables snapping rather than snapping to the
        // nearest thing regardless of distance.
        let strict = SnapOptions {
            tolerance: 0.0,
            ..options
        };
        assert!(!strict.is_active());
    }
}
