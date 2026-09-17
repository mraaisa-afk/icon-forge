import { Comparator } from "./Comparator";
import { EditorGate } from "../editor/EditorGate";
import { GroupOverlay } from "./GroupOverlay";
import { useStore } from "../../state/store";
import { fmt } from "../../lib/compareModel";

function shortHash(hash: string): string {
  return hash.length > 10 ? `${hash.slice(0, 6)}…${hash.slice(-4)}` : hash;
}

/**
 * Sheet detail drawer: vectorize button (T2 job), the sheet's stored icon
 * rows, and the A/B comparator for the selected icon.
 */
export function SheetPanel() {
  const sheet = useStore((s) => s.selectedSheet);
  const icons = useStore((s) => s.icons);
  const comparing = useStore((s) => s.comparing);
  const sheetBusy = useStore((s) => s.sheetBusy);
  const closeSheet = useStore((s) => s.closeSheet);
  const vectorizeSheet = useStore((s) => s.vectorizeSheet);
  const selectIcon = useStore((s) => s.selectIcon);

  if (!sheet) return null;

  return (
    <aside
      data-testid="sheet-panel"
      className="flex w-[26rem] shrink-0 flex-col overflow-y-auto border-l border-forge-edge bg-forge-panel"
    >
      <div className="flex items-start justify-between gap-2 p-3">
        <div className="min-w-0">
          <div className="truncate text-sm text-forge-text" title={sheet.sourcePath}>
            {sheet.sourcePath.split(/[\\/]/).pop()}
          </div>
          <div className="text-[10px] text-forge-dim">
            {shortHash(sheet.contentHash)} · {sheet.width}×{sheet.height}
          </div>
        </div>
        <button
          type="button"
          onClick={closeSheet}
          className="rounded px-2 py-1 text-xs text-forge-dim hover:text-forge-text"
        >
          close
        </button>
      </div>

      <GroupOverlay />

      <EditorGate />

      <div className="px-3">
        <button
          type="button"
          data-testid="vectorize-sheet"
          disabled={sheetBusy}
          onClick={() => void vectorizeSheet()}
          className="w-full rounded bg-forge-accent px-3 py-2 text-sm font-medium text-forge-bg hover:opacity-90 disabled:opacity-50"
        >
          Vectorize sheet
        </button>
        <div className="mt-1 text-[10px] text-forge-dim">
          Batch job (T2) — segments, groups and vectorizes every icon through the cache.
        </div>
      </div>

      <div className="p-3">
        {icons.length === 0 ? (
          <div className="rounded border border-dashed border-forge-edge p-3 text-xs text-forge-dim">
            No icons yet — run “Vectorize sheet”, then pick an icon below to compare it
            against the original.
          </div>
        ) : (
          <div className="grid grid-cols-4 gap-2">
            {icons.map((icon) => {
              const active =
                comparing &&
                comparing.bbox[0] === icon.bbox[0] &&
                comparing.bbox[1] === icon.bbox[1] &&
                comparing.bbox[2] === icon.bbox[2] &&
                comparing.bbox[3] === icon.bbox[3];
              return (
                <button
                  key={icon.id}
                  type="button"
                  data-testid="icon-tile"
                  onClick={() => void selectIcon(icon.bbox)}
                  className={
                    "flex flex-col items-center rounded border p-1 text-[10px] " +
                    (active
                      ? "border-forge-accent text-forge-text"
                      : "border-forge-edge text-forge-dim hover:text-forge-text")
                  }
                  title={`bbox ${icon.bbox.join(", ")} · ${icon.preset}`}
                >
                  <span>
                    {icon.bbox[0]},{icon.bbox[1]}
                  </span>
                  <span>{fmt(icon.ssim)}</span>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {comparing && <Comparator sheet={sheet} bbox={comparing.bbox} />}
    </aside>
  );
}
