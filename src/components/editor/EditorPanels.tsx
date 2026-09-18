import { useEffect, useState } from "react";

import { useEditor } from "../../state/editorStore";
import { ALIGN_EDGES, type AlignEdge } from "../../wasm/abi";

/** The glyphs the align buttons show, in the ABI's edge order. */
const EDGE_LABEL: Record<AlignEdge, string> = {
  left: "⇤",
  hcenter: "↔",
  right: "⇥",
  top: "⇡",
  vcenter: "↕",
  bottom: "⇣",
};

const EDGE_HINT: Record<AlignEdge, string> = {
  left: "Line up left edges",
  hcenter: "Line up horizontal centres",
  right: "Line up right edges",
  top: "Line up top edges",
  vcenter: "Line up vertical centres",
  bottom: "Line up bottom edges",
};

function PanelButton({
  label,
  hint,
  onClick,
  disabled,
  testId,
  active,
}: {
  label: string;
  hint: string;
  onClick: () => void;
  disabled?: boolean;
  testId: string;
  active?: boolean;
}) {
  return (
    <button
      type="button"
      data-testid={testId}
      title={hint}
      disabled={disabled}
      onClick={onClick}
      className={`rounded border px-1.5 py-1 text-xs hover:border-forge-accent hover:text-forge-accent disabled:opacity-40 disabled:hover:border-forge-edge disabled:hover:text-forge-text ${
        active
          ? "border-forge-accent text-forge-accent"
          : "border-forge-edge text-forge-text"
      }`}
    >
      {label}
    </button>
  );
}

/** A labelled cluster of controls in the panel rail. */
function Panel({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-1.5 rounded border border-forge-edge/60 px-2 py-1">
      <span className="mr-0.5 text-[10px] uppercase tracking-wide text-forge-dim">{title}</span>
      {children}
    </div>
  );
}

/**
 * Phase 4B panels: arrange, align, transform, groups and snapping.
 *
 * Every control is one ABI command (or one setting the canvas reads) — none of
 * them computes geometry, which is the engine's job. The controls follow the
 * selection: with nothing selected they are disabled rather than silently
 * failing, and a refusal from the engine still lands in the status line.
 */
