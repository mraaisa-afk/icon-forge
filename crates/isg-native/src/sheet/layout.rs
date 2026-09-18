//! Sheet layout — where each icon's cell sits on the generated sheet.
//!
//! Pure arithmetic: a grid of square cells, separated by a gap, inset by a
//! margin. No I/O, no image types, nothing outside `std` — the exporter, the
//! preview and the tests all use *this* function, so a sheet exported by the
//! native side and a sheet previewed in the webview cannot disagree about where
//! a cell is.
//!
//! The sheet is sized by content: `2·margin + columns·cell + (columns−1)·gap`.
//! A partial last row is left empty on the right (icons are packed in reading
//! order), because a row that is short must not move the icons that *are* on it.
//!
//! A sheet also *shrinks* to its content: three icons asked for sixteen columns
//! are one row of three, not one row of sixteen with thirteen empty cells. The
//! requested `columns` is a maximum, which is what makes the same spec usable
//! for a 4-icon preview and a 1024-icon export.

use super::SheetSpec;

/// A resolved grid: how many cells fit, and how big the sheet is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridLayout {
    /// Cells per row: `min(spec.columns, icon count)`, or `0` when empty.
    pub columns: u32,
    /// Rows needed for the icon count; `0` when there are no icons.
    pub rows: u32,
    /// Sheet width in pixels.
    pub width: u32,
    /// Sheet height in pixels.
    pub height: u32,
}

/// How many columns of `spec.cell` fit into `available` pixels of sheet width.
///
/// Used by the "fit the sheet to this width" control. Returns at least 1, so a
/// narrow window can never produce a zero-column sheet.
#[must_use]
pub fn fit_columns(available: f32, spec: &SheetSpec) -> u32 {
    let cell = spec.cell as f32;
    let gap = spec.gap as f32;
    let margin = spec.margin as f32;
    let usable = available - 2.0 * margin + gap;
    if !usable.is_finite() || usable < cell {
        return 1;
    }
    ((usable / (cell + gap)).floor() as u32).max(1)
}

impl GridLayout {
    /// Lays out `count` cells, sized so that every cell is fully on the sheet.
    #[must_use]
    pub fn solve(count: usize, spec: &SheetSpec) -> Self {
        // Only the cells that will be used: an empty sheet has no columns at
        // all, and a short sheet is exactly as wide as its longest row.
        let wanted = u32::try_from(count).unwrap_or(u32::MAX);
        let columns = if count == 0 {
            0
        } else {
            spec.columns.max(1).min(wanted)
        };
        let rows = if count == 0 {
            0
        } else {
            wanted.div_ceil(columns)
        };
        let width = span(spec, columns);
        let height = span(spec, rows);
        Self {
            columns,
            rows,
            width,
            height,
        }
    }

    /// The top-left corner of cell `index` in sheet pixels.
    ///
    /// Icons fill the grid in reading order: left to right, top to bottom. The
    /// index must be inside the layout ([`GridLayout::columns`] ·
    /// [`GridLayout::rows`]); the plan only ever asks for its own icons.
    #[must_use]
    pub fn cell_origin(&self, index: usize, spec: &SheetSpec) -> (u32, u32) {
        let index = index as u32;
        let column = index % self.columns;
        let row = index / self.columns;
        let x = spec.margin + column * (spec.cell + spec.gap);
        let y = spec.margin + row * (spec.cell + spec.gap);
        (x, y)
    }
}

