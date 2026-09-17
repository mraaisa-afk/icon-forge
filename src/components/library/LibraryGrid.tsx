import { useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "../../state/store";
import { virtualWindow } from "./virtualWindow";

const CELL = 128;
const ROW_HEIGHT = CELL + 10;
const PAGE = 200;

function shortHash(hash: string): string {
  return hash.length > 10 ? `${hash.slice(0, 6)}…${hash.slice(-4)}` : hash;
}

/**
 * Virtualized library grid — raw implementation (no grid library):
 * a spacer div sized to the full list, an absolutely positioned slice for
 * the visible window (+overscan), page fetches as the window moves.
 */
export function LibraryGrid() {
  const sheets = useStore((s) => s.sheets);
  const totalCount = useStore((s) => s.totalCount);
  const project = useStore((s) => s.project);
  const refreshPage = useStore((s) => s.refreshPage);
  const openSheet = useStore((s) => s.openSheet);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(600);
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    setHeight(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  // Column count adapts to the container width.
  const [columns, setColumns] = useState(6);
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setColumns(Math.max(1, Math.floor(el.clientWidth / CELL)));
    });
    ro.observe(el);
    setColumns(Math.max(1, Math.floor(el.clientWidth / CELL)));
    return () => ro.disconnect();
  }, []);

  const win = useMemo(
    () => virtualWindow(scrollTop, height, totalCount, { rowHeight: ROW_HEIGHT, columns, overscan: 4 }),
    [scrollTop, height, totalCount, columns],
  );

  // Fetch the page covering the requested window (bounded prefetch).
  useEffect(() => {
    if (!project || totalCount === 0) return;
    const needed = Math.min(totalCount, win.end + PAGE);
    if (sheets.length < needed) {
      void refreshPage(0, Math.min(totalCount, Math.max(sheets.length, needed)));
    }
  }, [project, totalCount, win.end, sheets.length, refreshPage]);

  const visible = sheets.slice(win.start, win.end);

  return (
    <div
      ref={scroller}
      data-testid="library-scroller"
      className="h-full overflow-y-auto bg-forge-bg"
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
    >
      <div style={{ height: win.totalHeight, position: "relative" }}>
        <div
          style={{
            position: "absolute",
            top: win.offsetY,
            left: 0,
            right: 0,
            display: "grid",
            gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`,
          }}
        >
          {visible.map((sheet) => (
            <button
              key={sheet.id}
              type="button"
              data-testid="library-cell"
              className="m-[5px] flex cursor-pointer flex-col items-center justify-center rounded border border-forge-edge bg-forge-panel p-2 hover:border-forge-accent"
              style={{ height: CELL }}
              title={sheet.sourcePath}
              onClick={() => void openSheet(sheet)}
            >
              <div className="text-[10px] text-forge-dim">{shortHash(sheet.contentHash)}</div>
              <div className="mt-1 truncate text-xs text-forge-text">
                {sheet.sourcePath.split(/[\\/]/).pop()}
              </div>
              <div className="text-[10px] text-forge-dim">
                {sheet.width}×{sheet.height}
              </div>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
