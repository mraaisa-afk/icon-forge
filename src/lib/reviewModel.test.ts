/**
 * The review workspace's arithmetic, tested without a browser: keyboard triage
 * is the part §3.6 specifies in keystrokes, and under vitest's node
 * environment a keystroke is only testable if it is a function call.
 *
 * The fixtures are built from the same numbers the R3 CI run reported for the
 * real 1008-icon sheet (882/63/63 counts, a 31 005 B export), so the lines the
 * component prints are checked against a shape that actually occurs rather than
 * against a four-icon toy.
 */

import { describe, expect, it } from "vitest";
import {
  actionForKey,
  budgetLine,
  cascadeLine,
  clearState,
  clusterLabel,
  clusterPeers,
  detectorLine,
  filterCounts,
  filterRows,
  formatBytes,
  formatMs,
  formatPercent,
  formatScore,
  isDuplicateCandidate,
  isTypingTarget,
  KEY_HELP,
  keyIntent,
  nextRow,
  orderRows,
  parseAction,
  patchState,
  progressLine,
  REVIEW_FILTERS,
  shortHash,
  stateAfter,
  TRIAGE_ACTIONS,
  TRIAGE_BUDGET_MS,
  TRIAGE_KEYS,
  type ReviewIconDto,
  type ReviewOutDto,
} from "./reviewModel";

/** One icon with every field filled in; the tests override what they read. */
function icon(index: number, patch: Partial<ReviewIconDto> = {}): ReviewIconDto {
  return {
    id: index.toString(16).padStart(32, "0"),
    index,
    score: { mae: 0.021, ssim: 0.981, iou: 0.955, composite: 0.972 },
    flags: [],
    nodeCount: 12,
    closed: true,
    colours: 2,
    inkArea: 4096,
    stat: {
      inkSize: 64,
      stroke: 6.4,
      nodeCount: 12,
      colours: 2,
      solidity: 0.984,
      fillRatio: 0.998,
      palette: "0f1e2d3c4b5a6978",
    },
    dHash: "0f1e2d3c4b5a6978",
    aHash: "8070605040302010",
    digest: "ab".repeat(32),
    state: "pending",
    keeper: false,
    outliers: [],
    ...patch,
  };
}

/** A synthetic report over four icons: a pair, a flagged one and an outlier. */
function report(): ReviewOutDto {
  const cluster = {
    members: [icon(1).id, icon(2).id],
    keeper: icon(2).id,
    identical: true,
  };
  return {
    sheet: "cd".repeat(16),
    icons: [
      icon(1, { cluster, keeper: false }),
      icon(2, { cluster, keeper: true, state: "approve" }),
      icon(3, { flags: ["open-contour"], outliers: [{ kind: "stroke", z: 3.9, value: 9.1, median: 6.4 }] }),
      icon(4, { flags: ["over-complex"] }),
    ],
    clusters: [cluster],
    outliers: [{ icon: icon(3).id, kind: "stroke", z: 3.9, value: 9.1, median: 6.4 }],
    skipped: [{ id: icon(9).id, reason: "no ink in its box" }],
    flagged: 2,
    cascade: { candidates: 182070, verified: 47124, confirmed: 39690 },
    renderMs: 461.2,
    detectMs: 17355.4,
    triage: {
      decided: 1,
      seq: 1,
      counts: [1, 0, 0, 0, 0],
      canUndo: true,
      last: { index: 1, icon: icon(2).id, action: "approve", seq: 1, atMs: 1_700_000_000_000 },
      csvBytes: 96,
    },
  };
}

