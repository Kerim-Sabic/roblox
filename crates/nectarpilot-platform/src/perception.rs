//! Screen-only visual perception with explicit uncertainty boundaries.
//!
//! The pipeline works exclusively on a [`ClientFrame`] from the already adopted
//! Roblox session. It has no input APIs and exposes every result as a typed
//! [`Detection`]. A caller must cross `Detection::actionable` (or the core
//! `LivePerception` helpers) before a visual result can influence movement.

use std::collections::VecDeque;

use chrono::Utc;
use image::{GrayImage, RgbaImage, imageops::FilterType};
use nectarpilot_contracts::{Detection, DetectionEvidence, NormalizedRegion};
use nectarpilot_core::quests::{QuestGiver, detect_science_bear_title, science_bear_catalog};
use nectarpilot_core::{
    FieldCandidate, HiveCandidate, LivePerception, PromptCandidate, QuestCandidate,
};
use thiserror::Error;

use crate::capture::{CaptureError, ClientCapture, ClientFrame, MAX_CAPTURE_PIXELS};
use crate::session::RobloxSession;

const MAX_TEMPLATE_PIXELS: u64 = 65_536;
const MAX_SCALED_TEMPLATE_PIXELS: u64 = 262_144;
const MAX_TEMPLATE_COMPARISONS: u64 = 20_000_000;
const MAX_SEARCH_POSITIONS: u64 = 250_000;
const MAX_VOCABULARY_ENTRIES: usize = 128;
const MAX_OCR_CHARACTERS: usize = 128;

#[derive(Debug, Error)]
pub enum PerceptionError {
    #[error("perception configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("template matching would exceed the bounded comparison budget")]
    SearchBudgetExceeded,
    #[error("template image dimensions are invalid")]
    InvalidTemplate,
}

/// A grayscale image with a reviewed identifier. Templates are supplied by a
/// signed/reviewed application asset pack; arbitrary runtime templates are not
/// loaded by this module.
#[derive(Clone, Debug)]
pub struct Template {
    id: String,
    image: GrayImage,
}

impl Template {
    pub fn new(id: impl Into<String>, image: GrayImage) -> Result<Self, PerceptionError> {
        let id = id.into();
        let pixels = u64::from(image.width())
            .checked_mul(u64::from(image.height()))
            .ok_or(PerceptionError::InvalidTemplate)?;
        if id.trim().is_empty()
            || id.len() > 96
            || image.width() < 2
            || image.height() < 2
            || pixels > MAX_TEMPLATE_PIXELS
        {
            return Err(PerceptionError::InvalidTemplate);
        }
        Ok(Self { id, image })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Clone, Debug)]
pub struct TemplateMatcherConfig {
    /// Scales are evaluated independently to tolerate Windows DPI, Roblox UI
    /// scale, and supported client resolutions.
    pub scales: Vec<f32>,
    /// Search stride in source pixels. A stride of one is allowed only when it
    /// remains inside the comparison budget.
    pub stride: u32,
    pub minimum_confidence: f32,
    pub ambiguity_margin: f32,
}

impl Default for TemplateMatcherConfig {
    fn default() -> Self {
        Self {
            scales: vec![0.8, 0.9, 1.0, 1.1, 1.25],
            stride: 2,
            minimum_confidence: 0.88,
            ambiguity_margin: 0.04,
        }
    }
}

