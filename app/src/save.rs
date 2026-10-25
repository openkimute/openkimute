//! The save data this port keeps, as the game's memory card save does: the records screen's tables (Time Attack,
//! Survival, Force), what is unlocked and played, and the settings (DIFFICULTY, graphics, controls).
//! Natively a JSON file in the system's folder for application data ([`save_dir`]: on macOS
//! `~/Library/Application Support/OpenKumite`, on Windows `%APPDATA%\OpenKumite`, elsewhere
//! `$XDG_DATA_HOME/openkumite` or `~/.local/share/openkumite`; `OPENKUMITE_SAVE_DIR` overrides it), in a browser the
//! page's local storage.
//!
//! A save never leaves a broken file behind: natively the new text goes to a temporary file, flushed to the disk,
//! the last save is kept as `.bak`, and the temporary file is renamed over the save (atomic), the folder flushed
//! too; a crash at any point leaves the old save or the new one whole. Loading takes the save, else the `.bak`;
//! one that does not read is kept aside (`.corrupt-<time>`), not written over.
//!
//! Each window writes the whole save: natively the first one holds the folder's lock (`openkumite_save.lock`) while it
//! runs, and another saves nothing (it would drop the first one's records). Browser tabs share the page's storage
//! with no such lock: the last tab to save wins.

use serde::{Deserialize, Serialize};

pub const FILE: &str = "openkumite_save.json";

/// A new entry waiting for its name (the records screen's name entry).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewRecord {
    /// Time Attack: the character id's row.
    TimeAttack(usize),
    /// Survival: the row 0..9.
    Survival(usize),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Records {
    /// Per character id: best total frames and the name.
    pub time_attack: Vec<(u32, String)>,
    /// Top ten: character id, wins, name.
    pub survival: Vec<(usize, u32, String)>,
    /// A Ball match has been played (save byte 0x80097F1E: the first one is against ball.json's first opponent).
    #[serde(default)]
    pub ball_played: bool,
    /// Force's hi-score (0x80097F14) and keys (0x80097F18).
    #[serde(default)]
    pub force_hiscore: u32,
    #[serde(default)]
    pub force_keys: u32,
    /// Per character id: the usage counters (save +0x108): 1P sessions, 2P wins, 2P losses, 2P draws.
    #[serde(default)]
    pub usage: Vec<[u16; 4]>,
    /// The records screen's next page (0x800980FC) and background stage rotor (0x800980FD).
    #[serde(default)]
    pub page: u8,
    #[serde(default)]
    pub stage_rotor: u8,
    /// Endings seen (0x80097EF8, bit = character id) and the alternate ones (0x80097EFC: the alternate endings and the
    /// long cut's second clear): the Theater's locks.
    #[serde(default)]
    pub endings: u32,
    #[serde(default)]
    pub alternate: u32,
    /// The extra fighters' endings seen (bit = theater.json `ending.extra_bits`; the Theater sees them as bits 32..
    /// of its locks).
    #[serde(default)]
    pub endings_extra: u32,
    /// ONLINE's nickname.
    #[serde(default)]
    pub online_name: String,
    /// OPTION's DIFFICULTY (0 EASY, 1 MEDIUM, 2 HARD).
    #[serde(default = "medium")]
    pub difficulty: usize,
    /// OpenKumite's graphics settings (OPTION > GRAPHICS).
    #[serde(default)]
    pub graphics: crate::graphics::Graphics,
    /// OpenKumite's controls (OPTION > KEY CONFIG); one that does not read is the defaults (the records stay).
    #[serde(default, deserialize_with = "lenient")]
    pub controls: crate::controls::Controls,
    /// The unlocking name typed as a record name (unlocks its character, 0x800561B0).
    #[serde(default)]
    pub name_unlock: bool,
    #[serde(skip)]
    pub new: Vec<NewRecord>,
}

impl Records {
    /// The rows of the per-character tables (the time-attack records by character id, the usage counters): the
    /// time-attack table's (its defaults, screens.json `modes.records.time_attack`, hold a row per character).
    pub fn characters(&self) -> usize {
        self.time_attack.len().max(1)
    }

    /// The saved records, or the game's defaults.
    pub fn load(ta: &[(u32, String)], survival: &[(usize, u32, String)]) -> Self {
        load_text().unwrap_or_else(|| Records { difficulty: 1, ..Default::default() }).completed(ta, survival)
    }

