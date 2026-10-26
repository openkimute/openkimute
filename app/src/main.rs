//! OpenKumite: application shell (window, 60 Hz loop, input, screens, drawing).
//!
//! The engine loads only converted assets in standard formats (glTF, PNG, JSON, WAV/FLAC, AVI), produced once
//! from your own disc by kumite_converter; where they are looked for: `assetdir.rs`.
//!
//! The game starts at the title: ARCADE (1P against ten CPU opponents), VS MODE (two players), WATCH CPU
//! and PRACTICE; then character select (the original 7x3 grid; the button you pick with chooses the costume)
//! and stage select; then the fight with the HUD; Esc pauses.
//!
//! Controls, one player: arrows or WASD to move/crouch/jump (tap up/down to sidestep), U I J K = buttons
//! 1 2 3 4 (left punch, right punch, left kick, right kick), Enter = start.
//! Two players (VS MODE): P1 = WASD + F G V B, P2 = arrows + ; ' . / (or numpad 4 5 1 2).
//! Gamepads (first = P1, second = P2): Square / Triangle / Cross / Circle = 1 / 2 / 3 / 4, d-pad or stick,
//! Start = start / pause, Select = back.
//! Blocking: standing still or holding back guards highs and mids, crouching (down / down-back) guards lows.
//!
//! Sandbox (straight into a free fight, no rounds): `--fight` or any of `--models A,B`, `--stage N`,
//! `--p1/--p2 SCRIPT`, `--shot`; `--no-intro` / `--intro V` for the camera fly-in.
//! Start a move at a fight step (testing): `--play "40:2:common_162"` (step:player:animation).
//! Scripted run in the window: `--keys "30:start,60:b1,400:shot" --quit-after 600` (menu keys / screenshots
//! at drawn frames). Headless check: `openkumite --shot out.png --frames N [--p1 "f*30,n*5,1,n*40"]`;
//! `--screen title|menu|options|select|vs|practice|practice-fight|versus|versus-vs|versus-practice|hud` renders a menu screen (after N frames) or a
//! match with its HUD; `--keys` apply at their step.
//!
//! `--help` lists the command line.
//!
//! Debug and testing (see `debug.rs`): F1 debug panel (FPS, timings, fighter/camera state, hit log, move
//! browser, fight setup), F2 pause, F3 step a frame, F4 hit volumes, F5 reset round, F6 record P1, F7 P2
//! mode (idle / mirror / replay recording / hold back / hold down-back), F8 free camera, F9 infinite health,
//! F12 screenshot, F10 video statistics (also `--stats`), - / = speed, M sound on/off. Options: `--mute`, `--debug` (panel open), `--paused`, `--speed X`, scripts as
//! `@file`, `--draw hurt,push,attack,skeleton,samples|all` and `--ui` (panel) for headless shots.

/// Tests: the converted assets, from OPENKUMITE_ASSETS (default: the repository's assets/).
#[cfg(test)]
fn test_assets() -> std::path::PathBuf {
    std::env::var_os("OPENKUMITE_ASSETS").map_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets"), Into::into)
}

mod art;
#[cfg(not(target_arch = "wasm32"))]
mod assetdir;
#[cfg(not(target_arch = "wasm32"))]
mod audio;
#[cfg(target_arch = "wasm32")]
#[path = "audio_web.rs"]
mod audio;
mod ballview;
mod blobshadow;
mod controls;
mod debug;
mod demo;
#[cfg(not(target_arch = "wasm32"))]
mod direct;
mod files;
mod forcerun;
#[cfg(not(target_arch = "wasm32"))]
mod gamepad;
#[cfg(target_arch = "wasm32")]
#[path = "gamepad_web.rs"]
mod gamepad;
mod graphics;
mod grid;
mod hud;
mod keyconfig;
mod launch;
mod menu;
mod modes;
mod movie;
mod netlink;
mod netplay;
mod online;
mod onlinemenu;
mod practice;
mod records;
mod replay;
mod replay_view;
mod room;
#[cfg(target_os = "macos")]
mod rumble_mac;
mod save;
mod tagrun;
mod theater;
#[cfg(not(target_arch = "wasm32"))]
mod video;
#[cfg(not(target_arch = "wasm32"))]
mod video_export;
#[cfg(target_os = "macos")]
mod video_mac;

use anyhow::{Context, Result};
use art::Art;
use debug::{Action, Catalog, Debug, EguiLayer};
use hud::{Hud, HudInfo};
use kumite_assets::Assets;
use kumite_game::bout::{Bout, Phase};
use kumite_game::Game;
use kumite_render::{Camera, Renderer, SkinVertex, SkinnedModel, StaticMesh, StaticVertex};
use kumite_sim::input::Pad;
use menu::{FightSetup, Key, Mode, Outcome, Roster, Screen};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use web_time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// A tag brawl is over for its replay this many steps into its end (`step_tag`'s wait before the next screen).
const TAG_SAVED_AFTER: u32 = 150;

/// The game logic runs at a fixed 60 steps per second (one step per NTSC field, kumite_converter's reference engine.md
/// §2.3).
const STEP: Duration = Duration::from_nanos(16_666_667);

/// GPU resources for a loaded fight.
struct Scene {
    sky: Vec<StaticMesh>,
    /// The floor map's repeat period (m), when the floor follows the fighters.
    floor_period: Option<[f32; 2]>,
    /// The drum's base below the floor (m), its view-space Y scale, drawn before the floor (stages.json `drum_first`).
    sky_base_y: f32,
    sky_y_scale: f32,
    sky_first: bool,
    /// The full-screen fill behind the stage (sRGB; one stage's sky blue, black elsewhere).
    background: [u8; 3],
    /// Ball's sky strip (it is the `sky` mesh then): its bottom distance ahead and height (game units, y
    /// down) and its height in screen pixels.
    strip: Option<[f32; 3]>,
    /// An extra stage: the sky drum follows the camera; the floor patch (tile size, tiles a side).
    camera_drum: bool,
    floor_patch: Option<[f32; 2]>,
    floor_light: Option<(u32, f32, [f32; 32])>,
    statics: Vec<StaticMesh>,
    models: Vec<SkinnedModel>,
    /// Force: the actors' GPU models by (actor, model record), made as bodies come in.
    force_models: Option<std::collections::HashMap<(usize, usize), SkinnedModel>>,
    /// The effect atlas (hit-spark sheets + generated cells), if the converted sheets are there.
    effects: Option<Arc<kumite_render::Texture>>,
    /// What each fighter's texture shows of its face animation (eyes shut, the moving pupils' offsets), per GPU model.
    faces_shown: [(usize, kumite_game::face::Face); 2],
    /// An extra level's walk grid (the fight keeps its fighters on it).
    walls: Option<Arc<kumite_sim::Walls>>,
    /// And the free floor its camera stays over.
    camera_walls: Option<Arc<kumite_sim::Walls>>,
    /// Cut away what stands between the camera and the fighters (a level without its walk grid: Force's
    /// extra stages).
    cutaway: bool,
    /// A level laid along a route: its surfaces above the floor beyond this z (the camera's side) are not drawn.
    near_cut: Option<f32>,
    /// The stage's light for the fighters (the original's table; None: the plain light).
    light: Option<kumite_render::StageLight>,
    /// The fighters' shadows (the stage's shadow angles; None: no shadows).
    shadow: Option<kumite_render::Shadow>,
    /// Force's blob shadows instead, in this colour (the light table's +0x18).
    blob: Option<[u8; 3]>,
}

/// The attract embu running: the scene and, per slot, a GPU mesh for every cast model.
struct EmbuRun {
    scene: kumite_game::embu::EmbuScene,
    models: [HashMap<usize, SkinnedModel>; 2],
}

/// The attract loop's state: its step (0x80097F3E), the embu variant last shown (0x80097F20), the demo list
/// index (0x80097F13).
#[derive(Default)]
struct Attract {
    step: u8,
    variant: usize,
    demo: usize,
}

/// A cut-scene inside an arcade match (the boss-change scene): its own two fighters and their GPU models.
struct Cutscene {
    scene: kumite_game::scene::BossChangeScene,
    models: Vec<SkinnedModel>,
    /// START was pressed: straight to the swap.
    skip: bool,
}

/// The skinned GPU meshes of a game's two fighters.
fn skinned_models(r: &Renderer, game: &Game) -> Vec<SkinnedModel> {
    game.bodies.iter().map(|b| skinned_body(r, b)).collect()
}

fn skinned_body(r: &Renderer, b: &kumite_game::Body) -> SkinnedModel {
    skinned_character(r, &b.character, b.model_record)
}

/// The fighters' shadow meshes (gamedata/shadows.json), loaded at the start.
static SHADOWS: std::sync::OnceLock<kumite_assets::Shadows> = std::sync::OnceLock::new();

/// A character's GPU model, with the shadow mesh its model record casts.
fn skinned_character(r: &Renderer, c: &kumite_assets::Character, record: usize) -> SkinnedModel {
    let verts: Vec<SkinVertex> = c.vertices.iter().map(|v| SkinVertex { pos: v.pos, uv: v.uv, joints: v.joints, weights: v.weights, normal: v.normal }).collect();
    let mut model = r.skinned(&verts, &c.indices, r.texture(c.texture.width, c.texture.height, &c.texture.rgba));
    if let Some((v, i)) = SHADOWS.get().and_then(|s| s.of(record)).and_then(|m| c.shadow(m)) {
        let v: Vec<SkinVertex> = v.iter().map(|v| SkinVertex { pos: v.pos, uv: v.uv, joints: v.joints, weights: v.weights, normal: v.normal }).collect();
        r.set_shadow(&mut model, &v, &i);
    }
    model
}

/// Copy the face animation's rectangles into a fighter's texture where `face` differs from what it shows (`shown`): the
/// blink's closed or original eyes, the moving pupils' eye strips shifted by their offsets (from the texels as loaded).
fn apply_face(r: &Renderer, model: &SkinnedModel, atlas: &kumite_assets::Image, data: &kumite_game::face::FaceData, id: usize, face: &kumite_game::face::Face, shown: &mut kumite_game::face::Face) {
    let copy = |src: [u32; 2], dest: [u32; 4]| {
        // (u64: sizes from the conversion cannot wrap; a rectangle outside the texels is skipped)
        let ([x, y, w, h], [sx, sy], aw, ah) = (dest.map(u64::from), src.map(u64::from), u64::from(atlas.width), u64::from(atlas.height));
        if sx + w > aw || sy + h > ah || x + w > aw || y + h > ah {
            return;
        }
        let mut px = Vec::new();
        let byte = |texel: u64| usize::try_from(texel.checked_mul(4)?).ok();
        for row in sy..sy + h {
            let at = row * aw + sx;
            let Some(line) = byte(at).zip(byte(at + w)).and_then(|(a, b)| atlas.rgba.get(a..b)) else { return };
            px.extend_from_slice(line);
        }
        r.update_texture(model.texture(), dest, &px);
    };
    if let Some(b) = data.blink.get(&id) {
        if face.closed != shown.closed {
            copy(if face.closed { b.closed } else { [b.dest[0], b.dest[1]] }, b.dest);
            shown.closed = face.closed;
        }
    }
    if let Some(eyes) = data.pupils.get(&id) {
        for (k, e) in eyes.iter().enumerate() {
            if let (Some(v), true) = (face.pupils[k], face.pupils[k] != shown.pupils[k]) {
                if let Ok(sy) = u32::try_from(i64::from(e.source_y) + i64::from(v)) {
                    copy([e.x, sy], [e.x, e.dest_y, e.w, e.h]);
                }
                shown.pupils[k] = face.pupils[k];
            }
        }
    }
}

/// What made a match being recorded as a replay (`replay.rs`).
struct ReplayRec {
    game: replay::Match,
    online: bool,
}

/// The fight being played.
struct Fight {
    bout: Bout,
    setup: Option<FightSetup>,
    /// Arcade: the ladder of opponent character ids.
    ladder: Vec<usize>,
    names: [String; 2],
    /// What the HUD shows for this fight.
    info: HudInfo,
    /// Pause menu: (cursor, player who paused).
    pause: Option<(usize, usize)>,
    /// Ball's rules card is open (the pause menu's RULES).
    rules: bool,
    /// Practice mode: its menu, dummy and attack data.
    practice: Option<practice::Practice>,
    /// What the fight's sounds go by (a replay's seek copies keep it).
    heard: Heard,
    /// Sound banks ("m071"), character ids and voice rows (+0x1A) of the two fighters.
    banks: [String; 2],
    characters: [usize; 2],
    voices: [usize; 2],
    /// The boss-change scene, while it runs.
    cutscene: Option<Cutscene>,
    /// The hanging / flapping joints' go-over between clips (drawing).
    secondary: std::cell::RefCell<[kumite_game::SecondaryBlend; 2]>,
    /// Force's co-op partner: its sound bank and character id (Ball 2 ON 2: P1's partner).
    partner: Option<(String, usize)>,
    /// Ball 2 ON 2: P2's partner's (actor 2) sound bank and character id.
    extra_voice: Option<(String, usize)>,
    /// Ball 2 ON 2: who plays each actor (0, 1 the sides' first, 2 the other side's partner, 3 P1's side's).
    seats: Option<[Option<usize>; 4]>,
}

/// What a fight's sounds go by: each fighter's move before the last step (the sound triggers), and the sounds of each
/// recorded frame (aligned with the game's replay record), replayed with it.
#[derive(Clone, Default)]
pub(crate) struct Heard {
    last_moves: kumite_game::sound::SoundState,
    log: std::collections::VecDeque<Vec<kumite_game::sound::Sound>>,
    /// Record frames logged so far this round.
    logged: u64,
}

impl Fight {
    /// The fighters' sounds of the step just made (see [`Heard::step`]).
    fn step_sounds(&mut self, sound: &kumite_game::sound::SoundData) -> Vec<kumite_game::sound::Sound> {
        let banks = [self.banks[0].as_str(), self.banks[1].as_str()];
        let partner = self.partner.as_ref().map(|(b, c)| (b.as_str(), *c));
        let extra = self.extra_voice.as_ref().map(|(b, c)| (b.as_str(), *c));
        self.heard.step(sound, &self.bout, banks, self.voices, partner, extra)
    }
}

impl Heard {
    /// The fighters' sounds of the step `bout` just made, logged with the frame the game recorded; in the game's
    /// replay of a K.O., the ones logged with its frames.
    fn step(
        &mut self,
        sound: &kumite_game::sound::SoundData,
        bout: &Bout,
        banks: [&str; 2],
        voices: [usize; 2],
        partner: Option<(&str, usize)>,
        extra: Option<(&str, usize)>,
    ) -> Vec<kumite_game::sound::Sound> {
        let game = &bout.game;
        // the replay plays the sounds recorded with its frames, on each new frame (even half-frames)
        if let Some((k, new)) = bout.replay_frame() {
            let first = game.recorded_total - game.recorded.len() as u64;
            let log_first = self.logged - self.log.len() as u64;
            let at = (first + k as u64).checked_sub(log_first);
            let replayed: Vec<_> = if new { at.and_then(|i| self.log.get(i as usize)).cloned().unwrap_or_default() } else { Vec::new() };
            if !replayed.is_empty() {
                log::debug!("replay frame {k}: {} sounds", replayed.len());
            }
            return replayed;
        }
        let sounds = sound.step_with_partner(bout, &mut self.last_moves, banks, voices, partner, extra);
        // log them with the frame the game just recorded (a new round starts the record again)
        if game.recorded_total < self.logged {
            self.log.clear();
            self.logged = 0;
        }
        if game.recorded_total > self.logged {
            while self.logged + 1 < game.recorded_total {
                self.log.push_back(Vec::new());
                self.logged += 1;
            }
            self.log.push_back(sounds.clone());
            self.logged += 1;
            while self.log.len() > game.recorded.len() {
                self.log.pop_front();
            }
        }
        sounds
    }
}

struct App {
    assets: Assets,
    models: [usize; 2],
    stage: usize,
    /// Round-start camera fly-in variant for the sandbox, or None to skip it.
    intro: Option<usize>,
    /// Start in the sandbox (free fight) instead of the title.
    sandbox: bool,
    window: Option<Arc<Window>>,
    /// The graphics settings in use (the save's, over them the command line's).
    graphics: graphics::Graphics,
    /// What the display and the GPU allow (the graphics settings are fitted to it).
    display: graphics::Display,
    /// `--graphics recommended|default`: the settings start from these (made once the screen and the GPU are
    /// known), the command line's other graphics options over them (`graphics_args`).
    graphics_preset: Option<graphics::Preset>,
    graphics_args: Vec<String>,
    /// In a browser: full screen asked for, waiting for the player's input (a browser enters it only on one).
    #[cfg(target_arch = "wasm32")]
    fullscreen_pending: bool,
    /// In a browser: the page's view last seen (CSS pixels), and whether the player made the browser itself full
    /// screen (F11, its menu): the canvas then fills it, the chosen size back when it leaves.
    #[cfg(target_arch = "wasm32")]
    page_view: [u32; 2],
    #[cfg(target_arch = "wasm32")]
    page_fullscreen: bool,
    /// (an element of the page full screen, last seen)
    #[cfg(target_arch = "wasm32")]
    page_api: bool,
    renderer: Option<Renderer>,
    scene: Option<Scene>,
    fight: Option<Fight>,
    /// Menus (title, select, continue, ending); None while fighting.
    screen: Option<Screen>,
    roster: Roster,
    art: Option<Art>,
    hud: Hud,
    /// VS MODE fights so far ("FIGHT n" on the VS screen).
    vs_fights: u32,
    /// VS MODE's win tally of this session.
    vs_record: grid::VsRecord,
    /// The Team Battle / Time Attack / Survival run in progress.
    run: Option<modes::Run>,
    /// The best times and the survival top ten (the save).
    records: save::Records,
    /// Ball: which sides the CPU plays, and how often each character was drawn as its opponent
    /// (ctx+0x44 + 2c, the least drawn come first).
    ball_cpu: [bool; 2],
    ball_counts: Vec<u32>,
    /// Force: the run in progress and its data (force.json).
    force: Option<forcerun::ForceRun>,
    /// Testing (`--ko-boss`): a Force boss goes down as soon as it is in.
    force_ko_boss: bool,
    force_data: Option<Arc<kumite_game::force::ForceData>>,
    /// TAG BRAWL's match in progress (OpenKumite's own mode).
    tag: Option<tagrun::TagRun>,
    seed: u32,
    keys: HashSet<KeyCode>,
    /// Keys pressed since the last step, so a tap shorter than one 60 Hz frame is not lost.
    tapped: HashSet<KeyCode>,
    /// Online: START pressed by this computer's players (0, 1) for the next frame's pads (it skips a replay or a win
    /// pose on every computer's same frame).
    net_start: [bool; 2],
    /// Online: a match over here, its session polled a while more ([`App::linger_net`], [`LINGER`]).
    net_linger: Option<Linger>,
    script: Vec<Pad>,
    p2_script: Vec<Pad>,
    debug: Debug,
    egui: Option<EguiLayer>,
    catalog: Catalog,
    ai: Arc<kumite_sim::ai::AiData>,
    sound: Option<kumite_game::sound::SoundData>,
    /// The boss-change scene's data (scenes.json).
    boss_change: Option<kumite_game::scene::BossChangeData>,
    /// The attract loop: its state, the embu's data and the one running, the demo fight's P1 list (character ids).
    attract: Attract,
    /// The records screen comes next (after a result screen or the demo), and its music (a new record's BGM 2).
    open_records: bool,
    records_music: Option<usize>,
    /// EXIT was chosen: steps left until the game closes (its sound plays out first).
    quit_in: Option<u32>,
    /// The Theater's music player: the BGM id and column playing.
    theater_music: Option<(usize, usize)>,
    /// The staff roll's BGM once started.
    roll_music: Option<usize>,
    /// The character id of a lost 1P session under CONTINUE? (counted as played at GAME OVER).
    continue_player: Option<usize>,
    embu_data: Option<Arc<kumite_game::embu::EmbuData>>,
    embu: Option<EmbuRun>,
    attract_demo_list: Vec<usize>,
    /// The attract demo's CPU level (attract.json `demo_fight.ai_level`).
    attract_demo_level: usize,
    /// The attract loop's steps (attract.json `cycle.steps`).
    attract_steps: Vec<serde_json::Value>,
    /// In a browser the renderer is created asynchronously and lands here.
    #[cfg(target_arch = "wasm32")]
    pending_renderer: std::rc::Rc<std::cell::RefCell<Option<Renderer>>>,
    /// Hit sparks, dust, shakes, the hit light.
    vfx: Option<kumite_game::vfx::Vfx>,
    /// The pads' vibration (the original's patterns, gamedata/rumble.json), and whether a fight was on last step
    /// (leaving it stops everything, as 0x80028F5C at a mode change).
    rumble: kumite_game::rumble::Rumble,
    rumble_fight: bool,
    /// The tag brawl's stage when a headless check names one (`--screen tag --stage N`).
    tag_stage: Option<usize>,
    /// `--hash-log N`: the fight's state hash logged every N fight steps (the determinism check).
    hash_every: Option<u64>,
    /// Each stage's light for the fighters (stages.json `light`: the original's table 0x80096C78).
    stage_lights: std::collections::HashMap<usize, (Option<kumite_render::StageLight>, Option<kumite_render::Shadow>, Option<[u8; 3]>)>,
    /// Testing: the round's clock (seconds) and the rounds to win, for every fight.
    round_time: Option<u32>,
    rounds: Option<u32>,
    /// `--ai`: the seats the AI plays offline (1P..4P), every match (shortcut: one started from the menus too,
    /// for the program's run; a demo or test starts with `--mode`).
    ai_seats: [Option<replay::AiSeat>; 4],
    hash_steps: u64,
    hash_detail: bool,
    /// Online play (rollback over UDP; the sync test): the fight's frames come from its session.
    net: Option<netplay::NetPlay>,
    /// `--online ROOM`: the room, until the other player is there.
    /// Online: the room (its computers, the host's keys and matches), and the mode it plays now.
    room: Option<room::Room>,
    online_mode: Option<room::OnlineMode>,
    /// Online: the frames left on the setup screen (shown everywhere; the host's running out finishes it).
    setup_clock: Option<u32>,
    /// Force's bosses as loaded (by model record).
    actor_cache: std::cell::RefCell<HashMap<usize, kumite_game::Actor>>,
    /// The signalling server for rooms (`--signal`, else the default).
    signal: String,
    /// ONLINE's screen as it was when the match began: where the match (or a disconnect) comes back to.
    online_back: Option<Box<onlinemenu::OnlineMenu>>,
    /// Online: the other player met (the link, this side's player, both nicknames, the tally); the VS character
    /// select and the matches run over it until one leaves.
    /// `--mode`: what to start once the renderer is there.
    pending_launch: Option<launch::LaunchSpec>,
    /// `--replay ID|latest`: a replay to watch once the renderer is there.
    pending_replay: Option<String>,
    /// A fight (or a tag match) waiting for its files to arrive (a browser), started once they have.
    waiting: Option<Waiting>,
    gamepads: Option<gamepad::Gamepads>,
    audio: Option<audio::Audio>,
    /// Free-camera drag: last cursor position while the left button is held.
    drag: Option<(f64, f64)>,
    cursor: (f64, f64),
    /// The free-camera drag pans (right button) instead of orbiting.
    drag_pan: bool,
    last: Instant,
    lag: Duration,
    /// Frames stepped, for the once-a-second status log.
    steps: u64,
    /// Scripted menu keys by frame (`--keys "30:start,60:b1"`), and the frame to quit at (`--quit-after`).
    key_script: Vec<(u64, usize, Option<Key>)>,
    quit_after: Option<u64>,
    /// `--play "40:2:common_162"`: start a move (by animation name) for a player at a fight step.
    play_script: Vec<(u64, usize, String)>,
    frames: u64,
    /// Logic steps since launch (the demo clock).
    logic_steps: u64,
    /// The session recorded as a demo (`--debug` / `--record-demo`), where `--record-demo` saves it, and a demo
    /// being played (`--demo`).
    demo_rec: Option<demo::DemoFile>,
    /// The match being recorded as a replay (its inputs are its bout's tape): what made it.
    replay_rec: Option<ReplayRec>,
    /// A replay about to load: `load` builds its match from the replay's parameters, not the mode's.
    replay_next: Option<Box<(replay::Entry, Arc<[kumite_game::bout::StepInput]>)>>,
    /// A replay being watched.
    replay_view: Option<Box<replay_view::View>>,
    /// The camera counters before the replay about to load (put back after it).
    replay_counters_before: Option<[usize; 3]>,
    /// A Force replay's players joining (the step before which, who), and the next one to come.
    replay_joins: Vec<(u64, u8)>,
    replay_join_next: usize,
    /// Joins replayed out of sight (the run-through, a seek): without their sound.
    replay_quiet: bool,
    /// The tag stage before a tag brawl's replay set its own (put back after it).
    replay_tag_stage: Option<Option<usize>>,
    /// The replays, open over the main menu.
    replays: Option<replay_view::Browser>,
    /// Keys are the game's again from then (a system dialog closed with one).
    keys_after: Option<Instant>,
    /// A replay to make a video of: its options being asked, and the export running (or just done).
    #[cfg(not(target_arch = "wasm32"))]
    video_dialog: Option<video_export::Dialog>,
    #[cfg(not(target_arch = "wasm32"))]
    video_export: Option<video_export::Export>,
    demo_out: Option<std::path::PathBuf>,
    demo_play: Option<demo::Playback>,
    /// Rewind buffer: the fight after each step (`--debug`), newest last; and the quick-saved fight.
    history: std::collections::VecDeque<Bout>,
    saved: Option<(Bout, [String; 2])>,
}

/// Fight states kept for rewinding (20 s).
const REWIND_FRAMES: usize = 1200;

impl App {
    fn game(&self) -> Option<&Game> {
        self.fight.as_ref().map(|f| &f.bout.game)
    }

    /// Load characters + stage and build the bout (sandbox when `setup` is None).
    fn load(&mut self, r: &Renderer, setup: Option<FightSetup>, ladder: Vec<usize>) -> Result<()> {
        self.history.clear();
        let setup_partners = setup.as_ref().and_then(|s| s.partners);
        let (models, stage) = match &setup {
            Some(s) => {
                let m = |k: usize| {
                    let (c, costume) = s.players[k];
                    let ch = &self.roster.characters[c];
                    ch.costumes[costume.min(ch.costumes.len() - 1)]
                };
                ([m(0), m(1)], s.stage)
            }
            None => (self.models, self.stage),
        };
        let t = Instant::now();
        let mut game = Game::load(&self.assets, models)?;
        game.handicap = setup.as_ref().map_or([100; 2], |s| s.handicap);
        if setup.is_none() {
            if let Some(v) = self.intro {
                game = game.with_intro(v, true);
            }
        }
        let mut scene = self.stage_scene(r, stage);
        game.fight.walls = scene.walls.clone();
        scene.models = skinned_models(r, &game);
        // Ball 2 ON 2: the four drawn as Force's actors (a GPU model per actor's body)
        if setup.as_ref().is_some_and(|s| s.partners.is_some()) {
            scene.models.clear();
            scene.force_models = Some(Default::default());
        }
        let names: [String; 2] = std::array::from_fn(|i| match &setup {
            Some(s) => self.roster.characters[s.players[i].0].name.clone(),
            None => game.bodies[i].character.name.split(" (").next().unwrap_or("?").to_string(),
        });
        // the run's rules: rounds, clock, full health, the health carried over (0x800516CC, rec10 0x800DA910)
        let mut rounds = self.roster.modes.rounds_to_win;
        let run = self.run.as_ref().filter(|r| setup.as_ref().is_some_and(|s| s.mode == r.mode()));
        if let Some(run) = run {
            let (r, clock, max) = run.rules(&self.roster.modes);
            rounds = r;
            game.max_health = [Some(max); 2];
            game.start_health = run.start_health;
            if let Some(c) = clock {
                game.round_frames = c;
            }
        }
        // Ball: the ball, the clock off (round time option 5), 130 health
        if let (Some(s), Some(data)) = (setup.as_ref().filter(|s| s.mode == Mode::Ball), self.roster.ball.clone()) {
            let (kind, server) = s.ball.unwrap_or((0, 0));
            game.ball = Some(kumite_game::BallSetup { data, kind, server, rounds: 0 });
            game.round_frames = u32::MAX / 2;
            // 2 ON 2: each side's partner (actor 3 beside P1, actor 2 beside P2), on the court every round
            if let Some(partners) = s.partners {
                let record = |p: (usize, usize)| {
                    let ch = &self.roster.characters[p.0.min(self.roster.characters.len() - 1)];
                    ch.costumes[p.1.min(ch.costumes.len() - 1)]
                };
                let partner = kumite_game::Actor::load(&self.assets, record(partners.partners[0]))?;
                let extra = kumite_game::Actor::load(&self.assets, record(partners.partners[1]))?;
                game.ball_team = Some([partner, extra]);
                game.ball_team_human = [partners.seats[3].is_some(), partners.seats[2].is_some()];
            }
        }
        // the K.O. collapse (ctx+0xB): set by Arcade, VS, Team, Time Attack and Survival; practice, the demo and
        // Ball fall through the air table
        game.ko_collapse = !matches!(setup.as_ref().map(|s| s.mode), Some(Mode::Practice | Mode::Watch | Mode::Ball));
        game.fight.ko_collapse = game.ko_collapse;
        // testing: `--round-time SECONDS`, `--rounds N` (not in an online room: each computer has its own command
        // line, and the match must be the same match on all)
        let testing = self.room.is_none();
        if let Some(t) = self.round_time.filter(|_| testing) {
            game.round_frames = t * 60;
        }
        let mut rounds = self.rounds.filter(|_| testing).unwrap_or(rounds);
        // a replay: the match as it was recorded (its parameters, not the mode's now), the camera counters as then
        let replaying = self.replay_next.as_ref().and_then(|n| match &n.0.file.game {
            replay::Match::Fight { params, .. } => Some(*params),
            _ => None,
        });
        if let Some(p) = replaying {
            (rounds, game.round_frames, game.max_health, game.start_health) = (p.rounds, p.round_frames, p.max_health, p.start_health);
            kumite_game::bout::set_counters(p.counters);
        }
        let (counters, round_frames, max_health, start_health) = (kumite_game::bout::counters(), game.round_frames, game.max_health, game.start_health);
        let mut bout = Bout::new(game, self.ai.clone(), rounds, setup.is_some());
        match setup.as_ref().map(|s| s.mode) {
            None => bout = bout.free_fight(),
            Some(Mode::Arcade) => {
                let Some(s) = setup.as_ref() else { anyhow::bail!("the mode's setup is missing") };
                bout.set_cpu(1, Some((s.difficulty, s.level)));
                // the last stage's boss turns into the one it becomes after the human's first round win (0x80051204)
                bout.interlude_check = self.boss_change.as_ref().is_some_and(|d| s.level == d.trigger.stage_index && self.roster.characters.get(s.players[1].0).is_some_and(|c| c.id != d.becomes));
            }
            Some(Mode::Versus) => {}
            Some(Mode::ExtraLevels) => {
                let Some(s) = setup.as_ref() else { anyhow::bail!("the mode's setup is missing") };
                bout.set_cpu(1, Some((s.difficulty, s.level)));
            }
            Some(Mode::TimeAttack | Mode::Survival) => {
                let Some(s) = setup.as_ref() else { anyhow::bail!("the mode's setup is missing") };
                bout.set_cpu(1, Some((s.difficulty, s.level)));
                bout.interlude_check = s.mode == Mode::TimeAttack
                    && self.boss_change.as_ref().is_some_and(|d| s.level == d.trigger.stage_index && self.roster.characters.get(s.players[1].0).is_some_and(|c| c.id != d.becomes));
            }
            Some(Mode::Force) => {}
            Some(Mode::Ball) => {
                let Some(s) = setup.as_ref() else { anyhow::bail!("the mode's setup is missing") };
                for k in 0..2 {
                    if self.ball_cpu[k] {
                        bout.set_cpu(k, Some((s.difficulty, s.level)));
                    }
                }
                // 2 ON 2: a partner nobody plays is the CPU's
                if let Some(d) = s.partners {
                    bout.partner_cpu = d.seats[3].is_none().then(|| kumite_sim::ai::Ai::new(s.difficulty, s.level, 0));
                    bout.extra_cpu = d.seats[2].is_none().then(|| kumite_sim::ai::Ai::new(s.difficulty, s.level, 1));
                }
            }
            Some(Mode::TeamBattle) => {
                let Some(s) = setup.as_ref() else { anyhow::bail!("the mode's setup is missing") };
                let humans = self.run.as_ref().map_or([true, false], |r| r.humans());
                for (k, human) in humans.iter().enumerate() {
                    if !human {
                        bout.set_cpu(k, Some((s.difficulty, s.level)));
                    }
                }
            }
            Some(Mode::Watch) => {
                let Some(s) = setup.as_ref() else { anyhow::bail!("the mode's setup is missing") };
                bout.set_cpu(0, Some((s.difficulty, s.level)));
                bout.set_cpu(1, Some((s.difficulty, s.level)));
            }
            Some(Mode::Practice) => {
                bout = bout.free_fight();
            }
        }
        if let (Some(p), Some(s)) = (replaying, setup.as_ref()) {
            for k in 0..2 {
                bout.set_cpu(k, p.cpu[k].then_some((s.difficulty, s.level)));
            }
            bout.interlude_check = p.interlude_check;
        }
        // the AI's seats (`--ai`, or the replay's): 1P, 2P, Ball 2 ON 2's partners (3P beside 1P, 4P beside 2P)
        if !matches!(setup.as_ref().map(|s| s.mode), None | Some(Mode::Watch | Mode::Practice)) {
            let seats = self.ai_seats();
            for (k, a) in seats.iter().take(2).enumerate() {
                if let Some(a) = a {
                    bout.set_cpu(k, Some((a.difficulty, a.level)));
                    bout.cpu[k] = Some(a.ai(k));
                }
            }
            if setup.as_ref().is_some_and(|s| s.partners.is_some()) {
                bout.partner_cpu = seats[2].map(|a| a.ai(0)).or(bout.partner_cpu.take());
                bout.extra_cpu = seats[3].map(|a| a.ai(1)).or(bout.extra_cpu.take());
            }
        }
        // every match of a mode with rounds is recorded (its inputs on the bout's tape), saved as a replay at its end
        let recordable = matches!(setup.as_ref().map(|s| s.mode), Some(Mode::Arcade | Mode::Versus | Mode::TeamBattle | Mode::TimeAttack | Mode::Survival | Mode::Ball | Mode::ExtraLevels));
        self.replay_rec = None;
        if let (None, true, Some(s)) = (replaying, recordable, setup.as_ref()) {
            bout.tape = Some(Default::default());
            let params = replay::MatchParams { rounds, round_frames, max_health, start_health, cpu: [bout.cpu[0].is_some(), bout.cpu[1].is_some()], interlude_check: bout.interlude_check, counters };
            let game = replay::Match::Fight { setup: s.clone(), params, ladder: ladder.clone() };
            self.replay_rec = Some(ReplayRec { game, online: self.net.is_some() || self.room.is_some() });
        }
        log::info!("{:?} fight {} vs {} on stage {stage} loaded in {:.0} ms", setup.as_ref().map(|s| s.mode), names[0], names[1], t.elapsed().as_secs_f32() * 1000.0);
        let banks = models.map(|m| format!("m{m:03}"));
        let characters = models.map(|m| self.roster.characters.iter().find(|c| c.costumes.contains(&m)).map_or(0, |c| c.id));
        let voices = std::array::from_fn(|i| self.roster.voice_of.get(&models[i]).copied().unwrap_or(characters[i]));
        // fight music follows the character (BGM ids 7..27): the opponent's; the attract demo plays P1's
        let watch = setup.as_ref().is_some_and(|s| s.mode == Mode::Watch);
        // Team Battle: the music of the side whose member came in
        let incoming = match self.run.as_ref().map(|r| &r.kind) {
            Some(modes::Kind::Team(t)) => t.incoming,
            _ => 1,
        };
        // (the selection's BGM id: its descriptor's, 0x8004F2C0)
        if let (Some(audio), Some(sound)) = (self.audio.as_mut(), self.sound.as_ref()) {
            let k = if watch { 0 } else { incoming };
            let bgm = self.roster.bgm_of.get(&models[k]).or(self.roster.bgm_of.get(&models[0])).copied();
            audio.music(bgm.and_then(|b| sound.music(b)).or_else(|| sound.music_for(&names[k])));
        }
        if watch {
            // the attract demo's fighters have 125 health (0x800516CC)
            for f in &mut bout.game.fight.fighters {
                f.max_health = 125;
                f.health = 125 * kumite_sim::ONE;
            }
        }
        self.debug.infinite_health = setup.as_ref().is_some_and(|s| s.mode == Mode::Practice) || (setup.is_none() && self.debug.infinite_health);
        self.scene = Some(scene);
        self.vfx = Some(kumite_game::vfx::Vfx::new(&self.assets));
        let mode = setup.as_ref().map(|s| s.mode);
        let info = HudInfo {
            models,
            costumes: models.map(|m| self.roster.costume_of.get(&m).copied().unwrap_or(0)),
            names: names.clone(),
            humans: [bout.cpu[0].is_none() && mode != Some(Mode::Watch), bout.cpu[1].is_none() && mode != Some(Mode::Practice)],
            demo: mode == Some(Mode::Watch),
            hidden: mode == Some(Mode::Practice),
            stage: (mode == Some(Mode::Arcade)).then(|| setup.as_ref().map_or(1, |s| s.level as u32 + 1)),
            ..Default::default()
        };
        let mut info = info;
        if let Some(run) = self.run.as_ref().filter(|r| mode == Some(r.mode())) {
            self.mode_hud(&mut info, run);
        }
        if mode == Some(Mode::Ball) {
            // BALL on the human's side, the challenger invite on the CPU's (0x800B4D4C)
            let label = hud::Under::Label(self.roster.modes.ball_label.clone(), self.roster.modes.label_colour);
            info.under = match self.ball_cpu {
                [false, false] => [label, hud::Under::None],
                [false, true] => [label, hud::Under::Push],
                [true, false] => [hud::Under::Push, label],
                _ => Default::default(),
            };
        }
        self.hud.reset(&info);
        let practice = (mode == Some(Mode::Practice)).then(|| practice::Practice::new(practice::PracticeData::load(&self.roster.screens)));
        // the copying character (its voice row: sound.json's) picked with down held as the fight loads is wooden
        // (0x8002AA3C: pad bit 0x4000)
        let copy_row = self.sound.as_ref().and_then(|s| s.copy_row());
        let last_moves = kumite_game::sound::SoundState { wooden: (0..2).map(|i| Some(voices[i]) == copy_row && self.down_held(i)).collect(), ..Default::default() };
        if last_moves.wooden.iter().any(|&w| w) {
            log::info!("a wooden copying character");
        }
        self.fight = Some(Fight {
            bout,
            setup,
            ladder,
            names,
            info,
            pause: None,
            rules: false,
            practice,
            heard: Heard { last_moves, ..Default::default() },
            banks,
            characters,
            voices,
            cutscene: None,
            partner: None,
            extra_voice: None,
            seats: None,
            secondary: Default::default(),
        });
        // Ball 2 ON 2: the partners' voices
        if let (Some(f), Some(partners)) = (self.fight.as_mut(), setup_partners) {
            let voice = |p: (usize, usize)| {
                let c = &self.roster.characters[p.0.min(self.roster.characters.len() - 1)];
                let model = c.costumes[p.1.min(c.costumes.len() - 1)];
                (format!("m{model:03}"), self.roster.voice_of.get(&model).copied().unwrap_or(c.id))
            };
            (f.partner, f.extra_voice) = (Some(voice(partners.partners[0])), Some(voice(partners.partners[1])));
            f.seats = Some(partners.seats);
        }
        self.begin_replay();
        self.screen = None;
        Ok(())
    }

