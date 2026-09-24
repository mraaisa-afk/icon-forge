/**
 * Phase 6 review model (ARCHITECTURE.md §3.6): the DTOs the five review
 * commands speak, and every decision the workspace makes *between* keystrokes.
 *
 * The split is the same one `canvasModel` / `groupModel` / `sheetModel` make:
 * the component renders, this file decides. Keyboard triage is the part that
 * most needs to live here — §3.6 binds `A` / `R` / `F` / `D` / `Space` /
 * `Shift+A`, and a shortcut table that can only be exercised by mounting a
 * browser cannot be tested at all under vitest's node environment.
 *
 * Two rules shape the API:
 *
 * * **The backend owns the truth.** A decision's authoritative result is the
 *   `triage` the command returns; the local patch in [`patchState`] only moves
 *   the one row's chip so the list does not flicker while the call is in
 *   flight. Nothing here computes sequence numbers or `csvBytes`, because
 *   `TriageLog::next_seq` and `to_csv` live in Rust and a second implementation
 *   of them in TypeScript would be a second answer.
 * * **A hash is text.** `dHash`, `aHash` and `stat.palette` arrive as 16-char
 *   hex strings because JSON numbers stop being exact in the webview at 2⁵³
 *   (see `StatOut::palette` on the native side) — so comparing two of them as
 *   strings is the only comparison that means anything.
 */

// ---- DTOs (the wire contract; mirrors `review_cmds.rs`) ------------------

/** Stage ⑧ score of the traced document against the sheet's own crop. */
export interface ReviewScoreDto {
  mae: number;
  ssim: number;
  iou: number;
  composite: number;
}

/** The per-icon numbers the outlier detector read. */
export interface ReviewStatDto {
  /** `√ink area`, in pixels. */
  inkSize: number;
  stroke: number;
  nodeCount: number;
  colours: number;
  solidity: number;
  fillRatio: number;
  /** 16-char hex of the folded palette hash. */
  palette: string;
}

/** One deviation of one icon. */
export interface ReviewOutlierDto {
  kind: string;
  z: number;
  value: number;
  median: number;
}

/** A sheet-level deviation: the deviation plus the icon it is about. */
export interface ReviewSheetOutlierDto extends ReviewOutlierDto {
  /** 32-char hex of the icon. */
  icon: string;
}

/** A duplicate cluster. */
export interface ReviewClusterDto {
  /** Member ids, ascending. */
  members: string[];
  /** The suggested keeper's id. */
  keeper: string;
  /** True when every member holds the same bytes — copies, not variants. */
  identical: boolean;
}

/** One reviewed icon. */
export interface ReviewIconDto {
  /** 32-char hex of the icon's id — the handle every command takes. */
  id: string;
  /** The sheet row the icon occupies; the triage log's key. */
  index: number;
  score: ReviewScoreDto;
  /** Quality flags, in a fixed order (`low-quality`, `over-complex`, …). */
  flags: string[];
  nodeCount: number;
  closed: boolean;
  colours: number;
  inkArea: number;
  stat: ReviewStatDto;
  /** 16-char hex. */
  dHash: string;
  /** 16-char hex. */
  aHash: string;
  /** 64-char hex of the cell's blake3 digest. */
  digest: string;
  /** A triage action name, or `pending`. */
  state: string;
  cluster?: ReviewClusterDto;
  keeper: boolean;
  outliers: ReviewOutlierDto[];
}

/** An icon the pass could not review. */
export interface ReviewSkippedDto {
  id: string;
  reason: string;
}

/** The duplicate cascade's funnel. */
export interface ReviewCascadeDto {
  /** Pairs that shared a hash band. */
  candidates: number;
  /** Candidates that passed IoU/Hausdorff verification. */
  verified: number;
  /** Verified pairs confirmed by digest or SSIM. */
  confirmed: number;
}

/** One triage decision. */
export interface ReviewDecisionDto {
  index: number;
  icon: string;
  action: string;
  seq: number;
  atMs: number;
}

