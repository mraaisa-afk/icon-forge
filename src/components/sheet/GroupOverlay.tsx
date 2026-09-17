import { useEffect, useRef, useState } from "react";
import type { Bbox } from "../../lib/compareModel";
import {
  GROUP_STATUS_COLORS,
  SENSITIVITY_SLIDERS,
  dragBox,
  groupColor,
  groupLabel,
  groupStatuses,
  isClick,
  marqueeSelection,
  overlaySummary,
  previewToSheet,
  type GroupingDto,
  type SensitivityDto,
} from "../../lib/groupModel";
import { useStore } from "../../state/store";

/** CSS pixel size of a group label; kept in sync with the canvas font. */
const LABEL_FONT = "600 11px ui-sans-serif, system-ui, sans-serif";

interface Marquee {
  start: [number, number];
  end: [number, number];
}

/**
 * The Group All overlay (W12).
 *
 * The backdrop is the normalized sheet the groups were measured on, so a box
 * drawn at `(x, y, w, h)` sheet pixels sits exactly on its icon. Interactions:
 *
 * * **Group All** — groups the sheet (`group_all`), reusing the cached mask.
 * * **click** a group — Split Here through the W9 watershed (`≤ 20 ms`).
 * * **drag** — marquee; **Group Selected** collapses the groups it hits.
 * * **sliders** — re-group from the same cached mask, no re-decode.
 */
