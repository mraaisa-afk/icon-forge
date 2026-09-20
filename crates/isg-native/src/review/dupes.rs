//! §3.6 detector 2 — the duplicate cascade.
//!
//! Three stages, cheapest first, and **each stage may only narrow**:
//!
//! 1. **Propose** — `dHash` (a 9×8 horizontal-gradient hash) and `aHash` (an
//!    8×8 average hash) over a normalised 64×64 luma render, banded into four
//!    16-bit LSH buckets per hash. Candidate pairs come out of shared buckets,
//!    which is `O(n)` in the number of icons instead of the `O(n²)` every-pair
//!    comparison a sheet of thousands would not survive.
//! 2. **Verify** — ink IoU ≥ `0.92` **or** normalised Hausdorff ≤ `0.02`
//!    (§3.6). The two catch different things: IoU is unforgiving about area,
//!    Hausdorff about a single wandering edge, and a duplicate only has to
//!    satisfy one of them.
//! 3. **Confirm** — an identical blake3 digest, or SSIM ≥ `0.97`.
//!
//! Nothing is reported as a duplicate on a hash match alone. That is the whole
//! reason the cascade exists, and it is what keeps precision at the level the
//! roadmap asks for rather than the level a 64-bit hash alone would give.
//!
//! # What this module does *not* do
//!
//! It never renders anything. The caller hands in the 64×64 luma plane (the
//! native half has the renderer), the blake3 digest of that plane
//! ([`HashItem::digest`]) and, for confirmation, the SSIM of the pair. Keeping
//! those three as inputs is what lets this file be tested without a corpus, a
//! GPU or an SVG parser — and it is also why the SSIM is not recomputed here:
//! `pipeline::score` owns that metric, and two implementations of SSIM that
//! disagree by a rounding step would be worse than none.

/// Bands per hash for the LSH stage: four 16-bit keys, so two icons that share
/// any 16 consecutive bits of either hash meet in a bucket.
pub const LSH_BANDS: u32 = 4;

/// Nominal size of the normalised render the caller must supply, in pixels.
/// The hash functions themselves take any size (they downscale first), but a
/// fixed input size is what makes one sheet's hashes comparable with another's.
pub const HASH_PLANE: u32 = 64;

/// Ink threshold, shared with the quality scorer: luma ≥ 128 counts as ink.
pub const INK_THRESHOLD: u8 = 128;

/// dHash grid: 9 wide × 8 tall, i.e. one 64-bit hash.
pub const DHASH_W: u32 = 9;
/// dHash grid height.
pub const DHASH_H: u32 = 8;
/// aHash grid: 8 × 8, one 64-bit hash.
pub const AHASH_W: u32 = 8;
/// aHash grid height.
pub const AHASH_H: u32 = 8;

/// The distances a duplicate must satisfy (§3.6), all overridable so a gate
/// can tighten them without editing this file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DupOptions {
    /// Minimum ink IoU for the verify stage.
    pub iou_min: f32,
    /// Maximum normalised Hausdorff distance for the verify stage.
    pub hausdorff_max: f32,
    /// Minimum SSIM for the confirm stage.
    pub ssim_min: f32,
}

impl Default for DupOptions {
    fn default() -> Self {
        Self {
            iou_min: 0.92,
            hausdorff_max: 0.02,
            ssim_min: 0.97,
        }
    }
}

/// Everything the cascade needs to know about one icon.
///
/// `digest` is the blake3 of the normalised plane — the caller owns hashing
/// (blake3 is not a dependency of the pure half). `d` and `a` come from
/// [`d_hash`] and [`a_hash`] over the same plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HashItem {
    /// The plan's id.
    pub id: u32,
    /// dHash of the normalised plane.
    pub d: u64,
    /// aHash of the normalised plane.
    pub a: u64,
    /// blake3 digest of the normalised plane, byte-for-byte.
    pub digest: [u8; 32],
}

/// The numbers behind a verify decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Verified {
    /// Ink IoU of the pair.
    pub iou: f32,
    /// Normalised Hausdorff distance of the pair.
    pub hausdorff: f32,
    /// True when either distance is inside its bound.
    pub pass: bool,
}