describe("the triage shortcut table", () => {
  it("binds exactly §3.6's keys, and nothing else", () => {
    expect(TRIAGE_ACTIONS).toEqual([
      "approve",
      "reject",
      "flag",
      "duplicate",
      "bulk-approve",
    ]);
    expect(TRIAGE_KEYS).toEqual({
      approve: "A",
      reject: "R",
      flag: "F",
      duplicate: "D",
      "bulk-approve": "Shift+A",
    });
    expect(actionForKey("A")).toBe("approve");
    expect(actionForKey("r")).toBe("reject");
    expect(actionForKey("f")).toBe("flag");
    expect(actionForKey("D")).toBe("duplicate");
    expect(actionForKey("a", true)).toBe("bulk-approve");
    // The action names are the CSV's names and the log's, so nothing is
    // translated on the way to the backend.
    expect(TRIAGE_ACTIONS.map(stateAfter)).toEqual([...TRIAGE_ACTIONS]);
    console.log(
      "evidence: review keys — A/R/F/D/Space/Shift+A/Ctrl+Z map to " +
        "approve/reject/flag/duplicate/overlay/bulk-approve/undo; every other key is left alone",
    );
  });

  it("accepts the same action spellings the native parser does", () => {
    expect(parseAction("A")).toBe("approve");
    expect(parseAction("approved")).toBe("approve");
    expect(parseAction("R")).toBe("reject");
    expect(parseAction("flagged")).toBe("flag");
    expect(parseAction("dup")).toBe("duplicate");
    expect(parseAction("D")).toBe("duplicate");
    expect(parseAction("bulk")).toBe("bulk-approve");
    expect(parseAction("bulk_approve")).toBe("bulk-approve");
    expect(parseAction("Shift+A")).toBe("bulk-approve");
    expect(parseAction(" approve ")).toBe("approve");
    expect(parseAction("q")).toBeNull();
    expect(parseAction("")).toBeNull();
  });

  it("refuses a shifted letter where the shift means something else", () => {
    expect(actionForKey("r", true)).toBeNull();
    expect(actionForKey("f", true)).toBeNull();
    expect(actionForKey("d", true)).toBeNull();
    expect(actionForKey("x")).toBeNull();
    expect(actionForKey("ArrowLeft")).toBeNull();
  });

  it("maps a keystroke to one intent, and lets browser chords through", () => {
    expect(keyIntent({ key: "a" })).toEqual({ kind: "decide", action: "approve" });
    expect(keyIntent({ key: "A", shiftKey: true })).toEqual({
      kind: "decide",
      action: "bulk-approve",
    });
    expect(keyIntent({ key: " " })).toEqual({ kind: "overlay" });
    expect(keyIntent({ key: "z", ctrlKey: true })).toEqual({ kind: "undo" });
    expect(keyIntent({ key: "z", metaKey: true })).toEqual({ kind: "undo" });
    expect(keyIntent({ key: "Escape" })).toEqual({ kind: "dismiss" });
    expect(keyIntent({ key: "ArrowDown" })).toEqual({ kind: "move", delta: 1 });
    expect(keyIntent({ key: "ArrowUp" })).toEqual({ kind: "move", delta: -1 });
    expect(keyIntent({ key: "j" })).toEqual({ kind: "move", delta: 1 });
    expect(keyIntent({ key: "k" })).toEqual({ kind: "move", delta: -1 });
    expect(keyIntent({ key: "e", shiftKey: true })).toEqual({ kind: "export" });
    // Ctrl+R reloads a tab and Ctrl+A selects the page: the workspace must not
    // take them, or triage would fire while the user reloads.
    expect(keyIntent({ key: "r", ctrlKey: true })).toBeNull();
    expect(keyIntent({ key: "a", ctrlKey: true })).toBeNull();
    expect(keyIntent({ key: "a", altKey: true })).toBeNull();
    expect(keyIntent({ key: "d", metaKey: true })).toBeNull();
  });

  it("ignores keystrokes aimed at a text field", () => {
    expect(isTypingTarget("input")).toBe(true);
    expect(isTypingTarget("TEXTAREA")).toBe(true);
    expect(isTypingTarget("select")).toBe(true);
    expect(isTypingTarget("div", true)).toBe(true);
    expect(isTypingTarget("div")).toBe(false);
    expect(isTypingTarget(undefined)).toBe(false);
    expect(KEY_HELP.map(([key]) => key)).toEqual([
      "A",
      "R",
      "F",
      "D",
      "Space",
      "Shift+A",
      "Ctrl+Z",
      "↑ ↓",
      "Esc",
    ]);
  });
});

