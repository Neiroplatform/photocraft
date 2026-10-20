//! Deterministic CycloneDX source SBOM for the browser release.
//!
//! The generated document is committed and copied into every web package. It
//! intentionally has no timestamp or random serial number: `--check` must
//! produce the same bytes on every checkout of the same source and lockfile.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use serde_json::{Value, json};

use crate::sha256;

pub const OUT: &str = "sbom/photocraft-web-source.cdx.json";
const ROOT_PACKAGE: &str = "photocraft-web";
const TARGET: &str = "wasm32-unknown-unknown";
const FEATURES: &str = "photocraft-web/heif,photocraft-web/ml-worker";
const MODEL_REV: &str = "8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97";
const MODEL_SHA256: &str = "5ad43a841b57cc798e2837e251c15aa9f602868a1780d195459383d117784cf7";
const OFFICIAL_MODEL_REV: &str = "f706ad9c4eb7f219c00d9050e46328518ffb65d2";
const OFFICIAL_MODEL_SHA256: &str = "6dbb90523a35330fedd7f1d3dfc66f995213d81b29a5ca8108dbcdd4e37d6c2f";

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let check = match args {
        [] => false,
        ["--check"] => true,
        _ => return Err("usage: cargo xtask sbom [--check]".into()),
    };
    let generated = generate(root)?;
    let path = root.join(OUT);
    if check {
        let current = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if current == generated.as_bytes() {
            println!("OK: {OUT} is current");
            return Ok(());
        }
        return Err(format!("{OUT} is stale; run `cargo xtask sbom`"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, generated).map_err(|e| format!("write {}: {e}", path.display()))?;
    println!("wrote {OUT}");
    Ok(())
}

fn generate(root: &Path) -> Result<String, String> {
    let metadata = metadata(root)?;
    let packages = metadata["packages"].as_array().ok_or("cargo metadata: packages is not an array")?;
    let nodes = metadata["resolve"]["nodes"].as_array().ok_or("cargo metadata: resolve.nodes is not an array")?;

    let packages_by_id: BTreeMap<&str, &Value> = packages.iter().filter_map(|package| Some((package["id"].as_str()?, package))).collect();
    let nodes_by_id: BTreeMap<&str, &Value> = nodes.iter().filter_map(|node| Some((node["id"].as_str()?, node))).collect();
    let root_id = packages
        .iter()
        .find(|package| package["name"] == ROOT_PACKAGE)
        .and_then(|package| package["id"].as_str())
        .ok_or("cargo metadata: photocraft-web package is missing")?;
    let reachable = reachable_ids(root_id, &nodes_by_id)?;
    let refs = component_refs(&reachable, &packages_by_id)?;

    let mut components = Vec::new();
    for id in &reachable {
        if id == root_id {
            continue;
        }
        components.push(component(packages_by_id.get(id.as_str()).ok_or_else(|| format!("cargo metadata: package {id} is missing"))?, &refs, &nodes_by_id)?);
    }
    components.sort_by(|a, b| a["bom-ref"].as_str().cmp(&b["bom-ref"].as_str()));
    components.push(model_component());

    let mut dependencies = Vec::new();
    for id in &reachable {
        let node = nodes_by_id.get(id.as_str()).ok_or_else(|| format!("cargo metadata: node {id} is missing"))?;
        let mut depends_on: Vec<String> = node["deps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|dep| dep["pkg"].as_str())
            .filter(|dep| reachable.contains(*dep))
            .filter_map(|dep| refs.get(dep).cloned())
            .collect();
        if id == root_id {
            depends_on.push("model:mobile-sam-tiny-vitt@8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97".into());
        }
        depends_on.sort();
        depends_on.dedup();
        dependencies.push(json!({"ref": refs[id], "dependsOn": depends_on}));
    }
    dependencies.push(json!({
        "ref": "model:mobile-sam-tiny-vitt@8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97",
        "dependsOn": []
    }));
    dependencies.sort_by(|a, b| a["ref"].as_str().cmp(&b["ref"].as_str()));

    let lock = std::fs::read(root.join("Cargo.lock")).map_err(|e| format!("read Cargo.lock: {e}"))?;
    let root_package = packages_by_id[root_id];
    let document = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": component(root_package, &refs, &nodes_by_id)?.as_object().map(|value| {
                let mut value = value.clone();
                value.insert("type".into(), Value::String("application".into()));
                Value::Object(value)
            }).ok_or("root component is not an object")?,
            "properties": [
                {"name": "photocraft:cargo-lock-sha256", "value": sha256::hex(&lock)},
                {"name": "photocraft:target", "value": TARGET},
                {"name": "photocraft:features", "value": FEATURES},
                {"name": "photocraft:reproducible", "value": "true"}
            ]
        },
        "components": components,
        "dependencies": dependencies
    });
    let mut output = serde_json::to_string_pretty(&document).map_err(|e| format!("serialize SBOM: {e}"))?;
    output.push('\n');
    Ok(output)
}

fn metadata(root: &Path) -> Result<Value, String> {
    let output = crate::cargo()
        .current_dir(root)
        .args(["metadata", "--locked", "--format-version", "1", "--filter-platform", TARGET, "--features", FEATURES])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&output.stderr)));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("cargo metadata: bad JSON: {e}"))
}

