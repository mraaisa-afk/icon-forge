//! §3.3-③ Clean — `median(3) → open(3) → close(3|5)` on the foreground mask.
//!
//! The frozen [`ForegroundMask`] packs rows *without* word padding, so every
//! pass bounces through a private row-padded scratch buffer
//! (`stride = ceil(w/64)` words per row) where 3×3 square-kernel morphology
//! becomes cheap word-parallel shifts. Out-of-image pixels count as
//! background, so kernels erode a 1-px frame at sheet borders — standard
//! morphology semantics, deterministic.
//!
//! `median(3)` is a majority filter (≥5 of 9 including the centre): it
//! removes JPEG salt-and-pepper and fills single-pixel pinholes. Note it
//! also cuts convex corners of solid shapes by one pixel; the subsequent
//! `open(3)` restores stroke geometry, and `close(3)` bridges 1–2 px gaps.
//! `close_passes = 2` (weak JPEG) means two dilate rounds then two erode
//! rounds — an effective 5×5 close.

use isg_core::{ForegroundMask, RleRun};

/// Row-padded bit matrix (scratch layout for morphology passes).
#[derive(Clone)]
struct PackedRows {
    w: u32,
    h: u32,
    stride: usize,
    words: Vec<u64>,
}

impl PackedRows {
    fn from_mask(mask: &ForegroundMask) -> Self {
        let (w, h) = (mask.width(), mask.height());
        let stride = w.div_ceil(64) as usize;
        let mut words = vec![0u64; stride * h as usize];
        for run in mask.runs() {
            let row_base = run.y as usize * stride;
            let mut x = run.x_start as usize;
            while x < run.x_end as usize {
                let wi = x / 64;
                let lo = x % 64;
                let take = (64 - lo).min(run.x_end as usize - x);
                let m = if take == 64 {
                    u64::MAX
                } else {
                    ((1u64 << take) - 1) << lo
                };
                words[row_base + wi] |= m;
                x += take;
            }
        }
        Self {
            w,
            h,
            stride,
            words,
        }
    }

    fn to_mask(&self) -> ForegroundMask {
        let mut runs = Vec::new();
        let last_word_valid = if self.w.is_multiple_of(64) {
            u64::MAX
        } else {
            (1u64 << (self.w % 64)) - 1
        };
        for y in 0..self.h as usize {
            let row = &self.words[y * self.stride..(y + 1) * self.stride];
            let mut run_start: Option<u32> = None;
            let mut prev_x: u32 = 0;
            for (wi, &word) in row.iter().enumerate() {
                let valid = if wi + 1 == self.stride {
                    word & last_word_valid
                } else {
                    word
                };
                let base = (wi * 64) as u32;
                let mut rest = valid;
                while rest != 0 {
                    let bit = rest.trailing_zeros();
                    let x = base + bit;
                    match run_start {
                        Some(_) if x == prev_x + 1 => {}
                        Some(s) => {
                            runs.push(RleRun {
                                y: y as u32,
                                x_start: s,
                                x_end: prev_x + 1,
                            });
                            run_start = Some(x);
                        }
                        None => run_start = Some(x),
                    }
                    prev_x = x;
                    rest &= rest - 1;
                }
            }
            if let Some(s) = run_start {
                runs.push(RleRun {
                    y: y as u32,
                    x_start: s,
                    x_end: prev_x + 1,
                });
            }
        }
        ForegroundMask::from_runs(self.w, self.h, &runs)
    }

    /// Value of pixel `(x, y)`; out-of-image reads as background (false).
    fn bit(&self, x: i64, y: i64) -> bool {
        if x < 0 || y < 0 || x >= i64::from(self.w) || y >= i64::from(self.h) {
            return false;
        }
        let idx = y as usize * self.stride + x as usize / 64;
        (self.words[idx] >> (x as u64 % 64)) & 1 == 1
    }