/** The triage log, as the workspace reads it. */
export interface ReviewTriageDto {
  /** How many icons carry a decision. */
  decided: number;
  /** The next sequence number (never reused). */
  seq: number;
  /** Decisions per action, indexed like `TriageAction::ALL`. */
  counts: [number, number, number, number, number];
  canUndo: boolean;
  last?: ReviewDecisionDto;
  /** The size of the export this state would produce. */
  csvBytes: number;
}

/** One sheet's review, ready for the workspace. */
export interface ReviewOutDto {
  /** 32-char hex of the sheet's id. */
  sheet: string;
  icons: ReviewIconDto[];
  clusters: ReviewClusterDto[];
  outliers: ReviewSheetOutlierDto[];
  skipped: ReviewSkippedDto[];
  /** How many icons carry at least one quality flag. */
  flagged: number;
  cascade: ReviewCascadeDto;
  /** Render + hash time, in milliseconds. */
  renderMs: number;
  /** Cascade + outlier time, in milliseconds. */
  detectMs: number;
  triage: ReviewTriageDto;
}

/** What an undo changed. */
export interface ReviewUndoDto {
  triage: ReviewTriageDto;
  /** 32-char hex of the icon whose decision was taken back. */
  icon: string;
  /** The state that icon went back to (`pending` when it had no earlier one). */
  restored: string;
}

/** `review_export`'s request. */
export interface ReviewExportRequestDto {
  sheetId: string;
  /** Directory to write `review-<sheet>.csv` into; omitted = preview only. */
  outDir?: string;
}

/** `review.csv` and where it landed. */
export interface ReviewExportDto {
  csv: string;
  decisions: number;
  seq: number;
  path?: string;
}

// ---- Actions and their keys ---------------------------------------------

/** Every triage action, in the order §3.6's bar and the counts array use. */
export const TRIAGE_ACTIONS = ["approve", "reject", "flag", "duplicate", "bulk-approve"] as const;

/** A triage action's canonical name — also the string `review.csv` writes. */
export type TriageActionName = (typeof TRIAGE_ACTIONS)[number];

/** The one-line chip label for each action. */
export const TRIAGE_LABELS: Record<TriageActionName, string> = {
  approve: "approved",
  reject: "rejected",
  flag: "flagged",
  duplicate: "duplicate",
  "bulk-approve": "approved · bulk",
};

/** The keys §3.6 binds, for the help line and the shortcut table. */
export const TRIAGE_KEYS: Record<TriageActionName, string> = {
  approve: "A",
  reject: "R",
  flag: "F",
  duplicate: "D",
  "bulk-approve": "Shift+A",
};

/**
 * The action a triage keystroke means, or `null`.
 *
 * Modifiers are deliberately strict: `Ctrl+R` reloads a browser tab and
 * `Cmd+A` selects a page, so a command that also accepted them would either
 * never fire or fire while the user was doing something else entirely. Only the
 * bare letters count, and `bulk-approve` requires the shift §3.6 gives it.
 */
export function actionForKey(key: string, shift = false): TriageActionName | null {
  const base = key.length === 1 ? key.toLowerCase() : key;
  switch (base) {
    case "a":
      return shift ? "bulk-approve" : "approve";
    case "r":
      return shift ? null : "reject";
    case "f":
      return shift ? null : "flag";
    case "d":
      return shift ? null : "duplicate";
    default:
      return null;
  }
}

/**
 * The action behind a name, a CSV spelling or a key — the same table
 * `TriageActionDto::parse` accepts, so the browser mock and the workspace agree
 * with the native side about what `"A"` and `"approved"` mean.
 */
export function parseAction(text: string): TriageActionName | null {
  switch (text.trim().toLowerCase()) {
    case "approve":
    case "approved":
    case "a":
      return "approve";
    case "reject":
    case "rejected":
    case "r":
      return "reject";
    case "flag":
    case "flagged":
    case "f":
      return "flag";
    case "duplicate":
    case "dup":
    case "d":
      return "duplicate";
    case "bulk-approve":
    case "bulk_approve":
    case "bulk":
    case "shift+a":
      return "bulk-approve";
    default:
      return null;
  }
}

/** What one keystroke asks the workspace to do. */
export type ReviewIntent =
  | { kind: "decide"; action: TriageActionName }
  | { kind: "undo" }
  | { kind: "overlay" }
  | { kind: "move"; delta: 1 | -1 }
  | { kind: "dismiss" }
  | { kind: "export" };