impl TemplateMatcherConfig {
    fn validate(&self) -> Result<(), PerceptionError> {
        if self.scales.is_empty()
            || self.scales.len() > 8
            || self.stride == 0
            || self.stride > 64
            || !valid_confidence(self.minimum_confidence)
            || !valid_confidence(self.ambiguity_margin)
            || self
                .scales
                .iter()
                .any(|scale| !scale.is_finite() || !(0.5..=2.0).contains(scale))
        {
            return Err(PerceptionError::InvalidConfiguration(
                "template matcher scales, stride, or confidence thresholds are outside bounds"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

/// One best match, reported relative to the image passed to the matcher.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateMatch {
    pub template_id: String,
    pub confidence: f32,
    pub region: NormalizedRegion,
    pub scale: f32,
}

/// A bounded multi-scale normalized-image matcher. It uses mean absolute luma
/// difference rather than an unbounded general vision model, making its work
/// budget and failure modes straightforward to audit.
#[derive(Clone, Debug)]
pub struct MultiScaleTemplateMatcher {
    config: TemplateMatcherConfig,
}

impl MultiScaleTemplateMatcher {
    pub fn new(config: TemplateMatcherConfig) -> Result<Self, PerceptionError> {
        config.validate()?;
        Ok(Self { config })
    }

    #[must_use]
    pub fn config(&self) -> &TemplateMatcherConfig {
        &self.config
    }

    /// Finds the best placement for one reviewed template. `None` means no
    /// scale fits the bounded source image; a score below policy threshold is
    /// still returned so higher-level ambiguity handling can explain it.
    pub fn find_best(
        &self,
        source: &RgbaImage,
        template: &Template,
    ) -> Result<Option<TemplateMatch>, PerceptionError> {
        if source.width() == 0 || source.height() == 0 {
            return Ok(None);
        }
        let source_pixels = u64::from(source.width())
            .checked_mul(u64::from(source.height()))
            .ok_or(PerceptionError::SearchBudgetExceeded)?;
        if source_pixels > MAX_CAPTURE_PIXELS {
            return Err(PerceptionError::SearchBudgetExceeded);
        }
        let source_luma = to_luma(source);
        let mut best: Option<TemplateMatch> = None;
        for &scale in &self.config.scales {
            let scaled_width = scaled_dimension(template.image.width(), scale)?;
            let scaled_height = scaled_dimension(template.image.height(), scale)?;
            let template_pixels = u64::from(scaled_width)
                .checked_mul(u64::from(scaled_height))
                .ok_or(PerceptionError::InvalidTemplate)?;
            if template_pixels > MAX_SCALED_TEMPLATE_PIXELS {
                return Err(PerceptionError::InvalidTemplate);
            }
            if scaled_width > source_luma.width() || scaled_height > source_luma.height() {
                continue;
            }
            let scaled = image::imageops::resize(
                &template.image,
                scaled_width,
                scaled_height,
                FilterType::Triangle,
            );
            let candidate = self.find_at_scale(&source_luma, &scaled, template.id(), scale)?;
            if best
                .as_ref()
                .is_none_or(|current| candidate.confidence.total_cmp(&current.confidence).is_gt())
            {
                best = Some(candidate);
            }
        }
        Ok(best)
    }

    fn find_at_scale(
        &self,
        source: &GrayImage,
        template: &GrayImage,
        template_id: &str,
        scale: f32,
    ) -> Result<TemplateMatch, PerceptionError> {
        let max_x = source.width() - template.width();
        let max_y = source.height() - template.height();
        let x_positions = sampled_offsets(max_x, self.config.stride);
        let y_positions = sampled_offsets(max_y, self.config.stride);
        let positions = u64::try_from(x_positions.len())
            .ok()
            .and_then(|x| {
                u64::try_from(y_positions.len())
                    .ok()
                    .and_then(|y| x.checked_mul(y))
            })
            .ok_or(PerceptionError::SearchBudgetExceeded)?;
        let comparisons = positions
            .checked_mul(u64::from(template.width()) * u64::from(template.height()))
            .ok_or(PerceptionError::SearchBudgetExceeded)?;
        if positions > MAX_SEARCH_POSITIONS || comparisons > MAX_TEMPLATE_COMPARISONS {
            return Err(PerceptionError::SearchBudgetExceeded);
        }

        let mut best = (0_u32, 0_u32, -1.0_f32);
        for y in y_positions {
            for &x in &x_positions {
                let score = similarity_at(source, template, x, y);
                if score > best.2 {
                    best = (x, y, score);
                }
            }
        }
        Ok(TemplateMatch {
            template_id: template_id.to_owned(),
            confidence: best.2.max(0.0),
            region: NormalizedRegion {
                x: normalized_fraction(best.0, source.width()),
                y: normalized_fraction(best.1, source.height()),
                width: normalized_fraction(template.width(), source.width()),
                height: normalized_fraction(template.height(), source.height()),
            },
            scale,
        })
    }
}

fn valid_confidence(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn scaled_dimension(value: u32, scale: f32) -> Result<u32, PerceptionError> {
    let scaled = (f64::from(value) * f64::from(scale)).round();
    if !scaled.is_finite() || scaled < 1.0 || scaled > f64::from(u32::MAX) {
        return Err(PerceptionError::InvalidTemplate);
    }
    bounded_dimension(scaled)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "client and source dimensions are capped at 2^24 pixels, exactly representable by f32"
)]
fn normalized_fraction(numerator: u32, denominator: u32) -> f32 {
    numerator as f32 / denominator as f32
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "scaled_dimension validates finite integral 1..=u32::MAX values before conversion"
)]
fn bounded_dimension(value: f64) -> Result<u32, PerceptionError> {
    if !value.is_finite() || !(1.0..=f64::from(u32::MAX)).contains(&value) {
        return Err(PerceptionError::InvalidTemplate);
    }
    Ok(value as u32)
}

fn sampled_offsets(maximum: u32, stride: u32) -> Vec<u32> {
    let mut offsets = (0..=maximum).step_by(stride as usize).collect::<Vec<_>>();
    if offsets.last().copied() != Some(maximum) {
        offsets.push(maximum);
    }
    offsets
}

fn to_luma(source: &RgbaImage) -> GrayImage {
    GrayImage::from_fn(source.width(), source.height(), |x, y| {
        let [red, green, blue, _] = source.get_pixel(x, y).0;
        // Integer BT.601 coefficients avoid platform-dependent floating-point
        // roundoff in fixture and release comparisons.
        let luma = (u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000;
        image::Luma([u8::try_from(luma).unwrap_or(u8::MAX)])
    })
}

fn similarity_at(source: &GrayImage, template: &GrayImage, left: u32, top: u32) -> f32 {
    let mut error = 0_u64;
    for template_y in 0..template.height() {
        for template_x in 0..template.width() {
            let observed = source.get_pixel(left + template_x, top + template_y).0[0];
            let expected = template.get_pixel(template_x, template_y).0[0];
            error += u64::from(observed.abs_diff(expected));
        }
    }
    let maximum = u64::from(template.width()) * u64::from(template.height()) * 255;
    score_from_error(error, maximum)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the comparison budget caps this ratio at 20 million operations; f32 is the contract confidence type"
)]
fn score_from_error(error: u64, maximum: u64) -> f32 {
    (1.0 - error as f64 / maximum as f64) as f32
}

/// A template plus the only typed value it is permitted to emit.
#[derive(Clone, Debug)]
pub struct TemplateBinding<T> {
    pub template: Template,
    pub value: T,
    pub search_region: NormalizedRegion,
}

#[derive(Clone, Copy, Debug)]
pub struct ConsensusPolicy {
    /// Total recent frames retained, including misses and ambiguity.
    pub window_frames: usize,
    /// How many matching `Found` frames are required in that window.
    pub required_agreements: usize,
    pub minimum_confidence: f32,
}

impl Default for ConsensusPolicy {
    fn default() -> Self {
        Self {
            window_frames: 3,
            required_agreements: 2,
            minimum_confidence: 0.85,
        }
    }
}

impl ConsensusPolicy {
    fn validate(self) -> Result<(), PerceptionError> {
        if !(2..=8).contains(&self.window_frames)
            || !(2..=self.window_frames).contains(&self.required_agreements)
            || !valid_confidence(self.minimum_confidence)
        {
            return Err(PerceptionError::InvalidConfiguration(
                "temporal consensus must require 2..=8 agreeing bounded frames".to_owned(),
            ));
        }
        Ok(())
    }
}

enum ConsensusFrame<T> {
    Found { value: T, confidence: f32 },
    Other,
}

/// A generic temporal gate. Every observed frame occupies a slot, so two
/// positive readings separated by a long run of misses cannot form consensus.
pub struct TemporalConsensus<T> {
    policy: ConsensusPolicy,
    frames: VecDeque<ConsensusFrame<T>>,
}

impl<T> TemporalConsensus<T>
where
    T: Clone + Eq,
{
    pub fn new(policy: ConsensusPolicy) -> Result<Self, PerceptionError> {
        policy.validate()?;
        Ok(Self {
            policy,
            frames: VecDeque::with_capacity(policy.window_frames),
        })
    }

    pub fn observe(&mut self, detection: Detection<T>) -> Detection<T> {
        let Detection::Found {
            value,
            confidence,
            mut evidence,
        } = detection
        else {
            self.push_other();
            return detection;
        };
        if !valid_confidence(confidence) {
            self.push_other();
            return Detection::Error {
                code: "invalid_detector_confidence".to_owned(),
                message: "detector returned confidence outside 0..=1".to_owned(),
                evidence: Some(evidence),
            };
        }
        if confidence < self.policy.minimum_confidence {
            self.push_other();
            evidence.notes.push(format!(
                "confidence {confidence:.3} is below temporal safety threshold {:.3}",
                self.policy.minimum_confidence
            ));
            return Detection::Uncertain {
                reason: "candidate confidence is below the temporal safety threshold".to_owned(),
                evidence,
            };
        }
        self.push(ConsensusFrame::Found {
            value: value.clone(),
            confidence,
        });
        let agreeing = self
            .frames
            .iter()
            .filter_map(|frame| match frame {
                ConsensusFrame::Found {
                    value: candidate,
                    confidence,
                } if candidate == &value => Some(*confidence),
                ConsensusFrame::Found { .. } | ConsensusFrame::Other => None,
            })
            .collect::<Vec<_>>();
        if agreeing.len() < self.policy.required_agreements {
            evidence.notes.push(format!(
                "{} of {} required recent frames agree",
                agreeing.len(),
                self.policy.required_agreements
            ));
            return Detection::Uncertain {
                reason: "waiting for temporal consensus".to_owned(),
                evidence,
            };
        }
        let agreeing_count = u16::try_from(agreeing.len())
            .expect("consensus policy bounds agreeing frames to at most eight");
        let average = agreeing.iter().sum::<f32>() / f32::from(agreeing_count);
        evidence.notes.push(format!(
            "{} agreeing frames in the last {} observations",
            agreeing.len(),
            self.frames.len()
        ));
        Detection::Found {
            value,
            confidence: average,
            evidence,
        }
    }

    fn push_other(&mut self) {
        self.push(ConsensusFrame::Other);
    }

    fn push(&mut self, frame: ConsensusFrame<T>) {
        self.frames.push_back(frame);
        while self.frames.len() > self.policy.window_frames {
            self.frames.pop_front();
        }
    }
}

/// A typed detector backed by reviewed templates and temporal consensus.
pub struct TemplateDetector<T> {
    detector: String,
    bindings: Vec<TemplateBinding<T>>,
    matcher: MultiScaleTemplateMatcher,
    consensus: TemporalConsensus<T>,
}

impl<T> TemplateDetector<T>
where
    T: Clone + Eq,
{
    pub fn new(
        detector: impl Into<String>,
        bindings: Vec<TemplateBinding<T>>,
        matcher: MultiScaleTemplateMatcher,
        consensus: ConsensusPolicy,
    ) -> Result<Self, PerceptionError> {
        let detector = detector.into();
        if detector.trim().is_empty() || detector.len() > 96 || bindings.is_empty() {
            return Err(PerceptionError::InvalidConfiguration(
                "template detector needs a bounded name and at least one binding".to_owned(),
            ));
        }
        if bindings
            .iter()
            .any(|binding| !binding.search_region.is_valid())
        {
            return Err(PerceptionError::InvalidConfiguration(
                "template detector search regions must be normalized client crops".to_owned(),
            ));
        }
        Ok(Self {
            detector,
            bindings,
            matcher,
            consensus: TemporalConsensus::new(consensus)?,
        })
    }

    #[must_use]
    pub fn detect(&mut self, frame: &ClientFrame) -> Detection<T> {
        let mut matches = Vec::with_capacity(self.bindings.len());
        for binding in &self.bindings {
            let crop = match frame.crop(binding.search_region) {
                Ok(crop) => crop,
                Err(error) => return self.error_detection("crop_failed", error.to_string()),
            };
            let Some(result) = (match self.matcher.find_best(&crop.image, &binding.template) {
                Ok(result) => result,
                Err(error) => {
                    return self.error_detection("template_match_failed", error.to_string());
                }
            }) else {
                continue;
            };
            matches.push(BoundMatch {
                value: binding.value.clone(),
                template_id: result.template_id,
                confidence: result.confidence,
                region: compose_region(binding.search_region, result.region),
                scale: result.scale,
            });
        }
        matches.sort_by(|left, right| right.confidence.total_cmp(&left.confidence));
        let Some(best) = matches.first() else {
            return self.consensus.observe(Detection::NotFound {
                evidence: evidence(
                    &self.detector,
                    None,
                    vec!["no configured template fit the crop".to_owned()],
                ),
            });
        };
        let evidence = evidence(
            &self.detector,
            Some(best.region),
            vec![format!(
                "template={} scale={:.2} score={:.3}",
                best.template_id, best.scale, best.confidence
            )],
        );
        if best.confidence < self.matcher.config().minimum_confidence {
            let near_threshold = best.confidence
                >= (self.matcher.config().minimum_confidence
                    - self.matcher.config().ambiguity_margin)
                    .max(0.0);
            let result = if near_threshold {
                Detection::Uncertain {
                    reason: "best template match was near, but below, the confidence threshold"
                        .to_owned(),
                    evidence,
                }
            } else {
                Detection::NotFound { evidence }
            };
            return self.consensus.observe(result);
        }
        if let Some(runner_up) = matches.get(1)
            && runner_up.value != best.value
            && best.confidence - runner_up.confidence < self.matcher.config().ambiguity_margin
        {
            return self.consensus.observe(Detection::Uncertain {
                reason: "multiple template candidates were too close to distinguish safely"
                    .to_owned(),
                evidence,
            });
        }
        self.consensus.observe(Detection::Found {
            value: best.value.clone(),
            confidence: best.confidence,
            evidence,
        })
    }

    fn error_detection(&mut self, code: &str, message: String) -> Detection<T> {
        self.consensus.observe(Detection::Error {
            code: code.to_owned(),
            message,
            evidence: Some(evidence(&self.detector, None, Vec::new())),
        })
    }
}

struct BoundMatch<T> {
    value: T,
    template_id: String,
    confidence: f32,
    region: NormalizedRegion,
    scale: f32,
}

fn compose_region(parent: NormalizedRegion, child: NormalizedRegion) -> NormalizedRegion {
    let right = (parent.x + (child.x + child.width) * parent.width).min(1.0);
    let bottom = (parent.y + (child.y + child.height) * parent.height).min(1.0);
    let x = (parent.x + child.x * parent.width).min(1.0);
    let y = (parent.y + child.y * parent.height).min(1.0);
    NormalizedRegion {
        x,
        y,
        width: (right - x).max(0.0),
        height: (bottom - y).max(0.0),
    }
}

/// A bounded OCR engine interface. Implementations receive the cropped pixels
/// and a fixed vocabulary, never a screen image or an open-ended text request.
pub trait ConstrainedOcr: Send {
    fn recognize(
        &mut self,
        image: &RgbaImage,
        request: OcrRequest<'_>,
    ) -> Result<OcrRead, OcrError>;
}

#[derive(Clone, Copy, Debug)]
pub struct OcrRequest<'a> {
    pub detector: &'a str,
    pub vocabulary: &'a [String],
    pub maximum_characters: usize,
}

#[derive(Clone, Debug)]
pub struct OcrRead {
    pub text: String,
    pub confidence: f32,
}

#[derive(Debug, Error)]
pub enum OcrError {
    #[error("OCR backend failed: {0}")]
    Backend(String),
    #[error("OCR is unavailable: {0}")]
    Unavailable(String),
    #[error("OCR request is unsafe or unsupported: {0}")]
    InvalidRequest(String),
}

/// Concrete Windows.Media.Ocr adapter for English Roblox text.
///
/// Construct it with [`WindowsOcr::english_us`]. It deliberately refuses to
/// fall back to a profile language: the v1 perception catalog and fixtures are
/// English-only, so an absent `en-US` recognizer is reported as unavailable
/// rather than silently interpreting a different UI language.
#[cfg(windows)]
#[derive(Clone, Debug)]
pub struct WindowsOcr {
    language_tag: String,
}

/// Non-Windows builds retain the same explicit constructor, but report that the
/// operating-system OCR service cannot be used. This makes unsupported runtime
/// state visible to readiness checks rather than looking like an empty scan.
#[cfg(not(windows))]
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsOcr;

#[cfg(windows)]
impl WindowsOcr {
    pub const ENGLISH_US: &'static str = "en-US";

    /// Creates an adapter only when the Windows English OCR language pack is
    /// installed and can initialize on the current worker thread.
    pub fn english_us() -> Result<Self, OcrError> {
        let _apartment = WinRtApartment::initialize()?;
        let language = windows::Globalization::Language::CreateLanguage(
            &windows::core::HSTRING::from(Self::ENGLISH_US),
        )
        .map_err(|error| OcrError::Backend(error.to_string()))?;
        let supported = windows::Media::Ocr::OcrEngine::IsLanguageSupported(&language)
            .map_err(|error| OcrError::Backend(error.to_string()))?;
        if !supported {
            return Err(OcrError::Unavailable(
                "Windows English (United States) OCR language data is not installed".to_owned(),
            ));
        }
        windows::Media::Ocr::OcrEngine::TryCreateFromLanguage(&language)
            .map_err(|error| OcrError::Unavailable(error.to_string()))?;
        Ok(Self {
            language_tag: Self::ENGLISH_US.to_owned(),
        })
    }

    #[must_use]
    pub fn language_tag(&self) -> &str {
        &self.language_tag
    }

    fn engine(&self) -> Result<windows::Media::Ocr::OcrEngine, OcrError> {
        let language = windows::Globalization::Language::CreateLanguage(
            &windows::core::HSTRING::from(self.language_tag.as_str()),
        )
        .map_err(|error| OcrError::Backend(error.to_string()))?;
        windows::Media::Ocr::OcrEngine::TryCreateFromLanguage(&language)
            .map_err(|error| OcrError::Unavailable(error.to_string()))
    }
}

#[cfg(not(windows))]
impl WindowsOcr {
    pub const ENGLISH_US: &'static str = "en-US";

    pub fn english_us() -> Result<Self, OcrError> {
        Err(OcrError::Unavailable(
            "Windows.Media.Ocr is available only on Windows 10/11".to_owned(),
        ))
    }

    #[must_use]
    pub const fn language_tag(&self) -> &str {
        Self::ENGLISH_US
    }
}

#[cfg(windows)]
impl ConstrainedOcr for WindowsOcr {
    fn recognize(
        &mut self,
        image: &RgbaImage,
        request: OcrRequest<'_>,
    ) -> Result<OcrRead, OcrError> {
        validate_ocr_request(image, request)?;
        let _apartment = WinRtApartment::initialize()?;
        let engine = self.engine()?;
        let maximum_dimension = windows::Media::Ocr::OcrEngine::MaxImageDimension()
            .map_err(|error| OcrError::Backend(error.to_string()))?;
        if image.width() > maximum_dimension || image.height() > maximum_dimension {
            return Err(OcrError::InvalidRequest(format!(
                "OCR crop {}x{} exceeds Windows maximum dimension {maximum_dimension}",
                image.width(),
                image.height()
            )));
        }
        let bitmap = software_bitmap_from_rgba(image)?;
        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(|error| OcrError::Backend(error.to_string()))?
            .get()
            .map_err(|error| OcrError::Backend(error.to_string()))?;
        let text = result
            .Text()
            .map_err(|error| OcrError::Backend(error.to_string()))?
            .to_string();
        if text.chars().count() > request.maximum_characters {
            return Err(OcrError::InvalidRequest(
                "Windows OCR returned text beyond the requested bounded length".to_owned(),
            ));
        }
        Ok(OcrRead {
            confidence: vocabulary_confidence(&text, request.vocabulary),
            text,
        })
    }
}

#[cfg(not(windows))]
impl ConstrainedOcr for WindowsOcr {
    fn recognize(
        &mut self,
        _image: &RgbaImage,
        _request: OcrRequest<'_>,
    ) -> Result<OcrRead, OcrError> {
        Err(OcrError::Unavailable(
            "Windows.Media.Ocr is available only on Windows 10/11".to_owned(),
        ))
    }
}

fn validate_ocr_request(image: &RgbaImage, request: OcrRequest<'_>) -> Result<(), OcrError> {
    let pixels = u64::from(image.width())
        .checked_mul(u64::from(image.height()))
        .ok_or_else(|| OcrError::InvalidRequest("OCR crop dimensions overflow".to_owned()))?;
    if image.width() == 0
        || image.height() == 0
        || pixels > 2_000_000
        || request.detector.trim().is_empty()
        || request.vocabulary.is_empty()
        || request.vocabulary.len() > MAX_VOCABULARY_ENTRIES
        || request.maximum_characters == 0
        || request.maximum_characters > MAX_OCR_CHARACTERS
        || request
            .vocabulary
            .iter()
            .any(|entry| entry.trim().is_empty() || entry.chars().count() > MAX_OCR_CHARACTERS)
    {
        return Err(OcrError::InvalidRequest(
            "OCR crop, vocabulary, or text limit exceeds the bounded policy".to_owned(),
        ));
    }
    Ok(())
}

/// Windows.Media.Ocr does not expose a per-word confidence score. We therefore
/// issue a high score only for an exact, unique normalized vocabulary match;
/// any other reading stays below the action threshold and the catalog matcher
/// plus two-frame consensus will retain it as `Uncertain`.
fn vocabulary_confidence(text: &str, vocabulary: &[String]) -> f32 {
    let observed = normalize_ocr_text(text);
    if observed.is_empty() {
        return 0.0;
    }
    let matches = vocabulary
        .iter()
        .filter(|candidate| normalize_ocr_text(candidate) == observed)
        .count();
    if matches == 1 { 0.92 } else { 0.5 }
}

fn normalize_ocr_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
struct WinRtApartment;

#[cfg(windows)]
impl WinRtApartment {
    fn initialize() -> Result<Self, OcrError> {
        // SAFETY: RoInitialize/RoUninitialize are balanced by this thread-local
        // RAII guard. A conflicting apartment model returns an explicit error.
        unsafe {
            windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            )
        }
        .map_err(|error| {
            OcrError::Unavailable(format!(
                "Windows OCR requires an MTA worker apartment: {error}"
            ))
        })?;
        Ok(Self)
    }
}

#[cfg(windows)]
impl Drop for WinRtApartment {
    fn drop(&mut self) {
        // SAFETY: this call balances the successful RoInitialize in `initialize`.
        unsafe { windows::Win32::System::WinRT::RoUninitialize() };
    }
}

#[cfg(windows)]
fn software_bitmap_from_rgba(
    image: &RgbaImage,
) -> Result<windows::Graphics::Imaging::SoftwareBitmap, OcrError> {
    use std::io::Cursor;

    let mut encoded = Vec::new();
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut Cursor::new(&mut encoded), image::ImageFormat::Png)
        .map_err(|error| OcrError::Backend(error.to_string()))?;
    let stream = windows::Storage::Streams::InMemoryRandomAccessStream::new()
        .map_err(|error| OcrError::Backend(error.to_string()))?;
    let output = stream
        .GetOutputStreamAt(0)
        .map_err(|error| OcrError::Backend(error.to_string()))?;
    let writer = windows::Storage::Streams::DataWriter::CreateDataWriter(&output)
        .map_err(|error| OcrError::Backend(error.to_string()))?;
    writer
        .WriteBytes(&encoded)
        .map_err(|error| OcrError::Backend(error.to_string()))?;
    writer
        .StoreAsync()
        .map_err(|error| OcrError::Backend(error.to_string()))?
        .get()
        .map_err(|error| OcrError::Backend(error.to_string()))?;
    stream
        .Seek(0)
        .map_err(|error| OcrError::Backend(error.to_string()))?;
    let decoder = windows::Graphics::Imaging::BitmapDecoder::CreateAsync(&stream)
        .map_err(|error| OcrError::Backend(error.to_string()))?
        .get()
        .map_err(|error| OcrError::Backend(error.to_string()))?;
    decoder
        .GetSoftwareBitmapAsync()
        .map_err(|error| OcrError::Backend(error.to_string()))?
        .get()
        .map_err(|error| OcrError::Backend(error.to_string()))
}

