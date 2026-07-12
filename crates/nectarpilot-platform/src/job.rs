use std::process::Child;

use thiserror::Error;

#[derive(Debug, Error)]
#[error("Windows job-object operation failed: {0}")]
pub struct JobError(pub String);

/// Safe owner for a job containing one exact launched child and its descendants.
/// Closing the job kills anything still running inside it.
#[cfg(windows)]
pub struct ChildJob {
    job: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
// SAFETY: A Windows job-object handle is a kernel object with no thread
// affinity. `ChildJob` owns it exclusively, exposes only `&mut self`
// termination, and closes it exactly once in Drop. Moving that exclusive owner
// between Tokio worker threads is therefore safe and is required for the
// contained legacy runner's cancellation future to remain Send.
unsafe impl Send for ChildJob {}

#[cfg(windows)]
impl ChildJob {
    pub fn assign(child: &Child) -> Result<Self, JobError> {
        use std::ffi::c_void;
        use std::mem::size_of;
        use std::os::windows::io::AsRawHandle;

        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        use windows::core::PCWSTR;

        // SAFETY: unnamed job creation uses no security descriptor; the returned
        // owned handle is closed exactly once by Drop.
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|error| JobError(error.to_string()))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: limits points to a fully initialized structure for exactly the
        // declared byte size and job is a live owned handle.
        if let Err(error) = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast::<c_void>(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .expect("job information size fits in u32"),
            )
        } {
            // SAFETY: job is owned here and has not been closed.
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(job) };
            return Err(JobError(error.to_string()));
        }
        let child_handle = HANDLE(child.as_raw_handle());
        // SAFETY: std::process::Child owns a valid process handle for the exact
        // child and job remains live for the entire ChildJob lifetime.
        if let Err(error) = unsafe { AssignProcessToJobObject(job, child_handle) } {
            // SAFETY: job is owned here and has not been closed.
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(job) };
            return Err(JobError(error.to_string()));
        }
        Ok(Self { job })
    }

    pub fn terminate(&mut self) {
        // SAFETY: this job handle remains owned and valid until Drop.
        let _ = unsafe { windows::Win32::System::JobObjects::TerminateJobObject(self.job, 1) };
    }
}

#[cfg(windows)]
impl Drop for ChildJob {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE guarantees descendants do not outlive the bridge.
        // SAFETY: this object owns the handle and drops exactly once.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.job) };
    }
}

#[cfg(not(windows))]
#[derive(Default)]
pub struct ChildJob;

#[cfg(not(windows))]
impl ChildJob {
    pub fn assign(_child: &Child) -> Result<Self, JobError> {
        Ok(Self)
    }

    pub fn terminate(&mut self) {}
}