    fn row(&self, y: usize) -> &[u64] {
        &self.words[y * self.stride..(y + 1) * self.stride]
    }
}

/// 3×3 square-kernel morphology of one output row: the 3-wide horizontal
/// kernel (with cross-word carries) is applied to rows `y-1`, `y`, `y+1`
/// separately, then folded with OR (dilate) / AND (erode). Missing rows
/// count as background, so erosion kills the border frame.
fn morph_row(p: &PackedRows, y: usize, dilate: bool) -> Vec<u64> {
    let kernel = |row: &[u64]| {
        row.iter()
            .enumerate()
            .map(|(i, &w)| {
                let l = (w << 1) | if i > 0 { row[i - 1] >> 63 } else { 0 };
                let r = (w >> 1)
                    | if i + 1 < row.len() {
                        row[i + 1] << 63
                    } else {
                        0
                    };
                if dilate {
                    w | l | r
                } else {
                    w & l & r
                }
            })
            .collect::<Vec<u64>>()
    };
    let mut v = kernel(p.row(y));
    for dy in [-1i64, 1] {
        let oy = y as i64 + dy;
        if oy < 0 || oy >= i64::from(p.h) {
            if !dilate {
                for v in &mut v {
                    *v = 0;
                }
            }
            continue;
        }
        let b = kernel(p.row(oy as usize));
        for (v, &b) in v.iter_mut().zip(b.iter()) {
            if dilate {
                *v |= b;
            } else {
                *v &= b;
            }
        }
    }
    v
}

fn morph(p: &PackedRows, dilate: bool) -> PackedRows {
    let mut words = Vec::with_capacity(p.words.len());
    for y in 0..p.h as usize {
        words.extend_from_slice(&morph_row(p, y, dilate));
    }
    PackedRows {
        w: p.w,
        h: p.h,
        stride: p.stride,
        words,
    }
}

/// 3×3 square dilation.
#[must_use]
pub fn dilate3(mask: &ForegroundMask) -> ForegroundMask {
    morph(&PackedRows::from_mask(mask), true).to_mask()
}

/// 3×3 square erosion (out-of-image = background).
#[must_use]
pub fn erode3(mask: &ForegroundMask) -> ForegroundMask {
    morph(&PackedRows::from_mask(mask), false).to_mask()
}

/// 3×3 majority filter: a pixel is ink when ≥5 of its 3×3 neighbourhood
/// (including itself; out-of-image = background) are ink.
#[must_use]
pub fn median3(mask: &ForegroundMask) -> ForegroundMask {
    let p = PackedRows::from_mask(mask);
    let mut out = PackedRows {
        w: p.w,
        h: p.h,
        stride: p.stride,
        words: vec![0u64; p.words.len()],
    };
    for y in 0..i64::from(p.h) {
        for x in 0..i64::from(p.w) {
            let mut n = 0u8;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if p.bit(x + dx, y + dy) {
                        n += 1;
                    }
                }
            }
            if n >= 5 {
                let idx = y as usize * p.stride + x as usize / 64;
                out.words[idx] |= 1u64 << (x as u64 % 64);
            }
        }
    }
    out.to_mask()
}

/// `erode3` then `dilate3` — removes isolated pixels and thin spurs.
#[must_use]
pub fn open3(mask: &ForegroundMask) -> ForegroundMask {
    dilate3(&erode3(mask))
}

/// `dilate3` × `passes` then `erode3` × `passes` — bridges 1–2 px gaps
/// (`passes = 2` ≈ 5×5 close for weak JPEG sources).
#[must_use]
pub fn close(mask: &ForegroundMask, passes: u8) -> ForegroundMask {
    let mut p = PackedRows::from_mask(mask);
    for _ in 0..passes {
        p = morph(&p, true);
    }
    for _ in 0..passes {
        p = morph(&p, false);
    }
    p.to_mask()
}