/** The subset of a keyboard event [`keyIntent`] reads. */
export interface KeyPress {
  key: string;
  shiftKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
}

/**
 * Maps one keystroke to one intent, or `null` when the workspace should let it
 * through (typing in a field, a browser shortcut, an unbound key).
 */
export function keyIntent(press: KeyPress): ReviewIntent | null {
  const { key } = press;
  const ctrl = press.ctrlKey === true || press.metaKey === true;
  const mod = ctrl || press.altKey === true;
  if (ctrl && key.toLowerCase() === "z") return { kind: "undo" };
  if (mod) return null;
  if (key === " ") return { kind: "overlay" };
  if (key === "ArrowDown" || key === "j") return { kind: "move", delta: 1 };
  if (key === "ArrowUp" || key === "k") return { kind: "move", delta: -1 };
  if (key === "Escape") return { kind: "dismiss" };
  if (press.shiftKey === true && key.length === 1 && key.toLowerCase() === "e") {
    return { kind: "export" };
  }
  const action = actionForKey(key, press.shiftKey === true);
  return action ? { kind: "decide", action } : null;
}

/**
 * True when a keystroke belongs to a text field rather than to the workspace.
 *
 * Keyboard triage is bound at the window, so without this guard typing a path
 * into the export box would approve icons on every "a".
 */
export function isTypingTarget(
  tagName: string | undefined,
  isContentEditable = false,
): boolean {
  if (isContentEditable) return true;
  const tag = (tagName ?? "").toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select";
}

// ---- Filters and ordering ------------------------------------------------

/** The filter bar's tabs, in the order they are shown. */
export const REVIEW_FILTERS = [
  "all",
  "pending",
  "flagged",
  "duplicates",
  "outliers",
  "done",
] as const;

/** One filter tab. */
export type ReviewFilter = (typeof REVIEW_FILTERS)[number];

/** The tab's label, and what it means in one phrase. */
export const REVIEW_FILTER_LABELS: Record<ReviewFilter, string> = {
  all: "All",
  pending: "Undecided",
  flagged: "Quality flags",
  duplicates: "Duplicates",
  outliers: "Outliers",
  done: "Decided",
};

/** True when an icon is still waiting for a human. */
export function isDecided(icon: ReviewIconDto): boolean {
  return icon.state !== "pending";
}

/** True when the icon belongs on the given tab. */
export function rowMatches(icon: ReviewIconDto, filter: ReviewFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "pending":
      return !isDecided(icon);
    case "done":
      return isDecided(icon);
    case "flagged":
      return icon.flags.length > 0;
    case "duplicates":
      return icon.cluster !== undefined;
    case "outliers":
      return icon.outliers.length > 0;
  }
}

/** The icons a tab holds, in the order given. */
export function filterRows(icons: ReviewIconDto[], filter: ReviewFilter): ReviewIconDto[] {
  return icons.filter((icon) => rowMatches(icon, filter));
}

/** What each tab would show, for the counts on the tabs. */
export function filterCounts(icons: ReviewIconDto[]): Record<ReviewFilter, number> {
  const counts = { all: 0, pending: 0, flagged: 0, duplicates: 0, outliers: 0, done: 0 };
  for (const icon of icons) {
    for (const filter of REVIEW_FILTERS) {
      if (rowMatches(icon, filter)) counts[filter] += 1;
    }
  }
  return counts;
}

/** How the list is sorted. */
export type ReviewOrder = "sheet" | "attention";

/**
 * The attention score an icon sorts by: lower is more likely to need a human.
 *
 * A cluster member that is not its keeper is a *proposed duplicate* — the
 * cascade's output, which is what a reviewer must rule on — so it leads. Then
 * quality flags, then deviations, then everything still undecided; decided
 * rows sink to the bottom in either order.
 */
function attentionRank(icon: ReviewIconDto): number {
  if (icon.cluster !== undefined && !icon.keeper) return 0;
  if (icon.flags.length > 0) return 1;
  if (icon.outliers.length > 0) return 2;
  if (!isDecided(icon)) return 3;
  return 4;
}