/// A set of icons the cascade believes are the same artwork.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DupCluster {
    /// Member ids, ascending.
    pub members: Vec<u32>,
    /// The suggested keeper: the member with the best composite score, ties
    /// broken toward the smaller id so the suggestion is deterministic.
    pub keeper: u32,
}

/// Box-filters `plane` down to `tw` × `th`, averaging every source pixel of
/// each target cell (a partial cell contributes only the pixels it covers).
///
/// The mean is rounded to nearest, and every target cell covers at least one
/// source pixel by construction, so no cell can be a division by zero.
/// `None` when the input length does not match `w` × `h`, or a dimension is
/// zero or larger than the target's source span.
#[must_use]
pub fn downscale_luma(plane: &[u8], w: u32, h: u32, tw: u32, th: u32) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || tw == 0 || th == 0 {
        return None;
    }
    if plane.len() != (w as usize) * (h as usize) {
        return None;
    }
    let mut out = Vec::with_capacity((tw as usize) * (th as usize));
    for ty in 0..th {
        let y0 = (ty * h) / th;
        let y1 = (((ty + 1) * h) / th).max(y0 + 1).min(h);
        for tx in 0..tw {
            let x0 = (tx * w) / tw;
            let x1 = (((tx + 1) * w) / tw).max(x0 + 1).min(w);
            let mut sum = 0u32;
            let mut count = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += u32::from(plane[(y as usize) * (w as usize) + (x as usize)]);
                    count += 1;
                }
            }
            out.push(u8::try_from((sum + count / 2) / count).unwrap_or(255));
        }
    }
    Some(out)
}

/// The 64-bit difference hash of a luma plane: downscale to 9×8, then one bit
/// per horizontal neighbour pair, set when the left pixel is **brighter** than
/// the right one. Bit `y * 8 + x` corresponds to the pair at `(x, y)`.
///
/// The gradient, rather than the value, is what makes the hash survive a
/// global brightness shift — a threshold-per-pixel hash would call a light
/// grey icon and a dark grey icon different.
#[must_use]
pub fn d_hash(plane: &[u8], w: u32, h: u32) -> Option<u64> {
    let small = downscale_luma(plane, w, h, DHASH_W, DHASH_H)?;
    let mut hash = 0u64;
    for y in 0..DHASH_H {
        for x in 0..(DHASH_W - 1) {
            let i = (y as usize) * (DHASH_W as usize) + (x as usize);
            if small[i] > small[i + 1] {
                hash |= 1u64 << (y * 8 + x);
            }
        }
    }
    Some(hash)
}

/// The 64-bit average hash of a luma plane: downscale to 8×8, then one bit per
/// pixel, set when it is brighter than the mean of the 64.
///
/// A perfectly uniform plane hashes to zero — every pixel equals the mean, and
/// the comparison is strict. That is deliberate: two blank icons are equal
/// (their blake3 digests say so) without the average hash pretending to have
/// found structure in them.
#[must_use]
pub fn a_hash(plane: &[u8], w: u32, h: u32) -> Option<u64> {
    let small = downscale_luma(plane, w, h, AHASH_W, AHASH_H)?;
    let sum: u32 = small.iter().map(|v| u32::from(*v)).sum();
    let mean = sum / (AHASH_W * AHASH_H);
    let mut hash = 0u64;
    for (i, value) in small.iter().enumerate() {
        if u32::from(*value) > mean {
            hash |= 1u64 << i;
        }
    }
    Some(hash)
}

/// Splits a hash into `bands` keys of `64 / bands` bits each.
///
/// Returns an empty vector when `bands` is zero or does not divide 64 — the
/// caller learns nothing rather than silently comparing half-keys. Band `i`
/// holds bits `[i·width, (i+1)·width)`.
#[must_use]
pub fn band_keys(hash: u64, bands: u32) -> Vec<(u32, u64)> {
    if bands == 0 || 64 % bands != 0 {
        return Vec::new();
    }
    let width = 64 / bands;
    let mask = if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    (0..bands)
        .map(|i| (i, (hash >> (i * width)) & mask))
        .collect()
}

