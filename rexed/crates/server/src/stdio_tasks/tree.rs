//! Kill the task's process group / job, including descendants.
use anyhow::Result;
#[cfg(unix)]
pub(super) struct ProcessTree {
    pid: u32,
    killed: std::sync::atomic::AtomicBool,
}
#[cfg(unix)]
impl ProcessTree {
    pub(super) fn new(pid: u32) -> Result<Self> {
        Ok(Self {
            pid,
            killed: std::sync::atomic::AtomicBool::new(false),
        })
    }
    pub(super) fn kill(&self) {
        if !self.killed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
        }
    }
}
#[cfg(windows)]
pub(super) struct ProcessTree(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
unsafe impl Send for ProcessTree {}
#[cfg(windows)]
impl ProcessTree {
    pub(super) fn new(pid: u32) -> Result<Self> {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, FALSE},
            System::{JobObjects::*, Threading::*},
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            anyhow::ensure!(
                !job.is_null(),
                "CreateJobObject: {}",
                std::io::Error::last_os_error()
            );
            let tree = Self(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            anyhow::ensure!(
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32
                ) != 0,
                "SetInformationJobObject: {}",
                std::io::Error::last_os_error()
            );
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, FALSE, pid);
            anyhow::ensure!(
                !process.is_null(),
                "OpenProcess: {}",
                std::io::Error::last_os_error()
            );
            let assigned = AssignProcessToJobObject(job, process);
            let error = std::io::Error::last_os_error();
            CloseHandle(process);
            anyhow::ensure!(assigned != 0, "AssignProcessToJobObject: {error}");
            Ok(tree)
        }
    }
    pub(super) fn kill(&self) {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(not(any(unix, windows)))]
pub(super) struct ProcessTree;
#[cfg(not(any(unix, windows)))]
impl ProcessTree {
    pub(super) fn new(_: u32) -> Result<Self> {
        anyhow::bail!("stdio tasks require Unix process groups or Windows jobs")
    }
    pub(super) fn kill(&self) {}
}

#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.kill();
    }
}
