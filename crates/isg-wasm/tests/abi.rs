//! Host-side tests for the WASM ABI.
//!
//! These run natively (`cargo test`) as well as in the local development mirror;
//! the browser path is the same dispatcher, so what is checked here is what the
//! canvas will call. The Phase 4 exit criterion — `undo(do(x)) == x` over
//! thousands of random edit sequences — is checked twice: once through the
//! explicit-table API and once through the module's own tables (the entry point
//! the exported `editor_call` uses).

use isg_wasm::abi::{
    self, feature, Abi, ABI_VERSION, DOC_HEADER, ERR_BAD_ARGUMENT, ERR_BAD_FEATURE, ERR_DEGENERATE,
    ERR_MISSING_NODE, ERR_NO_DOCUMENT, ERR_NO_HISTORY, ERR_NO_OP, ERR_NO_SELECTION,
    ERR_TRANSPARENT, IN_WORDS, MAX_FEATURE, MAX_NODES, NODE_RECORD_HEADER, OUT_WORDS,
};
use isg_wasm::doc_blob::{decode_doc, decode_path, encode_doc, encode_path};
use isg_wasm::editor::{Affine, Command, Doc, Node, NodeId, Point, Seg, Subpath};

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

fn square(x: f32, y: f32, size: f32) -> Subpath {
    Subpath {
        start: Point::new(x, y),
        segs: vec![
            Seg::Line(Point::new(x + size, y)),
            Seg::Line(Point::new(x + size, y + size)),
            Seg::Line(Point::new(x, y + size)),
        ],
        closed: true,
    }
}

fn blob_arc() -> Subpath {
    Subpath {
        start: Point::new(0.0, 0.0),
        segs: vec![Seg::Cubic {
            c1: Point::new(0.0, 10.0),
            c2: Point::new(10.0, 10.0),
            to: Point::new(10.0, 0.0),
        }],
        closed: false,
    }
}

/// A document that exercises the whole blob format: line and cubic segments,
/// open and closed subpaths, a non-identity transform, a hidden node and fills
/// that use the full byte range.
fn fixture_doc() -> Doc {
    let mut doc = Doc::new(200.0, 120.0);
    let a = doc.add(vec![square(10.0, 10.0, 20.0)], [255, 0, 128, 255]);
    let b = doc.add(vec![square(0.0, 0.0, 30.0), blob_arc()], [0, 255, 0, 200]);
    let c = doc.add(vec![blob_arc()], [1, 2, 3, 4]);
    for (id, m) in [
        (a, Affine::IDENTITY),
        // Rotate about the node's own origin, then place it: the natural
        // "position this icon" transform, and what the bounds tests assume.
        (b, Affine::rotate(30.0).then(Affine::translate(50.0, 20.0))),
        (c, Affine::scale(2.0, -1.0)),
    ] {
        let index = doc.index_of(id).expect("fixture node");
        let mut node = doc.node(id).expect("fixture node").clone();
        node.transform = m;
        doc.set_at(index, node);
    }
    let index = doc.index_of(c).expect("fixture node");
    let mut hidden = doc.node(c).expect("fixture node").clone();
    hidden.visible = false;
    doc.set_at(index, hidden);
    doc
}

/// A fresh dispatcher with explicit tables, mirroring what the module does.
struct Harness {
    abi: Abi,
    input: Vec<u32>,
    out: Vec<u32>,
}

impl Harness {
    fn new() -> Self {
        Self {
            abi: Abi::new(),
            input: vec![0; IN_WORDS],
            out: vec![0; OUT_WORDS],
        }
    }

    fn put(&mut self, words: &[u32], at: usize) {
        self.input[at..at + words.len()].copy_from_slice(words);
    }

    fn call(&mut self, feature: u32, a: u32, b: u32) -> u32 {
        self.abi.call(feature, a, b, &self.input, &mut self.out)
    }

    fn f(&mut self, feature: u32, a: f32, b: f32) -> u32 {
        self.call(feature, a.to_bits(), b.to_bits())
    }

