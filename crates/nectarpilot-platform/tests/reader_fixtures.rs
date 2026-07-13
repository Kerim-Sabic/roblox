//! Fixture tests for the honey counter reader, quest-bar reader, and quest
//! giver icon detector. Frames are composed from the real imported templates
//! under `nm_image_assets`, so these tests exercise the exact pixels the
//! legacy macro shipped without needing Roblox or a display.

use std::collections::VecDeque;
use std::path::PathBuf;

use chrono::Utc;
use image::{Rgba, RgbaImage};
use nectarpilot_contracts::Detection;
use nectarpilot_core::quests::QuestGiver;
use nectarpilot_platform::{
    ClientFrame, ConsensusPolicy, ConstrainedOcr, HoneyCounterReader, MultiScaleTemplateMatcher,
    OcrError, OcrRead, OcrRequest, ProcessId, QuestBarState, SessionTarget, TemplateDetector,
    TemplateMatcherConfig, WindowHandle, quest_giver_bindings, read_quest_bars,
};

fn repo_assets_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../nm_image_assets")
}

fn target() -> SessionTarget {
    SessionTarget {
        pid: ProcessId::new(31).unwrap(),
        window: WindowHandle::new(32).unwrap(),
    }
}

fn frame_from(image: RgbaImage) -> ClientFrame {
    ClientFrame::new(target(), 0, Utc::now(), image).unwrap()
}

// ---- quest bars ------------------------------------------------------------

const COMPLETE: Rgba<u8> = Rgba([0x6E, 0xFF, 0x60, 255]);
const INCOMPLETE: Rgba<u8> = Rgba([0xF4, 0x6C, 0x55, 255]);

fn paint_bar(image: &mut RgbaImage, top: u32, height: u32, color: Rgba<u8>) {
    for y in top..top + height {
        for x in 10..image.width() - 10 {
            image.put_pixel(x, y, color);
        }
    }
}

#[test]
fn quest_bars_read_in_screen_order_with_legacy_colors() {
    let mut crop = RgbaImage::from_pixel(300, 200, Rgba([40, 40, 46, 255]));
    paint_bar(&mut crop, 20, 12, INCOMPLETE);
    paint_bar(&mut crop, 70, 12, COMPLETE);
    paint_bar(&mut crop, 120, 12, INCOMPLETE);

    assert_eq!(
        read_quest_bars(&crop),
        vec![
            QuestBarState::Incomplete,
            QuestBarState::Complete,
            QuestBarState::Incomplete,
        ]
    );
}

#[test]
fn quest_bars_ignore_thin_noise_and_off_palette_colors() {
    let mut crop = RgbaImage::from_pixel(300, 120, Rgba([40, 40, 46, 255]));
    // Two rows only: below the three-row minimum.
    paint_bar(&mut crop, 10, 2, COMPLETE);
    // Full-height band in a color outside both tolerances.
    paint_bar(&mut crop, 40, 14, Rgba([0x20, 0x90, 0xF0, 255]));

    assert!(read_quest_bars(&crop).is_empty());
}

// ---- quest giver icons -----------------------------------------------------

#[test]
fn real_bucko_icon_is_detected_and_not_confused_with_riley() {
    let bindings = quest_giver_bindings(&repo_assets_root()).expect("repo icon templates load");
    assert!(bindings.len() >= 18, "all giver icon variants must load");

    // Paste the real Bucko icon into the quest-log third of a client frame.
    let bytes = std::fs::read(repo_assets_root().join("bucko.png")).unwrap();
    let icon = image::load_from_memory(&bytes).unwrap().to_rgba8();
    let mut client = RgbaImage::from_pixel(640, 360, Rgba([28, 28, 34, 255]));
    image::imageops::overlay(&mut client, &icon, 39, 120);
    let frame = frame_from(client);

    // Stride 3 keeps the 18-template sweep inside the bounded comparison
    // budget for a quest-log-sized region.
    let matcher = MultiScaleTemplateMatcher::new(TemplateMatcherConfig {
        scales: vec![1.0],
        stride: 3,
        minimum_confidence: 0.9,
        ambiguity_margin: 0.04,
    })
    .unwrap();
    let mut detector = TemplateDetector::new(
        "quest_giver",
        bindings,
        matcher,
        ConsensusPolicy {
            window_frames: 3,
            required_agreements: 2,
            minimum_confidence: 0.85,
        },
    )
    .unwrap();

    // First frame is Uncertain by temporal policy; the second must resolve.
    assert!(matches!(
        detector.detect(&frame),
        Detection::Uncertain { .. }
    ));
    let detection = detector.detect(&frame);
    assert_eq!(
        detection.actionable(0.85),
        Some(&QuestGiver::GiftedBuckoBee),
        "second agreeing frame must identify Bucko: {detection:?}"
    );
}