/// Candidate duplicate pairs from LSH banding, as index pairs into `items`,
/// sorted and deduplicated.
///
/// Each item is inserted into the bucket for every band of both its hashes; a
/// bucket with `k` members contributes its `k·(k−1)/2` pairs. A pair that
/// shares both a dHash band and an aHash band is reported once, not twice.
#[must_use]
pub fn candidate_pairs(items: &[HashItem], bands: u32) -> Vec<(usize, usize)> {
    let mut buckets: std::collections::HashMap<(u8, u32, u64), Vec<usize>> =
        std::collections::HashMap::new();
    for (index, item) in items.iter().enumerate() {
        for (kind, hash) in [(0u8, item.d), (1u8, item.a)] {
            for (band, value) in band_keys(hash, bands) {
                buckets.entry((kind, band, value)).or_default().push(index);
            }
        }
    }
    let mut pairs: std::collections::BTreeSet<(usize, usize)> = std::collections::BTreeSet::new();
    for members in buckets.values() {
        for (i, a) in members.iter().enumerate() {
            for b in members.iter().skip(i + 1) {
                pairs.insert(if a < b { (*a, *b) } else { (*b, *a) });
            }
        }
    }
    pairs.into_iter().collect()
}

/// Ink IoU of two planes: pixels at or above [`INK_THRESHOLD`] on both, over
/// pixels on either.
///
/// Two blank planes are `1.0` — identical, not undefined.
#[must_use]
pub fn ink_iou(a: &[u8], b: &[u8], w: u32, h: u32) -> Option<f32> {
    if w == 0 || h == 0 || a.len() != b.len() || a.len() != (w as usize) * (h as usize) {
        return None;
    }
    let (mut inter, mut union) = (0u64, 0u64);
    for (x, y) in a.iter().zip(b.iter()) {
        let (ai, bi) = (*x >= INK_THRESHOLD, *y >= INK_THRESHOLD);
        if ai && bi {
            inter += 1;
        }
        if ai || bi {
            union += 1;
        }
    }
    Some(if union == 0 {
        1.0
    } else {
        inter as f32 / union as f32
    })
}

/// For every pixel, the distance to the nearest ink pixel of `plane`
/// (0 inside the ink), by two-pass chamfer with 1 / √2 weights.
fn chamfer_distance(plane: &[u8], w: u32, h: u32) -> Vec<f32> {
    const SQRT2: f32 = std::f32::consts::SQRT_2;
    let (w, h) = (w as usize, h as usize);
    // Ink is the source (distance zero); background starts far away and is
    // relaxed toward the nearest ink. Getting this the other way round makes
    // the transform measure distance to the *background*, which silently
    // reports the far edge of every shape as a mismatch.
    let mut dt: Vec<f32> = plane
        .iter()
        .map(|v| {
            if *v >= INK_THRESHOLD {
                0.0
            } else {
                f32::INFINITY
            }
        })
        .collect();
    // Forward pass: north, west and the two diagonals behind.
    for y in 0..h {
        for x in 0..w {
            let index = y * w + x;
            if !dt[index].is_infinite() {
                continue; // already ink, or already relaxed
            }
            let mut best = f32::INFINITY;
            if y > 0 {
                best = best.min(dt[(y - 1) * w + x] + 1.0);
                if x > 0 {
                    best = best.min(dt[(y - 1) * w + x - 1] + SQRT2);
                }
                if x + 1 < w {
                    best = best.min(dt[(y - 1) * w + x + 1] + SQRT2);
                }
            }
            if x > 0 {
                best = best.min(dt[y * w + x - 1] + 1.0);
            }
            dt[index] = best;
        }
    }
    // Backward pass: south, east and the two diagonals ahead.
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let index = y * w + x;
            if !dt[index].is_infinite() {
                continue;
            }
            let mut best = dt[index];
            if y + 1 < h {
                best = best.min(dt[(y + 1) * w + x] + 1.0);
                if x > 0 {
                    best = best.min(dt[(y + 1) * w + x - 1] + SQRT2);
                }
                if x + 1 < w {
                    best = best.min(dt[(y + 1) * w + x + 1] + SQRT2);
                }
            }
            if x + 1 < w {
                best = best.min(dt[y * w + x + 1] + 1.0);
            }
            dt[index] = best;
        }
    }
    dt
}