/// OCR-backed Science Bear title detector. The recognizer is constrained to the
/// checked-in catalog, then the catalog matcher and temporal gate independently
/// reject ambiguous or one-frame readings.
pub struct ScienceBearQuestDetector<O> {
    ocr: O,
    region: NormalizedRegion,
    vocabulary: Vec<String>,
    consensus: TemporalConsensus<QuestCandidate>,
}

impl<O> ScienceBearQuestDetector<O>
where
    O: ConstrainedOcr,
{
    pub fn new(
        ocr: O,
        region: NormalizedRegion,
        consensus: ConsensusPolicy,
    ) -> Result<Self, PerceptionError> {
        if !region.is_valid() {
            return Err(PerceptionError::InvalidConfiguration(
                "quest OCR region must be a normalized client crop".to_owned(),
            ));
        }
        let vocabulary = science_bear_catalog()
            .quests
            .into_iter()
            .map(|quest| quest.name)
            .collect::<Vec<_>>();
        if vocabulary.is_empty() || vocabulary.len() > MAX_VOCABULARY_ENTRIES {
            return Err(PerceptionError::InvalidConfiguration(
                "Science Bear vocabulary is outside safe bounds".to_owned(),
            ));
        }
        Ok(Self {
            ocr,
            region,
            vocabulary,
            consensus: TemporalConsensus::new(consensus)?,
        })
    }

    #[must_use]
    pub fn detect(&mut self, frame: &ClientFrame) -> Detection<QuestCandidate> {
        let crop = match frame.crop(self.region) {
            Ok(crop) => crop,
            Err(error) => return self.error_detection("crop_failed", error.to_string()),
        };
        let read = match self.ocr.recognize(
            &crop.image,
            OcrRequest {
                detector: "science_bear_quest_title",
                vocabulary: &self.vocabulary,
                maximum_characters: MAX_OCR_CHARACTERS,
            },
        ) {
            Ok(read) => read,
            Err(error) => return self.error_detection("ocr_failed", error.to_string()),
        };
        let base_evidence = evidence(
            "science_bear_quest_title",
            Some(self.region),
            vec!["OCR result was constrained to the 31-title Science Bear catalog".to_owned()],
        );
        if read.text.chars().count() > MAX_OCR_CHARACTERS {
            return self.consensus.observe(Detection::Uncertain {
                reason: "OCR output exceeded the bounded title length".to_owned(),
                evidence: base_evidence,
            });
        }
        let matched = detect_science_bear_title(&read.text, read.confidence);
        let mapped = match matched {
            Detection::Found {
                value, confidence, ..
            } => Detection::Found {
                value: QuestCandidate {
                    giver: QuestGiver::ScienceBear,
                    quest_id: value.id,
                    sequence: value.sequence,
                    name: value.name,
                },
                confidence,
                evidence: base_evidence,
            },
            Detection::NotFound { .. } => Detection::NotFound {
                evidence: base_evidence,
            },
            Detection::Uncertain { reason, .. } => Detection::Uncertain {
                reason,
                evidence: base_evidence,
            },
            Detection::Error { code, message, .. } => Detection::Error {
                code,
                message,
                evidence: Some(base_evidence),
            },
        };
        self.consensus.observe(mapped)
    }

    fn error_detection(&mut self, code: &str, message: String) -> Detection<QuestCandidate> {
        self.consensus.observe(Detection::Error {
            code: code.to_owned(),
            message,
            evidence: Some(evidence(
                "science_bear_quest_title",
                Some(self.region),
                Vec::new(),
            )),
        })
    }
}

