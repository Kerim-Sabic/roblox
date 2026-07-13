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
use nectarpilot_core::quests::{
    QuestGiver, detect_quest_title, quest_catalog_for, quest_title_detector_name,
};
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

/// Upper bound for a prepared OCR image; also enforced on raw requests.
const MAX_OCR_PIXELS: u64 = 2_000_000;
/// Windows OCR recognizes game text reliably once glyphs are roughly 20+
/// pixels tall; HUD crops are upscaled toward this height before recognition.
const OCR_TARGET_HEIGHT: u32 = 96;
const OCR_MAX_UPSCALE: u32 = 4;

/// Deterministic OCR preparation: percentile contrast stretch plus bounded
/// integer upscaling. Small low-contrast HUD crops are the dominant OCR
/// failure mode; both transforms measurably improve Windows.Media.Ocr reads
/// while staying inside the fixed pixel budget.
#[must_use]
pub fn preprocess_for_ocr(image: &RgbaImage) -> RgbaImage {
    if image.width() == 0 || image.height() == 0 {
        return image.clone();
    }
    let stretched = stretch_contrast(image);
    let scale = ocr_upscale_factor(stretched.width(), stretched.height());
    if scale <= 1 {
        return stretched;
    }
    image::imageops::resize(
        &stretched,
        stretched.width() * scale,
        stretched.height() * scale,
        FilterType::CatmullRom,
    )
}

fn ocr_upscale_factor(width: u32, height: u32) -> u32 {
    if height == 0 || height >= OCR_TARGET_HEIGHT {
        return 1;
    }
    let desired = OCR_TARGET_HEIGHT.div_ceil(height).min(OCR_MAX_UPSCALE);
    let pixels = u64::from(width) * u64::from(height);
    let mut scale = desired.max(1);
    while scale > 1 && pixels * u64::from(scale) * u64::from(scale) > MAX_OCR_PIXELS {
        scale -= 1;
    }
    scale
}