    /// The tables' rows past the saved ones from the defaults; the controls whole; every value in range.
    fn completed(mut self, ta: &[(u32, String)], survival: &[(usize, u32, String)]) -> Self {
        if let Some(rest) = ta.get(self.time_attack.len()..) {
            self.time_attack.extend_from_slice(rest);
        }
        if let Some(rest) = survival.get(self.survival.len()..) {
            self.survival.extend_from_slice(rest);
            self.survival.sort_by_key(|e| std::cmp::Reverse(e.1)); // (most wins first; a stable sort keeps ties' order)
        }
        self.controls = std::mem::take(&mut self.controls).complete();
        self.sanitize();
        self
    }

    /// Every value in range: the save is the player's file (or the browser's storage), edited or broken any way;
    /// what reads it must not meet a number it cannot use (on the web a panic cannot be caught).
    fn sanitize(&mut self) {
        let name = |n: &mut String, max: usize| *n = n.chars().filter(|c| !c.is_control()).take(max).collect();
        self.difficulty = self.difficulty.min(crate::menu::DIFFICULTIES.len() - 1);
        self.page %= 3;
        self.time_attack.truncate(MAX_ROWS);
        self.survival.truncate(MAX_ROWS);
        for (_, n) in &mut self.time_attack {
            name(n, MAX_NAME);
        }
        for (c, _, n) in &mut self.survival {
            if *c >= self.time_attack.len().max(1) {
                *c = 0;
            }
            name(n, MAX_NAME);
        }
        self.usage.truncate(self.characters());
        name(&mut self.online_name, crate::room::MAX_NAME);
        self.graphics = std::mem::take(&mut self.graphics).sane();
    }

    fn count(&mut self, id: usize, k: usize) {
        let n = self.characters();
        if self.usage.len() < n {
            self.usage.resize(n, [0; 4]);
        }
        let c = &mut self.usage[id % n][k];
        *c = c.saturating_add(1);
    }

    /// A one-player session ended with this character (0x800513B4).
    pub fn played(&mut self, id: usize) {
        self.count(id, 0);
    }

    /// A two-player match: the winner's and the loser's characters (0x80051458), or a draw (0x80051550).
    pub fn two_player(&mut self, ids: [usize; 2], winner: Option<usize>) {
        match winner {
            Some(w) => {
                self.count(ids[w], 1);
                self.count(ids[1 - w], 2);
            }
            None => {
                self.count(ids[0], 3);
                self.count(ids[1], 3);
            }
        }
    }

    pub fn save(&self) {
        if let Ok(t) = serde_json::to_string_pretty(self) {
            write(&t);
        }
    }

    /// A Time Attack total for character `id`: a new best when below the record (0x800B46B8), with a blank name.
    pub fn time_attack(&mut self, id: usize, frames: u32) -> bool {
        match self.time_attack.get_mut(id) {
            Some(r) if frames < r.0 => {
                *r = (frames, "   ".into());
                self.new.push(NewRecord::TimeAttack(id));
                true
            }
            _ => false,
        }
    }

    /// The rank `wins` would take (0x800B467C): the first entry with fewer wins, 1-based; 0 = none.
    pub fn survival_rank(&self, wins: u32) -> u32 {
        self.survival.iter().position(|e| e.1 < wins).map_or(0, |i| i as u32 + 1)
    }

    /// A Survival run's wins: into the top ten when above an entry (0x800B47CC), with a blank name.
    pub fn survival(&mut self, id: usize, wins: u32) -> bool {
        let Some(i) = self.survival.iter().position(|e| e.1 < wins) else { return false };
        self.survival.insert(i, (id, wins, "   ".into()));
        self.survival.truncate(10);
        self.new.push(NewRecord::Survival(i));
        self.page = 1;
        true
    }
}

fn medium() -> usize {
    1
}

/// The saved records: the save, else its backup; one that does not read is kept aside.
fn load_text() -> Option<Records> {
    let backup = read(BACKUP);
    let main = read(FILE);
    let (records, broken) = choose(main.as_deref(), backup.as_deref());
    // (one there that does not read as text at all: not UTF-8, no permission)
    let broken = broken || (main.is_none() && present(FILE));
    if broken && !read_only() && !keep_aside(main.as_deref().unwrap_or_default()) {
        failed("The save does not read and could not be kept aside: nothing is saved this time".into());
        set_read_only();
    }
    records
}