/// Single 3×3 close — the default gap-bridge pass.
#[must_use]
pub fn close3(mask: &ForegroundMask) -> ForegroundMask {
    close(mask, 1)
}

/// §3.3-③ full clean pass: `median(3) → open(3) → close(passes)`.
#[must_use]
pub fn clean(mask: &ForegroundMask, close_passes: u8) -> ForegroundMask {
    close(&open3(&median3(mask)), close_passes.min(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a mask from `(x, y)` pixel lists.
    fn mask(w: u32, h: u32, px: &[(u32, u32)]) -> ForegroundMask {
        let mut m = ForegroundMask::new(w, h);
        for &(x, y) in px {
            m.set(x, y, true);
        }
        m
    }

    fn runs_of(m: &ForegroundMask) -> Vec<(u32, u32, u32)> {
        m.runs().iter().map(|r| (r.y, r.x_start, r.x_end)).collect()
    }

    #[test]
    fn dilate3_single_pixel_becomes_3x3_block() {
        let m = mask(7, 7, &[(3, 3)]);
        let d = dilate3(&m);
        assert_eq!(d.ink_count(), 9);
        assert_eq!(runs_of(&d), vec![(2, 2, 5), (3, 2, 5), (4, 2, 5)]);
    }

    #[test]
    fn erode3_of_3x3_block_returns_center() {
        let m = mask(
            7,
            7,
            &[
                (2, 2),
                (3, 2),
                (4, 2),
                (2, 3),
                (3, 3),
                (4, 3),
                (2, 4),
                (3, 4),
                (4, 4),
            ],
        );
        let e = erode3(&m);
        assert_eq!(runs_of(&e), vec![(3, 3, 4)]);
    }

    #[test]
    fn morphology_respects_word_boundaries() {
        // Width 100 → stride 2; runs crossing the 63|64 word boundary and
        // touching the right edge (padding bits must never leak into
        // results). Three ink rows so erosion has vertical support.
        let mut m = ForegroundMask::new(100, 5);
        for y in 1..=3u32 {
            m.set(63, y, true);
            m.set(64, y, true);
            for x in 95..100 {
                m.set(x, y, true);
            }
        }
        let d = dilate3(&m);
        assert!(d.get(62, 2) && d.get(65, 2) && d.get(94, 2) && d.get(99, 2));
        assert!(!d.get(61, 2), "dilation stays within ±1");
        let e = erode3(&d);
        // d is a 5-row band (rows 0..4) of {62..65} ∪ {94..99}; erosion of a
        // band leaves rows 1..=3 with the x-kernel core {63,64} ∪ {95..98}.
        assert!(!e.get(62, 2) && !e.get(65, 2), "edge pixels erode away");
        assert!(e.get(63, 2) && e.get(64, 2));
        assert!(e.get(95, 2) && e.get(96, 2) && e.get(97, 2) && e.get(98, 2));
        assert!(e.get(63, 1) && e.get(98, 3), "inner rows survive too");
        assert!(!e.get(63, 0) && !e.get(63, 4), "outer rows erode away");
        assert_eq!(e.ink_count(), 18);
    }

    #[test]
    fn median3_removes_isolated_and_cuts_corners() {
        // Single pixel → gone.
        let m = mask(7, 7, &[(3, 3)]);
        assert_eq!(median3(&m).ink_count(), 0);
        // Solid 3×3 → corners cut (4/9), edge mids and centre stay (6–9/9).
        let m = mask(
            7,
            7,
            &[
                (2, 2),
                (3, 2),
                (4, 2),
                (2, 3),
                (3, 3),
                (4, 3),
                (2, 4),
                (3, 4),
                (4, 4),
            ],
        );
        let md = median3(&m);
        assert_eq!(md.ink_count(), 5, "plus shape");
        assert!(md.get(3, 2) && md.get(2, 3) && md.get(3, 3) && md.get(4, 3) && md.get(3, 4));
        assert!(!md.get(2, 2) && !md.get(4, 4));
    }

    #[test]
    fn open3_removes_isolated_pixel_keeps_block() {
        let mut px = vec![(0u32, 0u32)];
        for y in 4..7 {
            for x in 4..7 {
                px.push((x, y));
            }
        }
        let m = mask(9, 9, &px);
        let o = open3(&m);
        assert!(!o.get(0, 0), "isolated pixel removed");
        assert_eq!(o.ink_count(), 9, "block restored");
        assert_eq!(runs_of(&o), vec![(4, 4, 7), (5, 4, 7), (6, 4, 7)]);
    }

    #[test]
    fn close3_bridges_two_pixel_gap() {
        let mut px = Vec::new();
        for y in 0..2 {
            for x in 0..2 {
                px.push((x, y));
            }
            for x in 4..6 {
                px.push((x, y));
            }
        }
        let m = mask(8, 4, &px);
        let c = close3(&m);
        assert!(c.get(2, 1) && c.get(3, 1), "gap bridged");
        // Dilated blocks merge to cols 0..=6 (rows 0..2); the erosion core
        // is a single row, cols 1..=5.
        assert_eq!(c.ink_count(), 5);
        assert_eq!(runs_of(&c), vec![(1, 1, 6)]);
    }

    #[test]
    fn close_passes_2_uses_wider_kernel() {
        // Two 3-row-tall blocks 3 apart (gap cols 2..5): close(3) cannot
        // bridge, the double-dilate of close(5) can. (Blocks must be ≥3
        // rows tall or the second erosion annihilates the thin core.)
        let mut px = Vec::new();
        for y in 0..3 {
            for x in 0..2 {
                px.push((x, y));
            }
            for x in 5..7 {
                px.push((x, y));
            }
        }
        let m = mask(9, 5, &px);
        assert!(!close(&m, 1).get(3, 1), "3×3 close cannot bridge 3-gap");
        assert!(
            close(&m, 2).get(3, 2) && close(&m, 2).get(4, 2),
            "5×5 close bridges"
        );
    }

    #[test]
    fn clean_end_to_end_denoises_and_bridges() {
        // 6×6 block at (2..8)² + two isolated noise pixels + a 2-px split.
        let mut px = Vec::new();
        for y in 2..8 {
            for x in 2..8 {
                px.push((x, y));
            }
        }
        let block_only = mask(16, 16, &px);
        let cleaned = clean(&block_only, 1);
        // Core intact, no phantom growth far away, sane total (16..49).
        assert!(cleaned.get(4, 4) && cleaned.get(5, 5));
        assert!(!cleaned.get(0, 0) && !cleaned.get(15, 15));
        assert!(
            (16..=49).contains(&cleaned.ink_count()),
            "{}",
            cleaned.ink_count()
        );

        px.push((12, 12));
        px.push((14, 14));
        let speckled = mask(16, 16, &px);
        let cleaned = clean(&speckled, 1);
        assert!(!cleaned.get(12, 12), "salt removed");
        assert!(!cleaned.get(14, 14), "pepper removed");
        assert!(cleaned.get(4, 4), "block survives");
    }

    #[test]
    fn empty_mask_stays_empty() {
        let m = ForegroundMask::new(70, 5);
        assert_eq!(clean(&m, 1).ink_count(), 0);
        assert_eq!(dilate3(&m).ink_count(), 0);
    }

    #[test]
    fn full_mask_roundtrips_through_clean() {
        // Solid 3×5 block (fills the 5×3 image): median cuts the 4 corners,
        // open restores the block, close re-cores it to row 1, cols 1..=3.
        let mut px = Vec::new();
        for y in 0..3 {
            for x in 0..5 {
                px.push((x, y));
            }
        }
        let m = mask(5, 3, &px);
        let c = clean(&m, 1);
        assert_eq!(runs_of(&c), vec![(1, 1, 4)]);
        assert_eq!(c.ink_count(), 3);
    }
}
