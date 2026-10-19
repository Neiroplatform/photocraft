//! Bounded local-inference contracts for PhotoCraft.
//!
//! This L4 crate knows nothing about egui, browser APIs, documents or history.
//! Callers provide a bounded RGB preview and normalized prompt points. Backends
//! return an untrusted mask proposal; only the engine may turn one into a
//! canonical selection.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::{Deserialize, Serialize};

#[cfg(any(feature = "mobile-sam", test))]
mod postprocess;

/// Official MobileSAM source/checkpoint identity used by the release provenance
/// gate. The Candle safetensors below are verified tensor-for-tensor against
/// this exact checkpoint before packaging.
pub const MOBILE_SAM_REPOSITORY_REVISION: &str = "f706ad9c4eb7f219c00d9050e46328518ffb65d2";
pub const MOBILE_SAM_OFFICIAL_PT_SHA256: &str = "6dbb90523a35330fedd7f1d3dfc66f995213d81b29a5ca8108dbcdd4e37d6c2f";
/// Immutable conversion mirror revision. It has no independent model card, so
/// it is never accepted without the official-checkpoint equivalence gate.
pub const MOBILE_SAM_CONVERSION_REVISION: &str = "8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97";
pub const MOBILE_SAM_FILE: &str = "mobile_sam-tiny-vitt.safetensors";
pub const MOBILE_SAM_BYTES: usize = 40_613_024;
pub const MOBILE_SAM_SHA256: &str = "5ad43a841b57cc798e2837e251c15aa9f602868a1780d195459383d117784cf7";

/// The largest side accepted by the MobileSAM encoder.
pub const MAX_PREVIEW_SIDE: u32 = 1024;
/// Keep requests bounded independently of integer overflow and backend details.
pub const MAX_PREVIEW_PIXELS: usize = (MAX_PREVIEW_SIDE as usize) * (MAX_PREVIEW_SIDE as usize);
/// Enough for iterative refinement without making prompt cost unbounded.
pub const MAX_PROMPT_POINTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PointKind {
    Foreground,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PromptPoint {
    /// Normalized horizontal coordinate in `0..=1`.
    pub x: f32,
    /// Normalized vertical coordinate in `0..=1`.
    pub y: f32,
    pub kind: PointKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub family: String,
    pub revision: String,
    pub weights_sha256: String,
}

impl ModelIdentity {
    pub fn mobile_sam_tiny() -> Self {
        Self { family: "mobile-sam-tinyvit-candle".into(), revision: MOBILE_SAM_REPOSITORY_REVISION.into(), weights_sha256: MOBILE_SAM_SHA256.into() }
    }
}

/// Borrowed request so the inference boundary does not add a second full-image
/// copy before a backend explicitly creates its tensor.
#[derive(Debug, Clone, Copy)]
pub struct SegmentationRequest<'a> {
    pub width: u32,
    pub height: u32,
    /// Packed row-major RGB8, exactly `width * height * 3` bytes.
    pub rgb: &'a [u8],
    pub points: &'a [PromptPoint],
}

/// Owned form used at asynchronous native/browser worker boundaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentationInput {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub points: Vec<PromptPoint>,
}

impl SegmentationInput {
    pub fn borrowed(&self) -> SegmentationRequest<'_> {
        SegmentationRequest { width: self.width, height: self.height, rgb: &self.rgb, points: &self.points }
    }
}

/// Versioned messages shared by the UI shell and the isolated inference worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub version: u8,
    pub id: u64,
    pub input: SegmentationInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerResponse {
    pub version: u8,
    pub id: u64,
    pub result: Result<SegmentationMask, String>,
}

pub const WORKER_PROTOCOL_VERSION: u8 = 1;