/// The save's text, else the backup's: the records, and whether the save was there but did not read.
fn choose(main: Option<&str>, backup: Option<&str>) -> (Option<Records>, bool) {
    let parse = |t: &str| serde_json::from_str::<Records>(t).map_err(|e| e.to_string());
    let broken = match main.map(parse) {
        Some(Ok(r)) => return (Some(r), false),
        Some(Err(e)) => {
            log::warn!("{FILE} does not read ({e}): kept aside, the backup instead");
            true
        }
        None => false,
    };
    match backup.map(parse) {
        Some(Ok(r)) => {
            log::warn!("{FILE}: loaded from {BACKUP}");
            (Some(r), broken)
        }
        Some(Err(e)) => {
            log::warn!("{BACKUP} does not read either ({e}): a new save");
            (None, broken)
        }
        None => (None, broken),
    }
}

const BACKUP: &str = "openkumite_save.json.bak";
#[cfg(not(target_arch = "wasm32"))]
const LOCK: &str = "openkumite_save.lock";
/// A records table's longest kept, and a record name's (the game's are three letters).
const MAX_ROWS: usize = 100;
const MAX_NAME: usize = 8;

/// Tool runs (`--shot`) never write the save.
static READ_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_read_only() {
    READ_ONLY.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn read_only() -> bool {
    READ_ONLY.load(std::sync::atomic::Ordering::Relaxed)
}

/// The last save that failed, not yet told the player.
static FAILED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// A save failed: logged, and kept for the player ([`take_failure`]).
fn failed(why: String) {
    log::warn!("{why}");
    *FAILED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(why);
}

/// A save that failed since the last call: what to tell the player (records, unlocks and options not kept).
pub fn take_failure() -> Option<String> {
    FAILED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
}

/// The folder of the save: `OPENKUMITE_SAVE_DIR`, else the system's folder for application data.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_dir() -> std::path::PathBuf {
    // (tests never touch the player's save)
    if cfg!(test) {
        return std::env::temp_dir().join(format!("openkumite_test_save_{}", std::process::id()));
    }
    env_dir("OPENKUMITE_SAVE_DIR").or_else(|| system_dir("OpenKumite", "openkumite")).unwrap_or_else(|| ".".into())
}

#[cfg(not(target_arch = "wasm32"))]
fn env_dir(k: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(Into::into)
}

/// The system's folder for application data called `name` (`linux` elsewhere than on macOS and Windows).
#[cfg(not(target_arch = "wasm32"))]
fn system_dir(name: &str, linux: &str) -> Option<std::path::PathBuf> {
    let home = env_dir("HOME").or_else(|| env_dir("USERPROFILE"));
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support").join(name))
    } else if cfg!(windows) {
        env_dir("APPDATA").map(|a| a.join(name))
    } else {
        env_dir("XDG_DATA_HOME").filter(|d| d.is_absolute()).or_else(|| home.map(|h| h.join(".local/share"))).map(|d| d.join(linux))
    }
}

/// The save's file `name` in its folder.
#[cfg(not(target_arch = "wasm32"))]
fn at(name: &str) -> std::path::PathBuf {
    save_dir().join(name)
}