    fn out_f32(&self, index: usize) -> f32 {
        f32::from_bits(self.out[index])
    }

    fn load(&mut self, doc: &Doc) -> u32 {
        let blob = encode_doc(doc);
        self.put(&blob, 0);
        self.call(feature::DOC_LOAD, 0, 0)
    }

    /// Applies a command and returns the operation count (0 on rejection).
    fn apply(&mut self, command: &Command) -> u32 {
        let spec = abi::encode_command(command);
        self.put(&spec, 0);
        self.call(feature::APPLY_SPEC, 0, 0)
    }

    fn error(&mut self) -> u32 {
        self.abi.error()
    }

    /// The nodes and the selection — the state a user can see. History internals
    /// and the id counter are deliberately excluded (an undo keeps the redo tail
    /// and never re-issues an id).
    fn view(&self) -> (Vec<Node>, Vec<NodeId>) {
        (
            self.abi
                .editor()
                .doc()
                .map(|d| d.nodes().to_vec())
                .unwrap_or_default(),
            self.abi.editor().selection().to_vec(),
        )
    }

    /// Reads the node records the last `NODE_SYNC` wrote.
    fn synced_nodes(&mut self) -> Vec<Node> {
        let count = self.call(feature::NODE_SYNC, 0, 0) as usize;
        let mut nodes = Vec::with_capacity(count);
        let mut at = 0usize;
        for _ in 0..count {
            let id = NodeId::new(self.out[at]);
            let mut m = [0.0f32; 6];
            for (i, slot) in m.iter_mut().enumerate() {
                *slot = f32::from_bits(self.out[at + 1 + i]);
            }
            let fill_word = self.out[at + 7];
            let path_words = self.out[at + 9] as usize;
            let mut consumed = 0;
            let path = decode_path(&self.out[at + NODE_RECORD_HEADER..], 0, &mut consumed)
                .expect("record path decodes");
            assert_eq!(consumed, path_words, "record path length agrees");
            let mut node = Node::new(
                id,
                path,
                [
                    (fill_word >> 24) as u8,
                    (fill_word >> 16) as u8,
                    (fill_word >> 8) as u8,
                    fill_word as u8,
                ],
            );
            node.transform = Affine::new(m);
            node.visible = self.out[at + 8] != 0;
            nodes.push(node);
            at += NODE_RECORD_HEADER + path_words;
        }
        nodes
    }
}

/// Deterministic xorshift so the property checks are reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u32) -> u32 {
        (self.next() % u64::from(n)) as u32
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }
}

// ---------------------------------------------------------------------------
// the blob format
// ---------------------------------------------------------------------------

#[test]
fn document_blob_round_trips_bit_for_bit() {
    let doc = fixture_doc();
    let words = encode_doc(&doc);
    assert_eq!(words[..3], [200.0f32.to_bits(), 120.0f32.to_bits(), 3]);
    assert_eq!(words[3] as usize, words.len(), "declared length matches");
    let back = decode_doc(&words).expect("decodes");
    assert_eq!(back.width(), doc.width());
    assert_eq!(back.height(), doc.height());
    assert_eq!(back.nodes(), doc.nodes());
    // Re-encoding the decoded document must produce the same words, otherwise a
    // load/edit/save cycle could drift.
    assert_eq!(encode_doc(&back), words);
    eprintln!(
        "evidence: editor document blob — {}-word encode, {} nodes, bit-for-bit round trip",
        words.len(),
        doc.node_count()
    );
}

#[test]
fn path_blob_round_trips_lines_and_cubics() {
    let path = vec![square(1.5, -2.25, 4.0), blob_arc()];
    let words = encode_path(&path);
    let mut consumed = 0;
    let back = decode_path(&words, 0, &mut consumed).expect("decodes");
    assert_eq!(consumed, words.len());
    assert_eq!(back, path);
}

