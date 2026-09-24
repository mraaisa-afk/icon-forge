/**
 * The review workspace's store slice, driven through the browser mock backend
 * — the same code path a plain `vite dev` session uses, and the only backend
 * available under vitest.
 *
 * The mock's review pass is synthetic but shaped like the real one (a duplicate
 * pair, a flagged icon with a deviation, an over-complex icon), so these tests
 * exercise the branches the native report's output takes: a cluster member that
 * is not its keeper, a `pending` row, and a row whose state an undo restores.
 */

import { beforeEach, describe, expect, it } from "vitest";
import { backend, type SheetDto } from "../lib/backend";
import { useStore } from "./store";

const SHEET: SheetDto = {
  id: "ab".repeat(16),
  sourcePath: "C:/icons/review_sheet.png",
  contentHash: "cd".repeat(32),
  width: 256,
  height: 192,
  importedAt: "0",
};

/** Opens the sheet, vectorizes it (the mock creates four rows) and reviews it. */
async function openReview(): Promise<void> {
  const s = useStore.getState();
  await s.createProject("C:/tmp/review.isgproj");
  await s.openSheet(SHEET);
  await useStore.getState().vectorizeSheet();
  await useStore.getState().reloadIcons();
  await useStore.getState().openReview();
}

const reviewed = () => useStore.getState();
const iconAt = (row: number) => reviewed().review!.icons[row];

