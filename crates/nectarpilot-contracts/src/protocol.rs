use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use uuid::Uuid;

use crate::Profile;

pub const PROTOCOL_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CommandEnvelope {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub profile_id: Uuid,
    pub command: Command,
}

impl CommandEnvelope {
    #[must_use]
    pub fn new(profile_id: Uuid, command: Command) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            profile_id,
            command,
        }
    }

    pub fn validate_version(&self) -> Result<(), ProtocolVersionError> {
        if self.protocol_version == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(ProtocolVersionError {
                expected: PROTOCOL_VERSION,
                received: self.protocol_version,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolVersionError {
    pub expected: u16,
    pub received: u16,
}

impl std::fmt::Display for ProtocolVersionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "protocol version mismatch: expected {}, received {}",
            self.expected, self.received
        )
    }
}

impl std::error::Error for ProtocolVersionError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Command {
    Start {
        mode: StartMode,
    },
    Pause,
    Resume,
    Stop,
    EmergencyStop,
    /// Internal desktop-to-daemon lifecycle request. It performs the same
    /// fail-safe input release as an emergency stop before process exit.
    ShutdownDaemon,
    GetSnapshot,
    GetProfiles,
    SelectProfile,
    /// Runs one manifest-pinned legacy `AutoHotkey` asset through the daemon's
    /// contained compatibility port. The daemon re-checks both this digest and
    /// the profile's stored hash-bound consent before process creation.
    StartLegacy {
        script_id: String,
        approved_sha256: String,
    },
    /// Runs the profile's configured field rotation as an orchestrated loop of
    /// trusted legacy assets (travel, gather pattern, reset/convert), bounded
    /// by cycle and wall-clock limits. Every referenced asset must already be
    /// hash-trusted in the profile.
    StartLegacySession {
        max_cycles: u32,
        max_minutes: u32,
    },
    /// Returns the exact generated harness for one manifest-pinned asset so a
    /// user can review what would run before trusting or starting it.
    InspectLegacy {
        script_id: String,
    },
    /// Stores one named secret (for example the private-server link) in the
    /// daemon's encrypted store. The value never appears in events or logs.
    ImportSecret {
        name: String,
        value: String,
    },
    GetRunHistory,
    /// Opens the in-game quest log from the client-anchored legacy menu
    /// position, verifies the open state, reads the giver icon, title, and
    /// objective bars, then closes it. Requires an idle engine and one
    /// foreground Roblox client; uncertain readings are reported, not guessed.
    ScanQuests,
    SaveProfile {
        profile: Box<Profile>,
    },
    DeleteProfile,
    ExportProfile,
    AcknowledgeAttention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    Normal,
    DryRun,
    Diagnostics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct EventEnvelope {
    pub protocol_version: u16,
    pub sequence: u64,
    pub run_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub event: DaemonEvent,
}

impl EventEnvelope {
    #[must_use]
    pub fn new(sequence: u64, run_id: Uuid, event: DaemonEvent) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            sequence,
            run_id,
            timestamp: Utc::now(),
            event,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum DaemonEvent {
    CommandAccepted {
        request_id: Uuid,
    },
    CommandRejected {
        request_id: Uuid,
        reason: String,
    },
    StateChanged {
        previous: RunState,
        current: RunState,
        reason: String,
    },
    ActionCompleted(ActionResult),
    ReconnectProgress(ReconnectProgress),
    Log {
        level: EventLevel,
        target: String,
        message: String,
        #[serde(default)]
        fields: Value,
    },
    Snapshot(RunSnapshot),
    ProfileSaved {
        profile_id: Uuid,
    },
    Profiles {
        profiles: Vec<Profile>,
        selected_profile_id: Uuid,
    },
    ProfileSelected {
        profile_id: Uuid,
    },
    ProfileDeleted {
        profile_id: Uuid,
    },
    ProfileExported {
        profile_id: Uuid,
        json: String,
    },
    SafeModeEntered {
        crash_count: usize,
        window_seconds: u64,
    },
    /// Final acknowledgement after input cleanup. The daemon flushes this
    /// event before a requested graceful process exit.
    ShutdownReady {
        request_id: Uuid,
    },
    /// Progress through an orchestrated legacy session plan.
    SessionProgress(SessionProgress),
    /// Review payload for one manifest-pinned legacy asset.
    LegacyInspection(LegacyInspection),
    /// Confirmation that a named secret was stored; the value is never echoed.
    SecretStored {
        name: String,
    },
    RunHistory {
        /// Profile whose records were requested. This lets consumers reject
        /// delayed responses after the active profile changes.
        profile_id: Uuid,
        entries: Vec<RunRecord>,
    },
    /// Passive screen-derived statistics. `None` fields mean the reading was
    /// not confident; they are never guessed.
    StatsSample(StatsSample),
    /// Advisory quest-log reading. Absent fields mean that part of the log
    /// could not be read confidently.
    QuestScan(QuestScanResult),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EventLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Idle,
    Preflight,
    Running,
    Paused,
    Recovering,
    NeedsAttention,
    Stopping,
    Faulted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct RunSnapshot {
    pub run_id: Uuid,
    pub profile_id: Uuid,
    pub state: RunState,
    pub safe_mode: bool,
    pub active_task: Option<String>,
    pub last_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ReconnectProgress {
    pub attempt: u8,
    pub maximum_attempts: u8,
    pub elapsed_seconds: u64,
    pub deadline_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    Succeeded,
    Skipped,
    Cancelled,
    Failed,
    NeedsAttention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SessionProgress {
    pub cycle: u32,
    pub max_cycles: u32,
    pub step_index: u32,
    pub step_count: u32,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LegacyInspection {
    pub script_id: String,
    pub sha256: String,
    pub bytes: u64,
    pub requires_legacy_bridge: bool,
    /// The complete generated harness that would execute, for human review.
    pub harness_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RunRecord {
    pub run_id: Uuid,
    pub profile_id: Uuid,
    /// `legacy`, `legacy_session`, or `native`.
    pub kind: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub final_state: String,
    pub summary: String,
    pub steps_succeeded: u32,
    pub steps_failed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct QuestScanResult {
    pub scanned_at: DateTime<Utc>,
    /// Detected quest giver (`snake_case`), when the icon match was confident.
    pub giver: Option<String>,
    pub quest_id: Option<String>,
    pub quest_name: Option<String>,
    /// Objective bar completion in screen order; empty when unreadable.
    pub bars_complete: Vec<bool>,
    /// Fields that advance the detected quest's incomplete objectives.
    pub recommended_fields: Vec<String>,
    /// Human-readable uncertainty and held-work explanations.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct StatsSample {
    pub sampled_at: DateTime<Utc>,
    /// Current honey read from the HUD counter; absent when OCR agreement
    /// was insufficient.
    pub honey: Option<u64>,
    /// Windowed rate derived from confident samples only.
    pub honey_per_hour: Option<f64>,
    pub session_minutes: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ActionResult {
    pub action: String,
    pub outcome: ActionOutcome,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub message: String,
    #[serde(default)]
    pub details: Value,
}
