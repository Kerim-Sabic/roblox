//! End-to-end fixture test for public capture/perception APIs.
//!
//! The metadata deliberately describes a synthetic *client* image rather than
//! a desktop screenshot. This exercises the public pipeline boundary without
//! requiring Roblox or a physical display in CI.

use std::collections::VecDeque;

use chrono::Utc;
use image::{GrayImage, Luma, Rgba, RgbaImage, imageops::FilterType};
use nectarpilot_contracts::{Detection, NormalizedRegion};
use nectarpilot_core::quests::FieldId;
use nectarpilot_core::{FieldCandidate, HiveCandidate, HiveState, PromptCandidate, PromptKind};
use nectarpilot_platform::session::{Rect, WindowGeometry, WindowSnapshot};
use nectarpilot_platform::{
    CaptureError, ClientCapture, ClientFrame, ConsensusPolicy, ConstrainedOcr,
    LivePerceptionPipeline, MultiScaleTemplateMatcher, OcrError, OcrRead, OcrRequest, ProcessId,
    RobloxSession, ScienceBearQuestDetector, SessionTarget, Template, TemplateBinding,
    TemplateDetector, TemplateMatcherConfig, WindowHandle,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct FixtureMetadata {
    id: String,
    client: ClientSize,
    template: TemplatePlacement,
    expected: Expected,
}

#[derive(Deserialize)]
struct ClientSize {
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct TemplatePlacement {
    x: u32,
    y: u32,
    scale: f32,
}

#[derive(Deserialize)]
struct Expected {
    field: String,
    quest_title: String,
    hive_slot: u8,
    prompt: String,
}

const FULL_CLIENT: NormalizedRegion = NormalizedRegion {
    x: 0.0,
    y: 0.0,
    width: 1.0,
    height: 1.0,
};

fn metadata() -> FixtureMetadata {
    serde_json::from_str(include_str!("fixtures/perception-fixture.json"))
        .expect("checked-in synthetic perception fixture metadata must parse")
}

fn target() -> SessionTarget {
    SessionTarget {
        pid: ProcessId::new(501).expect("nonzero fixture PID"),
        window: WindowHandle::new(502).expect("nonzero fixture HWND"),
    }
}

fn icon() -> GrayImage {
    GrayImage::from_fn(8, 8, |x, y| {
        let value = if x == y || x + y == 7 {
            240
        } else if (x + y) % 3 == 0 {
            160
        } else {
            35
        };
        Luma([value])
    })
}

fn frame_from_fixture(fixture: &FixtureMetadata) -> ClientFrame {
    assert!((fixture.template.scale - 1.5).abs() < f32::EPSILON);
    let width = 12;
    let height = 12;
    let scaled = image::imageops::resize(&icon(), width, height, FilterType::Triangle);
    let mut image = RgbaImage::from_pixel(
        fixture.client.width,
        fixture.client.height,
        Rgba([8, 11, 15, 255]),
    );
    for y in 0..height {
        for x in 0..width {
            let pixel = scaled.get_pixel(x, y).0[0];
            image.put_pixel(
                fixture.template.x + x,
                fixture.template.y + y,
                Rgba([pixel, pixel, pixel, 255]),
            );
        }
    }
    ClientFrame::new(target(), 0, Utc::now(), image).expect("fixture image is bounded")
}

#[derive(Clone)]
struct FixtureCapture(ClientFrame);

impl ClientCapture for FixtureCapture {
    fn capture(&self, session: &RobloxSession) -> Result<ClientFrame, CaptureError> {
        if session.target() != self.0.target {
            return Err(CaptureError::TargetMismatch);
        }
        Ok(self.0.clone())
    }
}

struct FixtureOcr {
    reads: VecDeque<Result<OcrRead, OcrError>>,
}

impl ConstrainedOcr for FixtureOcr {
    fn recognize(
        &mut self,
        _image: &RgbaImage,
        request: OcrRequest<'_>,
    ) -> Result<OcrRead, OcrError> {
        assert_eq!(request.detector, "science_bear_quest_title");
        assert!(request.vocabulary.len() >= 31);
        self.reads
            .pop_front()
            .unwrap_or_else(|| Err(OcrError::Backend("fixture OCR exhausted".to_owned())))
    }
}

fn matcher() -> MultiScaleTemplateMatcher {
    MultiScaleTemplateMatcher::new(TemplateMatcherConfig {
        scales: vec![1.0, 1.5],
        stride: 1,
        minimum_confidence: 0.9,
        ambiguity_margin: 0.04,
    })
    .expect("fixture matcher config is safe")
}

fn consensus() -> ConsensusPolicy {
    ConsensusPolicy {
        window_frames: 3,
        required_agreements: 2,
        minimum_confidence: 0.85,
    }
}

fn detector<T: Clone + Eq>(value: T, name: &str) -> TemplateDetector<T> {
    TemplateDetector::new(
        name,
        vec![TemplateBinding {
            template: Template::new(format!("{name}-fixture"), icon())
                .expect("fixture template is valid"),
            value,
            search_region: FULL_CLIENT,
        }],
        matcher(),
        consensus(),
    )
    .expect("fixture detector config is safe")
}

fn session(fixture: &FixtureMetadata) -> RobloxSession {
    RobloxSession::from_snapshot(WindowSnapshot {
        target: target(),
        geometry: WindowGeometry {
            outer: Rect {
                left: 0,
                top: 0,
                width: fixture.client.width,
                height: fixture.client.height,
            },
            client: Rect {
                left: 0,
                top: 0,
                width: fixture.client.width,
                height: fixture.client.height,
            },
            monitor: Rect {
                left: 0,
                top: 0,
                width: fixture.client.width,
                height: fixture.client.height,
            },
            dpi: 144,
            minimized: false,
            fullscreen: false,
        },
        is_foreground: true,
    })
}

#[test]
fn synthetic_fixture_requires_consensus_before_exporting_typed_targets() {
    let fixture = metadata();
    assert_eq!(fixture.id, "synthetic-client-150pct-field");
    assert_eq!(fixture.expected.field, "bamboo");
    assert_eq!(fixture.expected.prompt, "interact");

    let frame = frame_from_fixture(&fixture);
    let quest = ScienceBearQuestDetector::new(
        FixtureOcr {
            reads: VecDeque::from([
                Ok(OcrRead {
                    text: fixture.expected.quest_title.clone(),
                    confidence: 0.99,
                }),
                Ok(OcrRead {
                    text: fixture.expected.quest_title.clone(),
                    confidence: 0.99,
                }),
            ]),
        },
        FULL_CLIENT,
        consensus(),
    )
    .expect("fixture OCR config is safe");
    let mut pipeline = LivePerceptionPipeline::new(
        FixtureCapture(frame),
        quest,
        detector(
            FieldCandidate {
                field: FieldId::Bamboo,
            },
            "field",
        ),
        detector(
            HiveCandidate {
                slot: fixture.expected.hive_slot,
                state: HiveState::ClaimedByAttachedSession,
            },
            "hive",
        ),
        detector(
            PromptCandidate {
                kind: PromptKind::Interact,
            },
            "prompt",
        ),
    );
    let session = session(&fixture);

    let first = pipeline
        .observe(&session)
        .expect("fixture capture succeeds");
    assert!(matches!(first.field, Detection::Uncertain { .. }));
    assert_eq!(first.field_target(), None);
    assert_eq!(first.hive_target(), None);
    assert_eq!(first.prompt_target(), None);

    let second = pipeline
        .observe(&session)
        .expect("fixture capture succeeds");
    assert_eq!(second.field_target(), Some(FieldId::Bamboo));
    assert_eq!(second.hive_target().map(|hive| hive.slot), Some(1));
    assert_eq!(
        second.prompt_target().map(|prompt| prompt.kind),
        Some(PromptKind::Interact)
    );
    let quest = second.quest.actionable(0.85).expect("two OCR frames agree");
    assert_eq!(quest.name, fixture.expected.quest_title);
}
