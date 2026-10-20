//! Process CPU time and peak memory, for benchmark and diagnostics reports.

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Usage {
    /// User plus kernel CPU time consumed by this process so far.
    pub cpu_ms: f64,
    /// Largest resident memory (working set) this process has used.
    pub peak_memory_mb: f64,
}

#[cfg(windows)]
pub fn usage() -> Option<Usage> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let zero = || FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero(), zero(), zero(), zero());
    // FILETIME counts 100 ns intervals.
    let to_ms = |t: &FILETIME| {
        ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as f64 / 10_000.0
    };
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing;
    // every out-parameter is a valid local of the expected type and size.
    unsafe {
        let process = GetCurrentProcess();
        if GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) == 0 {
            return None;
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(process, &mut counters, counters.cb) == 0 {
            return None;
        }
        Some(Usage {
            cpu_ms: to_ms(&kernel) + to_ms(&user),
            peak_memory_mb: counters.PeakWorkingSetSize as f64 / 1_048_576.0,
        })
    }
}

#[cfg(not(windows))]
pub fn usage() -> Option<Usage> {
    // Linux development fallback: /proc (assumes the usual 100 Hz clock tick).
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks: f64 = fields.get(11)?.parse::<f64>().ok()? + fields.get(12)?.parse::<f64>().ok()?;
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let peak_kb: f64 = status
        .lines()
        .find(|l| l.starts_with("VmHWM:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(Usage {
        cpu_ms: ticks * 10.0,
        peak_memory_mb: peak_kb / 1024.0,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_usage() {
        let u = super::usage().expect("usage available");
        assert!(u.peak_memory_mb > 0.0);
        assert!(u.cpu_ms >= 0.0);
    }
}
