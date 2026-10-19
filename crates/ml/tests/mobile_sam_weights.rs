#![cfg(feature = "mobile-sam")]

use photocraft_ml::mobile_sam::MobileSamBackend;
use photocraft_ml::{MOBILE_SAM_BYTES, PointKind, PromptPoint, SegmentationBackend, SegmentationRequest};

/// Exact-weight smoke is opt-in so ordinary source tests never fetch or bundle
/// the 40.6 MB model. The candidate gate supplies this environment variable.
#[test]
#[ignore = "requires PHOTOCRAFT_MOBILE_SAM_WEIGHTS pointing at the exact immutable model"]
fn exact_weights_load_and_produce_a_bounded_mask() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("PHOTOCRAFT_MOBILE_SAM_WEIGHTS")?;
    let weights = std::fs::read(path)?;
    assert_eq!(weights.len(), MOBILE_SAM_BYTES);
    let mut backend = MobileSamBackend::from_safetensors(weights)?;
    let width = 32;
    let height = 24;
    let rgb = vec![128; width * height * 3];
    let points = [PromptPoint { x: 0.5, y: 0.5, kind: PointKind::Foreground }];
    let mask = backend.segment(SegmentationRequest { width: width as u32, height: height as u32, rgb: &rgb, points: &points })?;
    assert_eq!(mask.alpha.len(), width * height);
    assert!(mask.confidence.is_some());
    Ok(())
}
