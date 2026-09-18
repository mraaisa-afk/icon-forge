import { useState } from "react";

import { PRESETS } from "../../lib/compareModel";
import {
  ALL_FORMATS,
  DEFAULT_CSV,
  DEFAULT_CSV_COLUMNS,
  DEFAULT_FORMATS,
  DEFAULT_NAME_PATTERN,
  DEFAULT_SHEET_SPEC,
  PLACEMENTS,
  SHEET_SLIDERS,
  clampSpec,
  expandPattern,
  formatBytes,
  innerSide,
  planSizeLine,
  reportLine,
  targetInk,
  type SheetCsvDto,
  type SheetPlacementDto,
  type SheetSpecDto,
} from "../../lib/sheetModel";
import { useStore } from "../../state/store";

/** Where the source sheet lives, so an export has a sensible default home. */
function defaultOutDir(sourcePath: string): string {
  const cut = Math.max(sourcePath.lastIndexOf("/"), sourcePath.lastIndexOf("\\"));
  const dir = cut > 0 ? sourcePath.slice(0, cut) : "";
  return `${dir}/icons-export`;
}

/** The sheet's stem, as the native side derives it from the source path. */
function stem(sourcePath: string): string {
  const file = sourcePath.split(/[\\/]/).pop() ?? "sheet";
  return file.replace(/\.[^.]+$/, "");
}

/**
 * The sheet generator's wizard (§3.5): the cell spec, the leveling, the CSV
 * metadata and the exporters, in one card.
 *
 * The card never computes what it can ask for: the sheet size, the report and
 * the derived names all come back from `sheet_plan`, which is the same code
 * the export runs. `clampSpec`/`solveGrid` are only used for the *pre-request*
 * reads (the inner box and the target ink), which is what makes the sliders
 * feel live while the request is debounced by the user's own clicks.
 */
