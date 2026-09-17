/**
 * Store wiring for the Group All overlay, exercised through the browser mock
 * backend (the same code path a plain `vite dev` session uses, and the only
 * backend available under vitest).
 */

import { beforeEach, describe, expect, it } from "vitest";
import type { SheetDto } from "../lib/backend";
import { useStore } from "./store";

const SHEET: SheetDto = {
  id: "ab".repeat(16),
  sourcePath: "C:/icons/sheet_a.png",
  contentHash: "cd".repeat(32),
  width: 256,
  height: 192,
  importedAt: "0",
};

async function openMockSheet(): Promise<void> {
  const s = useStore.getState();
  await s.createProject("C:/tmp/grouping.isgproj");
  await s.openSheet(SHEET);
}

describe("grouping store actions (mock backend)", () => {
  beforeEach(async () => {
    await openMockSheet();
  });

  it("groups the sheet, then re-groups from the cached mask when a slider moves", async () => {
    await useStore.getState().groupAll();
    const first = useStore.getState().grouping;
    expect(first).not.toBeNull();
    expect(first!.groups).toHaveLength(12);
    expect(first!.maskCacheHit).toBe(false);
    expect(useStore.getState().sensitivity).toEqual(first!.sensitivity);

    await useStore.getState().setSensitivity({ mergeAreaRatio: 3 });
    const second = useStore.getState().grouping;
    expect(second!.groups).toHaveLength(11);
    expect(second!.maskCacheHit).toBe(true);
    expect(second!.sensitivity.mergeAreaRatio).toBe(3);
    expect(useStore.getState().groupNote).toContain("sliders");

    // The slider request is clamped into the native envelope.
    await useStore.getState().setSensitivity({ mergeGapFrac: 5 });
    expect(useStore.getState().sensitivity!.mergeGapFrac).toBe(1);
  });

  it("splits on a click, merges on a marquee, and resets manual edits", async () => {
    await useStore.getState().groupAll();
    const grouped = useStore.getState().grouping!;
    const tile = grouped.groups[0].bbox;

    await useStore.getState().splitHere(tile[0] + tile[2] / 2, tile[1] + tile[3] / 2);
    const split = useStore.getState().grouping!;
    expect(split.groups).toHaveLength(13);
    expect(split.manualEdits).toBe(1);
    expect(useStore.getState().groupNote).toContain("split group 0 into 2");

    // A marquee over the two halves of that tile re-merges them.
    const marquee: [number, number, number, number] = [tile[0] - 1, tile[1] - 1, tile[2] + 2, tile[3] + 2];
    await useStore.getState().groupSelected([marquee]);
    expect(useStore.getState().grouping!.groups).toHaveLength(12);
    expect(useStore.getState().groupNote).toContain("grouped the marquee");

    // Reset drops the edits by re-grouping from the cached mask.
    await useStore.getState().resetGrouping();
    const reset = useStore.getState().grouping!;
    expect(reset.manualEdits).toBe(0);
    expect(reset.groups).toHaveLength(12); // back to the automatic grouping
  });

  it("loads the preview once per sheet and clears the overlay on close", async () => {
    expect(useStore.getState().preview).toBeNull();
    await useStore.getState().loadPreview();
    const preview = useStore.getState().preview;
    expect(preview).not.toBeNull();
    expect(preview!.sheetWidth).toBe(256);
    expect(preview!.width).toBeLessThanOrEqual(256);

    useStore.getState().clearGrouping();
    expect(useStore.getState().grouping).toBeNull();
    expect(useStore.getState().preview).toBeNull();
  });
});