    /// A replay's match just loaded (a fight, a tag brawl, a Force stage): its inputs play it, and it is run
    /// through once first (the seek copies, the rounds, the check).
    fn begin_replay(&mut self) {
        let Some(next) = self.replay_next.take() else { return };
        let (entry, inputs) = *next;
        if let Some(t) = self.tag.as_mut() {
            t.game.play(inputs);
        } else if let Some(f) = self.fight.as_mut() {
            f.bout.play(inputs);
        } else {
            return;
        }
        let before = self.replay_counters_before.take().unwrap_or_else(kumite_game::bout::counters);
        self.replay_view = Some(Box::new(replay_view::View::new(entry, before)));
        self.replay_join_next = 0;
        let t = Instant::now();
        self.replay_prepare();
        log::info!("replay run through in {:.0} ms", t.elapsed().as_secs_f32() * 1000.0);
    }

    /// The match being recorded was changed by a cheat or a debug tool: its inputs no longer make it, so it is not
    /// saved as a replay.
    fn spoil_replay(&mut self, why: &str) {
        if self.replay_rec.take().is_some() {
            log::info!("this match is not saved as a replay: {why}");
        }
    }

    /// The AI's seats of the match loading: a replay's, or `--ai`'s (offline).
    fn ai_seats(&self) -> [Option<replay::AiSeat>; 4] {
        match &self.replay_next {
            Some(next) => next.0.file.ai,
            None if self.room.is_none() && self.net.is_none() => self.ai_seats,
            None => [None; 4],
        }
    }

    /// A replay at its end: it stops there (paused), checked once against the recorded state.
    fn replay_ended(&mut self) {
        let hash = self.match_hash();
        if let Some(v) = self.replay_view.as_mut() {
            if !v.ended {
                v.ended = true;
                v.watched_ok = Some(hash == v.file.hash);
                log::info!("replay {}: watched to its end, the state as recorded: {:?}", v.id, v.watched_ok);
            }
        }
        self.debug.paused = true;
    }

    /// The replay's match as it is now (a seek's copy).
    fn replay_snapshot(&self) -> Option<replay_view::Snapshot> {
        use replay_view::Snapshot as S;
        if let Some(t) = self.tag.as_ref() {
            return Some(S::Tag(Box::new(t.game.clone()), Box::new(t.hud.clone())));
        }
        let f = self.fight.as_ref()?;
        let force = self.force.as_ref().filter(|_| self.force_fight()).map(|run| Box::new(run.clone()));
        Some(S::Fight { bout: Box::new(f.bout.clone()), force, hud: self.hud.state.clone(), heard: Box::new(f.heard.clone()) })
    }

    fn replay_restore(&mut self, snap: replay_view::Snapshot) {
        use replay_view::Snapshot as S;
        match snap {
            S::Tag(g, hud) => {
                if let Some(t) = self.tag.as_mut() {
                    (t.game, t.hud) = (*g, *hud);
                }
            }
            S::Fight { bout, force, hud, heard } => {
                if let Some(f) = self.fight.as_mut() {
                    (f.bout, f.heard) = (*bout, *heard);
                }
                if let Some(run) = force {
                    self.force = Some(*run);
                }
                self.hud.state = hud;
            }
        }
        // (the joins still to come: from this step on)
        let at = self.replay_played() as u64;
        self.replay_join_next = replay::joins_to_come(&self.replay_joins, at);
    }

    /// The steps the replay has played.
    fn replay_played(&self) -> usize {
        match (self.tag.as_ref(), self.fight.as_ref()) {
            (Some(t), _) => t.game.played(),
            (None, Some(f)) => f.bout.played(),
            _ => 0,
        }
    }

    /// The replay's match is at its end (where it was recorded to).
    fn replay_finished(&self) -> bool {
        match (self.tag.as_ref(), self.fight.as_ref()) {
            (Some(t), _) => t.game.finished() && t.game.phase_frame >= TAG_SAVED_AFTER,
            (None, Some(f)) => f.bout.finished(),
            _ => true,
        }
    }

    /// The state hash a replay is checked by: the bout's or the tag brawl's.
    fn match_hash(&self) -> u64 {
        match (self.tag.as_ref(), self.fight.as_ref()) {
            (Some(t), _) => t.game.state_hash(),
            (None, Some(f)) => f.bout.game.state_hash(),
            _ => 0,
        }
    }

    /// The round on: the bout's, the tag brawl's.
    fn match_round(&self) -> u32 {
        match (self.tag.as_ref(), self.fight.as_ref()) {
            (Some(t), _) => t.game.round,
            (None, Some(f)) => f.bout.round,
            _ => 0,
        }
    }

    /// A Force replay's players joining before this step.
    fn replay_joins_due(&mut self) {
        let at = self.replay_played() as u64;
        while let Some(&(step, who)) = self.replay_joins.get(self.replay_join_next) {
            if !replay::join_due((step, who), at) {
                break;
            }
            self.replay_join_next += 1;
            match who {
                1 => self.join_force_partner(),
                p => self.join_force_player(usize::from(p)),
            }
        }
    }

    /// One step of the replay's match, its sounds not played and without effects (the run-through, a seek): the
    /// fight's or the tag brawl's, a Force stage's logic after it, then the HUD's (and the sounds' log).
    fn replay_sim_step(&mut self) {
        if let Some(t) = self.tag.as_mut() {
            t.game.step([Pad::default(); 4]);
            t.hud.step(&t.game, self.hud.data.bar_w());
            return;
        }
        self.replay_quiet = true;
        self.replay_joins_due();
        self.replay_quiet = false;
        let Some(f) = self.fight.as_mut() else { return };
        f.bout.step([Pad::default(); 2]);
        if self.force_fight() {
            let (Some(run), Some(f)) = (self.force.as_mut(), self.fight.as_mut()) else { return };
            let ctx = ForceCtx::of(run);
            if let Some(st) = run.stage.as_mut() {
                let _ = force_sim(&self.roster, &self.assets, &self.actor_cache, &ctx, &mut f.bout, st, false);
            }
        }
        if let Some(f) = self.fight.as_mut() {
            if let Some(sound) = self.sound.as_ref() {
                f.step_sounds(sound);
            }
            self.hud.step(&f.bout);
        }
    }

    /// The run-through: the match played to its end once, a copy kept every so often and the rounds' starts
    /// noted, its end checked against the recorded state; then back to its start.
    fn replay_prepare(&mut self) {
        let Some(total) = self.replay_view.as_ref().map(|v| v.total) else { return };
        let Some(start) = self.replay_snapshot() else { return };
        let (mut snaps, mut rounds, mut round) = (vec![(0, start.clone())], Vec::new(), 0);
        for step in 0..total {
            if step > 0 && step % replay_view::SNAPSHOT_EVERY == 0 {
                if let Some(s) = self.replay_snapshot() {
                    snaps.push((step, s));
                }
            }
            self.replay_sim_step();
            let r = self.match_round();
            if r != round {
                round = r;
                rounds.push((step, r));
            }
        }
        let verified = self.replay_view.as_ref().is_some_and(|v| self.match_hash() == v.file.hash);
        if !verified {
            log::warn!("the replay's run-through ended in another state than recorded");
        }
        self.replay_restore(start);
        if let Some(v) = self.replay_view.as_mut() {
            (v.snapshots, v.rounds, v.verified) = (snaps, rounds, verified);
        }
    }

    /// A stage's meshes and the effect atlas (no fighters yet).
    fn stage_scene(&self, r: &Renderer, stage: usize) -> Scene {
        self.stage_scene_with(r, stage, &|p| p)
    }

    /// The stage's scene, its points placed by `place` (metres; Force's extra stages lay a level along their
    /// route).
    fn stage_scene_with(&self, r: &Renderer, stage: usize, place: &dyn Fn(glam::Vec3) -> glam::Vec3) -> Scene {
        let mut scene = Scene {
            sky: vec![],
            sky_base_y: 0.0,
            sky_y_scale: 1.0,
            sky_first: false,
            background: [0; 3],
            strip: None,
            camera_drum: false,
            floor_patch: None,
            floor_light: None,
            floor_period: None,
            statics: vec![],
            models: vec![],
            force_models: None,
            effects: effect_atlas(r, &self.assets.root.join("pictures")),
            faces_shown: Default::default(),
            walls: None,
            camera_walls: None,
            cutaway: false,
            near_cut: None,
            light: self.stage_lights.get(&stage).and_then(|l| l.0),
            shadow: self.stage_lights.get(&stage).and_then(|l| l.1),
            blob: None,
        };
        match self.assets.stage(stage) {
            Ok(st) => {
                let tex: Vec<_> = st
                    .textures
                    .iter()
                    .enumerate()
                    .map(|(i, t)| if st.filtered.get(i).copied().unwrap_or(false) { r.texture_filtered(t.width, t.height, &t.rgba) } else { r.texture(t.width, t.height, &t.rgba) })
                    .collect();
                let walls = |w: &kumite_assets::StageWalls| Arc::new(kumite_sim::Walls { origin: w.origin, cell: w.cell, size: w.size, rows: w.rows.clone() });
                (scene.walls, scene.camera_walls) = (st.walls.as_ref().map(walls), st.camera_walls.as_ref().map(walls));
                let conv =
                    |m: &kumite_assets::Mesh| -> Vec<StaticVertex> { m.vertices.iter().map(|v| StaticVertex { pos: place(glam::Vec3::from(v.pos)).to_array(), uv: v.uv, color: v.color }).collect() };
                scene.sky = st.sky.iter().map(|m| r.sorted_mesh(&conv(m), &m.indices, tex[m.texture].clone())).collect();
                (scene.sky_base_y, scene.sky_y_scale, scene.sky_first, scene.floor_period) = (st.sky_base_y, st.sky_y_scale, st.sky_first, st.floor_period);
                scene.background = st.background;
                (scene.camera_drum, scene.floor_patch, scene.floor_light) = (st.camera_drum, st.floor_patch, st.floor_light);
                if let Some((m, at)) = st.strip.as_ref().filter(|(m, _)| !m.indices.is_empty()) {
                    scene.sky = vec![r.sorted_mesh(&conv(m), &m.indices, tex[m.texture].clone())];
                    scene.strip = Some(*at);
                }
                scene.statics = st.meshes.iter().map(|m| r.static_mesh(&conv(m), &m.indices, tex[m.texture].clone())).collect();
                log::info!("stage {stage}: {}", st.name);
            }
            Err(e) => {
                log::warn!("stage: {e:#}; using a checker floor");
                scene.statics.push(checker_floor(r));
            }
        }
        scene
    }

    /// A menu is open (a screen, a pause, practice's menu, a match's end): the keys move its cursor, not a fighter.
    fn in_menu(&self) -> bool {
        // (a replay playing and the replays' list: their controls are the menu keys)
        self.screen.is_some()
            || self.replays.is_some()
            || self.replay_view.is_some()
            || self.tag.as_ref().is_some_and(|t| t.pause.is_some())
            || self.fight.as_ref().is_some_and(|f| f.pause.is_some() || f.practice.as_ref().is_some_and(|p| p.open) || matches!(f.bout.phase, Phase::MatchOver { .. }))
    }

    /// The soft reset (0x8005101C): START held with SELECT pressed (nothing else held but L1 / R1) leaves a fight,
    /// a select screen or OPTION for the main menu, the music and sounds stopped. True when it did.
    fn soft_reset(&mut self) -> bool {
        let here = self.fight.is_some()
            || self.tag.is_some()
            || matches!(self.screen, Some(Screen::Select(_) | Screen::Grid(_) | Screen::Tag(_) | Screen::Options { .. } | Screen::Graphics(_) | Screen::KeyConfig(_) | Screen::Keyboard(_)));
        if !here {
            return false;
        }
        // online: a reset leaves the room (the others are told; the room would otherwise go on without this one)
        if self.room.is_some() {
            self.leave_online("");
            return true;
        }
        let difficulty = match (self.fight.as_ref().and_then(|f| f.setup.as_ref()), self.screen.as_ref()) {
            (Some(s), _) => s.difficulty,
            (None, Some(Screen::Select(s))) => s.difficulty,
            (None, Some(Screen::Grid(g))) => g.difficulty,
            (None, Some(Screen::Options { difficulty, .. })) => *difficulty,
            (None, Some(Screen::Graphics(g))) => g.difficulty,
            (None, Some(Screen::KeyConfig(k))) => k.difficulty,
            (None, Some(Screen::Keyboard(k))) => k.difficulty,
            _ => 1,
        };
        log::info!("soft reset: the main menu");
        // (a session without a room, `--net-local`: over too)
        self.net = None;
        if let Some(a) = self.audio.as_mut() {
            a.music(None);
        }
        self.fight = None;
        self.tag = None;
        self.scene = None;
        self.run = None;
        self.rumble.stop();
        self.screen = Some(Screen::menu(difficulty));
        true
    }

    /// Player `p` holds down now: a key of its keyboard layout (one player alone: either), or its gamepad's d-pad.
    fn down_held(&self, p: usize) -> bool {
        let layouts: &[usize] = if p == 0 { &[0, 1] } else { &[1] };
        let keys = layouts.iter().any(|&l| self.records.controls.keyboard[l].keys_of(controls::Command::Down).iter().any(|k| self.keys.contains(k)));
        let pad = self.gamepads.as_ref().and_then(|g| g.held()[p].clone()).is_some_and(|h| h.contains(&controls::PadButton::Down));
        keys || pad
    }

    /// The pads' vibration, a 60 Hz tick (0x800291D8, before the pads are read): each player's motors to their
    /// gamepad, when that side is a human's (0x800AE1F8) and its VIBRATION is on (+0x29); in the menus only
    /// KEY CONFIGURATION's test plays.
    fn rumble_tick(&mut self) {
        let fight = self.fight.is_some() || self.tag.is_some();
        if self.rumble_fight && !fight {
            self.rumble.stop();
        }
        self.rumble_fight = fight;
        let humans = match self.fight.as_ref() {
            Some(f) => {
                let mode = f.setup.as_ref().map(|s| s.mode);
                [f.bout.cpu[0].is_none() && mode != Some(Mode::Watch), f.bout.cpu[1].is_none() && !matches!(mode, Some(Mode::Practice | Mode::Watch))]
            }
            None => [true, true],
        };
        let out = self.rumble.tick();
        let Some(g) = self.gamepads.as_mut() else { return };
        for p in 0..2 {
            let on = humans[p] && self.records.controls.pads[p].vibration;
            let (small, large) = if on { out[p] } else { (0.0, 0) };
            g.rumble(p, large, small);
        }
    }

    /// The keyboard's pads (KEY CONFIG's layouts): one player alone has both, two players one each.
    fn keyboard_pads(&self, two_players: bool) -> [Pad; 2] {
        self.records.controls.keyboard_pads(two_players, |k| self.keys.contains(&k) || self.tapped.contains(&k))
    }

    /// The players' pads: keyboard (VS uses the two-player layouts) merged with their gamepads (players 3 and 4:
    /// gamepads 3 and 4).
    fn pads(&mut self) -> [Pad; 4] {
        let coop = self.fight.as_ref().is_some_and(|f| f.partner.is_some());
        // (online: two players at this computer split the keyboard too)
        let two_here = self.room.as_ref().is_some_and(|r| r.mine().len() > 1);
        let versus = coop || two_here || self.fight.as_ref().and_then(|f| f.setup.as_ref()).is_some_and(|s| s.mode == Mode::Versus);
        let kb = self.keyboard_pads(versus);
        let pads = self.gamepads.as_mut().map_or([None; 4], |g| g.pads4(&self.records.controls));
        std::array::from_fn(|i| {
            let k = kb.get(i).copied().unwrap_or_default();
            pads[i].map_or(k, |p| controls::merge(k, p))
        })
    }

    /// One fixed 60 Hz logic step.
    fn step(&mut self) {
        // a save that failed (a full disk, the browser's storage full): the player is told, once each time
        if let Some(why) = save::take_failure() {
            self.debug.say(why);
        }
        // a fight whose files were still arriving: started once all are here
        if let Some(w) = self.waiting.take() {
            match w {
                Waiting::Fight(setup, ladder) => self.load_fight(*setup, ladder),
                Waiting::Tag(setup) => self.start_tag(*setup),
            }
        }
        // online: meeting the other player, then the link: the VS select's keys, the match's start, leaving
        if self.renderer.is_some() {
            self.step_online();
        }
        if self.renderer.is_some() {
            if let Some(id) = self.pending_replay.take() {
                let all = replay::list();
                match all.into_iter().find(|e| e.id == id || id == "latest") {
                    Some(e) => self.play_replay(e),
                    None => log::error!("--replay {id}: no such replay"),
                }
            }
            if let Some(spec) = self.pending_launch.take() {
                self.launch(spec);
            }
        }
        if let Some(n) = self.quit_in.as_mut() {
            *n = n.saturating_sub(1);
        }
        self.rumble_tick();
        let step_no = self.logic_steps;
        self.logic_steps += 1;
        // a Force replay: its players joining before this step
        if self.replay_view.is_some() && self.force_fight() {
            self.replay_joins_due();
        }
        // a demo: the keys and cheats recorded before this step
        if let Some(pb) = self.demo_play.as_mut() {
            let events = pb.events(step_no);
            let done = pb.finished(step_no);
            for e in events {
                self.apply_event(e);
            }
            if done {
                self.demo_play = None;
                self.debug.say("demo finished: the controls are yours");
            }
        }
        self.debug.demo_status = match (&self.demo_play, &self.demo_rec) {
            (Some(pb), _) => format!("playing demo: step {step_no} / {}", pb.file.len()),
            (None, Some(d)) => format!("recording: {} steps", d.len()),
            (None, None) => "demo: not recording".into(),
        };
        // the menus' sounds queued since the last step
        for code in menu::sfx::take() {
            if let (Some(sound), Some(audio)) = (self.sound.as_ref(), self.audio.as_mut()) {
                if let Some(s) = sound.system(code) {
                    audio.sfx(&s.file, s.volume);
                }
            }
        }
        // the records' name entry: left / right held (either pad) for the letter's auto-repeat
        let held = if matches!(self.screen, Some(Screen::Records(_))) {
            let p = self.pads();
            [p[0].left || p[1].left, p[0].right || p[1].right]
        } else {
            [false; 2]
        };
        if let Some(screen) = &mut self.screen {
            // the continue countdown is spoken: "continue?", the seconds, then the end
            if let (Screen::Continue { countdown, .. }, Some(sound), Some(audio)) = (&*screen, self.sound.as_ref(), self.audio.as_mut()) {
                use kumite_game::sound::Call;
                let step = menu::CONTINUE_STEP;
                let call = match *countdown {
                    menu::CONTINUE_FRAMES => Some(Call::ContinueStart),
                    c if c % step == 0 && c > 0 && c < menu::CONTINUE_FRAMES - step => Some(Call::Count(c / step)),
                    1 => Some(Call::ContinueZero),
                    _ => None,
                };
                if let Some(s) = call.and_then(|c| sound.call(c)) {
                    audio.sfx(&s.file, s.volume);
                }
            }
            // the result screens: a tick per row / count, the final line, then the records
            if let Screen::Results(r) = screen {
                match r.tick() {
                    modes::ResultEvent::None => {}
                    modes::ResultEvent::Tick => menu::sfx::play(self.roster.modes.result_sfx.0),
                    modes::ResultEvent::Banner | modes::ResultEvent::Voice => menu::sfx::play(self.roster.modes.result_sfx.1),
                    modes::ResultEvent::Done => {
                        self.run = None;
                        self.open_records = true;
                    }
                }
            }
            if let Screen::Records(r) = screen {
                match r.tick(&mut self.records, &self.roster, held) {
                    records::Event::None => {}
                    records::Event::FadeMusic(_) => self.records_music = None,
                    records::Event::Done => {
                        self.records.save();
                        self.records_music = None;
                        self.request_title();
                        return;
                    }
                    records::Event::Menu => {
                        self.records.save();
                        self.records_music = None;
                        self.scene = None;
                        menu::sfx::play(menu::sfx::start());
                        self.screen = Some(Screen::menu(menu::difficulty()));
                        return;
                    }
                }
            }
            if let Screen::StaffRoll(r) = screen {
                match r.tick() {
                    theater::RollEvent::None => {}
                    theater::RollEvent::Sfx(code) => {
                        if let (Some(sound), Some(audio)) = (self.sound.as_ref(), self.audio.as_mut()) {
                            if let Some(f) = sound.file(code, "r301") {
                                audio.sfx(f, 1.0);
                            }
                        }
                    }
                    theater::RollEvent::Music(id) => self.roll_music = Some(id),
                    theater::RollEvent::Done => {
                        self.roll_music = None;
                        self.open_records = true;
                    }
                }
            }
            if self.open_records {
                self.open_records = false;
                self.start_records();
                return;
            }
            let Some(screen) = &mut self.screen else { return };
            if let Screen::ForceResults(r) = screen {
                let (sfx, done) = r.tick(self.roster.modes.force_result_sfx);
                if let Some(code) = sfx {
                    menu::sfx::play(code);
                }
                if done {
                    *screen = Screen::title();
                }
            }
            let was_playing = matches!(screen, Screen::Movie(m, _) if m.playing);
            // (the main menu waits while the replays are open over it: no idle return to the title)
            let outcome = if self.replays.is_some() && matches!(screen, Screen::Menu(_)) { Outcome::None } else { screen.tick(&self.roster, &mut self.seed) };
            // a movie's soundtrack starts with its pictures (once enough of it has arrived)
            if let (Screen::Movie(m, _), false, Some(audio)) = (&*screen, was_playing, self.audio.as_mut()) {
                if m.playing {
                    audio.music(m.audio.as_deref().map(|p| (p, m.looped)));
                }
            }
            if matches!(screen, Screen::GameOver { .. }) {
                if let Some(id) = self.continue_player.take() {
                    self.records.played(id);
                    self.records.save();
                }
            }
            // the VS session's tally goes with the fight the grid select starts
            if let Screen::Grid(g) = &*screen {
                self.vs_record = g.record;
            }
            // CONTINUE? and GAME OVER show over the loser shot, which keeps running; other screens end the fight
            if matches!(screen, Screen::Continue { .. } | Screen::GameOver { .. }) {
                if let Some(f) = self.fight.as_mut() {
                    f.bout.step([Pad::default(); 2]);
                }
            } else if self.fight.is_some() || self.tag.is_some() {
                self.fight = None;
                self.tag = None;
                self.scene = None;
            }
            self.tapped.clear();
            // menu music (traced): none on the title, the main menu and OPTION (record 10 starts no BGM, and its
            // load stops what played: the attract demo's BGM 0), the select screen BGM 1 (0x80110034)
            if let (Some(audio), Some(sound)) = (self.audio.as_mut(), self.sound.as_ref()) {
                let id = match screen {
                    Screen::Select(_) | Screen::Grid(_) | Screen::Tag(_) => Some(1),
                    Screen::Results(r) => Some(r.music()),
                    Screen::ForceResults(_) => Some(6),
                    _ => None,
                };
                match screen {
                    // a movie's own soundtrack, once (a looping ending's, over and over)
                    Screen::Movie(m, _) => audio.music(m.audio.as_deref().filter(|_| m.playing).map(|p| (p, m.looped))),
                    // the Theater's music player (the menu itself is silent)
                    Screen::Theater(_) => audio.music(self.theater_music.and_then(|(id, col)| sound.music_column(id, col))),
                    // the staff roll's music once it started
                    Screen::StaffRoll(_) => audio.music(self.roll_music.and_then(|id| sound.music(id))),
                    // the records: a new record's BGM 2 until its fade; else whatever played goes on
                    Screen::Records(r) => {
                        if r.entry.is_some() || self.records_music.is_some() {
                            audio.music(self.records_music.and_then(|i| sound.music(i)));
                        }
                    }
                    _ => audio.music(id.and_then(|i| sound.music(i))),
                }
            }
            self.outcome(outcome);
            return;
        }
        if self.embu.is_some() {
            self.tapped.clear();
            self.step_embu();
            return;
        }
        if self.fight.as_ref().is_some_and(|f| f.bout.phase == Phase::Interlude) {
            self.tapped.clear();
            self.step_cutscene();
            return;
        }
        if self.tag.is_some() {
            self.step_tag();
            return;
        }
        let [kb1, kb2, kb3, kb4] = self.pads();
        // (the scripts' pads go to fight steps only: none is lost while the fight is still loading)
        let fighting = self.fight.is_some();
        let p1 = if self.script.is_empty() || !fighting { kb1 } else { self.script.remove(0) };
        let p2_script = (!self.p2_script.is_empty() && fighting).then(|| self.p2_script.remove(0));
        self.tapped.clear();
        let Some(fight) = self.fight.as_mut() else { return };
        if let Some(audio) = self.audio.as_mut() {
            audio.duck(fight.pause.is_some());
        }
        if fight.pause.is_some() {
            return;
        }
        // practice: its menu pauses the fight (and opens by itself on the first fighting frame)
        if let Some(p) = fight.practice.as_mut() {
            p.step(fight.bout.phase == Phase::Fight);
            if let Some(audio) = self.audio.as_mut() {
                audio.duck(p.open);
            }
            if p.open {
                return;
            }
        }
        let mode = fight.setup.as_ref().map(|s| s.mode);
        // P2: the second keyboard player in VS, a script, the debug dummy modes (sandbox / practice), or the CPU
        let p2_human = mode == Some(Mode::Versus) || (matches!(mode, Some(Mode::TeamBattle | Mode::Ball)) && fight.bout.cpu[1].is_none());
        let mut pads = self.debug.pads(p1, p2_script.or(p2_human.then_some(kb2)));
        if mode == Some(Mode::Practice) {
            fight.bout.game.fight.clock = kumite_sim::ROUND_FRAMES;
        }
        if matches!(mode, Some(Mode::Arcade | Mode::Watch | Mode::TimeAttack | Mode::Survival | Mode::Force | Mode::ExtraLevels)) && p2_script.is_none() {
            pads[1] = Pad::default(); // the CPU decides
        }
        if matches!(mode, Some(Mode::TeamBattle | Mode::Ball)) && fight.bout.cpu[1].is_some() && p2_script.is_none() {
            pads[1] = Pad::default();
        }
        // the practice dummy: its TRAINING DUMMY action, the second pad (CONTROLLER) or the CPU (VS CPU)
        if let (Some(p), None) = (fight.practice.as_ref(), p2_script) {
            let f = &fight.bout.game.fight;
            let low = f.fighters[0].current_move(&f.data).attack_mask.mask == 0x10F;
            let d = &f.fighters[1];
            let down = p.data.inactive_moveset.is_none_or(|m| d.moveset.moveset_id != Some(m)) && d.next_is_lying(&f.data);
            pads[1] = if p.dummy_on_pad2() {
                kb2
            } else if p.cpu().is_some() {
                Pad::default()
            } else {
                p.dummy_pad(f.facing_right(1), low, down)
            };
        }
        // Force's co-op partner: the second player's pad (or the P2 script); 3P and 4P theirs
        if fight.partner.is_some() {
            fight.bout.partner_pad = p2_script.unwrap_or(kb2);
        }
        if mode == Some(Mode::Force) {
            for (k, pad) in [kb3, kb4].into_iter().enumerate() {
                let joined = fight.bout.game.more_body.get(k).is_some_and(Option::is_some);
                if let Some(slot) = fight.bout.more_human.get_mut(k) {
                    *slot = joined.then_some(pad);
                }
            }
        }
        // Ball 2 ON 2: each actor its player's pad (none: the CPU's)
        if let Some(seats) = fight.seats {
            let by = |p: Option<usize>| p.map(|p| [p1, p2_script.unwrap_or(kb2), kb3, kb4][p.min(3)]);
            pads = [by(seats[0]).unwrap_or_default(), by(seats[1]).unwrap_or_default()];
            fight.bout.partner_pad = by(seats[3]).unwrap_or_default();
            fight.bout.extra_human = by(seats[2]);
        }
        if let Some(pb) = self.demo_play.as_mut() {
            pads = pb.pads(step_no);
        }
        if let Some(d) = self.demo_rec.as_mut() {
            d.push_pads(step_no, pads);
        }
        let t = Instant::now();
        // online: the session's frames (none while connecting or waiting; several after a rollback: only a new
        // frame goes on to the sounds and effects below)
        // (Force online: the stage's events for the eyes and ears, from the frame shown)
        let mut force_shown = Vec::new();
        if let (Some(net), Some(room)) = (self.net.as_mut(), self.room.as_mut()) {
            netplay_ready(net, room);
        }
        if let Some(net) = self.net.as_mut() {
            // this computer's players' pads in (its first's, its second's); every player's out, to the fight's places
            // (BALL 2 ON 2: by its seats)
            // (a START press stays until a frame has run with it: a tick that waits would lose it; held longer, the
            // fight still takes it once)
            let start = self.net_start;
            let locals = [Pad { start: start[0], ..p1 }, Pad { start: start[1], ..kb2 }];
            let seats = fight.seats;
            let mut step = |bout: &mut Bout, all: &[Pad]| {
                // any player's START skips (BALL 2 ON 2's partner seats too): on the first side's pad, the same
                // on every computer
                let start = all.iter().any(|p| p.start);
                let by = |p: Option<usize>| p.and_then(|p| all.get(p).copied());
                let first = |p: Pad| Pad { start, ..p };
                match seats {
                    Some(s) => {
                        bout.partner_pad = by(s[3]).unwrap_or_default();
                        bout.extra_human = by(s[2]);
                        bout.step([first(by(s[0]).unwrap_or_default()), by(s[1]).unwrap_or_default()]);
                    }
                    None => bout.step([first(by(Some(0)).unwrap_or_default()), by(Some(1)).unwrap_or_default()]),
                }
            };
            let requests = net.tick(&locals[..net.local.len().min(2)]);
            let (advanced, fresh) = match (mode, self.force.as_mut().and_then(|r| r.stage.take().map(|st| (ForceCtx::of(r), st)))) {
                // Force: the fight and its stage together (P1 the player, P2 the partner, 3P and 4P), the
                // stage's events (the bosses) inside the rollback
                (Some(Mode::Force), Some((ctx, st))) => {
                    let mut state = netplay::ForceState(fight.bout.clone(), st);
                    let (roster, assets, cache) = (&self.roster, &self.assets, &self.actor_cache);
                    let mut last = Vec::new();
                    let r = netplay::apply(net, &mut state, requests, &mut |s: &mut netplay::ForceState, all: &[Pad]| {
                        let at = |p: usize| all.get(p).copied().unwrap_or_default();
                        s.0.partner_pad = at(1);
                        for k in 0..2 {
                            if let Some(slot) = s.0.more_human.get_mut(k) {
                                *slot = Some(at(2 + k));
                            }
                        }
                        // (the partner's START skips too: the step's second pad, START alone)
                        s.0.step([at(0), Pad { start: at(1).start, ..Pad::default() }]);
                        last = force_sim(roster, assets, cache, &ctx, &mut s.0, &mut s.1, false);
                    });
                    fight.bout = state.0;
                    if let Some(run) = self.force.as_mut() {
                        run.stage = Some(state.1);
                        if r.0 > 0 && r.1 {
                            run.step_hud(&fight.bout.game);
                        }
                    }
                    if r.0 > 0 && r.1 {
                        force_shown = last;
                    }
                    r
                }
                (_, back) => {
                    if let (Some(run), Some((_, st))) = (self.force.as_mut(), back) {
                        run.stage = Some(st);
                    }
                    netplay::apply(net, &mut fight.bout, requests, &mut step)
                }
            };
            // the match is over (a player left, the connection was lost, ...): why, for 3 seconds, then the room saying
            // so
            if net.gone.is_some_and(|g| g >= 180) {
                if let Some(room) = self.room.as_ref() {
                    let why = room.why_over(&net.status);
                    self.online_room(Some(why));
                } else {
                    self.net = None;
                }
                return;
            }
            if advanced > 0 {
                self.net_start = [false; 2];
            }
            if advanced == 0 || !fresh {
                return;
            }
        } else {
            fight.bout.step(pads);
        }
        self.debug.step_ms = t.elapsed().as_secs_f32() * 1000.0;
        // the run's clock: fighting frames only (0x8003CCEC)
        if fight.bout.phase == Phase::Fight {
            if let Some(r) = self.run.as_mut() {
                r.frames += 1;
            }
        }
        if mode == Some(Mode::Force) {
            if self.net.is_some() {
                self.force_shown(force_shown);
            } else {
                self.step_force();
            }
        }
        let Some(fight) = self.fight.as_mut() else { return };
        // Survival lost: GAME OVER over the human's bar
        if mode == Some(Mode::Survival) && matches!(fight.bout.phase, Phase::Result { winner } | Phase::MatchOver { winner } if winner != Some(0)) {
            let (t, c, dx) = self.roster.modes.game_over.clone();
            fight.info.under[0] = hud::Under::GameOver(t, c, dx);
        }
        // scripted moves (--play): own set first, then the common set (not online: one computer's fight only)
        while self.net.is_none() && self.play_script.first().is_some_and(|p| p.0 <= self.steps + 1) {
            let (_, player, anim) = self.play_script.remove(0);
            let g = &mut fight.bout.game.fight;
            let f = &g.fighters[player];
            let own = f.moveset.moves.iter().position(|m| m.anim == anim).map(|index| kumite_sim::Current { bank: kumite_sim::data::Bank::Own, index });
            let to = own.or_else(|| g.data.common.moves.iter().position(|m| m.anim == anim).map(|index| kumite_sim::Current { bank: kumite_sim::data::Bank::Common, index }));
            match to {
                Some(to) => g.fighters[player].play(to),
                None => log::warn!("--play: no move with animation {anim}"),
            }
        }
        // (the debug's god mode and infinite health: not online, one computer's fight only)
        if self.net.is_none() {
            self.debug.after_step(&mut fight.bout.game);
        }
        if let Some(n) = self.hash_every.filter(|&n| n > 0) {
            self.hash_steps += 1;
            if self.hash_steps.is_multiple_of(n) {
                log::info!("HASH {} {:016x}", self.hash_steps, fight.bout.game.state_hash());
                // (--hash-detail: what goes into it, to find the first field that differs)
                if self.hash_detail {
                    for (k, x) in fight.bout.game.fight.fighters.iter().enumerate() {
                        let b = |v: f32| v.to_bits();
                        log::info!(
                            "HASHD {} P{k} pos {:08x} {:08x} {:08x} yaw {:08x} root {:08x} off {:08x} {:08x} {:08x} hp {} move {:?} f {} rooty {:08x} stun {} juggle {} clock {} round {:?} fframe {}",
                            self.hash_steps,
                            b(x.position.x),
                            b(x.position.y),
                            b(x.position.z),
                            b(x.yaw),
                            b(x.root_yaw),
                            b(x.offset.x),
                            b(x.offset.y),
                            b(x.offset.z),
                            x.health,
                            x.current,
                            x.frame,
                            b(x.anim_root_y),
                            x.stun,
                            x.juggle,
                            fight.bout.game.fight.clock,
                            fight.bout.game.fight.state,
                            fight.bout.game.fight.frame
                        );
                    }
                }
            }
        }
        if self.debug.enabled {
            self.history.push_back(fight.bout.clone());
            if self.history.len() > REWIND_FRAMES {
                self.history.pop_front();
            }
            self.debug.history_len = self.history.len();
        }
        self.hud.step(&fight.bout);
        if let Some(p) = fight.practice.as_mut() {
            // a side acts on its own again (its own moves, not a reaction) once its combo is over
            let neutral = std::array::from_fn(|i| fight.bout.game.fight.fighters[i].current.bank == kumite_sim::data::Bank::Own);
            p.hits(&fight.bout.game.fight.hits, neutral);
        }
        if let Some(v) = self.vfx.as_mut() {
            v.step(&fight.bout.game);
        }
        // the pads' vibration: not in a replay or the attract demo (0x800954A4, battle type 6)
        if fight.bout.replay_frame().is_none() && mode != Some(Mode::Watch) {
            let g = &fight.bout.game.fight;
            self.rumble.fight(g, &g.data, &[(&g.fighters[0], 0, 1), (&g.fighters[1], 1, 0)]);
        }
        if let Some(sound) = &self.sound {
            let mut sounds = fight.step_sounds(sound);
            // the announcer: round calls, results, the match winner (YOU WIN if a human won)
            let humans = [fight.bout.cpu[0].is_none(), fight.bout.cpu[1].is_none()];
            sounds.extend(fight.bout.events.iter().filter_map(|&e| sound.announce(e, humans)));
            if let Some(audio) = self.audio.as_mut() {
                for s in sounds {
                    audio.sfx(&s.file, s.volume);
                }
            }
        }
        self.steps += 1;
        let f = &fight.bout.game.fight;
        let hp = |i: usize| f.fighters[i].health / kumite_sim::ONE;
        let status = format!("P1 {} | {:>2}s | {} P2  {:?}", hp(0), f.clock / 60, hp(1), fight.bout.phase);
        if let Some(w) = &self.window {
            let paused = if self.debug.paused { " [paused]" } else { "" };
            w.set_title(&format!("OpenKumite — {} vs {} — {status} — {:.0} fps{paused}", fight.names[0], fight.names[1], self.debug.fps()));
        }
        if self.steps.is_multiple_of(60) {
            let (a, fr) = f.fighters[0].animation(&f.data);
            log::info!("{status}; P1 {a}@{fr} at {:.0}", f.fighters[0].root().x);
        }
        // the attract demo ends 90 frames after round 1's K.O. or time-up: no replay, no win pose (0x80050924);
        // the records screens are not done, so the attract loop goes on
        let demo_over = fight.setup.as_ref().is_some_and(|s| s.mode == Mode::Watch) && matches!(fight.bout.phase, Phase::Replay { .. } | Phase::Result { .. } | Phase::MatchOver { .. });
        if demo_over {
            if let Some(a) = self.audio.as_mut() {
                a.music(None);
            }
            // the records screen (mode 16), unless the demo's step (the one before) skips them (attract.json
            // `records_after_demo`, 0x80050E6C)
            let n = self.attract_steps.len().max(1);
            let before = self.attract_steps.get((self.attract.step as usize + n - 1) % n);
            if before.is_none_or(|s| s["records_after_demo"] != false) {
                self.start_records();
            } else {
                self.request_title();
            }
            return;
        }
        if fight.bout.finished() {
            // a replay ends on its last frame (the viewer stays); a match played is saved as one, then goes on
            if self.replay_view.is_some() {
                self.replay_ended();
                return;
            }
            self.save_replay();
            self.match_over();
        }
    }