export function SheetExportCard() {
  const sheet = useStore((s) => s.selectedSheet);
  const grouping = useStore((s) => s.grouping);
  const plan = useStore((s) => s.sheetPlan);
  const csvPreview = useStore((s) => s.sheetCsv);
  const files = useStore((s) => s.sheetFiles);
  const busy = useStore((s) => s.sheetGenBusy);
  const note = useStore((s) => s.sheetNote);
  const planSheet = useStore((s) => s.planSheet);
  const previewSheetCsv = useStore((s) => s.previewSheetCsv);
  const exportSheet = useStore((s) => s.exportSheet);

  const [spec, setSpec] = useState<SheetSpecDto>(DEFAULT_SHEET_SPEC);
  const [preset, setPreset] = useState("flat-8");
  const [pattern, setPattern] = useState(DEFAULT_NAME_PATTERN);
  const [formats, setFormats] = useState<string[]>([...DEFAULT_FORMATS]);
  const [delimiter, setDelimiter] = useState(DEFAULT_CSV.delimiter);
  const [header, setHeader] = useState(DEFAULT_CSV.header);
  const [outDir, setOutDir] = useState("");

  if (!sheet) return null;

  const clamped = clampSpec(spec);
  const dir = outDir || defaultOutDir(sheet.sourcePath);
  const sheetStem = stem(sheet.sourcePath);
  const request = {
    sheetId: sheet.id,
    spec: clamped,
    sheetStem,
    preset,
    namePattern: pattern,
  };
  const csv: SheetCsvDto = { delimiter, header, columns: [...DEFAULT_CSV_COLUMNS] };
  const sample = expandPattern(pattern, {
    sheet: sheetStem,
    index: 1,
    row: 1,
    col: 1,
    preset,
  });

  const toggleFormat = (format: string): void =>
    setFormats((current) =>
      current.includes(format) ? current.filter((f) => f !== format) : [...current, format],
    );

  return (
    <section
      data-testid="sheet-export-card"
      className="border-t border-forge-edge px-3 py-3 text-xs text-forge-text"
    >
      <div className="mb-2 flex items-baseline justify-between">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-forge-dim">
          Sheet generator
        </h3>
        {grouping ? (
          <span className="text-[10px] text-forge-dim">{grouping.groups.length} grouped icons</span>
        ) : (
          <span className="text-[10px] text-forge-dim">run Group All first</span>
        )}
      </div>

      {SHEET_SLIDERS.map((slider) => (
        <label key={slider.key} className="mb-1 flex items-center gap-2 text-[10px] text-forge-dim">
          <span className="w-24 shrink-0" title={slider.hint}>
            {slider.label}
          </span>
          <input
            type="range"
            data-testid={`sheet-slider-${slider.key}`}
            min={slider.min}
            max={slider.max}
            step={slider.step}
            value={clamped[slider.key]}
            disabled={busy}
            onChange={(e) =>
              setSpec((current) => ({ ...current, [slider.key]: Number(e.target.value) }))
            }
            className="h-1 flex-1 accent-forge-accent"
          />
          <span className="w-12 shrink-0 text-right text-forge-text">{clamped[slider.key]}</span>
        </label>
      ))}

      <div className="mt-2 flex flex-wrap items-center gap-2 text-[10px] text-forge-dim">
        <select
          data-testid="sheet-placement"
          value={clamped.placement}
          disabled={busy}
          onChange={(e) =>
            setSpec((current) => ({
              ...current,
              placement: e.target.value as SheetSpecDto["placement"],
            }))
          }
          className="rounded border border-forge-edge bg-forge-bg px-1 py-0.5 text-forge-text"
        >
          {PLACEMENTS.map((p) => (
            <option key={p.id} value={p.id} title={p.hint}>
              {p.label}
            </option>
          ))}
        </select>
        <select
          data-testid="sheet-preset"
          value={preset}
          disabled={busy}
          onChange={(e) => setPreset(e.target.value)}
          className="rounded border border-forge-edge bg-forge-bg px-1 py-0.5 text-forge-text"
        >
          {PRESETS.map((p) => (
            <option key={p.name} value={p.name}>
              {p.label}
            </option>
          ))}
        </select>
        <label className="flex items-center gap-1">
          name
          <input
            data-testid="sheet-name-pattern"
            value={pattern}
            disabled={busy}
            onChange={(e) => setPattern(e.target.value)}
            className="w-40 rounded border border-forge-edge bg-forge-bg px-1 py-0.5 font-mono text-forge-text"
          />
        </label>
        <span data-testid="sheet-name-sample" className="text-forge-dim">
          → {sample}.svg
        </span>
      </div>

      <p className="mt-2 text-[10px] text-forge-dim">
        inner box {innerSide(clamped)} px · target ink {targetInk(clamped).toFixed(1)} px
      </p>

      <div className="mt-2 flex flex-wrap gap-2">
        <button
          type="button"
          data-testid="sheet-plan"
          disabled={busy || !grouping}
          onClick={() => void planSheet(request)}
          className="rounded bg-forge-accent px-2 py-1 text-[11px] font-medium text-forge-bg hover:opacity-90 disabled:opacity-50"
        >
          Plan
        </button>
        <button
          type="button"
          data-testid="sheet-csv-preview"
          disabled={busy || !grouping}
          onClick={() => void previewSheetCsv(request, csv)}
          className="rounded border border-forge-edge px-2 py-1 text-[11px] text-forge-text hover:border-forge-accent disabled:opacity-50"
        >
          CSV preview
        </button>
        <button
          type="button"
          data-testid="sheet-export"
          disabled={busy}
          onClick={() =>
            void exportSheet({ ...request, csv, formats, outDir: dir, rasterScale: 1 })
          }
          className="rounded border border-forge-edge px-2 py-1 text-[11px] text-forge-text hover:border-forge-accent disabled:opacity-50"
        >
          Export
        </button>
      </div>

      <div className="mt-2 flex flex-wrap items-center gap-2 text-[10px] text-forge-dim">
        {ALL_FORMATS.map((format) => (
          <label key={format} className="flex items-center gap-1">
            <input
              type="checkbox"
              data-testid={`sheet-format-${format}`}
              checked={formats.includes(format)}
              disabled={busy}
              onChange={() => toggleFormat(format)}
              className="accent-forge-accent"
            />
            {format}
          </label>
        ))}
        <label className="flex items-center gap-1">
          delimiter
          <input
            data-testid="sheet-delimiter"
            value={delimiter}
            maxLength={1}
            disabled={busy}
            onChange={(e) => setDelimiter(e.target.value || ",")}
            className="w-6 rounded border border-forge-edge bg-forge-bg px-1 text-center font-mono text-forge-text"
          />
        </label>
        <label className="flex items-center gap-1">
          <input
            type="checkbox"
            data-testid="sheet-header"
            checked={header}
            disabled={busy}
            onChange={(e) => setHeader(e.target.checked)}
            className="accent-forge-accent"
          />
          header row
        </label>
      </div>

      <label className="mt-2 flex items-center gap-1 text-[10px] text-forge-dim">
        into
        <input
          data-testid="sheet-out-dir"
          value={dir}
          disabled={busy}
          onChange={(e) => setOutDir(e.target.value)}
          className="min-w-0 flex-1 rounded border border-forge-edge bg-forge-bg px-1 py-0.5 font-mono text-forge-text"
        />
      </label>

      {note && (
        <p data-testid="sheet-note" className="mt-2 text-[10px] text-forge-dim">
          {note}
        </p>
      )}

      {plan && (
        <div className="mt-2 rounded border border-forge-edge p-2">
          <div data-testid="sheet-plan-size" className="text-[10px] text-forge-text">
            {planSizeLine(plan)}
          </div>
          <div data-testid="sheet-plan-report" className="mt-1 text-[10px] text-forge-dim">
            {reportLine(plan.report, plan.icons)}
          </div>
          <PlacementTable placements={plan.placements} />
        </div>
      )}

      {csvPreview && (
        <pre
          data-testid="sheet-csv-preview-text"
          className="mt-2 max-h-40 overflow-auto rounded border border-forge-edge p-2 font-mono text-[10px] text-forge-dim"
        >
          {csvPreview.text.split(/\r\n/).slice(0, 9).join("\n")}
        </pre>
      )}

      {files && files.length > 0 && (
        <ul data-testid="sheet-files" className="mt-2 space-y-1">
          {files.map((file) => (
            <li key={file.format} className="text-[10px] text-forge-dim">
              <span className="text-forge-text">{file.format}</span>{" "}
              <span className="font-mono">{file.path}</span>{" "}
              {file.bytes > 0 && <span>({formatBytes(file.bytes)})</span>}
              <div>{file.evidence}</div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** The first few placements, so the wizard shows what it is about to write. */
function PlacementTable({ placements }: { placements: SheetPlacementDto[] }) {
  return (
    <table className="mt-2 w-full text-left text-[10px] text-forge-dim">
      <thead>
        <tr>
          <th className="font-normal">#</th>
          <th className="font-normal">row, col</th>
          <th className="font-normal">ink</th>
          <th className="font-normal">name</th>
        </tr>
      </thead>
      <tbody>
        {placements.slice(0, 5).map((p) => (
          <tr key={p.id} data-testid={`sheet-placement-${p.id}`}>
            <td>{p.index}</td>
            <td>
              {p.row}, {p.col}
            </td>
            <td>
              {p.w.toFixed(1)}×{p.h.toFixed(1)}
            </td>
            <td className="truncate">
              {p.name}
              {p.flags.length > 0 && <span title={p.flags.join(", ")}> ⚑</span>}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
