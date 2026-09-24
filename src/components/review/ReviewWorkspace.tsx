/**
 * The review workspace (§3.6): the triage screen over one sheet's review pass.
 *
 * It takes the main area while it is open, because triaging 1000 icons is a
 * full-screen activity. The list is virtualized with the same raw window math
 * the library grid uses (`virtualWindow`) — §3.6's budget is 1000 icons, and a
 * browser that lays out 1000 rows on every keystroke is what makes a triage
 * session slow.
 *
 * All the *decisions* live in `lib/reviewModel` (keyboard table, filters,
 * ordering, pace projection) and in the store (backend calls); this file is the
 * screen: it renders those, and it is the only place a `KeyboardEvent` is
 * turned into a `ReviewIntent`.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import {
  budgetLine,
  cascadeLine,
  detectorLine,
  filterCounts,
  filterRows,
  formatBytes,
  formatMs,
  isTypingTarget,
  KEY_HELP,
  keyIntent,
  orderRows,
  progressLine,
  REVIEW_FILTER_LABELS,
  REVIEW_FILTERS,
  shortHash,
  TRIAGE_KEYS,
  type ReviewFilter,
  type TriageActionName,
} from "../../lib/reviewModel";
import { useStore } from "../../state/store";
import { virtualWindow } from "../library/virtualWindow";
import { ReviewRow } from "./ReviewRow";

/** Row height in px — must match the row's own layout closely enough to scroll. */
const ROW_HEIGHT = 52;

/** How often the pace line re-reads the clock. */
const PACE_TICK_MS = 5000;