    /// The match just over, saved as a replay (its tape and what made it): a fight, a tag brawl, a Force stage.
    fn save_replay(&mut self) {
        let Some(rec) = self.replay_rec.take() else { return };
        let name = |c: usize| self.roster.characters.get(c).map_or_else(|| "?".to_string(), |c| c.name.clone());
        let (tape, title, result) = if let Some(t) = self.tag.as_mut() {
            let Some(tape) = t.game.tape.take() else { return };
            let s = t.setup.match_slots();
            let names: Vec<String> = s.iter().map(|p| name(p.character)).collect();
            let title = if t.game.teams == [0, 0, 1, 1] { format!("TAG BRAWL · {} & {} vs {} & {}", names[0], names[1], names[2], names[3]) } else { format!("TAG BRAWL · {}", names.join(", ")) };
            let winner = match t.game.phase {
                kumite_game::tag::TagPhase::MatchOver { winner } => winner,
                _ => None,
            };
            let score = t.game.wins.iter().map(u32::to_string).collect::<Vec<_>>().join("–");
            let result = winner.map_or_else(|| format!("DRAW {score}"), |w| format!("TEAM {} WINS {score}", w + 1));
            (tape, title, result)
        } else if let Some(f) = self.fight.as_mut() {
            let Some(tape) = f.bout.tape.take() else { return };
            let winner = match f.bout.phase {
                Phase::MatchOver { winner } => winner,
                _ => None,
            };
            match &rec.game {
                replay::Match::Force { start, .. } => {
                    let number = self.force.as_ref().and_then(|r| r.card().0);
                    let stage = number.map_or_else(|| "FINAL STAGE".to_string(), |n| format!("STAGE {}", n + 1));
                    (tape, format!("FORCE · {} · {}", name(start.player.0), stage), if winner == Some(0) { "STAGE CLEAR".into() } else { "GAME OVER".into() })
                }
                replay::Match::Fight { setup, .. } => {
                    let mode = match setup.mode {
                        Mode::Arcade => "ARCADE",
                        Mode::Versus => "VS",
                        Mode::TeamBattle => "TEAM BATTLE",
                        Mode::TimeAttack => "TIME ATTACK",
                        Mode::Survival => "SURVIVAL",
                        Mode::Ball => "BALL",
                        Mode::ExtraLevels => "EXTRA LEVELS",
                        _ => "MATCH",
                    };
                    let online = if rec.online { "ONLINE " } else { "" };
                    let score = format!("{}–{}", f.bout.wins[0], f.bout.wins[1]);
                    let result = winner.map_or_else(|| format!("DRAW {score}"), |w| format!("{} WINS {score}", f.names[w.min(1)]));
                    (tape, format!("{online}{mode} · {} vs {}", f.names[0], f.names[1]), result)
                }
                replay::Match::Tag { .. } => return,
            }
        } else {
            return;
        };
        let inputs = tape.to_vec();
        let file = replay::ReplayFile {
            version: replay::VERSION,
            engine: env!("CARGO_PKG_VERSION").into(),
            data: self.assets.fingerprint(),
            recorded: replay::now(),
            title,
            result,
            online: rec.online,
            kept: false,
            game: rec.game,
            steps: inputs.len() as u64,
            hash: self.match_hash(),
            inputs: replay::encode(&inputs),
            ai: if rec.online { [None; 4] } else { self.ai_seats },
        };
        match replay::save(&file) {
            Ok(id) => log::info!("replay {id} saved: {} ({} steps)", file.title, file.steps),
            Err(e) => log::warn!("the replay was not saved: {e:#}"),
        }
    }

    /// Watch a replay: its match loaded from what made it (a fight, a tag brawl, a Force stage), its inputs playing
    /// it.
    fn play_replay(&mut self, entry: replay::Entry) {
        // (checked here, whatever asked: the list, `--replay`, a video)
        let why = if !entry.file.plays_on(&self.assets.fingerprint()) {
            Some("it was recorded on other game data or another version")
        } else if !entry.file.sane(&self.roster) {
            Some("the file is damaged")
        } else {
            None
        };
        if let Some(why) = why {
            let said = format!("This replay cannot play here: {why}.");
            log::warn!("replay {}: {said}", entry.id);
            match self.replays.as_mut() {
                Some(b) => b.message = Some(said),
                None => self.debug.say(said),
            }
            return;
        }
        let inputs: Arc<[kumite_game::bout::StepInput]> = replay::decode(&entry.file.inputs).into();
        let game = entry.file.game.clone();
        self.replays = None;
        self.replay_counters_before = Some(kumite_game::bout::counters());
        self.replay_next = Some(Box::new((entry, inputs)));
        (self.debug.paused, self.debug.speed, self.debug.free_cam) = (false, 1.0, None);
        self.replay_joins.clear();
        match game {
            replay::Match::Fight { setup, ladder, .. } => self.load_fight(setup, ladder),
            replay::Match::Tag { setup, stage } => {
                self.replay_tag_stage = Some(self.tag_stage);
                self.tag_stage = Some(stage);
                self.start_tag(setup);
            }
            replay::Match::Force { setup, start, joins } => {
                let Some(data) = self.force_data.clone() else {
                    self.replay_next = None;
                    self.debug.say("this game data has no Force");
                    return;
                };
                self.force = Some(forcerun::ForceRun::from_start(data, &start));
                self.replay_joins = joins;
                self.load_fight(setup, Vec::new());
            }
        }
    }

