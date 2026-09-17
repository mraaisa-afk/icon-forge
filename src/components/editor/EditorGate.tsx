import { useEffect } from "react";

import { useEditor } from "../../state/editorStore";
import { useStore } from "../../state/store";

/**
 * The way into the editor.
 *
 * 4A loads the icons the sheet already has rows for (the tracer's cached
 * outlines); a sheet that has never been vectorized has nothing to edit yet, so
 * the button says so instead of opening an empty canvas.
 */
export function EditorGate() {
  const sheet = useStore((s) => s.selectedSheet);
  const icons = useStore((s) => s.icons);
  const status = useEditor((s) => s.status);
  const open = useEditor((s) => s.open);
  const close = useEditor((s) => s.close);

  // Leaving the sheet leaves the editor too: the session holds one document.
  useEffect(() => {
    if (status !== "closed") close();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sheet?.id]);

  const ready = icons.length > 0;
  return (
    <div className="px-3 pt-2">
      <button
        type="button"
        data-testid="open-editor"
        disabled={!sheet || !ready || status === "loading"}
        onClick={() => {
          if (!sheet) return;
          void open(
            sheet,
            icons.map((icon) => icon.bbox),
          );
        }}
        className="w-full rounded border border-forge-edge px-3 py-2 text-sm text-forge-text hover:border-forge-accent hover:text-forge-accent disabled:opacity-40 disabled:hover:border-forge-edge disabled:hover:text-forge-text"
      >
        {status === "loading" ? "Opening the editor…" : `Edit icons (${icons.length})`}
      </button>
      {!ready ? (
        <div className="mt-1 text-[10px] text-forge-dim">
          Vectorize the sheet first — the editor loads the traced outlines.
        </div>
      ) : null}
    </div>
  );
}