/**
 * The rows as the list renders them.
 *
 * `sheet` is the pass's own order (the triage log's row order, which is what
 * the export reads); `attention` is a stable re-sort on top of it, ties broken
 * by index so two runs of the same report show the same list.
 */
export function orderRows(icons: ReviewIconDto[], order: ReviewOrder): ReviewIconDto[] {
  const rows = [...icons];
  if (order === "sheet") return rows;
  return rows.sort((a, b) => attentionRank(a) - attentionRank(b) || a.index - b.index);
}

/**
 * The id the cursor moves to, or `focus` when there is nowhere to go.
 *
 * `skipDecided` is what makes a triage pass a pass: after a decision the
 * cursor steps over everything already decided instead of walking the list one
 * row at a time. It never wraps — reaching the end and stopping is how the
 * workspace says "that is the sheet".
 */
export function nextRow(
  rows: ReviewIconDto[],
  focus: string | null,
  delta: 1 | -1,
  skipDecided = true,
): string | null {
  if (rows.length === 0) return null;
  const start = rows.findIndex((icon) => icon.id === focus);
  if (start === -1) {
    // No cursor yet: enter the list from the end the movement came from.
    const edge = delta > 0 ? 0 : rows.length - 1;
    if (!skipDecided) return rows[edge].id;
    for (let i = edge; i >= 0 && i < rows.length; i += delta) {
      if (!isDecided(rows[i])) return rows[i].id;
    }
    return rows[edge].id;
  }
  for (let i = start + delta; i >= 0 && i < rows.length; i += delta) {
    if (!skipDecided || !isDecided(rows[i])) return rows[i].id;
  }
  return focus;
}

// ---- Local state patching ------------------------------------------------

/**
 * The state string a decision leaves in the row — the action's own name, which
 * is exactly what `state_name` reads back off the log.
 */
export function stateAfter(action: TriageActionName): string {
  return action;
}

/**
 * The same report with one row's state replaced (and its cluster untouched).
 *
 * Used after a decision's response lands and after an undo, whose `restored`
 * names the state to put back. Rows are matched by id, never by position: the
 * attention order moves rows around, and the id is the only thing that did not
 * move.
 */
export function patchState(review: ReviewOutDto, iconId: string, state: string): ReviewOutDto {
  let touched = false;
  const icons = review.icons.map((icon) => {
    if (icon.id !== iconId || icon.state === state) return icon;
    touched = true;
    return { ...icon, state };
  });
  return touched ? { ...review, icons } : review;
}

/**
 * The unchanged report with one row's state cleared back to pending.
 *
 * The mock's undo path needs this, and so does a reader replaying an export:
 * both want "this row is undecided again" without re-deriving the log.
 */
export function clearState(review: ReviewOutDto, iconId: string): ReviewOutDto {
  return patchState(review, iconId, "pending");
}

// ---- Cluster and outlier helpers -----------------------------------------

/** The other members of an icon's cluster — the rows a reviewer compares it to. */
export function clusterPeers(cluster: ReviewClusterDto | undefined, iconId: string): string[] {
  if (!cluster) return [];
  return cluster.members.filter((id) => id !== iconId);
}

/** True when the icon is a proposed duplicate of another row rather than a keeper. */
export function isDuplicateCandidate(icon: ReviewIconDto): boolean {
  return icon.cluster !== undefined && !icon.keeper;
}

/** One cluster in a sentence: how many members, whether they are copies. */
export function clusterLabel(cluster: ReviewClusterDto): string {
  const size = `${cluster.members.length} icons`;
  return cluster.identical ? `${size} · byte-identical` : `${size} · variants`;
}

// ---- Lines and formatting ------------------------------------------------

