//! Below-normal CPU priority for the recorded-clip pass, so a simulator
//! running on the same computer keeps the CPU it needs.
//!
//! The `pose:clips` thread lowers itself with [`lower_this_thread`], and
//! [`worker_hook`] has every worker thread stance starts for a clip do the
//! same as its first step.
//!
//! - **Linux:** the thread's nice value becomes [`NICE`].
//! - **Windows:** the thread's priority becomes below normal.
//! - Elsewhere priorities are left as they are.

use stance::pose::WorkerHook;

/// The nice value of the clip pass on Linux.
#[cfg(target_os = "linux")]
pub const NICE: i32 = 10;

/// Lower the calling thread's priority to below normal. Best effort: a
/// refusal is ignored.
pub fn lower_this_thread() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getpriority/setpriority with `who` 0 read and change only
        // the calling thread's nice value on Linux; they take no pointers.
        let current = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        if current < NICE {
            // SAFETY: as above.
            unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, NICE) };
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
        };
        // SAFETY: GetCurrentThread returns a pseudo handle for the calling
        // thread that needs no closing; SetThreadPriority only reads it.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
    }
}

/// stance's clip-pass worker hook: each worker thread lowers its own
/// priority with [`lower_this_thread`] before it takes a frame.
pub fn worker_hook() -> WorkerHook {
    WorkerHook::new(lower_this_thread)
}

/// The calling thread's priority as the platform states it: the nice value
/// on Linux, the thread priority on Windows (tests).
#[cfg(all(test, any(target_os = "linux", windows)))]
pub fn current() -> i32 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: reads the calling thread's nice value.
        unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadPriority};
        // SAFETY: the pseudo handle of the calling thread, read only.
        unsafe { GetThreadPriority(GetCurrentThread()) }
    }
}

/// What [`current`] reads on a thread that started at `base` and then
/// lowered itself (tests).
#[cfg(all(test, any(target_os = "linux", windows)))]
pub fn lowered_from(base: i32) -> i32 {
    #[cfg(target_os = "linux")]
    {
        base.max(NICE)
    }
    #[cfg(windows)]
    {
        let _ = base;
        windows_sys::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL
    }
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::*;

    #[test]
    fn the_hook_lowers_the_thread_it_runs_on_and_no_other() {
        let base = current();
        let hook = worker_hook();
        let (lowered, untouched) = std::thread::spawn(move || {
            let before = current();
            hook.run();
            (current(), before)
        })
        .join()
        .expect("thread");
        assert_eq!(untouched, base);
        assert_eq!(lowered, lowered_from(base));
        // The calling thread of the test is untouched.
        assert_eq!(current(), base);
    }

    #[test]
    fn lowering_twice_changes_nothing_more() {
        let base = current();
        let (once, twice) = std::thread::spawn(|| {
            lower_this_thread();
            let once = current();
            lower_this_thread();
            (once, current())
        })
        .join()
        .expect("thread");
        assert_eq!(once, lowered_from(base));
        assert_eq!(twice, once);
    }
}
