//! The game build's fingerprint (`OPENKUMITE_BUILD`, said in the online hello): a hash of everything two computers in a
//! match must share: the sources of every crate of the fight and the app (lib.rs mixes the fight loop with the
//! screens), Cargo.lock (another version of a maths crate may round otherwise) and the manifests (features such as
//! glam's libm, the profiles). Line ends are left out (a Windows checkout converts them). A different build would
//! desync mid-match; it is refused at the hello instead. (A debug and a release build of one tree share it: debug
//! builds check overflows, so they are for testing, not for playing online.)

use std::path::{Path, PathBuf};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn main() {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    // the app and every crate a match runs: each one's sources and manifest (its dependencies' features: glam's
    // libm and scalar maths); the workspace's manifest (the profiles) and lock file (the dependencies' versions)
    let crates = [".", "../crates/kumite_sim", "../crates/kumite_game", "../crates/kumite_net", "../crates/kumite_assets", "../crates/kumite_fs"];
    let mut all = Vec::new();
    for c in crates.map(|c| here.join(c)) {
        files(&c.join("src"), &mut all);
        all.push(c.join("Cargo.toml"));
        println!("cargo:rerun-if-changed={}", c.join("src").display());
    }
    all.extend(["../Cargo.toml", "../Cargo.lock"].map(|f| here.join(f)));
    for f in all.iter().filter(|f| f.extension().is_none_or(|x| x != "rs")) {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    all.sort();
    // FNV-1a, 64 bits: names (relative) and contents
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h = (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    for p in &all {
        eat(p.strip_prefix(&here).unwrap_or(p).to_string_lossy().replace('\\', "/").as_bytes());
        eat(&[0]);
        let text: Vec<u8> = std::fs::read(p).unwrap_or_default().into_iter().filter(|&b| b != b'\r').collect();
        eat(&text);
    }
    println!("cargo:rustc-env=OPENKUMITE_BUILD={h:016x}");

    // Windows: the icon in the .exe (openkumite.rc)
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed={}", here.join("../packaging/windows/openkumite.ico").display());
        if let Err(e) = embed_resource::compile("openkumite.rc", embed_resource::NONE).manifest_optional() {
            println!("cargo:warning=the .exe has no icon: {e}");
        }
    }
}
