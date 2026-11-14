//! Ties each `dsh web` Tether starts — and everything it starts in turn — to
//! one owner, so stopping it ends the whole tree.
//!
//! `kill_on_drop` only ends the direct child. DSH's launcher is often a
//! chain (`dsh.cmd` → `cmd.exe` → Node.js or `DeepSeek Harness.exe`, plus
//! plugin helpers such as turbo-agent's proxy on a fixed port), and killing
//! the head leaves the rest running with the port held. So:
//!
//! - Windows: each launch gets its own job object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Dropping the [`Tree`] terminates the
//!   job; if Tether crashes, Windows closes the handle and the job dies too.
//! - Unix: the child leads a new process group; dropping the [`Tree`] sends
//!   `SIGKILL` to the group.

use tokio::process::{Child, Command};

/// Prepare `cmd` so what it starts can be ended as one tree.
pub(crate) fn prepare(cmd: &mut Command) {
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(not(unix))]
    let _ = cmd;
}

/// The process tree of one launched child; ended when dropped.
pub(crate) struct Tree {
    #[cfg(windows)]
    job: Option<imp::Job>,
    #[cfg(unix)]
    group: Option<i32>,
}

impl Tree {
    /// Take ownership of `child`'s tree (call right after spawning it).
    pub(crate) fn adopt(child: &Child) -> Self {
        #[cfg(windows)]
        {
            Self {
                job: imp::Job::adopt(child),
            }
        }
        #[cfg(unix)]
        {
            Self {
                group: child.id().and_then(|id| i32::try_from(id).ok()),
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = child;
            Self {}
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            // SAFETY: plain syscall; a negative pid names the process group
            // this child leads (`process_group(0)` in `prepare`).
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use tokio::process::Child;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
    };

    /// One job object holding one launched child and its descendants.
    pub(super) struct Job(HANDLE);
    // SAFETY: a job handle may be used from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        fn create() -> Option<Self> {
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
                let job = Job(handle);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job.0,
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
                Some(job)
            }
        }

        pub(super) fn adopt(child: &Child) -> Option<Self> {
            let process = child.raw_handle()?;
            let job = Self::create()?;
            // SAFETY: both handles are valid for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(job.0, process as HANDLE) };
            if ok == 0 {
                tracing::warn!(
                    "AssignProcessToJobObject failed: {}",
                    std::io::Error::last_os_error()
                );
                return None;
            }
            Some(job)
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this value and closed only here.
            unsafe {
                TerminateJobObject(self.0, 1);
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Stdio, time::Duration};

    use super::*;

    /// A shell whose grandchild outlives a plain kill of the shell: dropping
    /// the tree must end the grandchild too.
    #[tokio::test]
    async fn dropping_the_tree_ends_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("alive");
        let mut cmd = if cfg!(windows) {
            // cmd → powershell, which touches the marker every 100 ms.
            let script = format!(
                "while ($true) {{ Set-Content -LiteralPath '{}' -Value (Get-Date).Ticks; Start-Sleep -Milliseconds 100 }}",
                marker.display()
            );
            let mut c = Command::new("cmd.exe");
            c.args(["/C", "powershell.exe", "-NoProfile", "-Command", &script]);
            c
        } else {
            let script = format!(
                "sh -c 'while true; do date +%s%N > \"{}\"; sleep 0.1; done' & wait",
                marker.display()
            );
            let mut c = Command::new("sh");
            c.args(["-c", &script]);
            c
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        prepare(&mut cmd);
        let child = cmd.spawn().unwrap();
        let tree = Tree::adopt(&child);

        // Wait until the grandchild writes.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while std::fs::read_to_string(&marker).is_err() {
            assert!(tokio::time::Instant::now() < deadline, "never started");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        drop(tree);
        drop(child);
        tokio::time::sleep(Duration::from_millis(800)).await;
        let before = std::fs::read_to_string(&marker).unwrap_or_default();
        tokio::time::sleep(Duration::from_millis(800)).await;
        let after = std::fs::read_to_string(&marker).unwrap_or_default();
        assert_eq!(before, after, "the grandchild is still writing");
    }
}
