//! Replays: every match recorded as the inputs its steps took (`kumite_game::bout::Tape`), with what made the match
//! (its setup and the parameters the mode gave it), so it plays again exactly, as PC action games keep their demos.
//! Saved when the match ends: natively `replays/` in the save folder ([`crate::save::save_dir`]), in a browser the
//! page's local storage. The newest [`KEEP`] are kept, and any marked kept; older ones go.
//!
//! A replay plays only on the game data it was recorded on (the conversion's fingerprint), and checks itself: the
//! state at its end is the one recorded ([`ReplayFile::hash`]).

use crate::menu::FightSetup;
use kumite_game::bout::StepInput;
use kumite_sim::ai::{Ai, Style};
use kumite_sim::input::Pad;
use serde::{Deserialize, Serialize};

/// (2: the tag brawl and Force too, the match's kind in `game`; 3: the AI seats, `ai`. A 2 plays as one without)
pub const VERSION: u32 = 3;
/// The newest replays kept (natively; a browser's storage keeps fewer).
pub const KEEP: usize = if cfg!(target_arch = "wasm32") { 15 } else { 50 };
/// The longest match a replay holds (an hour): a longer one is a damaged file.
pub const MAX_STEPS: u64 = 60 * 60 * 60;
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

/// A seat the AI plays (`--ai`, OpenKumite's own): its style, difficulty 0..2 and level 0..9.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSeat {
    pub style: Style,
    pub difficulty: usize,
    pub level: usize,
}

impl AiSeat {
    /// Its CPU, in actor slot `slot`.
    pub fn ai(self, slot: usize) -> Ai {
        Ai::new(self.difficulty, self.level, slot).with_style(self.style)
    }
}

/// What a match is made from, by its kind.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Match {
    /// Two fighters' match (Ball 2 ON 2 too): the setup, what the mode gave it, the arcade's ladder.
    Fight { setup: FightSetup, params: MatchParams, ladder: Vec<usize> },
    /// The tag brawl: its four and their teams, on this stage.
    Tag { setup: crate::tagrun::TagSetup, stage: usize },
    /// A Force stage: its fight's setup, the run as the stage began, and players joining on the way (the step
    /// before which, and who: 1 the co-op partner, 2 and 3 for 3P and 4P).
    Force { setup: FightSetup, start: crate::forcerun::ForceStart, joins: Vec<(u64, u8)> },
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
    /// What made the match.
    pub game: Match,
    /// Steps in the match.
    pub steps: u64,
    /// The game's state hash after the last step.
    pub hash: u64,
    /// The inputs, run-length coded: (steps, the input's bits low, high).
    pub inputs: Vec<(u32, u64, u64)>,
    /// The seats the AI played (1P..4P; `--ai`), on top of the mode's own CPUs.
    #[serde(default)]
    pub ai: [Option<AiSeat>; 4],
}

impl ReplayFile {
    /// Whether it plays on this game data (its fingerprint; none known: any): of this format, recorded on it.
    pub fn plays_on(&self, data: &str) -> bool {
        (2..=VERSION).contains(&self.version) && (data.is_empty() || self.data == data)
    }

    /// Whether its inputs are as many steps as it says, at least one and no more than a replay can be (what they
    /// take in memory when played).
    pub fn length_ok(&self) -> bool {
        let n: u64 = self.inputs.iter().map(|r| u64::from(r.0)).sum();
        n == self.steps && n > 0 && n <= MAX_STEPS
    }

    /// Whether its length is right ([`Self::length_ok`]) and its numbers index only what there is (the roster, the
    /// CPU's levels, who joins a Force stage): it may be a file someone shared, damaged or made by hand.
    pub fn sane(&self, roster: &crate::menu::Roster) -> bool {
        use crate::room::MatchSetup;
        let pick = |(c, costume): (usize, usize)| roster.characters.get(c).is_some_and(|ch| costume < ch.costumes.len());
        self.length_ok()
            && self.ai.iter().flatten().all(|a| a.difficulty <= 2 && a.level <= 9)
            && match &self.game {
                Match::Fight { setup, .. } => MatchSetup::Fight(setup.clone()).numbers_valid(roster),
                Match::Tag { setup, .. } => MatchSetup::Tag(setup.clone()).numbers_valid(roster),
                Match::Force { setup, start, joins } => {
                    MatchSetup::Fight(setup.clone()).numbers_valid(roster)
                        && pick(start.player)
                        && start.partner.is_none_or(pick)
                        && start.more_players.iter().flatten().all(|&(c, costume, _)| pick((c, costume)))
                        && joins.iter().all(|&(_, who)| (1..=3).contains(&who))
                }
            }
    }
}

