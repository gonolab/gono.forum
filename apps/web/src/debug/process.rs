//! Cheap process-level sampling; concurrent requests share the process, so it cannot be treated as a resource owned by
//! the page.
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ProcessSample {
    pub rss_bytes: Option<u64>,
    pub cpu_user: Option<Duration>,
    pub cpu_system: Option<Duration>,
}

impl ProcessSample {
    pub fn capture() -> Self {
        let (cpu_user, cpu_system) = cpu_times();
        Self {
            rss_bytes: resident_memory(),
            cpu_user,
            cpu_system,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cpu_times() -> (Option<Duration>, Option<Duration>) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes a matching struct; the initialized result is read only on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return (None, None);
    }
    let usage = unsafe { usage.assume_init() };
    (timeval(usage.ru_utime), timeval(usage.ru_stime))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn timeval(value: libc::timeval) -> Option<Duration> {
    let seconds = u64::try_from(value.tv_sec).ok()?;
    let micros = u32::try_from(value.tv_usec).ok()?;
    (micros < 1_000_000).then(|| Duration::new(seconds, micros * 1000))
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc's mach_task_self is still the interface to the current process's task port on Darwin.
fn resident_memory() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::uninit();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    // SAFETY: the layout of info and count match MACH_TASK_BASIC_INFO; count holds the size returned after a
    // successful read.
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS || count < libc::MACH_TASK_BASIC_INFO_COUNT {
        return None;
    }
    Some(unsafe { info.assume_init() }.resident_size)
}

#[cfg(target_os = "linux")]
fn resident_memory() -> Option<u64> {
    use std::sync::OnceLock;
    static PAGE_SIZE: OnceLock<Option<u64>> = OnceLock::new();
    let page_size = (*PAGE_SIZE.get_or_init(|| {
        // SAFETY: sysconf takes no pointer; a non-positive value means the query failed.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        u64::try_from(size).ok().filter(|size| *size > 0)
    }))?;
    // statm RSS is a cheap approximation; the lifetime ru_maxrss is not passed off as the current memory.
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    resident_pages.checked_mul(page_size)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn cpu_times() -> (Option<Duration>, Option<Duration>) {
    (None, None)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident_memory() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn reads_current_process_memory_and_monotonic_cpu() {
        let start = ProcessSample::capture();
        let end = ProcessSample::capture();
        assert!(end.rss_bytes.is_some_and(|bytes| bytes > 0));
        assert!(end.cpu_user.unwrap() >= start.cpu_user.unwrap());
        assert!(end.cpu_system.unwrap() >= start.cpu_system.unwrap());
    }
}
