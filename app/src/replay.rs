//! Replays: every match recorded as the inputs its steps took (`kumite_game::bout::Tape`), with what made the match
//! (its setup and the parameters the mode gave it), so it plays again exactly, as PC action games keep their demos.
//! Saved when the match ends: natively `replays/` in the save folder ([`crate::save::save_dir`]), in a browser the
//! page's local storage. The newest [`KEEP`] are kept, and any marked kept; older ones go.
//!
//! A replay plays only on the game data it was recorded on (the conversion's fingerprint), and checks itself: the
//! state at its end is the one recorded ([`ReplayFile::hash`]).

use crate::menu::FightSetup;
use kumite_game::bout::StepInput;
use kumite_sim::input::Pad;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
/// The newest replays kept (natively; a browser's storage keeps fewer).
pub const KEEP: usize = if cfg!(target_arch = "wasm32") { 15 } else { 50 };
/// Native files' extension.
#[cfg(not(target_arch = "wasm32"))]
pub const EXT: &str = "okreplay";

/// What the mode gave the match besides its setup (`App::load`): rebuilt from these, not from the mode's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchParams {
    pub rounds: u32,
    pub round_frames: u32,
    pub max_health: [Option<i32>; 2],
    pub start_health: [Option<i32>; 2],
    /// Which sides the CPU played.
    pub cpu: [bool; 2],
    pub interlude_check: bool,
    /// The camera counters as the match began (`kumite_game::bout::counters`).
    pub counters: [usize; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayFile {
    pub version: u32,
    /// The build that recorded it.
    pub engine: String,
    /// The game data's fingerprint (manifest.json's).
    pub data: String,
    /// Unix seconds.
    pub recorded: u64,
    /// "VS MODE · PAUL vs LAW".
    pub title: String,
    /// "PAUL WINS 2–1".
    pub result: String,
    pub online: bool,
    pub kept: bool,
    pub setup: FightSetup,
    pub params: MatchParams,
    pub ladder: Vec<usize>,
    /// Steps in the match.
    pub steps: u64,
    /// The game's state hash after the last step.
    pub hash: u64,
    /// The inputs, run-length coded: (steps, the input's bits low, high).
    pub inputs: Vec<(u32, u64, u64)>,
}

/// A replay as listed: where it is, and the file (its inputs too: they are small).
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub file: ReplayFile,
}

fn pad_bits(p: Pad) -> u128 {
    u128::from(p.up) | u128::from(p.down) << 1 | u128::from(p.left) << 2 | u128::from(p.right) << 3 | u128::from(p.buttons) << 4 | u128::from(p.start) << 12
}

fn bits_pad(b: u128) -> Pad {
    Pad { up: b & 1 != 0, down: b & 2 != 0, left: b & 4 != 0, right: b & 8 != 0, buttons: (b >> 4) as u8, start: b >> 12 & 1 != 0 }
}

/// 14 bits a seat: present, and the pad's 13.
fn seat_bits(p: Option<Pad>) -> u128 {
    p.map_or(0, |p| 1 | pad_bits(p) << 1)
}

fn bits_seat(b: u128) -> Option<Pad> {
    (b & 1 != 0).then(|| bits_pad(b >> 1 & 0x1FFF))
}

fn pack(s: &StepInput) -> u128 {
    pad_bits(s.pads[0]) | pad_bits(s.pads[1]) << 13 | pad_bits(s.partner) << 26 | seat_bits(s.extra) << 39 | seat_bits(s.more[0]) << 53 | seat_bits(s.more[1]) << 67
}

fn unpack(b: u128) -> StepInput {
    let pad = |at: u32| bits_pad(b >> at & 0x1FFF);
    let seat = |at: u32| bits_seat(b >> at & 0x3FFF);
    StepInput { pads: [pad(0), pad(13)], partner: pad(26), extra: seat(39), more: [seat(53), seat(67)] }
}

/// The inputs, run-length coded.
pub fn encode(inputs: &[StepInput]) -> Vec<(u32, u64, u64)> {
    let mut out: Vec<(u32, u64, u64)> = Vec::new();
    for s in inputs {
        let b = pack(s);
        let (lo, hi) = (b as u64, (b >> 64) as u64);
        match out.last_mut() {
            Some(run) if run.1 == lo && run.2 == hi && run.0 < u32::MAX => run.0 += 1,
            _ => out.push((1, lo, hi)),
        }
    }
    out
}

pub fn decode(runs: &[(u32, u64, u64)]) -> Vec<StepInput> {
    runs.iter().flat_map(|&(n, lo, hi)| std::iter::repeat_n(unpack(u128::from(lo) | u128::from(hi) << 64), n as usize)).collect()
}

