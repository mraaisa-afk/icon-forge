/**
 * The review workspace's entry point in the sheet drawer (§3.6).
 *
 * The pass itself is seconds of work on a real sheet (≈17 s at 1000 icons), so
 * this card says so before the button is pressed, and it shows the triage state
 * of a sheet that has already been reviewed rather than pretending a fresh
 * sheet is undecided.
 */

import { useEffect } from "react";
import { formatBytes, progressLine } from "../../lib/reviewModel";
import { useStore } from "../../state/store";

export function ReviewCard() {
  const sheet = useStore((s) => s.selectedSheet);
  const review = useStore((s) => s.review);
  const log = useStore((s) => s.reviewLog);
  const busy = useStore((s) => s.reviewBusy);
  const openReview = useStore((s) => s.openReview);
  const refresh = useStore((s) => s.refreshReviewState);

  // A sheet can be reopened long after it was triaged: the log lives in the
  // project file, so its counts are re-read rather than remembered. That is
  // `review_state` — a journal replay, not a pass — which is why it runs on
  // open while the pass itself waits for the button.
  useEffect(() => {
    if (sheet && !review) void refresh();
  }, [sheet, review, refresh]);

  if (!sheet) return null;

  return (
    <div className="border-b border-forge-edge px-3 py-3">
      <div className="flex items-center justify-between gap-2">
        <div className="text-xs text-forge-text">Review</div>
        <button
          type="button"
          data-testid="open-review"
          disabled={busy}
          onClick={() => void openReview()}
          className="rounded bg-forge-accent px-2 py-1 text-[10px] font-medium text-forge-bg hover:opacity-90 disabled:opacity-50"
        >
          {review ? "Open workspace" : "Run review pass"}
        </button>
      </div>
      <div className="mt-1 text-[10px] text-forge-dim">
        {review
          ? progressLine(review.triage, review.icons.length)
          : log && log.decided > 0
            ? `This sheet already has ${log.decided} decision(s) in its log · ` +
              `${formatBytes(log.csvBytes)} of review.csv · seq ${log.seq}`
            : "Quality flags, duplicate cascade and outlier scan, then keyboard triage " +
              "(A/R/F/D · Space · Shift+A) and an undoable review.csv."}
      </div>
      {review && (
        <div className="mt-1 text-[10px] text-forge-dim">
          {review.flagged} flagged · {review.clusters.length} clusters ·{" "}
          {review.outliers.length} deviations · export {formatBytes(review.triage.csvBytes)}
        </div>
      )}
    </div>
  );
}