/// Linear luma stretch anchored on the 1st and 99th percentiles: wide enough
/// to trim outliers, tight enough that thin text strokes (a few percent of
/// the crop) still anchor the bright end. Flat images (no meaningful range)
/// pass through unchanged rather than amplifying noise.
fn stretch_contrast(image: &RgbaImage) -> RgbaImage {
    let mut histogram = [0_u32; 256];
    for pixel in image.pixels() {
        let [red, green, blue, _] = pixel.0;
        let luma = (u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000;
        histogram[luma.min(255) as usize] += 1;
    }
    let total = u64::from(image.width()) * u64::from(image.height());
    let low = percentile_level(&histogram, total, 1);
    let high = percentile_level(&histogram, total, 99);
    if high <= low || u16::from(high) - u16::from(low) < 24 {
        return image.clone();
    }
    let range = f32::from(high) - f32::from(low);
    let mut table = [0_u8; 256];
    for (level, slot) in table.iter_mut().enumerate() {
        let level = u16::try_from(level).expect("luma table has 256 levels");
        let scaled = ((f32::from(level) - f32::from(low)) / range * 255.0).clamp(0.0, 255.0);
        *slot = quantize_level(scaled);
    }
    RgbaImage::from_fn(image.width(), image.height(), |x, y| {
        let [red, green, blue, alpha] = image.get_pixel(x, y).0;
        image::Rgba([
            table[red as usize],
            table[green as usize],
            table[blue as usize],
            alpha,
        ])
    })
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0.0..=255.0 immediately before conversion"
)]
fn quantize_level(value: f32) -> u8 {
    value.round() as u8
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "histogram levels are 0..=255 and pixel counts are bounded by the capture budget"
)]
fn percentile_level(histogram: &[u32; 256], total: u64, percentile: u64) -> u8 {
    let threshold = total * percentile / 100;
    let mut seen = 0_u64;
    for (level, count) in histogram.iter().enumerate() {
        seen += u64::from(*count);
        if seen > threshold {
            return level as u8;
        }
    }
    255
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
        let prepared = preprocess_for_ocr(image);
        let _apartment = WinRtApartment::initialize()?;
        let engine = self.engine()?;
        let maximum_dimension = windows::Media::Ocr::OcrEngine::MaxImageDimension()
            .map_err(|error| OcrError::Backend(error.to_string()))?;
        if prepared.width() > maximum_dimension || prepared.height() > maximum_dimension {
            return Err(OcrError::InvalidRequest(format!(
                "OCR crop {}x{} exceeds Windows maximum dimension {maximum_dimension}",
                prepared.width(),
                prepared.height()
            )));
        }
        let bitmap = software_bitmap_from_rgba(&prepared)?;
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
        || pixels > MAX_OCR_PIXELS
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

/// Windows.Media.Ocr does not expose a per-word confidence score, so scores
/// are derived from how uniquely the reading fits the fixed vocabulary:
///
/// 1. exact unique normalized match — 0.92;
/// 2. exactly one vocabulary entry contained whole in the reading (the OCR
///    crop often includes surrounding UI text) — 0.88;
/// 3. a unique near match within the same edit-distance and margin bounds the
///    quest-title matcher uses — 0.78..=0.90 by similarity;
/// 4. anything ambiguous or distant — 0.5, safely below the 0.75 action
///    threshold, which downstream consensus keeps `Uncertain`.
fn vocabulary_confidence(text: &str, vocabulary: &[String]) -> f32 {
    let observed = normalize_ocr_text(text);
    if observed.is_empty() {
        return 0.0;
    }
    let normalized: Vec<String> = vocabulary
        .iter()
        .map(|candidate| normalize_ocr_text(candidate))
        .filter(|candidate| !candidate.is_empty())
        .collect();
    let exact = normalized
        .iter()
        .filter(|candidate| **candidate == observed)
        .count();
    if exact == 1 {
        return 0.92;
    }
    if exact > 1 {
        return 0.5;
    }
    let contained = normalized
        .iter()
        .filter(|candidate| contains_whole_words(&observed, candidate))
        .count();
    if contained == 1 {
        return 0.88;
    }
    let (best, runner_up) = normalized
        .iter()
        .fold((0.0_f32, 0.0_f32), |scores, candidate| {
            let similarity = title_similarity(&observed, candidate);
            if similarity > scores.0 {
                (similarity, scores.0)
            } else {
                (scores.0, scores.1.max(similarity))
            }
        });
    // The 0.82 similarity floor and 0.08 uniqueness margin mirror the
    // constrained quest-title matcher in nectarpilot-core.
    if best >= 0.82 && best - runner_up >= 0.08 {
        0.78 + (best - 0.82) / (1.0 - 0.82) * 0.12
    } else {
        0.5
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "vocabulary entries are bounded to 128 characters, exactly representable by f32"
)]
fn title_similarity(observed: &str, candidate: &str) -> f32 {
    let width = observed
        .chars()
        .count()
        .max(candidate.chars().count())
        .max(1);
    let distance = nectarpilot_core::quests::edit_distance(observed, candidate);
    (1.0 - distance as f32 / width as f32).max(0.0)
}

/// True when `candidate` appears in `observed` on word boundaries, e.g. the
/// quest banner "science bear preliminary research 0 500" contains the
/// vocabulary entry "preliminary research".
fn contains_whole_words(observed: &str, candidate: &str) -> bool {
    if candidate.is_empty() {
        return false;
    }
    let observed_words: Vec<&str> = observed.split(' ').collect();
    let candidate_words: Vec<&str> = candidate.split(' ').collect();
    observed_words
        .windows(candidate_words.len())
        .any(|window| window == candidate_words.as_slice())
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

/// Completion state of one quest-log objective bar, classified from the two
/// exact bar colors the legacy macro keyed on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuestBarState {
    Complete,
    Incomplete,
}

const QUEST_BAR_COMPLETE_RGB: [u8; 3] = [0x6E, 0xFF, 0x60];
const QUEST_BAR_INCOMPLETE_RGB: [u8; 3] = [0xF4, 0x6C, 0x55];
const QUEST_BAR_COLOR_TOLERANCE: u8 = 18;
const QUEST_BAR_MINIMUM_ROWS: u32 = 3;

/// Reads objective completion bars from a quest-log crop, top to bottom.
/// A row belongs to a bar when at least 30% of its pixels match one of the
/// two legacy bar colors; at least three consecutive matching rows form a
/// bar. Anything else is simply not reported — never guessed.
#[must_use]
pub fn read_quest_bars(image: &RgbaImage) -> Vec<QuestBarState> {
    if image.width() < 20 || image.height() < QUEST_BAR_MINIMUM_ROWS {
        return Vec::new();
    }
    let row_threshold = image.width() * 3 / 10;
    let mut bars = Vec::new();
    let mut current: Option<(QuestBarState, u32)> = None;
    for y in 0..image.height() {
        let mut complete = 0_u32;
        let mut incomplete = 0_u32;
        for x in 0..image.width() {
            let [red, green, blue, _] = image.get_pixel(x, y).0;
            if color_close([red, green, blue], QUEST_BAR_COMPLETE_RGB) {
                complete += 1;
            } else if color_close([red, green, blue], QUEST_BAR_INCOMPLETE_RGB) {
                incomplete += 1;
            }
        }
        let row_state = if complete >= row_threshold {
            Some(QuestBarState::Complete)
        } else if incomplete >= row_threshold {
            Some(QuestBarState::Incomplete)
        } else {
            None
        };
        current = match (current, row_state) {
            (Some((state, rows)), Some(row)) if state == row => Some((state, rows + 1)),
            (Some((state, rows)), transition) => {
                if rows >= QUEST_BAR_MINIMUM_ROWS {
                    bars.push(state);
                }
                transition.map(|row| (row, 1))
            }
            (None, Some(row)) => Some((row, 1)),
            (None, None) => None,
        };
    }
    if let Some((state, rows)) = current
        && rows >= QUEST_BAR_MINIMUM_ROWS
    {
        bars.push(state);
    }
    bars
}

fn color_close(observed: [u8; 3], expected: [u8; 3]) -> bool {
    observed
        .iter()
        .zip(expected.iter())
        .all(|(a, b)| a.abs_diff(*b) <= QUEST_BAR_COLOR_TOLERANCE)
}

/// Loads one reviewed template image from raw PNG bytes, downscaling large
/// icons until they fit the bounded template budget.
pub fn template_from_png_bytes(
    id: impl Into<String>,
    bytes: &[u8],
) -> Result<Template, PerceptionError> {
    let decoded = image::load_from_memory(bytes)
        .map_err(|error| PerceptionError::InvalidConfiguration(error.to_string()))?;
    let mut gray = decoded.to_luma8();
    while u64::from(gray.width()) * u64::from(gray.height()) > MAX_TEMPLATE_PIXELS
        && gray.width() > 2
        && gray.height() > 2
    {
        gray = image::imageops::resize(
            &gray,
            (gray.width() / 2).max(2),
            (gray.height() / 2).max(2),
            FilterType::Triangle,
        );
    }
    Template::new(id, gray)
}

/// The quest-log giver icon templates imported with the legacy macro, bound to
/// their typed givers. `assets_root` is the `nm_image_assets` directory; every
/// listed file is cataloged in `assets/detectors/_legacy-manifest.yaml`.
pub fn quest_giver_bindings(
    assets_root: &std::path::Path,
) -> Result<Vec<TemplateBinding<QuestGiver>>, PerceptionError> {
    const ICONS: [(QuestGiver, &[&str]); 5] = [
        (
            QuestGiver::PolarBear,
            &["polar_bear.png", "polar_bear2.png", "polar_bear3.png"],
        ),
        (
            QuestGiver::BlackBear,
            &[
                "black_bear.png",
                "black_bear2.png",
                "black_bear3.png",
                "black_bear4.png",
                "black_bear5.png",
                "black_bear6.png",
            ],
        ),
        (
            QuestGiver::BrownBear,
            &[
                "brown_bear1.png",
                "brown_bear2.png",
                "brown_bear3.png",
                "brown_bear4.png",
                "brown_bear5.png",
            ],
        ),
        (QuestGiver::GiftedBuckoBee, &["bucko.png", "bucko2.png"]),
        (QuestGiver::GiftedRileyBee, &["riley.png", "riley2.png"]),
    ];
    // The quest log occupies the left third of the client; matching outside it
    // would let unrelated screen content impersonate a quest giver.
    let search_region = NormalizedRegion {
        x: 0.0,
        y: 0.05,
        width: 0.4,
        height: 0.9,
    };
    let mut bindings = Vec::new();
    for (giver, files) in ICONS {
        for file in files {
            let path = assets_root.join(file);
            let bytes = std::fs::read(&path).map_err(|error| {
                PerceptionError::InvalidConfiguration(format!(
                    "quest giver template {} is unreadable: {error}",
                    path.display()
                ))
            })?;
            bindings.push(TemplateBinding {
                template: template_from_png_bytes(format!("giver-{file}"), &bytes)?,
                value: giver,
                search_region,
            });
        }
    }
    Ok(bindings)
}

/// HUD honey-counter reader using the same strategy the legacy `StatMonitor`
/// used: OCR several rescaled variants of the counter crop, normalize the
/// classic digit confusions (`o`→0, `i`/`l`→1, `a`→4), and accept only a
/// value that at least two variants agree on.
pub struct HoneyCounterReader<O> {
    ocr: O,
    digits_vocabulary: Vec<String>,
}

const HONEY_VARIANT_SIZES: [(u32, u32); 5] =
    [(250, 48), (350, 64), (450, 80), (550, 96), (650, 112)];
const HONEY_MAXIMUM_VALUE: u64 = 1_000_000_000_000_000;

impl<O> HoneyCounterReader<O>
where
    O: ConstrainedOcr,
{
    pub fn new(ocr: O) -> Self {
        Self {
            ocr,
            digits_vocabulary: (0..=9).map(|digit| digit.to_string()).collect(),
        }
    }

    /// Reads the current honey value from a full client frame. The counter
    /// sits at a fixed offset left of the client's horizontal center, exactly
    /// where the legacy macro sampled it.
    #[must_use]
    pub fn read(&mut self, frame: &ClientFrame) -> Detection<u64> {
        let image = frame.image();
        let base_evidence = evidence("honey_counter", None, Vec::new());
        if image.width() < 980 || image.height() < 500 {
            return Detection::Uncertain {
                reason: "client is too small for the honey counter layout".to_owned(),
                evidence: base_evidence,
            };
        }
        let crop_x = image.width() / 2 - 241;
        let crop = image::imageops::crop_imm(image, crop_x, 0, 140, 44).to_image();
        let prepared = preprocess_for_ocr(&crop);

        let mut votes: std::collections::BTreeMap<u64, u32> = std::collections::BTreeMap::new();
        let mut errors = 0_usize;
        for (width, height) in HONEY_VARIANT_SIZES {
            let variant = image::imageops::resize(&prepared, width, height, FilterType::CatmullRom);
            let read = self.ocr.recognize(
                &variant,
                OcrRequest {
                    detector: "honey_counter",
                    vocabulary: &self.digits_vocabulary,
                    maximum_characters: 24,
                },
            );
            match read {
                Ok(read) => {
                    if let Some(value) = normalize_honey_digits(&read.text) {
                        *votes.entry(value).or_insert(0) += 1;
                    }
                }
                Err(_) => errors += 1,
            }
        }
        if errors == HONEY_VARIANT_SIZES.len() {
            return Detection::Error {
                code: "ocr_failed".to_owned(),
                message: "every honey counter OCR variant failed".to_owned(),
                evidence: Some(base_evidence),
            };
        }
        let agreeing: Vec<(u64, u32)> = votes
            .iter()
            .filter(|(_, count)| **count >= 2)
            .map(|(value, count)| (*value, *count))
            .collect();
        match agreeing.as_slice() {
            [] => Detection::Uncertain {
                reason: "no two honey counter variants agreed".to_owned(),
                evidence: base_evidence,
            },
            [(value, count)] => {
                let mut evidence = base_evidence;
                evidence.notes.push(format!(
                    "{count} of {} variants agree",
                    HONEY_VARIANT_SIZES.len()
                ));
                let agreement = f32::from(u8::try_from(*count).unwrap_or(u8::MAX));
                let total = f32::from(
                    u8::try_from(HONEY_VARIANT_SIZES.len()).expect("five fixed variants"),
                );
                Detection::Found {
                    value: *value,
                    confidence: agreement / total,
                    evidence,
                }
            }
            _ => Detection::Uncertain {
                reason: "honey counter variants agreed on conflicting values".to_owned(),
                evidence: base_evidence,
            },
        }
    }
}

/// Applies the legacy digit-confusion substitutions and bounds the result.
fn normalize_honey_digits(text: &str) -> Option<u64> {
    let digits: String = text
        .to_ascii_lowercase()
        .chars()
        .filter_map(|character| match character {
            'o' => Some('0'),
            'i' | 'l' => Some('1'),
            'a' => Some('4'),
            digit if digit.is_ascii_digit() => Some(digit),
            _ => None,
        })
        .collect();
    if digits.is_empty() || digits.len() > 16 {
        return None;
    }
    let value = digits.parse::<u64>().ok()?;
    (1..=HONEY_MAXIMUM_VALUE).contains(&value).then_some(value)
}

/// OCR-backed quest-title detector for one cataloged giver. The recognizer is
/// constrained to that giver's checked-in catalog, then the catalog matcher
/// and temporal gate independently reject ambiguous or one-frame readings.
/// Bucko and Riley share several quest names, so the giver must come from an
/// independent signal (for example the quest-log icon templates) rather than
/// from the title text itself.
pub struct QuestTitleDetector<O> {
    ocr: O,
    giver: QuestGiver,
    region: NormalizedRegion,
    vocabulary: Vec<String>,
    consensus: TemporalConsensus<QuestCandidate>,
}

/// Backward-compatible name for the Science Bear configuration.
pub type ScienceBearQuestDetector<O> = QuestTitleDetector<O>;

impl<O> QuestTitleDetector<O>
where
    O: ConstrainedOcr,
{
    /// Science Bear detector, the original v1 configuration.
    pub fn new(
        ocr: O,
        region: NormalizedRegion,
        consensus: ConsensusPolicy,
    ) -> Result<Self, PerceptionError> {
        Self::for_giver(ocr, QuestGiver::ScienceBear, region, consensus)
    }

    /// Title detector constrained to `giver`'s checked-in catalog.
    pub fn for_giver(
        ocr: O,
        giver: QuestGiver,
        region: NormalizedRegion,
        consensus: ConsensusPolicy,
    ) -> Result<Self, PerceptionError> {
        if !region.is_valid() {
            return Err(PerceptionError::InvalidConfiguration(
                "quest OCR region must be a normalized client crop".to_owned(),
            ));
        }
        let Some(catalog) = quest_catalog_for(giver) else {
            return Err(PerceptionError::InvalidConfiguration(format!(
                "no checked-in quest catalog exists for {giver:?}"
            )));
        };
        let vocabulary = catalog
            .quests
            .into_iter()
            .map(|quest| quest.name)
            .collect::<Vec<_>>();
        if vocabulary.is_empty() || vocabulary.len() > MAX_VOCABULARY_ENTRIES {
            return Err(PerceptionError::InvalidConfiguration(format!(
                "{giver:?} vocabulary is outside safe bounds"
            )));
        }
        Ok(Self {
            ocr,
            giver,
            region,
            vocabulary,
            consensus: TemporalConsensus::new(consensus)?,
        })
    }

    #[must_use]
    pub fn detect(&mut self, frame: &ClientFrame) -> Detection<QuestCandidate> {
        let detector_name = quest_title_detector_name(self.giver);
        let crop = match frame.crop(self.region) {
            Ok(crop) => crop,
            Err(error) => return self.error_detection("crop_failed", error.to_string()),
        };
        let read = match self.ocr.recognize(
            &crop.image,
            OcrRequest {
                detector: detector_name,
                vocabulary: &self.vocabulary,
                maximum_characters: MAX_OCR_CHARACTERS,
            },
        ) {
            Ok(read) => read,
            Err(error) => return self.error_detection("ocr_failed", error.to_string()),
        };
        let base_evidence = evidence(
            detector_name,
            Some(self.region),
            vec![format!(
                "OCR result was constrained to the {}-title {:?} catalog",
                self.vocabulary.len(),
                self.giver
            )],
        );
        if read.text.chars().count() > MAX_OCR_CHARACTERS {
            return self.consensus.observe(Detection::Uncertain {
                reason: "OCR output exceeded the bounded title length".to_owned(),
                evidence: base_evidence,
            });
        }
        let matched = detect_quest_title(self.giver, &read.text, read.confidence);
        let mapped = match matched {
            Detection::Found {
                value, confidence, ..
            } => Detection::Found {
                value: QuestCandidate {
                    giver: self.giver,
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
                quest_title_detector_name(self.giver),
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
    fn fuzzy_vocabulary_confidence_recovers_near_and_embedded_titles() {
        let vocabulary: Vec<String> = [
            "Preliminary Research",
            "Bee Observation",
            "Applied Science",
            "Pollen Focus",
        ]
        .iter()
        .map(|title| (*title).to_owned())
        .collect();

        // Exact reading keeps the historical high score.
        assert!(super::vocabulary_confidence("Preliminary Research", &vocabulary) > 0.9);
        // One misread character must stay above the 0.75 action threshold.
        let near = super::vocabulary_confidence("Prelimlnary Researcb", &vocabulary);
        assert!((0.75..0.92).contains(&near), "near match scored {near}");
        // A banner reading with surrounding UI text still identifies the title.
        let embedded =
            super::vocabulary_confidence("Science Bear Preliminary Research 0 / 500", &vocabulary);
        assert!(embedded > 0.8, "embedded match scored {embedded}");
        // Garbage stays below the action threshold.
        assert!(super::vocabulary_confidence("Wxyz Qrst", &vocabulary) <= 0.5);
    }

    #[test]
    fn ocr_preprocessing_upscales_small_crops_within_budget() {
        let mut small = RgbaImage::from_pixel(120, 18, Rgba([90, 90, 90, 255]));
        for x in 10..40 {
            small.put_pixel(x, 9, Rgba([140, 140, 140, 255]));
        }
        let prepared = super::preprocess_for_ocr(&small);
        assert_eq!(prepared.height(), 18 * 4, "small text should upscale 4x");
        assert_eq!(prepared.width(), 120 * 4);

        // Contrast must widen: the darkest/brightest text levels move apart.
        let luma = |image: &RgbaImage, x: u32, y: u32| i32::from(image.get_pixel(x, y).0[0]);
        let original_range = (luma(&small, 20, 9) - luma(&small, 5, 5)).abs();
        let prepared_range = (luma(&prepared, 80, 38) - luma(&prepared, 20, 20)).abs();
        assert!(
            prepared_range > original_range,
            "contrast stretch must widen the text/background separation ({original_range} -> {prepared_range})"
        );

        // Large crops are not upscaled beyond the pixel budget.
        let large = RgbaImage::from_pixel(1600, 90, Rgba([10, 10, 10, 255]));
        let prepared_large = super::preprocess_for_ocr(&large);
        assert!(
            u64::from(prepared_large.width()) * u64::from(prepared_large.height())
                <= super::MAX_OCR_PIXELS
        );
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