/// A Force replay's join (`Match::Force`'s `joins`: the step it comes before) is due once `played` steps are done.
pub fn join_due(join: (u64, u8), played: u64) -> bool {
    join.0 <= played
}

/// The first of the joins (in order) still to come with `played` steps done: after a seek's restore, its copy taken
/// before that step's joins.
pub fn joins_to_come(joins: &[(u64, u8)], played: u64) -> usize {
    joins.iter().take_while(|j| j.0 < played).count()
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
    anyhow::ensure!((2..=VERSION).contains(&file.version), "this replay is of another OpenKumite version (its format {}, this one plays {VERSION})", file.version);
    anyhow::ensure!(file.length_ok(), "this replay file is damaged");
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

    /// A Force replay seeks back and plays on: each join once, before its step, wherever the seek lands (the copies
    /// are taken before a step's joins, every 300 steps; a join can fall on one).
    #[test]
    fn joins_come_once_each_across_a_seek() {
        let joins = [(0, 1), (300, 2), (301, 3)];
        // played straight through: which joins came before which step
        let play = |from: u64, mut next: usize, to: u64, came: &mut Vec<(u64, u8)>| {
            for step in from..to {
                while joins.get(next).is_some_and(|&j| join_due(j, step)) {
                    came.push((step, joins[next].1));
                    next += 1;
                }
            }
        };
        let mut straight = Vec::new();
        play(0, 0, 900, &mut straight);
        assert_eq!(straight, joins.to_vec());
        for back in [0, 1, 299, 300, 301, 302, 600] {
            let mut came: Vec<_> = straight.iter().copied().filter(|j| j.0 < back).collect();
            play(back, joins_to_come(&joins, back), 900, &mut came);
            assert_eq!(came, straight, "seeking back to {back}");
        }
    }

    /// A replay's numbers are checked before it plays: a good one plays, one damaged anywhere does not.
    #[test]
    fn a_damaged_replay_is_not_played() {
        let gd = crate::test_assets().join("gamedata");
        if !kumite_fs::exists(&gd.join("screens.json")) {
            return;
        }
        let roster = crate::menu::Roster::load(&gd);
        let mut good = file(1);
        let Match::Fight { setup, .. } = good.game.clone() else { unreachable!() };
        let start = crate::forcerun::ForceStart {
            player: (0, 0),
            selection: 0,
            player_char: 0,
            difficulty: 1,
            player_costume: 0,
            index: 0,
            extra: None,
            pending_final: false,
            score: 0,
            hiscore: 0,
            keys: 0,
            flags: 0,
            kills: 0,
            clock: 0,
            partner: Some((1, 0)),
            partner_costume: 0,
            more_players: [Some((2, 0, 0)), None],
        };
        good.game = Match::Force { setup, start, joins: vec![(10, 1), (20, 2)] };
        assert!(good.sane(&roster));
        good.ai[0] = Some(AiSeat { style: Style::Thrower, difficulty: 2, level: 9 });
        let damage: [fn(&mut ReplayFile); 8] = [
            |f| f.steps = 2,
            |f| f.inputs = vec![(u32::MAX, 0, 0); 2],
            |f| {
                if let Match::Force { joins, .. } = &mut f.game {
                    joins.push((30, 7))
                }
            },
            |f| {
                if let Match::Force { start, .. } = &mut f.game {
                    start.player.0 = 999
                }
            },
            |f| {
                if let Match::Force { start, .. } = &mut f.game {
                    start.more_players[1] = Some((0, 99, 0))
                }
            },
            |f| {
                if let Match::Force { setup, .. } = &mut f.game {
                    setup.players[1] = (999, 0)
                }
            },
            |f| f.ai[3] = Some(AiSeat { style: Style::Classic, difficulty: 3, level: 0 }),
            |f| f.ai[0] = Some(AiSeat { style: Style::Classic, difficulty: 0, level: 10 }),
        ];
        for (k, d) in damage.iter().enumerate() {
            let mut f = good.clone();
            d(&mut f);
            assert!(!f.sane(&roster), "damage {k} not caught");
        }
    }

    /// A file someone shared, damaged anywhere (bytes changed, digits put in, cut short): it does not read, or its
    /// checks say what it is, never a panic, never more inputs than a replay holds. (The app's own fuzzing: the
    /// fuzz targets, `fuzz/`, reach only the libraries.)
    #[test]
    fn a_damaged_file_never_panics() {
        let gd = crate::test_assets().join("gamedata");
        let roster = kumite_fs::exists(&gd.join("screens.json")).then(|| crate::menu::Roster::load(&gd));
        let mut good = file(9);
        good.inputs = encode(&[StepInput::default(), StepInput { partner: Pad { start: true, ..Pad::default() }, ..StepInput::default() }]);
        good.steps = 2;
        let good = to_bytes(&good).unwrap();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let (mut read, mut passed) = (0, 0);
        for _ in 0..20_000 {
            let mut b = good.clone();
            // (mostly its numbers: a count, an index, a step; now and then any byte, or the end cut off)
            for _ in 0..next() % 3 + 1 {
                let digits: Vec<usize> = (0..b.len()).filter(|&i| b[i].is_ascii_digit()).collect();
                if digits.is_empty() {
                    break;
                }
                let d = digits[next() as usize % digits.len()];
                match next() % 8 {
                    0..=3 => b[d] = b'0' + (next() % 10) as u8,
                    4..=5 => b.insert(d, b'0' + (next() % 10) as u8),
                    6 => {
                        let i = next() as usize % b.len();
                        b[i] = next() as u8;
                    }
                    _ => b.truncate(next() as usize % b.len()),
                }
            }
            let Ok(f) = serde_json::from_slice::<ReplayFile>(&b) else { continue };
            read += 1;
            if f.length_ok() {
                assert!(decode(&f.inputs).len() as u64 <= MAX_STEPS);
                passed += u32::from(roster.as_ref().is_some_and(|r| f.sane(r)));
            }
        }
        // (enough of them still read, the checks had something to say)
        assert!(read > 1000, "{read} read");
        eprintln!("{read} damaged files still read, {passed} passed the checks");
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
            game: Match::Fight { setup, params, ladder: vec![] },
            steps: 1,
            hash: 0,
            inputs: encode(&[StepInput::default()]),
            ai: [None; 4],
        }
    }

    /// The AI's seats go with the replay; one of format 2, from before them, reads and plays without.
    #[test]
    fn the_ai_seats_are_kept_and_an_older_file_still_plays() {
        let mut f = file(3);
        f.ai[1] = Some(AiSeat { style: Style::Rushdown, difficulty: 1, level: 4 });
        let back: ReplayFile = serde_json::from_slice(&to_bytes(&f).unwrap()).unwrap();
        assert_eq!(back.ai, f.ai);
        let mut old = serde_json::to_value(file(3)).unwrap();
        old["version"] = 2.into();
        old.as_object_mut().unwrap().remove("ai");
        let mut old: ReplayFile = serde_json::from_value(old).unwrap();
        assert_eq!(old.ai, [None; 4]);
        assert!(old.plays_on("d"));
        old.version = VERSION + 1;
        assert!(!old.plays_on("d"));
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
        // (longer than a replay can be: its inputs would not fit in memory)
        let mut huge = file(7);
        huge.inputs = vec![(u32::MAX, 0, 0); 1000];
        huge.steps = u64::from(u32::MAX) * 1000;
        assert!(import(&to_bytes(&huge).unwrap()).unwrap_err().to_string().contains("damaged"));
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