/// Unix seconds now.
pub fn now() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The match's length as "m:ss".
pub fn duration(steps: u64) -> String {
    let s = steps / 60;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The replays there are, the newest first.
pub fn list() -> Vec<Entry> {
    let mut all = store::list();
    all.sort_by(|a, b| b.file.recorded.cmp(&a.file.recorded).then_with(|| b.id.cmp(&a.id)));
    all
}

/// Save a replay, then let the oldest beyond [`KEEP`] go (not the ones kept). Its id, or the error.
pub fn save(file: &ReplayFile) -> anyhow::Result<String> {
    let id = store::save(file)?;
    for old in list().into_iter().filter(|e| !e.file.kept).skip(KEEP) {
        store::remove(&old.id);
    }
    Ok(id)
}

/// Keep a replay for good (or not).
pub fn set_kept(id: &str, kept: bool) -> anyhow::Result<()> {
    let mut e = list().into_iter().find(|e| e.id == id).ok_or_else(|| anyhow::anyhow!("no replay {id}"))?;
    e.file.kept = kept;
    store::write(id, &e.file)
}

pub fn remove(id: &str) {
    store::remove(id);
}

/// A replay as a file to share (what Export writes, Import reads).
pub fn to_bytes(file: &ReplayFile) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(file)?)
}

/// A file name for it: "openkumite-VS-Paul-vs-Law-2026-10-25-1520.okreplay" (the time in UTC).
pub fn file_name(file: &ReplayFile) -> String {
    let name: String = file.title.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let name = name.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    let (y, mo, d) = civil(file.recorded / 86400);
    let (h, mi) = (file.recorded % 86400 / 3600, file.recorded % 3600 / 60);
    format!("openkumite-{name}-{y:04}-{mo:02}-{d:02}-{h:02}{mi:02}.okreplay")
}

/// The date of a day count since 1970-01-01 (proleptic Gregorian).
fn civil(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Import a replay file someone shared: checked, then saved and kept (it is not one of this computer's newest).
/// Its id; the one already here, when it is.
pub fn import(bytes: &[u8]) -> anyhow::Result<String> {
    use anyhow::Context;
    let mut file: ReplayFile = serde_json::from_slice(bytes).context("this is not an OpenKumite replay file")?;
    anyhow::ensure!(file.version == VERSION, "this replay is of another OpenKumite version (its format {}, this one plays {VERSION})", file.version);
    let steps = file.inputs.iter().map(|r| u64::from(r.0)).sum::<u64>();
    anyhow::ensure!(steps == file.steps && steps > 0, "this replay file is damaged");
    if let Some(e) = list().into_iter().find(|e| e.file.recorded == file.recorded && e.file.hash == file.hash && e.file.inputs == file.inputs) {
        return Ok(e.id);
    }
    file.kept = true;
    store::save(&file)
}

#[cfg(not(target_arch = "wasm32"))]
mod store {
    use super::{Entry, ReplayFile, EXT};
    use std::path::PathBuf;

    fn dir() -> PathBuf {
        crate::save::save_dir().join("replays")
    }

    pub fn list() -> Vec<Entry> {
        let Ok(rd) = std::fs::read_dir(dir()) else { return Vec::new() };
        rd.flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == EXT))
            .filter_map(|p| {
                let file = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<ReplayFile>(&b).ok())?;
                Some(Entry { id: p.file_stem()?.to_string_lossy().into_owned(), file })
            })
            .collect()
    }

    pub fn save(file: &ReplayFile) -> anyhow::Result<String> {
        std::fs::create_dir_all(dir())?;
        // (two in one second: a suffix)
        let id = (0..).map(|n| if n == 0 { file.recorded.to_string() } else { format!("{}-{n}", file.recorded) }).find(|id| !dir().join(format!("{id}.{EXT}")).exists()).unwrap_or_default();
        write(&id, file)?;
        Ok(id)
    }

    /// Written whole: a temporary file renamed over it.
    pub fn write(id: &str, file: &ReplayFile) -> anyhow::Result<()> {
        let path = dir().join(format!("{id}.{EXT}"));
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(file)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn remove(id: &str) {
        let _ = std::fs::remove_file(dir().join(format!("{id}.{EXT}")));
    }
}

#[cfg(target_arch = "wasm32")]
mod store {
    use super::{Entry, ReplayFile};

    const PREFIX: &str = "openkumite_replay:";

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    pub fn list() -> Vec<Entry> {
        let Some(s) = storage() else { return Vec::new() };
        let n = s.length().unwrap_or(0);
        (0..n)
            .filter_map(|i| s.key(i).ok().flatten())
            .filter_map(|k| k.strip_prefix(PREFIX).map(str::to_string))
            .filter_map(|id| {
                let text = s.get_item(&format!("{PREFIX}{id}")).ok().flatten()?;
                Some(Entry { file: serde_json::from_str(&text).ok()?, id })
            })
            .collect()
    }

