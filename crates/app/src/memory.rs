//! The process's resident memory and CPU time, as the OS reports them.

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

/// User plus system CPU time the process has used.
#[cfg(target_os = "linux")]
pub fn cpu_time() -> Option<std::time::Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the command name, which is in parentheses and may hold spaces; utime and
    // stime are fields 14 and 15 of proc(5), in ticks of 1/100 s (USER_HZ).
    let mut f = stat[stat.rfind(')')? + 1..].split_whitespace().skip(11);
    let ticks: u64 = f.next()?.parse::<u64>().ok()? + f.next()?.parse::<u64>().ok()?;
    Some(std::time::Duration::from_millis(ticks * 10))
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn cpu_time() -> Option<std::time::Duration> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let [mut created, mut exited, mut kernel, mut user] = [zero; 4];
    // SAFETY: four valid FILETIME pointers, and the current process's pseudo-handle.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return None;
    }
    // In units of 100 ns.
    let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    Some(std::time::Duration::from_nanos((t(kernel) + t(user)) * 100))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn cpu_time() -> Option<std::time::Duration> {
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

    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn cpu_time_grows() {
        let a = super::cpu_time().unwrap();
        let t = std::time::Instant::now();
        let mut x = 0u64;
        while t.elapsed().as_millis() < 100 {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        assert!(super::cpu_time().unwrap() > a);
    }
}