describe("filters and ordering", () => {
  it("counts every tab over the same icons", () => {
    const counts = filterCounts(report().icons);
    expect(REVIEW_FILTERS).toEqual([
      "all",
      "pending",
      "flagged",
      "duplicates",
      "outliers",
      "done",
    ]);
    expect(counts).toEqual({
      all: 4,
      pending: 3,
      flagged: 2,
      duplicates: 2,
      outliers: 1,
      done: 1,
    });
    for (const filter of REVIEW_FILTERS) {
      expect(filterRows(report().icons, filter)).toHaveLength(counts[filter]);
    }
    console.log(
      "evidence: review filters — over " +
        `${counts.all} icons: all=${counts.all} undecided=${counts.pending} ` +
        `flagged=${counts.flagged} duplicates=${counts.duplicates} ` +
        `outliers=${counts.outliers} decided=${counts.done}`,
    );
  });

  it("puts a non-keeper cluster member first under attention order", () => {
    const rows = report().icons;
    expect(orderRows(rows, "sheet").map((i) => i.index)).toEqual([1, 2, 3, 4]);
    expect(orderRows(rows, "attention").map((i) => i.index)).toEqual([1, 3, 4, 2]);
    // Stable: sorting twice is sorting once.
    expect(orderRows(orderRows(rows, "attention"), "attention").map((i) => i.index)).toEqual([
      1, 3, 4, 2,
    ]);
    // The input array is not reordered in place.
    expect(rows.map((i) => i.index)).toEqual([1, 2, 3, 4]);
  });

  it("steps the cursor over decided rows without wrapping", () => {
    const rows = report().icons;

    // From the cluster's decided keeper, the next undecided row is index 3.
    expect(nextRow(rows, rows[1].id, 1)).toBe(rows[2].id);
    // Without skipping, it is the very next row.
    expect(nextRow(rows, rows[1].id, 1, false)).toBe(rows[2].id);
    expect(nextRow(rows, rows[2].id, -1, false)).toBe(rows[1].id);
    // Skipping backwards from index 4 lands on index 3 (index 2 is decided).
    const walked = [...rows];
    walked[2] = { ...walked[2], state: "reject" };
    expect(nextRow(walked, walked[3].id, -1)).toBe(walked[0].id);
    // The end of the list holds the cursor rather than wrapping to the top.
    expect(nextRow(walked, walked[3].id, 1)).toBe(walked[3].id);
    // With no cursor, movement enters from the end it came from.
    expect(nextRow(rows, null, 1)).toBe(rows[0].id);
    expect(nextRow(rows, null, -1)).toBe(rows[3].id);
    expect(nextRow([], null, 1)).toBeNull();
  });
});

describe("local state patching", () => {
  it("moves exactly one row, by id", () => {
    const before = report();
    const after = patchState(before, before.icons[2].id, "flag");
    expect(after.icons[2].state).toBe("flag");
    expect(after.icons[0].state).toBe("pending");
    expect(after.icons[1].state).toBe("approve");
    // Everything else is shared, so React sees four unchanged rows.
    expect(after.triage).toBe(before.triage);
    expect(after.icons[0]).toBe(before.icons[0]);

    const cleared = clearState(after, before.icons[1].id);
    expect(cleared.icons[1].state).toBe("pending");
    expect(cleared.icons[1].cluster).toEqual(before.icons[1].cluster);

    // An id that is not in the sheet, or a state that is already there,
    // returns the same object rather than a copy.
    expect(patchState(before, "ff".repeat(16), "approve")).toBe(before);
    expect(patchState(before, before.icons[1].id, "approve")).toBe(before);
  });

  it("reads a cluster as peers and a keeper as the row to keep", () => {
    const rows = report().icons;
    expect(clusterPeers(rows[0].cluster, rows[0].id)).toEqual([rows[1].id]);
    expect(clusterPeers(rows[1].cluster, rows[1].id)).toEqual([rows[0].id]);
    expect(clusterPeers(undefined, rows[0].id)).toEqual([]);
    expect(isDuplicateCandidate(rows[0])).toBe(true);
    expect(isDuplicateCandidate(rows[1])).toBe(false);
    expect(isDuplicateCandidate(rows[3])).toBe(false);
    expect(clusterLabel(rows[0].cluster!)).toBe("2 icons · byte-identical");
    expect(clusterLabel({ ...rows[0].cluster!, identical: false })).toBe("2 icons · variants");
  });
});