export function GroupOverlay() {
  const sheet = useStore((s) => s.selectedSheet);
  const grouping = useStore((s) => s.grouping);
  const preview = useStore((s) => s.preview);
  const busy = useStore((s) => s.groupBusy);
  const note = useStore((s) => s.groupNote);
  const sensitivity = useStore((s) => s.sensitivity);
  const groupAll = useStore((s) => s.groupAll);
  const setSensitivity = useStore((s) => s.setSensitivity);
  const splitHere = useStore((s) => s.splitHere);
  const groupSelected = useStore((s) => s.groupSelected);
  const resetGrouping = useStore((s) => s.resetGrouping);

  const canvas = useRef<HTMLCanvasElement>(null);
  const surface = useRef<HTMLDivElement>(null);
  const [marquee, setMarquee] = useState<Marquee | null>(null);
  const [selection, setSelection] = useState<Bbox[]>([]);

  // A new sheet (or a fresh grouping) invalidates the marquee.
  useEffect(() => {
    setMarquee(null);
    setSelection([]);
  }, [sheet?.id, grouping?.sheetId]);

  const scale = preview && preview.sheetWidth > 0 ? preview.width / preview.sheetWidth : 1;

  // ---- canvas: group boxes, labels, warnings and grid guides -------------
  useEffect(() => {
    const cv = canvas.current;
    if (!cv || !preview) return;
    cv.width = preview.width;
    cv.height = preview.height;
    const ctx = cv.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, cv.width, cv.height);
    if (!grouping) return;

    // Lattice guides first, so boxes and labels draw on top.
    ctx.save();
    ctx.setLineDash([6, 6]);
    ctx.lineWidth = 1;
    ctx.strokeStyle = "rgba(255,255,255,0.35)";
    if (grouping.hint.gridX) {
      for (const v of grouping.hint.valleyX) {
        ctx.beginPath();
        ctx.moveTo(v * scale, 0);
        ctx.lineTo(v * scale, cv.height);
        ctx.stroke();
      }
    }
    if (grouping.hint.gridY) {
      for (const v of grouping.hint.valleyY) {
        ctx.beginPath();
        ctx.moveTo(0, v * scale);
        ctx.lineTo(cv.width, v * scale);
        ctx.stroke();
      }
    }
    ctx.restore();

    const statuses = groupStatuses(grouping);
    const selectedIdx = new Set(
      grouping.groups
        .map((_, i) => i)
        .filter((i) => selection.some((box) => overlaps(grouping, i, box))),
    );

    ctx.font = LABEL_FONT;
    ctx.textBaseline = "bottom";
    grouping.groups.forEach((group, i) => {
      const [x, y, w, h] = group.bbox;
      const rx = x * scale;
      const ry = y * scale;
      const rw = Math.max(1, w * scale);
      const rh = Math.max(1, h * scale);
      const status = statuses[i];
      // A warning overrides the group's own hue: the overlay's job here is to
      // make "look at this one" impossible to miss.
      const colour =
        status === "ok"
          ? selectedIdx.has(i)
            ? GROUP_STATUS_COLORS.selected
            : groupColor(i)
          : GROUP_STATUS_COLORS[status];
      ctx.lineWidth = status === "ok" ? 1.5 : 2.5;
      ctx.strokeStyle = colour;
      ctx.strokeRect(rx, ry, rw, rh);
      if (selectedIdx.has(i)) {
        ctx.fillStyle = "rgba(163, 230, 53, 0.18)";
        ctx.fillRect(rx, ry, rw, rh);
      }

      const label = groupLabel(grouping, i);
      const tw = ctx.measureText(label).width;
      const lx = Math.min(Math.max(1, rx), Math.max(1, cv.width - tw - 3));
      const ly = ry - 2 < 12 ? ry + 12 : ry - 2;
      ctx.lineWidth = 3;
      ctx.strokeStyle = "rgba(10, 12, 18, 0.85)";
      ctx.strokeText(label, lx, ly);
      ctx.fillStyle = colour;
      ctx.fillText(label, lx, ly);
    });
  }, [grouping, preview, scale, selection]);

  if (!sheet) return null;

  const hits = grouping && selection.length > 0 ? marqueeSelection(grouping.groups, selection[0]) : [];
  const statuses = grouping ? groupStatuses(grouping) : [];
  const warnings = grouping
    ? grouping.warnings.map((w) => ({ ...w, index: w.group, stale: w.group >= grouping.groups.length }))
    : [];

  /** Preview-pixel point from a pointer event. */
  const point = (e: React.PointerEvent): [number, number] => {
    const el = surface.current;
    if (!el || !preview) return [0, 0];
    const rect = el.getBoundingClientRect();
    const sx = preview.width / rect.width;
    const sy = preview.height / rect.height;
    return [(e.clientX - rect.left) * sx, (e.clientY - rect.top) * sy];
  };

  const onPointerDown = (e: React.PointerEvent) => {
    if (!preview || !grouping) return;
    (e.target as Element).setPointerCapture?.(e.pointerId);
    const p = point(e);
    setMarquee({ start: p, end: p });
  };

  const onPointerMove = (e: React.PointerEvent) => {
    if (!marquee) return;
    setMarquee({ start: marquee.start, end: point(e) });
  };

  const onPointerUp = (e: React.PointerEvent) => {
    if (!marquee || !preview) return;
    const end = point(e);
    const [sx, sy] = previewToSheet(preview, marquee.start[0], marquee.start[1]);
    const [ex, ey] = previewToSheet(preview, end[0], end[1]);
    setMarquee(null);
    if (isClick(marquee.start, end)) {
      void splitHere(sx, sy);
      return;
    }
    setSelection([dragBox([sx, sy], [ex, ey], preview.sheetWidth, preview.sheetHeight)]);
  };

  return (
    <section data-testid="group-overlay" className="border-b border-forge-edge p-3">
      <div className="flex items-center justify-between gap-2">
        <div className="text-xs font-medium text-forge-text">Group All</div>
        <div className="flex gap-1">
          <button
            type="button"
            data-testid="group-all"
            disabled={busy}
            onClick={() => void groupAll()}
            className="rounded bg-forge-accent px-2 py-1 text-[11px] font-medium text-forge-bg hover:opacity-90 disabled:opacity-50"
          >
            Group All
          </button>
          <button
            type="button"
            data-testid="group-selected"
            disabled={busy || hits.length < 2}
            onClick={() => void groupSelected(selection)}
            className="rounded border border-forge-edge px-2 py-1 text-[11px] text-forge-text hover:border-forge-accent disabled:opacity-40"
          >
            Group Selected{hits.length > 0 ? ` (${hits.length})` : ""}
          </button>
          <button
            type="button"
            data-testid="group-reset"
            disabled={busy || !grouping || grouping.manualEdits === 0}
            onClick={() => void resetGrouping()}
            className="rounded border border-forge-edge px-2 py-1 text-[11px] text-forge-dim hover:text-forge-text disabled:opacity-40"
          >
            Reset
          </button>
        </div>
      </div>

      {preview ? (
        <div
          ref={surface}
          data-testid="group-surface"
          className="relative mt-2 touch-none select-none rounded border border-forge-edge bg-[#12151c]"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
        >
          <img
            src={`data:image/png;base64,${preview.png}`}
            alt="sheet"
            draggable={false}
            className="block w-full"
          />
          <canvas
            data-testid="group-canvas"
            className="pointer-events-none absolute inset-0 h-full w-full"
          />
          {marquee && preview && (
            <div
              className="pointer-events-none absolute border"
              style={{
                borderColor: GROUP_STATUS_COLORS.marquee,
                background: "rgba(232, 121, 249, 0.12)",
                left: `${(Math.min(marquee.start[0], marquee.end[0]) / preview.width) * 100}%`,
                top: `${(Math.min(marquee.start[1], marquee.end[1]) / preview.height) * 100}%`,
                width: `${(Math.abs(marquee.end[0] - marquee.start[0]) / preview.width) * 100}%`,
                height: `${(Math.abs(marquee.end[1] - marquee.start[1]) / preview.height) * 100}%`,
              }}
            />
          )}
        </div>
      ) : (
        <div className="mt-2 rounded border border-dashed border-forge-edge p-2 text-[10px] text-forge-dim">
          Rendering the sheet preview — the first look at a sheet decodes and
          segments it once; Group All is instant afterwards.
        </div>
      )}

      {grouping && (
        <div className="mt-2 space-y-2" data-testid="group-report">
          <div className="text-[11px] text-forge-text" data-testid="group-summary">
            {overlaySummary(grouping)}
            {grouping.maskCacheHit ? " · cached mask" : " · fresh mask"}
            {grouping.manualEdits > 0 ? ` · ${grouping.manualEdits} manual edit(s)` : ""}
          </div>
          <div className="text-[10px] text-forge-dim">{grouping.statusLine}</div>
          <div className="text-[10px] text-forge-dim">
            click a group to split · drag to marquee, then Group Selected
          </div>
          {note && (
            <div className="text-[10px] text-forge-accent" data-testid="group-note">
              {note}
            </div>
          )}
          {warnings.length > 0 && (
            <ul className="space-y-0.5" data-testid="group-warnings">
              {warnings.map((w, i) => (
                <li key={`${w.group}-${i}`} className="text-[10px]" style={{ color: GROUP_STATUS_COLORS[w.kind] }}>
                  #{w.group} {statuses[w.group] === w.kind ? "" : "(index moved) "}
                  {w.label}
                </li>
              ))}
            </ul>
          )}
          <div className="space-y-1 border-t border-forge-edge pt-2">
            {SENSITIVITY_SLIDERS.map((spec) => {
              const value = sensitivity ? sensitivity[spec.key] : grouping.sensitivity[spec.key];
              return (
                <label key={spec.key} className="flex items-center gap-2 text-[10px] text-forge-dim">
                  <span className="w-28 shrink-0">{spec.label}</span>
                  <input
                    type="range"
                    data-testid={`slider-${spec.key}`}
                    min={spec.min}
                    max={spec.max}
                    step={spec.step}
                    value={value}
                    disabled={busy}
                    onChange={(e) =>
                      void setSensitivity({
                        [spec.key]: Number(e.target.value),
                      } as Partial<SensitivityDto>)
                    }
                    className="h-1 flex-1 accent-forge-accent"
                  />
                  <span className="w-14 shrink-0 text-right text-forge-text">
                    {spec.format(value)}
                  </span>
                </label>
              );
            })}
            <div className="text-[10px] text-forge-dim">
              {grouping.stats.input} components → {grouping.stats.output} groups ·{" "}
              {grouping.stats.merges} merged · {grouping.stats.restored} restored ·{" "}
              {grouping.stats.flagged} flagged · refine {grouping.stats.elapsedMs.toFixed(1)} ms
            </div>
          </div>
        </div>
      )}
    </section>
  );
}

/** True when group `i` of `dto` overlaps `box` (sheet space). */
function overlaps(dto: GroupingDto, i: number, box: Bbox): boolean {
  const b = dto.groups[i].bbox;
  return b[0] < box[0] + box[2] && box[0] < b[0] + b[2] && b[1] < box[1] + box[3] && box[1] < b[1] + b[3];
}
