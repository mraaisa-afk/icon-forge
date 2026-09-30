/**
 * One triage row of the review workspace (§3.6).
 *
 * It shows the icon's own numbers, what the detectors said about it, and the
 * four decision buttons whose keys the keyboard binds. It is presentational:
 * every action is a callback, so the store stays the only place that talks to
 * the backend and the row stays renderable in any order the list chooses.
 */

import {
  clusterLabel,
  formatScore,
  isDuplicateCandidate,
  shortHash,
  TRIAGE_KEYS,
  type ReviewIconDto,
  type TriageActionName,
} from "../../lib/reviewModel";

/** The state chip's colours, by state name. Unlisted states fall back to dim. */
const CHIP: Record<string, string> = {
  pending: "border-forge-edge text-forge-dim",
  approve: "border-emerald-500/60 text-emerald-300",
  "bulk-approve": "border-emerald-500/40 text-emerald-200/90",
  reject: "border-rose-500/60 text-rose-300",
  flag: "border-amber-500/60 text-amber-300",
  duplicate: "border-sky-500/60 text-sky-300",
};

/** The four per-icon decisions, in the order §3.6's bar lists them. */
const DECISIONS: readonly { action: TriageActionName; letter: string; className: string }[] = [
  { action: "approve", letter: "A", className: "hover:border-emerald-500/60 hover:text-emerald-300" },
  { action: "reject", letter: "R", className: "hover:border-rose-500/60 hover:text-rose-300" },
  { action: "flag", letter: "F", className: "hover:border-amber-500/60 hover:text-amber-300" },
  { action: "duplicate", letter: "D", className: "hover:border-sky-500/60 hover:text-sky-300" },
];

/** One row. */
export function ReviewRow({
  icon,
  selected,
  onSelect,
  onDecide,
  onOverlay,
}: {
  icon: ReviewIconDto;
  selected: boolean;
  onSelect: () => void;
  onDecide: (action: TriageActionName) => void;
  onOverlay: () => void;
}) {
  return (
    <div
      data-testid="review-row"
      data-state={icon.state}
      // The row is a click target and a keyboard cursor, so it needs to be
      // something a screen reader can describe and a reviewer can hear: which
      // icon, what state it is in, and whether the cursor is on it.
      role="listitem"
      aria-label={`icon ${icon.index}, ${icon.state}, ${shortHash(icon.id, 4)}${
        selected ? ", selected" : ""
      }`}
      onClick={onSelect}
      className={
        "flex cursor-default items-center gap-3 border-b border-forge-edge/60 px-3 py-2 " +
        (selected ? "bg-forge-edge/40" : "hover:bg-forge-edge/20")
      }
    >
      <div className="w-8 shrink-0 text-right font-mono text-[10px] text-forge-dim">
        {icon.index}
      </div>

      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span
            className={
              "rounded border px-1.5 py-0.5 text-[10px] " + (CHIP[icon.state] ?? CHIP.pending)
            }
          >
            {icon.state}
          </span>
          <span className="font-mono text-[10px] text-forge-dim">{shortHash(icon.id, 4)}</span>
          <span className="text-[10px] text-forge-dim">
            ssim {formatScore(icon.score.ssim)} · iou {formatScore(icon.score.iou)} ·{" "}
            {icon.nodeCount} nodes · {icon.colours} colours · {icon.closed ? "closed" : "open"}
          </span>
          {icon.outliers.map((outlier) => (
            <span key={outlier.kind} className="text-[10px] text-sky-300">
              {outlier.kind} z {outlier.z.toFixed(1)} · {outlier.value.toFixed(1)} vs{" "}
              {outlier.median.toFixed(1)}
            </span>
          ))}
        </div>
        <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[10px]">
          {icon.flags.map((flag) => (
            <span key={flag} className="text-amber-300">
              quality: {flag}
            </span>
          ))}
          {icon.cluster !== undefined && (
            <span className="text-sky-300">
              {isDuplicateCandidate(icon) ? "duplicate of" : "keeper of"} {clusterLabel(icon.cluster)}
            </span>
          )}
          {!icon.closed && <span className="text-rose-300">open contour</span>}
          <span className="text-forge-dim">
            dHash {icon.dHash} · aHash {icon.aHash}
          </span>
        </div>
      </div>

      <div className="flex shrink-0 items-center gap-1">
        <button
          type="button"
          data-testid="review-overlay"
          title="Sheet crop overlay (Space)"
          // The glyph is the only label this button has on screen.
          aria-label={`sheet crop overlay for icon ${icon.index}`}
          // Space is the overlay key but not a decision, so it is not in
          // TRIAGE_KEYS; the other four shortcuts are read from the table.
          aria-keyshortcuts="Space"
          onClick={(e) => {
            e.stopPropagation();
            onOverlay();
          }}
          className="rounded border border-forge-edge px-1.5 py-1 text-[10px] text-forge-dim hover:text-forge-text"
        >
          ⤢
        </button>
        {DECISIONS.map((decision) => (
          <button
            key={decision.action}
            type="button"
            data-testid={`review-decide-${decision.action}`}
            title={`${decision.action} (${TRIAGE_KEYS[decision.action]})`}
            // "A" is not a name: the button is the action for *this* icon, and
            // the shortcut is announced rather than left in a tooltip.
            aria-label={`${decision.action} icon ${icon.index}`}
            aria-keyshortcuts={TRIAGE_KEYS[decision.action]}
            aria-pressed={icon.state === decision.action}
            onClick={(e) => {
              e.stopPropagation();
              onDecide(decision.action);
            }}
            className={
              "rounded border border-forge-edge px-1.5 py-1 text-[10px] text-forge-dim " +
              decision.className
            }
          >
            {decision.letter}
          </button>
        ))}
      </div>
    </div>
  );
}
