#![cfg(feature = "mobile-sam")]

use candle_core::{DType, Device, pickle, safetensors};

/// Release-only provenance gate. It proves that the browser safetensors are a
/// format conversion of the pinned official MobileSAM checkpoint, rather than
/// merely trusting the filename of a third-party mirror.
#[test]
#[ignore = "requires official .pt and candidate .safetensors paths"]
fn candidate_tensors_match_the_official_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
    let official_path = std::env::var("PHOTOCRAFT_OFFICIAL_MOBILE_SAM_PT")?;
    let candidate_path = std::env::var("PHOTOCRAFT_MOBILE_SAM_WEIGHTS")?;
    let official = pickle::read_all(official_path)?;
    let candidate = safetensors::load(candidate_path, &Device::Cpu)?;
    assert_eq!(official.len(), candidate.len(), "tensor count differs");
    for (name, expected) in official {
        let actual = candidate.get(&name).ok_or_else(|| std::io::Error::other(format!("candidate is missing tensor {name}")))?;
        assert_eq!(actual.dims(), expected.dims(), "shape differs for {name}");
        assert_eq!(actual.dtype(), expected.dtype(), "dtype differs for {name}");
        let expected = expected.flatten_all()?;
        let actual = actual.flatten_all()?;
        match expected.dtype() {
            DType::F32 => assert_eq!(actual.to_vec1::<f32>()?, expected.to_vec1::<f32>()?, "values differ for {name}"),
            DType::I64 => assert_eq!(actual.to_vec1::<i64>()?, expected.to_vec1::<i64>()?, "values differ for {name}"),
            dtype => return Err(std::io::Error::other(format!("provenance gate does not support {dtype:?} tensor {name}")).into()),
        }
    }
    Ok(())
}
