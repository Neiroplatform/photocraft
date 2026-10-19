//! Candle CPU/WASM backend for the immutable MobileSAM TinyViT candidate.

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::segment_anything::sam::Sam;

use crate::{MOBILE_SAM_BYTES, MOBILE_SAM_SHA256, MlError, ModelIdentity, PointKind, SegmentationBackend, SegmentationMask, SegmentationRequest};

pub struct MobileSamBackend {
    model: Sam,
    device: Device,
    identity: ModelIdentity,
}

impl std::fmt::Debug for MobileSamBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileSamBackend").field("identity", &self.identity).finish_non_exhaustive()
    }
}

impl MobileSamBackend {
    /// Load only the exact candidate weights. Verification occurs before the
    /// safetensors parser sees untrusted bytes.
    pub fn from_safetensors(weights: Vec<u8>) -> Result<Self, MlError> {
        if weights.len() != MOBILE_SAM_BYTES {
            return Err(MlError::WeightMismatch(format!("byte length is {}, expected {MOBILE_SAM_BYTES}", weights.len())));
        }
        let digest = sha256_hex(&weights);
        if digest != MOBILE_SAM_SHA256 {
            return Err(MlError::WeightMismatch(format!("SHA-256 is {digest}, expected {MOBILE_SAM_SHA256}")));
        }
        let device = Device::Cpu;
        let vb = VarBuilder::from_buffered_safetensors(weights, DType::F32, &device).map_err(|e| MlError::Backend(e.to_string()))?;
        let model = Sam::new_tiny(vb).map_err(|e| MlError::Backend(e.to_string()))?;
        Ok(Self { model, device, identity: ModelIdentity::mobile_sam_tiny() })
    }
}

impl SegmentationBackend for MobileSamBackend {
    fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    fn segment(&mut self, request: SegmentationRequest<'_>) -> Result<SegmentationMask, MlError> {
        request.validate()?;
        let h = request.height as usize;
        let w = request.width as usize;
        let image = Tensor::from_slice(request.rgb, (h, w, 3), &self.device).and_then(|t| t.permute((2, 0, 1))).map_err(|e| MlError::Backend(e.to_string()))?;
        let points = request.points.iter().map(|p| (p.x as f64, p.y as f64, p.kind == PointKind::Foreground)).collect::<Vec<_>>();
        // A single positive point is deliberately ambiguous: it can describe a
        // small part (a leaf), a larger part (the crown) or the complete object
        // (the potted plant). SAM's multi-mask mode returns those hypotheses
        // together with its predicted IoU for each one. Keeping only the first
        // mask made object clicks collapse to parts; choose the model's
        // highest-confidence hypothesis instead, as the reference SAM clients
        // do for an ambiguous point prompt.
        let multimask = request.points.len() == 1;
        let (mask, iou) = self.model.forward(&image, &points, multimask).map_err(|e| MlError::Backend(e.to_string()))?;
        let scores = iou.flatten_all().and_then(|t| t.to_vec1::<f32>()).map_err(|e| MlError::Backend(e.to_string()))?;
        let best = scores
            .iter()
            .enumerate()
            .filter(|(_, score)| score.is_finite())
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(index, _)| index)
            .unwrap_or(0);
        let logits = mask.i(best).and_then(|m| m.to_vec2::<f32>()).map_err(|e| MlError::Backend(e.to_string()))?;
        let mut alpha = Vec::with_capacity(w * h);
        for row in logits {
            alpha.extend(row.into_iter().map(|v| if v > 0.0 { 255 } else { 0 }));
        }
        crate::postprocess::clean_clicked_object(request.width, request.height, &mut alpha, request.points);
        let confidence = scores.get(best).copied().map(|value| value.clamp(0.0, 1.0));
        let proposal = SegmentationMask { width: request.width, height: request.height, alpha, confidence, model: self.identity.clone() };
        proposal.validate()?;
        Ok(proposal)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_length_before_parsing() {
        let result = MobileSamBackend::from_safetensors(vec![0; 32]);
        assert!(result.is_err(), "short weights must fail");
        let Err(error) = result else { return };
        assert!(matches!(error, MlError::WeightMismatch(_)));
    }

    #[test]
    fn sha256_implementation_matches_known_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn candle_image_size_matches_request_budget() {
        assert_eq!(candle_transformers::models::segment_anything::sam::IMAGE_SIZE, crate::MAX_PREVIEW_SIDE as usize);
    }
}