#[test]
fn malformed_blobs_are_rejected_not_panicked() {
    let doc = fixture_doc();
    let full = encode_doc(&doc);

    // Too short to hold a header.
    assert_eq!(decode_doc(&full[..2]), Err(ERR_BAD_ARGUMENT));
    // Node count says three, words run out after the first node.
    let mut truncated = full.clone();
    truncated[2] = 3;
    truncated.truncate(DOC_HEADER + NODE_RECORD_HEADER + 3);
    assert_eq!(decode_doc(&truncated), Err(ERR_BAD_ARGUMENT));
    // A node whose declared path length disagrees with the words present.
    let mut inconsistent = full.clone();
    let per_node = NODE_RECORD_HEADER + encode_path(&doc.nodes()[0].path).len();
    inconsistent[DOC_HEADER + 9] += 7;
    assert_eq!(decode_doc(&inconsistent), Err(ERR_BAD_ARGUMENT));
    assert!(per_node > NODE_RECORD_HEADER);
    // An unknown segment kind inside an otherwise valid blob.
    let mut kind = encode_path(&[square(0.0, 0.0, 1.0)]);
    kind[5] = 9;
    let mut consumed = 0;
    assert_eq!(
        decode_path(&kind, 0, &mut consumed),
        Err(ERR_BAD_ARGUMENT),
        "segment kind 9 is not part of the format"
    );
}

#[test]
fn every_documented_feature_has_an_answer() {
    // No feature inside the documented range may fall through to "unknown".
    let mut h = Harness::new();
    for feature_number in 0..=MAX_FEATURE {
        // Loading is the one feature that would make the session contain a
        // document, which is exactly what the rest of the loop needs it not to.
        if feature_number == feature::DOC_LOAD {
            continue;
        }
        h.call(feature_number, 0, 0);
        let code = h.error();
        assert_ne!(
            code, ERR_BAD_FEATURE,
            "feature {feature_number} is declared but not dispatched"
        );
        // Everything that touches a document must be gated on one existing —
        // the three features that do not care are listed explicitly.
        match feature_number {
            feature::VERSION | feature::CLOSE | feature::ERROR => {}
            other => assert_eq!(
                code, ERR_NO_DOCUMENT,
                "feature {other} must refuse to run without a document"
            ),
        }
    }
    // All-zero words are not a document either: the canvas has no size.
    assert_eq!(h.call(feature::DOC_LOAD, 0, 0), 0);
    assert_eq!(h.error(), ERR_BAD_ARGUMENT);
    assert!(!h.abi.editor().has_document());
    assert_eq!(h.call(MAX_FEATURE + 1, 0, 0), 0);
    assert_eq!(h.error(), ERR_BAD_FEATURE);
    eprintln!(
        "evidence: editor ABI v{ABI_VERSION} — features 0..={MAX_FEATURE} ({} of them) \
         all answered, unknown features refused with code {ERR_BAD_FEATURE}",
        MAX_FEATURE + 1
    );
}

// ---------------------------------------------------------------------------
// document, geometry and selection features
// ---------------------------------------------------------------------------

#[test]
fn load_and_report_the_document() {
    let doc = fixture_doc();
    let mut h = Harness::new();
    assert_eq!(h.load(&doc), 3);
    assert_eq!(h.call(feature::NODE_COUNT, 0, 0), 3);
    // 3 (square) + 3 + 1 (square + arc) + 1 (arc).
    assert_eq!(h.call(feature::SEGMENT_COUNT, 0, 0), 8);
    assert_eq!(h.call(feature::DOC_SIZE, 0, 0), 2);
    assert_eq!((h.out_f32(0), h.out_f32(1)), (200.0, 120.0));
    assert_eq!(h.call(feature::NODE_AT, 0, 0), doc.ids()[0].get());
    assert_eq!(h.call(feature::NODE_AT, 9, 0), 0);
    assert!(h.abi.revision() > 0);
}