describe("review workspace store actions (mock backend)", () => {
  beforeEach(async () => {
    await openReview();
  });

  it("runs the pass, opens the workspace and puts the cursor on the worst row", () => {
    const s = reviewed();
    expect(s.reviewOpen).toBe(true);
    expect(s.review).not.toBeNull();
    expect(s.review!.icons).toHaveLength(4);
    expect(s.review!.clusters).toHaveLength(1);
    expect(s.review!.clusters[0].identical).toBe(true);
    expect(s.review!.flagged).toBe(2);
    expect(s.review!.outliers).toHaveLength(1);
    expect(s.review!.outliers[0].icon).toBe(iconAt(2).id);
    expect(s.review!.triage.decided).toBe(0);
    expect(s.review!.triage.canUndo).toBe(false);
    // Attention order leads with the cluster member that is not the keeper.
    expect(s.reviewOrder).toBe("attention");
    expect(s.reviewSelected).toBe(iconAt(0).id);
    expect(s.reviewNote).toContain("4 icons reviewed");
    // The pass reports the log it found, and the cheap read agrees with it.
    expect(s.reviewLog!.decided).toBe(0);
    expect(s.reviewLog!.csvBytes).toBe(s.review!.triage.csvBytes);
  });

  it("reads the sheet's log, and decides, without a rendered pass", async () => {
    const target = iconAt(2);
    useStore.setState({ review: null, reviewLog: null });
    await reviewed().refreshReviewState();
    expect(reviewed().reviewLog!.decided).toBe(0);
    expect(reviewed().review).toBeNull();

    // A decision is a journal row, not a render: it works while the workspace
    // is closed and there is no report to patch.
    await reviewed().decideReview(target.id, "approve");
    expect(reviewed().review).toBeNull();
    expect(reviewed().reviewLog!.decided).toBe(1);
    expect(reviewed().reviewLog!.counts[0]).toBe(1);
    expect(reviewed().reviewLog!.canUndo).toBe(true);
    expect(reviewed().reviewNote).toContain("1 decided");

    await reviewed().undoReview();
    expect(reviewed().reviewLog!.decided).toBe(0);
    expect(reviewed().reviewLog!.counts[0]).toBe(0);
    expect(reviewed().reviewNote).toContain("pending");
  });

  it("records a decision, patches one row, and walks on to the next undecided", async () => {
    const target = iconAt(2);
    await reviewed().decideReview(target.id, "flag");
    const s = reviewed();
    expect(s.review!.icons[2].state).toBe("flag");
    expect(s.review!.icons[0].state).toBe("pending");
    // The log is the backend's: one decision, in the flag bucket (index 2).
    expect(s.review!.triage.decided).toBe(1);
    expect(s.review!.triage.counts[2]).toBe(1);
    expect(s.review!.triage.canUndo).toBe(true);
    expect(s.review!.triage.csvBytes).toBeGreaterThan(16);
    expect(s.reviewSelected).not.toBe(target.id);

    // The same row can be decided again; the log holds the newer action.
    await reviewed().decideReview(target.id, "duplicate");
    expect(reviewed().review!.icons[2].state).toBe("duplicate");
    expect(reviewed().review!.triage.decided).toBe(1);
    expect(reviewed().review!.triage.counts[3]).toBe(1);
    expect(reviewed().review!.triage.counts[2]).toBe(0);
  });

  it("undoes the last decision back to the state it replaced", async () => {
    const target = iconAt(2);
    await reviewed().decideReview(target.id, "flag");
    await reviewed().undoReview();
    let s = reviewed();
    expect(s.review!.icons[2].state).toBe("pending");
    expect(s.review!.triage.decided).toBe(0);
    expect(s.review!.triage.canUndo).toBe(false);
    expect(s.reviewSelected).toBe(target.id);
    expect(s.reviewNote).toContain("pending");

    // A second decision replaced by an undo restores the *first* one, not
    // `pending` — the restored state the command returns is what the row takes.
    await reviewed().decideReview(target.id, "flag");
    await reviewed().decideReview(target.id, "approve");
    await reviewed().undoReview();
    s = reviewed();
    expect(s.review!.icons[2].state).toBe("flag");
    expect(s.review!.triage.decided).toBe(1);
    expect(s.review!.triage.counts[2]).toBe(1);
    expect(s.review!.triage.counts[0]).toBe(0);

    // One more undo takes the flag back too: the history is a stack of
    // decisions, not a stack of *icons*.
    await reviewed().undoReview();
    expect(reviewed().review!.icons[2].state).toBe("pending");
    expect(reviewed().review!.triage.decided).toBe(0);
    expect(reviewed().review!.triage.canUndo).toBe(false);

    // With the history empty the undo is a note, not an error.
    await reviewed().undoReview();
    expect(reviewed().reviewNote).toContain("nothing left to undo");
    expect(reviewed().error).toBeNull();
    expect(reviewed().review!.triage.decided).toBe(0);
  });

  it("approves the rest in bulk, one decision each, and stops when none are left", async () => {
    const before = reviewed().review!.icons.filter((i) => i.state === "pending").length;
    await reviewed().bulkApproveReview();
    let s = reviewed();
    expect(s.review!.triage.decided).toBe(4);
    expect(s.review!.triage.counts[4]).toBe(before);
    expect(s.review!.icons.every((i) => i.state === "bulk-approve")).toBe(true);
    expect(s.reviewNote).toContain("approved 4 icon(s) in bulk");

    await reviewed().bulkApproveReview();
    expect(reviewed().reviewNote).toContain("nothing left undecided");
    expect(reviewed().error).toBeNull();
    expect(reviewed().review!.triage.decided).toBe(4);
  });

  it("exports review.csv and undoes across a re-read of the triage state", async () => {
    await reviewed().decideReview(iconAt(1).id, "reject");
    await reviewed().decideReview(iconAt(3).id, "approve");
    await reviewed().exportReview();
    const out = reviewed().reviewExported!;
    expect(out.decisions).toBe(2);
    expect(out.path).toBeUndefined();
    const lines = out.csv.split("\r\n");
    expect(lines[0]).toBe("seq,id,action,at_ms");
    // Header, two rows, and the empty tail the trailing CRLF leaves behind.
    expect(lines).toHaveLength(4);
    expect(lines[3]).toBe("");
    expect(out.csv).toContain(",reject,");
    expect(reviewed().reviewNote).toContain("preview · 2 decisions");

    // Writing it names the sheet's own file.
    await reviewed().exportReview("C:/tmp/review-out");
    expect(reviewed().reviewExported!.path).toBe(`C:/tmp/review-out/review-${SHEET.id}.csv`);

    // Re-reading the log after a reload gives the same counts.
    const before = reviewed().review!.triage;
    await reviewed().refreshReviewState();
    expect(reviewed().review!.triage).toEqual(before);

    console.log(
      "evidence: review workspace — decide, undo, bulk approve and export driven through the " +
        `store over the mock backend: 2 decisions export as ${out.csv.length} B, ` +
        "counts survive a re-read, and an empty history is a note rather than an error",
    );
  });

  it("filters, moves the cursor and pins the sheet crop for the overlay", async () => {
    await reviewed().decideReview(iconAt(2).id, "flag");
    // The decision moved the cursor to the next undecided row.
    const afterDecision = reviewed().reviewSelected!;
    expect(afterDecision).toBe(iconAt(3).id);

    reviewed().setReviewFilter("pending");
    // A tab that still has the cursor on screen keeps it: filtering must not
    // yank the reviewer somewhere else.
    expect(reviewed().reviewFilter).toBe("pending");
    expect(reviewed().reviewSelected).toBe(afterDecision);
    expect(reviewed().review!.icons.filter((i) => i.state === "pending")).toHaveLength(3);

    reviewed().moveReviewSelection(1);
    expect(reviewed().reviewSelected).toBe(iconAt(1).id);
    reviewed().moveReviewSelection(-1);
    expect(reviewed().reviewSelected).toBe(afterDecision);
    // Backwards past the top holds the cursor instead of wrapping.
    reviewed().moveReviewSelection(-1);
    reviewed().moveReviewSelection(-1);
    expect(reviewed().reviewSelected).toBe(iconAt(0).id);
    reviewed().moveReviewSelection(-1);
    expect(reviewed().reviewSelected).toBe(iconAt(0).id);

    // The duplicates tab holds the cluster pair, and the cursor is in it.
    reviewed().setReviewFilter("duplicates");
    expect(reviewed().review!.icons.filter((i) => i.cluster).length).toBe(2);
    expect(reviewed().reviewSelected).toBe(iconAt(0).id);
    reviewed().setReviewFilter("all");
    expect(reviewed().reviewOrder).toBe("attention");
    reviewed().setReviewOrder("sheet");
    expect(reviewed().review!.icons.map((i) => i.index)).toEqual([0, 1, 2, 3]);

    // Pinning one icon shows its own pixels from the sheet.
    await reviewed().toggleReviewOverlay(iconAt(2).id);
    expect(reviewed().reviewOverlay).toBe(iconAt(2).id);
    expect(reviewed().reviewCrop).not.toBeNull();
    expect(reviewed().reviewNote).toContain("nodes");

    // `Space` (no argument) puts the overlay away whatever is pinned, and
    // pressing it again opens the selected row's crop.
    reviewed().moveReviewSelection(1, false);
    const selected = reviewed().reviewSelected!;
    await reviewed().toggleReviewOverlay();
    expect(reviewed().reviewOverlay).toBeNull();
    expect(reviewed().reviewCrop).toBeNull();
    await reviewed().toggleReviewOverlay();
    expect(reviewed().reviewOverlay).toBe(selected);
    expect(reviewed().reviewCrop).not.toBeNull();
  });

  it("reopens the cached pass instead of paying for a second one", async () => {
    const first = reviewed().review;
    reviewed().closeReview();
    expect(reviewed().reviewOpen).toBe(false);
    expect(reviewed().review).toBe(first);
    await reviewed().openReview();
    expect(reviewed().review).toBe(first);
    // `force` is what the workspace's Re-run button uses.
    await reviewed().openReview(true);
    expect(reviewed().reviewOpen).toBe(true);
    expect(reviewed().review).not.toBe(first);
    expect(reviewed().review!.triage.decided).toBe(0);
  });

  it("drops the pass when the sheet closes", async () => {
    reviewed().closeSheet();
    const s = reviewed();
    expect(s.selectedSheet).toBeNull();
    expect(s.review).toBeNull();
    expect(s.reviewOpen).toBe(false);
    expect(s.reviewSelected).toBeNull();
    expect(s.reviewExported).toBeNull();
  });
});