    pub fn save(file: &ReplayFile) -> anyhow::Result<String> {
        let id = file.recorded.to_string();
        let id =
            (0..).map(|n| if n == 0 { id.clone() } else { format!("{id}-{n}") }).find(|id| storage().and_then(|s| s.get_item(&format!("{PREFIX}{id}")).ok().flatten()).is_none()).unwrap_or_default();
        write(&id, file)?;
        Ok(id)
    }

    pub fn write(id: &str, file: &ReplayFile) -> anyhow::Result<()> {
        let s = storage().ok_or_else(|| anyhow::anyhow!("no storage"))?;
        s.set_item(&format!("{PREFIX}{id}"), &serde_json::to_string(file)?).map_err(|_| anyhow::anyhow!("the browser's storage is full"))
    }

    pub fn remove(id: &str) {
        if let Some(s) = storage() {
            let _ = s.remove_item(&format!("{PREFIX}{id}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_pack_and_unpack() {
        let p = |b: u8| Pad { up: b & 1 != 0, down: b & 2 != 0, left: b & 4 != 0, right: b & 8 != 0, buttons: b, start: b & 16 != 0 };
        let steps: Vec<StepInput> = (0..300u32)
            .map(|i| {
                let k = (i / 7) as u8;
                StepInput { pads: [p(k), p(k.wrapping_mul(3))], partner: p(k ^ 5), extra: k.is_multiple_of(2).then(|| p(k)), more: [k.is_multiple_of(3).then(|| p(1)), None] }
            })
            .collect();
        let runs = encode(&steps);
        assert!(runs.len() < steps.len() / 5, "run-length coded");
        assert_eq!(decode(&runs), steps);
    }

    /// The tests that use the replays' folder, one at a time.
    static FOLDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn file(recorded: u64) -> ReplayFile {
        let setup = FightSetup { mode: crate::menu::Mode::Versus, players: [(0, 0), (1, 0)], stage: 0, difficulty: 1, level: 0, handicap: [100; 2], ball: None, partners: None };
        let params = MatchParams { rounds: 2, round_frames: 3600, max_health: [None; 2], start_health: [None; 2], cpu: [false; 2], interlude_check: false, counters: [0; 3] };
        ReplayFile {
            version: VERSION,
            engine: "test".into(),
            data: "d".into(),
            recorded,
            title: "t".into(),
            result: "r".into(),
            online: false,
            kept: false,
            setup,
            params,
            ladder: vec![],
            steps: 1,
            hash: 0,
            inputs: encode(&[StepInput::default()]),
        }
    }

    #[test]
    fn a_file_name_says_what_and_when() {
        let mut f = file(1_792_941_618);
        f.title = "ONLINE VS · Paul vs Law".into();
        assert_eq!(file_name(&f), "openkumite-ONLINE-VS-Paul-vs-Law-2026-10-25-1520.okreplay");
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn an_exported_replay_imports_once_and_kept() {
        let _one = FOLDER.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::remove_dir_all(crate::save::save_dir().join("replays"));
        let bytes = to_bytes(&file(5)).unwrap();
        let id = import(&bytes).unwrap();
        assert_eq!(import(&bytes).unwrap(), id, "the same one twice: not again");
        let all = list();
        assert_eq!(all.len(), 1);
        assert!(all[0].file.kept);
        assert!(import(b"{}").is_err());
        let mut damaged = file(6);
        damaged.steps = 99;
        assert!(import(&to_bytes(&damaged).unwrap()).unwrap_err().to_string().contains("damaged"));
        let _ = std::fs::remove_dir_all(crate::save::save_dir().join("replays"));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_newest_are_kept_and_the_kept_ones() {
        let _one = FOLDER.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::remove_dir_all(crate::save::save_dir().join("replays"));
        let first = save(&file(1)).unwrap();
        set_kept(&first, true).unwrap();
        for t in 2..=(KEEP as u64 + 10) {
            save(&file(t)).unwrap();
        }
        let all = list();
        // the newest KEEP, and the first: kept
        assert_eq!(all.len(), KEEP + 1);
        assert!(all.iter().any(|e| e.id == first && e.file.kept));
        assert_eq!(all.iter().filter(|e| !e.file.kept).map(|e| e.file.recorded).min(), Some(11));
        remove(&first);
        assert_eq!(list().len(), KEEP);
        let _ = std::fs::remove_dir_all(crate::save::save_dir().join("replays"));
    }
}