export function ReviewWorkspace() {
  const sheet = useStore((s) => s.selectedSheet);
  const review = useStore((s) => s.review);
  const busy = useStore((s) => s.reviewBusy);
  const note = useStore((s) => s.reviewNote);
  const filter = useStore((s) => s.reviewFilter);
  const order = useStore((s) => s.reviewOrder);
  const selected = useStore((s) => s.reviewSelected);
  const overlay = useStore((s) => s.reviewOverlay);
  const crop = useStore((s) => s.reviewCrop);
  const startedAt = useStore((s) => s.reviewStartedAt);
  const exported = useStore((s) => s.reviewExported);
  const icons = useStore((s) => s.icons);

  const [outDir, setOutDir] = useState("");
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(320);
  const listRef = useRef<HTMLDivElement | null>(null);
  const [now, setNow] = useState(() => Date.now());

  const rows = useMemo(
    () => (review ? filterRows(orderRows(review.icons, order), filter) : []),
    [review, order, filter],
  );
  const counts = useMemo(() => filterCounts(review?.icons ?? []), [review]);

  // The pace line is the only clock in the screen; it ticks slowly because a
  // projection in the tenth of a second is noise.
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), PACE_TICK_MS);
    return () => window.clearInterval(timer);
  }, []);

  // The virtual window needs the list's real height, which the flex layout only
  // knows after the first paint.
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const measure = () => setViewport(el.clientHeight);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // Keyboard triage. Bound at the window so a reviewer never has to click the
  // list first, and guarded against text fields so the export path box does not
  // approve icons while it is being typed into.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target && isTypingTarget(target.tagName, target.isContentEditable)) return;
      const intent = keyIntent(event);
      if (!intent) return;
      const store = useStore.getState();
      switch (intent.kind) {
        case "decide":
          if (store.reviewSelected) void store.decideReview(store.reviewSelected, intent.action);
          break;
        case "undo":
          void store.undoReview();
          break;
        case "overlay":
          void store.toggleReviewOverlay();
          break;
        case "move":
          store.moveReviewSelection(intent.delta);
          break;
        case "dismiss":
          if (store.reviewOverlay) void store.toggleReviewOverlay();
          else store.setReviewFilter("all");
          break;
        case "export":
          void store.exportReview();
          break;
      }
      if (event.key === " ") event.preventDefault();
      if (event.key.startsWith("Arrow")) event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (!sheet || !review) return null;

  const listWindow = virtualWindow(scrollTop, viewport, rows.length, {
    rowHeight: ROW_HEIGHT,
    columns: 1,
  });
  const slice = rows.slice(listWindow.start, listWindow.end);
  const pace = budgetLine(review.triage.decided, review.icons.length, now - startedAt);
  const overlayRow = overlay ? review.icons.find((icon) => icon.id === overlay) : undefined;
  const overlayTile = overlay ? icons.find((icon) => icon.id === overlay) : undefined;

  const decide = (action: TriageActionName) => {
    if (selected) void useStore.getState().decideReview(selected, action);
  };

  return (
    <section className="flex min-h-0 flex-1 flex-col bg-forge-bg" data-testid="review-workspace">
      <header className="shrink-0 border-b border-forge-edge px-3 py-2">
        <div className="flex items-center justify-between gap-2">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className="text-sm text-forge-text">Review</span>
              <span className="text-[10px] text-forge-dim">
                {sheet.sourcePath.split(/[\\/]/).pop()}
              </span>
              {busy && <span className="text-[10px] text-forge-accent">working…</span>}
            </div>
            <div className="truncate text-[10px] text-forge-dim" data-testid="review-detectors">
              {detectorLine(review)}
            </div>
            <div className="truncate text-[10px] text-forge-dim">
              cascade {cascadeLine(review.cascade, review.icons.length)} · pass ran in{" "}
              {formatMs(review.renderMs + review.detectMs)} of engine time
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-1">
            <button
              type="button"
              data-testid="review-rerun"
              disabled={busy}
              onClick={() => void useStore.getState().openReview(true)}
              className="rounded border border-forge-edge px-2 py-1 text-[10px] text-forge-dim hover:text-forge-text disabled:opacity-50"
            >
              Re-run pass
            </button>
            <button
              type="button"
              data-testid="review-bulk"
              disabled={busy || counts.pending === 0}
              title={`approve every undecided icon (${TRIAGE_KEYS["bulk-approve"]})`}
              onClick={() => void useStore.getState().bulkApproveReview()}
              className="rounded border border-forge-edge px-2 py-1 text-[10px] text-forge-dim hover:text-forge-text disabled:opacity-50"
            >
              Approve rest
            </button>
            <button
              type="button"
              data-testid="review-close"
              onClick={() => useStore.getState().closeReview()}
              className="rounded px-2 py-1 text-[10px] text-forge-dim hover:text-forge-text"
            >
              close
            </button>
          </div>
        </div>

        <div className="mt-2 flex flex-wrap items-center gap-1">
          {REVIEW_FILTERS.map((tab: ReviewFilter) => (
            <button
              key={tab}
              type="button"
              data-testid={`review-filter-${tab}`}
              onClick={() => useStore.getState().setReviewFilter(tab)}
              className={
                "rounded border px-2 py-0.5 text-[10px] " +
                (tab === filter
                  ? "border-forge-accent text-forge-text"
                  : "border-forge-edge text-forge-dim hover:text-forge-text")
              }
            >
              {REVIEW_FILTER_LABELS[tab]} {counts[tab]}
            </button>
          ))}
          <span className="ml-2 flex items-center gap-1">
            {(["attention", "sheet"] as const).map((mode) => (
              <button
                key={mode}
                type="button"
                onClick={() => useStore.getState().setReviewOrder(mode)}
                className={
                  "rounded border px-2 py-0.5 text-[10px] " +
                  (mode === order
                    ? "border-forge-accent text-forge-text"
                    : "border-forge-edge text-forge-dim hover:text-forge-text")
                }
              >
                {mode === "attention" ? "worst first" : "sheet order"}
              </button>
            ))}
          </span>
        </div>
      </header>

      <div className="flex min-h-0 flex-1">
        <div
          ref={listRef}
          onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
          className="min-h-0 flex-1 overflow-y-auto"
        >
          {rows.length === 0 ? (
            <div className="p-4 text-xs text-forge-dim">
              Nothing on this tab — {review.icons.length} icons in the pass.
            </div>
          ) : (
            <div style={{ height: listWindow.totalHeight, position: "relative" }}>
              <div style={{ transform: `translateY(${listWindow.offsetY}px)` }}>
                {slice.map((icon) => (
                  <ReviewRow
                    key={icon.id}
                    icon={icon}
                    selected={icon.id === selected}
                    onSelect={() => useStore.getState().selectReviewIcon(icon.id)}
                    onDecide={(action) => void useStore.getState().decideReview(icon.id, action)}
                    onOverlay={() => void useStore.getState().toggleReviewOverlay(icon.id)}
                  />
                ))}
              </div>
            </div>
          )}
        </div>

        <aside className="w-80 shrink-0 overflow-y-auto border-l border-forge-edge p-3 text-[10px]">
          <div className="text-forge-text">Triage</div>
          <div className="mt-1 text-forge-dim" data-testid="review-progress">
            {progressLine(review.triage, review.icons.length)}
          </div>
          <div className="mt-1 text-forge-dim" data-testid="review-pace">
            {pace.line}
          </div>
          <div className="mt-1 text-forge-dim">
            {review.triage.canUndo
              ? `Ctrl+Z undoes seq ${review.triage.seq - 1}`
              : "no decision to undo yet"}
            {review.triage.last && (
              <>
                {" · last "}
                {review.triage.last.action} on row {review.triage.last.index}
              </>
            )}
          </div>

          <div className="mt-3 flex items-center gap-1">
            <input
              value={outDir}
              onChange={(e) => setOutDir(e.target.value)}
              placeholder="output folder (blank = preview)"
              data-testid="review-outdir"
              className="min-w-0 flex-1 rounded border border-forge-edge bg-forge-panel px-2 py-1 text-[10px] text-forge-text"
            />
            <button
              type="button"
              data-testid="review-export"
              disabled={busy}
              onClick={() => void useStore.getState().exportReview(outDir.trim() || undefined)}
              className="rounded border border-forge-edge px-2 py-1 text-forge-dim hover:text-forge-text disabled:opacity-50"
            >
              Export
            </button>
          </div>
          {exported && (
            <div className="mt-1 text-forge-dim" data-testid="review-export-info">
              {exported.path ? `${exported.path} · ` : "preview · "}
              {exported.decisions} decisions · {formatBytes(exported.csv.length)} · seq{" "}
              {exported.seq}
            </div>
          )}

          {note && <div className="mt-2 text-forge-accent">{note}</div>}

          <div className="mt-3 border-t border-forge-edge pt-2 text-forge-dim">
            <div className="text-forge-text">Keyboard</div>
            <div className="mt-1 flex flex-wrap gap-x-2 gap-y-1">
              {KEY_HELP.map(([key, what]) => (
                <span key={key}>
                  <kbd className="rounded border border-forge-edge px-1">{key}</kbd> {what}
                </span>
              ))}
            </div>
          </div>

          <div className="mt-3 border-t border-forge-edge pt-2">
            <div className="flex items-center gap-1">
              {(
                [
                  ["approve", "A"],
                  ["reject", "R"],
                  ["flag", "F"],
                  ["duplicate", "D"],
                ] as const
              ).map(([action, key]) => (
                <button
                  key={action}
                  type="button"
                  data-testid={`review-side-decide-${action}`}
                  disabled={busy || selected === null}
                  onClick={() => decide(action)}
                  className="rounded border border-forge-edge px-2 py-1 text-forge-dim hover:text-forge-text disabled:opacity-50"
                >
                  {key} {action}
                </button>
              ))}
            </div>
            <div className="mt-1 text-forge-dim">
              {selected ? `cursor on ${shortHash(selected, 4)}` : "no row selected"}
            </div>
          </div>

          {review.skipped.length > 0 && (
            <div className="mt-3 border-t border-forge-edge pt-2 text-forge-dim">
              <div className="text-forge-text">Skipped ({review.skipped.length})</div>
              <ul className="mt-1 list-disc pl-4">
                {review.skipped.slice(0, 8).map((skip) => (
                  <li key={skip.id}>
                    {shortHash(skip.id, 4)} — {skip.reason}
                  </li>
                ))}
              </ul>
            </div>
          )}

          {overlayRow ? (
            <div className="mt-3 border-t border-forge-edge pt-2" data-testid="review-overlay-panel">
              <div className="flex items-center justify-between">
                <span className="text-forge-text">Overlay · row {overlayRow.index}</span>
                <button
                  type="button"
                  onClick={() => void useStore.getState().toggleReviewOverlay()}
                  className="text-forge-dim hover:text-forge-text"
                >
                  hide
                </button>
              </div>
              {crop ? (
                <img
                  src={`data:image/png;base64,${crop}`}
                  alt={`sheet crop of row ${overlayRow.index}`}
                  className="mt-1 w-full rounded border border-forge-edge bg-forge-bg"
                  style={{ imageRendering: "pixelated" }}
                />
              ) : (
                <div className="mt-1 text-forge-dim">
                  {overlayTile ? "loading the crop…" : "no sheet row for this icon"}
                </div>
              )}
              <div className="mt-1 text-forge-dim">
                ink {overlayRow.inkArea} px² · stroke {overlayRow.stat.stroke.toFixed(2)} px ·
                solidity {overlayRow.stat.solidity.toFixed(3)} · fill{" "}
                {overlayRow.stat.fillRatio.toFixed(3)}
              </div>
              <div className="text-forge-dim">
                palette {overlayRow.stat.palette} · digest {shortHash(overlayRow.digest, 6)}
              </div>
            </div>
          ) : (
            <div className="mt-3 border-t border-forge-edge pt-2 text-forge-dim">
              Space shows the selected row's own pixels from the sheet.
            </div>
          )}
        </aside>
      </div>
    </section>
  );
}
