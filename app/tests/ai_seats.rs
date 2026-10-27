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

/// VS `--p2-cpu` (P1 idle): 2P is the game's CPU, kept in the replay as its seat, so the replay plays it again.
#[test]
fn a_vs_match_against_the_cpu_replays_as_recorded() {
    let mode = ["--mode", "vs", "--fighters", "paul,law", "--p2-cpu"];
    an_ai_match_replays_as_recorded(&mode, "9000", r#""ai":[null,{"style":"Classic","difficulty":1,"level":5},null,null]"#, "p2cpu");
}

/// Force's co-op of two AIs (chickens, the soldiers split, a down 2P up again: a recorded join): a stage's replay
/// ends as recorded.
#[test]
fn a_force_co_op_of_two_ais_replays_as_recorded() {
    let mode = ["--mode", "force", "--fighters", "paul,law", "--ai", "1,2", "--ai-style", "1:thrower,2:rushdown", "--coop"];
    an_ai_match_replays_as_recorded(&mode, "30000", r#""ai":[{"style":"Thrower","difficulty":2,"level":9},{"style":"Rushdown","difficulty":2,"level":9},null,null]"#, "force");
}

/// The game's log of one headless run (its own save folder), or None without a GPU / the game data.
fn log_of(name: &str, args: &[&str]) -> Option<String> {
    let assets = assets()?;
    let save = std::env::temp_dir().join(format!("openkumite_ai_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&save);
    std::fs::create_dir_all(&save).unwrap();
    let shot = save.join("shot.png").to_string_lossy().into_owned();
    let mut a = args.to_vec();
    a.extend(["--shot", &shot]);
    let log = run(&assets, &save, &a);
    let _ = std::fs::remove_dir_all(&save);
    log.contains("--mode Force").then_some(log)
}

/// Force's co-op: a boss a player, never the same character twice (Jun's stage 2 had two Heihachis: the second
/// was the next stage's boss, the same one in another costume).
#[test]
fn the_co_op_bosses_are_different_characters() {
    let Some(log) = log_of("bosses", &["--mode", "force", "--fighters", "t2:jun,jin", "--coop", "--force-stage", "2", "--boss", "--ai", "1,2", "--frames", "300"]) else {
        eprintln!("skipping: no Force run");
        return;
    };
    let line = log.lines().find_map(|l| l.split_once("Force: bosses ").map(|(_, b)| b.to_string())).expect("the bosses picked");
    let chars: Vec<&str> = line.trim_matches(['[', ']']).split("), (").map(|p| p.trim_matches(['(', ')']).split(',').next().unwrap().trim()).collect();
    assert_eq!(chars.len(), 2, "a boss each: {line}");
    assert_ne!(chars[0], chars[1], "two different bosses: {line}");
}

/// `--ai-style random` draws from the game's seed: `--seed` gives other styles (headless runs fixed it at 1).
#[test]
fn random_styles_follow_the_seed() {
    let styles = |seed: &str| {
        log_of(&format!("seed{seed}"), &["--mode", "force", "--fighters", "paul,law", "--coop", "--ai", "1,2", "--ai-style", "random", "--seed", seed, "--frames", "10"])
            .map(|l| l.lines().find(|l| l.contains("--ai: ")).unwrap().to_string())
    };
    let (Some(a), Some(b), Some(a2)) = (styles("2"), styles("3"), styles("2")) else {
        eprintln!("skipping: no Force run");
        return;
    };
    assert_ne!(a.split("--ai: ").nth(1), b.split("--ai: ").nth(1), "seeds 2 and 3 draw other styles");
    assert_eq!(a.split("--ai: ").nth(1), a2.split("--ai: ").nth(1), "the same seed, the same styles");
}

/// Force co-op's stage 2 to its end (a pair of AIs): in its boss fight a second boss's hit once took Jun out of
/// her throw; the boss she held played her move, which only the throw ends, and stood held with the clock
/// stopped for good. The stage now ends (here cleared) and its replay plays to the state recorded.
#[test]
fn a_co_op_boss_fight_with_a_throw_cut_short_ends() {
    let Some(assets) = assets() else { return };
    let save = std::env::temp_dir().join(format!("openkumite_ai_force_throw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&save);
    std::fs::create_dir_all(&save).unwrap();
    let shot = save.join("shot.png").to_string_lossy().into_owned();
    let mode = ["--mode", "force", "--fighters", "t2:jun,t2:lei", "--coop", "--force-stage", "2", "--ai", "1,2", "--ai-style", "1:defensive,2:defensive"];
    let mut args = mode.to_vec();
    args.extend(["--shot", &shot, "--frames", "10300"]);
    let log = run(&assets, &save, &args);
    if !log.contains("--mode Force") {
        eprintln!("skipping: no Force run (a GPU?)");
        return;
    }
    let id = log.lines().find_map(|l| l.split_once("replay ").and_then(|(_, r)| r.split_once(" saved: ")).map(|(id, _)| id.to_string())).expect("the stage ended and its replay was saved");
    let file = std::fs::read_to_string(save.join("replays").join(format!("{id}.okreplay"))).unwrap();
    assert!(file.contains(r#""result":"STAGE CLEAR""#), "the stage is cleared");
    let log = run(&assets, &save, &["--replay", &id, "--shot", &shot, "--frames", "10300"]);
    assert!(log.contains("the state as recorded: Some(true)"), "its replay plays to the state recorded:\n{log}");
    let _ = std::fs::remove_dir_all(&save);
}