    /// What the replays' list asked for.
    fn browser_action(&mut self, action: Option<replay_view::BrowserAction>) {
        use replay_view::BrowserAction as A;
        // (after a system dialog: the key that closed it, and its release, stay out of the game)
        match action {
            Some(A::Play(e)) => self.play_replay(*e),
            Some(A::Close) => self.replays = None,
            Some(A::Export(e)) => {
                let saved = replay::to_bytes(&e.file).and_then(|b| files::save(&replay::file_name(&e.file), &files::REPLAY, &b));
                self.keys_after = Some(Instant::now() + Duration::from_millis(400));
                let said = match saved {
                    Ok(Some(to)) => format!("Exported: {to}"),
                    Ok(None) => return,
                    Err(err) => format!("Not exported: {err:#}"),
                };
                if let Some(b) = self.replays.as_mut() {
                    b.message = Some(said);
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            Some(A::Video(e)) => self.video_dialog = Some(video_export::Dialog::new(*e, true, None)),
            #[cfg(target_arch = "wasm32")]
            Some(A::Video(_)) => {}
            Some(A::Import) => {
                let opened = files::open(&files::REPLAY);
                self.keys_after = Some(Instant::now() + Duration::from_millis(400));
                if !opened.is_empty() {
                    self.import_replays(opened);
                }
            }
            None => {}
        }
    }

    /// The video's options answered (save it: where, then the export), and its panel's buttons.
    #[cfg(not(target_arch = "wasm32"))]
    fn video_actions(&mut self, dialog: Option<video_export::DialogAction>, status: Option<video_export::StatusAction>) {
        match dialog {
            Some(video_export::DialogAction::Export) => {
                let Some(d) = self.video_dialog.take() else { return };
                if self.video_export.as_ref().is_some_and(video_export::Export::running) {
                    self.debug.say("a video is being made: one at a time");
                    return;
                }
                let name = replay::file_name(&d.entry.file).replace(".okreplay", ".mp4");
                let out = files::save_path(&name, &files::VIDEO);
                self.keys_after = Some(Instant::now() + Duration::from_millis(400));
                let Some(out) = out else { return };
                match video_export::Export::start(&d, out, &self.assets.root) {
                    Ok(x) => self.video_export = Some(x),
                    Err(e) => self.debug.say(format!("the video could not start: {e:#}")),
                }
            }
            Some(video_export::DialogAction::Cancel) => self.video_dialog = None,
            None => {}
        }
        match status {
            Some(video_export::StatusAction::Cancel) => {
                if let Some(mut x) = self.video_export.take() {
                    x.cancel();
                }
            }
            Some(video_export::StatusAction::Show) => {
                if let Some(x) = self.video_export.as_ref() {
                    x.show();
                }
            }
            Some(video_export::StatusAction::Close) => self.video_export = None,
            None => {}
        }
    }

    /// Replay files someone shared (opened, or dropped on the window): imported, the list open with the last.
    fn import_replays(&mut self, opened: Vec<(String, Vec<u8>)>) {
        let (mut last, mut said) = (None, Vec::new());
        for (name, bytes) in opened {
            match replay::import(&bytes) {
                Ok(id) => {
                    said.push(format!("Imported {name}"));
                    last = Some(id);
                }
                Err(e) => said.push(format!("{name}: {e:#}")),
            }
        }
        if self.replays.is_none() && self.replay_view.is_none() && (self.screen.is_some() || self.fight.is_none()) {
            self.replays = Some(replay_view::Browser::new(self.assets.fingerprint(), None));
        }
        if let Some(b) = self.replays.as_mut() {
            b.reload(last.as_deref(), said.join(" · "));
        } else {
            self.debug.say(said.join(" · "));
        }
    }

    /// Leave the replay: back to the replays over the main menu.
    fn exit_replay(&mut self) {
        let Some(v) = self.replay_view.take() else { return };
        kumite_game::bout::set_counters(v.counters_before);
        (self.debug.paused, self.debug.speed, self.debug.free_cam) = (false, 1.0, None);
        (self.fight, self.scene, self.tag, self.force) = (None, None, None, None);
        if let Some(stage) = self.replay_tag_stage.take() {
            self.tag_stage = stage;
        }
        self.replay_joins.clear();
        if let Some(vfx) = self.vfx.as_mut() {
            vfx.clear();
        }
        if let Some(a) = self.audio.as_mut() {
            a.music(None);
        }
        self.screen = Some(Screen::menu(menu::difficulty()));
        self.replays = Some(replay_view::Browser::new(self.assets.fingerprint(), Some(&v.id)));
    }

    /// Seek the replay to `step`: from the copy before it, run on to it (the sounds and effects of the steps
    /// passed are not played).
    fn replay_seek(&mut self, step: usize) {
        let at = self.replay_played();
        let Some(v) = self.replay_view.as_mut() else { return };
        let step = step.min(v.total);
        if step < v.total {
            v.ended = false;
        }
        let from = (step < at || step - at > replay_view::SNAPSHOT_EVERY).then(|| v.snapshot_before(step).map(|s| s.1.clone())).flatten();
        if let Some(snap) = from {
            self.replay_restore(snap);
        }
        while self.replay_played() < step && !self.replay_finished() {
            self.replay_sim_step();
        }
        if let Some(vfx) = self.vfx.as_mut() {
            vfx.clear();
        }
        self.lag = Duration::ZERO;
    }

    /// What the replay viewer asked for this frame.
    fn replay_requests(&mut self) {
        let Some(v) = self.replay_view.as_mut() else { return };
        for r in std::mem::take(&mut v.requests) {
            match r {
                replay_view::Request::Seek(s) => self.replay_seek(s),
                replay_view::Request::TogglePause => {
                    // at the end: from the start again
                    if self.replay_finished() {
                        self.replay_seek(0);
                        self.debug.paused = false;
                    } else {
                        self.debug.paused = !self.debug.paused;
                    }
                }
                replay_view::Request::Step => {
                    self.debug.paused = true;
                    self.debug.step_frames += 1;
                }
                replay_view::Request::Speed(x) => self.debug.speed = x,
                replay_view::Request::FreeCam => {
                    let from = self.game().map(|g| -kumite_game::view_camera(&g.camera.view).1);
                    self.debug.toggle_free_cam(from);
                }
                replay_view::Request::Keep(kept) => {
                    if let Some(v) = self.replay_view.as_mut() {
                        match replay::set_kept(&v.id, kept) {
                            Ok(()) => v.file.kept = kept,
                            Err(e) => log::warn!("keeping the replay: {e:#}"),
                        }
                    }
                }
                replay_view::Request::Video =>
                {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(v) = self.replay_view.as_ref() {
                        self.debug.paused = true;
                        let entry = replay::Entry { id: v.id.clone(), file: v.file.clone() };
                        self.video_dialog = Some(video_export::Dialog::new(entry, v.hud, self.debug.free_cam));
                    }
                }
                replay_view::Request::Exit => {
                    self.exit_replay();
                    return;
                }
            }
        }
    }

    /// A key while a replay plays (keyboard or pad): START pauses, BACK leaves, left / right seek 5 s, up / down the
    /// speed, button 1 a step.
    fn replay_key(&mut self, key: Key) {
        let at = self.replay_played();
        let Some(v) = self.replay_view.as_mut() else { return };
        let speed = self.debug.speed;
        let next_speed = |up: bool| {
            let i = replay_view::SPEEDS.iter().position(|s| (s - speed).abs() < 1e-3).unwrap_or(3);
            replay_view::SPEEDS[if up { (i + 1).min(replay_view::SPEEDS.len() - 1) } else { i.saturating_sub(1) }]
        };
        let r = match key {
            Key::Start => replay_view::Request::TogglePause,
            Key::Back => replay_view::Request::Exit,
            Key::Left => replay_view::Request::Seek(at.saturating_sub(300)),
            Key::Right => replay_view::Request::Seek(at + 300),
            Key::Up => replay_view::Request::Speed(next_speed(true)),
            Key::Down => replay_view::Request::Speed(next_speed(false)),
            Key::Button(0) => replay_view::Request::Step,
            Key::Button(1) => replay_view::Request::FreeCam,
            Key::Button(2) => {
                v.hud = !v.hud;
                return;
            }
            Key::Button(_) => {
                v.inputs = !v.inputs;
                return;
            }
        };
        v.requests.push(r);
    }

    /// Online: the room's messages and events, each frame.
    fn step_online(&mut self) {
        let Some(room) = self.room.as_mut() else { return };
        let mut events = room.poll();
        // the setup's clock: nobody keeps the others waiting (the host's running out finishes the setup everywhere)
        let on_setup = matches!(self.screen, Some(Screen::Grid(_) | Screen::Tag(_))) && self.fight.is_none() && self.tag.is_none();
        if !on_setup {
            self.setup_clock = None;
        } else {
            let left = self.setup_clock.get_or_insert(room::SETUP_SECONDS * 60);
            *left = left.saturating_sub(1);
            if *left == 0 && room.host() {
                self.setup_clock = Some(u32::MAX);
                room.time_up(&mut events);
            }
        }
        self.room_events(events);
        if let (Some(room), Some(Screen::Room(rs))) = (self.room.as_ref(), self.screen.as_mut()) {
            rs.update(room);
        }
    }

    /// What happened in the room: keys on the room's screens (every computer applies them alike), the host's
    /// matches, the room changing or ending.
    fn room_events(&mut self, events: Vec<room::RoomEvent>) {
        let mut queue: std::collections::VecDeque<room::RoomEvent> = events.into();
        while let Some(e) = queue.pop_front() {
            let Some(room) = self.room.as_mut() else { return };
            match e {
                room::RoomEvent::Key { player, key } => {
                    let players = room.players();
                    let outcome = match self.screen.as_mut() {
                        Some(Screen::Room(rs)) => {
                            if let Some(mode) = rs.key(player, key, players) {
                                self.online_setup(mode);
                            }
                            None
                        }
                        // the two-player select: player k is side k
                        Some(Screen::Grid(g)) if player < 2 => Some(g.key(player, key, &self.roster)),
                        Some(Screen::Tag(t)) => Some(t.key(player, key, &self.roster)),
                        _ => None,
                    };
                    match outcome {
                        Some(Outcome::Quit) => self.online_room(None),
                        o => queue.extend(self.host_start(o)),
                    }
                }
                room::RoomEvent::TimeUp => {
                    let outcome = match self.screen.as_mut() {
                        Some(Screen::Grid(g)) => {
                            g.finish(&self.roster);
                            None
                        }
                        Some(Screen::Tag(t)) => Some(t.finish(&self.roster)),
                        _ => None,
                    };
                    queue.extend(self.host_start(outcome));
                }
                room::RoomEvent::Start { setup, seed, session } if setup.valid(&self.roster) => self.start_room_match(setup, seed, session),
                room::RoomEvent::Start { .. } => {
                    self.leave_online("THE HOST'S MATCH IS NOT VALID");
                    return;
                }
                room::RoomEvent::Members => {
                    // the room's mode cursor back to the top on every computer (the host's list and the keys come in
                    // one order): one seated just now starts there, so every room screen moves alike
                    if let Some(Screen::Room(rs)) = self.screen.as_mut() {
                        rs.cursor = 0;
                    }
                    // a player of the match on left: the match ends, back to the room
                    let players = room.players();
                    if let Some(n) = self.net.as_ref() {
                        if n.players != players {
                            self.online_room(Some("A PLAYER LEFT".into()));
                        }
                    } else if !matches!(self.screen, Some(Screen::Room(_))) && room.members.len() > 1 && self.online_mode.is_some_and(|m| !m.fits(players)) {
                        self.online_room(Some("A PLAYER LEFT".into()));
                    }
                }
                // the host went back to the room: a match this one still plays (alone) is over
                // (a match already over shows its own reason out first; the events after this one still apply)
                room::RoomEvent::Back => {
                    // (also a computer that finished its match first, on the setup screen: the host is in the room)
                    let over_here = self.net.as_ref().is_some_and(|n| n.gone.is_some());
                    if !over_here && !matches!(self.screen, Some(Screen::Room(_))) {
                        self.online_room(Some("THE HOST WENT BACK TO THE ROOM".into()));
                    }
                }
                room::RoomEvent::Left(why) => {
                    self.leave_online(&why);
                    return;
                }
            }
        }
    }

    /// A match over here (not cut short): for [`LINGER`] more its session is polled, no frame run, and its last
    /// inputs are sent again every 100 ms (ggrs's own resend waits for a quiet the others' resends keep breaking: a
    /// lost last packet held them short of the match's end).
    fn linger_net(&mut self) {
        if let Some(n) = self.net.take().filter(|n| n.gone.is_none()) {
            let session = self.room.as_ref().map(|r| r.matches);
            self.net_linger = Some(Linger { net: n, session, since: Instant::now(), sent: None });
        }
    }

    /// Online: the room's screen (after leaving a setup, or a match cut short: `note` why); the host lets players in.
    fn online_room(&mut self, note: Option<String>) {
        self.net = None;
        self.waiting = None;
        self.fight = None;
        self.tag = None;
        self.scene = None;
        self.online_mode = None;
        if let Some(a) = self.audio.as_mut() {
            a.music(None);
        }
        let Some(room) = self.room.as_mut() else {
            // (a session without a room, `--net-local`: its match over, the title)
            self.screen = Some(Screen::title());
            return;
        };
        room.open = true;
        // the guests still in a match (one that went on alone: TAG ARCADE's next pair) come back too
        if room.host() {
            room.back();
        }
        let mut rs = room::RoomScreen { note, ..Default::default() };
        rs.update(room);
        self.screen = Some(Screen::Room(Box::new(rs)));
    }

    /// Online: back to the mode's setup after a run (Force).
    fn online_room_setup(&mut self) {
        self.linger_net();
        self.fight = None;
        self.scene = None;
        if let Some(a) = self.audio.as_mut() {
            a.music(None);
        }
        match self.online_mode {
            Some(mode) => self.online_setup(mode),
            None => self.online_room(None),
        }
    }

    /// Online: the mode's setup screen, on every computer alike (the room closes to newcomers).
    fn online_setup(&mut self, mode: room::OnlineMode) {
        let Some(room) = self.room.as_mut() else { return };
        room.open = false;
        self.online_mode = Some(mode);
        let seed = room.seed;
        let record = room.record;
        self.screen = Some(mode.screen(&self.roster, seed, record));
    }

    /// Online: a setup screen's match, started by the host (its events, for every computer alike).
    fn host_start(&mut self, outcome: Option<Outcome>) -> Vec<room::RoomEvent> {
        let setup = match outcome {
            Some(Outcome::Fight(f, _)) => room::MatchSetup::Fight(f),
            Some(Outcome::StartTag(t)) => room::MatchSetup::Tag(t),
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        if let Some(room) = self.room.as_mut().filter(|r| r.host()) {
            room.start(setup, &self.roster, &mut out);
        }
        out
    }

    /// Online: the match the host started: every computer loads it and joins its rollback session (a player per
    /// room player: this computer's own are local, the others at their computers' seats).
    fn start_room_match(&mut self, setup: room::MatchSetup, seed: u32, session: u32) {
        let Some(room) = self.room.as_mut() else { return };
        // (not seated yet: not in this match)
        if room.seat().is_none() {
            return;
        }
        // (the host's number: a guest seated late counts from it)
        room.matches = session;
        self.net_start = [false; 2];
        kumite_game::bout::reset_camera_counters();
        room.link.borrow_mut().seats = room.members.iter().map(|m| m.token).collect();
        let players = room.players();
        log::info!("online: match {session} ({players} players), seed {seed}");
        self.seed = seed;
        self.screen = None;
        // (a seat or slot given to a player the room no longer has, or never had: the CPU's, alike everywhere)
        let here = |p: Option<usize>| p.filter(|&p| p < players);
        match setup {
            room::MatchSetup::Fight(mut setup) => {
                if let Some(d) = setup.partners.as_mut() {
                    d.seats = d.seats.map(here);
                }
                // BALL: a side nobody plays is the CPU's, the same pick on every computer (from the seed)
                if setup.mode == Mode::Ball {
                    let n = self.roster.characters.len().max(1);
                    self.ball_cpu = match setup.partners {
                        Some(d) => [d.seats[0].is_none(), d.seats[1].is_none()],
                        None => setup.players.map(|p| p.0 == menu::CPU_SIDE),
                    };
                    for (k, p) in setup.players.iter_mut().enumerate() {
                        if p.0 == menu::CPU_SIDE {
                            *p = ((seed as usize >> (4 * k)) % n, 0);
                        }
                    }
                }
                self.load_fight(setup, Vec::new());
            }
            room::MatchSetup::Tag(mut setup) => {
                for s in &mut setup.slots {
                    s.player = here(s.player);
                }
                if setup.mode == tagrun::TagMode::Force {
                    self.start_room_force(&setup);
                } else {
                    self.start_tag(setup);
                }
            }
        }
        // (the match did not load: the room is left, no session for it)
        self.open_room_session();
    }

    /// Online: a Force run for the room's players: 1P's fighter plays the player, 2P's the partner, 3P's and
    /// 4P's beside them (all from the first stage).
    fn start_room_force(&mut self, setup: &tagrun::TagSetup) {
        let players = self.room.as_ref().map_or(1, |r| r.players());
        let pick = |p: usize| setup.slots.iter().find(|s| s.player == Some(p)).map(|s| (s.character, s.costume));
        let p1 = pick(0).unwrap_or((setup.slots[0].character, setup.slots[0].costume));
        self.new_force_run(p1, 1);
        let costume_num = |p: (usize, usize)| {
            let c = &self.roster.characters[p.0];
            let model = c.costumes[p.1.min(c.costumes.len() - 1)];
            self.roster.costume_of.get(&model).copied().unwrap_or(0)
        };
        let partner = (players > 1).then(|| pick(1)).flatten();
        let more: [Option<(usize, usize, usize)>; 2] = std::array::from_fn(|j| (players > 2 + j).then(|| pick(2 + j)).flatten().map(|p| (p.0, p.1, costume_num(p))));
        if let Some(run) = self.force.as_mut() {
            run.partner = partner;
            run.more_players = more;
        }
        self.start_force_stage();
    }

    /// Online: a new rollback session for the next match of a run (TAG ARCADE's next pair), on every computer at the
    /// same point of the match before.
    fn room_session(&mut self) {
        let Some(room) = self.room.as_mut() else { return };
        room.matches = room.matches.wrapping_add(1);
        self.open_room_session();
    }

    /// Online: the rollback session of the room's current match (`room.matches`), a player a room player (this
    /// computer's own local, the others at their computers' seats).
    fn open_room_session(&mut self) {
        let Some(room) = self.room.as_mut() else { return };
        let me = room.seat();
        let players: Vec<Option<netplay::NetAddr>> = (0..room.players()).map(|p| room.owner(p).filter(|&s| Some(s) != me).map(|s| netplay::NetAddr::Seat(s as u8))).collect();
        match netplay::NetPlay::p2p(netlink::LinkSocket::open(room.link.clone(), room.matches), &players) {
            Ok(n) => self.net = Some(n),
            // (no session: the match would run alone here, unchecked; the room is left, saying why)
            // (the host: back to the room, the guests called back; a guest: out of the room, the others told)
            Err(e) => {
                log::error!("online: {e:#}");
                if self.room.as_ref().is_some_and(|r| r.host()) {
                    self.online_room(Some("THE MATCH COULD NOT START".into()));
                } else {
                    self.leave_online("THE MATCH COULD NOT START");
                }
            }
        }
    }

    /// Online: leave the room (BACK on its screen, or the room ended): ONLINE's screen with `why`.
    fn leave_online(&mut self, why: &str) {
        self.net_linger = None;
        if let Some(mut r) = self.room.take() {
            r.leave();
        }
        self.net = None;
        self.waiting = None;
        self.fight = None;
        self.tag = None;
        self.scene = None;
        self.online_mode = None;
        if let Some(a) = self.audio.as_mut() {
            a.music(None);
        }
        self.screen = Some(match self.online_back.take() {
            Some(mut o) => {
                o.waiting = (!why.is_empty()).then(|| why.to_string());
                Screen::Online(o)
            }
            None => Screen::menu(menu::difficulty()),
        });
    }

    /// ONLINE's START: into a room (create or join one by its code, host or join by IP).
    fn start_online(&mut self, setup: onlinemenu::OnlineSetup) {
        self.records.online_name = setup.name.clone();
        self.records.save();
        let link: anyhow::Result<netlink::Link> = match setup.method {
            onlinemenu::Method::CreateRoom => Ok(online::room_link(&self.signal, &setup.code, &setup.name, setup.locals, true)),
            onlinemenu::Method::JoinRoom => Ok(online::room_link(&self.signal, &setup.code, &setup.name, setup.locals, false)),
            #[cfg(not(target_arch = "wasm32"))]
            onlinemenu::Method::HostIp => direct::host(direct::DEFAULT_PORT, &setup.name, setup.locals),
            #[cfg(not(target_arch = "wasm32"))]
            onlinemenu::Method::JoinIp => direct::join(&setup.code, &setup.name, setup.locals),
            #[cfg(target_arch = "wasm32")]
            _ => Err(anyhow::anyhow!("no IP play in a browser")),
        };
        match link {
            Ok(l) => self.enter_room(l),
            Err(e) => {
                if let Some(Screen::Online(o)) = self.screen.as_mut() {
                    o.waiting = Some(format!("{e:#}").to_uppercase());
                }
            }
        }
    }

    /// Into the room over `link`: its screen (ONLINE's comes back when the room ends).
    fn enter_room(&mut self, mut link: netlink::Link) {
        link.assets = self.assets.fingerprint();
        if let Some(Screen::Online(o)) = self.screen.take() {
            let mut o = o;
            o.waiting = None;
            self.online_back = Some(o);
        }
        // (from the command line: ONLINE's screen to come back to all the same)
        if self.online_back.is_none() {
            self.online_back = Some(Box::new(onlinemenu::OnlineMenu::new(self.seed, 1)));
        }
        // (files without a fingerprint cannot be told apart from another computer's: not online)
        if link.assets.is_empty() {
            self.leave_online("RECONVERT THE GAME FILES (KUMITE_CONVERT)");
            return;
        }
        let seed = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(1, |d| d.subsec_nanos() | 1);
        self.room = Some(room::Room::new(link, seed));
        self.online_room(None);
    }

    /// The match is over: next arcade stage, continue, ending, or back to select.
    fn match_over(&mut self) {
        // online Force: a cleared stage waits for its tally (300 frames), then the next stage in a session of
        // its own; the run over (lost or done), back to its setup
        if self.room.is_some() && self.net.is_some() && self.force_fight() {
            let winner = self.fight.as_ref().and_then(|f| match f.bout.phase {
                Phase::MatchOver { winner } => winner,
                _ => None,
            });
            let Some(run) = self.force.as_mut() else { return };
            if winner == Some(0) {
                run.clear_frames += 1;
                if !(run.stage.as_ref().is_some_and(|s| s.tally_done) && run.clear_frames >= 300) {
                    return;
                }
                if let Some(f) = self.fight.as_ref() {
                    run.clock = f.bout.game.fight.clock;
                }
                let more = run.next();
                self.fight = None;
                self.scene = None;
                self.linger_net();
                if more {
                    // (online: the next stage's bout counts its cameras alike on every computer)
                    if self.room.is_some() {
                        kumite_game::bout::reset_camera_counters();
                    }
                    self.start_force_stage();
                    self.room_session();
                    return;
                }
            }
            self.force = None;
            self.online_room_setup();
            return;
        }
        // online: the tally, then back to the mode's setup (the room stays)
        if self.net.is_some() {
            self.linger_net();
            let winner = self.fight.as_ref().and_then(|f| match f.bout.phase {
                Phase::MatchOver { winner } => winner,
                _ => None,
            });
            if let (Some(r), Some(room::OnlineMode::Versus)) = (self.room.as_mut(), self.online_mode) {
                match winner {
                    Some(w) => r.record.wins[w] += 1,
                    None => r.record.draws += 1,
                }
            }
            if let Some(a) = self.audio.as_mut() {
                a.music(None);
            }
            let setup = self.fight.as_ref().and_then(|f| f.setup.clone());
            self.fight = None;
            self.scene = None;
            match (self.online_mode, setup) {
                // BALL 2 ON 2: its four-slot setup again, the same seats
                (Some(room::OnlineMode::Ball2), Some(s)) => self.screen = Some(Screen::Tag(Box::new(tagrun::TagSelect::ball_again(&self.roster, &s)))),
                (Some(mode), _) => self.online_setup(mode),
                _ => self.online_room(None),
            }
            return;
        }
        if self.force.is_some() && self.fight.as_ref().is_some_and(|f| f.setup.as_ref().is_some_and(|s| s.mode == Mode::Force)) {
            let winner = self.fight.as_ref().and_then(|f| match f.bout.phase {
                Phase::MatchOver { winner } => winner,
                _ => None,
            });
            let start = self.tapped.contains(&KeyCode::Enter);
            self.force_over(winner, start);
            return;
        }
        let Some(fight) = self.fight.take() else { return };
        let winner = match fight.bout.phase {
            Phase::MatchOver { winner } => winner,
            _ => None,
        };
        let Some(setup) = fight.setup.clone() else {
            self.fight = Some(fight); // sandbox: keep going
            return;
        };
        // a lost arcade match keeps its scene: CONTINUE? shows over the loser shot
        let continues = matches!(setup.mode, Mode::Arcade | Mode::TimeAttack) && winner != Some(0);
        if !continues {
            self.scene = None;
        }
        let id_of = |roster: &Roster, k: usize| roster.characters.get(k).map_or(0, |c| c.id);
        let ids = [id_of(&self.roster, setup.players[0].0), id_of(&self.roster, setup.players[1].0)];
        if let Some(mut run) = self.run.take().filter(|r| r.mode() == setup.mode) {
            // Team Battle between two players: each match counts for both (0x800B1328 / 0x800B13AC)
            if setup.mode == Mode::TeamBattle && run.humans() == [true, true] {
                self.records.two_player(ids, winner);
            }
            let f = &fight.bout.game.fight;
            let health = [f.fighters[0].health, f.fighters[1].health];
            let max = [f.fighters[0].max_health, f.fighters[1].max_health];
            match run.after(&setup, winner, health, max, &self.roster.modes, &mut self.records, &self.roster) {
                modes::Next::Fight => {
                    let next = run.setup(&self.roster, &self.roster.modes, &mut self.seed, self.frames as u32);
                    self.run = Some(run);
                    self.start_fight(next, Vec::new());
                }
                modes::Next::Continue(s) => {
                    self.continue_player = Some(ids[0]);
                    self.run = Some(run);
                    self.screen = Some(Screen::Continue { setup: s, ladder: Vec::new(), countdown: menu::CONTINUE_FRAMES });
                    self.fight = Some(fight);
                }
                modes::Next::Results(r) => {
                    // the session's character(s) played (TA / Survival: the player; Team: the human sides' leaders)
                    self.continue_player = None;
                    match &r.kind {
                        modes::ResultKind::Team { run: t } => {
                            for team in t.teams.iter().filter(|t| t.human) {
                                if let Some(m) = team.members.first() {
                                    self.records.played(id_of(&self.roster, m.0));
                                }
                            }
                        }
                        _ => self.records.played(ids[0]),
                    }
                    self.records.save();
                    self.screen = Some(Screen::Results(r));
                }
            }
            return;
        }
        match setup.mode {
            Mode::Arcade if winner == Some(0) && setup.level + 1 >= fight.ladder.len() => {
                self.continue_player = None;
                self.records.played(ids[0]);
                self.records.save();
                // the champion's ending movie, then the ending screen
                // the ending flags (0x800B1E88), then the champion's ending movie (0x800FEFD4) and the staff roll
                let id = fight.characters[0];
                let costume = self.roster.costume_of.get(&fight.bout.game.bodies[0].model_record).copied().unwrap_or(0);
                self.screen = Some(match self.roster.theater.clone() {
                    Some(t) => {
                        t.clear(id, costume, &mut self.records.endings, &mut self.records.alternate);
                        if let Some(b) = t.extra_bit(id, costume) {
                            self.records.endings_extra |= 1 << b.min(31);
                        }
                        self.records.save();
                        let m = t
                            .ending(id, costume, self.records.alternate)
                            .and_then(|p| t.params(p))
                            .and_then(|p| movie::Movie::with(&self.assets.root, &p).map_err(|e| log::warn!("ending movie: {e:#}")).ok());
                        match m {
                            Some(m) => Screen::Movie(Box::new(m), menu::AfterMovie::Credits),
                            None => Screen::StaffRoll(Box::new(theater::StaffRoll::new(t))),
                        }
                    }
                    None => Screen::title(),
                });
            }
            Mode::Arcade if winner == Some(0) => {
                let next = menu::arcade_setup(&self.roster, setup.players[0], setup.difficulty, &fight.ladder, setup.level + 1);
                self.start_fight(next, fight.ladder);
            }
            Mode::Arcade => {
                self.continue_player = Some(ids[0]);
                // the loser shot keeps running under CONTINUE?
                self.screen = Some(Screen::Continue { setup, ladder: fight.ladder.clone(), countdown: menu::CONTINUE_FRAMES });
                self.fight = Some(fight);
            }
            // the title's demo goes back to the title
            Mode::Watch => self.screen = Some(Screen::title()),
            // the extra levels: back to its list
            Mode::ExtraLevels => {
                self.records.played(ids[0]);
                self.records.save();
                self.screen = Some(Screen::extra_levels(&self.roster, setup.stage, setup.difficulty));
            }
            // Ball: always back to its select, the ball as chosen (0x800B4860)
            Mode::Ball => {
                for k in (0..2).filter(|&k| fight.bout.cpu[k].is_none()) {
                    self.records.played(ids[k]);
                }
                self.records.save();
                let kind = setup.ball.map_or(0, |b| b.0);
                self.screen = Some(if setup.partners.is_some() {
                    Screen::Tag(Box::new(tagrun::TagSelect::ball_again(&self.roster, &setup)))
                } else {
                    Screen::Grid(Box::new(grid::GridSelect::ball(&self.roster, setup.difficulty, kind)))
                });
            }
            // straight into select for the same mode
            // VS MODE: the tally grows and both pick again on the grid
            Mode::Versus => {
                match winner {
                    Some(w) => self.vs_record.wins[w] += 1,
                    None => self.vs_record.draws += 1,
                }
                // the winner's play (0x800B11B0), the win and the loss, or the draw
                if let Some(w) = winner {
                    self.records.played(ids[w]);
                }
                self.records.two_player(ids, winner);
                self.records.save();
                self.screen = Some(Screen::Grid(Box::new(grid::GridSelect::new(self.roster.grid.clone(), setup.difficulty, self.vs_record))));
            }
            mode => self.screen = Some(Screen::Select(Box::new(menu::Select::new(mode, setup.difficulty, &self.roster, self.seed)))),
        }
    }

    /// The boss-change scene: started when the bout stops for it, one frame per step, then the swap.
    fn step_cutscene(&mut self) {
        if self.fight.as_ref().is_some_and(|f| f.cutscene.is_none()) {
            if let Err(e) = self.start_cutscene() {
                log::error!("the boss-change scene: {e:#}");
                self.finish_cutscene();
            }
            return;
        }
        let Some(cs) = self.fight.as_mut().and_then(|f| f.cutscene.as_mut()) else { return };
        cs.scene.step();
        for e in &cs.scene.events {
            match *e {
                kumite_game::scene::SceneEvent::Music(id) => {
                    if let (Some(audio), Some(sound)) = (self.audio.as_mut(), self.sound.as_ref()) {
                        audio.music(sound.music(id));
                    }
                }
            }
        }
        if cs.scene.done || cs.skip {
            self.finish_cutscene();
        }
    }

    /// Init (0x800B3CA0): the human's slot shows the second-to-last stage's boss's body, the other stays the last
    /// stage's boss.
    fn start_cutscene(&mut self) -> Result<()> {
        let data = self.boss_change.clone().context("no scenes.json")?;
        let f = self.fight.as_ref().context("no fight")?;
        let human = (0..2).find(|&i| f.bout.cpu[i].is_none()).unwrap_or(0);
        // the second-to-last stage's opponents always wear costume 0 here (the arcade ladder's)
        let body = data.body_model(f.characters[human], 0).context("no body model")?;
        let mut models = [0; 2];
        models[human] = body;
        models[1 - human] = f.bout.game.bodies[1 - human].model_record;
        let game = Game::load(&self.assets, models)?;
        let r = self.renderer.as_ref().context("no renderer")?;
        let gpu = skinned_models(r, &game);
        if let Some(v) = self.vfx.as_mut() {
            v.clear();
        }
        let scene = kumite_game::scene::BossChangeScene::new(game, data, human);
        log::info!("boss-change scene at step {}: models {models:?}", self.logic_steps);
        if let Some(f) = self.fight.as_mut() {
            f.cutscene = Some(Cutscene { scene, models: gpu, skip: false });
        }
        Ok(())
    }

    /// After the scene (0x800B2870, 0x800B08F8): the opponent becomes the one scenes.json names, in the same costume,
    /// stage and AI level; both fighters load again without a VS screen and the match goes on from the next round.
    fn finish_cutscene(&mut self) {
        let Some(fight) = self.fight.take() else { return };
        let (Some(setup), Some(data)) = (fight.setup.clone(), self.boss_change.clone()) else { return };
        let Some(idx) = self.roster.characters.iter().position(|c| c.id == data.becomes) else {
            log::error!("the scene's character {} is not in the roster", data.becomes);
            return;
        };
        let (round, wins) = (fight.bout.round, fight.bout.wins);
        let mut ladder = fight.ladder.clone();
        if let Some(l) = ladder.get_mut(setup.level) {
            *l = data.becomes; // a continue retries the one it became
        }
        let mut next = setup.clone();
        next.players[1] = (idx, setup.players[1].1);
        self.load_fight(next, ladder);
        if let Some(f) = self.fight.as_mut() {
            f.bout.resume_match(round, wins);
        }
    }

    /// A menu key from the player (keyboard, gamepad, `--keys`): ignored while a demo plays, recorded otherwise.
    fn input_key(&mut self, player: usize, key: Key) -> bool {
        if self.demo_play.is_some() {
            return true;
        }
        // (a browser page cannot close itself: Back where the game would quit does nothing there)
        self.apply_event(demo::Event::Key(player, key)) || cfg!(target_arch = "wasm32")
    }

    /// Apply a menu key or a debug cheat, and record it in the demo.
    fn apply_event(&mut self, e: demo::Event) -> bool {
        if let Some(d) = self.demo_rec.as_mut() {
            d.push_event(self.logic_steps, &e);
        }
        match e {
            demo::Event::Key(player, key) => self.menu_key(player, key),
            demo::Event::Cheat(c) => {
                self.spoil_replay("a cheat changed the match");
                self.cheat(c);
                true
            }
        }
    }

    fn cheat(&mut self, c: demo::Cheat) {
        use demo::Cheat;
        match c {
            Cheat::Ko(p) => match self.fight.as_mut() {
                Some(f) => {
                    f.bout.game.fight.fighters[p].health = 0;
                    self.debug.say(format!("K.O. P{}", p + 1));
                }
                None => self.debug.say("no fight"),
            },
            Cheat::WinMatch => match self.fight.as_mut() {
                Some(f) => {
                    let b = &mut f.bout;
                    b.wins[0] = b.wins[0].max(b.rounds_to_win.saturating_sub(1));
                    b.game.fight.fighters[1].health = 0;
                    self.debug.say("P1 wins the match");
                }
                None => self.debug.say("no fight"),
            },
            Cheat::ArcadeStage(n) => self.arcade_from(n, None, None),
            Cheat::Health(m) => {
                (self.debug.god_mode, self.debug.infinite_health) = (m == 1, m == 2);
                self.debug.say(["god mode off (F9)", "god mode: P1 (F9)", "god mode: both fighters (F9)"][m.min(2) as usize]);
            }
        }
    }

    /// Arcade from ladder stage `level` (0..9): the running arcade's ladder and player, or a new ladder for
    /// `player` (roster index, costume) / the current P1.
    fn arcade_from(&mut self, level: usize, player: Option<(usize, usize)>, difficulty: Option<usize>) {
        let current = self.fight.as_ref().and_then(|f| f.setup.clone().map(|s| (s, f.ladder.clone())));
        let (player, difficulty, ladder) = match current {
            Some((s, l)) if s.mode == Mode::Arcade && !l.is_empty() && player.is_none() => (s.players[0], difficulty.unwrap_or(s.difficulty), l),
            current => {
                let player = player.or(current.as_ref().map(|c| c.0.players[0])).unwrap_or_else(|| {
                    let m = self.models[0];
                    let idx = self.roster.characters.iter().position(|c| c.costumes.contains(&m)).unwrap_or(0);
                    (idx, self.roster.characters[idx].costumes.iter().position(|&x| x == m).unwrap_or(0))
                });
                let id = self.roster.characters[player.0].id;
                let difficulty = difficulty.or(current.map(|c| c.0.difficulty)).unwrap_or(1);
                (player, difficulty, menu::arcade_ladder(&self.roster, id, &mut self.seed))
            }
        };
        let level = level.min(ladder.len().saturating_sub(1));
        let setup = menu::arcade_setup(&self.roster, player, difficulty, &ladder, level);
        let opponent = self.roster.characters.get(setup.players[1].0).map_or("?", |c| c.name.as_str());
        self.debug.say(format!("arcade stage {} against {opponent}", level + 1));
        self.fight = None;
        self.scene = None;
        self.start_fight(setup, ladder);
    }

    /// One fight step back (the rewind buffer).
    fn rewind(&mut self) -> bool {
        let Some(f) = self.fight.as_mut() else { return false };
        if self.history.len() < 2 {
            return false;
        }
        self.history.pop_back();
        f.bout = self.history.back().cloned().unwrap_or_else(|| f.bout.clone());
        self.debug.history_len = self.history.len();
        if let Some(v) = self.vfx.as_mut() {
            v.clear();
        }
        self.stop_demo_recording("the fight was rewound");
        true
    }

    /// A rewind or a state load cannot be replayed: the demo recording ends.
    fn stop_demo_recording(&mut self, why: &str) {
        if self.demo_rec.take().is_some() {
            self.debug.say(format!("demo recording stopped: {why}"));
        }
    }

    /// What a menu screen asked for.
    /// New graphics settings (OPTION > GRAPHICS): the window (full screen, size) and the renderer change at once,
    /// and the save keeps them.
    fn set_graphics(&mut self, g: graphics::Graphics) {
        let g = self.display.fit(g);
        self.apply_graphics(&g);
        self.records.graphics = g.clone();
        self.records.save();
        self.graphics = g;
    }

    /// A Force run for a player.
    fn new_force_run(&mut self, player: (usize, usize), difficulty: usize) {
        let Some(data) = self.force_data.clone() else { return };
        let c = &self.roster.characters[player.0];
        let model = c.costumes[player.1.min(c.costumes.len() - 1)];
        let selection = c.id * 4 + self.roster.costume_of.get(&model).copied().unwrap_or(0);
        let drb = false;
        self.force = Some(forcerun::ForceRun::new(data, player, selection, c.id, difficulty, self.records.force_keys, self.records.force_hiscore, drb));
    }

    /// `--mode`: start a mode straight away, past its menus (debugging; see `launch.rs`).
    fn launch(&mut self, spec: launch::LaunchSpec) {
        use launch::LaunchMode as L;
        let roster = &self.roster;
        let [p1, p2, p3, p4] = spec.players;
        let id = |p: (usize, usize)| roster.characters.get(p.0).map_or(0, |c| c.id);
        log::info!("--mode {:?}: {:?}", spec.mode, spec.players.iter().map(|&p| (roster.characters[p.0].name.as_str(), p.1 + 1)).collect::<Vec<_>>());
        if let Some(a) = self.audio.as_mut() {
            a.music(None);
        }
        (self.fight, self.tag, self.force, self.run, self.scene, self.embu, self.screen) = (None, None, None, None, None, None, None);
        // the AI's seats (`random`: a style from the game's seed, as the menus draw)
        for k in 0..4 {
            self.ai_seats[k] = spec.ai[k].then(|| {
                let style = spec.ai_style[k].unwrap_or_else(|| {
                    self.seed = self.seed.wrapping_mul(0x41C6_4E6D).wrapping_add(12345);
                    let all = kumite_sim::ai::Style::ALL;
                    all[((self.seed >> 16) & 0x7FFF) as usize % all.len()]
                });
                replay::AiSeat { style, difficulty: spec.ai_difficulty, level: spec.ai_level }
            });
        }
        // VS `--p2-cpu`: 2P the game's CPU, an AI seat like the others (so its replay has it)
        if spec.mode == L::Versus && spec.p2_cpu && self.ai_seats[1].is_none() {
            self.ai_seats[1] = Some(replay::AiSeat { style: kumite_sim::ai::Style::Classic, difficulty: spec.difficulty, level: 5 });
        }
        log::info!("--ai: {:?}", self.ai_seats);
        let fight = |mode: Mode, stage: usize, ball| FightSetup {
            mode,
            players: [p1, p2],
            stage: spec.stage.unwrap_or(stage),
            difficulty: spec.difficulty,
            level: 5,
            handicap: [100; 2],
            ball,
            partners: None,
        };
        match spec.mode {
            L::Arcade => {
                let mut ladder = menu::arcade_ladder(roster, id(p1), &mut self.seed);
                let level = spec.level.min(ladder.len().saturating_sub(1));
                if spec.given[1] {
                    ladder[level] = id(p2);
                }
                let mut setup = menu::arcade_setup(roster, p1, spec.difficulty, &ladder, level);
                if spec.given[1] {
                    setup.players[1] = p2;
                }
                if let Some(s) = spec.stage {
                    setup.stage = s;
                }
                self.load_fight(setup, ladder);
            }
            L::Versus | L::Practice | L::Ball => {
                let mode = match spec.mode {
                    L::Versus => Mode::Versus,
                    L::Practice => Mode::Practice,
                    _ => Mode::Ball,
                };
                let ball = (mode == Mode::Ball).then_some((0, 0));
                self.ball_cpu = [spec.ai[0], spec.p2_cpu || spec.ai[1]];
                // Ball 2 ON 2: --fighters A,B,C,D (C beside A, D beside B)
                if mode == Mode::Ball && spec.given[2] && spec.given[3] {
                    let mut setup = fight(mode, roster.ball_stage, ball);
                    let seats = [(!self.ball_cpu[0]).then_some(0), (!self.ball_cpu[1]).then_some(1), None, None];
                    setup.partners = Some(menu::Doubles { partners: [p3, p4], seats });
                    self.load_fight(setup, Vec::new());
                    return;
                }
                // Ball is played on its own stage (0x8004F71C), as the select screen does
                let stage = if mode == Mode::Ball { roster.ball_stage } else { roster.home_stage(id(p2), p2.1) };
                self.load_fight(fight(mode, stage, ball), Vec::new());
            }
            L::ExtraLevels => {
                let Some(stage) = spec.stage.or(roster.extra_levels.first().map(|l| l.0)) else {
                    log::error!("--mode levels: no levels installed (stages.json)");
                    return;
                };
                self.load_fight(fight(Mode::ExtraLevels, stage, None), Vec::new());
            }
            L::TimeAttack | L::Survival => {
                let mut run = if spec.mode == L::TimeAttack { modes::Run::time_attack(roster, p1, &mut self.seed) } else { modes::Run::survival(p1) };
                match &mut run.kind {
                    modes::Kind::TimeAttack(t) => {
                        t.stage = spec.level.min(t.ladder.len().saturating_sub(1));
                        if spec.given[1] {
                            t.ladder[t.stage] = id(p2);
                        }
                    }
                    // (the wins so far: over the first pool, as if each was beaten in turn; the count is their sum)
                    modes::Kind::Survival(s) => {
                        s.wins = spec.level as u32;
                        let pool = self.roster.modes.pools.first().map(|p| p.1.clone()).unwrap_or_default();
                        for k in 0..spec.level {
                            if let Some(b) = pool.get(k % pool.len().max(1)).and_then(|&c| s.beaten.get_mut(c)) {
                                *b += 1;
                            }
                        }
                    }
                    modes::Kind::Team(_) => {}
                }
                let mut setup = run.setup(&self.roster, &self.roster.modes, &mut self.seed, self.frames as u32);
                if spec.given[1] && spec.mode == L::Survival {
                    setup.players[1] = p2;
                    setup.stage = self.roster.home_stage(id(p2), p2.1);
                }
                if let Some(s) = spec.stage {
                    setup.stage = s;
                }
                setup.difficulty = spec.difficulty;
                self.run = Some(run);
                self.load_fight(setup, Vec::new());
            }
            L::Force => {
                self.new_force_run(p1, spec.difficulty);
                let Some(run) = self.force.as_mut() else {
                    log::error!("--mode force: no force.json");
                    return;
                };
                let (extras, last) = (run.data.extra_stages.len(), run.data.final_index());
                (run.index, run.extra) = match spec.force_stage {
                    Some(s) if s < last => (s, None),
                    Some(s) if s - last < extras => (last.saturating_sub(1), Some(s - last)),
                    Some(s) => {
                        log::warn!("--force-stage {}: only {} extra stages; the final stage", s + 1, extras);
                        (last, None)
                    }
                    None => (last, None),
                };
                run.keys = spec.force_keys.min(last.saturating_sub(1) as u32);
                if spec.force_coop {
                    let n = self.roster.characters[p1.0].costumes.len().max(1);
                    run.partner = Some(if spec.given[1] { p2 } else { (p1.0, (p1.1 + 1) % n) });
                }
                let setup = FightSetup {
                    mode: Mode::Force,
                    players: [p1, p1],
                    stage: run.stage_id(),
                    difficulty: spec.difficulty,
                    level: run.data.ai_level(run.index),
                    handicap: [100; 2],
                    ball: None,
                    partners: None,
                };
                self.load_fight(setup, Vec::new());
                if spec.force_boss || spec.force_keys != 0 {
                    self.spoil_replay("--boss / --force-keys changed the stage");
                }
                if let Some(st) = self.force.as_mut().and_then(|r| r.stage.as_mut()) {
                    st.keys = spec.force_keys;
                    if spec.force_boss {
                        if let Some(f) = self.fight.as_mut() {
                            st.skip_to_boss(&mut f.bout.game);
                        }
                    }
                }
            }
            L::TagFree | L::TagTeams | L::TagArcade => {
                let mode = match spec.mode {
                    L::TagFree => tagrun::TagMode::Free,
                    L::TagTeams => tagrun::TagMode::Teams,
                    _ => tagrun::TagMode::Arcade,
                };
                let mut slots = tagrun::TagSelect::new(roster, spec.difficulty, self.seed, mode).slots;
                for (k, p) in [p1, p2, p3, p4].into_iter().enumerate() {
                    if spec.given[k] {
                        (slots[k].character, slots[k].costume) = p;
                    }
                    slots[k].player = match k {
                        _ if spec.ai[k] => None,
                        0 => Some(0),
                        1 if !spec.p2_cpu => Some(1),
                        _ => None,
                    };
                }
                let mut setup = tagrun::TagSetup { slots, difficulty: spec.difficulty, mode, arcade: None };
                if mode == tagrun::TagMode::Arcade {
                    let pick = |k: usize| (slots[k].character, slots[k].costume);
                    let mut a = tagrun::TagArcade::new(roster, [pick(0), pick(1)], &mut self.seed);
                    a.level = spec.level.min(a.ladder.len().saturating_sub(1));
                    setup.arcade = Some(a);
                }
                self.tag_stage = spec.stage;
                self.start_tag(setup);
                self.tag_stage = None;
            }
        }
    }

    fn outcome(&mut self, o: Outcome) {
        match o {
            Outcome::None | Outcome::Quit | Outcome::Exit => {}
            // online: both picked on the VS select; player 1 decides the match and sends it
            // online: the host's select decides (its match goes to every computer); the others wait for it
            Outcome::Fight(setup, _) if self.room.is_some() => {
                let mut out = Vec::new();
                if let Some(room) = self.room.as_mut().filter(|r| r.host() && self.fight.is_none() && self.net.is_none()) {
                    room.start(room::MatchSetup::Fight(setup), &self.roster, &mut out);
                }
                self.room_events(out);
            }
            Outcome::StartTag(setup) if self.room.is_some() => {
                let mut out = Vec::new();
                if let Some(room) = self.room.as_mut().filter(|r| r.host() && self.tag.is_none() && self.net.is_none()) {
                    room.start(room::MatchSetup::Tag(setup), &self.roster, &mut out);
                }
                self.room_events(out);
            }
            Outcome::Fight(setup, ladder) => self.start_fight(setup, ladder),
            Outcome::StartTag(setup) => self.start_tag(setup),
            Outcome::StartOnline(setup) => self.start_online(*setup),
            Outcome::CancelOnline => {
                self.net_linger = None;
                if let Some(mut r) = self.room.take() {
                    r.leave();
                }
            }
            Outcome::StartForce(player, difficulty) => {
                self.new_force_run(player, difficulty);
                self.start_force_stage();
            }
            Outcome::Run(run) => {
                let mut run = *run;
                let setup = run.setup(&self.roster, &self.roster.modes, &mut self.seed, self.frames as u32);
                self.run = Some(run);
                self.start_fight(setup, Vec::new());
            }
            Outcome::Load(setup, ladder) => self.load_fight(setup, ladder),
            Outcome::Demo => {
                self.attract.step = ((self.attract.step as usize + 1) % self.attract_steps.len().max(1)) as u8;
                self.start_demo();
            }
            Outcome::Attract => self.request_title(),
            Outcome::OpenTheater => {
                if let Some(t) = self.roster.theater.clone() {
                    self.screen =
                        Some(Screen::Theater(Box::new(theater::Theater::new(t.clone(), theater::TheaterData::locks(self.records.endings, self.records.endings_extra), self.records.alternate))));
                }
            }
            Outcome::Theater(e) => self.theater_event(e),
            Outcome::OpenReplays => self.replays = Some(replay_view::Browser::new(self.assets.fingerprint(), None)),
            Outcome::OpenGraphics(difficulty) => {
                self.refit();
                log::info!("display: {:?}", self.display);
                self.screen = Some(Screen::Graphics(Box::new(menu::GraphicsMenu::new(self.graphics.clone(), self.display.clone(), difficulty))));
            }
            Outcome::Graphics(g) => self.set_graphics(g),
            Outcome::OpenKeyConfig(difficulty) => {
                self.screen = Some(Screen::KeyConfig(Box::new(keyconfig::KeyConfig::new(self.records.controls.clone(), difficulty))));
            }
            Outcome::OpenKeyboard(difficulty) => {
                self.screen = Some(Screen::Keyboard(Box::new(keyconfig::KeyboardConfig::new(self.records.controls.clone(), difficulty))));
            }
            Outcome::Difficulty(d) => {
                menu::set_difficulty(d);
                self.records.difficulty = d;
                self.records.save();
            }
            Outcome::Controls(c) => {
                // VIBRATION switched on: the test shake (record 10, 0x800DED60: pattern 0x0D)
                for p in 0..2 {
                    if c.pads[p].vibration && !self.records.controls.pads[p].vibration {
                        self.rumble.request_test(p);
                    }
                }
                self.records.controls = c;
                self.records.save();
            }
            Outcome::TitleStart => self.attract.step = ((self.attract.step as usize + 1) % self.attract_steps.len().max(1)) as u8,
            Outcome::AttractNext(menu::AttractNext::Embu) => {
                if let Err(e) = self.start_embu() {
                    log::warn!("the embu: {e:#}");
                    self.screen = Some(Screen::Title { frame: menu::WHITE_HOLD, difficulty: menu::difficulty() });
                }
            }
            // the opening or the prologue, then the title
            Outcome::AttractNext(menu::AttractNext::Movie(index)) => {
                self.screen = Some(match movie::Movie::load(&self.assets.root, index) {
                    Ok(m) => Screen::Movie(Box::new(m), menu::AfterMovie::Title),
                    Err(e) => {
                        log::warn!("attract movie {index}: {e:#}");
                        Screen::Title { frame: menu::WHITE_HOLD, difficulty: menu::difficulty() }
                    }
                });
            }
        }
    }

    /// The attract loop's next step (`request_scene(3)`, 0x8004F8F8): the PRESENTS screen and the opening movie (step
    /// 0), the embu (1) or the prologue movie (2), then the title; or the title fading in from white (3). The
    /// embu's cast variant moves on each time it plays, up to what the unlocks allow.
    fn request_title(&mut self) {
        self.fight = None;
        self.embu = None;
        self.scene = None;
        let step = self.attract.step;
        // (attract.json `cycle`: the step's movie, video/NN.avi, else the embu)
        let s = self.attract_steps.get(step as usize).cloned().unwrap_or_default();
        if s["embu"] == true {
            let unlocked: Vec<usize> = self.roster.characters.iter().map(|c| c.id).collect();
            let max = self.embu_data.as_ref().map_or(0, |d| d.max_variant(&unlocked));
            self.attract.variant = if self.attract.variant + 1 > max { 0 } else { self.attract.variant + 1 };
        }
        let movie = s["movie"].as_str().and_then(|f| std::path::Path::new(f).file_stem()?.to_str()?.parse().ok());
        self.screen = Some(if s["presents"] == true { Screen::Presents { frame: 0, next: movie.map_or(menu::AttractNext::Embu, menu::AttractNext::Movie) } } else { Screen::title() });
    }

    /// What the Theater asked for: a movie (the Theater comes back after it), the music player's track, the exit.
    fn theater_event(&mut self, e: theater::Event) {
        let Some(Screen::Theater(t)) = self.screen.as_ref() else { return };
        match e {
            theater::Event::None => {}
            theater::Event::Movie(p) => {
                let m = t.data.params(p).map(|p| movie::Movie::with(&self.assets.root, &p));
                match m {
                    Some(Ok(m)) => {
                        let back = menu::AfterMovie::Theater(t.clone());
                        self.theater_music = None;
                        self.screen = Some(Screen::Movie(Box::new(m), back));
                    }
                    Some(Err(e)) => log::warn!("theater movie {p}: {e:#}"),
                    None => {}
                }
            }
            theater::Event::Music(track) => self.theater_music = track,
            theater::Event::Exit => {
                self.theater_music = None;
                self.screen = Some(Screen::menu(menu::difficulty()));
            }
        }
    }

    /// The records screen (mode 16 → 17): its page over the next stage of the rotor, the camera circling it; a
    /// new record's name entry with BGM 2.
    fn start_records(&mut self) {
        self.fight = None;
        self.embu = None;
        let screen = records::RecordsScreen::new(&self.roster.screens["records"], &mut self.records);
        self.records_music = (!self.records.new.is_empty()).then(|| self.roster.screens["records"]["timing"]["record_bgm"].as_u64()).flatten().map(|b| b as usize);
        self.scene = self.renderer.take().map(|r| {
            let mut stage = self.stage_scene(&r, screen.stage);
            stage.models.clear();
            self.renderer = Some(r);
            stage
        });
        self.screen = Some(Screen::Records(Box::new(screen)));
    }

    /// The embu (record 0 mode 6): its stage and the variant's cast; the title follows it.
    fn start_embu(&mut self) -> Result<()> {
        let data = self.embu_data.clone().context("no attract.json embu")?;
        let r = self.renderer.take().context("no renderer")?;
        let result = (|| -> Result<()> {
            let scene = kumite_game::embu::EmbuScene::new(&self.assets, data.clone(), self.attract.variant)?;
            let mut stage = self.stage_scene(&r, data.stage);
            stage.models.clear();
            // each slot gets its own GPU copy of every cast mesh (both slots may show one model)
            let mut models: [HashMap<usize, SkinnedModel>; 2] = Default::default();
            for slot in &mut models {
                for (&m, ch) in &scene.characters {
                    slot.insert(m, skinned_character(&r, ch, m));
                }
            }
            if let Some(v) = self.vfx.as_mut() {
                v.clear();
            } else {
                self.vfx = Some(kumite_game::vfx::Vfx::new(&self.assets));
            }
            log::info!("embu variant {}: {} characters", self.attract.variant, scene.characters.len());
            self.scene = Some(stage);
            self.embu = Some(EmbuRun { scene, models });
            self.screen = None;
            self.fight = None;
            Ok(())
        })();
        self.renderer = Some(r);
        result
    }

    /// One embu frame; at its end the title (picture under fading white).
    fn step_embu(&mut self) {
        let Some(run) = self.embu.as_mut() else { return };
        run.scene.step();
        let eye = run.scene.camera().0;
        let events = run.scene.events.clone();
        let models = run.scene.models;
        for e in events {
            match e {
                kumite_game::embu::EmbuEvent::Music(id) => {
                    if let (Some(audio), Some(sound)) = (self.audio.as_mut(), self.sound.as_ref()) {
                        audio.music(sound.music(id));
                    }
                }
                kumite_game::embu::EmbuEvent::GroundImpact(p) => {
                    if let Some(v) = self.vfx.as_mut() {
                        v.ground_at(p);
                    }
                }
                kumite_game::embu::EmbuEvent::Effect { op, pos, model } => {
                    let record = |m: usize| 71 + 4 * m;
                    let other = models.iter().copied().find(|&m| m != model).unwrap_or(model);
                    if let Some(v) = self.vfx.as_mut() {
                        v.effect_at(op, pos, record(model), record(other), eye);
                    }
                }
            }
        }
        if let Some(v) = self.vfx.as_mut() {
            v.tick();
        }
        if run.scene.done {
            self.embu = None;
            self.scene = None;
            self.screen = Some(Screen::Title { frame: menu::WHITE_HOLD, difficulty: menu::difficulty() });
        }
    }

    /// The attract demo fight (battle type 6, 0x800D2E3C): P1 the next of the demo list (unlocked only), P2 picked
    /// from the frame counter, never the same; P1's stage and music, AI level 4, 125 health; one round.
    fn start_demo(&mut self) {
        let list: Vec<usize> = self.attract_demo_list.iter().filter_map(|&id| self.roster.characters.iter().position(|c| c.id == id)).collect();
        let n = list.len();
        if n < 2 {
            return self.request_title();
        }
        self.attract.demo = (self.attract.demo + 1) % n;
        let i1 = self.attract.demo;
        let i2 = ((self.frames as usize & 0xFFF) % (n - 1) + i1 + 1) % n;
        let (a, b) = (list[i1], list[i2]);
        let stage = self.roster.home_stage(self.roster.characters[a].id, 0);
        let level = self.attract_demo_level;
        self.start_fight(FightSetup { mode: Mode::Watch, players: [(a, 0), (b, 0)], stage, difficulty: 1, level, handicap: [100; 2], ball: None, partners: None }, Vec::new());
    }

    /// The VS screen, then the fight.
    fn start_fight(&mut self, mut setup: FightSetup, ladder: Vec<usize>) {
        if setup.mode == Mode::Versus {
            self.vs_fights += 1;
        }
        if setup.mode == Mode::Ball {
            self.ball_cpu = setup.players.map(|p| p.0 == menu::CPU_SIDE);
            for k in 0..2 {
                if self.ball_cpu[k] {
                    setup.players[k] = self.ball_opponent();
                }
            }
            // 2 ON 2: the four were picked on its setup; who plays them as seated there
            if let Some(d) = setup.partners {
                self.ball_cpu = [d.seats[0].is_none(), d.seats[1].is_none()];
            }
        }
        // no model twice on screen (the CPU's picks: Survival's, Ball's opponent, the demo's)
        menu::distinct_models(&self.roster, &mut setup);
        // (a browser: the fight's files arrive while the VS screen shows)
        kumite_fs::prefetch(&self.fight_files(&setup));
        let run = self.run.as_ref().filter(|r| r.mode() == setup.mode);
        let number = run.map_or(self.vs_fights, |r| r.number());
        let humans = match setup.mode {
            Mode::Versus => 2,
            Mode::Watch => 0,
            _ => run.map_or(1, |r| r.humans().iter().filter(|h| **h).count()),
        };
        let teams = match run.map(|r| &r.kind) {
            Some(modes::Kind::Team(t)) => Some(t.teams.clone()),
            _ => None,
        };
        let force = (setup.mode == Mode::Force).then(|| self.force.as_ref().map(|r| r.card())).flatten();
        let extra = Box::new(menu::VsExtra { humans, teams, force });
        self.screen = Some(Screen::Versus { setup, ladder, frame: 0, number, extra });
    }

    /// Ball's CPU opponent (0x800B4DC8): from ball.json's pool, the least drawn first (up to 4 more draws
    /// than the least); the very first Ball match is against its first one.
    fn ball_opponent(&mut self) -> (usize, usize) {
        let roster = &self.roster;
        if let Some(first) = roster.ball_first.filter(|&c| !self.records.ball_played && roster.by_id(c).is_some()) {
            self.records.ball_played = true;
            self.records.save();
            // (a character id: the save's tally of the opponents met, grown as needed)
            if first < 1024 && self.ball_counts.len() <= first {
                self.ball_counts.resize(first + 1, 0);
            }
            if let Some(n) = self.ball_counts.get_mut(first) {
                *n += 1;
            }
            return (roster.index_of(first).unwrap_or(0), 0);
        }
        let mut list: Vec<(usize, u32)> = roster.ball_pool.iter().copied().filter(|&c| roster.by_id(c).is_some()).map(|c| (c, self.ball_counts.get(c).copied().unwrap_or(0))).collect();
        list.sort_by_key(|e| e.1);
        let least = list.first().map_or(0, |e| e.1);
        let n = list.iter().filter(|e| e.1 <= least + roster.ball_spread).count().max(1);
        self.seed = self.seed.wrapping_mul(0x41C6_4E6D).wrapping_add(12345);
        let c = list.get(((self.seed >> 16) & 0x7FFF) as usize % n).map_or(0, |e| e.0);
        if c < 1024 && self.ball_counts.len() <= c {
            self.ball_counts.resize(c + 1, 0);
        }
        if let Some(n) = self.ball_counts.get_mut(c) {
            *n += 1;
        }
        let idx = roster.index_of(c).unwrap_or(0);
        let costume = (self.frames as usize & 1).min(roster.characters[idx].costumes.len() - 1);
        (idx, costume)
    }

    /// Force: the stage card (the VS screen's slot), then the stage.
    fn start_force_stage(&mut self) {
        let Some(run) = self.force.as_ref() else { return };
        let setup = FightSetup {
            mode: Mode::Force,
            players: [run.player, (run.player.0, run.player.1)],
            stage: run.stage_id(),
            difficulty: run.difficulty,
            level: run.data.ai_level(run.index),
            handicap: [100; 2],
            ball: None,
            partners: None,
        };
        self.start_fight(setup, Vec::new());
    }

    /// Load a Force stage (0x8002A508): the player, the stage's two soldier kinds in the two enemy slots
    /// (out of play until spawned), the stage logic; the final stage brings its character in at once.
    fn load_force(&mut self, r: &Renderer, setup: FightSetup) -> Result<()> {
        let seats = self.ai_seats();
        let run = self.force.as_mut().context("no Force run")?;
        // (the run as this stage begins: a replay of the stage starts from it)
        let start = run.start();
        let data = run.data.clone();
        let i = run.index.min(data.final_index());
        let c = &self.roster.characters[run.player.0];
        let player_model = c.costumes[run.player.1.min(c.costumes.len() - 1)];
        let costumes = data.soldier_costumes.get(i).copied().unwrap_or([0, 1]);
        // the soldiers: selections soldier_selection_base + costume (force.json)
        let soldier_of = |k: usize| self.roster.by_selection.get(&(data.soldier_selection_base + costumes[k].min(3))).copied().context("no soldier in roster.json");
        let soldier = |k: usize| kumite_game::Actor::load(&self.assets, soldier_of(k)?.1);
        let soldiers = [soldier(0)?, soldier(1)?];
        let (soldier_char, soldier_record) = soldier_of(0)?;
        let mut game = Game::load(&self.assets, [player_model, soldier_record])?;
        game.round_frames = run.clock;
        (game.ko_collapse, game.fight.ko_collapse) = (false, false);
        let mut bout = Bout::new(game, self.ai.clone(), 1, false);
        bout.force = true;
        let level = data.ai_level(i);
        bout.set_cpu(1, Some((setup.difficulty, level)));
        bout.extra_cpu = Some(kumite_sim::ai::Ai::new(setup.difficulty, level, 1));
        // the AI's seats: the player, the co-op partner
        if let Some(a) = seats[0] {
            bout.set_cpu(0, Some((a.difficulty, a.level)));
            bout.cpu[0] = Some(a.ai(0));
        }
        bout.partner_cpu = seats[1].map(|a| a.ai(0));
        let mut st = kumite_game::force::ForceStage::new(data.clone(), i, soldiers, run.selection, run.player_char, run.keys, run.score, run.hiscore, run.kills);
        if let Some(e) = run.extra {
            st = st.extra_stage(e);
        }
        let route = st.route().cloned();
        let number = run.card().0;
        st.setup(&mut bout.game);
        if i == data.final_index() {
            // the final stage: Force's final character, waiting
            let boss = data.constants.last.character;
            let model = self.roster.by_id(boss).and_then(|c| c.costumes.first().copied()).with_context(|| format!("no character {boss}: the final stage's boss"))?;
            let actor = kumite_game::Actor::load(&self.assets, model)?;
            st.set_boss(&mut bout.game, &actor, (boss, 0));
            // (set_boss leaves a boss fight, 6: back to the final stage's own start, 8)
            st.state = 8;
        }
        run.stage = Some(st);
        run.clear_frames = 0;
        let player_char = run.player_char;
        // an extra stage: its level laid along the route (the walk and camera grids are the extra levels' fight's:
        // off); walls between the camera and the fighters cut away
        let mut scene = match &route {
            Some(route) => {
                let mut scene = self.stage_scene_with(r, setup.stage, &|p| route.local(p));
                (scene.walls, scene.camera_walls, scene.cutaway) = (None, None, true);
                // the walls on the camera's side of the route: gone, so they never stand before the fight
                scene.near_cut = Some(route.half_width + 0.5);
                scene
            }
            None => self.stage_scene(r, setup.stage),
        };
        scene.force_models = Some(Default::default());
        // Force casts no model shadows (0x8003AFCC skips them): blobs instead (0x800793B8), in the stage's
        // colour (an extra stage, without a table of its own: BACKSTREETS')
        scene.shadow = None;
        scene.blob = self.stage_lights.get(&setup.stage).and_then(|l| l.2).or_else(|| data.stages.first().and_then(|s| self.stage_lights.get(&s.stage)).and_then(|l| l.2));
        let name = c.name.clone();
        // the player's music; the final stage its own
        if let (Some(audio), Some(sound)) = (self.audio.as_mut(), self.sound.as_ref()) {
            let own = self.roster.music_of(c.id, 0).and_then(|b| sound.music(b));
            audio.music(if i == data.final_index() { sound.music(data.constants.last.bgm) } else { own.or_else(|| sound.music_for(&name)) });
        }
        let models = [player_model, bout.game.bodies[1].model_record];
        let names = [name.clone(), self.roster.by_id(soldier_char).map(|c| c.name.clone()).unwrap_or_default()];
        // STAGE n or FINAL STAGE (screens.json hud.intro.force)
        let t = &self.roster.screens["hud"]["intro"]["force"][if number.is_some() { "stage" } else { "final" }];
        let num = |k: &str| t[k].as_f64().unwrap_or(0.0);
        let text = art::printf(t["text"].as_str().unwrap_or_default(), &[number.map_or(0, |n| n as u32 + 1)]);
        let intro = (text, num("font") as usize, num("colour") as u32, num("x") as f32, num("y") as f32);
        let info = HudInfo { models, names: names.clone(), humans: [true, false], hidden: true, intro: Some(intro), no_result: true, ..Default::default() };
        self.hud.reset(&info);
        self.scene = Some(scene);
        self.vfx = Some(kumite_game::vfx::Vfx::new(&self.assets));
        let banks = models.map(|m| format!("m{m:03}"));
        let characters = [player_char, soldier_char];
        // (the soldiers' voice row: their +0x1A)
        let voices = [self.roster.voice_of.get(&player_model).copied().unwrap_or(player_char), self.roster.voice_of.get(&soldier_record).copied().unwrap_or(soldier_char)];
        self.fight = Some(Fight {
            bout,
            setup: Some(setup),
            ladder: Vec::new(),
            names,
            info,
            pause: None,
            rules: false,
            practice: None,
            heard: Heard::default(),
            banks,
            characters,
            voices,
            cutscene: None,
            partner: None,
            extra_voice: None,
            seats: None,
            secondary: Default::default(),
        });
        self.screen = None;
        // a co-op partner goes on with the run (and 3P, 4P)
        if self.force.as_ref().is_some_and(|r| r.partner.is_some()) {
            self.join_force_partner();
        }
        for p in 2..4 {
            if self.force.as_ref().is_some_and(|r| r.more_players[p - 2].is_some()) {
                self.join_force_player(p);
            }
        }
        // every stage is recorded (from here: the players carried over have joined), saved as a replay at its end;
        // or a replay plays
        self.replay_rec = None;
        // (`--ko-boss`: the stage's logic changed, not recorded)
        if self.replay_next.is_none() && !self.force_ko_boss {
            if let Some(f) = self.fight.as_mut() {
                f.bout.tape = Some(Default::default());
                let game = replay::Match::Force { setup: f.setup.clone().context("the stage's setup")?, start, joins: Vec::new() };
                self.replay_rec = Some(ReplayRec { game, online: self.net.is_some() || self.room.is_some() });
            }
        }
        self.begin_replay();
        Ok(())
    }

    /// A player joining a Force stage being recorded (1 the co-op partner, 2 and 3 for 3P and 4P): noted with the
    /// step it comes before.
    fn note_force_join(&mut self, who: u8) {
        let at = self.fight.as_ref().and_then(|f| f.bout.tape.as_ref()).map_or(0, kumite_game::bout::Tape::len) as u64;
        if let Some(ReplayRec { game: replay::Match::Force { joins, .. }, .. }) = self.replay_rec.as_mut() {
            joins.push((at, who));
        }
    }

    /// Force for more than two: the soldiers' places and their CPUs for every player (two a player).
    fn widen_force(&mut self) {
        let (Some(run), Some(fight)) = (self.force.as_mut(), self.fight.as_mut()) else { return };
        let Some(st) = run.stage.as_mut() else { return };
        st.widen(&mut fight.bout.game);
        let n = fight.bout.game.fight.more.len();
        let (d, l) = (run.difficulty, run.data.ai_level(run.index));
        if fight.bout.more_cpu.len() < n {
            fight.bout.more_cpu = (4..4 + n).map(|k| (!kumite_sim::Fight::is_player(k)).then(|| kumite_sim::ai::Ai::new(d, l, 1))).collect();
            fight.bout.more_human = vec![None; n];
        }
    }

    /// Force (OpenKumite's own): 3P or 4P (`p` 2, 3) comes in beside the player, as another character (the
    /// run's own pick again on later stages), at full health; the soldiers come two a player from then.
    fn join_force_player(&mut self, p: usize) {
        self.note_force_join(p as u8);
        self.widen_force();
        let (Some(run), Some(fight)) = (self.force.as_mut(), self.fight.as_mut()) else { return };
        let n = self.roster.characters.len().max(1);
        let (ch, cos) = run.more_players[p - 2].map(|(c, k, _)| (c, k)).unwrap_or(((run.player.0 + 3 * p + 1) % n, 0));
        let c = &self.roster.characters[ch];
        let model = c.costumes[cos.min(c.costumes.len() - 1)];
        match kumite_game::Actor::load(&self.assets, model) {
            Ok(actor) => {
                fight.bout.game.join_player(p, &actor);
                run.more_players[p - 2] = Some((ch, cos, self.roster.costume_of.get(&model).copied().unwrap_or(0)));
                if !self.replay_quiet {
                    menu::sfx::play(menu::sfx::start());
                }
                log::info!("Force: {}P joins as {} ({model})", p + 1, c.name);
            }
            Err(e) => log::warn!("Force {}P {model}: {e:#}", p + 1),
        }
    }

    /// The fight on is a Force stage.
    fn force_fight(&self) -> bool {
        self.force.is_some() && self.fight.as_ref().is_some_and(|f| f.setup.as_ref().is_some_and(|s| s.mode == Mode::Force))
    }

    /// Force's co-op (OpenKumite's own): the second player comes in beside the player, as the player's
    /// Character in the next costume (or the run's partner again), at full health; the keyboard splits into the
    /// two VS layouts.
    fn join_force_partner(&mut self) {
        self.note_force_join(1);
        let (Some(run), Some(fight)) = (self.force.as_mut(), self.fight.as_mut()) else { return };
        let (ch, cos) = run.partner.unwrap_or_else(|| {
            let n = self.roster.characters[run.player.0].costumes.len().max(1);
            (run.player.0, (run.player.1 + 1) % n)
        });
        let c = &self.roster.characters[ch];
        let model = c.costumes[cos.min(c.costumes.len() - 1)];
        match kumite_game::Actor::load(&self.assets, model) {
            Ok(actor) => {
                fight.bout.game.join_partner(&actor);
                // (an AI partner guards with guard moves, as a CPU does)
                if let (Some(_), Some(p)) = (fight.bout.partner_cpu.as_ref(), fight.bout.game.fight.partner.as_mut()) {
                    p.guard_input = false;
                }
                if let Some(st) = run.stage.as_mut() {
                    st.widen(&mut fight.bout.game);
                }
                let n = fight.bout.game.fight.more.len();
                let (d, l) = (run.difficulty, run.data.ai_level(run.index));
                if fight.bout.more_cpu.len() < n {
                    fight.bout.more_cpu = (4..4 + n).map(|k| (!kumite_sim::Fight::is_player(k)).then(|| kumite_sim::ai::Ai::new(d, l, 1))).collect();
                    fight.bout.more_human = vec![None; n];
                }
                run.partner = Some((ch, cos));
                run.partner_costume = self.roster.costume_of.get(&model).copied().unwrap_or(0);
                fight.partner = Some((format!("m{model:03}"), self.roster.voice_of.get(&model).copied().unwrap_or(c.id)));
                if !self.replay_quiet {
                    menu::sfx::play(menu::sfx::start());
                }
                log::info!("Force: 2P joins as {} ({model})", c.name);
            }
            Err(e) => log::warn!("the co-op partner {model}: {e:#}"),
        }
    }

    /// Force's stage logic after the fight's step, and its events.
    fn step_force(&mut self) {
        let (Some(run), Some(fight)) = (self.force.as_mut(), self.fight.as_mut()) else { return };
        let ctx = ForceCtx::of(run);
        let Some(st) = run.stage.as_mut() else { return };
        let shown = force_sim(&self.roster, &self.assets, &self.actor_cache, &ctx, &mut fight.bout, st, self.force_ko_boss);
        run.step_hud(&fight.bout.game);
        self.force_shown(shown);
        // an AI 2P down gets up again: its START, as a person's (the join is recorded; a replay has it)
        let down = |f: &Fight| f.bout.partner_cpu.is_some() && f.bout.phase == Phase::Fight && f.bout.game.fight.partner.as_ref().is_some_and(|p| p.health <= 0);
        if self.replay_view.is_none() && self.fight.as_ref().is_some_and(down) {
            self.join_force_partner();
        }
    }

    /// Force's events for the eyes and ears: sounds, the music's fade, the boss's music.
    fn force_shown(&mut self, events: Vec<kumite_game::force::ForceEvent>) {
        for e in events {
            match e {
                kumite_game::force::ForceEvent::Sound(code) => menu::sfx::play(code),
                kumite_game::force::ForceEvent::MusicFade(_) => {
                    if let Some(a) = self.audio.as_mut() {
                        a.music(None);
                    }
                }
                kumite_game::force::ForceEvent::Boss(ch, _) => {
                    if let (Some(audio), Some(sound), Some(c)) = (self.audio.as_mut(), self.sound.as_ref(), self.roster.by_id(ch)) {
                        audio.music(self.roster.music_of(ch, 0).and_then(|b| sound.music(b)).or_else(|| sound.music_for(&c.name)));
                    }
                }
                _ => {}
            }
        }
    }

    /// Force's match end (0x800B5CF0): a cleared stage waits for its tally (300 frames, or START once
    /// it is done), then the next stage or the results; a lost one is GAME OVER.
    fn force_over(&mut self, winner: Option<usize>, start: bool) {
        let Some(run) = self.force.as_mut() else { return };
        if winner == Some(0) {
            run.clear_frames += 1;
            let done = run.stage.as_ref().is_some_and(|s| s.tally_done);
            if !(done && (run.clear_frames >= 300 || start)) {
                return;
            }
            if let Some(f) = self.fight.as_ref() {
                run.clock = f.bout.game.fight.clock;
            }
            let more = run.next();
            self.records.force_keys = self.records.force_keys.max(run.keys);
            self.fight = None;
            self.scene = None;
            if more {
                self.start_force_stage();
            } else {
                run.new_hiscore = run.score > self.records.force_hiscore;
                self.records.force_hiscore = self.records.force_hiscore.max(run.score);
                self.records.save();
                self.records.played(run.player_char);
                self.records.save();
                let r = forcerun::ForceResults {
                    score: run.score,
                    hiscore: self.records.force_hiscore,
                    new_hiscore: run.new_hiscore,
                    flags: run.flags,
                    bosses: run.bosses.clone(),
                    kills: run.kills,
                    t: 0,
                    frame: 0,
                    fade: 0,
                    leaving: false,
                };
                self.force = None;
                self.screen = Some(Screen::ForceResults(Box::new(r)));
            }
            return;
        }
        // lost: the hi-score is kept, GAME OVER
        self.records.played(run.player_char);
        let score = run.stage.as_ref().map_or(run.score, |s| s.score);
        self.records.force_hiscore = self.records.force_hiscore.max(score);
        self.records.save();
        self.force = None;
        self.fight = None;
        self.scene = None;
        self.screen = Some(Screen::GameOver { frames: 0 });
    }

    /// The HUD of a run's fight: the under-bar kinds per side, the intro text, the team icons.
    fn mode_hud(&self, info: &mut HudInfo, run: &modes::Run) {
        let d = &self.roster.modes;
        let label = |k: usize| hud::Under::Label(d.labels[k].clone(), d.label_colour);
        match &run.kind {
            modes::Kind::TimeAttack(t) => {
                info.under = [hud::Under::Stage(t.stage as u32 + 1), label(1)];
                info.time_base = Some(t.total + t.carried);
            }
            modes::Kind::Survival(sv) => {
                let rank = d.ranks.get(sv.rank as usize).cloned().unwrap_or_default();
                let total = sv.total().min(9999);
                let digits = total.max(1).to_string().len();
                let fmt = d.wins.get(if total <= 1 { 0 } else { digits.min(4) }).cloned().unwrap_or_default();
                let wins = fmt.replace("%1d", &format!("{total:1}")).replace("%2d", &format!("{total:2}")).replace("%3d", &format!("{total:3}")).replace("%4d", &format!("{total:4}"));
                info.under = [hud::Under::Survival(rank, d.rank_colour, wins, d.wins_colour, d.wins_dx), label(2)];
                let t = &d.survival_intro;
                info.intro = Some((t.text.clone(), t.font, t.colour, t.x, t.y));
            }
            modes::Kind::Team(t) => {
                info.under = match (t.teams[0].human, t.teams[1].human) {
                    (true, true) => [label(0), hud::Under::None],
                    (true, false) => [label(0), hud::Under::Push],
                    _ => [hud::Under::Push, label(0)],
                };
                let it = &d.team_intro;
                info.intro = Some((it.text.clone(), it.font, it.colour, it.x, it.y));
                info.team = Some([0, 1].map(|k| (t.teams[k].members.len(), t.teams[k].current, t.just_lost == Some(k))));
            }
        }
    }

    /// The files a fight reads beyond the shared game data (what a browser fetches ahead): both fighters' models
    /// and movesets, the stage; Force's soldiers.
    fn fight_files(&self, setup: &FightSetup) -> Vec<std::path::PathBuf> {
        let mut records: Vec<usize> = setup
            .players
            .iter()
            .filter(|p| p.0 < self.roster.characters.len())
            .map(|&(c, costume)| {
                let ch = &self.roster.characters[c];
                ch.costumes[costume.min(ch.costumes.len() - 1)]
            })
            .collect();
        if let (Mode::Force, Some(data)) = (setup.mode, self.force_data.as_ref()) {
            records.extend((0..4).filter_map(|k| self.roster.by_selection.get(&(data.soldier_selection_base + k))).map(|s| s.1));
            // the stage's bosses for up to four players (they load in the middle of the stage, online inside the
            // rollback step: a browser must have them by then)
            if let Some(run) = self.force.as_ref().filter(|r| r.extra.is_none()) {
                let first = data.boss(run.selection, run.player_char, run.index.min(data.final_index()));
                let model_of = |(c, k): (usize, usize)| self.roster.by_id(c).and_then(|r| r.costumes.get(k.min(r.costumes.len().saturating_sub(1))).copied());
                records.extend(force_boss_picks(data, &ForceCtx::of(run), first, 4).into_iter().filter_map(model_of));
            }
        }
        for &(c, costume) in setup.partners.iter().flat_map(|d| d.partners.iter()).filter(|p| p.0 < self.roster.characters.len()) {
            let ch = &self.roster.characters[c];
            records.push(ch.costumes[costume.min(ch.costumes.len() - 1)]);
        }
        let mut files: Vec<std::path::PathBuf> = records.iter().flat_map(|&r| kumite_game::model_files(&self.assets, r)).collect();
        // the HUD's name plates
        files.extend(records.iter().map(|r| self.assets.root.join(format!("pictures/names/model_{r:03}_index.png"))));
        files.push(self.assets.root.join(format!("stages/stage_{:02}.glb", setup.stage)));
        files
    }

    /// TAG BRAWL: load the four fighters and the stage (team 2's first member's home), start the match.
    fn start_tag(&mut self, setup: tagrun::TagSetup) {
        let roster = &self.roster;
        let model = |s: &tagrun::SlotPick| {
            let c = &roster.characters[s.character];
            c.costumes[s.costume.min(c.costumes.len() - 1)]
        };
        // (a browser: once the four and the stage have arrived)
        let home = setup.match_slots()[2];
        let stage = self.tag_stage.unwrap_or_else(|| roster.home_stage(roster.characters[home.character].id, home.costume));
        let mut files: Vec<std::path::PathBuf> = setup.match_slots().iter().flat_map(|s| kumite_game::model_files(&self.assets, model(s))).collect();
        files.push(self.assets.root.join(format!("stages/stage_{stage:02}.glb")));
        if !kumite_fs::ready(&files) {
            kumite_fs::prefetch(&files);
            self.waiting = Some(Waiting::Tag(Box::new(setup)));
            return;
        }
        let Some(r) = self.renderer.take() else { return };
        let level = tagrun::TagRun::cpu_level(setup.difficulty);
        let picks = setup.match_slots();
        // (the AI's seats: their own level, then their style)
        let seats = self.ai_seats();
        let slots: [_; 4] = std::array::from_fn(|k| kumite_game::tag::Slot { record: model(&picks[k]), cpu: seats[k].map(|a| (a.difficulty, a.level)).or(picks[k].player.is_none().then_some(level)) });
        match kumite_game::tag::TagGame::load(&self.assets, slots, setup.mode.teams(), self.ai.clone(), self.roster.modes.rounds_to_win) {
            Ok(mut game) => {
                for (k, a) in seats.iter().enumerate() {
                    if let Some(a) = a {
                        game.cpu[k] = Some(a.ai(usize::from(k >= 2)));
                    }
                }
                let home = &picks[2];
                // (`--stage N` with `--screen tag`: a test's stage)
                let stage = self.tag_stage.unwrap_or_else(|| roster.home_stage(roster.characters[home.character].id, home.costume));
                let mut scene = self.stage_scene(&r, stage);
                scene.models = game.bodies.iter().map(|b| skinned_body(&r, b)).collect();
                self.scene = Some(scene);
                self.vfx = Some(kumite_game::vfx::Vfx::new(&self.assets));
                if let (Some(audio), Some(sound)) = (self.audio.as_mut(), self.sound.as_ref()) {
                    let c = &roster.characters[home.character];
                    audio.music(roster.music_of(c.id, home.costume).and_then(|b| sound.music(b)).or_else(|| sound.music_for(&c.name)));
                }
                // every brawl is recorded (its inputs on the game's tape), saved as a replay at its end; or a replay plays
                self.replay_rec = None;
                if self.replay_next.is_none() {
                    game.tape = Some(Default::default());
                    self.replay_rec = Some(ReplayRec { game: replay::Match::Tag { setup: setup.clone(), stage }, online: self.net.is_some() || self.room.is_some() });
                }
                self.tag = Some(tagrun::TagRun::new(game, setup, &self.roster));
                self.fight = None;
                self.screen = None;
                self.begin_replay();
            }
            Err(e) => self.load_failed("the tag brawl", &e),
        }
        self.renderer = Some(r);
    }

    /// TAG BRAWL is over (or left from the pause menu): back to its setup with the same picks.
    fn leave_tag(&mut self) {
        let Some(mut t) = self.tag.take() else { return };
        t.setup.arcade = None;
        self.scene = None;
        self.linger_net();
        if let Some(v) = self.vfx.as_mut() {
            v.clear();
        }
        if let Some(a) = self.audio.as_mut() {
            a.duck(false);
            a.music(None);
        }
        self.screen = Some(Screen::Tag(Box::new(tagrun::TagSelect::with(t.setup, &self.roster))));
    }

    /// One TAG BRAWL step: four pads (the keyboard's layouts and the gamepads by player), the match, the HUD,
    /// effects and sounds.
    fn step_tag(&mut self) {
        // a replay at its end stays there
        if self.replay_view.is_some() && self.tag.as_ref().is_some_and(|t| t.game.finished() && t.game.phase_frame >= TAG_SAVED_AFTER) {
            self.replay_ended();
            return;
        }
        let Some(t) = self.tag.as_ref() else { return };
        let players: Vec<usize> = t.setup.slots.iter().filter_map(|s| s.player).collect();
        // (online: this computer's two players split the keyboard)
        let two_here = self.room.as_ref().is_some_and(|r| r.mine().len() > 1);
        let kb = self.keyboard_pads(players.contains(&1) || two_here);
        let gp = self.gamepads.as_mut().map_or([None; 4], |g| g.pads4(&self.records.controls));
        let mut by_player: [Pad; 4] = std::array::from_fn(|p| {
            let k = kb.get(p).copied().unwrap_or_default();
            gp[p].map_or(k, |g| controls::merge(k, g))
        });
        // (`--p1`: player 1's script)
        if !self.script.is_empty() && t.phase_is_fight() {
            by_player[0] = self.script.remove(0);
        }
        self.tapped.clear();
        let Some(t) = self.tag.as_mut() else { return };
        if let Some(audio) = self.audio.as_mut() {
            audio.duck(t.pause.is_some());
        }
        if t.pause.is_some() {
            return;
        }
        let pads = t.setup.slots.map(|s| s.player.map_or(Pad::default(), |p| by_player[p]));
        if let (Some(net), Some(room)) = (self.net.as_mut(), self.room.as_mut()) {
            netplay_ready(net, room);
        }
        match self.net.as_mut() {
            // online: this computer's players' pads in, every player's to the slots' players (rolled back and run
            // again as the others' arrive)
            Some(net) => {
                let slots = t.setup.slots;
                let locals = [by_player[0], by_player[1]];
                let requests = net.tick(&locals[..net.local.len().min(2)]);
                let (advanced, fresh) = netplay::apply(net, &mut t.game, requests, &mut |g, all| g.step(slots.map(|s| s.player.and_then(|p| all.get(p).copied()).unwrap_or_default())));
                if net.gone.is_some_and(|g| g >= 180) {
                    let why = self.room.as_ref().map_or_else(|| net.status.clone(), |r| r.why_over(&net.status));
                    self.online_room(Some(why));
                    return;
                }
                if advanced == 0 || !fresh {
                    return;
                }
            }
            None => t.game.step(pads),
        }
        t.hud.step(&t.game, self.hud.data.bar_w());
        let g = &t.game;
        if let Some(v) = self.vfx.as_mut() {
            let (eye, _, _) = kumite_game::view_camera(&g.camera.view);
            let models: Vec<usize> = g.bodies.iter().map(|b| b.model_record).collect();
            let actors: Vec<(&kumite_sim::Fighter, usize, usize)> = (0..4).map(|i| (&g.melee.actors[i], i, g.melee.facing(i))).collect();
            let at = |i: usize, node: usize| {
                let (pose, place) = g.pose(i);
                Some(place.transform_point3(pose.get(node)?.w_axis.truncate()))
            };
            let node = |i: usize, name: &str| g.bodies[i].character.nodes.iter().position(|n| n.name == name);
            v.step_actors(&g.melee.hits, &models, &actors, eye, &at, &node);
        }
        if let Some(sound) = &self.sound {
            let fighters: Vec<&kumite_sim::Fighter> = g.melee.actors.iter().collect();
            let opponents: Vec<usize> = (0..4).map(|i| g.melee.facing(i)).collect();
            let banks: Vec<&str> = t.banks.iter().map(String::as_str).collect();
            let mut sounds = sound.step_actors(&fighters, &opponents, &[0, 1, 2, 3], &g.melee.hits, &g.melee.fight.data, g.melee.fight.frame, &mut t.sound, &banks, &t.characters);
            sounds.extend(g.events.iter().filter_map(|&e| {
                let (e, humans) = t.announcement(e);
                sound.announce(e, humans)
            }));
            // TAG ARCADE's continue count is spoken: "continue?", the seconds, then the end
            if let Some(c) = t.cont {
                use kumite_game::sound::Call;
                let step = menu::CONTINUE_STEP;
                let call = match c {
                    menu::CONTINUE_FRAMES => Some(Call::ContinueStart),
                    c if c % step == 0 && c > 0 && c < menu::CONTINUE_FRAMES - step => Some(Call::Count(c / step)),
                    1 => Some(Call::ContinueZero),
                    _ => None,
                };
                sounds.extend(call.and_then(|c| sound.call(c)));
            }
            if let Some(audio) = self.audio.as_mut() {
                for s in sounds {
                    audio.sfx(&s.file, s.volume);
                }
            }
        }
        self.steps += 1;
        if let Some(w) = &self.window {
            let hp: Vec<String> = g.melee.actors.iter().map(|f| (f.health / kumite_sim::ONE).to_string()).collect();
            w.set_title(&format!("OpenKumite — TAG BRAWL — {} | {:>2}s | {}  {:?}", hp[..2].join(" "), g.melee.clock / 60, hp[2..].join(" "), g.phase));
        }
        // TAG ARCADE: the continue count, then GAME OVER, then the setup
        if let Some(c) = t.cont.as_mut() {
            *c -= 1;
            if *c == 0 {
                t.cont = None;
                t.game_over = Some(0);
            }
        }
        if let Some(f) = t.game_over.as_mut() {
            *f += 1;
            if *f >= 120 {
                self.leave_tag();
            }
            return;
        }
        if t.retry {
            let setup = t.setup.clone();
            self.restart_tag(setup);
            return;
        }
        if g.finished() && g.phase_frame >= TAG_SAVED_AFTER && t.cont.is_none() {
            // the brawl over: saved as a replay (a replay stops here, on its last frame)
            if self.replay_view.is_some() {
                self.replay_ended();
                return;
            }
            self.save_replay();
            let Some(t) = self.tag.as_mut() else { return };
            // online: the arcade's next pair in a session of its own; lost or cleared, back to the setup
            if self.room.is_some() {
                match t.next() {
                    tagrun::TagNext::Next(setup) => {
                        self.linger_net();
                        self.restart_tag(*setup);
                        self.room_session();
                    }
                    _ => self.leave_tag(),
                }
                return;
            }
            match t.next() {
                tagrun::TagNext::Setup => self.leave_tag(),
                tagrun::TagNext::Continue => t.cont = Some(menu::CONTINUE_FRAMES),
                tagrun::TagNext::Next(setup) => self.restart_tag(*setup),
                // all ten pairs beaten: the staff roll, then the title
                tagrun::TagNext::Cleared => {
                    self.leave_tag();
                    if let Some(th) = self.roster.theater.clone() {
                        self.screen = Some(Screen::StaffRoll(Box::new(theater::StaffRoll::new(th))));
                    }
                }
            }
        }
    }

    /// The next (or the same) TAG ARCADE match: this one's stage and effects go, the next loads.
    fn restart_tag(&mut self, setup: tagrun::TagSetup) {
        self.tag = None;
        self.scene = None;
        if let Some(v) = self.vfx.as_mut() {
            v.clear();
        }
        self.start_tag(setup);
    }

    /// Load the fight (after the VS screen).
    fn load_fight(&mut self, setup: FightSetup, ladder: Vec<usize>) {
        // (a browser: once its files have arrived; meanwhile the screen stays, NOW LOADING over it)
        let files = self.fight_files(&setup);
        if !kumite_fs::ready(&files) {
            kumite_fs::prefetch(&files);
            self.waiting = Some(Waiting::Fight(Box::new(setup), ladder));
            return;
        }
        let Some(r) = self.renderer.take() else { return };
        let result = if setup.mode == Mode::Force { self.load_force(&r, setup) } else { self.load(&r, Some(setup), ladder) };
        if let Err(e) = result {
            self.load_failed("the fight", &e);
        }
        self.renderer = Some(r);
    }

    /// A match did not load: said; back to the title, or online out of the room (the others would wait for this
    /// computer's session for good).
    fn load_failed(&mut self, what: &str, e: &anyhow::Error) {
        log::error!("loading {what}: {e:#}");
        self.debug.say(format!("loading failed: {e:#}"));
        if self.room.is_some() {
            self.leave_online("THE MATCH DID NOT LOAD");
        } else {
            self.screen = Some(Screen::title());
        }
    }

    /// A menu key on a menu screen or the pause menu.
    fn menu_key(&mut self, player: usize, key: Key) -> bool {
        // the video's options: BACK closes them, the rest is the mouse's
        #[cfg(not(target_arch = "wasm32"))]
        if self.video_dialog.is_some() {
            if key == Key::Back {
                self.video_dialog = None;
            }
            return true;
        }
        // the replays' list over the main menu, and a replay playing, take the keys
        if let Some(b) = self.replays.as_mut() {
            let action = b.key(key);
            self.browser_action(action);
            return true;
        }
        if self.replay_view.is_some() && self.screen.is_none() {
            self.replay_key(key);
            return true;
        }
        // players 3 and 4 (gamepads 3 and 4): the setups that seat four take their keys, and Force their
        // START (they join the run, or get up again); nothing else does
        if player >= 2 {
            // (online: a computer has two players at most; a third pad's key would change this computer's screen only)
            if self.room.is_some() {
                return true;
            }
            if let Some(Screen::Tag(t)) = self.screen.as_mut() {
                t.key(player, key, &self.roster);
            } else if key == Key::Start && self.screen.is_none() && self.force_fight() && self.net.is_none() {
                let Some(fight) = self.fight.as_ref() else { return true };
                let down = fight.bout.game.fight.actor(2 + player.min(3)).is_none_or(|p| p.health <= 0 || p.inactive);
                if down && fight.pause.is_none() && fight.bout.phase == Phase::Fight {
                    self.join_force_player(player.min(3));
                }
            }
            return true;
        }
        let player = player.min(1);
        // online: any of this side's controllers moves its own cursor on the VS select
        // online: this computer's players' keys go through the room (BACK on its screen leaves it)
        if let Some(room) = self.room.as_mut() {
            if matches!(self.screen, Some(Screen::Room(_))) && key == Key::Back {
                self.leave_online("");
                return true;
            }
            if self.screen.is_some() && !matches!(self.screen, Some(Screen::Online(_))) {
                let mut out = Vec::new();
                room.key(player, key, &mut out);
                self.room_events(out);
                return true;
            }
        }
        if let Some(screen) = &mut self.screen {
            match screen.key(player, key, &self.roster, &mut self.seed) {
                Outcome::Quit => {
                    if matches!(screen, Screen::Title { .. } | Screen::Presents { .. }) {
                        return false;
                    }
                    self.screen = Some(Screen::title());
                }
                // EXIT: the confirm sound plays out (sys/08, 0.77 s), then the game closes
                Outcome::Exit => self.quit_in = Some(48),
                o => self.outcome(o),
            }
            return true;
        }
        // START during the embu: the main menu (no sound), and the attract loop moves on a step
        if self.embu.is_some() {
            if key == Key::Start {
                self.embu = None;
                self.scene = None;
                self.attract.step = ((self.attract.step as usize + 1) % self.attract_steps.len().max(1)) as u8;
                if let Some(a) = self.audio.as_mut() {
                    a.music(None);
                }
                self.screen = Some(Screen::menu(menu::difficulty()));
            } else if key == Key::Back {
                return false;
            }
            return true;
        }
        // the tag brawl's pause: RESET is the soft reset (to the main menu); none online (the others play on)
        if let Some(t) = self.tag.as_mut() {
            if self.net.is_some() {
                return true;
            }
            if t.key(player, key) {
                if let Some(v) = self.vfx.as_mut() {
                    v.clear();
                }
                if let Some(a) = self.audio.as_mut() {
                    a.duck(false);
                }
                self.soft_reset();
            }
            return true;
        }
        let Some(fight) = self.fight.as_mut() else { return true };
        // START skips the boss-change scene
        if let Some(cs) = fight.cutscene.as_mut() {
            if key == Key::Start {
                cs.skip = true;
            }
            return true;
        }
        // the attract demo: START (only) goes to the main menu, without a sound (0x800510D4)
        if fight.setup.as_ref().is_some_and(|s| s.mode == Mode::Watch) {
            if key == Key::Start {
                self.fight = None;
                self.scene = None;
                if let Some(a) = self.audio.as_mut() {
                    a.music(None);
                }
                self.screen = Some(Screen::menu(menu::difficulty()));
            } else if key == Key::Back {
                return false;
            }
            return true;
        }
        // practice: START (or Esc) opens its menu; the menu takes the keys while open
        if let Some(p) = fight.practice.as_mut() {
            if !p.open {
                if matches!(key, Key::Start | Key::Back) {
                    p.open_menu();
                }
                return true;
            }
            match p.key(key) {
                practice::Action::None => {}
                practice::Action::Closed => {
                    let cpu = p.cpu();
                    fight.bout.set_cpu(1, cpu);
                }
                // RESET leaves practice for the main menu (the soft reset)
                practice::Action::Reset => {
                    self.soft_reset();
                }
                practice::Action::PlayerSelect => {
                    let difficulty = fight.setup.as_ref().map_or(1, |s| s.difficulty);
                    self.fight = None;
                    self.scene = None;
                    self.screen = Some(Screen::Select(Box::new(menu::Select::new(Mode::Practice, difficulty, &self.roster, self.seed))));
                }
            }
            return true;
        }
        // (online: a key outside the pads changes one computer's fight only, a desync: skipping goes through the pads,
        // joining is not done)
        let online = self.net.is_some();
        // START skips the replay (and with rounds to go the win pose) or ends the win pose
        if matches!(fight.bout.phase, Phase::Replay { .. } | Phase::Result { .. }) {
            if key == Key::Start {
                if online {
                    // (online: through the pads, on every computer's same frame)
                    self.net_start[player.min(1)] = true;
                } else {
                    fight.bout.skip();
                }
            }
            return true;
        }
        // Force: the second player's START joins the run (or, down, gets up again)
        if player == 1 && key == Key::Start && fight.pause.is_none() && fight.setup.as_ref().is_some_and(|s| s.mode == Mode::Force) {
            if online {
                return true;
            }
            let down = fight.bout.game.fight.partner.as_ref().is_none_or(|p| p.health <= 0 || p.inactive);
            if down && fight.bout.phase == Phase::Fight {
                self.join_force_partner();
            }
            return true;
        }
        // the game's pause menu: CANCEL, RESET (the soft reset: the main menu), in Ball RULES; COMMAND (the move list)
        // is not done
        let ball = fight.bout.game.ball.is_some();
        let options = PAUSE_ITEMS.len() + usize::from(ball);
        // the rules card: START or a face button closes it, back to the list on CANCEL (record 8 0x800B3BC0)
        if fight.rules {
            if matches!(key, Key::Start | Key::Button(_)) {
                menu::sfx::play(menu::sfx::pick());
                fight.rules = false;
                if let Some(p) = fight.pause.as_mut() {
                    p.0 = 0;
                }
            }
            return true;
        }
        // (online: no pause, the other side's fight goes on)
        let online = self.net.is_some();
        match (&mut fight.pause, key) {
            (None, Key::Back | Key::Start) if online => {}
            (None, Key::Back | Key::Start) => {
                menu::sfx::play(menu::sfx::pick());
                fight.pause = Some((0, player));
            }
            (Some(_), Key::Back) => fight.pause = None,
            (Some((c, _)), Key::Up) => {
                menu::sfx::play(menu::sfx::cursor());
                *c = (*c + options - 1) % options;
            }
            (Some((c, _)), Key::Down) => {
                menu::sfx::play(menu::sfx::cursor());
                *c = (*c + 1) % options;
            }
            (Some((c, _)), Key::Start | Key::Button(_)) => {
                menu::sfx::play(menu::sfx::pick());
                let choice = *c;
                if choice == 2 {
                    fight.rules = true;
                    return true;
                }
                fight.pause = None;
                // RESET: the soft reset, to the main menu (0x8005101C with 0x80098A41 = 3)
                if choice == 1 {
                    if let Some(a) = self.audio.as_mut() {
                        a.duck(false);
                    }
                    self.soft_reset();
                }
            }
            _ => {}
        }
        true
    }

    fn draw(&mut self) {
        self.debug.frame_tick();
        let page_full = self.page_full_screen();
        let Some(r) = self.renderer.as_mut() else { return };
        // the face animation: the blink and the moving pupils copied into each fighter's texture when they change
        if let (Some(fight), Some(scene)) = (self.fight.as_ref(), self.scene.as_mut()) {
            let g = &fight.bout.game;
            for k in 0..2 {
                let model = match scene.force_models.as_ref() {
                    Some(cache) => cache.get(&(k, g.bodies[k].model_record)),
                    None => scene.models.get(k),
                };
                if let Some(model) = model {
                    let key = std::ptr::addr_of!(*model) as usize;
                    let shown = &mut scene.faces_shown[k];
                    if shown.0 != key {
                        *shown = (key, Default::default());
                    }
                    apply_face(r, model, &g.bodies[k].character.texture, &g.face_data, g.model_id(k), &g.faces[k], &mut shown.1);
                }
            }
        }
        // TAG BRAWL's four faces
        if let (Some(t), Some(scene)) = (self.tag.as_mut(), self.scene.as_ref()) {
            let g = &t.game;
            for (k, model) in scene.models.iter().enumerate().take(4) {
                let key = std::ptr::addr_of!(*model) as usize;
                let shown = &mut t.faces_shown[k];
                if shown.0 != key {
                    *shown = (key, Default::default());
                }
                apply_face(r, model, &g.bodies[k].character.texture, &g.face_data, g.model_id(k), &g.faces[k], &mut shown.1);
            }
        }
        // Force: a GPU model for every actor's body, made when it comes in
        if let (Some(fight), Some(scene)) = (self.fight.as_ref(), self.scene.as_mut()) {
            if let Some(cache) = scene.force_models.as_mut() {
                let g = &fight.bout.game;
                for (k, b) in (0..g.fight.actor_count()).filter_map(|k| g.body_of(k).map(|b| (k, b))) {
                    cache.entry((k, b.model_record)).or_insert_with(|| skinned_body(r, b));
                }
            }
        }
        // Force's blob shadows, shown when the camera allows
        let mut blobs = Vec::new();
        // ---- the 3D fight (or nothing behind the menus)
        let (cam, lines) = match (self.fight.as_ref(), self.scene.as_ref()) {
            (Some(fight), Some(scene)) => {
                let cutscene = fight.cutscene.as_ref();
                let (game, gpu_models) = match cutscene {
                    Some(cs) => (&cs.scene.game, &cs.models),
                    None => (&fight.bout.game, &scene.models),
                };
                for (i, model) in gpu_models.iter().enumerate() {
                    let (mut pose, mut place) = game.pose(i);
                    if i < 2 {
                        fight.secondary.borrow_mut()[i].apply(&game.bodies[i].character, game.shown_clip(i), game.fight.frame, &mut pose);
                    }
                    if !game.visible[i] {
                        // not drawn (the win pose shows the winner only, the loser shot the loser)
                        place = glam::Mat4::from_scale(glam::Vec3::ZERO);
                    }
                    r.update_bones(model, &game.bodies[i].character.skin_matrices(&pose, place));
                }
                // Force: the player, the target and the other soldier (hidden out of play, blinking dying)
                if let (Some(cache), None) = (scene.force_models.as_ref(), cutscene) {
                    let st = self.force.as_ref().and_then(|f| f.stage.as_ref());
                    let shown = |pos: usize| st.is_none_or(|s| (0..s.slots.len()).any(|k| s.slot_actor(k) == pos && s.visible(k)) || s.boss_phase && pos == 1);
                    for k in 0..game.fight.actor_count() {
                        let out = game.fight.actor(k).is_none_or(|f| f.inactive);
                        let (Some(body), Some((pose, mut place))) = (game.body_of(k), game.pose_actor(k)) else { continue };
                        let foe = !kumite_sim::Fight::is_player(k);
                        if (foe && (out || !shown(k))) || (k >= 4 && out) {
                            place = glam::Mat4::from_scale(glam::Vec3::ZERO);
                        }
                        if let Some(model) = cache.get(&(k, body.model_record)) {
                            r.update_bones(model, &body.character.skin_matrices(&pose, place));
                        }
                        // the blob under each one drawn (bone slots 14 and 17, the feet; 2, the head)
                        if let (Some(colour), false) = (scene.blob, place.x_axis.length() == 0.0) {
                            let ch = &body.character;
                            let at = |name: &str| {
                                let node = ch.nodes.iter().position(|n| n.name == name)?;
                                let p = place.transform_point3(pose[node].w_axis.truncate()) * kumite_game::UNITS;
                                Some([p.x.round() as i32, -p.y.round() as i32, -p.z.round() as i32])
                            };
                            let lying = game.fight.actor(k).is_some_and(|f| f.current_move(&game.fight.data).posture.raw & 0x204 != 0);
                            if let (Some(a), Some(b), Some(h)) = (at("foot_A"), at("foot_B"), at("head")) {
                                blobs.extend(blobshadow::blob(a, b, h, lying, colour));
                            }
                        }
                    }
                }
                // the game's fight camera (kumite_sim::camera), stepped with the fight, plus the move's screen shake;
                // or the debug orbit camera
                let mut view = game.camera.view;
                view.pitch += self.vfx.as_ref().map_or(0, |v| v.shake_pitch()) as i64;
                let (eye, forward, fov_y) = kumite_game::view_camera(&view);
                let mut cam = match self.debug.free_cam {
                    Some(fc) => {
                        let f = &game.fight.fighters;
                        let target = (f[0].root() + f[1].root()) / (2.0 * kumite_game::UNITS) + glam::Vec3::Y * 1.0 + fc.pan;
                        let dir = glam::Vec3::new(fc.yaw.sin() * fc.pitch.cos(), fc.pitch.sin(), fc.yaw.cos() * fc.pitch.cos());
                        Camera::new(target + dir * fc.distance, target, fov_y)
                    }
                    None => match cutscene {
                        // the scene's scripted camera
                        Some(cs) => {
                            let (e, t) = cs.scene.camera();
                            Camera::new(e, t, fov_y)
                        }
                        None => Camera::new(eye, eye + forward, fov_y),
                    },
                };
                // the stage around what the camera frames: the fighters as shown (the replay's)
                let shown = |i: usize| game.display.map_or(game.fight.fighters[i].root(), |d| d[i].root);
                stage_view(&mut cam, scene, [shown(0), shown(1)].map(|p| p / kumite_game::UNITS), self.debug.free_cam.is_some());
                // the hit light on the fighters (the scene has none)
                if cutscene.is_none() {
                    cam.hit_light = self.vfx.as_ref().and_then(|v| v.hit_light()).map(|(c, k, p)| (c.map(|x| x * k), p));
                }
                (cam, self.debug.lines(game))
            }
            // TAG BRAWL: the four fighters on the fight camera
            (None, Some(scene)) if self.tag.is_some() => {
                let Some(t) = self.tag.as_ref() else { return };
                let game = &t.game;
                for (i, model) in scene.models.iter().enumerate().take(4) {
                    let (pose, mut place) = game.pose(i);
                    if !game.visible[i] {
                        place = glam::Mat4::from_scale(glam::Vec3::ZERO);
                    }
                    r.update_bones(model, &game.bodies[i].character.skin_matrices(&pose, place));
                }
                let mut view = game.camera.view;
                view.pitch += self.vfx.as_ref().map_or(0, |v| v.shake_pitch()) as i64;
                let (eye, forward, fov_y) = kumite_game::view_camera(&view);
                let actors = &game.melee.actors;
                let mid = actors.iter().map(|f| f.root()).sum::<glam::Vec3>() / (actors.len() as f32 * kumite_game::UNITS);
                let mut cam = match self.debug.free_cam {
                    Some(fc) => {
                        let target = mid + glam::Vec3::Y * 1.0 + fc.pan;
                        let dir = glam::Vec3::new(fc.yaw.sin() * fc.pitch.cos(), fc.pitch.sin(), fc.yaw.cos() * fc.pitch.cos());
                        Camera::new(target + dir * fc.distance, target, fov_y)
                    }
                    None => Camera::new(eye, eye + forward, fov_y),
                };
                // the stage around what the camera frames (its two framing points, as a fight's two fighters)
                stage_view(&mut cam, scene, game.framing_points().map(|p| p / kumite_game::UNITS), self.debug.free_cam.is_some());
                cam.hit_light = self.vfx.as_ref().and_then(|v| v.hit_light()).map(|(c, k, p)| (c.map(|x| x * k), p));
                (cam, Vec::new())
            }
            // the attract embu: its two slots on the scripted camera
            (None, Some(scene)) if self.embu.is_some() => {
                let Some(run) = self.embu.as_ref() else { return };
                for s in 0..2 {
                    if let (Some((pose, place)), Some(model)) = (run.scene.pose(s), run.models[s].get(&run.scene.models[s])) {
                        let ch = &run.scene.characters[&run.scene.models[s]];
                        r.update_bones(model, &ch.skin_matrices(&pose, place));
                    }
                }
                let fov_y = kumite_game::view_camera(&kumite_sim::camera::View { h: 500, ..Default::default() }).2;
                let (e, t) = run.scene.camera();
                let mut cam = Camera::new(e, t, fov_y);
                cam.sky = glam::Mat4::from_translation(glam::Vec3::new(0.0, -scene.sky_base_y, 0.0));
                cam.sky_y_scale = scene.sky_y_scale;
                cam.sky_first = scene.sky_first;
                cam.background = Some(scene.background);
                scene_lighting(&mut cam, scene);
                (cam, Vec::new())
            }
            // the records screen: the stage alone, the eye circling its centre
            (None, Some(scene)) if matches!(self.screen, Some(Screen::Records(_))) => {
                let Some(Screen::Records(rs)) = self.screen.as_ref() else { unreachable!() };
                let (e, t) = rs.eye();
                let to_m = |p: [f32; 3]| glam::Vec3::new(p[0], -p[1], -p[2]) / kumite_game::UNITS;
                let fov_y = kumite_game::view_camera(&kumite_sim::camera::View { h: 500, ..Default::default() }).2;
                let mut cam = Camera::new(to_m(e), to_m(t), fov_y);
                cam.sky = glam::Mat4::from_translation(glam::Vec3::new(0.0, -scene.sky_base_y, 0.0));
                cam.sky_y_scale = scene.sky_y_scale;
                cam.sky_first = scene.sky_first;
                cam.background = Some(scene.background);
                (cam, Vec::new())
            }
            _ => (Camera::new(glam::Vec3::Z, glam::Vec3::ZERO, 0.8), Vec::new()),
        };
        let (sky, statics, models): (Vec<_>, Vec<_>, Vec<_>) = match (&self.scene, &self.fight) {
            (Some(s), None) if matches!(self.screen, Some(Screen::Records(_))) => (s.sky.iter().collect(), s.statics.iter().collect(), vec![]),
            (Some(s), None) if self.tag.is_some() => (s.sky.iter().collect(), s.statics.iter().collect(), s.models.iter().collect()),
            (Some(s), None) if self.embu.is_some() => {
                let Some(run) = self.embu.as_ref() else { return };
                let shown = (0..2).filter(|&k| run.scene.pose(k).is_some()).filter_map(|k| run.models[k].get(&run.scene.models[k])).collect();
                (s.sky.iter().collect(), s.statics.iter().collect(), shown)
            }
            (Some(s), Some(f)) => match &f.cutscene {
                Some(cs) if cs.scene.stage_visible => (s.sky.iter().collect(), s.statics.iter().collect(), cs.models.iter().collect()),
                Some(cs) => (vec![], vec![], cs.models.iter().collect()),
                None if s.force_models.is_some() => {
                    let g = &f.bout.game;
                    let Some(cache) = s.force_models.as_ref() else { return };
                    let models = (0..g.fight.actor_count()).filter_map(|k| g.body_of(k).and_then(|b| cache.get(&(k, b.model_record)))).collect();
                    (s.sky.iter().collect(), s.statics.iter().collect(), models)
                }
                None => (s.sky.iter().collect(), s.statics.iter().collect(), s.models.iter().collect()),
            },
            _ => (vec![], vec![], vec![]),
        };
        // effect sprites, as quads facing the camera
        let (mut p_alpha, mut p_add) = (Vec::new(), Vec::new());
        if let (Some(v), true) = (self.vfx.as_ref(), self.fight.is_some() || self.embu.is_some() || self.tag.is_some()) {
            effect_quads(&v.sprites(), v.atlas(), &cam, &mut p_alpha, &mut p_add);
        }
        // the projection the renderer uses, for texts at 3D positions
        {
            let proj = glam::camera::rh::proj::directx::perspective(cam.fov_y, r.aspect(), 0.05, 400.0);
            let view = glam::camera::rh::view::look_at_mat4(cam.eye, cam.target, glam::Vec3::Y);
            self.hud.view_proj = Some(proj * view);
        }
        if !blobs.is_empty() && shadows_shown(&cam) {
            r.set_blob_shadows(blobs);
        }
        let (mut flat_alpha, mut flat_add) = (Vec::new(), Vec::new());
        // (in a replay, the ball as it was)
        let ball = self.fight.as_ref().and_then(|f| if f.bout.game.display.is_some() { f.bout.game.display_ball.as_ref() } else { f.bout.game.fight.ball.as_ref() });
        if let (Some(b), Some(data)) = (ball, self.roster.ball.as_deref()) {
            // its shadow in the stage's shadow colour (the beach's: black)
            let shade = self.scene.as_ref().and_then(|s| s.shadow).map(|s| s.colour);
            ballview::draw(b, data, &self.roster.ball_hud["court_lines"], &cam, self.frames, shade, &mut flat_alpha, &mut flat_add);
        }
        let effects_tex = self.scene.as_ref().and_then(|s| s.effects.clone());
        let particles = effects_tex.as_deref().map(|texture| kumite_render::Particles { texture, alpha: &p_alpha, additive: &p_add, flat_alpha: &flat_alpha, flat_add: &flat_add });
        let t = Instant::now();
        let size = r.size();
        // what the replays' list asked for (a click), and the video's options and panel
        let mut browser_action = None;
        #[cfg(not(target_arch = "wasm32"))]
        let (mut dialog_action, mut status_action) = (None, None);
        let result = match self.egui.as_mut() {
            Some(egui) => {
                if self.art.is_none() {
                    self.art = Some(Art::load(&egui.ctx, &self.assets.root));
                }
                let (debug, hud, roster, catalog, frames) = (&mut self.debug, &self.hud, &self.roster, &self.catalog, self.frames);
                let net_status = self.net.as_ref().filter(|_| self.fight.is_some() || self.tag.is_some()).map(|n| n.status.clone());
                // online on the VS select: who is who
                // a fight's files still arriving (a browser): NOW LOADING and how much
                let loading = self.waiting.is_some().then(kumite_fs::progress);
                let names_line = self.room.as_ref().filter(|r| r.players() >= 2 && matches!(self.screen, Some(Screen::Grid(_)))).map(|r| format!("{}  VS  {}", r.name_of(0), r.name_of(1)));
                // online: the setup's seconds left
                let setup_secs = self.setup_clock.filter(|&f| f != u32::MAX && self.room.is_some()).map(|f| f.div_ceil(60));
                let Some(art) = self.art.as_ref() else { return };
                let (screen, fight) = (self.screen.as_ref(), self.fight.as_ref());
                let (mut replays, mut replay_view) = (self.replays.as_mut(), self.replay_view.as_deref_mut().filter(|_| self.screen.is_none()));
                #[cfg(not(target_arch = "wasm32"))]
                let (mut video_dialog, mut video_export) = (self.video_dialog.as_mut(), self.video_export.as_mut());
                let (paused, speed) = (debug.paused, debug.speed);
                let force = self.force.as_ref().filter(|_| fight.is_some_and(|f| f.setup.as_ref().is_some_and(|s| s.mode == Mode::Force)));
                let tag = self.tag.as_ref();
                let embu_white = self.embu.as_ref().map_or(0, |e| e.scene.white);
                let adapter = r.adapter_name().to_string();
                let video = self.graphics.stats.then(|| {
                    let g = &self.graphics;
                    let scene = [((size[0] as f32 * g.scale).round() as u32).min(r.max_texture()), ((size[1] as f32 * g.scale).round() as u32).min(r.max_texture())];
                    debug::VideoInfo {
                        frame: size,
                        scene,
                        scale: g.scale,
                        display: if page_full { "BROWSER FULLSCREEN" } else { g.display.label() },
                        refresh: self.window.as_ref().and_then(|w| w.current_monitor()).and_then(|m| m.refresh_rate_millihertz()).map(|mhz| mhz as f32 / 1000.0),
                        vsync: if cfg!(target_arch = "wasm32") {
                            "browser"
                        } else if g.vsync {
                            "on"
                        } else {
                            "off"
                        },
                        textures: if g.smooth_textures { "smooth" } else { "sharp" },
                        // (a browser does not tell the GPU's name)
                        adapter: if adapter.starts_with(" (") { format!("hidden by the browser{}", adapter) } else { adapter.clone() },
                        gpu: format!("{:?}", r.device_type()),
                        max_texture: r.max_texture(),
                    }
                });
                let mut frame = egui.run(self.window.as_deref(), size, |ctx| {
                    if embu_white > 0 {
                        // the embu's additive white
                        let view = art::Screen::fit(ctx.content_rect());
                        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("embu_fade")));
                        Art::shade(&painter, &view, [0.0, 0.0, art::SCREEN_W, art::SCREEN_H], [art::additive(egui::Color32::from_gray(embu_white)); 4]);
                    }
                    if let Some(screen) = screen {
                        screen.draw(ctx, art, roster);
                    } else if let Some(cs) = fight.and_then(|f| f.cutscene.as_ref()) {
                        // the scene has no HUD; its white fade adds over the picture
                        if cs.scene.white > 0 {
                            let view = art::Screen::fit(ctx.content_rect());
                            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("scene_fade")));
                            let full = [0.0, 0.0, art::SCREEN_W, art::SCREEN_H];
                            Art::shade(&painter, &view, full, [art::additive(egui::Color32::from_gray(cs.scene.white)); 4]);
                        }
                    } else if let Some(t) = tag {
                        if replay_view.as_ref().is_none_or(|v| v.hud) {
                            t.hud.draw(ctx, art, &hud.data, &t.game, &t.info);
                        }
                        if let Some((c, player)) = t.pause {
                            draw_pause(ctx, art, &hud.data.pause_items, c, player, frames, false);
                        }
                        // TAG ARCADE lost: "CONTINUE? n", then GAME OVER, in the big font (as the arcade's)
                        let view = art::Screen::fit(ctx.content_rect());
                        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("tag_continue")));
                        if let Some(c) = t.cont {
                            Art::shade(&painter, &view, [0.0, 0.0, art::SCREEN_W, art::SCREEN_H], [egui::Color32::from_black_alpha(140); 4]);
                            art.text(&painter, &view, 2, 9, 63.0, 200.0, &roster.ui.continue_text.replace("%d", &c.div_ceil(menu::CONTINUE_STEP).to_string()), egui::Color32::WHITE);
                        } else if t.game_over.is_some() {
                            Art::shade(&painter, &view, [0.0, 0.0, art::SCREEN_W, art::SCREEN_H], [egui::Color32::from_black_alpha(140); 4]);
                            art.text(&painter, &view, 2, 1, 85.0, 200.0, &roster.ui.game_over, egui::Color32::WHITE);
                        }
                    } else if let Some(f) = fight {
                        // Force's chickens behind the fighters (not while paused): the game's GTE place
                        let pickups = |painter: &egui::Painter, view: &art::Screen| {
                            if let (Some(run), None) = (force, f.pause) {
                                let window = ctx.content_rect();
                                let forward = (cam.target - cam.eye).normalize_or_zero();
                                let place = |p: glam::Vec3| {
                                    let m = p / kumite_game::UNITS;
                                    let (sx, sy) = project_point(&cam, size, window, view, m)?;
                                    Some((sx, sy, (m - cam.eye).dot(forward) * kumite_game::UNITS / 4.0))
                                };
                                run.draw_pickups(painter, view, art, &place);
                            }
                        };
                        if replay_view.as_ref().is_none_or(|v| v.hud) {
                            hud.draw_with(ctx, art, &f.bout, &f.info, &pickups);
                        }
                        if let Some(run) = force {
                            let view = art::Screen::fit(ctx.content_rect());
                            let window = ctx.content_rect();
                            let project = |m: glam::Vec3| project_point(&cam, size, window, &view, m);
                            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("force_hud"))).with_clip_rect(view.rect);
                            run.draw(&painter, &view, art, hud, roster, &f.bout.game, frames, &project);
                        }
                        if let Some(p) = &f.practice {
                            let view = art::Screen::fit(ctx.content_rect());
                            let window = ctx.content_rect();
                            let project = |m: glam::Vec3| project_point(&cam, size, window, &view, m);
                            p.draw(ctx, art, f.bout.phase == Phase::Intro, &project);
                        }
                        if let (Some(_), true) = (f.pause, f.rules) {
                            draw_rules(ctx, art);
                        } else if let Some((c, player)) = f.pause {
                            let ball = f.bout.game.ball.is_some();
                            // (no ball_items: the usual items)
                            let items = if ball && !hud.data.pause_ball_items.is_empty() { &hud.data.pause_ball_items } else { &hud.data.pause_items };
                            draw_pause(ctx, art, items, c, player, frames, ball);
                        }
                        debug.ui(ctx, &f.bout.game, catalog, &adapter, size);
                    }
                    // a replay's controls, over a fight or a tag brawl: the steps played, each player's inputs
                    if let Some(v) = replay_view.as_deref_mut() {
                        let (at, inputs): (usize, Vec<(String, &kumite_sim::input::InputState)>) = match (tag, fight) {
                            (Some(t), _) => (t.game.played(), t.game.melee.actors.iter().enumerate().map(|(i, a)| (format!("{}P", i + 1), &a.input)).collect()),
                            (None, Some(f)) => (f.bout.played(), f.bout.game.fight.fighters.iter().take(2).enumerate().map(|(i, a)| (format!("P{}", i + 1), &a.input)).collect()),
                            _ => (0, Vec::new()),
                        };
                        v.ui(ctx, at, &inputs, paused, speed, debug.free_cam.is_some());
                    }
                    if let Some(b) = replays.as_deref_mut() {
                        browser_action = b.ui(ctx);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        if let Some(d) = video_dialog.as_deref_mut() {
                            dialog_action = d.ui(ctx);
                        }
                        if let Some(x) = video_export.as_deref_mut() {
                            status_action = x.ui(ctx);
                        }
                    }
                    if let Some(v) = &video {
                        debug.video_stats(ctx, v);
                    }
                    if let Some((got, total, _)) = loading {
                        let view = art::Screen::fit(ctx.content_rect());
                        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("loading")));
                        // bottom right of the 368 x 480 screen
                        art.text(&painter, &view, 0, 6, 352.0 - art.text_w(0, "NOW LOADING"), 436.0, "NOW LOADING", egui::Color32::WHITE);
                        if total > 0 {
                            let w = 100.0 * (got as f32 / total as f32).clamp(0.0, 1.0);
                            Art::shade(&painter, &view, [252.0, 454.0, 100.0, 3.0], [egui::Color32::from_gray(48); 4]);
                            Art::shade(&painter, &view, [252.0, 454.0, w, 3.0], [egui::Color32::from_gray(200); 4]);
                        }
                    }
                    if let Some(secs) = setup_secs {
                        let view = art::Screen::fit(ctx.content_rect());
                        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("setup_clock")));
                        let t = format!("{secs:02}");
                        Art::shade(&painter, &view, [160.0, 0.0, 48.0, 30.0], [egui::Color32::from_black_alpha(170); 4]);
                        art.text(&painter, &view, 1, if secs <= 5 && frames & 0x10 != 0 { 2 } else { 6 }, crate::art::CENTRE - art.text_w(1, &t) / 2.0, 4.0, &t, egui::Color32::WHITE);
                    }
                    // online: the connection's line (CONNECTING, the ping, a desync) under the timer
                    if let Some(t) = &names_line {
                        let view = art::Screen::fit(ctx.content_rect());
                        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("net_names")));
                        Art::shade(&painter, &view, [0.0, 452.0, art::SCREEN_W, 22.0], [egui::Color32::from_black_alpha(170); 4]);
                        art.text(&painter, &view, 0, 6, crate::art::CENTRE - art.text_w(0, t) / 2.0, 456.0, t, egui::Color32::WHITE);
                    }
                    if let Some(t) = &net_status {
                        let view = art::Screen::fit(ctx.content_rect());
                        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("net_status")));
                        // small, over the timer (its digits start at y 22)
                        let scale = 0.75;
                        art.text_scaled(&painter, &view, 0, if t.starts_with("PING") { 6 } else { 1 }, crate::art::CENTRE - art.text_w(0, t) * scale / 2.0, 7.0, scale, t, egui::Color32::WHITE);
                    }
                    debug.toasts(ctx);
                });
                self.debug.ui_ms = t.elapsed().as_secs_f32() * 1000.0;
                r.render_with(&cam, &sky, &statics, &models, particles.as_ref(), &lines, Some(&mut frame))
            }
            None => r.render_with(&cam, &sky, &statics, &models, particles.as_ref(), &lines, None),
        };
        match result {
            Ok(stats) => {
                let skipped = self.debug.stats.skipped + stats.skipped;
                self.debug.stats = RenderStatsExt::with_skipped(stats, skipped);
            }
            Err(e) => log::error!("render: {e:#}"),
        }
        self.debug.render_ms = t.elapsed().as_secs_f32() * 1000.0 - self.debug.ui_ms;
        if let Some(img) = r.take_capture() {
            match img.and_then(|img| save_screenshot(&img)) {
                Ok(path) => self.debug.say(format!("screenshot: {}", path.display())),
                Err(e) => self.debug.say(format!("screenshot failed: {e:#}")),
            }
        }
        self.apply_actions();
        self.browser_action(browser_action);
        #[cfg(not(target_arch = "wasm32"))]
        self.video_actions(dialog_action, status_action);
        // (a browser's picker: the replay files it has read)
        let opened = files::take_opened();
        if !opened.is_empty() {
            self.import_replays(opened);
        }
        self.replay_requests();
    }

    /// Apply what the debug panel asked for.
    fn apply_actions(&mut self) {
        // (online: the debug panel changes nothing but the picture: a change of one computer's fight is a desync)
        let online = self.net.is_some();
        for action in std::mem::take(&mut self.debug.actions) {
            if online && !matches!(action, Action::Screenshot) {
                continue;
            }
            if !matches!(action, Action::Screenshot) {
                self.spoil_replay("the debug panel changed the match");
            }
            let game = self.fight.as_mut().map(|f| &mut f.bout.game);
            match action {
                Action::ResetRound => {
                    if let Some(g) = game {
                        g.reset_round();
                        self.debug.say("round reset");
                    }
                }
                Action::StepFrames(n) => {
                    for _ in 0..n {
                        self.step();
                    }
                }
                Action::SetGuardInput { player, on } => {
                    if let Some(g) = game {
                        g.fight.fighters[player].guard_input = on;
                    }
                }
                Action::PlayMove { player, to } => {
                    if let Some(g) = game {
                        g.fight.fighters[player].play(to);
                    }
                }
                Action::Reload { models, stage } => {
                    let Some(r) = self.renderer.take() else { continue };
                    let (old_models, old_stage) = (self.models, self.stage);
                    (self.models, self.stage) = (models, stage);
                    if let Err(e) = self.load(&r, None, Vec::new()) {
                        self.debug.say(format!("loading failed: {e:#}"));
                        (self.models, self.stage) = (old_models, old_stage);
                        let _ = self.load(&r, None, Vec::new());
                    } else {
                        self.debug.say(format!("loaded models {models:?} on stage {stage}"));
                    }
                    self.renderer = Some(r);
                }
                #[cfg(target_arch = "wasm32")]
                Action::Screenshot => self.debug.say("screenshots: not in a browser"),
                #[cfg(not(target_arch = "wasm32"))]
                Action::Screenshot => {
                    if let Some(r) = self.renderer.as_mut() {
                        r.request_capture();
                    }
                }
                Action::Cheat(c) => {
                    self.apply_event(demo::Event::Cheat(c));
                }
                Action::SaveState => match self.fight.as_ref() {
                    Some(f) => {
                        self.saved = Some((f.bout.clone(), f.names.clone()));
                        self.debug.say(format!("state saved at fight frame {} (6 loads it)", f.bout.game.fight.frame));
                    }
                    None => self.debug.say("no fight to save"),
                },
                Action::LoadState => match (self.fight.as_mut(), &self.saved) {
                    (Some(f), Some((b, names))) if f.names == *names => {
                        f.bout = b.clone();
                        self.history.clear();
                        if let Some(v) = self.vfx.as_mut() {
                            v.clear();
                        }
                        self.debug.say("state loaded");
                        self.stop_demo_recording("a state was loaded");
                    }
                    (Some(_), Some(_)) => self.debug.say("the saved state is from another fight"),
                    _ => self.debug.say("no saved state"),
                },
                Action::SaveDemo => match &self.demo_rec {
                    Some(d) => {
                        let path = Path::new("openkumite_demos").join(format!("demo_{}.json", unix_time()));
                        match d.save(&path) {
                            Ok(()) => self.debug.say(format!("saved {} ({} steps; play: openkumite --demo {})", path.display(), d.len(), path.display())),
                            Err(e) => self.debug.say(format!("saving the demo failed: {e:#}")),
                        }
                    }
                    None => self.debug.say("no demo is being recorded (a rewind or a state load ends it)"),
                },
                Action::SaveRecording => {
                    let dir = Path::new("openkumite_recordings");
                    let path = dir.join(format!("rec_{}.txt", unix_time()));
                    let result = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, kumite_game::format_script(&self.debug.record)));
                    match result {
                        Ok(()) => self.debug.say(format!("saved {} (play with --p1 @{})", path.display(), path.display())),
                        Err(e) => self.debug.say(format!("saving failed: {e}")),
                    }
                }
            }
        }
    }

    /// Debug hotkeys (function keys and - / =). Returns true when the key was one of them.
    /// The replay viewer's keys besides the menu keys: Space plays / pauses, `.` a step, Home the start, Page Up /
    /// Down the previous / next round, C the free camera, H the HUD, I the inputs, K keep.
    fn replay_hotkey(&mut self, code: KeyCode, repeat: bool) -> bool {
        if self.screen.is_some() {
            return false;
        }
        let at = self.replay_played();
        let Some(v) = self.replay_view.as_mut() else { return false };
        use replay_view::Request as R;
        let r = match code {
            KeyCode::Space if !repeat => R::TogglePause,
            KeyCode::Period => R::Step,
            KeyCode::Home => R::Seek(0),
            KeyCode::PageUp => R::Seek(v.rounds.iter().rev().map(|r| r.0).find(|&s| s + 30 < at).unwrap_or(0)),
            KeyCode::PageDown => match v.rounds.iter().map(|r| r.0).find(|&s| s > at) {
                Some(s) => R::Seek(s),
                None => return true,
            },
            KeyCode::KeyC if !repeat => R::FreeCam,
            KeyCode::KeyK if !repeat => R::Keep(!v.file.kept),
            KeyCode::KeyV if !repeat => R::Video,
            KeyCode::KeyH if !repeat => {
                v.hud = !v.hud;
                return true;
            }
            KeyCode::KeyI if !repeat => {
                v.inputs = !v.inputs;
                return true;
            }
            _ => return false,
        };
        v.requests.push(r);
        true
    }

    fn hotkey(&mut self, code: KeyCode) -> bool {
        use demo::{Cheat, Event};
        match code {
            KeyCode::F12 => {
                self.debug.actions.push(Action::Screenshot);
                return true;
            }
            // F10: the video statistics on / off (kept, as OPTION > GRAPHICS > STATS)
            KeyCode::F10 => {
                let mut g = self.graphics.clone();
                g.stats = !g.stats;
                self.set_graphics(g.clone());
                if let Some(Screen::Graphics(m)) = self.screen.as_mut() {
                    m.settings = g;
                }
                return true;
            }
            KeyCode::KeyM => {
                if let Some(a) = self.audio.as_mut() {
                    let muted = a.toggle_mute();
                    self.debug.say(if muted { "sound off (M)" } else { "sound on (M)" });
                }
                return true;
            }
            _ if !self.debug.enabled => return false,
            // (online: no debug hotkey changes the fight, a desync)
            _ if self.net.is_some() => return false,
            KeyCode::Digit1 => {
                self.apply_event(Event::Cheat(Cheat::Ko(1)));
                return true;
            }
            KeyCode::Digit2 => {
                self.apply_event(Event::Cheat(Cheat::Ko(0)));
                return true;
            }
            KeyCode::Digit3 => {
                self.apply_event(Event::Cheat(Cheat::WinMatch));
                return true;
            }
            KeyCode::BracketLeft | KeyCode::BracketRight => {
                let level = self.fight.as_ref().and_then(|f| f.setup.as_ref()).filter(|s| s.mode == Mode::Arcade).map(|s| s.level);
                let next = match (level, code == KeyCode::BracketRight) {
                    (Some(l), true) => (l + 1).min(9),
                    (Some(l), false) => l.saturating_sub(1),
                    (None, _) => 0,
                };
                self.apply_event(Event::Cheat(Cheat::ArcadeStage(next)));
                return true;
            }
            KeyCode::Digit5 => {
                self.debug.actions.push(Action::SaveState);
                return true;
            }
            KeyCode::Digit6 => {
                self.debug.actions.push(Action::LoadState);
                return true;
            }
            KeyCode::F11 => {
                self.debug.actions.push(Action::SaveDemo);
                return true;
            }
            KeyCode::F9 => {
                let mode = if self.debug.infinite_health {
                    0
                } else if self.debug.god_mode {
                    2
                } else {
                    1
                };
                self.apply_event(Event::Cheat(Cheat::Health(mode)));
                return true;
            }
            KeyCode::F8 => {
                let from = self.game().map(|g| -kumite_game::view_camera(&g.camera.view).1);
                self.debug.toggle_free_cam(from);
                // the free camera looks at a stopped fight; leaving it runs the fight again
                self.debug.paused = self.debug.free_cam.is_some();
                return true;
            }
            _ => {}
        }
        let d = &mut self.debug;
        match code {
            KeyCode::F1 => d.show_ui = !d.show_ui,
            KeyCode::F2 => {
                d.paused = !d.paused;
                d.say(if d.paused { "paused (F3 steps a frame)" } else { "running" });
            }
            KeyCode::F3 => {
                d.paused = true;
                d.actions.push(Action::StepFrames(1));
            }
            KeyCode::F4 => d.toggle_hit_volumes(),
            KeyCode::F5 => d.actions.push(Action::ResetRound),
            KeyCode::F6 => d.toggle_recording(),
            KeyCode::F7 => d.cycle_p2(),
            KeyCode::Minus => {
                d.speed = (d.speed * 0.5).max(0.05);
                d.say(format!("speed {:.2}x", d.speed));
            }
            KeyCode::Equal => {
                d.speed = (d.speed * 2.0).min(4.0);
                d.say(format!("speed {:.2}x", d.speed));
            }
            _ => return false,
        }
        true
    }
}

