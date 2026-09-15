//! The foreground mask representation — **bit-packed 1-bpp storage with
//! run-length-encoded row extraction** (ARCHITECTURE.md §3.4).
//!
//! Storage is a plain `Vec<u64>` (64 pixels per word, row-major, no per-row
//! padding): a 4096×4096 sheet costs 2 MiB instead of the 16 MiB a
//! `Vec<bool>` costs and 64 MiB an RGBA8 buffer costs. [`ForegroundMask::runs`]
//! extracts `{y, x_start, x_end}` rows — the representation every downstream
//! grouping stage is mandated to consume.
//!
//! This module is part of the **frozen** `isg-core` surface (Phase 1).

/// One contiguous foreground run on a single raster row (half-open:
/// `x_end` is exclusive, so a run covers `x in x_start..x_end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RleRun {
    /// Row index (top-left origin, y down).
    pub y: u32,
    /// First foreground column of the run.
    pub x_start: u32,
    /// One past the last foreground column of the run.
    pub x_end: u32,
}

impl RleRun {
    /// Number of pixels covered by this run.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.x_end.saturating_sub(self.x_start)
    }

    /// True when the run covers no pixels.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.x_end <= self.x_start
    }
}

/// Bit-packed 1-bpp foreground mask over a `width × height` pixel grid.
///
/// * Layout: row-major, 64 pixels per `u64` word, no per-row padding —
///   pixel `(x, y)` is bit `y*width + x`.
/// * Padding bits beyond `width*height` in the last word are always `0`.
/// * Ink count is maintained incrementally (`O(1)` [`ForegroundMask::ink_count`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForegroundMask {
    w: u32,
    h: u32,
    bits: Vec<u64>,
    ink: u64,
}