#[test]
fn node_sync_matches_the_document_and_terminates() {
    let doc = fixture_doc();
    let mut h = Harness::new();
    h.load(&doc);
    let nodes = h.synced_nodes();
    assert_eq!(
        nodes,
        doc.nodes(),
        "records round-trip paths and transforms"
    );
    // The record after the last one is zeroed, so a stale output table can never
    // be walked as if it held fresh records.
    let synced = h.call(feature::NODE_SYNC, 0, 0) as usize;
    let words: usize = h
        .synced_nodes()
        .iter()
        .map(|n| NODE_RECORD_HEADER + encode_path(&n.path).len())
        .sum();
    assert_eq!(synced, 3);
    assert_eq!(h.out[words], 0);
}

#[test]
fn path_flush_folds_in_the_transform() {
    let doc = fixture_doc();
    let mut h = Harness::new();
    h.load(&doc);
    let second = doc.ids()[1];
    let words = h.call(feature::PATH_FLUSH, second.get(), 0) as usize;
    assert!(words > 0);
    let mut consumed = 0;
    let placed = decode_path(&h.out[..words], 0, &mut consumed).expect("decodes");
    assert_eq!(consumed, words);
    // The first subpath starts at the node's own origin, which the fixture's
    // rotate-then-place transform maps to (50, 20).
    let first = placed[0].start;
    assert!(
        (first.x - 50.0).abs() < 1e-3 && (first.y - 20.0).abs() < 1e-3,
        "{first:?}"
    );
    // An unknown id is an error, not a silent empty path.
    assert_eq!(h.call(feature::PATH_FLUSH, 999, 0), 0);
    assert_eq!(h.error(), ERR_BAD_ARGUMENT);
}

#[test]
fn bounds_follow_the_placed_transform() {
    let doc = fixture_doc();
    let mut h = Harness::new();
    h.load(&doc);
    let first = doc.ids()[0];
    assert_eq!(h.call(feature::NODE_BOUNDS, first.get(), 0), 1);
    let b = [h.out_f32(0), h.out_f32(1), h.out_f32(2), h.out_f32(3)];
    assert_eq!(b, [10.0, 10.0, 30.0, 30.0]);
    // Scaled by two on x and mirrored on y: the box follows.
    // The second node is rotated by 30° about its own origin and then placed,
    // so its box is the rotated one, not the axis-aligned original.
    let second = doc.ids()[1];
    assert_eq!(h.call(feature::NODE_BOUNDS, second.get(), 0), 1);
    let placed = (h.out_f32(0), h.out_f32(1), h.out_f32(2), h.out_f32(3));
    let expect = doc.node(second).unwrap().bounds().unwrap();
    for (got, want) in [
        (placed.0, expect.0.x),
        (placed.1, expect.0.y),
        (placed.2, expect.1.x),
        (placed.3, expect.1.y),
    ] {
        assert!((got - want).abs() < 1e-3, "{placed:?} vs {expect:?}");
    }
    // The third node is mirrored in y (scale 2, −1), so its box lives below the
    // origin: the arc reaches y = −10.
    let third = doc.ids()[2];
    assert_eq!(h.call(feature::NODE_BOUNDS, third.get(), 0), 1);
    assert_eq!([h.out_f32(0), h.out_f32(2)], [0.0, 20.0]);
    assert!(
        (h.out_f32(3) - 0.0).abs() < 1e-3,
        "mirrored in y: {}",
        h.out_f32(3)
    );
}

