import { EditorCanvas } from "./EditorCanvas";
import { EditorPanels } from "./EditorPanels";
import { EDITOR_ICON_LIMIT, useEditor } from "../../state/editorStore";

/** One toolbar button: the editor's whole command surface. */
function ToolButton({
  label,
  hint,
  onClick,
  disabled,
  testId,
}: {
  label: string;
  hint: string;
  onClick: () => void;
  disabled?: boolean;
  testId: string;
}) {
  return (
    <button
      type="button"
      data-testid={testId}
      title={hint}
      disabled={disabled}
      onClick={onClick}
      className="rounded border border-forge-edge px-2 py-1 text-xs text-forge-text hover:border-forge-accent hover:text-forge-accent disabled:opacity-40 disabled:hover:border-forge-edge disabled:hover:text-forge-text"
    >
      {label}
    </button>
  );
}

/**
 * Phase 4A workbench: one sheet's icons as editable nodes.
 *
 * Every button is one ABI command, and the two history buttons are the entire
 * undo surface — the engine keeps the history (bounded, with a dropped-step
 * count in the footer), so nothing here has to remember a document revision.
 */
export function EditorWorkbench() {
  const status = useEditor((s) => s.status);
  const error = useEditor((s) => s.error);
  const note = useEditor((s) => s.note);
  const nodes = useEditor((s) => s.nodes);
  const selection = useEditor((s) => s.selection);
  const history = useEditor((s) => s.history);
  const width = useEditor((s) => s.width);
  const height = useEditor((s) => s.height);
  const loadedIcons = useEditor((s) => s.loadedIcons);
  const availableIcons = useEditor((s) => s.availableIcons);
  const segmentCount = useEditor((s) => s.nodes.reduce((acc, node) => acc + node.path.reduce((n, p) => n + p.segs.length, 0), 0));
  const close = useEditor((s) => s.close);
  const undo = useEditor((s) => s.undo);
  const redo = useEditor((s) => s.redo);
  const selectAll = useEditor((s) => s.selectAll);
  const clearSelection = useEditor((s) => s.clearSelection);
  const cycleFill = useEditor((s) => s.cycleFill);
  const toggleVisible = useEditor((s) => s.toggleVisible);
  const duplicate = useEditor((s) => s.duplicate);
  const remove = useEditor((s) => s.remove);
  const apply = useEditor((s) => s.apply);
  const snap = useEditor((s) => s.snap);

  const noSelection = selection.length === 0;

  return (
    <section data-testid="editor-workbench" className="flex h-full min-h-0 flex-col bg-forge-bg">
      <header className="flex flex-wrap items-center gap-2 border-b border-forge-edge px-3 py-2">
        <span className="mr-1 text-xs font-medium text-forge-text">Editor</span>
        <ToolButton
          testId="editor-undo"
          label={history.undoLabel ? `Undo ${history.undoLabel}` : "Undo"}
          hint="Undo the last edit (Ctrl+Z)"
          disabled={!history.canUndo}
          onClick={undo}
        />
        <ToolButton
          testId="editor-redo"
          label={history.redoLabel ? `Redo ${history.redoLabel}` : "Redo"}
          hint="Redo the last undone edit (Ctrl+Shift+Z)"
          disabled={!history.canRedo}
          onClick={redo}
        />
        <span className="mx-1 h-4 w-px bg-forge-edge" />
        <ToolButton
          testId="editor-select-all"
          label="Select all"
          hint="Select every node (Ctrl+A)"
          disabled={status !== "ready" || nodes.length === 0}
          onClick={selectAll}
        />
        <ToolButton
          testId="editor-select-none"
          label="Deselect"
          hint="Clear the selection (Esc)"
          disabled={noSelection}
          onClick={clearSelection}
        />
        <span className="mx-1 h-4 w-px bg-forge-edge" />
        <ToolButton
          testId="editor-fill"
          label="Fill"
          hint="Cycle to the next fill colour"
          disabled={noSelection}
          onClick={cycleFill}
        />
        <ToolButton
          testId="editor-visible"
          label="Show/Hide"
          hint="Toggle visibility of the selection"
          disabled={noSelection}
          onClick={toggleVisible}
        />
        <ToolButton
          testId="editor-duplicate"
          label="Duplicate"
          hint="Copy the selection, offset by 8 px"
          disabled={noSelection}
          onClick={duplicate}
        />
        <ToolButton
          testId="editor-center"
          label="Center"
          hint="Centre the selection on the canvas"
          disabled={noSelection}
          onClick={() => apply({ kind: "center" })}
        />
        <ToolButton
          testId="editor-nudge"
          label="Nudge +1"
          hint="Move the selection one pixel down-right (arrow keys move by 1, Shift by 10)"
          disabled={noSelection}
          onClick={() => apply({ kind: "translate", dx: 1, dy: 1 })}
        />
        <ToolButton
          testId="editor-delete"
          label="Delete"
          hint="Delete the selection (Del)"
          disabled={noSelection}
          onClick={remove}
        />
        <span className="flex-1" />
        <ToolButton
          testId="editor-close"
          label="Close editor"
          hint="Leave the editor (unsaved edits are discarded in 4A)"
          onClick={close}
        />
      </header>

      <EditorPanels />

      <div className="min-h-0 flex-1">
        {status === "ready" ? (
          <EditorCanvas />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center text-forge-dim">
            <div className="text-3xl">⚒</div>
            <div className="text-sm">
              {status === "loading"
                ? "Loading the editor module…"
                : error
                  ? ownMessage(error)
                  : "The editor is not open."}
            </div>
            {error?.includes("not found") || error?.includes("Failed to fetch") ? (
              <div className="max-w-md text-xs">
                The WebAssembly editor module is missing from this build. Build it with{" "}
                <code className="rounded bg-forge-panel px-1">
                  cargo build --release --target wasm32-unknown-unknown -p isg-wasm
                </code>{" "}
                and copy the artifact to <code className="rounded bg-forge-panel px-1">public/</code>.
              </div>
            ) : null}
          </div>
        )}
      </div>

      <footer className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-forge-edge px-3 py-1.5 text-[10px] text-forge-dim">
        <span data-testid="editor-status">{note ?? error ?? (status === "ready" ? "ready" : status)}</span>
        <span className="flex-1" />
        <span>
          {nodes.length} node{nodes.length === 1 ? "" : "s"} · {segmentCount} segments
        </span>
        <span>{selection.length} selected</span>
        <span>
          {loadedIcons}
          {availableIcons > loadedIcons ? `/${availableIcons}` : ""} icon
          {loadedIcons === 1 ? "" : "s"} loaded
          {availableIcons > EDITOR_ICON_LIMIT ? ` (4A loads the first ${EDITOR_ICON_LIMIT})` : ""}
        </span>
        <span data-testid="editor-snap-state">
          snap {snapFilter("canvas", snap.canvas)} {snapFilter("nodes", snap.nodes)}{" "}
          {snap.grid ? `grid ${snap.gridStep}` : snapFilter("grid", false)} · {snap.tolerance} u
        </span>
        <span>
          history {history.undoable}/{history.redoable}
          {history.dropped > 0 ? ` · ${history.dropped} dropped` : ""}
        </span>
        <span>
          canvas {width}×{height}
        </span>
      </footer>
    </section>
  );
}

/** A snap family as the footer shows it: on, or struck through when off. */
function snapFilter(name: string, on: boolean): string {
  return on ? name : `${name}✗`;
}

/** Trims the browser's fetch noise down to the part the user needs. */
function ownMessage(error: string): string {
  return error.replace(/^editor module unavailable: /, "");
}