impl ForegroundMask {
    /// Creates an all-background mask of `width × height` pixels.
    ///
    /// # Panics
    /// Panics when `width * height` overflows `usize`.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let n = width as usize * height as usize;
        Self {
            w: width,
            h: height,
            bits: vec![0; n.div_ceil(64)],
            ink: 0,
        }
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.w
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.h
    }

    /// Total pixel count (`width * height`).
    #[must_use]
    pub const fn pixel_count(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    /// Number of foreground (ink) pixels.
    #[must_use]
    pub const fn ink_count(&self) -> u64 {
        self.ink
    }

    /// Raw bit words (row-major, 64 pixels per word). Exposed for adapters
    /// that copy masks in bulk; the bit order is documented on the type.
    #[must_use]
    pub fn words(&self) -> &[u64] {
        &self.bits
    }

    /// Value of pixel `(x, y)`.
    ///
    /// # Panics
    /// Panics when `(x, y)` is outside the mask (debug builds).
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> bool {
        self.debug_check(x, y);
        self.get_index(self.linear(x, y))
    }

    /// Value of the pixel at linear index `i` (= `y * width + x`).
    ///
    /// # Panics
    /// Panics when `i` is outside the mask (debug builds).
    #[must_use]
    pub fn get_index(&self, i: usize) -> bool {
        debug_assert!(
            i < self.pixel_count() as usize,
            "mask index {i} out of range"
        );
        self.bits[i >> 6] & (1u64 << (i & 63)) != 0
    }

    /// Sets pixel `(x, y)`.
    ///
    /// # Panics
    /// Panics when `(x, y)` is outside the mask (debug builds).
    pub fn set(&mut self, x: u32, y: u32, value: bool) {
        self.debug_check(x, y);
        self.set_index(self.linear(x, y), value);
    }

    /// Sets the pixel at linear index `i` (= `y * width + x`).
    ///
    /// # Panics
    /// Panics when `i` is outside the mask (debug builds).
    pub fn set_index(&mut self, i: usize, value: bool) {
        debug_assert!(
            i < self.pixel_count() as usize,
            "mask index {i} out of range"
        );
        let bit = 1u64 << (i & 63);
        let word = &mut self.bits[i >> 6];
        if value {
            if *word & bit == 0 {
                *word |= bit;
                self.ink += 1;
            }
        } else if *word & bit != 0 {
            *word &= !bit;
            self.ink -= 1;
        }
    }

    /// Foreground runs, row-major (`y` ascending, then `x_start` ascending).
    ///
    /// Zero words are skipped in bulk, so a 5 %-ink 4096² sheet extracts in a
    /// few hundred microseconds; the output is exactly the representation
    /// ARCHITECTURE.md §3.4 mandates for all downstream grouping stages.
    #[must_use]
    pub fn runs(&self) -> Vec<RleRun> {
        let mut out = Vec::new();
        for y in 0..self.h {
            let row_start = y as usize * self.w as usize;
            let row_end = row_start + self.w as usize;
            let mut x = row_start;
            while x < row_end {
                // Bulk-skip fully-background words.
                if x % 64 == 0 && self.bits[x >> 6] == 0 {
                    x += 64;
                    continue;
                }
                if !self.get_index(x) {
                    x += 1;
                    continue;
                }
                let run_start = x;
                while x < row_end && self.get_index(x) {
                    x += 1;
                }
                out.push(RleRun {
                    y,
                    x_start: (run_start - row_start) as u32,
                    x_end: (x - row_start) as u32,
                });
            }
        }
        out
    }

    /// Builds a mask from row runs (the inverse of [`ForegroundMask::runs`]).
    ///
    /// Runs may arrive in any order; duplicate/overlapping runs are idempotent
    /// (`set` is). Runs outside the grid are debug-asserted and clamped.
    #[must_use]
    pub fn from_runs(width: u32, height: u32, runs: &[RleRun]) -> Self {
        let mut mask = Self::new(width, height);
        for r in runs {
            if r.y >= height {
                continue;
            }
            let start = r.x_start.min(width);
            let end = r.x_end.min(width);
            let row = r.y as usize * width as usize;
            for x in start..end {
                mask.set_index(row + x as usize, true);
            }
        }
        mask
    }

    const fn linear(&self, x: u32, y: u32) -> usize {
        y as usize * self.w as usize + x as usize
    }

    // Not `const`: debug_assert! with format arguments is not
    // const-compatible, and this only runs in debug builds anyway.
    fn debug_check(&self, x: u32, y: u32) {
        debug_assert!(x < self.w, "mask x {x} out of range (width {})", self.w);
        debug_assert!(y < self.h, "mask y {y} out of range (height {})", self.h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_get_set_roundtrip() {
        let mut m = ForegroundMask::new(67, 3); // not a multiple of 64
        assert_eq!(m.pixel_count(), 67 * 3);
        assert_eq!(m.ink_count(), 0);
        assert!(!m.get(0, 0));
        m.set(0, 0, true);
        m.set(66, 2, true); // very last pixel (padding-adjacent)
        m.set(33, 1, true);
        assert!(m.get(0, 0));
        assert!(m.get(33, 1));
        assert!(m.get(66, 2));
        assert!(!m.get(65, 2));
        assert_eq!(m.ink_count(), 3);
        // Idempotent sets do not change ink.
        m.set(0, 0, true);
        m.set(1, 1, false);
        assert_eq!(m.ink_count(), 3);
        // Clearing works.
        m.set(0, 0, false);
        assert!(!m.get(0, 0));
        assert_eq!(m.ink_count(), 2);
    }

    #[test]
    fn set_index_matches_set() {
        let mut a = ForegroundMask::new(20, 5);
        let mut b = ForegroundMask::new(20, 5);
        a.set(13, 4, true);
        b.set_index(4 * 20 + 13, true);
        assert_eq!(a, b);
        assert!(b.get_index(4 * 20 + 13));
    }

    #[test]
    fn runs_roundtrip() {
        let runs = [
            RleRun {
                y: 1,
                x_start: 3,
                x_end: 8,
            },
            RleRun {
                y: 1,
                x_start: 10,
                x_end: 11,
            },
            RleRun {
                y: 4,
                x_start: 0,
                x_end: 64, // exactly one full word
            },
            RleRun {
                y: 7,
                x_start: 130,
                x_end: 200, // spans words, ends mid-word
            },
        ];
        let m = ForegroundMask::from_runs(256, 10, &runs);
        assert_eq!(m.ink_count(), 5 + 1 + 64 + 70);
        assert_eq!(m.runs(), runs.to_vec(), "roundtrip must be lossless");
    }

    #[test]
    fn runs_skip_zero_words_in_bulk() {
        // 4096-wide row with ink only in the last 8 columns.
        let mut m = ForegroundMask::new(4096, 1);
        for x in 4088..4096 {
            m.set(x, 0, true);
        }
        let r = m.runs();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].y, 0);
        assert_eq!(r[0].x_start, 4088);
        assert_eq!(r[0].x_end, 4096);
    }

    #[test]
    fn five_percent_ink_4096_sheet_is_two_mib() {
        // The §3.4 memory claim: bit-packed storage for a 4096² sheet.
        let m = ForegroundMask::new(4096, 4096);
        assert_eq!(m.words().len(), 4096 * 4096 / 64);
        assert_eq!(m.words().len() * 8, 2 * 1024 * 1024); // 2 MiB of words
        assert_eq!(m.ink_count(), 0);
    }

    #[test]
    fn from_runs_clamps_out_of_range() {
        let runs = [
            RleRun {
                y: 0,
                x_start: 10,
                x_end: 50,
            },
            RleRun {
                y: 9,
                x_start: 0,
                x_end: 5,
            }, // y beyond grid
            RleRun {
                y: 1,
                x_start: 14,
                x_end: 90,
            }, // clamped to width
        ];
        let m = ForegroundMask::from_runs(20, 2, &runs);
        // Run 1 clamps 10..50 -> 10 px on row 0; run 3 clamps 14..90 -> 6 px
        // on row 1; run 2 (y = 9 >= height) is skipped entirely.
        assert_eq!(m.ink_count(), 10 + 6);
        assert!(!m.get(9, 0)); // x_start boundary respected
        assert!(m.get(10, 0));
        assert!(m.get(19, 0)); // x_end clamped to width
        assert!(m.get(19, 1));
        assert!(!m.get(0, 0)); // the skipped y=9 run did not land on row 0
        assert!(!m.get(0, 1)); // run 3 starts at x = 14, not 0
    }
}
