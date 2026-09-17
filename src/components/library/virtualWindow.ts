/**
 * Pure window math for the hand-rolled virtualized library grid
 * (ARCHITECTURE.md: raw implementation, no grid library).
 *
 * The grid is a fixed set of columns with uniform cell size; the window is
 * the visible row range plus `overscan` rows on each side, clamped to the
 * item count. Keeping this pure makes the invariant testable:
 * `firstVisible ≤ start` and `end ≥ lastVisible` whenever items exist.
 */

export interface VirtualWindow {
  /** Index of the first rendered item. */
  start: number;
  /** One past the last rendered item (clamped to `itemCount`). */
  end: number;
  /** Top offset of the rendered slice, in px. */
  offsetY: number;
  /** Total scrollable height, in px. */
  totalHeight: number;
  /** Grid columns actually used for the given width. */
  columns: number;
}

/** Computes the rendered slice for a scroll position. */
export function virtualWindow(
  scrollTop: number,
  viewportHeight: number,
  itemCount: number,
  opts: { rowHeight: number; columns: number; overscan?: number },
): VirtualWindow {
  const { rowHeight, columns } = opts;
  const overscan = opts.overscan ?? 4;
  const rows = columns > 0 ? Math.ceil(itemCount / columns) : 0;
  const totalHeight = rows * rowHeight;

  if (itemCount === 0 || rows === 0 || viewportHeight <= 0 || columns <= 0) {
    return { start: 0, end: 0, offsetY: 0, totalHeight, columns: Math.max(columns, 0) };
  }

  // Clamp to the max scroll position: real scrollers clamp lazily, and
  // overscroll/momentum or programmatic writes can transiently exceed it.
  // Without this, a scrolled-past-the-end window would render nothing.
  const maxScroll = Math.max(0, totalHeight - viewportHeight);
  const top = Math.min(Math.max(scrollTop, 0), maxScroll);

  const firstVisibleRow = Math.floor(top / rowHeight);
  const visibleRows = Math.ceil(viewportHeight / rowHeight);
  const startRow = Math.max(0, firstVisibleRow - overscan);
  const endRow = Math.min(rows, firstVisibleRow + visibleRows + overscan);

  return {
    start: startRow * columns,
    end: Math.min(itemCount, endRow * columns),
    offsetY: startRow * rowHeight,
    totalHeight,
    columns,
  };
}