/// A concrete assembly of capture, OCR, and typed template detectors. It emits
/// observations only; task/state-machine code owns all movement and input.
pub struct LivePerceptionPipeline<C, O> {
    capture: C,
    quest: ScienceBearQuestDetector<O>,
    field: TemplateDetector<FieldCandidate>,
    hive: TemplateDetector<HiveCandidate>,
    prompt: TemplateDetector<PromptCandidate>,
}

impl<C, O> LivePerceptionPipeline<C, O>
where
    C: ClientCapture,
    O: ConstrainedOcr,
{
    #[must_use]
    pub fn new(
        capture: C,
        quest: ScienceBearQuestDetector<O>,
        field: TemplateDetector<FieldCandidate>,
        hive: TemplateDetector<HiveCandidate>,
        prompt: TemplateDetector<PromptCandidate>,
    ) -> Self {
        Self {
            capture,
            quest,
            field,
            hive,
            prompt,
        }
    }

    /// Captures only the adopted Roblox client then produces four typed,
    /// non-actionable observations. A capture failure is not converted into a
    /// target and must be handled by the caller as a paused/recovery condition.
    pub fn observe(&mut self, session: &RobloxSession) -> Result<LivePerception, CaptureError> {
        let frame = self.capture.capture(session)?;
        Ok(self.analyze_frame(&frame))
    }

    #[must_use]
    pub fn analyze_frame(&mut self, frame: &ClientFrame) -> LivePerception {
        LivePerception {
            quest: self.quest.detect(frame),
            field: self.field.detect(frame),
            hive: self.hive.detect(frame),
            prompt: self.prompt.detect(frame),
        }
    }
}