/// Keeps the count of skipped (hidden-window) frames across frames.
trait RenderStatsExt {
    fn with_skipped(self, skipped: u32) -> Self;
}
impl RenderStatsExt for kumite_render::RenderStats {
    fn with_skipped(mut self, skipped: u32) -> Self {
        self.skipped = skipped;
        self
    }
}

/// A world point (metres) on the original 368x480 screen, as the 3D view shows it (same projection as the
/// renderer: right-handed look-at, the window's aspect).
fn project_point(cam: &Camera, size: [u32; 2], window: egui::Rect, view: &art::Screen, p: glam::Vec3) -> Option<(f32, f32)> {
    let aspect = size[0] as f32 / size[1].max(1) as f32;
    let proj = glam::camera::rh::proj::directx::perspective(cam.fov_y, aspect, 0.05, 400.0);
    let look = glam::camera::rh::view::look_at_mat4(cam.eye, cam.target, glam::Vec3::Y);
    let c = proj * look * p.extend(1.0);
    if c.w <= 0.0 {
        return None;
    }
    let (nx, ny) = (c.x / c.w, c.y / c.w);
    let px = window.left() + (nx + 1.0) / 2.0 * window.width();
    let py = window.top() + (1.0 - ny) / 2.0 * window.height();
    Some(((px - view.rect.left()) / view.rect.width() * art::SCREEN_W, (py - view.rect.top()) / view.rect.height() * art::SCREEN_H))
}

