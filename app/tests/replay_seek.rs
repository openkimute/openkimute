//! A replay's seeks in the real game: a match recorded, then played with a seek forward and one back; each must show
//! what playing straight to the same step shows (the match's state, the HUD, the picture, pixel for pixel).
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

/// The replay's line of a `--shot` run: its step, state and HUD.
fn replay_line(log: &str) -> &str {
    let line = log.lines().find(|l| l.contains("replay at step")).unwrap_or_else(|| panic!("no replay line in:\n{log}"));
    &line[line.find("replay at step").unwrap()..]
}

/// Record a match of `mode` (the player idle, the CPU winning), then seek in its replay: 5 s forward at step 400, and
/// back from step 1700 (into the round before).
fn seeks_show_what_playing_straight_shows(mode: &[&str], name: &str) {
    let Some(assets) = assets() else { return };
    let save = std::env::temp_dir().join(format!("openkumite_seek_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&save);
    std::fs::create_dir_all(&save).unwrap();
    let shot = |n: &str| save.join(format!("{n}.png")).to_string_lossy().into_owned();
    let mut args = mode.to_vec();
    let rec = shot("rec");
    args.extend(["--round-time", "20", "--shot", &rec, "--frames", "9000"]);
    let log = run(&assets, &save, &args);
    let Some(id) = std::fs::read_dir(save.join("replays")).ok().into_iter().flatten().flatten().find_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned())) else {
        // (no GPU here: nothing ran)
        assert!(!log.contains("replay"), "a match was played but no replay saved:\n{log}");
        eprintln!("skipping: no match ran (a GPU?):\n{log}");
        return;
    };
    let play = |n: &str, frames: u32, keys: Option<&str>| {
        let (out, frames) = (shot(n), frames.to_string());
        let mut a = vec!["--replay", &id, "--shot", &out, "--frames", &frames];
        if let Some(k) = keys {
            a.extend(["--keys", k]);
        }
        let log = run(&assets, &save, &a);
        (replay_line(&log).to_string(), std::fs::read(&out).unwrap())
    };
    for (what, (seek, keys), straight) in [("forward", (600, "400:right"), 900), ("back", (1800, "1700:left"), 1500)] {
        let (line, png) = play(&format!("{what}_seek"), seek, Some(keys));
        let (line0, png0) = play(&format!("{what}_straight"), straight, None);
        assert!(line.starts_with(&format!("replay at step {straight},")), "{name}: the seek {what} lands where it should: {line}");
        assert_eq!(line, line0, "{name}: after the seek {what}, the match and HUD as when played straight");
        assert!(png == png0, "{name}: after the seek {what}, the picture as when played straight (see {})", save.display());
    }
    let _ = std::fs::remove_dir_all(&save);
}

#[test]
fn a_fight_replay_seeks_as_played_straight() {
    seeks_show_what_playing_straight_shows(&["--mode", "arcade", "--player", "paul"], "fight");
}

#[test]
fn a_tag_replay_seeks_as_played_straight() {
    seeks_show_what_playing_straight_shows(&["--mode", "tag-teams", "--fighters", "paul,law,king,lei"], "tag");
}
