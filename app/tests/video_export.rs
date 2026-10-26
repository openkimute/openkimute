//! A replay made a video by the real game (`--export-video`, the default encoder: on macOS the hardware HEVC one): a
//! short match recorded, then exported small; the MP4 must hold the video's codec box at the size asked, and the audio.
//! On the converted assets (OPENKUMITE_ASSETS, default the repository's assets/) and a GPU; skipped without either.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests: a failure stops them

use std::path::{Path, PathBuf};
use std::process::Command;

fn assets() -> Option<PathBuf> {
    let root = std::env::var_os("OPENKUMITE_ASSETS").map_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets"), PathBuf::from);
    kumite_assets::Assets::new(&root).map_err(|e| eprintln!("skipping: {e:#}")).ok().map(|_| root)
}

/// The game run headless with its own save folder; its log and stdout.
fn run(assets: &Path, save: &Path, args: &[&str]) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_openkumite")).arg("--assets").arg(assets).args(args).env("OPENKUMITE_SAVE_DIR", save).env("RUST_LOG", "info").output().unwrap();
    (String::from_utf8_lossy(&out.stderr).into_owned(), String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The payload of the first box `name` anywhere under `data`'s boxes (into the containers on the way to a sample entry).
fn find_box<'a>(data: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    let mut at = 0;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let kind = &data[at + 4..at + 8];
        let body = data.get(at + 8..at + size.max(8))?;
        if kind == name {
            return Some(body);
        }
        // (stsd: a version and a count before its entries; a visual sample entry: 78 bytes before its boxes)
        let inner = match kind {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => Some(body),
            b"stsd" => body.get(8..),
            b"hvc1" | b"av01" => body.get(78..),
            _ => None,
        };
        if let Some(found) = inner.and_then(|b| find_box(b, name)) {
            return Some(found);
        }
        at += size.max(8);
    }
    None
}

#[test]
fn a_replay_exports_to_a_valid_mp4() {
    let Some(assets) = assets() else { return };
    let save = std::env::temp_dir().join(format!("openkumite_video_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&save);
    std::fs::create_dir_all(&save).unwrap();
    let shot = save.join("end.png").to_string_lossy().into_owned();
    let (log, _) = run(&assets, &save, &["--mode", "vs", "--fighters", "paul,law", "--ai", "1,2", "--rounds", "1", "--round-time", "5", "--shot", &shot, "--frames", "3000"]);
    if !log.contains("replay ") {
        eprintln!("skipping: no match ran (a GPU?):\n{log}");
        return;
    }
    let out = save.join("v.mp4");
    let (log, stdout) = run(&assets, &save, &["--export-video", "latest", "--out", out.to_str().unwrap(), "--size", "320x180"]);
    assert!(stdout.lines().any(|l| l == "DONE") && stdout.contains("PROGRESS "), "the export reports its progress and end:\n{log}");
    let mp4 = std::fs::read(&out).unwrap();
    let (codec, entry) = if cfg!(target_os = "macos") && !log.contains("the software encoder") { (b"hvcC", b"hvc1") } else { (b"av1C", b"av01") };
    assert!(find_box(&mp4, codec).is_some(), "the video's codec box {}:\n{log}", String::from_utf8_lossy(codec));
    let entry = find_box(&mp4, entry).unwrap();
    // (a visual sample entry: its width and height 24 bytes in)
    assert_eq!((u16::from_be_bytes([entry[24], entry[25]]), u16::from_be_bytes([entry[26], entry[27]])), (320, 180));
    assert!(find_box(&mp4, b"Opus").is_some(), "the audio");
    if let Ok(p) = Command::new("ffprobe").args(["-v", "error", "-show_entries", "stream=codec_name,width,height,r_frame_rate", "-of", "csv=p=0"]).arg(&out).output() {
        let p = String::from_utf8_lossy(&p.stdout);
        assert!(p.contains(",320,180,60/1") && p.contains("opus"), "ffprobe: {p}");
    }
    let _ = std::fs::remove_dir_all(&save);
}