/// What a Force stage's step needs of its run (the same on every computer online).
struct ForceCtx {
    difficulty: usize,
    index: usize,
    selection: usize,
    player_char: usize,
    player: (usize, usize),
}

impl ForceCtx {
    fn of(run: &forcerun::ForceRun) -> Self {
        Self { difficulty: run.difficulty, index: run.index, selection: run.selection, player_char: run.player_char, player: run.player }
    }
}

/// A Force stage's bosses for `players` (`first`, the stage's own; more players: a boss each, the next stages'
/// bosses beside it, none twice); for fewer players, the first of those for more.
fn force_boss_picks(data: &kumite_game::force::ForceData, ctx: &ForceCtx, first: (usize, usize), players: usize) -> Vec<(usize, usize)> {
    let mut picks = vec![first];
    for j in 1..players {
        let (c, k) = data.boss(ctx.selection, ctx.player_char, (ctx.index + j) % data.final_index().max(1));
        let k = if picks.contains(&(c, k)) { k + 1 } else { k };
        picks.push((c, k));
    }
    picks
}

/// A character's actor, loaded once (Force's bosses: a rollback runs a frame again, the load comes from here).
fn cached_actor(assets: &Assets, cache: &std::cell::RefCell<HashMap<usize, kumite_game::Actor>>, model: usize) -> Option<kumite_game::Actor> {
    if let Some(a) = cache.borrow().get(&model) {
        return Some(a.clone());
    }
    let a = kumite_game::Actor::load(assets, model).map_err(|e| log::warn!("Force actor {model}: {e:#}")).ok()?;
    cache.borrow_mut().insert(model, a.clone());
    Some(a)
}

/// Force's stage logic after the fight's step, and the events that change the fight (the bosses come in,
/// the soldiers trade places): the same on every computer for the same fight and stage (online it runs inside the
/// rollback). Returns the events for the eyes and ears.
fn force_sim(
    roster: &Roster,
    assets: &Assets,
    cache: &std::cell::RefCell<HashMap<usize, kumite_game::Actor>>,
    ctx: &ForceCtx,
    bout: &mut Bout,
    st: &mut kumite_game::force::ForceStage,
    ko_boss: bool,
) -> Vec<kumite_game::force::ForceEvent> {
    use kumite_game::force::ForceEvent;
    let fighting = bout.phase == Phase::Fight;
    let won = matches!(bout.phase, Phase::RoundOver { winner: Some(0) } | Phase::MatchOver { winner: Some(0) });
    if ko_boss && st.boss_phase && fighting {
        bout.game.fight.fighters[1].health = 0;
    }
    st.step(&mut bout.game, fighting, won);
    let model_of = |c: usize, k: usize| roster.by_id(c).and_then(|r| r.costumes.get(k.min(r.costumes.len().saturating_sub(1))).copied());
    let (d, l) = (ctx.difficulty, st.data.ai_level(ctx.index));
    let mut shown = Vec::new();
    let mut events = std::mem::take(&mut st.events);
    while !events.is_empty() {
        for e in std::mem::take(&mut events) {
            match e {
                ForceEvent::Swapped => bout.swap_cpus(),
                ForceEvent::LoadBoss(ch, cos) => {
                    // more players (OpenKumite's own): a boss each, the next stages' bosses beside this one's
                    let picks = force_boss_picks(&st.data, ctx, (ch, cos), bout.game.players().len());
                    let actors: Vec<kumite_game::Actor> = picks.iter().filter_map(|&(c, k)| cached_actor(assets, cache, model_of(c, k)?)).collect();
                    if actors.len() == picks.len() {
                        if actors.len() == 1 {
                            st.set_boss(&mut bout.game, &actors[0], (ch, cos));
                        } else {
                            st.set_bosses(&mut bout.game, &actors, &picks);
                            bout.extra_cpu = Some(kumite_sim::ai::Ai::new(d, l, 1));
                        }
                        bout.set_cpu(1, Some((d, l)));
                        events.extend(std::mem::take(&mut st.events));
                    } else {
                        log::warn!("the Force bosses {picks:?} did not all load");
                    }
                }
                // an extra stage: bosses drawn from its pool (the roster's, none twice, never the player's model),
                // by the stage's own random numbers
                // shortcut: not known ahead, so not fetched ahead (`fight_files`): in a browser, online, the first
                // load of each is a read inside the rollback step; extra stages come from players' own data
                ForceEvent::LoadBosses { pool, n } => {
                    let n = n.max(bout.game.players().len());
                    let mut pool: Vec<usize> = pool.iter().copied().filter(|&c| roster.by_id(c).is_some()).collect();
                    let mut picks = Vec::new();
                    while picks.len() < n && !pool.is_empty() {
                        st.rng = st.rng.wrapping_mul(0x41C6_4E6D).wrapping_add(12345);
                        let ch = pool.remove(((st.rng >> 16) & 0x7FFF) as usize % pool.len());
                        let c = roster.by_id(ch).map_or(1, |c| c.costumes.len());
                        let mine = roster.characters[ctx.player.0].id == ch;
                        let cos = if mine { menu::free_costume(c, 0, |k| k == ctx.player.1).unwrap_or(0) } else { 0 };
                        picks.push((ch, cos));
                    }
                    let actors: Vec<kumite_game::Actor> = picks.iter().filter_map(|&(c, k)| cached_actor(assets, cache, model_of(c, k)?)).collect();
                    if actors.len() == picks.len() && !actors.is_empty() {
                        st.set_bosses(&mut bout.game, &actors, &picks);
                        bout.set_cpu(1, Some((d, l)));
                        bout.extra_cpu = Some(kumite_sim::ai::Ai::new(d, l, 1));
                        events.extend(std::mem::take(&mut st.events));
                        log::info!("Force: bosses {picks:?}");
                    }
                }
                e => shown.push(e),
            }
        }
    }
    shown
}

/// Each stage's light from stages.json (the original's per-stage table 0x80096C78, as the GTE takes it): the
/// background colour (each channel the ambient >> 4, of 256), the key light's colour (of 256) and its direction (the
/// light matrix's row is the decoded direction negated: towards the light, y up, z flipped into the metres' axes),
/// the material colour (128 = 1).
///
/// The shadows (0x80035F2C): the light the shadow falls from is RotMatrix(0, -A, -B) applied to x, A and B the
/// table's angles at +0x16 and +0x14 (4096 a turn): (cos A cos B, -sin B, sin A cos B) in the game's axes; their
/// colour the table's +0x10 (opaque flat polygons).
fn stage_lights(gd: &Path) -> std::collections::HashMap<usize, (Option<kumite_render::StageLight>, Option<kumite_render::Shadow>, Option<[u8; 3]>)> {
    let json: serde_json::Value = kumite_fs::read(&gd.join("stages.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let f = art::num;
    let rgb = |v: &serde_json::Value| glam::Vec3::new(f(&v[0]), f(&v[1]), f(&v[2])) / 255.0;
    json["stages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s["id"].as_u64()? as usize;
            let Some(l) = s.get("light") else {
                // a stage without a light table: its own shadow entry (towards the light, y up), if any
                let sh = s.get("shadow")?;
                let t = &sh["toward"];
                let toward = glam::Vec3::new(f(&t[0]), f(&t[1]), f(&t[2])).normalize_or_zero();
                return Some((id, (None, Some(kumite_render::Shadow { toward, floor: 0.0, colour: rgb(&sh["colour"]) }), None)));
            };
            let amb = (l["ambient"].as_u64()? >> 4) as f32 / 256.0;
            let key = &l["key_color_rgb"];
            let mat = &l["material_rgbc"];
            let kd = &l["key_direction_raw"];
            let toward = glam::Vec3::new(-f(&kd[0]), f(&kd[1]), f(&kd[2])).normalize_or_zero();
            let light = kumite_render::StageLight {
                ambient: glam::Vec3::splat(amb),
                key: glam::Vec3::new(f(&key[0]), f(&key[1]), f(&key[2])) / 256.0,
                toward,
                material: glam::Vec3::new(f(&mat[0]), f(&mat[1]), f(&mat[2])) / 128.0,
            };
            let sa = &l["shadow_angles"];
            let turn = |v: f32| v * std::f32::consts::TAU / 4096.0;
            let (b, a) = (turn(f(&sa[0])), turn(f(&sa[1])));
            // (the game's axes to the metres': y up, z flipped)
            let shadow = kumite_render::Shadow { toward: glam::Vec3::new(a.cos() * b.cos(), b.sin(), -(a.sin() * b.cos())), floor: 0.0, colour: rgb(&l["unknown_10"]) };
            let blob = [0, 1, 2].map(|k| l["unknown_18"][k].as_u64().unwrap_or(0) as u8);
            Some((id, (Some(light), Some(shadow), Some(blob))))
        })
        .collect()
}

/// The stage's light and the fighters' shadows on a camera.
fn scene_lighting(cam: &mut Camera, scene: &Scene) {
    cam.light = scene.light;
    cam.shadow = scene.shadow.filter(|_| shadows_shown(cam));
}

/// The shadows are drawn only while the camera looks no more than 0xEF (21 degrees) up (0x80048E9C's flag
/// 0x800ADFD0: sin(pitch + 0xEF) > 0).
fn shadows_shown(cam: &Camera) -> bool {
    let down = -(cam.target - cam.eye).normalize_or_zero().y.clamp(-1.0, 1.0).asin();
    (down + 0xEF as f32 * std::f32::consts::TAU / 4096.0).sin() > 0.0
}

/// The stage's view for a camera framing two points `pair` (metres; a fight's two fighters, the tag brawl's framing
/// extremes): the backdrop drum (or Ball's strip, an extra stage's drum around the camera) and the floor
/// around their midpoint, the floor's light and patch, an arena's walls (the camera kept over free floor, what
/// stands before the fighters cut away).
fn stage_view(cam: &mut Camera, scene: &Scene, pair: [glam::Vec3; 2], free_cam: bool) {
    // the backdrop drum follows the midpoint, its base below the floor
    let mid = (pair[0] + pair[1]) / 2.0;
    cam.sky = glam::Mat4::from_translation(glam::Vec3::new(mid.x, -scene.sky_base_y, mid.z));
    cam.sky_y_scale = scene.sky_y_scale;
    cam.sky_first = scene.sky_first;
    cam.background = Some(scene.background);
    // Ball's 2D sky strip (0x800B5A48): a drum around the camera, its bottom on the point
    // `ahead` in front at the strip's height, `px` screen rows tall; drawn before the floor
    if let Some([ahead, y, px]) = scene.strip {
        let radius = ahead / kumite_game::UNITS;
        let h = px * radius * (cam.fov_y * 0.5).tan() / 240.0;
        cam.sky = glam::Mat4::from_translation(glam::Vec3::new(cam.eye.x, -y / kumite_game::UNITS, cam.eye.z)) * glam::Mat4::from_scale(glam::Vec3::new(radius, h, radius));
        // the original's floor is a finite 10 x 10 tile patch around the midpoint (0x8004994C):
        // the strip shows beyond it
        let tile = 1.8;
        let (x0, z0) = (((mid.x / tile).floor() - 5.0) * tile, ((mid.z / tile).floor() - 5.0) * tile);
        cam.floor_clip = Some([x0, x0 + 10.0 * tile, z0, z0 + 10.0 * tile]);
        (cam.sky_y_scale, cam.sky_first, cam.sky_cull) = (1.0, true, false);
    }
    // an extra stage (0x80084560): the panorama drum around the camera, drawn first; the floor only on
    // the 10 x 10 tile patch around the midpoint snapped to whole tiles (0x80082E60 / 0x8008364C)
    if scene.camera_drum {
        cam.sky = glam::Mat4::from_translation(cam.eye);
        (cam.sky_y_scale, cam.sky_first, cam.sky_cull) = (1.0, true, false);
    }
    // an arena with walls: the camera stays over the free floor. From the fighters' midpoint towards the eye,
    // the free run h (to 0.3 m short of the first blocked cell); when the eye's distance D does not fit, it
    // rises keeping D (so both fighters stay framed) until it stands over the free floor, looking at the
    // midpoint chest high
    if let (Some(w), false) = (scene.camera_walls.as_ref(), free_cam) {
        let centre = glam::Vec3::new(mid.x, 1.0, mid.z);
        let flat = glam::Vec3::new(cam.eye.x - mid.x, 0.0, cam.eye.z - mid.z);
        let (len, dir) = (flat.length(), flat.normalize_or_zero());
        let mut h = 0.0;
        while h + 0.1 <= len && !w.blocked((mid + dir * (h + 0.1)) * kumite_game::UNITS) {
            h += 0.1;
        }
        if h + 0.1 <= len {
            let room = (h - 0.3).max(0.0);
            let d = (cam.eye - centre).length();
            let up = (room / d).clamp(0.0, 1.0).acos();
            cam.eye = centre + dir * (d * up.cos()) + glam::Vec3::Y * (d * up.sin());
            cam.target = centre;
            cam.cutaway_all = true;
        }
    }
    // an arena with walls: cut away what stands between the camera and the fighters
    cam.near_cut = scene.near_cut;
    scene_lighting(cam, scene);
    if scene.walls.is_some() || scene.cutaway {
        let at = |i: usize| glam::Vec3::new(pair[i].x, 0.0, pair[i].z);
        cam.cutaway = Some(([at(0), at(1)], 0.5));
    }
    if let Some((mode, shade, table)) = scene.floor_light {
        let f = |i: usize| [pair[i].x, pair[i].z];
        cam.floor_light = Some(kumite_render::FloorLight { mode, shade, fighters: [f(0), f(1)], table });
    }
    if let Some([tile, n]) = scene.floor_patch {
        let (cx, cz) = ((mid.x / tile).round() * tile, (mid.z / tile).round() * tile);
        let h = n * tile / 2.0;
        cam.floor_clip = Some([cx - h, cx + h, cz - h, cz + h]);
    }
    // the floor is laid around the midpoint (0x8004994C), so the arena never ends
    if let Some([px, pz]) = scene.floor_period {
        let snap = |v: f32, p: f32| (v / p).round() * p;
        cam.floor = glam::Mat4::from_translation(glam::Vec3::new(snap(mid.x, px), 0.0, snap(mid.z, pz)));
    }
}

/// The pause items shown: screens.json hud.pause.items' CANCEL and RESET (COMMAND, the move list, is not done);
/// Ball adds ball_items' RULES.
const PAUSE_ITEMS: [usize; 2] = [0, 2];

/// The game's pause box (0x800780BC): a grey-edged navy box with the items in font 0 (the selected one red),
/// and "nP PAUSE" blinking in font 1 on the side of the player who paused.
/// Ball's rules card (0x800B3BC0): a black shadow, a white border and the card (record 55's picture).
fn draw_rules(ctx: &egui::Context, art: &Art) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("rules")));
    let v = art::Screen::fit(ctx.content_rect());
    let painter = painter.with_clip_rect(v.rect);
    Art::shade(&painter, &v, [60.0, 132.0, 264.0, 220.0], [egui::Color32::BLACK; 4]);
    Art::shade(&painter, &v, [52.0, 120.0, 264.0, 220.0], [egui::Color32::WHITE; 4]);
    if let Some((tex, [w, h])) = art.pic("r055_raw/000.png") {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex.id(), v.rect(56.0, 128.0, w as f32, h as f32), uv, egui::Color32::WHITE);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_pause(ctx: &egui::Context, art: &Art, items: &[String], cursor: usize, player: usize, frame: u64, ball: bool) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("pause")));
    let v = art::Screen::fit(ctx.content_rect());
    let painter = painter.with_clip_rect(v.rect);
    let mut shown: Vec<&str> = PAUSE_ITEMS.iter().map(|&k| items.get(k).map_or("", String::as_str)).collect();
    if ball {
        shown.push(items.get(3).map_or("RULES", String::as_str));
    }
    let n = shown.len() as f32;
    Art::shade(&painter, &v, [147.0, 98.0, 74.0, 9.0 + 20.0 * n], [egui::Color32::from_gray(0xE0); 4]);
    Art::shade(&painter, &v, [148.0, 100.0, 72.0, 5.0 + 20.0 * n], [egui::Color32::from_rgb(0, 0x10, 0x30); 4]);
    for (k, item) in shown.iter().enumerate() {
        let colour = if k == cursor { 2 } else { 6 };
        art.text(&painter, &v, 0, colour, crate::art::CENTRE - (9 * item.len() / 2) as f32, 104.0 + 20.0 * k as f32, item, egui::Color32::WHITE);
    }
    if frame & 0x20 != 0 {
        let x = if player == 0 { 32.0 } else { 232.0 };
        let header = art::printf(art.screens["hud"]["pause"]["header"].as_str().unwrap_or_default(), &[player as u32 + 1]);
        art.text(&painter, &v, 1, 5, x, 120.0, &header, egui::Color32::WHITE);
    }
}

