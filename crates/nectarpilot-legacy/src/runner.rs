use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

use nectarpilot_platform::job::ChildJob;

use crate::converter::{ConversionReport, convert_movement_pattern};

const DEFAULT_MAX_SCRIPT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScriptTrust {
    pub canonical_path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LegacyImportReport {
    pub trust: ScriptTrust,
    pub conversion: ConversionReport,
}

/// Reads and assesses a legacy script without executing it.
pub fn inspect_script(path: impl AsRef<Path>) -> Result<LegacyImportReport, LegacyError> {
    inspect_script_with_limit(path.as_ref(), DEFAULT_MAX_SCRIPT_BYTES)
}

fn inspect_script_with_limit(
    path: &Path,
    max_script_bytes: u64,
) -> Result<LegacyImportReport, LegacyError> {
    let (trust, source) = read_and_hash(path, max_script_bytes)?;
    let name = trust
        .canonical_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("imported-pattern")
        .to_owned();
    Ok(LegacyImportReport {
        trust,
        conversion: convert_movement_pattern(&name, &source),
    })
}

/// Non-serializable proof that the user acknowledged the bridge risk for one
/// exact script hash. A changed script always requires new consent.
#[derive(Clone, Debug)]
pub struct LegacyConsent {
    approved_path: PathBuf,
    approved_sha256: String,
    approved_bytes: u64,
    risk_acknowledged: bool,
}

impl LegacyConsent {
    #[must_use]
    pub fn acknowledge_for(trust: &ScriptTrust) -> Self {
        Self {
            approved_path: trust.canonical_path.clone(),
            approved_sha256: trust.sha256.clone(),
            approved_bytes: trust.bytes,
            risk_acknowledged: true,
        }
    }

    fn matches(&self, trust: &ScriptTrust) -> bool {
        self.risk_acknowledged
            && self.approved_path == trust.canonical_path
            && self.approved_sha256 == trust.sha256
            && self.approved_bytes == trust.bytes
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RunnerPolicy {
    pub maximum_timeout: Duration,
    pub maximum_script_bytes: u64,
    pub poll_interval: Duration,
}

impl Default for RunnerPolicy {
    fn default() -> Self {
        Self {
            maximum_timeout: Duration::from_secs(30 * 60),
            maximum_script_bytes: DEFAULT_MAX_SCRIPT_BYTES,
            poll_interval: Duration::from_millis(20),
        }
    }
}

#[derive(Debug)]
pub struct ExecutionRequest {
    pub interpreter: PathBuf,
    /// Fixed interpreter arguments such as `AutoHotkey`'s `/ErrorStdOut` switch.
    /// No shell parses these values.
    pub interpreter_arguments: Vec<OsString>,
    pub script: PathBuf,
    pub consent: Option<LegacyConsent>,
    pub timeout: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionOutcome {
    Completed { pid: u32, exit_code: Option<i32> },
}

#[derive(Debug, Error)]
pub enum LegacyError {
    #[error("legacy bridge execution requires explicit hash-bound consent")]
    ConsentRequired,
    #[error("the script changed after consent; inspect it and consent again")]
    TrustMismatch,
    #[error("script exceeds the configured {maximum} byte limit")]
    ScriptTooLarge { maximum: u64 },
    #[error("timeout must be non-zero and no greater than {maximum_seconds} seconds")]
    InvalidTimeout { maximum_seconds: u64 },
    #[error("legacy script is not valid UTF-8: {0}")]
    InvalidText(#[from] std::string::FromUtf8Error),
    #[error("legacy filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to create the legacy process: {0}")]
    Spawn(String),
    #[error("failed to contain the legacy process: {0}")]
    Containment(String),
    #[error("legacy process {pid} exceeded its time budget")]
    TimedOut { pid: u32 },
    #[error("legacy process {pid} was cancelled")]
    Cancelled { pid: u32 },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LegacyRunner {
    policy: RunnerPolicy,
}

impl LegacyRunner {
    #[must_use]
    pub const fn new(policy: RunnerPolicy) -> Self {
        Self { policy }
    }

    pub fn inspect(&self, path: impl AsRef<Path>) -> Result<LegacyImportReport, LegacyError> {
        inspect_script_with_limit(path.as_ref(), self.policy.maximum_script_bytes)
    }

    /// Runs a separately inspected script only after re-hashing it immediately
    /// before process creation and matching fresh explicit consent.
    pub async fn execute(
        &self,
        request: ExecutionRequest,
        cancellation: CancellationToken,
    ) -> Result<ExecutionOutcome, LegacyError> {
        if request.timeout.is_zero() || request.timeout > self.policy.maximum_timeout {
            return Err(LegacyError::InvalidTimeout {
                maximum_seconds: self.policy.maximum_timeout.as_secs(),
            });
        }
        let consent = request
            .consent
            .as_ref()
            .ok_or(LegacyError::ConsentRequired)?;
        let (current_trust, _) = read_and_hash(&request.script, self.policy.maximum_script_bytes)?;
        if !consent.matches(&current_trust) {
            return Err(LegacyError::TrustMismatch);
        }
        if cancellation.is_cancelled() {
            return Err(LegacyError::Cancelled { pid: 0 });
        }

        let mut command = Command::new(&request.interpreter);
        command
            .args(&request.interpreter_arguments)
            .arg(&current_trust.canonical_path)
            .current_dir(
                current_trust
                    .canonical_path
                    .parent()
                    .unwrap_or_else(|| Path::new(".")),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command
            .spawn()
            .map_err(|error| LegacyError::Spawn(error.to_string()))?;
        let mut child = ContainedChild::attach(child)?;
        let pid = child.id();
        let deadline = Instant::now() + request.timeout;

        loop {
            if cancellation.is_cancelled() {
                child.terminate();
                return Err(LegacyError::Cancelled { pid });
            }
            if let Some(status) = child.try_wait()? {
                child.mark_complete();
                return Ok(completed(pid, status));
            }
            if Instant::now() >= deadline {
                child.terminate();
                return Err(LegacyError::TimedOut { pid });
            }
            sleep(self.policy.poll_interval).await;
        }
    }
}

fn completed(pid: u32, status: ExitStatus) -> ExecutionOutcome {
    ExecutionOutcome::Completed {
        pid,
        exit_code: status.code(),
    }
}

fn read_and_hash(path: &Path, maximum: u64) -> Result<(ScriptTrust, String), LegacyError> {
    let canonical_path = fs::canonicalize(path)?;
    let bytes = fs::read(&canonical_path)?;
    let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if byte_count > maximum {
        return Err(LegacyError::ScriptTooLarge { maximum });
    }
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let source = String::from_utf8(bytes)?;
    Ok((
        ScriptTrust {
            canonical_path,
            sha256,
            bytes: byte_count,
        },
        source,
    ))
}

struct ContainedChild {
    child: Child,
    containment: ChildJob,
    complete: bool,
}

impl ContainedChild {
    fn attach(mut child: Child) -> Result<Self, LegacyError> {
        match ChildJob::assign(&child) {
            Ok(containment) => Ok(Self {
                child,
                containment,
                complete: false,
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(LegacyError::Containment(error.to_string()))
            }
        }
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, LegacyError> {
        self.child.try_wait().map_err(LegacyError::Io)
    }

    fn mark_complete(&mut self) {
        self.complete = true;
    }

    fn terminate(&mut self) {
        self.containment.terminate();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.complete = true;
    }
}

impl Drop for ContainedChild {
    fn drop(&mut self) {
        if !self.complete {
            self.terminate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn changed_script_is_blocked_before_process_creation() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("pattern.ahk");
        fs::write(&script, "Sleep 10\n").unwrap();
        let report = inspect_script(&script).unwrap();
        let consent = LegacyConsent::acknowledge_for(&report.trust);
        fs::write(&script, "Sleep 20\n").unwrap();
        let request = ExecutionRequest {
            interpreter: PathBuf::from("definitely-does-not-exist.exe"),
            interpreter_arguments: Vec::new(),
            script,
            consent: Some(consent),
            timeout: Duration::from_secs(1),
        };

        let error = LegacyRunner::default()
            .execute(request, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, LegacyError::TrustMismatch));
    }

    #[tokio::test]
    async fn execution_is_stopped_at_the_timeout() {
        let directory = tempfile::tempdir().unwrap();

        #[cfg(windows)]
        let (script, interpreter, arguments) = {
            let script = directory.path().join("wait.ps1");
            fs::write(&script, "Start-Sleep -Seconds 10\n").unwrap();
            (
                script,
                PathBuf::from("powershell.exe"),
                vec![
                    OsString::from("-NoProfile"),
                    OsString::from("-NonInteractive"),
                    OsString::from("-ExecutionPolicy"),
                    OsString::from("Bypass"),
                    OsString::from("-File"),
                ],
            )
        };

        #[cfg(not(windows))]
        let (script, interpreter, arguments) = {
            use std::os::unix::fs::PermissionsExt;
            let script = directory.path().join("wait.sh");
            fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            (script, PathBuf::from("/bin/sh"), Vec::new())
        };

        let report = inspect_script(&script).unwrap();
        let request = ExecutionRequest {
            interpreter,
            interpreter_arguments: arguments,
            script,
            consent: Some(LegacyConsent::acknowledge_for(&report.trust)),
            timeout: Duration::from_millis(100),
        };
        let started = Instant::now();
        let error = LegacyRunner::default()
            .execute(request, CancellationToken::new())
            .await
            .unwrap_err();

        assert!(matches!(error, LegacyError::TimedOut { .. }));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