fn evidence(
    detector: &str,
    region: Option<NormalizedRegion>,
    notes: Vec<String>,
) -> DetectionEvidence {
    DetectionEvidence {
        detector: detector.to_owned(),
        observed_at: Utc::now(),
        region,
        artifact_id: None,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use chrono::Utc;
    use image::{GrayImage, Luma, Rgba, RgbaImage, imageops::FilterType};
    use nectarpilot_contracts::{Detection, NormalizedRegion};
    use nectarpilot_core::quests::FieldId;
    use nectarpilot_core::{FieldCandidate, HiveCandidate, HiveState, PromptCandidate, PromptKind};

    use super::{
        ClientCapture, ClientFrame, ConsensusPolicy, ConstrainedOcr, LivePerceptionPipeline,
        MultiScaleTemplateMatcher, OcrError, OcrRead, OcrRequest, PerceptionError,
        ScienceBearQuestDetector, Template, TemplateBinding, TemplateDetector,
        TemplateMatcherConfig,
    };
    use crate::capture::CaptureError;
    use crate::session::{
        ProcessId, Rect, RobloxSession, SessionTarget, WindowGeometry, WindowHandle, WindowSnapshot,
    };

    const FULL: NormalizedRegion = NormalizedRegion {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    fn target() -> SessionTarget {
        SessionTarget {
            pid: ProcessId::new(71).unwrap(),
            window: WindowHandle::new(72).unwrap(),
        }
    }

    fn fixture_icon() -> GrayImage {
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

    fn frame_with_icon(scale: f32, x: u32, y: u32) -> ClientFrame {
        let icon = fixture_icon();
        let width = super::scaled_dimension(8, scale).unwrap();
        let height = super::scaled_dimension(8, scale).unwrap();
        let scaled = image::imageops::resize(&icon, width, height, FilterType::Triangle);
        let mut image = RgbaImage::from_pixel(160, 100, Rgba([8, 11, 15, 255]));
        for icon_y in 0..height {
            for icon_x in 0..width {
                let value = scaled.get_pixel(icon_x, icon_y).0[0];
                image.put_pixel(x + icon_x, y + icon_y, Rgba([value, value, value, 255]));
            }
        }
        ClientFrame::new(target(), 0, Utc::now(), image).unwrap()
    }

    fn matcher() -> MultiScaleTemplateMatcher {
        MultiScaleTemplateMatcher::new(TemplateMatcherConfig {
            scales: vec![1.0, 1.5],
            stride: 1,
            minimum_confidence: 0.9,
            ambiguity_margin: 0.04,
        })
        .unwrap()
    }

    fn consensus() -> ConsensusPolicy {
        ConsensusPolicy {
            window_frames: 3,
            required_agreements: 2,
            minimum_confidence: 0.85,
        }
    }

    #[test]
    fn fixture_multiscale_match_locates_scaled_template_in_client_coordinates() {
        let frame = frame_with_icon(1.5, 80, 40);
        let template = Template::new("fixture-field", fixture_icon()).unwrap();
        let result = matcher()
            .find_best(frame.image(), &template)
            .unwrap()
            .unwrap();

        assert!((result.scale - 1.5).abs() < f32::EPSILON);
        assert!((result.confidence - 1.0).abs() < f32::EPSILON);
        assert!((result.region.x - 0.5).abs() < 0.001);
        assert!((result.region.y - 0.4).abs() < 0.001);
        assert!(result.region.is_valid());
    }

    #[test]
    fn fixture_ambiguity_stays_uncertain() {
        let frame = frame_with_icon(1.0, 20, 20);
        let first = TemplateBinding {
            template: Template::new("bamboo", fixture_icon()).unwrap(),
            value: FieldCandidate {
                field: FieldId::Bamboo,
            },
            search_region: FULL,
        };
        let second = TemplateBinding {
            template: Template::new("pineapple", fixture_icon()).unwrap(),
            value: FieldCandidate {
                field: FieldId::Pineapple,
            },
            search_region: FULL,
        };
        let mut detector =
            TemplateDetector::new("field", vec![first, second], matcher(), consensus()).unwrap();

        let detection = detector.detect(&frame);
        assert!(matches!(detection, Detection::Uncertain { .. }));
        assert_eq!(detection.actionable(0.0), None);
    }

    #[test]
    fn fixture_temporal_consensus_requires_two_recent_frames() {
        let frame = frame_with_icon(1.0, 20, 20);
        let binding = TemplateBinding {
            template: Template::new("bamboo", fixture_icon()).unwrap(),
            value: FieldCandidate {
                field: FieldId::Bamboo,
            },
            search_region: FULL,
        };
        let mut detector =
            TemplateDetector::new("field", vec![binding], matcher(), consensus()).unwrap();

        assert!(matches!(
            detector.detect(&frame),
            Detection::Uncertain { .. }
        ));
        let detection = detector.detect(&frame);
        assert_eq!(
            detection.actionable(0.85),
            Some(&FieldCandidate {
                field: FieldId::Bamboo
            })
        );
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
            assert!(
                request
                    .vocabulary
                    .iter()
                    .any(|title| title == "Preliminary Research")
            );
            self.reads
                .pop_front()
                .unwrap_or_else(|| Err(OcrError::Backend("fixture exhausted".to_owned())))
        }
    }

    #[test]
    fn fixture_constrained_ocr_requires_catalog_match_and_temporal_consensus() {
        let frame = frame_with_icon(1.0, 20, 20);
        let mut detector = ScienceBearQuestDetector::new(
            FixtureOcr {
                reads: VecDeque::from([
                    Ok(OcrRead {
                        text: "Preliminary Research".to_owned(),
                        confidence: 0.98,
                    }),
                    Ok(OcrRead {
                        text: "Preliminary Research".to_owned(),
                        confidence: 0.96,
                    }),
                ]),
            },
            FULL,
            consensus(),
        )
        .unwrap();

        assert!(matches!(
            detector.detect(&frame),
            Detection::Uncertain { .. }
        ));
        let detection = detector.detect(&frame);
        let Some(candidate) = detection.actionable(0.85) else {
            panic!("two agreeing catalog frames must become a confident detection");
        };
        assert_eq!(candidate.sequence, 1);
        assert_eq!(
            candidate.giver,
            nectarpilot_core::quests::QuestGiver::ScienceBear
        );
    }

    #[test]
    fn unknown_ocr_is_not_a_quest_candidate() {
        let frame = frame_with_icon(1.0, 20, 20);
        let mut detector = ScienceBearQuestDetector::new(
            FixtureOcr {
                reads: VecDeque::from([Ok(OcrRead {
                    text: "Brown Bear Unknown".to_owned(),
                    confidence: 0.99,
                })]),
            },
            FULL,
            consensus(),
        )
        .unwrap();
        let detection = detector.detect(&frame);
        assert!(matches!(detection, Detection::Uncertain { .. }));
        assert_eq!(detection.actionable(0.0), None);
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

    fn fixture_session() -> RobloxSession {
        RobloxSession::from_snapshot(WindowSnapshot {
            target: target(),
            geometry: WindowGeometry {
                outer: Rect {
                    left: 0,
                    top: 0,
                    width: 160,
                    height: 100,
                },
                client: Rect {
                    left: 0,
                    top: 0,
                    width: 160,
                    height: 100,
                },
                monitor: Rect {
                    left: 0,
                    top: 0,
                    width: 160,
                    height: 100,
                },
                dpi: 96,
                minimized: false,
                fullscreen: false,
            },
            is_foreground: true,
        })
    }

    fn detector<T: Clone + Eq>(value: T, name: &str) -> TemplateDetector<T> {
        TemplateDetector::new(
            name,
            vec![TemplateBinding {
                template: Template::new(format!("{name}-icon"), fixture_icon()).unwrap(),
                value,
                search_region: FULL,
            }],
            matcher(),
            consensus(),
        )
        .unwrap()
    }

    #[test]
    fn fixture_pipeline_emits_typed_observations_but_no_first_frame_target() {
        let frame = frame_with_icon(1.0, 20, 20);
        let capture = FixtureCapture(frame);
        let quest = ScienceBearQuestDetector::new(
            FixtureOcr {
                reads: VecDeque::from([Ok(OcrRead {
                    text: "Preliminary Research".to_owned(),
                    confidence: 0.99,
                })]),
            },
            FULL,
            consensus(),
        )
        .unwrap();
        let mut pipeline = LivePerceptionPipeline::new(
            capture,
            quest,
            detector(
                FieldCandidate {
                    field: FieldId::Bamboo,
                },
                "field",
            ),
            detector(
                HiveCandidate {
                    slot: 1,
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

        let perception = pipeline.observe(&fixture_session()).unwrap();
        assert!(matches!(perception.field, Detection::Uncertain { .. }));
        assert_eq!(perception.field_target(), None);
        assert!(matches!(perception.quest, Detection::Uncertain { .. }));
    }

    #[test]
    fn unsafe_matcher_config_is_rejected() {
        let error = MultiScaleTemplateMatcher::new(TemplateMatcherConfig {
            scales: vec![0.25],
            ..TemplateMatcherConfig::default()
        })
        .unwrap_err();
        assert!(matches!(error, PerceptionError::InvalidConfiguration(_)));
    }
}
