//! The binary geometry format that crosses the ABI boundary.
//!
//! Everything is a `u32` word; floats cross as `f32::to_bits` so the editor's
//! stored bits travel unchanged. Both directions are total: a truncated or
//! nonsensical blob is rejected with [`crate::abi::ERR_BAD_ARGUMENT`] instead of
//! panicking (a panic inside a `wasm32` instance would trap the module and lose
//! the user's unsaved edits).
//!
//! ## Document blob
//!
//! ```text
//! word 0   canvas width   (f32 bits)
//! word 1   canvas height  (f32 bits)
//! word 2   node count
//! word 3   total words including this header
//! then, per node:
//!          id, m0..m5, fill (0xRRGGBBAA), flags (bit0 = visible), path words
//!          followed by that node's path blob
//! ```
//!
//! ## Path blob
//!
//! ```text
//! word 0   subpath count
//! then, per subpath:
//!          start_x, start_y, closed, segment count,
//!          then per segment: kind (0 = line, 1 = cubic), c1_x, c1_y, c2_x,
//!          c2_y, to_x, to_y
//! ```
//!
//! A line stores its end point in all three point slots, so a path round-trips
//! bit-for-bit regardless of how the geometry was authored.

use isg_core::editor::{Affine, Doc, Node, NodeId, Point, Seg, Subpath};

use crate::abi::ERR_BAD_ARGUMENT;

/// Words per segment record.
pub const SEG_WORDS: usize = 7;
/// Words per subpath header (start + closed + segment count).
pub const SUBPATH_HEADER_WORDS: usize = 4;
/// Words per node header (id, six transform entries, fill, flags, path length)
/// — the path blob follows it.
pub const NODE_HEADER_WORDS: usize = 10;
/// Words in the document header.
pub const DOC_HEADER_WORDS: usize = 4;
/// Segment kind: a straight line.
pub const KIND_LINE: u32 = 0;
/// Segment kind: a cubic Bézier.
pub const KIND_CUBIC: u32 = 1;

fn bits(v: f32) -> u32 {
    v.to_bits()
}

fn unbind(w: u32) -> f32 {
    f32::from_bits(w)
}

fn push_point(out: &mut Vec<u32>, p: Point) {
    out.push(bits(p.x));
    out.push(bits(p.y));
}

/// Encodes a path (a node's local geometry) as a path blob.
#[must_use]
pub fn encode_path(path: &[Subpath]) -> Vec<u32> {
    let mut out = Vec::with_capacity(
        1 + path.len() * (SUBPATH_HEADER_WORDS + 4)
            + path.iter().map(|s| s.segs.len() * SEG_WORDS).sum::<usize>(),
    );
    out.push(path.len() as u32);
    for sub in path {
        push_point(&mut out, sub.start);
        out.push(u32::from(sub.closed));
        out.push(sub.segs.len() as u32);
        for seg in &sub.segs {
            match seg {
                Seg::Line(to) => {
                    out.push(KIND_LINE);
                    push_point(&mut out, *to);
                    push_point(&mut out, *to);
                    push_point(&mut out, *to);
                }
                Seg::Cubic { c1, c2, to } => {
                    out.push(KIND_CUBIC);
                    push_point(&mut out, *c1);
                    push_point(&mut out, *c2);
                    push_point(&mut out, *to);
                }
            }
        }
    }
    out
}

/// Decodes a path blob; `consumed` receives the number of words read.
///
/// # Errors
///
/// Returns [`ERR_BAD_ARGUMENT`] when the blob is truncated or a subpath/segment
/// count does not match the words present.
pub fn decode_path(words: &[u32], at: usize, consumed: &mut usize) -> Result<Vec<Subpath>, u32> {
    let start = at;
    let sub_count = *words.get(at).ok_or(ERR_BAD_ARGUMENT)? as usize;
    let mut cursor = at + 1;
    let mut path = Vec::with_capacity(sub_count.min(1024));
    for _ in 0..sub_count {
        if words.len() < cursor + SUBPATH_HEADER_WORDS {
            return Err(ERR_BAD_ARGUMENT);
        }
        let start_point = Point::new(unbind(words[cursor]), unbind(words[cursor + 1]));
        let closed = words[cursor + 2] != 0;
        let seg_count = words[cursor + 3] as usize;
        cursor += SUBPATH_HEADER_WORDS;
        if words.len() < cursor + seg_count * SEG_WORDS {
            return Err(ERR_BAD_ARGUMENT);
        }
        let mut sub = Subpath::new(start_point);
        sub.closed = closed;
        for _ in 0..seg_count {
            let kind = words[cursor];
            let p = |i: usize| Point::new(unbind(words[cursor + i]), unbind(words[cursor + i + 1]));
            match kind {
                KIND_LINE => sub.segs.push(Seg::Line(p(5))),
                KIND_CUBIC => sub.segs.push(Seg::Cubic {
                    c1: p(1),
                    c2: p(3),
                    to: p(5),
                }),
                _ => return Err(ERR_BAD_ARGUMENT),
            }
            cursor += SEG_WORDS;
        }
        path.push(sub);
    }
    *consumed = cursor - start;
    Ok(path)
}

