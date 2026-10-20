#![cfg(feature = "mobile-sam")]

use photocraft_ml::mobile_sam::MobileSamBackend;
use photocraft_ml::{MOBILE_SAM_BYTES, PointKind, PromptPoint, SegmentationBackend, SegmentationMode, SegmentationRequest};
use std::path::Path;

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
    let mask = backend.segment(SegmentationRequest {
        width: width as u32,
        height: height as u32,
        rgb: &rgb,
        points: &points,
        mode: SegmentationMode::PromptPoints,
    })?;
    assert_eq!(mask.alpha.len(), width * height);
    assert!(mask.confidence.is_some());
    Ok(())
}

/// Opt-in real-image diagnostic used by the browser release gate. It writes
/// each independent probe and the automatic merged result as PNG masks, making
/// a regression inspectable without changing production logging or protocols.
#[test]
#[ignore = "requires exact weights, PHOTOCRAFT_SEGMENTATION_FIXTURE and PHOTOCRAFT_SEGMENTATION_OUTPUT"]
fn external_fixture_writes_automatic_subject_proposals() -> Result<(), Box<dyn std::error::Error>> {
    let weights = std::fs::read(std::env::var("PHOTOCRAFT_MOBILE_SAM_WEIGHTS")?)?;
    let source = image::open(std::env::var("PHOTOCRAFT_SEGMENTATION_FIXTURE")?)?.thumbnail(1024, 1024).to_rgb8();
    let output = std::env::var("PHOTOCRAFT_SEGMENTATION_OUTPUT")?;
    std::fs::create_dir_all(&output)?;
    let (width, height) = source.dimensions();
    let probes = [(0.5, 0.5), (0.32, 0.38), (0.32, 0.55), (0.68, 0.38), (0.68, 0.55)];
    let points = probes.iter().map(|(x, y)| PromptPoint { x: *x, y: *y, kind: PointKind::Foreground }).collect::<Vec<_>>();
    let mut backend = MobileSamBackend::from_safetensors(weights)?;
    if std::env::var_os("PHOTOCRAFT_SEGMENTATION_VERBOSE").is_some() {
        for (index, point) in points.iter().enumerate() {
            let mask = backend.segment(SegmentationRequest {
                width,
                height,
                rgb: source.as_raw(),
                points: std::slice::from_ref(point),
                mode: SegmentationMode::PromptPoints,
            })?;
            image::save_buffer(Path::new(&output).join(format!("probe-{index}.png")), &mask.alpha, width, height, image::ColorType::L8)?;
            eprintln!("probe {index}: {} pixels, confidence {:?}", mask.alpha.iter().filter(|value| **value >= 128).count(), mask.confidence);
            let duplicate = [*point, *point];
            match backend.segment(SegmentationRequest { width, height, rgb: source.as_raw(), points: &duplicate, mode: SegmentationMode::AutomaticSubject }) {
                Ok(automatic_probe) => {
                    image::save_buffer(
                        Path::new(&output).join(format!("automatic-probe-{index}.png")),
                        &automatic_probe.alpha,
                        width,
                        height,
                        image::ColorType::L8,
                    )?;
                    eprintln!(
                        "automatic probe {index}: {} pixels, confidence {:?}",
                        automatic_probe.alpha.iter().filter(|value| **value >= 128).count(),
                        automatic_probe.confidence
                    );
                }
                Err(error) => eprintln!("automatic probe {index}: rejected ({error})"),
            }
        }
    }
    let mask = backend.segment(SegmentationRequest { width, height, rgb: source.as_raw(), points: &points, mode: SegmentationMode::AutomaticSubject })?;
    image::save_buffer(Path::new(&output).join("automatic.png"), &mask.alpha, width, height, image::ColorType::L8)?;
    eprintln!("automatic: {} pixels, confidence {:?}", mask.alpha.iter().filter(|value| **value >= 128).count(), mask.confidence);
    Ok(())
}
