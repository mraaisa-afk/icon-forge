//! Process RSS measurement (Phase-2 exit gate: batch peak RSS ≤ 2 GB).
//!
//! `current_rss_bytes` reads the OS-provided resident set with zero
//! polling cost: `/proc/self/status` (`VmRSS`) on Linux,
//! `GetProcessMemoryInfo` (PSAPI) on Windows. [`RssWatcher`] samples it
//! from a small side thread so long batch jobs record their *peak*
//! without instrumenting every loop body.

/// Resident set size of this process in bytes, or `None` when the
/// platform has no supported source (never the case on Windows/Linux).
#[must_use]
pub fn current_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        rss_linux()
    }
    #[cfg(windows)]
    {
        rss_windows()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn rss_linux() -> Option<u64> {
    // "VmRSS:    1234 kB" — the kernel reports kilobytes.
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.trim().split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// The only `unsafe` in the workspace: a read-only PSAPI query against
/// our own process pseudo-handle (Windows API contract, not our data).
#[cfg(windows)]
#[allow(unsafe_code)]
fn rss_windows() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) };
    if ok != 0 {
        Some(pmc.WorkingSetSize as u64)
    } else {
        None
    }
}

/// Peak-RSS sampler: a daemon-ish side thread that polls
/// [`current_rss_bytes`] every `interval_ms` and keeps the maximum.
/// Stopped and joined on [`Drop`].
#[derive(Debug)]
pub struct RssWatcher {
    peak: std::sync::Arc<std::sync::atomic::AtomicU64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl RssWatcher {
    /// Starts sampling; `interval_ms` clamped to at least 1 ms.
    #[must_use]
    pub fn start(interval_ms: u64) -> Self {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        let peak = std::sync::Arc::new(AtomicU64::new(0));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let interval = std::time::Duration::from_millis(interval_ms.max(1));
        let (peak_t, stop_t) = (Arc::clone(&peak), Arc::clone(&stop));
        let handle = std::thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                if let Some(rss) = current_rss_bytes() {
                    peak_t.fetch_max(rss, Ordering::Relaxed);
                }
                std::thread::sleep(interval);
            }
        });
        RssWatcher {
            peak,
            stop,
            handle: Some(handle),
        }
    }

    /// Highest sample observed so far (0 before the first tick; callers
    /// usually seed with a synchronous [`current_rss_bytes`] sample).
    #[must_use]
    pub fn peak(&self) -> u64 {
        self.peak.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for RssWatcher {
    fn drop(&mut self) {
        self.stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn current_rss_is_reported_on_supported_platforms() {
        let rss = current_rss_bytes().expect("CI legs are Windows/Linux");
        assert!(rss > 0, "a live process always has resident pages");
    }

    #[test]
    fn watcher_records_a_peak() {
        let watcher = RssWatcher::start(2);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let peak = watcher.peak();
        drop(watcher);
        assert!(peak > 0, "a live process always has resident pages");
    }
}
