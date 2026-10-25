//! Bad files found by fuzzing (`fuzz/`) and review: errors, never panics. A panic is caught natively (`from_glb`
//! turns it into an error), but not on the web: so any panic here fails the test, whatever `from_glb` returns.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

static PANICKED: AtomicBool = AtomicBool::new(false);

fn no_panic<T>(f: impl FnOnce() -> T) -> T {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| PANICKED.store(true, Ordering::SeqCst)));
    let r = f();
    std::panic::set_hook(hook);
    r
}

/// A glb of a JSON chunk and a binary one.
fn glb(json: &serde_json::Value, bin: &[u8]) -> Vec<u8> {
    let mut j = json.to_string().into_bytes();
    j.resize(j.len().next_multiple_of(4), b' ');
    let mut out = Vec::new();
    for w in [0x4654_6C67, 2, (28 + j.len() + bin.len()) as u32, j.len() as u32, 0x4E4F_534A] {
        out.extend(w.to_le_bytes());
    }
    out.extend(j);
    out.extend([bin.len() as u32, 0x004E_4942].iter().flat_map(|w| w.to_le_bytes()));
    out.extend(bin);
    out
}

/// The smallest model both loaders take: one triangle skinned to one joint, a 1×1 PNG, one rotation track.
fn model() -> (serde_json::Value, Vec<u8>) {
    let f = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    // a 1×1 RGBA PNG
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\nIDATx\x9cc\0\x01\0\0\x05\0\x01\r\n-\xb4\0\0\0\0IEND\xaeB`\x82".to_vec();
    let mut identity = [0.0; 16];
    identity[0..16].iter_mut().step_by(5).for_each(|x| *x = 1.0);
    let parts = [
        f(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]),
        f(&[0.; 6]),
        vec![0; 24],
        f(&[1., 0., 0., 0.].repeat(3)),
        [0u16, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect(),
        f(&identity),
        f(&[0.]),
        f(&[0., 0., 0., 1.]),
        png,
    ];
    let mut bin = Vec::new();
    let mut views = Vec::new();
    for p in &parts {
        views.push(serde_json::json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": p.len()}));
        bin.extend(p);
        bin.resize(bin.len().next_multiple_of(4), 0);
    }
    let acc = |view: usize, c: u32, count: usize, t: &str| serde_json::json!({"bufferView": view, "componentType": c, "count": count, "type": t});
    let mut accessors = vec![
        acc(0, 5126, 3, "VEC3"),
        acc(1, 5126, 3, "VEC2"),
        acc(2, 5123, 3, "VEC4"),
        acc(3, 5126, 3, "VEC4"),
        acc(4, 5123, 3, "SCALAR"),
        acc(5, 5126, 1, "MAT4"),
        acc(6, 5126, 1, "SCALAR"),
        acc(7, 5126, 1, "VEC4"),
    ];
    accessors[0]["min"] = serde_json::json!([0, 0, 0]);
    accessors[0]["max"] = serde_json::json!([1, 1, 0]);
    accessors[6]["min"] = serde_json::json!([0]);
    accessors[6]["max"] = serde_json::json!([0]);
    let json = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"byteLength": bin.len()}],
        "bufferViews": views,
        "accessors": accessors,
        "images": [{"bufferView": 8, "mimeType": "image/png"}],
        "textures": [{"source": 0}],
        "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "TEXCOORD_0": 1, "JOINTS_0": 2, "WEIGHTS_0": 3}, "indices": 4, "material": 0}]}],
        "nodes": [{"mesh": 0, "skin": 0}],
        "skins": [{"joints": [0], "inverseBindMatrices": 5}],
        "animations": [{"samplers": [{"input": 6, "output": 7}], "channels": [{"sampler": 0, "target": {"node": 0, "path": "rotation"}}]}],
        "scenes": [{"nodes": [0]}],
        "scene": 0,
    });
    (json, bin)
}

#[test]
fn a_bad_glb_is_an_error_without_a_panic() {
    let (json, bin) = model();
    assert!(kumite_assets::Character::from_glb(&glb(&json, &bin)).is_ok() && kumite_assets::Stage::from_glb(&glb(&json, &bin)).is_ok(), "the model to break loads");
    // one field broken each (as fuzzing found them in converted models), and the error it gives
    let broken: [(&str, fn(&mut serde_json::Value), &str); 6] = [
        ("animation_node", |j| j["animations"][0]["channels"][0]["target"]["node"] = 1.into(), "an animation channel of the wrong kind, or of no node"),
        ("animation_path", |j| j["animations"][0]["channels"][0]["target"]["path"] = "rktation".into(), "a glb with values out of range"),
        ("image_source", |j| j["images"][0] = serde_json::json!({"mimeType": "image/png"}), "an image that is not a PNG of the glb's own, or too big"),
        ("position_accessor", |j| j["accessors"] = serde_json::json!([]), "a mesh's attribute names no accessor"),
        ("view_range", |j| j["bufferViews"][1]["byteOffset"] = 1_000_000.into(), "a buffer view outside its buffer"),
        (
            "animations_nodes",
            |j| {
                j["animations"] = vec![j["animations"][0].clone(); 2049].into();
                let mut nodes = vec![serde_json::json!({}); 2049];
                nodes[0] = j["nodes"][0].clone();
                j["nodes"] = nodes.into();
            },
            "more animations times nodes than a glb has",
        ),
    ];
    let mut bad: Vec<(String, Vec<u8>, &str)> = broken
        .iter()
        .map(|(name, f, error)| {
            let mut j = json.clone();
            f(&mut j);
            (name.to_string(), glb(&j, &bin), *error)
        })
        .collect();
    // two images each said (by the PNG's header) to be 4096×4096
    let (mut big, mut j) = (bin.clone(), json.clone());
    let at = json["bufferViews"][8]["byteOffset"].as_u64().unwrap() as usize;
    big[at + 16..at + 24].copy_from_slice(&[0, 0, 16, 0, 0, 0, 16, 0]);
    j["images"] = vec![j["images"][0].clone(); 2].into();
    bad.push(("texture_pixels".into(), glb(&j, &big), "images of more pixels together than a glb has"));
    // one big index accessor (4M, of bytes) used by three primitives: 4 MB asking for 50 MB of indices, and a
    // thousand primitives would ask for 16 GB
    let (mut big, mut j) = (bin.clone(), json.clone());
    j["bufferViews"].as_array_mut().unwrap().push(serde_json::json!({"buffer": 0, "byteOffset": big.len(), "byteLength": 1 << 22}));
    big.resize(big.len() + (1 << 22), 0);
    j["buffers"][0]["byteLength"] = big.len().into();
    j["accessors"].as_array_mut().unwrap().push(serde_json::json!({"bufferView": 9, "componentType": 5121, "count": 1 << 22, "type": "SCALAR"}));
    let mut prim = j["meshes"][0]["primitives"][0].clone();
    prim["indices"] = 8.into();
    j["meshes"][0]["primitives"] = vec![prim; 3].into();
    bad.push(("reused_accessor".into(), glb(&j, &big), "more to read than a glb has"));
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bad");
    for f in std::fs::read_dir(dir).unwrap().flatten() {
        bad.push((format!("{:?}", f.path()), std::fs::read(f.path()).unwrap(), ""));
    }
    assert!(bad.len() >= 13, "the bad files are there");
    for (name, b, error) in &bad {
        no_panic(|| {
            for e in [kumite_assets::Stage::from_glb(b).err(), kumite_assets::Character::from_glb(b).err()] {
                assert!(e.as_ref().is_some_and(|e| e.to_string().contains(error)), "{name}: {e:?}");
            }
        });
        assert!(!PANICKED.load(Ordering::SeqCst), "{name} panicked (an abort on the web)");
    }
}

/// Every converted model passes the checks (they refuse what the converter never writes, nothing it does).
#[test]
fn every_converted_glb_loads() {
    let root = std::env::var_os("OPENKUMITE_ASSETS").map_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets"), std::path::PathBuf::from);
    let glbs = |dir: &str| -> Vec<std::path::PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(root.join(dir)).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "glb")).collect();
        v.sort();
        v
    };
    for p in glbs("stages") {
        if let Err(e) = kumite_assets::Stage::load(&p) {
            panic!("{}: {e:#}", p.display());
        }
    }
    for p in glbs("characters") {
        if let Err(e) = kumite_assets::Character::load(&p) {
            panic!("{}: {e:#}", p.display());
        }
    }
}