impl SegmentationRequest<'_> {
    pub fn validate(&self) -> Result<(), MlError> {
        if self.width == 0 || self.height == 0 {
            return Err(MlError::InvalidRequest("preview dimensions must be non-zero".into()));
        }
        if self.width > MAX_PREVIEW_SIDE || self.height > MAX_PREVIEW_SIDE {
            return Err(MlError::InvalidRequest(format!("preview is {}x{}; each side is limited to {MAX_PREVIEW_SIDE}", self.width, self.height)));
        }
        let pixels = (self.width as usize).checked_mul(self.height as usize).ok_or_else(|| MlError::InvalidRequest("preview dimensions overflow".into()))?;
        if pixels > MAX_PREVIEW_PIXELS {
            return Err(MlError::InvalidRequest("preview pixel budget exceeded".into()));
        }
        let expected = pixels.checked_mul(3).ok_or_else(|| MlError::InvalidRequest("preview byte length overflows".into()))?;
        if self.rgb.len() != expected {
            return Err(MlError::InvalidRequest(format!("RGB byte length is {}, expected {expected}", self.rgb.len())));
        }
        if self.points.is_empty() {
            return Err(MlError::InvalidRequest("at least one prompt point is required".into()));
        }
        if self.points.len() > MAX_PROMPT_POINTS {
            return Err(MlError::InvalidRequest(format!("{} prompt points exceed the limit of {MAX_PROMPT_POINTS}", self.points.len())));
        }
        if self.points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite() || !(0.0..=1.0).contains(&p.x) || !(0.0..=1.0).contains(&p.y)) {
            return Err(MlError::InvalidRequest("prompt coordinates must be finite and normalized".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentationMask {
    pub width: u32,
    pub height: u32,
    /// Row-major binary mask (`0` background, `255` foreground).
    pub alpha: Vec<u8>,
    /// Model IOU estimate when supplied by the backend.
    pub confidence: Option<f32>,
    pub model: ModelIdentity,
}

impl SegmentationMask {
    pub fn validate(&self) -> Result<(), MlError> {
        let expected = (self.width as usize).checked_mul(self.height as usize).ok_or_else(|| MlError::Backend("mask dimensions overflow".into()))?;
        if self.alpha.len() != expected {
            return Err(MlError::Backend(format!("mask byte length is {}, expected {expected}", self.alpha.len())));
        }
        if self.alpha.iter().any(|a| *a != 0 && *a != 255) {
            return Err(MlError::Backend("mask must be binary".into()));
        }
        if self.confidence.is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v)) {
            return Err(MlError::Backend("confidence must be finite and normalized".into()));
        }
        Ok(())
    }
}

pub trait SegmentationBackend {
    fn identity(&self) -> &ModelIdentity;
    fn segment(&mut self, request: SegmentationRequest<'_>) -> Result<SegmentationMask, MlError>;
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum MlError {
    #[error("invalid segmentation request: {0}")]
    InvalidRequest(String),
    #[error("model weights do not match the immutable candidate: {0}")]
    WeightMismatch(String),
    #[error("local inference failed: {0}")]
    Backend(String),
}

#[cfg(feature = "mobile-sam")]
pub mod mobile_sam;

#[cfg(test)]
mod tests {
    use super::*;

    fn point() -> PromptPoint {
        PromptPoint { x: 0.5, y: 0.5, kind: PointKind::Foreground }
    }

    #[test]
    fn request_accepts_exact_rgb_and_normalized_point() {
        let rgb = [0; 2 * 3 * 3];
        assert!(SegmentationRequest { width: 2, height: 3, rgb: &rgb, points: &[point()] }.validate().is_ok());
    }

    #[test]
    fn request_rejects_empty_oversize_bad_bytes_and_non_finite_points() {
        let p = point();
        let bad = PromptPoint { x: f32::NAN, ..p };
        let cases = [
            SegmentationRequest { width: 0, height: 1, rgb: &[], points: &[p] },
            SegmentationRequest { width: MAX_PREVIEW_SIDE + 1, height: 1, rgb: &[], points: &[p] },
            SegmentationRequest { width: 1, height: 1, rgb: &[0, 0], points: &[p] },
            SegmentationRequest { width: 1, height: 1, rgb: &[0, 0, 0], points: &[bad] },
            SegmentationRequest { width: 1, height: 1, rgb: &[0, 0, 0], points: &[] },
        ];
        for request in cases {
            assert!(matches!(request.validate(), Err(MlError::InvalidRequest(_))));
        }
    }

    #[test]
    fn mask_rejects_wrong_size_non_binary_and_bad_confidence() {
        let model = ModelIdentity::mobile_sam_tiny();
        for mask in [
            SegmentationMask { width: 2, height: 1, alpha: vec![0], confidence: None, model: model.clone() },
            SegmentationMask { width: 1, height: 1, alpha: vec![1], confidence: None, model: model.clone() },
            SegmentationMask { width: 1, height: 1, alpha: vec![255], confidence: Some(2.0), model: model.clone() },
        ] {
            assert!(matches!(mask.validate(), Err(MlError::Backend(_))));
        }
    }

    #[test]
    fn immutable_mobile_sam_identity_is_complete() {
        let id = ModelIdentity::mobile_sam_tiny();
        assert_eq!(id.revision.len(), 40);
        assert_eq!(id.weights_sha256.len(), 64);
        assert_eq!(MOBILE_SAM_CONVERSION_REVISION.len(), 40);
        assert_eq!(MOBILE_SAM_OFFICIAL_PT_SHA256.len(), 64);
        assert_eq!(MOBILE_SAM_BYTES, 40_613_024);
    }
}
