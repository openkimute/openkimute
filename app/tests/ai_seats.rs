//! The AI playing seats (`--ai`) in the real game: a match of AIs recorded, its replay saved with their seats, then
//! played back to the state recorded.
//! On the converted assets (OPENKUMITE_ASSETS, default the repository's assets/) and a GPU; skipped without either.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests: a failure stops them

use std::path::{Path, PathBuf};
use std::process::Command;

fn assets() -> Option<PathBuf> {
    let root = std::env::var_os("OPENKUMITE_ASSETS").map_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets"), PathBuf::from);
    kumite_assets::Assets::new(&root).map_err(|e| eprintln!("skipping: {e:#}")).ok().map(|_| root)
}

/// The game run headless (`--shot`) with its own save folder; its log.
fn run(assets: &Path, save: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_openkumite")).arg("--assets").arg(assets).args(args).env("OPENKUMITE_SAVE_DIR", save).env("RUST_LOG", "info").output().unwrap();
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Record a match of `mode` with its AI seats for `frames`, then play its replay to the end: as recorded.
fn an_ai_match_replays_as_recorded(mode: &[&str], frames: &str, seats: &str, name: &str) {
    let Some(assets) = assets() else { return };
    let save = std::env::temp_dir().join(format!("openkumite_ai_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&save);
    std::fs::create_dir_all(&save).unwrap();
    let shot = save.join("shot.png").to_string_lossy().into_owned();
    let mut args = mode.to_vec();
    args.extend(["--round-time", "20", "--shot", &shot, "--frames", frames]);
    let log = run(&assets, &save, &args);
    let Some(path) = std::fs::read_dir(save.join("replays")).ok().into_iter().flatten().flatten().map(|e| e.path()).next() else {
        // (no GPU here: nothing ran)
        assert!(!log.contains("replay"), "a match was played but no replay saved:\n{log}");
        eprintln!("skipping: no match ran (a GPU?):\n{log}");
        return;
    };
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(file.contains(seats), "{name}: the replay keeps the AI's seats ({seats}):\n{file}");
    let id = path.file_stem().unwrap().to_string_lossy().into_owned();
    let log = run(&assets, &save, &["--replay", &id, "--shot", &shot, "--frames", frames]);
    assert!(log.contains("replay at step"), "{name}: the replay played:\n{log}");
    assert!(!log.contains("the replay's run-through ended in another state than recorded"), "{name}: the replay ends as recorded:\n{log}");
    assert!(log.contains("the state as recorded: Some(true)"), "{name}: watched to its end, as recorded:\n{log}");
    let _ = std::fs::remove_dir_all(&save);
}

#[test]
fn an_ai_vs_ai_match_replays_as_recorded() {
    let mode = ["--mode", "vs", "--ai", "1,2", "--ai-style", "1:rushdown,2:defensive"];
    an_ai_match_replays_as_recorded(&mode, "9000", r#""ai":[{"style":"Rushdown","difficulty":2,"level":9},{"style":"Defensive","difficulty":2,"level":9},null,null]"#, "vs");
}

#[test]
fn a_tag_brawl_of_four_ais_replays_as_recorded() {
    let mode = ["--mode", "tag-teams", "--fighters", "paul,law,king,lei", "--ai", "1,2,3,4", "--ai-style", "random", "--ai-level", "7"];
    an_ai_match_replays_as_recorded(&mode, "12000", r#""difficulty":2,"level":7},{"style":"#, "tag");
}