/// `--keys "30:start,60:b1,90:shot"`: menu keys (up down left right start back b1..b4) or a screenshot
/// (`shot`) at drawn-frame numbers.
/// `--keys "30:down,40:p3:start"`: frame:key (1P's), or frame:pN:key (player N's).
fn parse_keys(s: &str) -> Result<Vec<(u64, usize, Option<Key>)>> {
    let mut out = Vec::new();
    for item in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let (f, k) = item.split_once(':').with_context(|| format!("--keys item {item:?}: frame:key"))?;
        let (player, k) = match k.split_once(':') {
            Some((p, k)) if p.starts_with('p') => (p[1..].parse::<usize>().context("--keys pN: player 1..4")?.clamp(1, 4) - 1, k),
            _ => (0, k),
        };
        let key = match k {
            "up" => Some(Key::Up),
            "down" => Some(Key::Down),
            "left" => Some(Key::Left),
            "right" => Some(Key::Right),
            "start" => Some(Key::Start),
            "back" => Some(Key::Back),
            "b1" | "b2" | "b3" | "b4" => Some(Key::Button(k[1..].parse::<usize>()? - 1)),
            "shot" => None,
            other => anyhow::bail!("--keys: unknown key {other:?}"),
        };
        out.push((f.parse()?, player, key));
    }
    out.sort_by_key(|k| k.0);
    Ok(out)
}

/// `--play "40:2:common_162,90:1:motion_020"`: fight step, player (1/2), animation name of the move to start.
fn parse_play(s: &str) -> Result<Vec<(u64, usize, String)>> {
    let mut out = Vec::new();
    for item in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let mut parts = item.splitn(3, ':');
        let (Some(f), Some(p), Some(anim)) = (parts.next(), parts.next(), parts.next()) else { anyhow::bail!("--play item {item:?}: step:player:anim") };
        let player: usize = p.parse()?;
        anyhow::ensure!((1..=2).contains(&player), "--play: player 1 or 2");
        out.push((f.parse()?, player - 1, anim.to_string()));
    }
    out.sort_by_key(|p| p.0);
    Ok(out)
}

/// The effect atlas from the converted hit-spark sheets' index maps (EXE pictures 007 / 008).
fn effect_atlas(r: &Renderer, pictures: &Path) -> Option<Arc<kumite_render::Texture>> {
    // the effect cells in the game's palettes (export_effects)
    let mut dec = png::Decoder::new(std::io::Cursor::new(kumite_fs::read(&pictures.join("effects/atlas.png")).ok()?));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    (info.color_type == png::ColorType::Rgba).then(|| r.texture(info.width, info.height, &buf[..(info.width * info.height * 4) as usize]))
}

/// Effect sprites as triangles: camera-facing quads, or quads flat on the floor (turned by their angle).
fn effect_quads(sprites: &[kumite_game::vfx::Sprite], (cols, rows): (u32, u32), cam: &Camera, alpha: &mut Vec<StaticVertex>, add: &mut Vec<StaticVertex>) {
    use kumite_game::vfx::Blend;
    let forward = (cam.target - cam.eye).normalize_or_zero();
    let right = forward.cross(glam::Vec3::Y).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    for s in sprites {
        // drawn `nearer` the camera along the line of sight, scaled to keep its place and size on screen
        let depth = (s.pos - cam.eye).dot(forward);
        let k = if depth > s.nearer + 0.1 { (depth - s.nearer) / depth } else { 1.0 };
        let pos = cam.eye + (s.pos - cam.eye) * k;
        let h = s.size * 0.5 * k;
        let (cx, cy) = ((s.cell % cols) as f32, (s.cell / cols) as f32);
        let (mut u0, v0) = (cx / cols as f32, cy / rows as f32);
        let (mut u1, v1) = (u0 + 1.0 / cols as f32, v0 + 1.0 / rows as f32);
        if s.mirror {
            std::mem::swap(&mut u0, &mut u1);
        }
        let corner = |dx: f32, dy: f32, u: f32, v: f32| StaticVertex { pos: (pos + right * dx * h + up * dy * h).to_array(), uv: [u, v], color: s.color };
        let quad = [corner(-1.0, 1.0, u0, v0), corner(1.0, 1.0, u1, v0), corner(-1.0, -1.0, u0, v1), corner(1.0, -1.0, u1, v1)];
        let out = if s.blend == Blend::Add { &mut *add } else { &mut *alpha };
        out.extend([quad[0], quad[2], quad[1], quad[1], quad[2], quad[3]]);
    }
}

fn unix_time() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Save a captured frame as `openkumite_screenshots/shot_<time>_<n>.png`.
fn save_screenshot(img: &kumite_render::Image) -> Result<std::path::PathBuf> {
    let dir = Path::new("openkumite_screenshots");
    std::fs::create_dir_all(dir)?;
    let mut n = 0;
    let path = loop {
        let p = dir.join(format!("shot_{}_{n}.png", unix_time()));
        if !p.exists() {
            break p;
        }
        n += 1;
    };
    write_png(&path, img)?;
    Ok(path)
}

fn write_png(path: &Path, img: &kumite_render::Image) -> Result<()> {
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&img.rgba)?;
    Ok(())
}

impl App {
    /// The window and renderer are ready: the overlay, the sandbox fight, the clock.
    fn start(&mut self, event_loop: &ActiveEventLoop, window: Arc<Window>, mut renderer: Renderer) {
        // (in a browser the canvas may have been resized while the GPU was being set up)
        let size = window.inner_size();
        renderer.resize(size.width, size.height);
        self.egui = Some(EguiLayer::new(renderer.device(), renderer.overlay_format(), Some(&window)));
        if self.sandbox {
            if let Err(e) = self.load(&renderer, None, Vec::new()) {
                log::error!("loading the fight: {e:#}");
                event_loop.exit();
            }
        }
        self.renderer = Some(renderer);
        self.window = Some(window);
        self.last = Instant::now();
        self.refit();
        // (applied: later refits keep what the player chooses)
        self.graphics_preset = None;
    }

    /// The display and the GPU as they are now (the window may have moved to another screen, the page resized).
    fn query_display(&mut self) {
        let monitor = self.window.as_ref().and_then(|w| w.current_monitor().or_else(|| w.primary_monitor()));
        let max = self.renderer.as_ref().map_or(u32::MAX, Renderer::max_texture);
        self.display = graphics::Display::query(monitor, max);
        self.display.gpu = self.renderer.as_ref().map_or(graphics::Gpu::Unknown, |r| graphics::Gpu::of(r.device_type()));
    }

    /// `--graphics recommended|default` for the display as now known, the command line's other options over it.
    fn preset_graphics(&self) -> Option<graphics::Graphics> {
        let base = self.display.preset_over(self.graphics_preset?, &self.graphics);
        let args = &self.graphics_args;
        let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
        let g = base.clone().with_args(arg, |name| args.iter().any(|a| a == name)).unwrap_or(base);
        Some(self.display.fit(g))
    }

    /// Settings the display and the GPU can show, applied (not saved: the save keeps what was chosen).
    fn refit(&mut self) {
        self.query_display();
        let g = self.preset_graphics().unwrap_or_else(|| self.display.fit(self.graphics.clone()));
        if g != self.graphics {
            log::info!("graphics fitted to the display: {g:?}");
            self.apply_graphics(&g);
            self.graphics = g;
        }
    }

    /// In a browser: full screen asked for before, now on the player's input (the browser allows it then).
    #[cfg(target_arch = "wasm32")]
    fn enter_pending_fullscreen(&mut self) {
        if let (true, Some(w)) = (self.fullscreen_pending, self.window.as_ref()) {
            if w.fullscreen().is_none() && self.graphics.display != graphics::DisplayMode::Window {
                w.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            }
        }
    }

    /// The browser made full screen by the player (not by the game).
    fn page_full_screen(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        return self.page_fullscreen && self.graphics.display == graphics::DisplayMode::Window;
        #[cfg(not(target_arch = "wasm32"))]
        false
    }

    /// In a browser: the page's view changed (the browser window resized, made full screen by the player or back):
    /// the resolutions offered follow it; the browser full screen, the canvas fills it.
    #[cfg(target_arch = "wasm32")]
    fn watch_page(&mut self) {
        let Some(w) = web_sys::window() else { return };
        let num = |v: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| v.ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as u32;
        let view = [num(w.inner_width()), num(w.inner_height())];
        let api = w.document().is_some_and(|d| d.fullscreen_element().is_some());
        if (view == self.page_view && api == self.page_api) || view[0] == 0 || view[1] == 0 {
            return;
        }
        self.page_view = view;
        self.page_api = api;
        self.query_display();
        // full screen by the browser (F11, its menu): its display mode, or its window covering the screen (both in
        // device-independent pixels, whatever the page's zoom), or an element other than our canvas full screen
        let mode = w.match_media("(display-mode: fullscreen)").ok().flatten().is_some_and(|m| m.matches());
        let covers = w.screen().ok().is_some_and(|sc| {
            let (sw, sh) = (sc.width().unwrap_or(i32::MAX), sc.height().unwrap_or(i32::MAX));
            num(w.outer_width()) as i32 >= sw && num(w.outer_height()) as i32 >= sh
        });
        let ours = self.window.as_ref().is_some_and(|w| w.fullscreen().is_some());
        let full = !ours && (mode || covers || api);
        if full != self.page_fullscreen {
            log::info!("browser full screen: {full} (page {}x{})", view[0], view[1]);
        }
        self.page_fullscreen = full;
        if self.graphics.display != graphics::DisplayMode::Window {
            return;
        }
        let g = self.display.fit(self.graphics.clone());
        self.size_canvas(&g);
        if g != self.graphics {
            if let Some(r) = self.renderer.as_mut() {
                r.set_settings(g.render());
            }
            self.graphics = g;
        }
    }

    /// In a browser, a window: the canvas at the chosen size, or filling the browser's full screen.
    #[cfg(target_arch = "wasm32")]
    fn size_canvas(&self, g: &graphics::Graphics) {
        let Some(w) = self.window.as_ref() else { return };
        let [cw, ch] = if self.page_fullscreen { self.page_view } else { g.resolution };
        let _ = w.request_inner_size(winit::dpi::LogicalSize::new(cw as f64, ch as f64));
    }

    /// Put settings into effect: the window (full screen, size), the renderer.
    fn apply_graphics(&mut self, g: &graphics::Graphics) {
        let window_changed = g.display != self.graphics.display || g.resolution != self.graphics.resolution;
        if let (true, Some(w)) = (window_changed, self.window.as_ref()) {
            g.apply(w);
            // (the browser full screen by the player: the canvas keeps filling it)
            #[cfg(target_arch = "wasm32")]
            if g.display == graphics::DisplayMode::Window && self.page_fullscreen {
                self.size_canvas(g);
            }
            // a browser enters full screen only on the player's input: asked again on the next one
            #[cfg(target_arch = "wasm32")]
            {
                self.fullscreen_pending = g.display != graphics::DisplayMode::Window;
            }
        }
        if let Some(r) = self.renderer.as_mut() {
            r.set_settings(g.render());
        }
    }
}

impl App {
    /// The window, and the renderer (in a browser it arrives later, `about_to_wait`).
    fn on_resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // the settings fitted to the screen the window opens on (the GPU's limit follows with the renderer)
        let monitor = event_loop.primary_monitor().or_else(|| event_loop.available_monitors().next());
        self.display = graphics::Display::query(monitor.clone(), u32::MAX);
        self.graphics = self.preset_graphics().unwrap_or_else(|| self.display.fit(self.graphics.clone()));
        #[cfg(target_arch = "wasm32")]
        {
            self.fullscreen_pending = self.graphics.display != graphics::DisplayMode::Window;
        }
        let attrs = self.graphics.window_attributes(Window::default_attributes().with_title("OpenKumite"), monitor);
        // Windows: the title bar's and the taskbar's icon, the .exe's own (app/openkumite.rc)
        #[cfg(windows)]
        let attrs = {
            use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
            match winit::window::Icon::from_resource(1, None) {
                Ok(icon) => attrs.with_window_icon(Some(icon.clone())).with_taskbar_icon(Some(icon)),
                Err(e) => {
                    log::warn!("no window icon: {e}");
                    attrs
                }
            }
        };
        // in a browser: a canvas added to the page
        #[cfg(target_arch = "wasm32")]
        let attrs = winit::platform::web::WindowAttributesExtWebSys::with_append(attrs, true);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("no window: {e}");
                eprintln!("OpenKumite: no window could be opened ({e})");
                #[cfg(target_arch = "wasm32")]
                page_error(&format!("The game did not start: no canvas could be made ({e})"));
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut renderer = match Renderer::new(window.clone(), size.width, size.height) {
                Ok(r) => r,
                Err(e) => {
                    log::error!("no renderer: {e:#}");
                    eprintln!("OpenKumite: the graphics could not start ({e:#})");
                    event_loop.exit();
                    return;
                }
            };
            renderer.set_settings(self.graphics.render());
            self.start(event_loop, window, renderer);
        }
        // a browser cannot wait for the GPU: the renderer arrives later (`about_to_wait`)
        #[cfg(target_arch = "wasm32")]
        {
            let slot = self.pending_renderer.clone();
            let render = self.graphics.render();
            let w = window.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match Renderer::new_async(w, size.width.max(1), size.height.max(1)).await {
                    Ok(mut r) => {
                        r.set_settings(render);
                        *slot.borrow_mut() = Some(r)
                    }
                    Err(e) => {
                        log::error!("renderer: {e:#}");
                        page_error(&format!("The graphics could not start (WebGPU or WebGL 2 is needed): {e:#}"));
                    }
                }
            });
            self.window = Some(window);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.guarded(|app| app.on_resumed(event_loop));
        // (a panic before the window was kept: nothing to go back to)
        if self.window.is_none() {
            event_loop.exit();
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // (a bug here does not end the game: back to the main menu)
        self.guarded(|app| app.on_window_event(event_loop, event));
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let (Some(d), Some(path)) = (&self.demo_rec, &self.demo_out) {
            match d.save(path) {
                Ok(()) => log::info!("demo saved: {} ({} steps)", path.display(), d.len()),
                Err(e) => log::error!("saving the demo: {e:#}"),
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.guarded(|app| app.on_about_to_wait(event_loop));
    }
}

impl App {
    fn on_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let consumed = match (&mut self.egui, &self.window) {
            (Some(e), Some(w)) => e.on_event(w, &event),
            _ => false,
        };
        let ui_keyboard = self.egui.as_ref().is_some_and(|e| e.wants_keyboard());
        let ui_pointer = self.egui.as_ref().is_some_and(|e| e.wants_pointer());
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(s) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(s.width, s.height);
                }
                // in a browser: full screen entered, or left by the browser (Esc): the setting follows
                #[cfg(target_arch = "wasm32")]
                if let Some(w) = &self.window {
                    let full = w.fullscreen().is_some();
                    if full {
                        self.fullscreen_pending = false;
                    } else if !self.fullscreen_pending && self.graphics.display != graphics::DisplayMode::Window {
                        let mut g = self.graphics.clone();
                        g.display = graphics::DisplayMode::Window;
                        self.set_graphics(g.clone());
                        if let Some(Screen::Graphics(m)) = self.screen.as_mut() {
                            m.settings = g;
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(code), state, repeat, ref text, .. }, .. } => {
                // (a system dialog just closed: the key that closed it is not the game's)
                if self.keys_after.is_some_and(|t| Instant::now() < t) {
                    return;
                }
                // ONLINE's code / address being typed: the letters and digits go there, not to the controls
                if let Some(Screen::Online(o)) = self.screen.as_mut() {
                    if o.typing() && state == ElementState::Pressed {
                        if code == KeyCode::Backspace {
                            o.type_char('\u{8}');
                            return;
                        }
                        if let Some(c) = text.as_ref().and_then(|t| t.chars().next()).filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':')) {
                            o.type_char(c);
                            return;
                        }
                    }
                }
                #[cfg(target_arch = "wasm32")]
                if state == ElementState::Pressed {
                    self.enter_pending_fullscreen();
                }
                // R (with --debug): held, the fight runs backwards; a tap while paused goes one frame back
                if code == KeyCode::KeyR && self.debug.enabled && !ui_keyboard && self.net.is_none() {
                    self.debug.rewinding = state == ElementState::Pressed;
                    if state == ElementState::Pressed && !repeat && self.debug.paused && !self.rewind() {
                        self.debug.say("nothing to rewind");
                    }
                    return;
                }
                // the soft reset on the keyboard: a START key held (Enter, the layouts' START) and Esc / Backspace
                if state == ElementState::Pressed && !repeat && matches!(code, KeyCode::Escape | KeyCode::Backspace) {
                    let starts = self.records.controls.keyboard.iter().flat_map(|l| l.keys_of(controls::Command::Start).to_vec()).chain([KeyCode::Enter]);
                    if starts.into_iter().any(|k| self.keys.contains(&k)) && self.soft_reset() {
                        return;
                    }
                }
                // KEY CONFIG waiting for a key: the key itself, before any layout
                if state == ElementState::Pressed && !repeat {
                    if let Some(Screen::Keyboard(k)) = self.screen.as_mut().filter(|s| matches!(s, Screen::Keyboard(k) if k.waiting)) {
                        if let Some(o) = k.capture(code) {
                            self.outcome(o);
                        }
                        return;
                    }
                }
                if state == ElementState::Pressed && !ui_keyboard && self.replay_hotkey(code, repeat) {
                    return;
                }
                if state == ElementState::Pressed && !repeat && !ui_keyboard && self.hotkey(code) {
                    return;
                }
                if (consumed || ui_keyboard) && state == ElementState::Pressed {
                    return; // typing into the debug panel
                }
                // menus (and the pause menu): Esc / Enter always; the other keys only outside the fight
                let in_menu = self.in_menu();
                if state == ElementState::Pressed && !repeat {
                    // Force: P2's START key (right Shift / numpad Enter) is the second player's (the co-op join)
                    let p2_start = self.force_fight() && self.records.controls.keyboard[1].commands_of(code).any(|c| c == controls::Command::Start);
                    let mapped = self.records.controls.menu_key(code);
                    if let Some(k) = if p2_start { Some(Key::Start) } else { mapped.map(|m| m.1) } {
                        // on the VS select screen the second keyboard layout moves P2's cursor
                        // (KEY CONFIGURATION: each layout drives its player's panel)
                        let versus = matches!(&self.screen, Some(Screen::Select(s)) if s.mode == Mode::Versus) || matches!(&self.screen, Some(Screen::Grid(_) | Screen::KeyConfig(_)));
                        let player = usize::from(versus && mapped.is_some_and(|m| m.0 == 1) || p2_start);
                        if (in_menu || matches!(k, Key::Back | Key::Start)) && !self.input_key(player, k) {
                            event_loop.exit();
                        }
                    }
                }
                match state {
                    ElementState::Pressed => {
                        self.keys.insert(code);
                        self.tapped.insert(code);
                    }
                    ElementState::Released => {
                        self.keys.remove(&code);
                    }
                }
            }
            // replay files dropped on the window: imported
            WindowEvent::DroppedFile(path) if path.extension().is_some_and(|e| e == files::REPLAY.ext) => {
                if let Ok(bytes) = std::fs::read(&path) {
                    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    self.import_replays(vec![(name, bytes)]);
                }
            }
            WindowEvent::Focused(false) => {
                self.keys.clear();
                self.debug.rewinding = false;
            }
            WindowEvent::MouseInput { state, button: button @ (MouseButton::Left | MouseButton::Right), .. } => {
                #[cfg(target_arch = "wasm32")]
                if state == ElementState::Pressed {
                    self.enter_pending_fullscreen();
                }
                self.drag = (state == ElementState::Pressed && !ui_pointer && self.debug.free_cam.is_some()).then_some(self.cursor);
                self.drag_pan = button == MouseButton::Right;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if let (Some((x, y)), Some(fc)) = (self.drag, self.debug.free_cam.as_mut()) {
                    let (dx, dy) = ((position.x - x) as f32, (position.y - y) as f32);
                    if self.drag_pan {
                        // right drag moves the orbit centre in the view plane
                        let right = glam::Vec3::new(fc.yaw.cos(), 0.0, -fc.yaw.sin());
                        fc.pan += (-right * dx + glam::Vec3::Y * dy) * fc.distance * 0.0015;
                    } else {
                        fc.yaw -= dx * 0.008;
                        // drag up raises the camera; it stays above the floor
                        fc.pitch = (fc.pitch - dy * 0.008).clamp(0.03, 1.5);
                    }
                    self.drag = Some(self.cursor);
                }
            }
            WindowEvent::MouseWheel { delta, .. } if !ui_pointer => {
                if let Some(fc) = self.debug.free_cam.as_mut() {
                    let d = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    fc.distance = (fc.distance * (1.0 - d * 0.1)).clamp(1.0, 40.0);
                }
            }
            WindowEvent::RedrawRequested => {
                // gamepad buttons drive the menus like the keyboard; so do scripted keys (--keys)
                let pressed = self.gamepads.as_mut().map(|g| g.poll(&self.records.controls)).unwrap_or_default();
                let mut keys: Vec<(usize, Key)> = pressed.iter().filter_map(|&(p, _, k)| Some((p, k?))).filter(|(p, _)| *p < 4).collect();
                // the soft reset: START held, SELECT pressed (L1 / R1 may be held too)
                if pressed.iter().any(|&(p, b, _)| {
                    b == controls::PadButton::Select
                        && self.gamepads.as_ref().and_then(|g| g.held()[p].clone()).is_some_and(|held| {
                            use controls::PadButton as B;
                            held.contains(&B::Start) && held.iter().all(|b| matches!(b, B::Start | B::Select | B::L1 | B::R1))
                        })
                }) && self.soft_reset()
                {
                    keys.clear();
                }
                // KEY CONFIGURATION: the buttons each player's pad holds open their lists
                if let Some(Screen::KeyConfig(k)) = self.screen.as_mut() {
                    let held = self.gamepads.as_ref().map(|g| g.held()).unwrap_or_default();
                    k.hold([held[0].as_deref(), held[1].as_deref()]);
                    // (a pad's face buttons on SETTING hold, they do not pick)
                    let on_setting = [k.on_setting(0), k.on_setting(1)];
                    keys.retain(|(p, key)| !(matches!(key, Key::Button(_)) && on_setting[(*p).min(1)]));
                }
                self.frames += 1;
                while self.key_script.first().is_some_and(|(f, _, _)| *f <= self.frames) {
                    match self.key_script.remove(0) {
                        (_, p, Some(k)) => keys.push((p, k)),
                        (_, _, None) => self.debug.actions.push(Action::Screenshot),
                    }
                }
                if self.quit_in == Some(0) {
                    event_loop.exit();
                }
                if self.quit_after.is_some_and(|q| self.frames >= q) {
                    let fight = self.fight.as_ref().map(|f| (f.bout.phase, f.bout.game.fight.frame, f.bout.wins));
                    log::info!("quitting after {} drawn frames, {} fight steps; fight {fight:?}; occluded frames {}", self.frames, self.steps, self.debug.stats.skipped);
                    event_loop.exit();
                }
                for (player, k) in keys {
                    let in_menu = self.in_menu();
                    if (in_menu || matches!(k, Key::Back | Key::Start)) && !self.input_key(player, k) {
                        event_loop.exit();
                    }
                }
                let now = Instant::now();
                // (online: no debug pause or speed: the others run at 60 a second)
                if self.net.is_some() {
                    (self.debug.paused, self.debug.speed, self.debug.rewinding) = (false, 1.0, false);
                }
                // a match over here: its session a while more, its last inputs sent again
                if let Some(l) = self.net_linger.as_mut() {
                    l.net.poll();
                    if let (Some(room), Some(session)) = (self.room.as_ref(), l.session) {
                        if l.sent.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) {
                            room.link.borrow_mut().resend_inputs(session);
                            l.sent = Some(Instant::now());
                        }
                    }
                    if l.since.elapsed() >= LINGER {
                        self.net_linger = None;
                    }
                }
                let steps = self.debug.steps_for(&mut self.lag, now - self.last, STEP);
                self.last = now;
                for _ in 0..steps {
                    if self.debug.rewinding {
                        self.rewind();
                    } else {
                        self.step();
                    }
                }
                self.draw();
            }
            _ => {}
        }
    }

    fn on_about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Poll);
        #[cfg(target_arch = "wasm32")]
        if self.renderer.is_none() {
            let ready = self.pending_renderer.borrow_mut().take();
            if let (Some(r), Some(w)) = (ready, self.window.clone()) {
                self.start(event_loop, w, r);
            }
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(a) = &mut self.audio {
            a.resume();
        }
        #[cfg(target_arch = "wasm32")]
        if self.renderer.is_some() {
            self.watch_page();
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Run `f`; should the game's code panic in it (a bug), the game goes on: whatever was being played is
    /// dropped (the fight, the room, the run), the renderer made again if it was out, and the main menu comes back
    /// with a note. (Native: a browser build aborts on a panic.)
    fn guarded(&mut self, f: impl FnOnce(&mut App)) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        if let Err(e) = result {
            let what = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_else(|| "?".into());
            log::error!("the game hit an error and goes back to the main menu: {what}");
            self.recover(&what);
        }
    }

    /// After a caught panic: back to a state that works.
    fn recover(&mut self, what: &str) {
        if let Some(mut r) = self.room.take() {
            r.leave();
        }
        (self.net, self.fight, self.tag, self.scene, self.force, self.run, self.embu, self.waiting) = (None, None, None, None, None, None, None, None);
        // (a lingering session too: its poll may be what panicked, and it belongs to the room just dropped)
        self.net_linger = None;
        self.online_mode = None;
        if let Some(a) = self.audio.as_mut() {
            a.duck(false);
            a.music(None);
        }
        // (a load in progress had the renderer out)
        #[cfg(not(target_arch = "wasm32"))]
        if self.renderer.is_none() {
            if let Some(w) = self.window.clone() {
                let size = w.inner_size();
                match Renderer::new(w.clone(), size.width, size.height) {
                    Ok(mut r) => {
                        r.set_settings(self.graphics.render());
                        // (the overlay's textures belonged to the old device: made again, the pictures reloaded)
                        self.egui = Some(EguiLayer::new(r.device(), r.overlay_format(), Some(&w)));
                        self.art = None;
                        self.renderer = Some(r);
                    }
                    Err(e) => log::error!("the renderer could not be made again: {e:#}"),
                }
            }
        }
        self.screen = Some(Screen::menu(menu::difficulty()));
        self.debug.say(format!("ERROR: {what} (back to the main menu)"));
    }
}

/// `--export-video ID|latest --out FILE.mp4 [--size WxH] [--no-hud] [--free-cam YAW,PITCH,DIST,X,Y,Z]`: the replay
/// made a video, off-screen, a frame each game step (and a second more on its last), its sounds mixed (video.rs).
/// Prints `PROGRESS done total` as it goes (the game shows it), then `DONE`.
#[cfg(not(target_arch = "wasm32"))]
fn export_video(app: &mut App, id: &str, out: &Path, (width, height): (u32, u32), hud: bool, free_cam: Option<debug::FreeCam>) -> Result<()> {
    use std::io::Write;
    let entry = replay::list().into_iter().find(|e| e.id == id || id == "latest").with_context(|| format!("no replay {id}"))?;
    let total = entry.file.steps;
    let mut r = Renderer::headless(width, height)?;
    // (the picture at the video's size: the 3D at 1x, whatever the window's scale)
    r.set_settings(kumite_render::RenderSettings { scale: 1.0, ..app.graphics.render() });
    app.egui = Some(EguiLayer::new(r.device(), r.overlay_format(), None));
    app.audio = Some(audio::Audio::capture());
    app.renderer = Some(r);
    app.play_replay(entry);
    let v = app.replay_view.as_mut().context("the replay did not load (its game data, or its files)")?;
    (v.hud, v.bar) = (hud, false);
    app.debug.free_cam = free_cam;
    let mut writer = video::Writer::new(out, width, height)?;
    let all = total + video::FPS;
    // the progress: frames encoded (the rendering runs ahead of them), a few times a second until the end
    let (encoded, stop) = (writer.encoded(), Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let reporter = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut stdout = std::io::stdout();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = writeln!(stdout, "PROGRESS {} {all}", encoded.load(std::sync::atomic::Ordering::Relaxed));
                let _ = stdout.flush();
                std::thread::sleep(Duration::from_millis(250));
            }
        })
    };
    for step in 0..all {
        if step < total {
            if let Some(c) = app.audio.as_mut().and_then(|a| a.capture.as_mut()) {
                c.step = step;
            }
            app.step();
        }
        // (no message on the picture: this process's own, such as the save being another window's)
        app.debug.hush();
        app.draw();
        let img = app.renderer.as_ref().context("renderer")?.capture()?;
        writer.frame(img.rgba)?;
    }
    let (events, music) = app.audio.as_ref().map_or((Vec::new(), 0.55), |a| (a.capture.as_ref().map_or_else(Vec::new, |c| c.events.clone()), a.music_volume));
    let finished = writer.finish(&video::mix(&events, all, music));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = reporter.join();
    finished?;
    let mut stdout = std::io::stdout();
    let sounds = events.iter().filter(|e| matches!(e.1, audio::Event::Sfx(..))).count();
    let tracks = events.iter().filter(|e| matches!(e.1, audio::Event::Music(_))).count();
    log::info!("{}: {all} frames ({}), {sounds} sounds, {tracks} music changes", out.display(), replay::duration(all));
    let _ = writeln!(stdout, "DONE");
    Ok(())
}

