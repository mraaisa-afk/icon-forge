import { useState } from "react";
import * as Tooltip from "@radix-ui/react-tooltip";
import { useStore } from "../state/store";

/**
 * Top toolbar: project lifecycle, import, save. In the browser (no Tauri
 * shell) file pickers fall back to a text path input.
 */
export function Toolbar() {
  const project = useStore((s) => s.project);
  const busy = useStore((s) => s.busy);
  const openProject = useStore((s) => s.openProject);
  const createProject = useStore((s) => s.createProject);
  const closeProject = useStore((s) => s.closeProject);
  const saveProject = useStore((s) => s.saveProject);
  const importFolder = useStore((s) => s.importFolder);
  const [importRoot, setImportRoot] = useState("");

  return (
    <Tooltip.Provider delayDuration={200}>
      <div className="flex flex-wrap items-center gap-2 border-b border-forge-edge bg-forge-panel px-3 py-2">
        <span className="mr-2 font-semibold tracking-wide text-forge-accent">⚒ Icon Forge</span>

        <input
          className="w-[26rem] rounded border border-forge-edge bg-forge-bg px-2 py-1 font-mono text-xs text-forge-text placeholder:text-forge-dim"
          placeholder="project path (e.g. D:\\icons\\library.isgproj)"
          value={project?.path ?? ""}
          readOnly={!!project}
          id="project-path"
        />
        <ToolButton label="Open project" onClick={() => void openProject((document.getElementById("project-path") as HTMLInputElement).value)} disabled={busy} />
        <ToolButton label="New project" onClick={() => void createProject((document.getElementById("project-path") as HTMLInputElement).value)} disabled={busy} />
        <ToolButton label="Save (atomic)" onClick={() => void saveProject()} disabled={busy || !project} />
        <ToolButton label="Close" onClick={() => void closeProject()} disabled={!project} />

        <div className="mx-2 h-5 w-px bg-forge-edge" />

        <input
          className="w-[24rem] rounded border border-forge-edge bg-forge-bg px-2 py-1 font-mono text-xs text-forge-text placeholder:text-forge-dim"
          placeholder="folder to import (recursive, png/jpg)"
          value={importRoot}
          onChange={(e) => setImportRoot(e.target.value)}
          id="import-root"
        />
        <ToolButton
          label="Import folder (T2 job)"
          onClick={() => {
            void importFolder(importRoot);
          }}
          disabled={!project || importRoot.trim().length === 0}
        />

        <div className="ml-auto text-xs text-forge-dim">
          {project ? (
            <span>
              {project.path} · {project.sheetCount.toLocaleString()} sheets ·{" "}
              {project.iconCount.toLocaleString()} icons
            </span>
          ) : (
            <span>no project open</span>
          )}
        </div>
      </div>
    </Tooltip.Provider>
  );
}

function ToolButton(props: { label: string; onClick: () => void; disabled?: boolean }) {
  return (
    <Tooltip.Root>
      <Tooltip.Trigger asChild>
        <button
          type="button"
          onClick={props.onClick}
          disabled={props.disabled}
          className="rounded border border-forge-edge bg-forge-bg px-2 py-1 text-xs text-forge-text hover:border-forge-accent disabled:cursor-not-allowed disabled:opacity-40"
        >
          {props.label}
        </button>
      </Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content className="rounded bg-forge-edge px-2 py-1 text-xs text-forge-text" sideOffset={4}>
          {props.label}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
  );
}
