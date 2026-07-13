//! Advisory quest-log scanning.
//!
//! The scan reproduces `nm_OpenMenu("questlog")`: click the fixed menu-button
//! offset, verify the open state against the pinned `questlog` template,
//! read the giver icon / title / objective bars with the typed detectors,
//! then toggle the log closed and release inputs. Every uncertain reading is
//! reported as a note instead of a guess, and nothing here moves the player.

use std::path::PathBuf;

use async_trait::async_trait;
use nectarpilot_contracts::{Profile, QuestScanResult};
use nectarpilot_core::{AutomationError, QuestScanPort, TaskContext};

const SCAN_CANCELLED: &str = "quest scan cancelled";

pub struct QuestScanService {
    #[cfg_attr(not(windows), allow(dead_code))]
    root: PathBuf,
}

impl QuestScanService {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl QuestScanPort for QuestScanService {
    async fn scan(
        &self,
        profile: &Profile,
        context: TaskContext,
    ) -> Result<QuestScanResult, AutomationError> {
        #[cfg(not(windows))]
        {
            let _ = (profile, context);
            Err(AutomationError::InvalidCommand(
                "quest scanning requires Windows".into(),
            ))
        }
        #[cfg(windows)]
        {
            let _ = profile;
            let root = self.root.clone();
            let cancellation = context.cancellation_token();
            let worker_cancellation = cancellation.child_token();
            let run_cancellation = worker_cancellation.clone();
            let mut scan =
                tokio::task::spawn_blocking(move || windows_scan::run(&root, &run_cancellation));
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    worker_cancellation.cancel();
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        &mut scan,
                    ).await;
                    Err(AutomationError::Cancelled)
                }
                () = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    // `spawn_blocking` cannot be aborted safely. Signal the
                    // worker first; it checks this token before every input,
                    // during every wait, and drops its broker on exit.
                    worker_cancellation.cancel();
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        &mut scan,
                    ).await;
                    Err(AutomationError::Backend(
                        "quest scan exceeded its 60-second budget".into(),
                    ))
                }
                joined = &mut scan => match joined {
                    Ok(result) => result.map_err(|message| {
                        if message == SCAN_CANCELLED {
                            AutomationError::Cancelled
                        } else {
                            AutomationError::Backend(message)
                        }
                    }),
                    Err(join_error) => Err(AutomationError::Backend(format!(
                    "quest scan worker panicked: {join_error}"
                    ))),
                },
            }
        }
    }
}

#[cfg(windows)]
mod windows_scan {
    use std::path::Path;
    use std::thread::sleep;
    use std::time::Duration;

    use chrono::Utc;
    use nectarpilot_contracts::{Detection, NormalizedRegion, QuestScanResult};
    use nectarpilot_core::quests::{QuestGiver, QuestObjective, quest_catalog_for};
    use nectarpilot_platform::capture::{ClientCapture, WindowsClientCapture};
    use nectarpilot_platform::input::{InputAction, InputBroker, MouseButton};
    use nectarpilot_platform::windows_backend::WindowsInputSink;
    use nectarpilot_platform::{
        ClientFrame, ConsensusPolicy, MultiScaleTemplateMatcher, QuestTitleDetector, RobloxSession,
        TemplateDetector, TemplateMatcherConfig, WindowsOcr, discover_roblox_clients,
        quest_giver_bindings, read_quest_bars, template_from_png_bytes,
    };
    use tokio_util::sync::CancellationToken;

    use super::SCAN_CANCELLED;

    /// Natro's fixed menu-button offsets inside the client area.
    const MENU_CLICK_X: i32 = 85;
    const MENU_CLICK_Y: i32 = 120;
    const DEFOCUS_CLICK_X: i32 = 350;
    const DEFOCUS_CLICK_Y: i32 = 100;

