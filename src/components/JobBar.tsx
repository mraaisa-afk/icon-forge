import { useStore } from "../state/store";

/** Bottom status bar: active/finished jobs with cancel + progress. */
export function JobBar() {
  const jobs = useStore((s) => s.jobs);
  const error = useStore((s) => s.error);
  const cancelJob = useStore((s) => s.cancelJob);
  const list = Object.values(jobs).slice(-4);

  return (
    <div className="border-t border-forge-edge bg-forge-panel px-3 py-1 text-xs">
      {error && <div className="mb-1 text-red-400">⚠ {error}</div>}
      <div className="flex flex-col gap-1">
        {list.map((job) => (
          <div key={job.id} className="flex items-center gap-2">
            <span className="text-forge-dim">#{job.id}</span>
            <span className="text-forge-text">{job.name || (job.outcome ? "" : "job")}</span>
            {job.outcome === null && job.total > 0 && (
              <span className="text-forge-dim">
                {job.done}/{job.total}
              </span>
            )}
            {job.message && <span className="truncate text-forge-dim">{job.message}</span>}
            {job.outcome === null && (
              <button
                type="button"
                className="ml-auto rounded border border-forge-edge px-2 text-[10px] text-forge-text hover:border-red-400"
                onClick={() => void cancelJob(job.id)}
              >
                cancel
              </button>
            )}
            {job.outcome && (
              <span
                className={
                  job.outcome === "succeeded"
                    ? "text-green-400"
                    : job.outcome === "failed"
                      ? "text-red-400"
                      : "text-forge-dim"
                }
              >
                {job.outcome}
              </span>
            )}
          </div>
        ))}
        {list.length === 0 && <span className="text-forge-dim">idle — T0/T1/T2 engine ready</span>}
      </div>
    </div>
  );
}