/// The symmetric Hausdorff distance between two planes' ink, normalised by the
/// plane diagonal so the number is comparable across icon sizes.
///
/// `None` when the sizes do not match, or either plane has no ink at all — the
/// distance from a shape to nothing is not a number a duplicate decision
/// should be made on.
#[must_use]
pub fn hausdorff_normalised(a: &[u8], b: &[u8], w: u32, h: u32) -> Option<f32> {
    if w == 0 || h == 0 || a.len() != b.len() || a.len() != (w as usize) * (h as usize) {
        return None;
    }
    let ink_a: Vec<usize> = a
        .iter()
        .enumerate()
        .filter(|(_, v)| **v >= INK_THRESHOLD)
        .map(|(i, _)| i)
        .collect();
    let ink_b: Vec<usize> = b
        .iter()
        .enumerate()
        .filter(|(_, v)| **v >= INK_THRESHOLD)
        .map(|(i, _)| i)
        .collect();
    if ink_a.is_empty() || ink_b.is_empty() {
        return None;
    }
    let to_b = chamfer_distance(b, w, h);
    let to_a = chamfer_distance(a, w, h);
    let forward = ink_a.iter().map(|i| to_b[*i]).fold(0.0f32, f32::max);
    let backward = ink_b.iter().map(|i| to_a[*i]).fold(0.0f32, f32::max);
    let diagonal = ((w * w + h * h) as f32).sqrt();
    Some(forward.max(backward) / diagonal)
}

/// Runs the verify stage on one candidate pair.
#[must_use]
pub fn verify(a: &[u8], b: &[u8], w: u32, h: u32, options: &DupOptions) -> Option<Verified> {
    let iou = ink_iou(a, b, w, h)?;
    let hausdorff = hausdorff_normalised(a, b, w, h)?;
    Some(Verified {
        iou,
        hausdorff,
        pass: iou >= options.iou_min || hausdorff <= options.hausdorff_max,
    })
}

/// The confirm stage: identical digests, or an SSIM at or above the bound.
///
/// `ssim` is computed by the caller ([`crate::pipeline::score`] owns that
/// metric); it is an argument rather than a call so this module stays pure.
#[must_use]
pub fn confirm(digest_a: &[u8; 32], digest_b: &[u8; 32], ssim: f64, options: &DupOptions) -> bool {
    digest_a == digest_b || ssim >= f64::from(options.ssim_min)
}

