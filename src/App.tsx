import { useEffect } from "react";
import { Toolbar } from "./components/Toolbar";
import { JobBar } from "./components/JobBar";
import { LibraryGrid } from "./components/library/LibraryGrid";
import { useStore } from "./state/store";

export function App() {
  const project = useStore((s) => s.project);
  const startEventPump = useStore((s) => s.startEventPump);

  useEffect(() => {
    startEventPump();
  }, [startEventPump]);

  return (
    <div className="flex h-full flex-col">
      <Toolbar />
      <div className="min-h-0 flex-1">
        {project ? (
          <LibraryGrid />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-forge-dim">
            <div className="text-4xl">⚒</div>
            <div>Open or create an .isgproj to begin.</div>
            <div className="text-xs">
              (In the browser bridge a mock backend is used; the Tauri shell uses the real SQLite engine.)
            </div>
          </div>
        )}
      </div>
      <JobBar />
    </div>
  );
}