describe("the browser fallback can reach the workspace", () => {
  it("imports synthetic sheets, so the pass has something to review", async () => {
    // Every screen behind the library grid — the comparator, the sheet wizard,
    // this workspace — is unreachable in a plain `vite dev` session unless the
    // mock's import actually produces sheets.
    const b = await backend();
    await b.projectCreate("C:/tmp/browser-fallback.isgproj");
    await b.importSubmit("C:/icons/set");
    await new Promise((resolve) => setTimeout(resolve, 800));
    const sheets = await b.librarySheets(0, 200);
    expect(sheets.map((sheet) => sheet.sourcePath.split("/").pop())).toEqual([
      "sheet_a.png",
      "sheet_b.png",
    ]);
    for (const sheet of sheets) {
      expect(sheet.id).toMatch(/^[0-9a-f]{32}$/);
      expect(sheet.width).toBeGreaterThan(0);
    }

    // The same folder imports the same ids: a demo that reshuffled them would
    // lose the triage log between reloads.
    await b.importSubmit("C:/icons/set");
    await new Promise((resolve) => setTimeout(resolve, 800));
    expect((await b.librarySheets(0, 200)).map((sheet) => sheet.id)).toEqual(
      sheets.map((sheet) => sheet.id),
    );
    console.log(
      "evidence: review workspace — the browser fallback imports 2 deterministic synthetic " +
        `sheets (${sheets[0].id.slice(0, 8)}…, ${sheets[1].id.slice(0, 8)}…), so the review ` +
        "workspace is reachable in a plain dev session without the native shell",
    );
  });
});