/// Groups verified pairs into clusters, each with a suggested keeper.
///
/// `scores` runs parallel to `items` (the composite score of each icon);
/// missing entries count as `0.0`, so a caller that only has the confirmed
/// pairs still gets the lower id as keeper. Clusters of one are not reported —
/// "unique" is not a duplicate decision. The result is sorted by keeper, so
/// the same input always yields the same output.
#[must_use]
pub fn cluster(items: &[HashItem], pairs: &[(usize, usize)], scores: &[f32]) -> Vec<DupCluster> {
    let mut parent: Vec<usize> = (0..items.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for (a, b) in pairs {
        if *a >= items.len() || *b >= items.len() {
            continue;
        }
        let (ra, rb) = (find(&mut parent, *a), find(&mut parent, *b));
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for index in 0..items.len() {
        let root = find(&mut parent, index);
        groups.entry(root).or_default().push(index);
    }
    let mut out: Vec<DupCluster> = Vec::new();
    for members in groups.values() {
        if members.len() < 2 {
            continue;
        }
        let ids: Vec<u32> = members.iter().map(|i| items[*i].id).collect();
        let member_scores: Vec<f32> = members
            .iter()
            .map(|i| scores.get(*i).copied().unwrap_or(0.0))
            .collect();
        let keeper = suggest_keeper(&ids, &member_scores).unwrap_or(ids[0]);
        let mut ids = ids;
        ids.sort_unstable();
        out.push(DupCluster {
            members: ids,
            keeper,
        });
    }
    out.sort_by(|a, b| {
        a.keeper
            .cmp(&b.keeper)
            .then_with(|| a.members.cmp(&b.members))
    });
    out
}

/// The best member of a cluster by score; ties go to the smaller id.
#[must_use]
pub fn suggest_keeper(members: &[u32], scores: &[f32]) -> Option<u32> {
    members
        .iter()
        .enumerate()
        .max_by(|(ia, ida), (ib, idb)| {
            let (sa, sb) = (
                scores.get(*ia).copied().unwrap_or(0.0),
                scores.get(*ib).copied().unwrap_or(0.0),
            );
            sa.partial_cmp(&sb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| idb.cmp(ida))
        })
        .map(|(_, id)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64×64 plane with the left half dark and the right half bright.
    fn half_plane() -> Vec<u8> {
        let mut plane = vec![0u8; 64 * 64];
        for y in 0..64 {
            for x in 0..64 {
                plane[y * 64 + x] = if x >= 32 { 255 } else { 0 };
            }
        }
        plane
    }

    fn item(id: u32, d: u64, a: u64, byte: u8) -> HashItem {
        HashItem {
            id,
            d,
            a,
            digest: [byte; 32],
        }
    }

    #[test]
    fn downscale_averages_each_cell() {
        let plane: Vec<u8> = vec![0, 100, 200, 255];
        let small = downscale_luma(&plane, 4, 1, 2, 1).expect("fits");
        assert_eq!(small, vec![50, 228]); // (100+0)/2, (200+255)/2 rounded
        assert_eq!(downscale_luma(&plane, 3, 1, 2, 1), None, "length mismatch");
        assert_eq!(downscale_luma(&plane, 4, 1, 0, 1), None, "zero target");
    }

    /// A 64×64 plane whose rows fall off smoothly left to right: every
    /// adjacent pair descends, so every dHash bit is set.
    fn descending_plane() -> Vec<u8> {
        let mut plane = vec![0u8; 64 * 64];
        for y in 0..64 {
            for x in 0..64 {
                plane[y * 64 + x] = ((63 - x) * 4) as u8;
            }
        }
        plane
    }

    #[test]
    fn dhash_reads_the_direction_of_the_gradient() {
        // Falling brightness: every bit set (left brighter than right).
        assert_eq!(d_hash(&descending_plane(), 64, 64), Some(u64::MAX));
        // Rising brightness: no bit set, because no pixel is brighter than the
        // one after it. The two planes are mirror images of each other, and a
        // hash that treated them alike could not tell an icon from its flip.
        assert_eq!(d_hash(&half_plane(), 64, 64), Some(0));
    }

    #[test]
    fn dhash_and_ahash_are_zero_for_a_uniform_plane() {
        let flat = vec![200u8; 64 * 64];
        assert_eq!(d_hash(&flat, 64, 64), Some(0));
        assert_eq!(a_hash(&flat, 64, 64), Some(0));
    }

    #[test]
    fn ahash_sets_the_bright_half() {
        let hash = a_hash(&half_plane(), 64, 64).expect("fits");
        // Row-major: the right half of every row is above the mean.
        for y in 0..8usize {
            for x in 0..8usize {
                let bit = (hash >> (y * 8 + x)) & 1;
                assert_eq!(bit, u64::from(x >= 4), "bit ({x},{y})");
            }
        }
    }

    #[test]
    fn band_keys_split_and_round_trip() {
        let hash = 0x0123_4567_89ab_cdefu64;
        let keys = band_keys(hash, LSH_BANDS);
        assert_eq!(keys.len(), 4);
        assert_eq!(keys[0], (0, 0xcdef));
        assert_eq!(keys[3], (3, 0x0123));
        assert!(band_keys(hash, 3).is_empty(), "3 does not divide 64");
        assert!(band_keys(hash, 0).is_empty());
    }

    #[test]
    fn candidates_come_from_shared_bands_only() {
        // Full-width hashes, as a real 64×64 render produces: with small
        // values every item's high bands are zero and *everything* meets in
        // the zero bucket, which says nothing about the artwork.
        let items = vec![
            item(1, 0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210, 1),
            item(2, 0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210, 2),
            item(3, 0x1111_2222_3333_4444, 0x5555_6666_7777_8888, 3),
        ];
        assert_eq!(candidate_pairs(&items, LSH_BANDS), vec![(0, 1)]);
    }

    #[test]
    fn a_pair_sharing_both_hashes_is_reported_once() {
        let items = vec![item(1, 0xaaaa, 0xbbbb, 1), item(2, 0xaaaa, 0xbbbb, 2)];
        assert_eq!(candidate_pairs(&items, LSH_BANDS), vec![(0, 1)]);
    }

    #[test]
    fn three_identical_hashes_give_three_pairs() {
        let items = vec![
            item(1, 0x1234, 0x5678, 1),
            item(2, 0x1234, 0x5678, 2),
            item(3, 0x1234, 0x5678, 3),
        ];
        assert_eq!(
            candidate_pairs(&items, LSH_BANDS),
            vec![(0, 1), (0, 2), (1, 2)]
        );
    }

    #[test]
    fn iou_is_one_for_identical_and_zero_for_disjoint() {
        let ink = half_plane();
        let blank = vec![0u8; 64 * 64];
        assert_eq!(ink_iou(&ink, &ink, 64, 64), Some(1.0));
        assert_eq!(ink_iou(&ink, &blank, 64, 64), Some(0.0));
        assert_eq!(ink_iou(&blank, &blank, 64, 64), Some(1.0));
    }

    #[test]
    fn hausdorff_measures_the_shift_in_pixels() {
        let mut moved = half_plane();
        // Move the boundary two pixels right: the symmetric Hausdorff distance
        // is 2 px over the 64×64 diagonal.
        for y in 0..64 {
            for x in 0..64 {
                moved[y * 64 + x] = if (32..34).contains(&x) {
                    0
                } else {
                    moved[y * 64 + x]
                };
            }
        }
        let d = hausdorff_normalised(&half_plane(), &moved, 64, 64).expect("both have ink");
        let expected = 2.0 / ((64f32 * 64.0 + 64.0 * 64.0).sqrt());
        assert!((d - expected).abs() < 0.01, "got {d}, expected {expected}");
        assert_eq!(
            hausdorff_normalised(&half_plane(), &half_plane(), 64, 64),
            Some(0.0)
        );
    }

    #[test]
    fn hausdorff_refuses_a_blank_plane() {
        let blank = vec![0u8; 64 * 64];
        assert_eq!(hausdorff_normalised(&half_plane(), &blank, 64, 64), None);
    }

    #[test]
    fn verify_passes_on_either_distance() {
        let options = DupOptions::default();
        let ink = half_plane();
        let verified = verify(&ink, &ink, 64, 64, &options).expect("fits");
        assert!(verified.pass);
        assert_eq!(verified.iou, 1.0);
        let blank = vec![0u8; 64 * 64];
        assert!(verify(&ink, &blank, 64, 64, &options).is_none());
    }

    #[test]
    fn confirm_needs_a_digest_or_ssim_match() {
        let options = DupOptions::default();
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert!(confirm(&a, &a, 0.0, &options), "identical digests confirm");
        assert!(confirm(&a, &b, 0.98, &options), "high SSIM confirms");
        assert!(!confirm(&a, &b, 0.90, &options));
    }

    #[test]
    fn clusters_keep_the_best_scored_member() {
        let items = vec![item(7, 1, 1, 1), item(9, 1, 1, 2), item(4, 2, 2, 3)];
        let pairs = vec![(0, 1)];
        let clusters = cluster(&items, &pairs, &[0.4, 0.9, 0.5]);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].members, vec![7, 9]);
        assert_eq!(clusters[0].keeper, 9);
    }

    #[test]
    fn clusters_are_sorted_and_singletons_dropped() {
        let items = vec![
            item(5, 1, 1, 1),
            item(6, 1, 1, 2),
            item(1, 9, 9, 3),
            item(2, 9, 9, 4),
        ];
        let pairs = vec![(0, 1), (2, 3)];
        let clusters = cluster(&items, &pairs, &[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].keeper, 1, "sorted by keeper");
        assert_eq!(clusters[1].keeper, 5);
        let unique = cluster(&items, &[], &[0.0; 4]);
        assert!(unique.is_empty(), "no pairs, no clusters");
    }

    #[test]
    fn keepers_prefer_the_best_score_then_the_smaller_id() {
        assert_eq!(
            suggest_keeper(&[8, 3], &[0.5, 0.5]),
            Some(3),
            "a tie goes to id 3"
        );
        assert_eq!(suggest_keeper(&[8, 3], &[0.6, 0.5]), Some(8));
        assert_eq!(suggest_keeper(&[], &[]), None);
        assert_eq!(
            suggest_keeper(&[8, 3], &[0.5]),
            Some(8),
            "the member with a score beats the one whose score is missing"
        );
        assert_eq!(
            suggest_keeper(&[8, 3], &[]),
            Some(3),
            "with no scores at all it is purely the smaller id"
        );
    }
}