/// `--shot out.png [--frames N] [--p1 script] [--draw ...] [--ui] [--screen
/// title|menu|options|select|vs|practice|hud]`: run headless and save a PNG. `--screen hud` plays an arcade match (P1
/// by script, P2 the CPU) for N frames.
#[cfg(not(target_arch = "wasm32"))]
fn screenshot(app: &mut App, path: &str, frames: u64, ui: bool, screen: Option<&str>) -> Result<()> {
    #[allow(unused_mut)]
    let mut r = Renderer::headless(960, 720)?;
    r.set_settings(app.graphics.render());
    app.egui = Some(EguiLayer::new(r.device(), r.overlay_format(), None));
    app.debug.show_ui = ui;
    // who the previews show (screens.json `previews`: character ids)
    let pv = app.roster.screens["previews"].clone();
    let id = |v: &serde_json::Value| v.as_u64().map_or(0, |i| i as usize);
    let previewed = app.roster.index_of(id(&pv["player"])).unwrap_or(0);
    match screen {
        Some("title") => app.screen = Some(Screen::title()),
        Some("presents") => app.screen = Some(Screen::Presents { frame: 0, next: menu::AttractNext::Embu }),
        // the main menu itself (--keys move its cursor)
        Some("main-menu") => app.screen = Some(Screen::menu(menu::difficulty())),
        Some("menu") => {
            let mut s = Screen::title();
            for _ in 0..2 {
                let _ = s.key(0, Key::Start, &app.roster, &mut app.seed);
            }
            app.screen = Some(s);
        }
        Some("options") => app.screen = Some(Screen::Options { frame: 0, cursor: 0, difficulty: 1 }),
        Some("keyconfig") => app.screen = Some(Screen::KeyConfig(Box::new(keyconfig::KeyConfig::new(app.records.controls.clone(), 1)))),
        Some("keyboard") => app.screen = Some(Screen::Keyboard(Box::new(keyconfig::KeyboardConfig::new(app.records.controls.clone(), 1)))),
        // Force stage --variant N (the stages, the final one, then OpenKumite's extra stages) with the previews'
        // player; --boss: straight to the boss
        Some("force") => {
            let index = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(0usize);
            let player = (previewed, 0);
            app.outcome(Outcome::StartForce(player, 1));
            let mut stage = 0;
            if let Some(run) = app.force.as_mut() {
                let last = run.data.final_index();
                (run.index, run.extra) = if index > last { (last.saturating_sub(1), Some(index - last - 1)) } else { (index, None) };
                stage = run.stage_id();
            }
            app.screen = None;
            let setup = FightSetup { mode: Mode::Force, players: [player, player], stage, difficulty: 1, level: 0, handicap: [100; 2], ball: None, partners: None };
            app.load_force(&r, setup)?;
            if std::env::args().any(|a| a == "--coop") {
                app.join_force_partner();
            }
            // --chicken: a chicken 1500 ahead of the player and one nearer the camera behind; and the three keys
            if std::env::args().any(|a| a == "--chicken") {
                if let Some(st) = app.force.as_mut().and_then(|r| r.stage.as_mut()) {
                    st.keys = 3;
                    st.drop_chicken(1500, 0);
                    st.drop_chicken(-1200, -1500);
                }
            }
            if std::env::args().any(|a| a == "--boss") {
                if let (Some(run), Some(fight)) = (app.force.as_mut(), app.fight.as_mut()) {
                    if let Some(st) = run.stage.as_mut() {
                        st.skip_to_boss(&mut fight.bout.game);
                    }
                }
            }
            // --tally: the stage cleared, its tally (with bonus time)
            if std::env::args().any(|a| a == "--tally") {
                if let Some(st) = app.force.as_mut().and_then(|r| r.stage.as_mut()) {
                    st.bonus_time = 1234;
                    st.start_tally();
                }
            }
        }
        // Force's results: --variant the run's flags (keys 1 / 2 / 4, the final character saved 8, unlocked before 16)
        Some("force-results") => {
            let flags = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(7u32);
            let stages = app.force_data.as_ref().map_or(0, |d| d.final_index());
            let bosses = (0..stages).map(|k| pv["force_bosses"].get(k).map(|b| (id(&b[0]), id(&b[1])))).collect();
            let r = forcerun::ForceResults { score: 123_450, hiscore: 123_450, new_hiscore: true, flags, bosses, kills: 99, t: 0, frame: 0, fade: 0, leaving: false };
            app.screen = Some(Screen::ForceResults(Box::new(r)));
        }
        // the records screen: --variant 0 / 1 / 2 the page; records-entry: a new Time Attack record's name entry (the
        // previews' player, --variant typed letters as keys after the slide), records-survival: a new Survival record
        Some(m @ ("records" | "records-entry" | "records-survival")) => {
            let variant = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(0u8);
            if app.records.usage.is_empty() {
                // a spread of plays for the CHARACTERS page
                for u in pv["usage"].as_array().into_iter().flatten() {
                    for _ in 0..id(&u[1]).min(99) {
                        app.records.played(id(&u[0]));
                    }
                }
            }
            match m {
                "records-entry" => {
                    app.records.time_attack(id(&pv["player"]), 2 * 3600 + 13 * 60 + 41);
                }
                "records-survival" => {
                    app.records.survival(id(&pv["survival"]), 25);
                }
                _ => app.records.page = variant % 3,
            }
            app.renderer = Some(r);
            app.start_records();
            r = app.renderer.take().context("renderer")?;
        }
        // THEATER MODE: --variant 0 a fresh save, 1 a few endings seen, 2 everything (the music player's button);
        // theater-music: the music player; staff-roll: the credits N frames in; ending: movie parameters
        // --variant P as the Theater plays it (its captions, letterbox)
        Some(m @ ("theater" | "theater-music")) => {
            let variant = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(1u8);
            app.records.endings_extra = if variant == 2 { u32::MAX } else { 0 };
            // (screens.json `previews.theater`: the endings seen as character ids, those of everyone and the alternates)
            let bits = |v: &serde_json::Value| v.as_array().into_iter().flatten().filter_map(|b| b.as_u64()).filter(|&b| b < 32).fold(0u32, |a, b| a | 1 << b);
            let th = &pv["theater"];
            (app.records.endings, app.records.alternate) = match (m, variant) {
                ("theater-music", _) | (_, 2) => (bits(&th["all"]), bits(&th["alternates"])),
                (_, 1) => (bits(&th["some"]), 0),
                _ => (0, 0),
            };
            app.outcome(Outcome::OpenTheater);
            if let Some(Screen::Theater(t)) = app.screen.as_mut() {
                if m == "theater-music" {
                    t.key(Key::Down);
                    t.key(Key::Down);
                    t.key(Key::Down);
                    t.key(Key::Down);
                    t.key(Key::Start);
                }
            }
        }
        Some("staff-roll") => {
            if let Some(t) = app.roster.theater.clone() {
                app.screen = Some(Screen::StaffRoll(Box::new(theater::StaffRoll::new(t))));
            }
        }
        Some("ending") => {
            let p = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(10usize);
            let t = app.roster.theater.clone().context("no theater.json")?;
            let params = t.params(p).context("no such movie")?;
            app.screen = Some(Screen::Movie(Box::new(movie::Movie::with(&app.assets.root, &params)?), menu::AfterMovie::Credits));
        }
        // Force's stage card (--variant N: the stage, then the final one, then the extra stages)
        Some("force-card") => {
            let index = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(0usize);
            app.outcome(Outcome::StartForce((previewed, 0), 1));
            if let Some(run) = app.force.as_mut() {
                let last = run.data.final_index();
                (run.index, run.extra) = if index > last { (last.saturating_sub(1), Some(index - last - 1)) } else { (index, None) };
            }
            app.start_force_stage();
        }
        // a movie (--variant N, default the attract cycle's first), N frames in
        Some("movie") => {
            let first = app.attract_steps.iter().find_map(|s| s["movie"].as_str().and_then(|f| Path::new(f).file_stem()?.to_str()?.parse().ok()));
            let index = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).or(first).unwrap_or(0);
            app.screen = Some(Screen::Movie(Box::new(movie::Movie::load(&app.assets.root, index)?), menu::AfterMovie::Title));
        }
        // a mode's result screen after a scripted run: Time Attack cleared, a Survival streak, a Team Battle
        Some(m @ ("results-ta" | "results-survival" | "results-team")) => {
            let roster = &app.roster;
            let data = roster.modes.clone();
            let mut seed = 7;
            let player = (previewed, 0);
            let mut records = save::Records::load(&data.ta_defaults, &data.survival_defaults);
            let mut run = match m {
                "results-ta" => modes::Run::time_attack(roster, player, &mut seed),
                "results-survival" => modes::Run::survival(player),
                _ => {
                    let team = |ids: &[usize], human| modes::Team { members: ids.iter().filter_map(|&i| roster.index_of(i)).map(|i| (i, 0)).collect(), current: 0, picked: ids.len(), human };
                    let ids = |k: usize| pv["teams"][k].as_array().into_iter().flatten().map(id).collect::<Vec<_>>();
                    modes::Run::team([team(&ids(0), true), team(&ids(1), false)], 1)
                }
            };
            let results = (0..40).find_map(|k| {
                let setup = run.setup(roster, &data, &mut seed, k);
                run.frames = 1500 + 37 * k;
                let winner = match m {
                    "results-survival" if k == 12 => Some(1),
                    "results-team" => Some([0, 1, 0, 0, 1, 0][k as usize % 6]),
                    _ => Some(0),
                };
                match run.after(&setup, winner, [40 * kumite_sim::ONE; 2], [140; 2], &data, &mut records, roster) {
                    modes::Next::Results(r) => Some(r),
                    _ => None,
                }
            });
            app.screen = results.map(Screen::Results);
        }
        Some(m @ ("select" | "vs" | "practice")) => {
            let mode = match m {
                "vs" => Mode::Versus,
                "practice" => Mode::Practice,
                _ => Mode::Arcade,
            };
            app.screen = Some(Screen::Select(Box::new(menu::Select::new(mode, 1, &app.roster, app.seed))));
        }
        Some(m @ ("versus" | "versus-vs" | "versus-practice")) => {
            let mode = match m {
                "versus-vs" => Mode::Versus,
                "versus-practice" => Mode::Practice,
                _ => Mode::Arcade,
            };
            let at = |k: usize| app.roster.index_of(id(&pv["versus"][k])).unwrap_or(0);
            let (a, b) = (at(0), at(1));
            app.screen = Some(Screen::Versus {
                setup: FightSetup { mode, players: [(a, 0), (b, 1)], stage: 0, difficulty: 1, level: 0, handicap: [100; 2], ball: None, partners: None },
                ladder: Vec::new(),
                frame: 0,
                number: 1,
                extra: Box::new(menu::VsExtra { humans: 1, teams: None, force: None }),
            });
        }
        Some("winpose") => {
            // CPU against CPU (--models, --stage) until the first win pose, then --frames more steps
            let idx = |m: usize| app.roster.characters.iter().position(|c| c.costumes.contains(&m)).unwrap_or(0);
            let cos = |m: usize| app.roster.characters[idx(m)].costumes.iter().position(|&x| x == m).unwrap_or(0);
            let players = [(idx(app.models[0]), cos(app.models[0])), (idx(app.models[1]), cos(app.models[1]))];
            app.load(&r, Some(FightSetup { mode: Mode::Watch, players, stage: app.stage, difficulty: 2, level: 9, handicap: [100; 2], ball: None, partners: None }), Vec::new())?;
            app.renderer = Some(r);
            for _ in 0..60 * 60 * 6 {
                app.step();
                if app.fight.as_ref().is_some_and(|f| matches!(f.bout.phase, Phase::Result { winner: Some(_) })) {
                    break;
                }
            }
            let r = app.renderer.take().context("renderer")?;
            app.renderer = Some(r);
            for _ in 0..frames {
                app.step();
            }
            for _ in 0..13 {
                app.draw();
            }
            let r = app.renderer.take().context("renderer")?;
            write_png(Path::new(path), &r.capture()?)?;
            if let Some(f) = &app.fight {
                let w = match f.bout.phase {
                    Phase::Result { winner: Some(w) } => w,
                    _ => 0,
                };
                let fi = &f.bout.game.fight.fighters[w];
                log::info!("winpose: winner P{} {:?} pos {:?} camera {:?}", w + 1, fi.animation(&f.bout.game.fight.data), fi.position, f.bout.game.camera.view);
            }
            return Ok(());
        }
        Some("embu") => {
            // the attract embu (`--variant N`), N frames in
            app.attract.variant = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
            app.renderer = Some(r);
            app.start_embu()?;
            for _ in 0..frames {
                app.step();
            }
            for _ in 0..13 {
                app.draw();
            }
            let r = app.renderer.take().context("renderer")?;
            write_png(Path::new(path), &r.capture()?)?;
            if let Some(e) = &app.embu {
                log::info!("embu frame {}: models {:?}", e.scene.frame(), e.scene.models);
            }
            return Ok(());
        }
        // TAG BRAWL: its modes; a mode's setup (--variant 0 free for all, 1 two on two, 2 tag arcade); tag: a match
        // of four CPUs (--variant 1: slot 1 is 1P; 2: free for all; 3: tag arcade, --level N its pair)
        // ONLINE's screen (--variant N: the way, --code: the room code or address)
        Some("online") => {
            let variant = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(0usize);
            let code = std::env::args().skip_while(|a| a != "--code").nth(1);
            let mut o = onlinemenu::OnlineMenu::new(3, 1);
            o.preset(variant, code);
            app.screen = Some(Screen::Online(Box::new(o)));
        }
        Some("tag-modes") => app.screen = Some(Screen::TagModes { frame: 20, cursor: 0, difficulty: 1 }),
        Some("ball-modes") => app.screen = Some(Screen::BallModes { frame: 20, cursor: 0, difficulty: 1 }),
        Some("tag-setup") => {
            let variant = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(1usize);
            let mode = tagrun::TagMode::ALL[variant.min(2)];
            app.screen = Some(Screen::Tag(Box::new(tagrun::TagSelect::new(&app.roster, 1, 1, mode))));
        }
        Some("tag") => {
            let variant = std::env::args().skip_while(|a| a != "--variant").nth(1).and_then(|v| v.parse().ok()).unwrap_or(0u8);
            let mode = match variant {
                2 => tagrun::TagMode::Free,
                3 => tagrun::TagMode::Arcade,
                _ => tagrun::TagMode::Teams,
            };
            let slots = tagrun::TagSelect::new(&app.roster, 2, 1, mode).slots;
            let mut setup = tagrun::TagSetup { slots, difficulty: 2, mode, arcade: None };
            if mode == tagrun::TagMode::Arcade {
                let mut seed = 1;
                let mut a = tagrun::TagArcade::new(&app.roster, [(slots[0].character, slots[0].costume), (slots[1].character, slots[1].costume)], &mut seed);
                a.level = std::env::args().skip_while(|a| a != "--level").nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                setup.arcade = Some(a);
            }
            app.tag_stage = std::env::args().skip_while(|a| a != "--stage").nth(1).and_then(|v| v.parse().ok());
            for (k, s) in setup.slots.iter_mut().enumerate() {
                s.player = (variant == 1 && k == 0).then_some(0);
            }
            app.renderer = Some(r);
            app.start_tag(setup);
            r = app.renderer.take().context("renderer")?;
        }
        Some("grid") => {
            let record = grid::VsRecord { wins: [3, 5], draws: 1 };
            app.screen = Some(Screen::Grid(Box::new(grid::GridSelect::new(app.roster.grid.clone(), 1, record))));
        }
        Some("practice-fight") => {
            let at = |k: usize| app.roster.index_of(id(&pv["practice"][k])).unwrap_or(0);
            let (a, b) = (at(0), at(1));
            let stage = app.roster.home_stage(app.roster.characters[b].id, 0);
            app.load(&r, Some(FightSetup { mode: Mode::Practice, players: [(a, 0), (b, 0)], stage, difficulty: 1, level: 5, handicap: [100; 2], ball: None, partners: None }), Vec::new())?;
        }
        Some("hud") => {
            let roster = &app.roster;
            let mut s = Screen::Select(Box::new(menu::Select::new(Mode::Arcade, 1, roster, app.seed)));
            let _ = s.key(0, Key::Button(0), roster, &mut app.seed);
            let fight = (0..200).find_map(|_| match s.tick(roster, &mut app.seed) {
                Outcome::Fight(setup, ladder) => Some((setup, ladder)),
                _ => None,
            });
            let Some((setup, ladder)) = fight else { anyhow::bail!("arcade did not start") };
            app.load(&r, Some(setup), ladder)?;
        }
        // --arcade: its VS screen, then the fight; --mode: started at the first step
        None if matches!(app.screen, Some(Screen::Versus { .. })) || app.pending_launch.is_some() || app.room.is_some() => {}
        _ => app.load(&r, None, Vec::new())?,
    }
    app.renderer = Some(r);
    // fights and menu screens alike run N steps (menus animate: blinking, the scrolling wall, the timer);
    // --keys apply at their step here
    for i in 0..frames {
        while app.key_script.first().is_some_and(|(f, _, _)| *f <= i) {
            if let (_, p, Some(k)) = app.key_script.remove(0) {
                app.input_key(p, k);
            }
        }
        // (a replay's seeks: at once, as each drawn frame does)
        app.replay_requests();
        app.step();
    }
    // egui lays windows out on the first frame and fades them in over a few more
    for _ in 0..13 {
        app.draw();
    }
    let r = app.renderer.take().context("renderer")?;
    write_png(Path::new(path), &r.capture()?)?;
    if let Some(fight) = &app.fight {
        let g = &fight.bout.game;
        let f = &g.fight;
        let hp = |i: usize| f.fighters[i].health / kumite_sim::ONE;
        let (a0, f0) = f.fighters[0].animation(&f.data);
        let (a1, f1) = f.fighters[1].animation(&f.data);
        log::info!("after {frames} frames: P1 {a0}@{f0} hp {} | P2 {a1}@{f1} hp {} | {:?} | {:?} wins {:?}", hp(0), hp(1), f.state, fight.bout.phase, fight.bout.wins);
        log::info!("camera {:?} (intro {}); samples {:?}", g.camera.view, g.camera.in_intro(), g.camera_samples());
    }
    // a replay: where it is, and what shows (the seek's integration test compares these)
    if app.replay_view.is_some() {
        let hud = app.tag.as_ref().map_or_else(|| format!("{:?}", app.hud.state), |t| format!("{:?}", t.hud));
        log::info!("replay at step {}, state {:016x}, HUD {hud}", app.replay_played(), app.match_hash());
    }
    if let Some(t) = &app.tag {
        let g = &t.game;
        let hp: Vec<i32> = g.melee.actors.iter().map(|f| f.health / kumite_sim::ONE).collect();
        log::info!("tag after {frames} frames: hp {hp:?} targets {:?} {:?} wins {:?} round {}", g.melee.target, g.phase, g.wins, g.round);
    }
    log::info!("wrote {path}");
    if let (Some(d), Some(out)) = (&app.demo_rec, &app.demo_out) {
        d.save(out)?;
        log::info!("demo saved: {} ({} steps)", out.display(), d.len());
    }
    Ok(())
}

/// Placeholder floor when no converted stage is available: 1.8 m checker tiles.
fn checker_floor(r: &Renderer) -> StaticMesh {
    let tex = r.texture(2, 1, &[90, 90, 100, 255, 140, 140, 150, 255]);
    let (mut v, mut i) = (Vec::new(), Vec::new());
    let (n, t) = (16i32, 1.8f32);
    for z in -n..n {
        for x in -n..n {
            let b = v.len() as u32;
            let u = if (x + z) & 1 == 0 { 0.25 } else { 0.75 };
            for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                v.push(StaticVertex { pos: [(x + dx) as f32 * t, 0.0, (z + dz) as f32 * t], uv: [u, 0.5], color: [1.0; 4] });
            }
            i.extend([b, b + 2, b + 1, b + 1, b + 2, b + 3]);
        }
    }
    r.static_mesh(&v, &i, tex)
}

/// How long a finished match's session lingers ([`App::linger_net`]): past the others' stall at the prediction
/// window, so its last inputs reach them.
const LINGER: Duration = Duration::from_secs(3);

/// A finished match's session, still polled: since when, its number, when its last inputs were last sent again.
struct Linger {
    net: netplay::NetPlay,
    session: Option<u32>,
    since: Instant,
    sent: Option<Instant>,
}

/// Online, each tick of a loaded match: this computer says once that it has loaded it, and the session learns
/// whether every computer has (its clocks wait until then: a browser may still be downloading).
fn netplay_ready(net: &mut netplay::NetPlay, room: &mut room::Room) {
    if !net.ready_said {
        room.ready();
        net.ready_said = true;
    }
    net.peers_loading = !room.all_ready();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wgpu=warn,naga=warn,webrtc=error,webrtc_ice=error,webrtc_mdns=error,webrtc_sctp=error,webrtc_dtls=error,matchbox_socket=warn"),
    )
    .init();
    let args: Vec<String> = std::env::args().collect();
    // (--help needs no assets)
    let help = args.iter().skip(1).any(|a| a == "--help" || a == "-h");
    let root = if help { Default::default() } else { assetdir::find(&args, &|k| std::env::var_os(k), std::env::current_exe().ok().as_deref(), std::env::consts::OS)? };
    run(root, args)
}

/// `--help`: the command line.
const HELP: &str = "\
OpenKumite, a 3D fighting game engine that plays the data converted from your own game discs. Usage: openkumite [OPTIONS]
The converted assets (kumite_converter's output) come from the first of: --assets DIR; OPENKUMITE_ASSETS; assets/ next to
the executable; macOS: the .app's Contents/Resources/assets, ~/Library/Application Support/OpenKumite/assets;
Windows: %APPDATA%\\OpenKumite\\assets; Linux: $XDG_DATA_HOME/openkumite/assets (~/.local/share), then
$XDG_DATA_DIRS' (/usr/local/share, /usr/share); the repository's assets/. Settings and records are kept in
openkumite_save.json in the system's folder for application data (macOS: ~/Library/Application Support/OpenKumite;
Windows: %APPDATA%\\OpenKumite; Linux: ~/.local/share/openkumite; OPENKUMITE_SAVE_DIR overrides it);
graphics options given here apply over the saved ones for this run.

  --assets DIR                 the converted assets' folder

Graphics (also OPTION > GRAPHICS):
  --fullscreen                 full screen at --resolution (the monitor's video mode)
  --borderless                 a window covering the screen
  --windowed                   a window
  --resolution WxH             the window's size or the full-screen mode (only what the screen can show)
  --render-scale S             the 3D scene's resolution over the window's: 0.25..2 (or 150%)
  --vsync on|off
  --textures sharp|smooth      the original's nearest texels, or filtered
  --graphics recommended|default
                               start from the settings chosen for the screen and the GPU, or the defaults
  --stats                      video statistics (FPS, frame times, resolutions, GPU); F10 in the game

Game:
  --arcade N                   arcade from stage N (1..)
  --player NAME|ID             ... with this character
  --costume C                  ... in this costume
  --difficulty 0..2            ... at this difficulty
  --mute                       no sound (M toggles it in the game)

Straight into a mode (debugging; past the menus):
  --mode NAME                  arcade, vs, practice, time-attack, survival, ball, force, tag-free, tag-teams,
                               tag-arcade, levels
  --player NAME|ID[:C]         P1 (a name as the roster has it, any case; PREFIX:NAME for one of the same name
  --opponent NAME|ID[:C]       with the roster's prefix; :C the costume 1..); P2: the opponent, Practice's dummy,
                               Force's 2P
  --costume C, --costume2 C    P1's, P2's costume (1..)
  --fighters A,B,C,D           the fighters in order (the tag brawl's four slots; --mode ball: 2 ON 2, C and D
                               beside A and B)
  --stage ID                   the stage (stages.json id; default: the mode's own)
  --difficulty easy|medium|hard
  --level N                    arcade / Time Attack stage, Tag Arcade pair (1..10); Survival: the wins so far
  --p2-cpu, --p2-human         VS and Tag Arcade: P2 the CPU (default human); Ball, the tag brawl: P2 human
  --force-stage N|final        Force's stage (1..; past the game's own, the extra stages)
  --boss                       ... straight to the boss
  --coop                       ... with 2P (--opponent, or the player's next costume)
  --force-keys N               ... the keys held (all of them at the last stage earn the final stage)
  --ai SEATS                   the AI plays these seats (1,2; 1..4 in the tag brawl and Ball 2 ON 2; Force: 1,
                               2 with --coop); offline only; the save is not written
  --ai-style S | 1:S,2:S       classic, rushdown, defensive, counter, thrower, evasive, juggler, mixed, or random
  --ai-difficulty D            easy|medium|hard (default hard)
  --ai-level N                 0..9 (default 9)

Sandbox (straight into a free fight):
  --fight                      a free fight
  --models A,B                 the two fighters' model records
  --stage N                    the stage
  --p1 SCRIPT, --p2 SCRIPT     scripted pads (e.g. \"f*30,n*5,1,n*40\"; @file reads one)
  --play STEP:PLAYER:ANIM,...  start moves at fight steps
  --intro V | --no-intro       the round's camera fly-in variant, or none

Debug and testing:
  --debug                      the debug tools (F1 panel, F2 pause, F3 step, F4 hit volumes, F5 reset round,
                               F6/F7 recording and P2 modes, F8 free camera, F9 god mode, F11 save demo)
  --paused                     start paused
  --speed X                    game speed 0.05..4
  --free-cam YAW,PITCH,DIST[,X,Y,Z]
                               start with the free camera
  --draw hurt,push,attack,skeleton,samples|all
                               debug overlays
  --record-demo FILE           record the session's inputs; --demo FILE replays one
  --replay ID|latest           watch a replay (REPLAYS lists them; ID: its file's name)
  --keys \"FRAME:KEY,...\"       scripted menu keys (up down left right start back b1..b4, shot)
  --quit-after N               quit after N drawn frames
  --seed N                     the game's random seed
  --round-time SECONDS, --rounds N
                               every fight's round clock and the rounds to win (testing)
  --hash-log N                 log the fight's state hash every N fight steps (native vs browser determinism)
  --net-synctest N             rollback check: every frame goes back N frames and runs them again (--mode vs)
  --net-local PORT --net-peer HOST:PORT --net-player 0|1
                               online VS over UDP (two windows: ports 7000/7001, players 0/1; same --mode args)
  --online ROOM                online: into the room of that code (WebRTC; --signal ws://HOST:PORT, default a
                               local matchbox_server; --host-room: this one creates it), then the room's screen
  --host [PORT]                online by IP, no server: host a room on a UDP port (default 7000; over the internet
                               it must be forwarded to this computer)
  --join HOST[:PORT]           ... join a host's room
  --name NICK, --locals 1|2    online: the nickname, and the players at this computer (one or two)
  --net-delay MS, --net-jitter MS, --net-loss PCT
                               ... over a bad connection (simulated on the packets received)
  --ko-boss                    the boss falls at once (testing)

Headless (no window; the save is not written):
  --shot FILE.png              render to a PNG and quit
  --frames N                   ... after N frames
  --screen NAME                ... of a screen: title, main-menu, options, keyconfig, keyboard, select, vs,
                               practice, versus, hud, tag, grid, force (--tally), force-results, presents, ending,
                               staff-roll, movie, embu, ...
  --ui                         ... with the debug panel
  --variant N, --coop, --chicken, --boss
                               ... the screen's variant (some screens)

  -h, --help                   this help

In a browser the options go in the page's query: http://127.0.0.1:8770/?--stats&--arcade&10
";

/// In a browser: the assets are served next to the page (`assets/`); the page's query string gives the options
/// (`?--debug&--arcade&10`).
#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    let query = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
    let mut args = vec!["openkumite".to_string()];
    args.extend(query.trim_start_matches('?').split('&').filter(|a| !a.is_empty()).map(|a| a.replace("%20", " ")));
    if let Err(e) = run("assets".into(), args) {
        log::error!("{e:#}");
        page_error(&format!("The game did not start: {e:#}"));
    }
}

/// In a browser: a failure the game cannot go on from, shown on the page (`#error`, web/index.html), not only in the
/// console (else the page stays black).
#[cfg(target_arch = "wasm32")]
fn page_error(message: &str) {
    if let Some(e) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("error")) {
        e.set_text_content(Some(&format!("{}{message}\n", e.text_content().unwrap_or_default())));
    }
}

/// What waits for its files to arrive (a browser).
enum Waiting {
    Fight(Box<FightSetup>, Vec<usize>),
    Tag(Box<tagrun::TagSetup>),
}

/// Writes the files read when dropped (`--record-reads`).
struct RecordReads(std::path::PathBuf, std::path::PathBuf);

impl Drop for RecordReads {
    fn drop(&mut self) {
        let list: Vec<(String, u64)> = kumite_fs::recorded()
            .iter()
            .filter_map(|p| {
                let rel = p.strip_prefix(&self.1).ok()?;
                let size = std::fs::metadata(p).ok()?.len();
                Some((format!("assets/{}", rel.to_string_lossy().replace('\\', "/")), size))
            })
            .collect();
        if let Err(e) = std::fs::write(&self.0, serde_json::to_vec(&list).unwrap_or_default()) {
            log::error!("{}: {e}", self.0.display());
        }
    }
}

fn run(root: std::path::PathBuf, args: Vec<String>) -> Result<()> {
    if args.iter().skip(1).any(|a| a == "--help" || a == "-h") {
        #[cfg(not(target_arch = "wasm32"))]
        print!("{HELP}");
        #[cfg(target_arch = "wasm32")]
        log::info!("{HELP}");
        return Ok(());
    }
    log::info!("assets: {}", root.display());
    // `--record-reads FILE`: the files read until the end, as [path under the assets, size] (web/make_preload.sh)
    let _record = args.iter().position(|a| a == "--record-reads").and_then(|i| args.get(i + 1)).map(|f| {
        kumite_fs::record();
        RecordReads(std::path::PathBuf::from(f), root.clone())
    });
    let mut args = args;
    // `--demo FILE` replays a recorded session: its launch arguments come first, then the ones given now
    let value_of = |args: &[String], name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let without = |args: &[String], names: &[&str]| -> Vec<String> {
        let mut out = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if names.contains(&a.as_str()) {
                it.next();
            } else {
                out.push(a.clone());
            }
        }
        out
    };
    let mut playback = None;
    if let Some(path) = value_of(&args, "--demo") {
        let file = demo::DemoFile::load(Path::new(&path))?;
        log::info!("playing demo {path}: {} steps, args {:?}", file.len(), file.args);
        let mut a = vec![args[0].clone()];
        a.extend(file.args.iter().cloned());
        a.extend(without(&args[1..], &["--demo"]));
        args = a;
        playback = Some(demo::Playback::new(file));
    }
    // tool options stay out of a demo (keys given by --keys are recorded as keys)
    let demo_args: Vec<String> =
        without(&args[1..], &["--assets", "--demo", "--record-demo", "--shot", "--frames", "--screen", "--keys", "--quit-after"]).into_iter().filter(|a| a != "--ui").collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    // a script argument can be the script itself or @file
    let script = |name: &str| -> Result<Vec<Pad>> {
        match arg(name) {
            Some(s) if s.starts_with('@') => {
                let text = kumite_fs::read_to_string(Path::new(&s[1..])).with_context(|| format!("reading {}", &s[1..]))?;
                kumite_game::parse_script(&text)
            }
            Some(s) => kumite_game::parse_script(&s),
            None => Ok(Vec::new()),
        }
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    let assets = Assets::new(root)?;
    // (online, the hello says the assets' fingerprint: a browser fetches it early)
    kumite_fs::prefetch(&[assets.root.join("manifest.json")]);
    let gd = assets.root.join("gamedata");
    let _ = SHADOWS.set(assets.gamedata("shadows.json").unwrap_or_default());
    let sandbox = !flag("--mode") && !flag("--online") && !flag("--host") && !flag("--join") && ["--fight", "--models", "--stage", "--p1", "--p2", "--intro", "--no-intro"].iter().any(|f| flag(f));
    let roster = Roster::load(&gd);
    // (by default the roster's first two characters' first costumes)
    let first = |k: usize| roster.characters.get(k).and_then(|c| c.costumes.first().copied()).unwrap_or(0);
    let models = match arg("--models") {
        Some(m) => {
            let v: Vec<usize> = m.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            [v.first().copied().unwrap_or_else(|| first(0)), v.get(1).copied().unwrap_or_else(|| first(1))]
        }
        None => [first(0), first(1)],
    };
    let stage = arg("--stage").and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut debug = Debug::new(flag("--debug"), models, stage);
    debug.paused = flag("--paused");
    // `--free-cam yaw,pitch,distance[,pan x,y,z]`: start with the debug orbit camera (radians, metres; the pan moves
    // its centre from the fighters' midpoint 1 m up)
    if let Some(v) = arg("--free-cam") {
        let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        anyhow::ensure!(n.len() == 3 || n.len() == 6, "--free-cam yaw,pitch,distance[,x,y,z]");
        let pan = if n.len() == 6 { glam::Vec3::new(n[3], n[4], n[5]) } else { glam::Vec3::ZERO };
        debug.free_cam = Some(debug::FreeCam { yaw: n[0], pitch: n[1], distance: n[2], pan });
    }
    debug.speed = arg("--speed").and_then(|s| s.parse().ok()).unwrap_or(1.0f32).clamp(0.05, 4.0);
    if let Some(d) = arg("--draw") {
        for part in d.split(',') {
            let all = part == "all";
            debug.draw_hurt |= all || part == "hurt";
            debug.draw_push |= all || part == "push";
            debug.draw_attack |= all || part == "attack";
            debug.draw_skeleton |= all || part == "skeleton";
            debug.draw_samples |= all || part == "samples";
        }
    }
    let attract: serde_json::Value = kumite_sim::data::load(&gd.join("attract.json")).unwrap_or_default();
    let mut hud_data = hud::HudData::load(&roster.screens, &roster.ball_hud);
    // the extra fighters (roster.json selections with a "source"): their HUD names
    if let Ok(b) = kumite_fs::read(&gd.join("roster.json")) {
        let r: serde_json::Value = serde_json::from_slice(&b).unwrap_or_default();
        for e in r["selections"]["entries"].as_array().into_iter().flatten().filter(|e| e["source"].is_string()) {
            if let (Some(m), Some(n)) = (e["model_record"].as_u64(), e["name"].as_str()) {
                hud_data.extra_names.insert(m as usize, n.to_string());
            }
        }
    }
    let hud = Hud::new(hud_data);
    // (and the AI's matches: what it wins is not the player's)
    if arg("--shot").is_some() || flag("--ai") {
        save::set_read_only();
    }
    #[cfg(not(target_arch = "wasm32"))]
    log::info!("save: {}", save::save_dir().join(save::FILE).display());
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(m) = save::lock() {
        debug.say(m);
    }
    let records = save::Records::load(&roster.modes.ta_defaults, &roster.modes.survival_defaults);
    menu::set_difficulty(records.difficulty);
    let graphics = records.graphics.clone().with_args(|n| arg(n), |n| flag(n))?;
    let graphics_preset = arg("--graphics").map(|p| graphics::Preset::parse(&p)).transpose()?;
    let mut app = App {
        catalog: Catalog::load(&gd),
        roster,
        ai: kumite_game::ai_data(&assets)?,
        sound: kumite_game::sound::SoundData::load(&assets).map_err(|e| log::warn!("no sound data: {e:#}")).ok(),
        boss_change: kumite_game::scene::BossChangeData::load(&assets).map_err(|e| log::warn!("no scenes.json: {e:#}")).ok(),
        attract: Attract::default(),
        open_records: false,
        records_music: None,
        quit_in: None,
        continue_player: None,
        theater_music: None,
        roll_music: None,
        embu_data: kumite_game::embu::EmbuData::load(&assets).map(Arc::new).map_err(|e| log::warn!("no attract.json embu: {e:#}")).ok(),
        embu: None,
        attract_demo_list: attract["demo_fight"]["p1_list"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64()).map(|x| x as usize).collect()).unwrap_or_default(),
        attract_demo_level: attract["demo_fight"]["ai_level"].as_u64().unwrap_or(0) as usize,
        attract_steps: attract["cycle"]["steps"].as_array().cloned().unwrap_or_default(),
        vfx: None,
        rumble: kumite_game::rumble::Rumble::new(None),
        rumble_fight: false,
        tag_stage: None,
        pending_launch: None,
        pending_replay: None,
        waiting: None,
        net: None,
        room: None,
        online_mode: None,
        setup_clock: None,
        actor_cache: Default::default(),
        signal: online::DEFAULT_SIGNAL.to_string(),
        online_back: None,
        hash_every: None,
        round_time: None,
        ai_seats: [None; 4],
        stage_lights: stage_lights(&gd),
        rounds: None,
        hash_steps: 0,
        hash_detail: false,
        gamepads: None,
        audio: None,
        assets,
        models,
        stage,
        intro: if flag("--no-intro") { None } else { Some(arg("--intro").and_then(|v| v.parse().ok()).unwrap_or(0)) },
        sandbox,
        window: None,
        graphics,
        display: graphics::Display::unknown(),
        graphics_preset,
        graphics_args: args.clone(),
        #[cfg(target_arch = "wasm32")]
        fullscreen_pending: false,
        #[cfg(target_arch = "wasm32")]
        page_view: [0, 0],
        #[cfg(target_arch = "wasm32")]
        page_fullscreen: false,
        #[cfg(target_arch = "wasm32")]
        page_api: false,
        renderer: None,
        scene: None,
        fight: None,
        screen: (!sandbox).then(Screen::title),
        art: None,
        hud,
        vs_fights: 0,
        vs_record: Default::default(),
        run: None,
        ball_cpu: [false; 2],
        ball_counts: Vec::new(),
        force: None,
        force_ko_boss: flag("--ko-boss"),
        tag: None,
        force_data: kumite_sim::data::load::<kumite_game::force::ForceData>(&gd.join("force.json")).map(Arc::new).map_err(|e| log::warn!("no force.json: {e:#}")).ok(),
        seed: unix_time() as u32,
        keys: HashSet::new(),
        tapped: HashSet::new(),
        net_start: [false; 2],
        net_linger: None,
        script: script("--p1")?,
        p2_script: script("--p2")?,
        debug,
        egui: None,
        drag: None,
        drag_pan: false,
        cursor: (0.0, 0.0),
        last: Instant::now(),
        lag: Duration::ZERO,
        steps: 0,
        records,
        key_script: arg("--keys").map(|k| parse_keys(&k)).transpose()?.unwrap_or_default(),
        quit_after: arg("--quit-after").and_then(|v| v.parse().ok()),
        play_script: arg("--play").map(|s| parse_play(&s)).transpose()?.unwrap_or_default(),
        frames: 0,
        logic_steps: 0,
        demo_rec: None,
        replay_rec: None,
        replay_next: None,
        replay_view: None,
        replay_counters_before: None,
        replay_joins: Vec::new(),
        replay_join_next: 0,
        replay_quiet: false,
        replay_tag_stage: None,
        replays: None,
        keys_after: None,
        #[cfg(not(target_arch = "wasm32"))]
        video_dialog: None,
        #[cfg(not(target_arch = "wasm32"))]
        video_export: None,
        demo_out: None,
        demo_play: None,
        history: Default::default(),
        saved: None,
        #[cfg(target_arch = "wasm32")]
        pending_renderer: Default::default(),
    };
    // the demo: played (its seed), and recorded with --debug (F11 saves it) or --record-demo FILE (saved on exit)
    if let Some(pb) = &playback {
        app.seed = pb.file.seed;
    }
    app.demo_out = arg("--record-demo").map(Into::into);
    if (flag("--debug") && playback.is_none()) || app.demo_out.is_some() {
        app.demo_rec = Some(demo::DemoFile::new(app.seed, demo_args));
    }
    app.demo_play = playback;
    // `--arcade N [--player name|id] [--costume C] [--difficulty D]`: arcade from stage N (1.., within the ladder)
    if let Some(n) = arg("--arcade") {
        let level: usize = n.parse().context("--arcade N: a stage number from 1")?;
        let player = match arg("--player") {
            Some(p) => {
                let idx = app.roster.characters.iter().position(|c| p.parse::<usize>().ok() == Some(c.id) || c.name.eq_ignore_ascii_case(&p));
                let idx = idx.with_context(|| format!("--player {p}: no such character (names: {})", app.roster.characters.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")))?;
                Some((idx, arg("--costume").and_then(|c| c.parse().ok()).unwrap_or(0)))
            }
            None => None,
        };
        let difficulty = arg("--difficulty").and_then(|d| d.parse().ok());
        app.screen = None;
        app.arcade_from(level.max(1) - 1, player, difficulty);
    }
    app.hash_every = arg("--hash-log").and_then(|n| n.parse().ok());
    app.round_time = arg("--round-time").and_then(|n| n.parse().ok());
    app.rounds = arg("--rounds").and_then(|n| n.parse().ok());
    if let Some(s) = arg("--signal") {
        app.signal = s;
    }
    {
        app.net = netplay::NetPlay::from_args(&args)?;
        // `--online ROOM` / `--host [PORT]` / `--join HOST[:PORT]` with `--name NICK`: meet, then the VS select
        // (the same letters the menu's nickname row takes)
        onlinemenu::set_default_name(&arg("--name").unwrap_or_else(|| app.records.online_name.clone()).to_uppercase());
        let name = onlinemenu::default_name();
        // (`--locals 2`: two players at this computer; `--host-room`: this one creates the room)
        let locals: u8 = arg("--locals").and_then(|n| n.parse().ok()).unwrap_or(1).clamp(1, 2);
        if let Some(room) = arg("--online") {
            let link = online::room_link(&app.signal, &room.to_uppercase(), &name, locals, flag("--host-room"));
            app.enter_room(link);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if flag("--host") || arg("--join").is_some() {
            let link = match arg("--join") {
                Some(addr) => direct::join(&addr, &name, locals)?,
                None => direct::host(arg("--host").and_then(|p| p.parse().ok()).unwrap_or(direct::DEFAULT_PORT), &name, locals)?,
            };
            app.enter_room(link);
        }
    }
    app.hash_detail = flag("--hash-detail");
    if let Some(seed) = arg("--seed").and_then(|n| n.parse().ok()) {
        app.seed = seed;
    }
    // `--screen online` in a window (testing ONLINE: --variant N the way, --code the room code or address)
    if arg("--shot").is_none() && arg("--screen").as_deref() == Some("online") {
        let mut o = onlinemenu::OnlineMenu::new(app.seed, 1);
        o.preset(arg("--variant").and_then(|v| v.parse().ok()).unwrap_or(0), arg("--code"));
        app.screen = Some(Screen::Online(Box::new(o)));
    }
    // `--mode NAME ...`: that mode straight away (debugging)
    app.pending_launch = launch::from_args(&args, &app.roster)?;
    app.pending_replay = arg("--replay");
    if app.pending_launch.is_some() {
        app.screen = None;
    }
    // the attract loop from its first step (boot: request_scene(3))
    if !sandbox && arg("--arcade").is_none() && arg("--shot").is_none() && app.pending_launch.is_none() && arg("--online").is_none() && !flag("--host") && !flag("--join") && arg("--screen").is_none()
    {
        app.request_title();
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(id) = arg("--export-video") {
        let out = arg("--out").context("--export-video ID --out FILE.mp4")?;
        let size = arg("--size").map_or(Ok((1920, 1080)), |s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).context("--size WIDTHxHEIGHT"))?;
        let free_cam = app.debug.free_cam;
        return export_video(&mut app, &id, Path::new(&out), size, !flag("--no-hud"), free_cam);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = arg("--shot") {
        let frames = arg("--frames").and_then(|f| f.parse().ok()).unwrap_or(0);
        app.seed = 1;
        let screen = arg("--screen");
        return screenshot(&mut app, &path, frames, flag("--ui"), screen.as_deref());
    }
    if !flag("--mute") {
        app.audio = audio::Audio::new();
    }
    app.gamepads = Some(gamepad::Gamepads::new());
    app.rumble = kumite_game::rumble::Rumble::new(kumite_game::rumble::RumbleData::load(&app.assets));
    #[cfg(not(target_arch = "wasm32"))]
    EventLoop::new()?.run_app(&mut app)?;
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        EventLoop::new()?.spawn_app(app);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CPU-vs-CPU match on the test assets, with its HUD and the sound data (None: no test assets).
    fn cpu_match() -> Option<(Bout, hud::Hud, kumite_game::sound::SoundData)> {
        let assets = Assets::new(test_assets()).map_err(|e| eprintln!("skipping: {e:#}")).ok()?;
        let roster = menu::Roster::load(&assets.root.join("gamedata"));
        let mut bout = Bout::new(Game::load(&assets, [131, 71]).ok()?, kumite_game::ai_data(&assets).ok()?, 2, true);
        bout.set_cpu(0, Some((1, 0)));
        bout.set_cpu(1, Some((1, 0)));
        let hud = hud::Hud::new(hud::HudData::load(&roster.screens, &roster.ball_hud));
        Some((bout, hud, kumite_game::sound::SoundData::load(&assets).ok()?))
    }

    /// A step of the match, its HUD and its sounds, as a replay plays it (and as its run-through and seeks do).
    fn step(bout: &mut Bout, hud: &mut hud::Hud, heard: &mut Heard, sound: &kumite_game::sound::SoundData) -> Vec<kumite_game::sound::Sound> {
        bout.step([Pad::default(); 2]);
        hud.step(bout);
        heard.step(sound, bout, ["m131", "m071"], [0, 1], None, None)
    }

    /// A seek starts from a copy of the match: the HUD and the sounds' log go on from it as when played straight (the
    /// health bars as full as they were, the run's time on; the K.O.'s replay with the sounds of its frames).
    #[test]
    fn a_seek_copy_goes_on_as_the_match_played_straight() {
        let Some((mut bout, mut hud, sound)) = cpu_match() else { return };
        let mut heard = Heard::default();
        let mut copy = None;
        let mut straight = Vec::new();
        let mut replayed_sounds = 0;
        for n in 0..60 * 60 * 5 {
            if n == 1200 {
                copy = Some((bout.clone(), hud.state.clone(), heard.clone()));
            }
            let s = step(&mut bout, &mut hud, &mut heard, &sound);
            replayed_sounds += if bout.replay_frame().is_some() { s.len() } else { 0 };
            if n >= 1200 {
                straight.push((s, format!("{:?}", hud.state)));
            }
            if bout.finished() {
                break;
            }
        }
        assert!(bout.finished(), "the match ends");
        assert!(replayed_sounds > 0, "a K.O.'s replay plays the sounds logged with its frames");
        let (mut bout, state, mut heard) = copy.expect("a copy");
        hud.state = state;
        for (k, (sounds, hud_then)) in straight.iter().enumerate() {
            let s = step(&mut bout, &mut hud, &mut heard, &sound);
            assert_eq!(&s, sounds, "the sounds {k} steps after the copy");
            assert_eq!(&format!("{:?}", hud.state), hud_then, "the HUD {k} steps after the copy");
        }
    }

    /// The HUD a seek starts from is the copy's, not a new round's: the bars full, the K.O. text and the run's time
    /// kept.
    #[test]
    fn a_seek_copy_keeps_the_bars_shown() {
        let Some((mut bout, mut hud, _)) = cpu_match() else { return };
        for _ in 0..600 {
            bout.step([Pad::default(); 2]);
            hud.step(&bout);
        }
        let copy = hud.state.clone();
        let mut fresh = hud::Hud::new(hud.data.clone());
        fresh.state = copy;
        let mut reset = hud::Hud::new(hud.data.clone());
        reset.reset(&hud::HudInfo::default());
        bout.step([Pad::default(); 2]);
        for h in [&mut hud, &mut fresh, &mut reset] {
            h.step(&bout);
        }
        assert_eq!(format!("{:?}", fresh.state), format!("{:?}", hud.state));
        assert_ne!(format!("{:?}", reset.state), format!("{:?}", hud.state), "a reset HUD shows other bars");
    }
}