#[cfg(not(target_arch = "wasm32"))]
fn read(name: &str) -> Option<String> {
    std::fs::read_to_string(at(name)).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn present(name: &str) -> bool {
    at(name).exists()
}

/// A save that does not read: its bytes copied to `.corrupt-<unix time>` (never written over); whether they are.
#[cfg(not(target_arch = "wasm32"))]
fn keep_aside(_text: &str) -> bool {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    std::fs::copy(at(FILE), at(&format!("{FILE}.corrupt-{t}"))).map_err(|e| log::warn!("keeping {FILE} aside: {e}")).is_ok()
}

/// The save folder's lock, held from the first call to the end of the run; when another window holds it, saving
/// is off and this is what to tell the player.
#[cfg(not(target_arch = "wasm32"))]
pub fn lock() -> Option<&'static str> {
    static HELD: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();
    if read_only() || HELD.get().is_some() {
        return None;
    }
    let file = std::fs::create_dir_all(save_dir()).and_then(|_| std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(at(LOCK)));
    match file.map(|f| (f.try_lock(), f)) {
        Ok((Ok(()), f)) => {
            let _ = HELD.set(f);
            None
        }
        Ok((Err(std::fs::TryLockError::WouldBlock), _)) => {
            log::warn!("another window has {}: this one saves nothing", save_dir().display());
            set_read_only();
            Some("another game window has the save: this one saves nothing")
        }
        // (a folder without locks: saved as before)
        Ok((Err(std::fs::TryLockError::Error(e)), _)) | Err(e) => {
            log::warn!("locking {}: {e}", at(LOCK).display());
            None
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn write(text: &str) {
    if read_only() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(save_dir()).and_then(|_| write_atomic(&at(FILE), text.as_bytes())) {
        log::warn!("saving {}: {e}", at(FILE).display());
        failed(format!("The game could not be saved: {e}"));
    }
}

/// `path` replaced by `bytes` so that a crash at any point leaves the old file or the new one whole: the new
/// text to `<path>.tmp` (flushed), the old file copied to `.bak` (through `.bak.tmp`, flushed), the temporary
/// file renamed over the save, the folder flushed.
#[cfg(not(target_arch = "wasm32"))]
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let with = |ext: &str| {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        std::path::PathBuf::from(p)
    };
    let flushed = |p: &std::path::Path, data: &[u8]| -> std::io::Result<()> {
        let mut f = std::fs::File::create(p)?;
        f.write_all(data)?;
        f.sync_all()
    };
    let tmp = with(".tmp");
    flushed(&tmp, bytes)?;
    // the last save, whole, as the backup
    if let Ok(old) = std::fs::read(path) {
        if serde_json::from_slice::<Records>(&old).is_ok() {
            let bak_tmp = with(".bak.tmp");
            flushed(&bak_tmp, &old)?;
            std::fs::rename(&bak_tmp, with(".bak"))?;
        }
    }
    std::fs::rename(&tmp, path)?;
    // the renames themselves on the disk (the folder's entries)
    #[cfg(unix)]
    if let Some(dir) = path.parent().map(|d| if d.as_os_str().is_empty() { std::path::Path::new(".") } else { d }) {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(target_arch = "wasm32")]
fn read(name: &str) -> Option<String> {
    storage()?.get_item(name).ok().flatten()
}

#[cfg(target_arch = "wasm32")]
fn present(name: &str) -> bool {
    read(name).is_some()
}

#[cfg(target_arch = "wasm32")]
fn keep_aside(text: &str) -> bool {
    storage().is_some_and(|s| s.set_item(&format!("{FILE}.corrupt-{}", js_sys::Date::now() as u64), text).is_ok())
}

/// In a browser each item is written whole (local storage); the last save is kept as the backup item first.
#[cfg(target_arch = "wasm32")]
fn write(text: &str) {
    if read_only() {
        return;
    }
    let Some(s) = storage() else {
        failed("The game could not be saved: this browser keeps no storage for the page".into());
        return;
    };
    if let Some(old) = s.get_item(FILE).ok().flatten().filter(|o| serde_json::from_str::<Records>(o).is_ok()) {
        let _ = s.set_item(BACKUP, &old);
    }
    // (the page's storage is shared with the replays: full, some of them go)
    if s.set_item(FILE, text).is_err() {
        failed("The game could not be saved: the browser's storage for this page is full (delete some replays)".into());
    }
}

/// A value that does not read as `T`: T's default (a setting must not lose the records with it).
fn lenient<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let v = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_else(|e| {
        log::warn!("save: a setting does not read ({e}); its defaults");
        T::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_save_falls_back_to_the_backup() {
        let good = serde_json::to_string(&Records { difficulty: 2, ..Default::default() }).unwrap();
        let (r, broken) = choose(Some(&good), None);
        assert_eq!((r.map(|r| r.difficulty), broken), (Some(2), false));
        // cut off mid-write (the old non-atomic save), the backup whole
        let (r, broken) = choose(Some(&good[..good.len() / 2]), Some(&good));
        assert_eq!((r.map(|r| r.difficulty), broken), (Some(2), true));
        let (r, broken) = choose(Some("{"), Some("garbage"));
        assert!(r.is_none() && broken);
        assert!(choose(None, None).0.is_none());
        // a save from before a field existed reads, with its default
        let (r, _) = choose(Some("{\"name_unlock\": true}"), None);
        assert_eq!(r.map(|r| (r.name_unlock, r.difficulty)), Some((true, 1)));
    }

    /// A save edited by hand (or broken) with numbers out of every range: loaded, then all in range.
    #[test]
    fn a_hostile_save_is_brought_into_range() {
        let long = "X".repeat(100_000);
        let text = format!(
            r#"{{"time_attack": [[4294967295, "{long}"]], "survival": [[18446744073709551615, 7, "{long}"]], "usage": {usage},
               "page": 255, "stage_rotor": 255, "difficulty": 18446744073709551615, "online_name": "{long}",
               "graphics": {{"display": "Window", "resolution": [0, 4294967295], "scale": 1e30, "vsync": true, "smooth_textures": false, "stats": false}}}}"#,
            usage = serde_json::to_string(&vec![[u16::MAX; 4]; 5000]).unwrap()
        );
        let (r, broken) = choose(Some(&text), None);
        assert!(!broken);
        let mut r = r.expect("it reads");
        r.sanitize();
        assert_eq!((r.page, r.difficulty, r.usage.len()), (0, 2, r.time_attack.len()));
        assert_eq!(r.survival[0].0, 0);
        assert!(r.survival[0].2.len() <= MAX_NAME && r.time_attack[0].1.len() <= MAX_NAME && r.online_name.len() <= crate::room::MAX_NAME);
        assert!((0.25..=2.0).contains(&r.graphics.scale));
        assert!(r.graphics.resolution.iter().all(|&d| (crate::graphics::MIN_SIDE..=crate::graphics::MAX_SIDE).contains(&d)));
        let mut nan = Records::default();
        nan.graphics.scale = f32::NAN;
        nan.sanitize();
        assert_eq!(nan.graphics.scale, 1.0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn saving_replaces_the_file_whole_and_keeps_the_last_one() {
        let dir = std::env::temp_dir().join(format!("openkumite_save_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("save.json");
        let text = |d: usize| serde_json::to_string(&Records { difficulty: d, ..Default::default() }).unwrap();
        write_atomic(&path, text(0).as_bytes()).unwrap();
        write_atomic(&path, text(2).as_bytes()).unwrap();
        let read = |p: &std::path::Path| serde_json::from_str::<Records>(&std::fs::read_to_string(p).unwrap()).unwrap().difficulty;
        assert_eq!(read(&path), 2);
        assert_eq!(read(&dir.join("save.json.bak")), 0, "the last save kept");
        assert!(!dir.join("save.json.tmp").exists() && !dir.join("save.json.bak.tmp").exists(), "no temporary files left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A save from before a table grew keeps its rows; the defaults fill the rest.
    #[test]
    fn short_tables_keep_their_rows() {
        let saved = Records { time_attack: vec![(5, "AAA".into())], survival: vec![(1, 1, "BBB".into())], ..Default::default() };
        let r = saved.completed(&[(100, "X".into()), (200, "Y".into())], &[(0, 3, "C".into()), (0, 2, "D".into())]);
        assert_eq!(r.time_attack, [(5, "AAA".to_string()), (200, "Y".to_string())]);
        assert_eq!(r.survival, [(0, 2, "D".to_string()), (1, 1, "BBB".to_string())], "by wins");
    }

    /// A save there that is not text is kept aside, byte for byte, before anything is written over it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_save_not_text_is_kept_aside() {
        std::fs::create_dir_all(save_dir()).unwrap();
        let bytes = b"{\"name_unlock\": \xff}";
        std::fs::write(at(FILE), bytes).unwrap();
        assert!(load_text().is_none() && !read_only());
        let kept: Vec<_> = std::fs::read_dir(save_dir()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{FILE}.corrupt-"))).map(|e| e.path()).collect();
        assert_eq!(kept.iter().map(|p| std::fs::read(p).unwrap()).collect::<Vec<_>>(), [bytes.to_vec()]);
        for p in kept.iter().chain([&at(FILE)]) {
            std::fs::remove_file(p).unwrap();
        }
    }

    /// The first window holds the folder's lock; another one cannot take it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_save_folder_is_locked() {
        assert!(lock().is_none() && !read_only());
        let other = std::fs::OpenOptions::new().write(true).open(at(LOCK)).unwrap();
        assert!(matches!(other.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    }

    #[test]
    fn survival_top_ten() {
        let mut r = Records { time_attack: vec![(100, "AAA".into())], survival: (0..10).map(|i| (i, 10 - i as u32, "X".into())).collect(), ..Default::default() };
        assert_eq!(r.survival_rank(11), 1);
        assert_eq!(r.survival_rank(1), 0);
        assert!(r.survival(3, 5));
        assert_eq!(r.survival[6], (3, 5, "   ".to_string()), "inserted after the entries with as many wins or more");
        assert_eq!(r.survival.len(), 10);
        assert!(r.time_attack(0, 99));
        assert!(!r.time_attack(0, 99));
    }
}