// ---- honey counter ---------------------------------------------------------

struct ScriptedOcr {
    reads: VecDeque<Result<OcrRead, OcrError>>,
}

impl ConstrainedOcr for ScriptedOcr {
    fn recognize(
        &mut self,
        _image: &RgbaImage,
        request: OcrRequest<'_>,
    ) -> Result<OcrRead, OcrError> {
        assert_eq!(request.detector, "honey_counter");
        self.reads
            .pop_front()
            .unwrap_or(Err(OcrError::Backend("script exhausted".into())))
    }
}

fn honey_frame() -> ClientFrame {
    frame_from(RgbaImage::from_pixel(1280, 720, Rgba([20, 20, 24, 255])))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "reads sit alongside Err entries in the scripted OCR queues"
)]
fn read(text: &str) -> Result<OcrRead, OcrError> {
    Ok(OcrRead {
        text: text.to_owned(),
        confidence: 0.5,
    })
}

#[test]
fn honey_value_needs_two_agreeing_variants_with_digit_normalization() {
    // "1,234,5o7" normalizes to 1234507 and agrees with the clean read; the
    // stray "999" variant is outvoted.
    let mut reader = HoneyCounterReader::new(ScriptedOcr {
        reads: VecDeque::from([
            read("1,234,507"),
            read("1,234,5o7"),
            read("999"),
            Err(OcrError::Backend("blur".into())),
            read("garbage"),
        ]),
    });
    let detection = reader.read(&honey_frame());
    assert_eq!(detection.actionable(0.0), Some(&1_234_507));
}

#[test]
fn conflicting_or_lone_honey_votes_stay_uncertain() {
    let mut reader = HoneyCounterReader::new(ScriptedOcr {
        reads: VecDeque::from([
            read("100"),
            read("100"),
            read("200"),
            read("200"),
            read("300"),
        ]),
    });
    assert!(matches!(
        reader.read(&honey_frame()),
        Detection::Uncertain { .. }
    ));

    let mut reader = HoneyCounterReader::new(ScriptedOcr {
        reads: VecDeque::from([read("100"), read("2"), read("3"), read("4"), read("5")]),
    });
    assert!(matches!(
        reader.read(&honey_frame()),
        Detection::Uncertain { .. }
    ));
}

#[test]
fn small_clients_and_total_ocr_failure_are_reported_not_guessed() {
    let mut reader = HoneyCounterReader::new(ScriptedOcr {
        reads: VecDeque::new(),
    });
    let small = frame_from(RgbaImage::from_pixel(640, 360, Rgba([0, 0, 0, 255])));
    assert!(matches!(reader.read(&small), Detection::Uncertain { .. }));

    let mut reader = HoneyCounterReader::new(ScriptedOcr {
        reads: VecDeque::from([
            Err(OcrError::Backend("a".into())),
            Err(OcrError::Backend("b".into())),
            Err(OcrError::Backend("c".into())),
            Err(OcrError::Backend("d".into())),
            Err(OcrError::Backend("e".into())),
        ]),
    });
    assert!(matches!(
        reader.read(&honey_frame()),
        Detection::Error { .. }
    ));
}