/// Encodes a whole document, including its canvas size.
#[must_use]
pub fn encode_doc(doc: &Doc) -> Vec<u32> {
    let mut out = Vec::with_capacity(DOC_HEADER_WORDS + doc.node_count() * (NODE_HEADER_WORDS + 8));
    out.push(bits(doc.width()));
    out.push(bits(doc.height()));
    out.push(doc.node_count() as u32);
    out.push(0); // patched with the total length below
    for node in doc.nodes() {
        let path = encode_path(&node.path);
        out.push(node.id.get());
        for m in node.transform.to_array() {
            out.push(bits(m));
        }
        out.push(
            (u32::from(node.fill[0]) << 24)
                | (u32::from(node.fill[1]) << 16)
                | (u32::from(node.fill[2]) << 8)
                | u32::from(node.fill[3]),
        );
        out.push(u32::from(node.visible));
        out.push(path.len() as u32);
        out.extend_from_slice(&path);
    }
    let total = out.len() as u32;
    out[3] = total;
    out
}

/// Decodes a document blob.
///
/// # Errors
///
/// Returns [`ERR_BAD_ARGUMENT`] for a truncated blob, a node count that does not
/// match the words present, or a path that fails [`decode_path`].
pub fn decode_doc(words: &[u32]) -> Result<Doc, u32> {
    if words.len() < DOC_HEADER_WORDS {
        return Err(ERR_BAD_ARGUMENT);
    }
    let width = unbind(words[0]);
    let height = unbind(words[1]);
    // A document without a positive, finite canvas cannot be hit-tested or
    // drawn, so it is malformed rather than merely empty.
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(ERR_BAD_ARGUMENT);
    }
    let node_count = words[2] as usize;
    let mut doc = Doc::new(width, height);
    let mut cursor = DOC_HEADER_WORDS;
    for _ in 0..node_count {
        if words.len() < cursor + NODE_HEADER_WORDS {
            return Err(ERR_BAD_ARGUMENT);
        }
        let id = NodeId::new(words[cursor]);
        let mut m = [0.0f32; 6];
        for (i, slot) in m.iter_mut().enumerate() {
            *slot = unbind(words[cursor + 1 + i]);
        }
        let fill_word = words[cursor + 7];
        let fill = [
            (fill_word >> 24) as u8,
            (fill_word >> 16) as u8,
            (fill_word >> 8) as u8,
            fill_word as u8,
        ];
        let visible = words[cursor + 8] != 0;
        let path_words = words[cursor + 9] as usize;
        cursor += NODE_HEADER_WORDS;
        let mut consumed = 0;
        let path = decode_path(words, cursor, &mut consumed)?;
        // The declared path length must agree with what was parsed, otherwise
        // the blob is describing two different documents at once.
        if consumed != path_words {
            return Err(ERR_BAD_ARGUMENT);
        }
        cursor += consumed;
        let mut node = Node::new(id, path, fill);
        node.transform = Affine::new(m);
        node.visible = visible;
        doc.insert_at(doc.node_count(), node);
    }
    Ok(doc)
}

/// Writes one node's header plus path into `out` (the `NODE_SYNC` record).
pub fn write_node_record(out: &mut [u32], node: &Node) -> Option<usize> {
    let path = encode_path(&node.path);
    if out.len() < NODE_HEADER_WORDS + path.len() {
        return None;
    }
    out[0] = node.id.get();
    for (i, m) in node.transform.to_array().iter().enumerate() {
        out[1 + i] = bits(*m);
    }
    out[7] = (u32::from(node.fill[0]) << 24)
        | (u32::from(node.fill[1]) << 16)
        | (u32::from(node.fill[2]) << 8)
        | u32::from(node.fill[3]);
    out[8] = u32::from(node.visible);
    out[9] = path.len() as u32;
    out[NODE_HEADER_WORDS..NODE_HEADER_WORDS + path.len()].copy_from_slice(&path);
    Some(NODE_HEADER_WORDS + path.len())
}
