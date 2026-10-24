//! The process's resident memory, as the OS reports it.

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    /// Resident set (Linux) or working set (Windows), bytes.
    pub rss: u64,
    /// Its peak since the process started.
    pub peak: u64,
}

#[cfg(target_os = "linux")]
pub fn usage() -> Option<Usage> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib = |key: &str| {
        let line = status.lines().find(|l| l.starts_with(key))?;
        let n: u64 = line[key.len()..]
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        Some(n * 1024)
    };
    Some(Usage {
        rss: kib("VmRSS:")?,
        peak: kib("VmHWM:")?,
    })
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn usage() -> Option<Usage> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: the counters struct is plain data of the size passed, and the pseudo-handle of
    // the current process needs no closing.
    let c = unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = cb;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut c, cb) == 0 {
            return None;
        }
        c
    };
    Some(Usage {
        rss: c.WorkingSetSize as u64,
        peak: c.PeakWorkingSetSize as u64,
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn usage() -> Option<Usage> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn reports_some_memory() {
        let u = super::usage().unwrap();
        assert!(u.rss > 0 && u.peak >= u.rss);
    }
}
