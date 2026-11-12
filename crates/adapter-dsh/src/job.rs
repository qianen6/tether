//! Ties the `dsh web` Tether starts to Tether's own life.
//!
//! `kill_on_drop` only helps when Tether shuts down in order. On Windows the
//! child goes into a job object that closes with Tether's process, so a crash
//! or a forced stop takes `dsh web` and what it started (plugin helpers such
//! as turbo-agent's proxy, which holds a fixed port) along. Elsewhere this is
//! a no-op.

use tokio::process::Child;

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use tokio::process::Child;
    use windows_sys::Win32::{
        Foundation::HANDLE,
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
    };

    /// The job, kept open for the life of the process (closing it would kill
    /// its members).
    struct Job(HANDLE);
    // SAFETY: a job handle may be used from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    fn job() -> Option<&'static Job> {
        static JOB: OnceLock<Option<Job>> = OnceLock::new();
        JOB.get_or_init(|| {
            // SAFETY: plain Win32 calls on a handle created here.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    tracing::warn!(
                        "CreateJobObjectW failed: {}",
                        std::io::Error::last_os_error()
                    );
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    tracing::warn!(
                        "SetInformationJobObject failed: {}",
                        std::io::Error::last_os_error()
                    );
                    return None;
                }
                Some(Job(handle))
            }
        })
        .as_ref()
    }

    pub(super) fn adopt(child: &Child) {
        let (Some(job), Some(process)) = (job(), child.raw_handle()) else {
            return;
        };
        // SAFETY: both handles are valid for the duration of the call.
        let ok = unsafe { AssignProcessToJobObject(job.0, process as HANDLE) };
        if ok == 0 {
            tracing::warn!(
                "AssignProcessToJobObject failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Puts `child` (and what it starts from now on) in Tether's job.
pub(crate) fn adopt(child: &Child) {
    #[cfg(windows)]
    imp::adopt(child);
    #[cfg(not(windows))]
    let _ = child;
}