    #[allow(
        clippy::too_many_lines,
        reason = "the scan is one linear open-read-close sequence; splitting it would hide the cleanup ordering"
    )]
    pub fn run(root: &Path, cancellation: &CancellationToken) -> Result<QuestScanResult, String> {
        check_cancelled(cancellation)?;
        let mut notes = Vec::new();

        // Exactly one restored foreground client, mirroring legacy preflight.
        let clients = discover_roblox_clients().map_err(|error| error.to_string())?;
        let visible: Vec<_> = clients
            .into_iter()
            .filter_map(|client| client.window)
            .collect();
        let [snapshot] = visible.as_slice() else {
            return Err(format!(
                "quest scanning requires exactly one visible Roblox client; found {}",
                visible.len()
            ));
        };
        let mut snapshot = *snapshot;
        if snapshot.geometry.minimized || !snapshot.is_foreground {
            // The scan button focuses the NectarPilot window; activate the
            // game first, exactly as the legacy macro's ActivateRoblox did,
            // then re-verify fail-closed before any click is sent.
            let _ = nectarpilot_platform::bring_window_to_foreground(snapshot.target.window);
            let mut activated = false;
            for _attempt in 0..4 {
                sleep(Duration::from_millis(150));
                let refreshed = discover_roblox_clients()
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .filter_map(|client| client.window)
                    .find(|candidate| candidate.target == snapshot.target);
                if let Some(refreshed) = refreshed
                    && !refreshed.geometry.minimized
                    && refreshed.is_foreground
                {
                    snapshot = refreshed;
                    activated = true;
                    break;
                }
            }
            if !activated {
                return Err("the Roblox client must be foreground and restored".into());
            }
        }
        let session = RobloxSession::from_snapshot(snapshot);
        let target = session.target();

        // Pinned open-state template from the imported general bitmaps.
        let general = std::fs::read_to_string(
            root.join("nm_image_assets")
                .join("general")
                .join("bitmaps.ahk"),
        )
        .map_err(|error| error.to_string())?;
        let questlog_bytes = nectarpilot_legacy::extract_inline_template(&general, "questlog")
            .ok_or("the pinned questlog template is missing")?;
        let questlog = template_from_png_bytes("questlog", &questlog_bytes)
            .map_err(|error| error.to_string())?;
        let strip_matcher = MultiScaleTemplateMatcher::new(TemplateMatcherConfig {
            scales: vec![0.9, 1.0, 1.1],
            stride: 2,
            minimum_confidence: 0.9,
            ambiguity_margin: 0.04,
        })
        .map_err(|error| error.to_string())?;

        let mut broker = InputBroker::new(target, WindowsInputSink);
        let capture = WindowsClientCapture;
        let is_open = |frame: &ClientFrame| -> bool {
            frame
                .crop(strip_region(frame))
                .ok()
                .and_then(|crop| {
                    strip_matcher
                        .find_best(&crop.image, &questlog)
                        .ok()
                        .flatten()
                })
                .is_some_and(|found| found.confidence >= 0.9)
        };
        // MouseMoveClient takes normalized client coordinates.
        #[allow(
            clippy::cast_precision_loss,
            reason = "client dimensions are far below f32's exact-integer ceiling"
        )]
        let (client_width, client_height) = {
            let client = session.geometry().client;
            ((client.width.max(1)) as f32, (client.height.max(1)) as f32)
        };
        let click = move |broker: &mut InputBroker<WindowsInputSink>, x: i32, y: i32| {
            #[allow(
                clippy::cast_precision_loss,
                reason = "menu offsets are small fixed constants"
            )]
            for action in [
                InputAction::MouseMoveClient {
                    x: x as f32 / client_width,
                    y: y as f32 / client_height,
                },
                InputAction::MouseDown {
                    button: MouseButton::Left,
                },
                InputAction::MouseUp {
                    button: MouseButton::Left,
                },
            ] {
                check_cancelled(cancellation)?;
                broker
                    .dispatch(action)
                    .map_err(|error| format!("quest-log click rejected: {error}"))?;
                interruptible_sleep(cancellation, Duration::from_millis(40))?;
            }
            Ok::<(), String>(())
        };

        // Open the quest log; verify against the template before reading.
        let mut opened = false;
        for _attempt in 0..4 {
            click(&mut broker, MENU_CLICK_X, MENU_CLICK_Y)?;
            interruptible_sleep(cancellation, Duration::from_millis(600))?;
            let frame = capture
                .capture(&session)
                .map_err(|error| error.to_string())?;
            if is_open(&frame) {
                opened = true;
                break;
            }
        }
        if !opened {
            let _ = broker.release_all();
            return Ok(QuestScanResult {
                scanned_at: Utc::now(),
                giver: None,
                quest_id: None,
                quest_name: None,
                bars_complete: Vec::new(),
                recommended_fields: Vec::new(),
                notes: vec![
                    "the quest log did not open at the expected menu position; \
                     no reading was attempted"
                        .into(),
                ],
            });
        }

        let reading = read_open_log(root, &session, capture, cancellation, &mut notes);
        // Cancellation is fail-closed: do not click again merely to restore UI
        // state. Dropping the broker still releases every held button.
        check_cancelled(cancellation)?;

        // Always toggle the log closed and release inputs, even after errors.
        for _attempt in 0..4 {
            click(&mut broker, MENU_CLICK_X, MENU_CLICK_Y)?;
            interruptible_sleep(cancellation, Duration::from_millis(500))?;
            let closed = capture
                .capture(&session)
                .map_or(true, |frame| !is_open(&frame));
            if closed {
                break;
            }
        }
        let _ = click(&mut broker, DEFOCUS_CLICK_X, DEFOCUS_CLICK_Y);
        broker
            .release_all()
            .map_err(|error| format!("input release failed: {error}"))?;

        let (giver, quest_id, quest_name, bars_complete, recommended_fields) = reading?;
        Ok(QuestScanResult {
            scanned_at: Utc::now(),
            giver,
            quest_id,
            quest_name,
            bars_complete,
            recommended_fields,
            notes,
        })
    }

    type LogReading = (
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<bool>,
        Vec<String>,
    );

    #[allow(
        clippy::too_many_lines,
        reason = "icon, bars, and title reads share captures and notes in one pass"
    )]
    fn read_open_log(
        root: &Path,
        session: &RobloxSession,
        capture: WindowsClientCapture,
        cancellation: &CancellationToken,
        notes: &mut Vec<String>,
    ) -> Result<LogReading, String> {
        check_cancelled(cancellation)?;
        // Giver icon: two agreeing frames within the quest-log region.
        let bindings = quest_giver_bindings(&root.join("nm_image_assets"))
            .map_err(|error| error.to_string())?;
        let icon_matcher = MultiScaleTemplateMatcher::new(TemplateMatcherConfig {
            scales: vec![0.9, 1.0, 1.1],
            stride: 3,
            minimum_confidence: 0.9,
            ambiguity_margin: 0.04,
        })
        .map_err(|error| error.to_string())?;
        let consensus = ConsensusPolicy {
            window_frames: 3,
            required_agreements: 2,
            minimum_confidence: 0.85,
        };
        let mut giver_detector =
            TemplateDetector::new("quest_giver", bindings, icon_matcher, consensus)
                .map_err(|error| error.to_string())?;

        let mut giver = None;
        let mut icon_region = None;
        for _frame_index in 0..3 {
            check_cancelled(cancellation)?;
            let frame = capture
                .capture(session)
                .map_err(|error| error.to_string())?;
            let detection = giver_detector.detect(&frame);
            if let Detection::Found {
                value, evidence, ..
            } = &detection
                && detection.actionable(0.85).is_some()
            {
                giver = Some(*value);
                icon_region = evidence.region;
                break;
            }
            interruptible_sleep(cancellation, Duration::from_millis(350))?;
        }
        let Some(giver_value) = giver else {
            notes.push("no quest giver icon reached two-frame consensus".into());
            return Ok((None, None, None, Vec::new(), Vec::new()));
        };
        let giver_name = serde_json::to_value(giver_value)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or("quest giver identity could not be serialized")?;
        let Some(icon) = icon_region else {
            notes.push("giver icon detection carried no region evidence".into());
            return Ok((Some(giver_name), None, None, Vec::new(), Vec::new()));
        };

        // Objective bars sit below the detected icon in the log column.
        let bars_region = clamp_region(NormalizedRegion {
            x: 0.01,
            y: icon.y + icon.height,
            width: 0.34,
            height: 0.30,
        });
        let bars_frame = capture
            .capture(session)
            .map_err(|error| error.to_string())?;
        let bars_complete: Vec<bool> = bars_frame
            .crop(bars_region)
            .ok()
            .map(|crop| {
                read_quest_bars(&crop.image)
                    .into_iter()
                    .map(|state| state == nectarpilot_platform::QuestBarState::Complete)
                    .collect()
            })
            .unwrap_or_default();
        if bars_complete.is_empty() {
            notes.push("no objective completion bars were readable".into());
        }

        // Title OCR, constrained to the detected giver's catalog.
        let (quest_id, quest_name) = if quest_catalog_for(giver_value).is_none() {
            notes.push(format!(
                "{giver_name} quests are dynamic; title matching is held until the \
                 dynamic-objective reader is live-validated"
            ));
            (None, None)
        } else {
            match WindowsOcr::english_us() {
                Err(error) => {
                    notes.push(format!("title OCR unavailable: {error}"));
                    (None, None)
                }
                Ok(ocr) => {
                    let title_region = clamp_region(NormalizedRegion {
                        x: icon.x + icon.width,
                        y: (icon.y - 0.01).max(0.0),
                        width: 0.30,
                        height: icon.height + 0.02,
                    });
                    let mut title_detector = QuestTitleDetector::for_giver(
                        ocr,
                        giver_value,
                        title_region,
                        ConsensusPolicy {
                            window_frames: 3,
                            required_agreements: 2,
                            minimum_confidence: 0.75,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                    let mut matched = (None, None);
                    for _frame_index in 0..3 {
                        check_cancelled(cancellation)?;
                        let frame = capture
                            .capture(session)
                            .map_err(|error| error.to_string())?;
                        let detection = title_detector.detect(&frame);
                        if let Some(candidate) = detection.actionable(0.75) {
                            matched = (
                                Some(candidate.quest_id.clone()),
                                Some(candidate.name.clone()),
                            );
                            break;
                        }
                        interruptible_sleep(cancellation, Duration::from_millis(350))?;
                    }
                    if matched.0.is_none() {
                        notes.push("no quest title reached two agreeing confident frames".into());
                    }
                    matched
                }
            }
        };

        let recommended_fields =
            recommend_fields(giver_value, quest_id.as_deref(), &bars_complete, notes);
        Ok((
            Some(giver_name),
            quest_id,
            quest_name,
            bars_complete,
            recommended_fields,
        ))
    }

    /// Fields that advance the quest's incomplete objectives, in catalog
    /// order. Non-field objectives become notes so held work stays visible.
    fn recommend_fields(
        giver: QuestGiver,
        quest_id: Option<&str>,
        bars_complete: &[bool],
        notes: &mut Vec<String>,
    ) -> Vec<String> {
        let Some(quest_id) = quest_id else {
            return Vec::new();
        };
        let Some(catalog) = quest_catalog_for(giver) else {
            return Vec::new();
        };
        let Some(quest) = catalog.quests.iter().find(|quest| quest.id == quest_id) else {
            return Vec::new();
        };
        if bars_complete.len() != quest.objectives.len() {
            notes.push(format!(
                "read {} objective bars but the matched quest has {}; field recommendations are held",
                bars_complete.len(),
                quest.objectives.len()
            ));
            return Vec::new();
        }
        let mut fields = Vec::new();
        for (index, objective) in quest.objectives.iter().enumerate() {
            if bars_complete[index] {
                continue;
            }
            match objective {
                QuestObjective::Pollen {
                    field: Some(field), ..
                }
                | QuestObjective::Goo {
                    field: Some(field), ..
                } => {
                    let name = serde_json::to_value(field)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| format!("{field:?}"));
                    if !fields.contains(&name) {
                        fields.push(name);
                    }
                }
                QuestObjective::Pollen {
                    color: Some(color), ..
                } => {
                    notes.push(format!("incomplete objective: gather {color:?} pollen"));
                }
                QuestObjective::Defeat { mob, .. } => {
                    notes.push(format!("incomplete objective: defeat {mob}"));
                }
                other => {
                    notes.push(format!(
                        "incomplete non-field objective: {}",
                        serde_json::to_string(other).unwrap_or_else(|_| "unknown".into())
                    ));
                }
            }
        }
        fields
    }

    /// The open-state strip Natro checked: y 72..152, x 0..350 client pixels.
    #[allow(
        clippy::cast_precision_loss,
        reason = "capture dimensions are capped far below f32's exact-integer ceiling"
    )]
    fn strip_region(frame: &ClientFrame) -> NormalizedRegion {
        let width = frame.image().width().max(1) as f32;
        let height = frame.image().height().max(1) as f32;
        clamp_region(NormalizedRegion {
            x: 0.0,
            y: (72.0 / height).min(0.9),
            width: (350.0 / width).min(1.0),
            height: (80.0 / height).min(0.5),
        })
    }

    fn clamp_region(region: NormalizedRegion) -> NormalizedRegion {
        let x = region.x.clamp(0.0, 0.98);
        let y = region.y.clamp(0.0, 0.98);
        NormalizedRegion {
            x,
            y,
            width: region.width.clamp(0.01, 1.0 - x),
            height: region.height.clamp(0.01, 1.0 - y),
        }
    }

    fn check_cancelled(cancellation: &CancellationToken) -> Result<(), String> {
        if cancellation.is_cancelled() {
            Err(SCAN_CANCELLED.into())
        } else {
            Ok(())
        }
    }

    fn interruptible_sleep(
        cancellation: &CancellationToken,
        duration: Duration,
    ) -> Result<(), String> {
        let deadline = std::time::Instant::now() + duration;
        loop {
            check_cancelled(cancellation)?;
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(25)));
        }
    }
}