export function EditorPanels() {
  const status = useEditor((s) => s.status);
  const selection = useEditor((s) => s.selection);
  const selectionBox = useEditor((s) => s.selectionBox);
  const snap = useEditor((s) => s.snap);
  const align = useEditor((s) => s.align);
  const arrange = useEditor((s) => s.arrange);
  const group = useEditor((s) => s.group);
  const ungroup = useEditor((s) => s.ungroup);
  const resize = useEditor((s) => s.resize);
  const rotate = useEditor((s) => s.rotate);
  const setSnap = useEditor((s) => s.setSnap);

  const [frame, setFrame] = useState<"selection" | "canvas">("selection");
  const [size, setSize] = useState({ width: "", height: "" });

  // The size fields track the selection box until the user types into them.
  useEffect(() => {
    if (!selectionBox) {
      setSize({ width: "", height: "" });
      return;
    }
    const round = (value: number): string => String(Math.round(value * 100) / 100);
    setSize({
      width: round(selectionBox.x1 - selectionBox.x0),
      height: round(selectionBox.y1 - selectionBox.y0),
    });
  }, [selectionBox]);

  const groupCount = useEditor((s) => {
    const ids = new Set<number>();
    for (const node of s.nodes) if (node.group) ids.add(node.group);
    return ids.size;
  });

  const ready = status === "ready";
  const nothing = !ready || selection.length === 0;

  return (
    <div
      data-testid="editor-panels"
      className="flex flex-wrap items-center gap-2 border-b border-forge-edge px-3 py-1.5"
    >
      <Panel title="Arrange">
        <PanelButton
          testId="editor-front"
          label="Front"
          hint="Bring the selection to the front"
          disabled={nothing}
          onClick={() => arrange("front")}
        />
        <PanelButton
          testId="editor-back"
          label="Back"
          hint="Send the selection to the back"
          disabled={nothing}
          onClick={() => arrange("back")}
        />
      </Panel>

      <Panel title="Align">
        <PanelButton
          testId="editor-align-frame-selection"
          label="to selection"
          hint="Align the selection's own nodes to each other"
          disabled={nothing}
          active={frame === "selection"}
          onClick={() => setFrame("selection")}
        />
        <PanelButton
          testId="editor-align-frame-canvas"
          label="to canvas"
          hint="Align the selection to the canvas"
          disabled={nothing}
          active={frame === "canvas"}
          onClick={() => setFrame("canvas")}
        />
        {ALIGN_EDGES.map((edge) => (
          <PanelButton
            key={edge}
            testId={`editor-align-${edge}`}
            label={EDGE_LABEL[edge]}
            hint={EDGE_HINT[edge]}
            disabled={nothing}
            onClick={() => align(frame, edge)}
          />
        ))}
      </Panel>

      <Panel title="Transform">
        <input
          data-testid="editor-size-width"
          aria-label="Selection width"
          className="w-14 rounded border border-forge-edge bg-forge-panel px-1 py-0.5 text-[11px] text-forge-text"
          value={size.width}
          disabled={nothing}
          onChange={(event) => setSize((s) => ({ ...s, width: event.target.value }))}
        />
        <span className="text-[10px] text-forge-dim">×</span>
        <input
          data-testid="editor-size-height"
          aria-label="Selection height"
          className="w-14 rounded border border-forge-edge bg-forge-panel px-1 py-0.5 text-[11px] text-forge-text"
          value={size.height}
          disabled={nothing}
          onChange={(event) => setSize((s) => ({ ...s, height: event.target.value }))}
        />
        <PanelButton
          testId="editor-size-apply"
          label="Size"
          hint="Resize the selection to that width and height"
          disabled={nothing}
          onClick={() =>
            resize({
              width: Number(size.width) || undefined,
              height: Number(size.height) || undefined,
            })
          }
        />
        <PanelButton
          testId="editor-rotate-left"
          label="↺ 90°"
          hint="Rotate the selection 90° counter-clockwise about its centre"
          disabled={nothing}
          onClick={() => rotate(-90)}
        />
        <PanelButton
          testId="editor-rotate-right"
          label="↻ 90°"
          hint="Rotate the selection 90° clockwise about its centre"
          disabled={nothing}
          onClick={() => rotate(90)}
        />
      </Panel>

      <Panel title="Group">
        <PanelButton
          testId="editor-group"
          label="Group"
          hint="Group the selection, so a click selects all of it"
          disabled={nothing || selection.length < 2}
          onClick={group}
        />
        <PanelButton
          testId="editor-ungroup"
          label="Ungroup"
          hint="Break the selected nodes out of their groups"
          disabled={nothing || groupCount === 0}
          onClick={ungroup}
        />
        {groupCount > 0 ? (
          <span className="text-[10px] text-forge-dim">
            {groupCount} group{groupCount === 1 ? "" : "s"}
          </span>
        ) : null}
      </Panel>

      <Panel title="Snap">
        {(
          [
            ["canvas", "editor-snap-canvas", "To the canvas edges and centre lines"],
            ["nodes", "editor-snap-nodes", "To other nodes' edges and centres"],
            ["grid", "editor-snap-grid", "To a grid"],
          ] as const
        ).map(([key, testId, hint]) => (
          <label
            key={key}
            title={hint}
            className="flex cursor-pointer items-center gap-1 text-[11px] text-forge-text"
          >
            <input
              type="checkbox"
              data-testid={testId}
              checked={snap[key]}
              disabled={!ready}
              onChange={(event) => setSnap({ [key]: event.target.checked })}
            />
            {key}
          </label>
        ))}
        <label className="flex items-center gap-1 text-[11px] text-forge-text" title="Grid pitch">
          step
          <input
            type="number"
            data-testid="editor-snap-step"
            className="w-14 rounded border border-forge-edge bg-forge-panel px-1 py-0.5 text-[11px] text-forge-text"
            value={snap.gridStep}
            min={1}
            disabled={!ready}
            onChange={(event) => setSnap({ gridStep: Number(event.target.value) || 1 })}
          />
        </label>
        <label className="flex items-center gap-1 text-[11px] text-forge-text" title="How close counts">
          within
          <input
            type="number"
            data-testid="editor-snap-tolerance"
            className="w-14 rounded border border-forge-edge bg-forge-panel px-1 py-0.5 text-[11px] text-forge-text"
            value={snap.tolerance}
            min={0}
            disabled={!ready}
            onChange={(event) => setSnap({ tolerance: Math.max(0, Number(event.target.value) || 0) })}
          />
        </label>
      </Panel>
    </div>
  );
}