fn reachable_ids(root_id: &str, nodes: &BTreeMap<&str, &Value>) -> Result<BTreeSet<String>, String> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([root_id.to_owned()]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let node = nodes.get(id.as_str()).ok_or_else(|| format!("cargo metadata: node {id} is missing"))?;
        for dep in node["deps"].as_array().into_iter().flatten().filter_map(|dep| dep["pkg"].as_str()) {
            queue.push_back(dep.to_owned());
        }
    }
    Ok(seen)
}

fn component_refs(ids: &BTreeSet<String>, packages: &BTreeMap<&str, &Value>) -> Result<BTreeMap<String, String>, String> {
    let mut base_counts = BTreeMap::<String, usize>::new();
    for id in ids {
        let package = packages.get(id.as_str()).ok_or_else(|| format!("cargo metadata: package {id} is missing"))?;
        let base = format!("pkg:cargo/{}@{}", required_str(package, "name")?, required_str(package, "version")?);
        *base_counts.entry(base).or_default() += 1;
    }
    let mut refs = BTreeMap::new();
    for id in ids {
        let package = packages[id.as_str()];
        let base = format!("pkg:cargo/{}@{}", required_str(package, "name")?, required_str(package, "version")?);
        let reference = if base_counts[&base] == 1 {
            base
        } else {
            let source = package["source"].as_str().unwrap_or("workspace");
            format!("{base}#{}", &sha256::hex(source.as_bytes())[..12])
        };
        refs.insert(id.clone(), reference);
    }
    Ok(refs)
}

fn component(package: &Value, refs: &BTreeMap<String, String>, nodes: &BTreeMap<&str, &Value>) -> Result<Value, String> {
    let id = required_str(package, "id")?;
    let name = required_str(package, "name")?;
    let version = required_str(package, "version")?;
    let mut value = serde_json::Map::new();
    value.insert("type".into(), Value::String("library".into()));
    value.insert("bom-ref".into(), Value::String(refs.get(id).ok_or_else(|| format!("component ref for {id} is missing"))?.clone()));
    value.insert("name".into(), Value::String(name.into()));
    value.insert("version".into(), Value::String(version.into()));
    value.insert("purl".into(), Value::String(format!("pkg:cargo/{name}@{version}")));
    if let Some(license) = package["license"].as_str().filter(|license| !license.is_empty()) {
        value.insert("licenses".into(), json!([{"expression": license}]));
    }
    let mut properties = Vec::new();
    if let Some(source) = package["source"].as_str() {
        properties.push(json!({"name": "cargo:source", "value": source}));
    } else {
        properties.push(json!({"name": "cargo:source", "value": "workspace"}));
    }
    if let Some(node) = nodes.get(id)
        && let Some(features) = node["features"].as_array()
    {
        let mut features: Vec<&str> = features.iter().filter_map(Value::as_str).collect();
        features.sort_unstable();
        if !features.is_empty() {
            properties.push(json!({"name": "cargo:features", "value": features.join(",")}));
        }
    }
    value.insert("properties".into(), Value::Array(properties));
    Ok(Value::Object(value))
}

fn model_component() -> Value {
    json!({
        "type": "machine-learning-model",
        "bom-ref": "model:mobile-sam-tiny-vitt@8b4cb7c743f3b3cb8afd212a86ae15b1bbfdac97",
        "name": "MobileSAM TinyViT",
        "version": MODEL_REV,
        "hashes": [{"alg": "SHA-256", "content": MODEL_SHA256}],
        "licenses": [{"license": {"id": "Apache-2.0"}}],
        "externalReferences": [
            {"type": "distribution", "url": format!("https://huggingface.co/lmz/candle-sam/tree/{MODEL_REV}")},
            {"type": "vcs", "url": format!("https://github.com/ChaoningZhang/MobileSAM/tree/{OFFICIAL_MODEL_REV}")}
        ],
        "properties": [
            {"name": "photocraft:official-checkpoint-sha256", "value": OFFICIAL_MODEL_SHA256},
            {"name": "photocraft:provenance-gate", "value": "tensor-equivalent-to-official-checkpoint"},
            {"name": "photocraft:runtime-path", "value": "models/mobile-sam-tiny-vitt.safetensors.part-*"}
        ]
    })
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value[field].as_str().ok_or_else(|| format!("cargo metadata: {field} is missing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_package_coordinates_receive_stable_unique_refs() {
        let a = json!({"id":"a", "name":"same", "version":"1.0.0", "source":"registry+a"});
        let b = json!({"id":"b", "name":"same", "version":"1.0.0", "source":"git+b"});
        let packages = BTreeMap::from([("a", &a), ("b", &b)]);
        let refs = component_refs(&BTreeSet::from(["a".into(), "b".into()]), &packages).expect("refs");
        assert_ne!(refs["a"], refs["b"]);
        assert!(refs["a"].starts_with("pkg:cargo/same@1.0.0#"));
    }

    #[test]
    fn model_identity_is_pinned_in_the_component() {
        let model = model_component();
        assert_eq!(model["hashes"][0]["content"], MODEL_SHA256);
        assert_eq!(model["licenses"][0]["license"]["id"], "Apache-2.0");
    }
}