/** `31.0 kB` — the size of the export a state would write. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  return kb < 1024 ? `${kb.toFixed(1)} kB` : `${(kb / 1024).toFixed(1)} MB`;
}

/** `1.3 ms`, `17.4 s` — a duration at the scale a reviewer reads. */
export function formatMs(ms: number): string {
  if (ms < 1000) return `${ms.toFixed(1)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

/** `0.968` — a score to three places, the precision the bars are quoted at. */
export function formatScore(value: number): string {
  return value.toFixed(3);
}

/** `96.9 %` — a rate for the summary line. */
export function formatPercent(value: number): string {
  return `${(value * 100).toFixed(1)} %`;
}

/** Renders a `u64` hash that arrived as 16-char hex, or a short digest. */
export function shortHash(hash: string, keep = 4): string {
  if (hash.length <= keep * 2) return hash;
  return `${hash.slice(0, keep)}…${hash.slice(-keep)}`;
}

/** The cascade's funnel, as the header shows it. */
export function cascadeLine(cascade: ReviewCascadeDto, icons: number): string {
  const perIcon = icons > 0 ? `${(cascade.verified / icons).toFixed(2)}× icons` : "—";
  return (
    `${cascade.candidates.toLocaleString("en-US")} band pairs → ` +
    `${cascade.verified.toLocaleString("en-US")} verified → ` +
    `${cascade.confirmed.toLocaleString("en-US")} confirmed ` +
    `(${perIcon})`
  );
}

/** What the pass found, in one line, for the workspace header. */
export function detectorLine(review: ReviewOutDto): string {
  return (
    `${review.icons.length} icons · ${review.clusters.length} clusters · ` +
    `${review.flagged} flagged · ${review.outliers.length} deviations · ` +
    `${review.skipped.length} skipped · render ${formatMs(review.renderMs)} · ` +
    `detect ${formatMs(review.detectMs)}`
  );
}

/** What the reviewer has done so far, in one line. */
export function progressLine(triage: ReviewTriageDto, total: number): string {
  const [approve, reject, flag, duplicate, bulk] = triage.counts;
  const parts = [
    `${approve + bulk} approved`,
    `${reject} rejected`,
    `${flag} flagged`,
    `${duplicate} duplicate`,
  ];
  return (
    `${triage.decided} of ${total} decided · ${parts.join(" · ")} · ` +
    `export ${formatBytes(triage.csvBytes)}`
  );
}

/** A reviewer's own pace against §8's twenty-minute budget. */
export interface PaceReport {
  /** Milliseconds per decision so far; 0 before the first one. */
  perDecisionMs: number;
  /** Milliseconds the whole sheet projects to at that pace. */
  projectedMs: number;
  projectedMinutes: number;
  /** Milliseconds left of the budget (negative once it is gone). */
  remainingMs: number;
  /** True when the projection still fits the budget. */
  withinBudget: boolean;
  line: string;
}

/** §8's exit criterion, as a number the workspace can show: 20 minutes per sheet. */
export const TRIAGE_BUDGET_MS = 20 * 60 * 1000;

/**
 * Projects the current pace onto the whole sheet.
 *
 * This is *the reviewer's own* pace, not the 3-user criterion §8 asks for —
 * only a real pass measures that — but it is the one number that tells a
 * reviewer halfway through whether they are going to make the budget, and it is
 * arithmetic, so it belongs in a tested function rather than in a component.
 */
export function budgetLine(
  decided: number,
  total: number,
  elapsedMs: number,
  budgetMs = TRIAGE_BUDGET_MS,
): PaceReport {
  const perDecisionMs = decided > 0 ? elapsedMs / decided : 0;
  const projectedMs = perDecisionMs * Math.max(total, decided);
  const projectedMinutes = projectedMs / 60000;
  const remainingMs = budgetMs - elapsedMs;
  const withinBudget = decided === 0 || projectedMs <= budgetMs;
  const line =
    decided === 0
      ? `pace: waiting for the first decision · budget ${(budgetMs / 60000).toFixed(0)} min`
      : `pace: ${(perDecisionMs / 1000).toFixed(2)} s/icon · projected ` +
        `${projectedMinutes.toFixed(1)} min of ${(budgetMs / 60000).toFixed(0)} min · ` +
        `${withinBudget ? "within" : "over"} budget`;
  return { perDecisionMs, projectedMs, projectedMinutes, remainingMs, withinBudget, line };
}

/** The keyboard help line: the spec's six chords, spelled out. */
export const KEY_HELP = [
  ["A", "approve"],
  ["R", "reject"],
  ["F", "flag"],
  ["D", "duplicate"],
  ["Space", "overlay"],
  ["Shift+A", "approve rest"],
  ["Ctrl+Z", "undo"],
  ["↑ ↓", "move"],
  ["Esc", "close"],
] as const;
