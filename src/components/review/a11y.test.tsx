/**
 * Accessibility of the triage surfaces (Phase 7).
 *
 * The review workspace exists to be driven from the keyboard — that is the
 * whole 1000-icons-in-20-minutes argument — so the semantics that make it
 * usable without sight are part of the product, not decoration: a named
 * landmark, a list of named rows, every glyph button carrying the action it
 * performs, the shortcut announced rather than hidden in a tooltip, and the
 * progress line as a live region so deciding an icon *says* something.
 *
 * These tests render to static markup: that is exactly the layer being checked
 * (what the accessibility tree is built from), and it needs no DOM.
 */

// @vitest-environment jsdom
//
// A DOM is needed for the workspace half: it reads live zustand state through
// hooks, and the store's *server* snapshot is the state it was created with, so
// `renderToStaticMarkup` can only ever render the initial screen (measured, not
// assumed). The row half is presentational and renders to static markup.

import { act, type ReactElement } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it } from "vitest";

import type { ReviewIconDto } from "../../lib/reviewModel";
import { TRIAGE_KEYS } from "../../lib/reviewModel";
import { type SheetDto } from "../../lib/backend";
import { useStore } from "../../state/store";
import { ReviewRow } from "./ReviewRow";
import { ReviewWorkspace } from "./ReviewWorkspace";

/** One row, shaped like a real pending icon (worst-first ordering puts it first). */
const ICON: ReviewIconDto = {
  id: "ab".repeat(16),
  index: 7,
  score: { ssim: 0.9812, iou: 0.9703, mae: 0.011, composite: 0.9777 },
  flags: ["over-complex"],
  nodeCount: 412,
  closed: true,
  colours: 2,
  inkArea: 1234,
  stat: {
    inkSize: 38.4,
    stroke: 3.2,
    nodeCount: 412,
    colours: 2,
    solidity: 0.77,
    fillRatio: 0.81,
    palette: "0011223344556677",
  },
  dHash: "0123456789abcdef",
  aHash: "fedcba9876543210",
  digest: "cd".repeat(32),
  state: "pending",
  keeper: true,
  outliers: [],
};

const SHEET: SheetDto = {
  id: "ab".repeat(16),
  sourcePath: "C:/icons/a11y_sheet.png",
  contentHash: "cd".repeat(32),
  width: 256,
  height: 192,
  importedAt: "0",
};

/** Renders through the client path, so hooks see the state the test set. */
function renderToDom(node: ReactElement): string {
  // React only batches updates inside `act` when it is told it is in a test;
  // without this the state updates below warn on every render.
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  const html = container.innerHTML;
  act(() => {
    root.unmount();
  });
  container.remove();
  return html;
}

function renderRow(overrides: Partial<ReviewIconDto> = {}, selected = false): string {
  return renderToStaticMarkup(
    <ReviewRow
      icon={{ ...ICON, ...overrides }}
      selected={selected}
      onSelect={() => {}}
      onDecide={() => {}}
      onOverlay={() => {}}
    />,
  );
}

describe("review row semantics", () => {
  it("describes the icon, its state and whether the cursor is on it", () => {
    expect(renderRow()).toContain('role="listitem"');
    expect(renderRow()).toContain("icon 7, pending");
    expect(renderRow({}, true)).toContain(", selected");
    expect(renderRow({ state: "approve" })).toContain("icon 7, approve");
  });

  it("gives every glyph button the action and the icon it applies to", () => {
    const html = renderRow();
    const actions = ["approve", "reject", "flag", "duplicate"] as const;
    for (const action of actions) {
      expect(html).toContain(`aria-label="${action} icon 7"`);
      // The shortcut is announced, not just tooltipped.
      expect(html).toContain(`aria-keyshortcuts="${TRIAGE_KEYS[action]}"`);
    }
    // The overlay button is a glyph (⤢) with no text of its own.
    expect(html).toContain('aria-label="sheet crop overlay for icon 7"');
    expect(html).toContain('aria-keyshortcuts="Space"');
  });

  it("reports the row's own decision as pressed, and only that one", () => {
    const approved = renderRow({ state: "approve" });
    const pressed = approved.match(/aria-pressed="true"/g) ?? [];
    expect(pressed).toHaveLength(1);
    expect(approved).toContain('aria-pressed="false"');
  });
});

describe("review workspace semantics", () => {
  beforeEach(async () => {
    const s = useStore.getState();
    await s.createProject("C:/tmp/a11y.isgproj");
    await s.openSheet(SHEET);
    await useStore.getState().vectorizeSheet();
    await useStore.getState().reloadIcons();
    await useStore.getState().openReview();
  });

  it("is a landmark, lists the icons it is showing, and announces progress", () => {
    const html = renderToDom(<ReviewWorkspace />);
    expect(html).toContain('aria-label="Review workspace"');
    expect(html).toContain('role="group" aria-label="Review filters"');
    // The list is labelled with what is on the current tab (the rows themselves
    // are pinned by the row tests above: they are listitems once the virtual
    // window has a viewport, which a DOM under test does not provide).
    expect(html).toContain('role="list"');
    expect(html).toContain('aria-label="icons under review');
  });

  it("announces progress, which is the only feedback the keyboard loop gives", () => {
    const html = renderToDom(<ReviewWorkspace />);
    const progress = html.slice(html.indexOf('data-testid="review-progress"'));
    expect(progress.length).toBeGreaterThan(0);
    expect(progress.slice(0, 200)).toContain('role="status"');
    expect(progress.slice(0, 200)).toContain('aria-live="polite"');
  });
});