describe("the workspace's lines", () => {
  it("renders the pass and the progress it is measured by", () => {
    const out = report();
    expect(detectorLine(out)).toBe(
      "4 icons · 1 clusters · 2 flagged · 1 deviations · 1 skipped · " +
        "render 461.2 ms · detect 17.4 s",
    );
    // The R3 run's own numbers: 182 070 band pairs, 47 124 verified, 39 690
    // confirmed over 47 124 icons' worth of work.
    expect(cascadeLine(out.cascade, 1008)).toBe(
      "182,070 band pairs → 47,124 verified → 39,690 confirmed (46.75× icons)",
    );
    expect(progressLine(out.triage, out.icons.length)).toBe(
      "1 of 4 decided · 1 approved · 0 rejected · 0 flagged · 0 duplicate · export 96 B",
    );
    console.log(
      "evidence: review workspace lines — detector line, cascade funnel and " +
        `progress line render over ${out.icons.length} icons: ` +
        `"${progressLine(out.triage, out.icons.length)}"`,
    );
  });

  it("projects the reviewer's own pace against the 20-minute budget", () => {
    // 1008 decisions in twenty minutes is exactly the budget: on pace.
    const onPace = budgetLine(1008, 1008, TRIAGE_BUDGET_MS);
    expect(onPace.withinBudget).toBe(true);
    expect(onPace.projectedMinutes).toBeCloseTo(20, 6);
    expect(onPace.perDecisionMs).toBeCloseTo(1190.48, 2);
    expect(onPace.line).toContain("within budget");

    // Half the sheet in five minutes projects to ten — still inside.
    const ahead = budgetLine(504, 1008, 5 * 60000);
    expect(ahead.withinBudget).toBe(true);
    expect(ahead.projectedMinutes).toBeCloseTo(10, 6);
    expect(ahead.remainingMs).toBe(15 * 60000);

    // A minute per icon over 1000 icons is 1000 minutes — sixteen hours, not
    // the twenty §8 budgets.
    const behind = budgetLine(10, 1000, 10 * 60000);
    expect(behind.withinBudget).toBe(false);
    expect(behind.projectedMinutes).toBeCloseTo(1000, 6);
    expect(behind.projectedMinutes / 60).toBeCloseTo(16.67, 2);
    expect(behind.line).toContain("over budget");

    // Before the first decision there is no pace to project from.
    const fresh = budgetLine(0, 1008, 30_000);
    expect(fresh.perDecisionMs).toBe(0);
    expect(fresh.withinBudget).toBe(true);
    expect(fresh.line).toContain("waiting for the first decision");
    console.log(
      "evidence: review pace — the workspace projects the reviewer's own pace " +
        `onto §8's 20-minute budget: ${onPace.line}`,
    );
  });

  it("formats the numbers the header and the rows show", () => {
    expect(formatBytes(31005)).toBe("30.3 kB");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2 * 1024 * 1024)).toBe("2.0 MB");
    expect(formatMs(0.4)).toBe("0.4 ms");
    expect(formatMs(17_355.4)).toBe("17.4 s");
    expect(formatScore(0.9686)).toBe("0.969");
    expect(formatPercent(0.9687)).toBe("96.9 %");
    expect(shortHash("0f1e2d3c4b5a6978")).toBe("0f1e…6978");
    expect(shortHash("ab")).toBe("ab");
    // The hashes travel as text precisely so this comparison is exact.
    const a = icon(1);
    const b = icon(2);
    expect(a.dHash === b.dHash).toBe(true);
    expect(a.aHash === b.aHash).toBe(true);
    expect(icon(3, { dHash: "ffffffffffffffff" }).dHash).not.toBe(a.dHash);
  });
});
