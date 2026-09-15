import { useEffect, useRef, useState } from "react";
import { backend, type SheetDto } from "../../lib/backend";
import {
  fmt,
  grade,
  iconKey,
  PRESETS,
  wipeClips,
  type Bbox,
} from "../../lib/compareModel";
import { useStore } from "../../state/store";

const MAX_PANE = 320;

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error(`decode failed: ${src.slice(0, 40)}…`));
    img.src = src;
  });
}

/**
 * A/B comparator: A = the icon's crop from the normalized sheet (the exact
 * pixels stage 8 scored), B = the vectorized SVG, rendered on a raw Canvas2D.
 * Modes: A only, split (wipe divider), B only.
 */
export function Comparator({ sheet, bbox }: { sheet: SheetDto; bbox: Bbox }) {
  const preset = useStore((s) => s.comparing?.preset ?? "flat-8");
  const view = useStore((s) => s.view);
  const wipe = useStore((s) => s.wipe);
  const result = useStore((s) => s.vectorized[iconKey(bbox, preset)]);
  const setPreset = useStore((s) => s.setPreset);
  const setView = useStore((s) => s.setView);
  const setWipe = useStore((s) => s.setWipe);

  const [cropUrl, setCropUrl] = useState<string | null>(null);
  const [cropError, setCropError] = useState<string | null>(null);
  const canvas = useRef<HTMLCanvasElement>(null);

  const [x, y, w, h] = bbox;

  // A-side: base64 PNG crop from the native segmenter.
  useEffect(() => {
    let alive = true;
    setCropUrl(null);
    setCropError(null);
    void backend()
      .then((b) => b.sheetCrop(sheet.id, x, y, w, h))
      .then((b64) => {
        if (alive) setCropUrl(`data:image/png;base64,${b64}`);
      })
      .catch((e) => {
        if (alive) setCropError(String(e));
      });
    return () => {
      alive = false;
    };
  }, [sheet.id, x, y, w, h]);

  // Canvas render: B below, A clipped on top of the left wipe fraction.
  useEffect(() => {
    const cv = canvas.current;
    if (!cv) return;
    cv.width = w;
    cv.height = h;
    const ctx = cv.getContext("2d");
    if (!ctx) return;
    let alive = true;
    ctx.imageSmoothingEnabled = false;
    ctx.clearRect(0, 0, w, h);

    void (async () => {
      const clips = wipeClips(wipe, w, h);
      const svgUrl = result
        ? `data:image/svg+xml;charset=utf-8,${encodeURIComponent(result.svg)}`
        : null;
      try {
        if (view !== "a" && svgUrl) {
          const b = await loadImage(svgUrl);
          if (!alive) return;
          if (view === "b") {
            ctx.drawImage(b, 0, 0);
          } else {
            ctx.save();
            ctx.beginPath();
            ctx.rect(...clips.b);
            ctx.clip();
            ctx.drawImage(b, 0, 0);
            ctx.restore();
          }
        }
        if (view !== "b" && cropUrl) {
          const a = await loadImage(cropUrl);
          if (!alive) return;
          if (view === "a") {
            ctx.drawImage(a, 0, 0);
          } else {
            ctx.save();
            ctx.beginPath();
            ctx.rect(...clips.a);
            ctx.clip();
            ctx.drawImage(a, 0, 0);
            ctx.restore();
          }
        }
        if (view === "split") {
          const dx = Math.min(Math.max(clips.a[2] - 1, 0), w - 1);
          ctx.fillStyle = "#ffb020";
          ctx.fillRect(dx, 0, 2, h);
        }
      } catch {
        // A side that fails to decode simply does not draw (error shown below).
      }
    })();

    return () => {
      alive = false;
    };
  }, [cropUrl, result, view, wipe, w, h]);

  const scale = Math.max(1, Math.floor(MAX_PANE / Math.max(w, h)));
  const score = result?.score;
  const verdict = score ? grade(score.composite) : null;

  return (
    <div className="flex flex-col gap-2 border-t border-forge-edge p-3">
      <div className="flex items-center justify-between">
        <div className="text-xs text-forge-dim">
          icon @ {x},{y} · {w}×{h}
        </div>
        {result?.cached && (
          <span className="rounded bg-forge-edge px-1.5 py-0.5 text-[10px] text-forge-dim">
            cached
          </span>
        )}
      </div>

      <div
        className="flex items-center justify-center rounded border border-forge-edge p-2"
        style={{ backgroundImage: "repeating-conic-gradient(#20242e 0% 25%, #262b38 0% 50%)", backgroundSize: "16px 16px" }}
      >
        <canvas
          ref={canvas}
          data-testid="comparator-canvas"
          style={{
            width: w * scale,
            height: h * scale,
            imageRendering: "pixelated",
          }}
        />
      </div>

      <div className="flex items-center gap-1 text-xs">
        {(["a", "split", "b"] as const).map((m) => (
          <button
            key={m}
            type="button"
            onClick={() => setView(m)}
            className={
              "rounded px-2 py-1 uppercase " +
              (view === m
                ? "bg-forge-accent text-forge-bg"
                : "bg-forge-panel text-forge-dim hover:text-forge-text")
            }
          >
            {m}
          </button>
        ))}
        <input
          aria-label="wipe"
          type="range"
          min={0}
          max={1000}
          value={Math.round(wipe * 1000)}
          disabled={view !== "split"}
          onChange={(e) => setWipe(Number(e.target.value) / 1000)}
          className="ml-2 flex-1 accent-forge-accent disabled:opacity-40"
        />
      </div>

      <label className="flex items-center gap-2 text-xs text-forge-dim">
        preset
        <select
          value={preset}
          onChange={(e) => void setPreset(e.target.value)}
          className="flex-1 rounded border border-forge-edge bg-forge-panel px-1 py-1 text-forge-text"
        >
          {PRESETS.map((p) => (
            <option key={p.name} value={p.name}>
              {p.label}
            </option>
          ))}
        </select>
      </label>

      {cropError && <div className="text-xs text-rose-400">A-side crop failed: {cropError}</div>}

      {score && verdict ? (
        <div data-testid="score-overlay" className="rounded bg-forge-panel p-2 text-xs">
          <div className="flex items-baseline justify-between">
            <span className={verdict.className}>
              {verdict.label} · {fmt(score.composite)}
            </span>
            <span className="text-forge-dim">{preset}</span>
          </div>
          <div className="mt-1 grid grid-cols-3 gap-1 text-forge-dim">
            <span>mae {fmt(score.mae)}</span>
            <span>ssim {fmt(score.ssim)}</span>
            <span>iou {fmt(score.iou)}</span>
          </div>
        </div>
      ) : (
        <div className="text-xs text-forge-dim">vectorizing…</div>
      )}
    </div>
  );
}