#[test]
fn picking_prefers_the_topmost_node_and_respects_visibility() {
    let doc = fixture_doc();
    let mut h = Harness::new();
    h.load(&doc);
    let ids = doc.ids();
    // (56, 25) is inside the rotated, placed second square and outside the
    // first one at (10, 10)–(30, 30).
    assert!(
        !doc.node(ids[0]).unwrap().contains(56.0, 25.0, 0.0),
        "fixture check: the point misses the first square"
    );
    let hit = h.f(feature::PICK, 56.0, 25.0);
    assert_eq!(hit, ids[1].get(), "the topmost visible node wins");
    assert_eq!(h.f(feature::PICK, 195.0, 115.0), 0, "empty space");
    // The third node is hidden, so it can never be picked.
    let hidden_point = {
        let (lo, hi) = doc.node(ids[2]).unwrap().bounds().unwrap();
        ((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0)
    };
    assert_eq!(h.f(feature::PICK, hidden_point.0, hidden_point.1), 0);
    // Tolerance is a live setting, not a constant.
    assert!(h.call(feature::GET_TOLERANCE, 0, 0) > 0);
    h.call(feature::SET_TOLERANCE, 64.0f32.to_bits(), 0);
    assert_eq!(h.call(feature::GET_TOLERANCE, 0, 0), 64.0f32.to_bits());
    h.call(feature::SET_TOLERANCE, (-1.0f32).to_bits(), 0);
    assert_eq!(h.error(), ERR_BAD_ARGUMENT, "negative tolerance refused");
}

#[test]
fn marquee_and_selection_features_round_trip() {
    let doc = fixture_doc();
    let mut h = Harness::new();
    h.load(&doc);
    let ids = doc.ids();
    h.put(
        &[
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            45.0f32.to_bits(),
            45.0f32.to_bits(),
        ],
        0,
    );
    let count = h.call(feature::MARQUEE, 0, 0);
    // The first square and the placed second one both reach into the rectangle;
    // the hidden third node does not, even though its box intersects it.
    assert_eq!(count, 2);
    assert_eq!(&h.out[..2], &[ids[0].get(), ids[1].get()]);
    assert_eq!(h.out[2], 0, "id lists carry a terminator");

    assert_eq!(h.call(feature::SELECT_ONLY, ids[1].get(), 0), 1);
    assert_eq!(h.call(feature::SELECTION_COUNT, 0, 0), 1);
    assert_eq!(h.call(feature::SELECT_ADD, ids[0].get(), 0), 2);
    assert_eq!(h.call(feature::SELECTION_IDS, 0, 0), 2);
    assert_eq!(&h.out[..2], &[ids[0].get(), ids[1].get()], "z order");
    assert_eq!(h.call(feature::SELECTION_BOUNDS, 0, 0), 1);
    assert_eq!(h.call(feature::SELECT_TOGGLE, ids[0].get(), 0), 1);
    assert_eq!(h.call(feature::SELECT_ALL, 0, 0), 3);
    assert_eq!(h.call(feature::SELECT_CLEAR, 0, 0), 0);
    assert_eq!(h.call(feature::SELECTION_COUNT, 0, 0), 0);
    // A marquee that matches nothing is not an error (and the corners may be
    // given in any order — this one is entirely below the canvas).
    h.put(
        &[
            600.0f32.to_bits(),
            600.0f32.to_bits(),
            500.0f32.to_bits(),
            500.0f32.to_bits(),
        ],
        0,
    );
    assert_eq!(h.call(feature::MARQUEE, 0, 0), 0);
    assert_eq!(h.error(), abi::ERR_NONE);
}

// ---------------------------------------------------------------------------
// commands, history and error codes
// ---------------------------------------------------------------------------

#[test]
fn each_engine_error_maps_to_its_wire_code() {
    let mut h = Harness::new();

    // Nothing loaded at all.
    assert_eq!(h.call(feature::NODE_COUNT, 0, 0), 0);
    assert_eq!(h.error(), ERR_NO_DOCUMENT);
    assert_eq!(h.call(feature::APPLY_SPEC, 0, 0), 0);
    assert_eq!(h.error(), ERR_NO_DOCUMENT);

    let doc = fixture_doc();
    h.load(&doc);
    let ids = doc.ids();

    // Nothing selected.
    assert_eq!(h.apply(&Command::Delete), 0);
    assert_eq!(h.error(), ERR_NO_SELECTION);

    h.call(feature::SELECT_ONLY, ids[0].get(), 0);
    assert_eq!(h.apply(&Command::Translate { dx: 0.0, dy: 0.0 }), 0);
    assert_eq!(h.error(), ERR_NO_OP);
    assert_eq!(
        h.apply(&Command::Scale {
            factor: 0.0,
            pivot: (0.0, 0.0)
        }),
        0
    );
    assert_eq!(h.error(), ERR_DEGENERATE);
    assert_eq!(h.apply(&Command::SetFill { to: [1, 2, 3, 0] }), 0);
    assert_eq!(h.error(), ERR_TRANSPARENT);
    // A fill that is already in force changes nothing.
    let fill = h.abi.editor().doc().unwrap().node(ids[0]).unwrap().fill;
    assert_eq!(h.apply(&Command::SetFill { to: fill }), 0);
    assert_eq!(h.error(), ERR_NO_OP);
    // Reordering the bottom node downwards is impossible.
    assert_eq!(h.apply(&Command::Reorder { up: false }), 0);
    assert_eq!(h.error(), ERR_NO_OP);
    // A malformed command spec is refused before it can touch the document.
    h.put(&[42, 0, 0], 0);
    assert_eq!(h.call(feature::APPLY_SPEC, 0, 0), 0);
    assert_eq!(h.error(), ERR_BAD_ARGUMENT);
    // Nothing has been recorded, so there is nothing to undo.
    assert_eq!(h.call(feature::UNDO, 0, 0), 0);
    assert_eq!(h.error(), ERR_NO_HISTORY);

    // A stale node id is reported as such.
    h.abi.editor_mut().apply(&Command::Delete).ok();
    h.call(feature::SELECT_ONLY, ids[0].get(), 0);
    assert_eq!(h.abi.editor().selection().len(), 0, "deleted node is gone");
    assert_eq!(
        h.abi
            .editor_mut()
            .apply(&Command::Translate { dx: 1.0, dy: 1.0 }),
        Err(isg_wasm::CommandError::EmptySelection)
    );
    assert_eq!(
        abi::error_code(isg_wasm::CommandError::MissingNode(ids[0])),
        ERR_MISSING_NODE
    );
}

#[test]
fn undo_and_redo_report_their_label_in_the_output_table() {
    let mut h = Harness::new();
    h.load(&fixture_doc());
    let first = h.abi.editor().doc().unwrap().ids()[0];
    h.call(feature::SELECT_ONLY, first.get(), 0);
    assert_eq!(h.apply(&Command::Translate { dx: 4.0, dy: 0.0 }), 1);

    assert_eq!(h.call(feature::UNDO_LABEL, 0, 0), 4);
    assert_eq!(h.label(4), "move");
    assert_eq!(h.call(feature::LAST_LABEL, 0, 0), 4);
    assert_eq!(h.label(4), "move");
    assert_eq!(h.call(feature::CAN_UNDO, 0, 0), 1);
    assert_eq!(h.call(feature::CAN_REDO, 0, 0), 0);
    assert_eq!(h.call(feature::UNDO, 0, 0), 4);
    assert_eq!(h.label(4), "move", "UNDO delivers the label it undid");
    assert_eq!(h.call(feature::HISTORY_LEN, 0, 0), 0);
    assert_eq!(h.call(feature::HISTORY_REDO_DEPTH, 0, 0), 1);
    assert_eq!(h.call(feature::CAN_REDO, 0, 0), 1);
    assert_eq!(h.call(feature::REDO, 0, 0), 4);
    assert_eq!(h.label(4), "move");
    assert_eq!(h.call(feature::HISTORY_DROPPED, 0, 0), 0);
}

#[test]
fn close_drops_the_session_and_the_history() {
    let mut h = Harness::new();
    h.load(&fixture_doc());
    h.call(feature::SELECT_ALL, 0, 0);
    h.apply(&Command::Translate { dx: 1.0, dy: 1.0 });
    assert_eq!(h.call(feature::CLOSE, 0, 0), 0);
    assert_eq!(h.call(feature::NODE_COUNT, 0, 0), 0);
    assert_eq!(h.error(), ERR_NO_DOCUMENT);
    assert!(!h.abi.editor().history().can_undo());
    // Loading again starts from a clean baseline.
    h.load(&fixture_doc());
    assert!(!h.abi.editor().history().can_undo());
    assert_eq!(h.call(feature::SELECTION_COUNT, 0, 0), 0);
}

#[test]
fn the_node_cap_is_enforced() {
    let mut doc = Doc::new(10.0, 10.0);
    for i in 0..(abi::MAX_NODES + 1) {
        doc.add(
            vec![square(0.0, 0.0, 1.0)],
            [1, 2, 3, u8::try_from(i % 255 + 1).unwrap()],
        );
    }
    let mut h = Harness::new();
    let blob = encode_doc(&doc);
    h.put(&blob, 0);
    assert_eq!(h.call(feature::DOC_LOAD, 0, 0), 0);
    assert_eq!(h.error(), abi::ERR_CAPACITY);
    eprintln!(
        "evidence: editor node cap {MAX_NODES} enforced on the wire (a {}-node load is refused)",
        MAX_NODES + 1
    );
}

// ---------------------------------------------------------------------------
// the exit criterion: undo(do(x)) == x
// ---------------------------------------------------------------------------

impl Harness {
    fn label(&self, len: usize) -> String {
        let bytes: Vec<u8> = self
            .out
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .take(len)
            .collect();
        String::from_utf8(bytes).expect("labels are UTF-8")
    }
}

/// Runs one randomised edit-and-reverse sequence. Returns how many edits landed.
fn random_walk(h: &mut Harness, rng: &mut Rng, iterations: u32) -> u32 {
    let mut applied = 0;
    for _ in 0..iterations {
        let ids: Vec<u32> = h
            .abi
            .editor()
            .doc()
            .map(|d| d.ids().iter().map(|i| i.get()).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            break;
        }
        let roll = rng.below(10);
        if roll < 3 {
            h.call(
                feature::SELECT_ONLY,
                ids[rng.below(ids.len() as u32) as usize],
                0,
            );
        } else if roll < 6 {
            h.call(feature::SELECT_ALL, 0, 0);
        } else if roll < 7 {
            h.call(feature::SELECT_CLEAR, 0, 0);
        } else {
            h.call(
                feature::SELECT_ADD,
                ids[rng.below(ids.len() as u32) as usize],
                0,
            );
        }

        let command = match rng.below(10) {
            0 => Command::Translate {
                dx: rng.unit() * 20.0 - 10.0,
                dy: rng.unit() * 20.0 - 10.0,
            },
            1 => Command::Scale {
                factor: 0.25 + rng.unit() * 3.0,
                pivot: (rng.unit() * 200.0, rng.unit() * 120.0),
            },
            2 => Command::Rotate {
                degrees: rng.unit() * 720.0 - 360.0,
                pivot: (rng.unit() * 200.0, rng.unit() * 120.0),
            },
            3 => Command::Duplicate {
                dx: rng.unit() * 10.0 - 5.0,
                dy: rng.unit() * 10.0 - 5.0,
            },
            4 => Command::SetFill {
                to: [
                    rng.below(256) as u8,
                    rng.below(256) as u8,
                    rng.below(256) as u8,
                    (1 + rng.below(255)) as u8,
                ],
            },
            5 => Command::SetVisible {
                to: rng.below(2) == 1,
            },
            6 => Command::Reorder {
                up: rng.below(2) == 1,
            },
            7 => Command::Delete,
            8 => Command::CenterOnCanvas,
            _ => Command::Translate { dx: 3.0, dy: -2.0 },
        };

        let before = h.view();
        if h.apply(&command) == 0 {
            // Rejections must not have changed anything either.
            assert_eq!(
                h.view(),
                before,
                "rejected command {command:?} mutated state"
            );
            continue;
        }
        applied += 1;
        let after = h.view();

        let label = h.call(feature::UNDO, 0, 0);
        assert!(label > 0, "undo after {command:?} reports its label");
        assert_eq!(
            h.view(),
            before,
            "undo of {command:?} did not restore state"
        );

        let label = h.call(feature::REDO, 0, 0);
        assert!(label > 0, "redo after {command:?} reports its label");
        assert_eq!(
            h.view(),
            after,
            "redo of {command:?} did not reproduce state"
        );

        // Leaving the walk undone keeps the document non-empty and the next
        // iteration meaningful (a redo in force would let a delete-all end the
        // walk prematurely).
        h.call(feature::UNDO, 0, 0);
    }
    applied
}

#[test]
fn property_undo_do_is_identity_over_random_sequences() {
    let mut h = Harness::new();
    h.load(&fixture_doc());
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let applied = random_walk(&mut h, &mut rng, 4000);
    assert!(
        applied > 2000,
        "the walk must actually exercise the engine, applied {applied}"
    );
    eprintln!(
        "evidence: editor property (host ABI) — {applied} edits landed over 4000 \
         randomised sequences, zero undo/redo divergences"
    );
}

#[test]
fn property_undo_do_is_identity_through_the_module_tables() {
    // The same walk, but driven through the module's own input/output tables —
    // the exact entry point `editor_call` uses in the browser.
    let mut h = Harness::new();
    let blob = encode_doc(&fixture_doc());
    h.put(&blob, 0);
    assert_eq!(h.call(feature::DOC_LOAD, 0, 0), 3);

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut applied = 0;
    for _ in 0..600 {
        let ids: Vec<u32> = h
            .abi
            .editor()
            .doc()
            .map(|d| d.ids().iter().map(|i| i.get()).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            break;
        }
        h.call(feature::SELECT_ALL, 0, 0);
        let command = match rng.below(4) {
            0 => Command::Translate { dx: 1.5, dy: -0.5 },
            1 => Command::Rotate {
                degrees: 15.0,
                pivot: (0.0, 0.0),
            },
            2 => Command::Duplicate { dx: 1.0, dy: 1.0 },
            _ => Command::Delete,
        };
        let before = h.view();
        if h.apply(&command) == 0 {
            continue;
        }
        applied += 1;
        let after = h.view();
        h.call(feature::UNDO, 0, 0);
        assert_eq!(h.view(), before, "undo through the tables");
        h.call(feature::REDO, 0, 0);
        assert_eq!(h.view(), after, "redo through the tables");
        h.call(feature::UNDO, 0, 0);
    }
    assert!(applied > 300, "applied {applied}");
    eprintln!(
        "evidence: editor property (module tables) — {applied} edits landed over 600 \
         iterations, zero undo/redo divergences"
    );
}

#[test]
fn the_module_entry_point_sees_the_same_session() {
    // `abi::call` is what the exported `editor_call` forwards to; a load through
    // it must be visible to the exports' own table accessors.
    let blob = encode_doc(&fixture_doc());
    assert!(abi::put_input(&blob, 0));
    assert_eq!(abi::call(feature::DOC_LOAD, 0, 0), 3);
    assert_eq!(abi::call(feature::NODE_COUNT, 0, 0), 3);
    assert_eq!(abi::last_error(), abi::ERR_NONE);
    assert!(abi::revision() > 0);
    // Ids are never 0, which is what makes the zero terminator unambiguous.
    let count = abi::call(feature::SELECT_ALL, 0, 0);
    assert_eq!(count, 3);
    assert_eq!(abi::call(feature::SELECTION_IDS, 0, 0), 3);
    assert_eq!(abi::get_output(4), vec![1, 2, 3, 0]);
    assert_eq!(abi::call(feature::CLOSE, 0, 0), 0);
    assert_eq!(abi::call(feature::NODE_COUNT, 0, 0), 0);
    assert_eq!(abi::last_error(), ERR_NO_DOCUMENT);
}
