//! Candle CPU/WASM backend for the immutable MobileSAM TinyViT candidate.

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::segment_anything::sam::Sam;

use crate::{
    MOBILE_SAM_BYTES, MOBILE_SAM_SHA256, MlError, ModelIdentity, PointKind, PromptPoint, SegmentationBackend, SegmentationMask, SegmentationMode,
    SegmentationRequest,
};

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
        if request.mode == SegmentationMode::AutomaticSubject {
            return self.segment_automatic_subject(&image, request);
        }
        let points = request.points.iter().map(model_point).collect::<Vec<_>>();
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

impl MobileSamBackend {
    /// Automatic background-removal requests carry several independent probes.
    /// Encode the image once, evaluate each probe as its own SAM hypothesis,
    /// reject border-dominant background masks and join related pieces of the
    /// same foreground object. A manual click remains the ordinary one-prompt
    /// path above, so it can intentionally select a nested detail.
    fn segment_automatic_subject(&self, image: &Tensor, request: SegmentationRequest<'_>) -> Result<SegmentationMask, MlError> {
        let (channels, h, w) = image.dims3().map_err(|error| MlError::Backend(error.to_string()))?;
        if channels != 3 {
            return Err(MlError::Backend("automatic subject image must be RGB".into()));
        }
        let embeddings = self.model.embeddings(image).map_err(|error| MlError::Backend(error.to_string()))?;
        let mut candidates = Vec::new();
        for (prompt_index, prompt) in request.points.iter().copied().enumerate() {
            let point = [model_point(&prompt)];
            let (masks, iou) = self.model.forward_for_embeddings(&embeddings, h, w, &point, true).map_err(|error| MlError::Backend(error.to_string()))?;
            let masks = masks
                .upsample_nearest2d(
                    candle_transformers::models::segment_anything::sam::IMAGE_SIZE,
                    candle_transformers::models::segment_anything::sam::IMAGE_SIZE,
                )
                .and_then(|tensor| tensor.get(0))
                .and_then(|tensor| tensor.i((.., ..h, ..w)))
                .map_err(|error| MlError::Backend(error.to_string()))?;
            let scores = iou.flatten_all().and_then(|tensor| tensor.to_vec1::<f32>()).map_err(|error| MlError::Backend(error.to_string()))?;
            let mut prompt_candidates = Vec::new();
            for (index, confidence) in scores.into_iter().enumerate() {
                if !confidence.is_finite() {
                    continue;
                }
                let logits = masks.i(index).and_then(|mask| mask.to_vec2::<f32>()).map_err(|error| MlError::Backend(error.to_string()))?;
                let mut alpha = Vec::with_capacity(w * h);
                for row in logits {
                    alpha.extend(row.into_iter().map(|value| if value > 0.0 { 255 } else { 0 }));
                }
                crate::postprocess::clean_clicked_object(request.width, request.height, &mut alpha, std::slice::from_ref(&prompt));
                let metrics = crate::postprocess::automatic_mask_metrics(request.width, request.height, &alpha, prompt);
                if metrics.viable() {
                    prompt_candidates.push((alpha, metrics, confidence.clamp(0.0, 1.0)));
                }
            }
            if let Some(candidate) = prompt_candidates
                .into_iter()
                .max_by(|left, right| automatic_candidate_score(left.1, left.2).total_cmp(&automatic_candidate_score(right.1, right.2)))
            {
                candidates.push((candidate.0, candidate.1, candidate.2, prompt_index));
            }
        }
        let best = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.3 == 0 && strong_centre_candidate(candidate.1))
            .max_by(|(_, left), (_, right)| automatic_candidate_score(left.1, left.2).total_cmp(&automatic_candidate_score(right.1, right.2)))
            .map(|(index, _)| index)
            .or_else(|| {
                candidates
                    .iter()
                    .enumerate()
                    .max_by(|(_, left), (_, right)| automatic_candidate_score(left.1, left.2).total_cmp(&automatic_candidate_score(right.1, right.2)))
                    .map(|(index, _)| index)
            })
            .ok_or_else(|| MlError::Backend("automatic subject discovery found only background masks".into()))?;
        let (mut alpha, mut selected_metrics, confidence, selected_probe) = candidates.swap_remove(best);
        let mut accepted_probes = vec![selected_probe];
        for (candidate, metrics, _, candidate_probe) in candidates {
            let allow_seam = accepted_probes.iter().any(|probe| vertical_probe_pair(*probe, candidate_probe));
            if crate::postprocess::related_masks(request.width, request.height, &alpha, selected_metrics, &candidate, metrics, allow_seam) {
                for (value, addition) in alpha.iter_mut().zip(candidate) {
                    *value = (*value).max(addition);
                }
                let area = alpha.iter().filter(|value| **value >= 128).count();
                selected_metrics = crate::postprocess::union_metrics(selected_metrics, metrics, area, alpha.len());
                accepted_probes.push(candidate_probe);
            }
        }
        let proposal = SegmentationMask { width: request.width, height: request.height, alpha, confidence: Some(confidence), model: self.identity.clone() };
        proposal.validate()?;
        Ok(proposal)
    }
}

fn automatic_candidate_score(metrics: crate::postprocess::MaskMetrics, confidence: f32) -> f32 {
    let background_penalty = if metrics.area_ratio > 0.55 { 1.0 } else { 0.0 };
    metrics.score() + confidence * 0.05 - background_penalty
}

fn strong_centre_candidate(metrics: crate::postprocess::MaskMetrics) -> bool {
    metrics.viable() && metrics.area_ratio >= 0.1 && metrics.area_ratio <= 0.45 && metrics.border_ratio <= 0.2 && metrics.edges_touched <= 1
}

fn vertical_probe_pair(left: usize, right: usize) -> bool {
    matches!((left.min(right), left.max(right)), (1, 2) | (3, 4))
}

fn model_point(point: &PromptPoint) -> (f64, f64, bool) {
    (point.x as f64, point.y as f64, point.kind == PointKind::Foreground)
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