/// `2·margin + n·cell + (n−1)·gap`, saturating (a silly spec must not wrap).
///
/// Computed in `u64` with saturating steps: `n · (cell + gap)` itself overflows
/// `u32` for a large cell *and* a large count, and a sheet that silently wraps
/// to a small number is worse than one that saturates.
fn span(spec: &SheetSpec, n: u32) -> u32 {
    let step = u64::from(spec.cell).saturating_add(u64::from(spec.gap));
    let cells = u64::from(n)
        .saturating_mul(step)
        .saturating_sub(u64::from(spec.gap));
    let total = u64::from(spec.margin)
        .saturating_mul(2)
        .saturating_add(cells);
    total.min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Placement;

    fn spec() -> SheetSpec {
        SheetSpec {
            cell: 64,
            padding: 8,
            gap: 8,
            margin: 8,
            columns: 4,
            ink_ratio: 0.8,
            placement: Placement::Center,
        }
    }

    #[test]
    fn sizes_the_sheet_from_its_content() {
        let s = spec();
        let layout = GridLayout::solve(10, &s);
        assert_eq!(layout.columns, 4);
        assert_eq!(layout.rows, 3);
        // 8 + 4·64 + 3·8 + 8 = 296 wide, 8 + 3·64 + 2·8 + 8 = 224 tall: three
        // rows of four cells is not a square sheet, and the arithmetic says so.
        assert_eq!(layout.width, 296);
        assert_eq!(layout.height, 224);
    }

    #[test]
    fn the_requested_column_count_is_a_maximum() {
        let s = spec();
        // Three icons asked for sixteen columns: one row of three, no empties.
        let wide = GridLayout::solve(3, &SheetSpec { columns: 16, ..s });
        assert_eq!(wide.columns, 3);
        assert_eq!(wide.rows, 1);
        assert_eq!(wide.width, 2 * 8 + 3 * 64 + 2 * 8);
        // A count above the request still wraps.
        let many = GridLayout::solve(9, &SheetSpec { columns: 4, ..s });
        assert_eq!((many.columns, many.rows), (4, 3));
    }

    #[test]
    fn the_last_row_may_be_short_without_moving_anything() {
        let s = spec();
        let layout = GridLayout::solve(5, &s);
        assert_eq!(layout.rows, 2);
        assert_eq!(layout.cell_origin(4, &s), (8, 80));
        // The empty fifth cell of the row exists; nothing is placed in it.
        assert_eq!(layout.cell_origin(5, &s), (80, 80));
        // A fifth cell is still inside the sheet.
        let (x, y) = layout.cell_origin(5, &s);
        assert!(x + s.cell <= layout.width && y + s.cell <= layout.height);
    }

    #[test]
    fn an_empty_sheet_is_still_a_sheet() {
        let layout = GridLayout::solve(0, &spec());
        // No cells at all — but still a margin-wide, positive-area page, which
        // is what lets the PDF writer draw an empty sheet instead of refusing.
        assert_eq!((layout.columns, layout.rows), (0, 0));
        assert_eq!(layout.width, 16);
        assert_eq!(layout.height, 16);
        assert!(layout.width > 0 && layout.height > 0);
    }

    #[test]
    fn one_column_keeps_the_icons_stacked() {
        let s = SheetSpec {
            columns: 1,
            ..spec()
        };
        let layout = GridLayout::solve(3, &s);
        assert_eq!(layout.cell_origin(0, &s), (8, 8));
        assert_eq!(layout.cell_origin(2, &s), (8, 152));
        assert_eq!(layout.width, 80);
    }

    #[test]
    fn fit_columns_uses_the_width_it_is_given() {
        let s = spec();
        // A sheet of n columns is 2·8 + n·64 + (n−1)·8 wide, so it fits in `w`
        // when n ≤ (w − 16 + 8) / 72: 296 px is exactly four columns, 295 is not.
        assert_eq!(fit_columns(296.0, &s), 4);
        assert_eq!(fit_columns(295.0, &s), 3);
        assert_eq!(fit_columns(288.0, &s), 3);
        // Never zero, whatever the window does.
        assert_eq!(fit_columns(80.0, &s), 1);
        assert_eq!(fit_columns(10.0, &s), 1);
        assert_eq!(fit_columns(f32::NAN, &s), 1);
    }

    #[test]
    fn a_silly_spec_saturates_instead_of_wrapping() {
        let s = SheetSpec {
            cell: u32::MAX,
            gap: u32::MAX,
            margin: u32::MAX,
            columns: u32::MAX,
            ..spec()
        };
        let layout = GridLayout::solve(4, &s);
        assert_eq!(layout.width, u32::MAX);
        assert_eq!(layout.height, u32::MAX);
    }
}
