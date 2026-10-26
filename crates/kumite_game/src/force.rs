//! Force (battle type 8; force research), the per-stage logic of record 7's frame function 0x800B1294:
//! the stage states (script, waits, the clear, the boss), the event script and the spawn streams, the two soldier
//! slots (spawn, life, death, the time bonus), the player's target, the roast chicken, the overlay camera
//! (0x800B4F78 / 0x800B5020), the player's bounds, the clock, the countdown voice and the score. Game units and
//! the game's axes (x, y down, z); the simulation's z is the game's −z.

use crate::{Actor, Game, UNITS};
use glam::Vec3;
use kumite_sim::camera::View;
use kumite_sim::detmath::DetMath;
use kumite_sim::ONE;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize)]
pub struct Event {
    pub x: i32,
    #[serde(rename = "type")]
    pub kind: i32,
    pub dx: i32,
    pub z: i32,
    pub p0: i32,
    pub p1: i32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StageData {
    pub name: [String; 2],
    pub stage: usize,
    pub name_colour: u32,
    pub script: Vec<Event>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Class {
    pub bar_w: i32,
    pub seconds: i32,
}

#[derive(Clone, Debug, Deserialize)]
struct Stream {
    entries: Vec<[i32; 2]>,
}

#[derive(Clone, Debug, Deserialize)]
struct Routes {
    by_selection: Vec<Vec<[i32; 2]>>,
    /// A selection past the table's.
    #[serde(rename = "default")]
    otherwise: [i32; 2],
}

/// force.json `constants` OpenKumite reads.
#[derive(Clone, Debug, Deserialize)]
pub struct Constants {
    /// The final stage: its boss (character id) and music (BGM id).
    #[serde(rename = "final")]
    pub last: Final,
    ai_level: AiLevel,
    bounds: Bounds,
    chicken: ChickenSounds,
    countdown: Countdown,
    tally: TallySounds,
}

/// A chicken's sound as it is eaten, and the voice 15 frames before its text ends (0x800B3E44 / 0x800B3E90).
#[derive(Clone, Debug, Deserialize)]
struct ChickenSounds {
    sfx: u16,
    voice: u16,
}

/// The countdown voice: at each frames left, its sound (0x800B2590).
#[derive(Clone, Debug, Deserialize)]
struct Countdown {
    frames: Vec<u32>,
    codes: Vec<u16>,
}

/// The tally's tick and its key / final challenge sound (0x800B4538).
#[derive(Clone, Debug, Deserialize)]
struct TallySounds {
    sfx: u16,
    key_sfx: u16,
}
/// The players' movement limits (0x80043240): z between `front_z` and `back_z` (on the slanted stage's arena:
/// `front_slanted` [slope, offset] off the camera's x); x within the frustum's edges, a line in z each
/// (`human_x`: [left slope, left, right slope, right] per mille, the slanted arena's second); the CPUs ±`cpu_x`.
#[derive(Clone, Debug, Deserialize)]
struct Bounds {
    back_z: f32,
    front_z: f32,
    slanted_stage: usize,
    front_slanted: [f32; 2],
    human_x: [[f32; 4]; 2],
    cpu_x: f32,
}
/// The CPU's level per stage index (0x800B5C2C).
#[derive(Clone, Debug, Deserialize)]
struct AiLevel {
    by_stage: Vec<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Final {
    pub character: usize,
    pub bgm: usize,
}

/// Where an extra stage's route lies in its level (metres): the route's x = 0 on the floor, its +x along `axis`
/// (x, z), its +z the axis turned a quarter left; the fighters keep within `half_width` of its line.
#[derive(Clone, Debug, Deserialize)]
pub struct Route {
    pub origin: [f32; 3],
    pub axis: [f32; 2],
    pub half_width: f32,
}

impl Route {
    /// A level point (metres) in the route's frame.
    pub fn local(&self, p: Vec3) -> Vec3 {
        let d = p - Vec3::from(self.origin);
        let [ax, az] = self.axis;
        Vec3::new(d.x * ax + d.z * az, d.y, -d.x * az + d.z * ax)
    }
}

/// OpenKumite's extra stages after the original's fourth (made by kumite_converter): an extra level, the
/// tables of original stage `base`, its own script; `bosses` bosses come in together, drawn from `boss_pool`.
#[derive(Clone, Debug, Deserialize)]
pub struct ExtraStage {
    pub name: [String; 2],
    pub stage: usize,
    pub name_colour: u32,
    pub base: usize,
    pub route: Route,
    pub script: Vec<Event>,
    pub bosses: usize,
    pub boss_pool: Vec<usize>,
}

/// gamedata/force.json.
#[derive(Clone, Debug, Deserialize)]
pub struct ForceData {
    pub stages: Vec<StageData>,
    #[serde(default)]
    pub extra_stages: Vec<ExtraStage>,
    stream: Stream,
    pub soldier_costumes: Vec<[usize; 2]>,
    pub soldier_selection_base: usize,
    pub health: Vec<[i32; 5]>,
    pub classes: Vec<Class>,
    pub boss_target_frames: Vec<i32>,
    pub stage_bonus: Vec<u32>,
    boss_routes: Routes,
    pub target_priority: Vec<u32>,
    #[serde(default)]
    pub hud: Hud,
    /// The HUD's pieces as the original draws them (places, sizes, pictures), for the app.
    #[serde(default)]
    pub draw: serde_json::Value,
    pub constants: Constants,
}

/// The run's HUD (force.json `hud`, 0x800B2C34): the original's three health bars' places and its texts.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Hud {
    pub bars: Vec<[i32; 2]>,
    pub stage_clear: String,
    pub key: String,
    pub final_challenge: String,
    pub now_loading: String,
    /// The tally's lines and the score lines: printf formats.
    pub stage_bonus: String,
    pub clear_bonus: String,
    pub clear_time: String,
    pub bonus_time: String,
    pub score: String,
    pub hi_score: String,
    /// "+N SEC" and "+LIFE!".
    pub sec: String,
    pub life: String,
}

impl ForceData {
    /// The final stage's index: `stages`' last (the others play in order before it, the last of them earning the
    /// keys).
    pub fn final_index(&self) -> usize {
        self.stages.len().saturating_sub(1)
    }

    /// The CPU's level on stage `index` (past the table: the last stage's).
    pub fn ai_level(&self, index: usize) -> usize {
        let l = &self.constants.ai_level.by_stage;
        l.get(index).or(l.last()).copied().unwrap_or(0)
    }

    /// Stage `index`'s event script, or extra stage `extra`'s.
    fn script(&self, extra: Option<usize>, index: usize) -> &[Event] {
        match extra.and_then(|e| self.extra_stages.get(e)) {
            Some(x) => &x.script,
            None => self.stages.get(index).or(self.stages.last()).map(|s| s.script.as_slice()).unwrap_or_default(),
        }
    }

    /// The boss of stage `i` for the player's selection (char * 4 + costume) (0x800B2658): character, costume.
    pub fn boss(&self, selection: usize, player_char: usize, i: usize) -> (usize, usize) {
        let [ch, cos] = self.boss_routes.by_selection.get(selection).and_then(|r| r.get(i % r.len().max(1))).copied().unwrap_or(self.boss_routes.otherwise);
        let cos = if cos >= 0 {
            cos as usize
        } else if player_char as i32 == ch {
            usize::from(selection & 3 == 0)
        } else {
            usize::from(selection & 3 != 0)
        };
        (ch.max(0) as usize, cos)
    }
}

/// The clock (60 s), its cap (99 s).
pub const CLOCK: u32 = 3600;
const CLOCK_CAP: u32 = 5940;

/// Something for the app to do this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum ForceEvent {
    /// A system / announcer sound (SePlay code).
    Sound(u16),
    /// The stage-clear fade of the music (frames).
    MusicFade(u32),
    /// The boss is in: play its music (the boss's character, costume).
    Boss(usize, usize),
    /// Load the boss's body (the app loads it and calls `set_boss`).
    LoadBoss(usize, usize),
    /// An extra stage: load `n` bosses drawn from the pool (character ids; the app draws them, loads them and
    /// calls `set_bosses`).
    LoadBosses { pool: Vec<usize>, n: usize },
    /// The two soldiers swapped places (their CPUs go with them: `Bout::swap_cpus`).
    Swapped,
}

/// One enemy slot (0x800B6598).
#[derive(Clone, Debug, Default)]
pub struct Slot {
    pub state: u8,
    timer: u64,
    pub class: usize,
    pub kind: usize,
    wanted: usize,
    spawn: (i32, i32),
    /// The "+N SEC" popup: seconds, world position (simulation), frames left.
    pub popup: Option<(i32, Vec3, u32)>,
}

/// A roast chicken (0x800B690C): state, position (game x, z), frames left of the +LIFE! text.
#[derive(Clone, Copy, Debug, Default)]
pub struct Chicken {
    pub state: u8,
    pub x: i32,
    pub z: i32,
    end: u64,
    /// The +LIFE! text's place (the player's point when it was picked up).
    pub at: Vec3,
}

#[derive(Clone)]
pub struct ForceStage {
    pub data: Arc<ForceData>,
    /// The stage's own random numbers (an extra stage's bosses), seeded alike on every computer online.
    pub rng: u32,
    /// Stage index: the stages in order, then the final one (`ForceData::final_index`; an extra stage: its base's).
    pub index: usize,
    /// The extra stage played (force.json `extra_stages`).
    pub extra: Option<usize>,
    /// Bosses that came in together (an extra stage's): in play until all are down.
    pub bosses_in: usize,
    /// The two soldier kinds' bodies (kind 0, 1).
    soldiers: [Actor; 2],
    /// Slot k is shown in actor position `at[k]` (1 = the player's target, 2 = the other); slots 2.. (more than
    /// two players: two soldiers a player, OpenKumite's own) in actors 6...
    at: [usize; 2],
    pub slots: Vec<Slot>,
    /// Stage state S, frame counters F and G, state timer T.
    pub state: u8,
    f: u64,
    t: u64,
    ev: usize,
    pub camera_mode: u8,
    kills: i32,
    wait_n: i32,
    stream: Option<(usize, i32)>,
    pub boss_phase: bool,
    clock0: u32,
    pub elapsed: u32,
    pub bonus_time: u32,
    pub clear_bonus: u32,
    pub tally_done: bool,
    pub key_earned: bool,
    pub final_challenge: bool,
    pub keys: u32,
    pub chickens: [Chicken; 2],
    /// Camera: x, pitch (<< 6), its target, mode margins and arena.
    pub cam_x: i32,
    pitch: i32,
    pitch_target: i32,
    margins: [i32; 2],
    arena: (i32, i32),
    /// Target switching cooldown.
    target_cd: i32,
    pub score: u32,
    pub hiscore: u32,
    pub kills_total: u32,
    pub events: Vec<ForceEvent>,
    /// The boss met at this stage (character, costume).
    pub boss: Option<(usize, usize)>,
    /// The player's selection and character (boss routes).
    selection: usize,
    player_char: usize,
}

impl ForceStage {
    /// Stage `index` (0x800B1108): slots free, the camera at the start, the script at its first event; the final
    /// stage starts with its character in.
    #[allow(clippy::too_many_arguments)]
    pub fn new(data: Arc<ForceData>, index: usize, soldiers: [Actor; 2], selection: usize, player_char: usize, keys: u32, score: u32, hiscore: u32, kills: u32) -> Self {
        let state = if index == data.final_index() { 8 } else { 0 };
        Self {
            data,
            rng: 0x2545_F491 ^ index as u32,
            state,
            index,
            extra: None,
            bosses_in: 0,
            soldiers,
            at: [1, 2],
            slots: vec![Slot::default(); 2],
            f: 0,
            t: 0,
            ev: 0,
            camera_mode: 0,
            kills: 0,
            wait_n: 0,
            stream: None,
            boss_phase: false,
            clock0: 0,
            elapsed: 0,
            bonus_time: 0,
            clear_bonus: 0,
            tally_done: false,
            key_earned: false,
            final_challenge: false,
            keys,
            chickens: Default::default(),
            cam_x: 0,
            pitch: 0x2AAA,
            pitch_target: 0x2AAA,
            margins: [0, 1],
            arena: (0, 0),
            target_cd: 0,
            score,
            hiscore,
            kills_total: kills,
            events: Vec::new(),
            boss: None,
            selection,
            player_char,
        }
    }

    /// Set the fight up for the stage (0x8002A508 / 0x8002AC0C): the player at the origin facing +x, both
    /// soldier positions out of play; the round rules (one round, only the player's K.O. ends it).
    pub fn setup(&mut self, game: &mut Game) {
        let f = &mut game.fight.fighters[0];
        f.position = Vec3::ZERO;
        f.yaw = std::f32::consts::FRAC_PI_2;
        f.root_yaw = f.yaw;
        game.set_actor(1, &self.soldiers[0]);
        game.set_actor(2, &self.soldiers[1]);
        for f in [&mut game.fight.fighters[1]].into_iter().chain(game.fight.extra.as_deref_mut()) {
            f.inactive = true;
            f.health = 0;
            f.position = Vec3::new(5000.0, 0.0, 0.0);
            f.yaw = -std::f32::consts::FRAC_PI_2;
        }
        game.fight.ko_rule = kumite_sim::KoRule::PlayerOnly;
        self.slots[0].kind = 0;
        self.slots[1].kind = 1;
        self.set_camera_mode(0);
        game.camera_mode = crate::CameraMode::Frozen;
        self.apply_camera(game);
    }

    /// An extra stage `e`: its base's tables, its own script and route.
    pub fn extra_stage(mut self, e: usize) -> Self {
        if let Some(x) = self.data.extra_stages.get(e) {
            self.index = x.base.min(self.data.final_index().saturating_sub(1));
            self.extra = Some(e);
        }
        self
    }

    /// The stage's event script.
    fn script(&self) -> &[Event] {
        self.data.script(self.extra, self.index)
    }

    /// The script's end (its last event's x: the original's 32768; an extra stage's scaled to its route).
    pub fn end_x(&self) -> i32 {
        self.script().iter().map(|e| e.x).max().unwrap_or(32768).max(1)
    }

    /// The extra stage's route.
    pub fn route(&self) -> Option<&Route> {
        self.extra.and_then(|e| self.data.extra_stages.get(e)).map(|x| &x.route)
    }

    fn fighter<'a>(&self, game: &'a mut Game, k: usize) -> Option<&'a mut kumite_sim::Fighter> {
        game.fight.actor_mut(self.slot_actor(k))
    }

    fn fighter_ref<'a>(&self, game: &'a Game, k: usize) -> Option<&'a kumite_sim::Fighter> {
        game.fight.actor(self.slot_actor(k))
    }

    /// How many soldier slots are in play: two, or (more than one player) two a player.
    fn live_slots(&self, game: &Game) -> usize {
        (2 * game.players().len()).clamp(2, self.slots.len())
    }

    /// The players are more than two (or two with the widened melee): the soldiers' places for all of them.
    pub fn widen(&mut self, game: &mut Game) {
        game.widen_coop(12);
        // (no kind yet: the first spawn loads its body, else the place's stand-in, without one, is never hit)
        while self.slots.len() < 8 {
            self.slots.push(Slot { kind: usize::MAX, ..Default::default() });
        }
    }

    /// The camera's progress (RVIEW eye x − its start).
    fn progress(&self) -> i32 {
        self.cam_x
    }

    fn set_camera_mode(&mut self, m: u8) {
        if m == 0 {
            self.margins[1] = 1;
        }
        if m == 2 {
            self.margins = [-100, 60];
            self.arena = (self.cam_x - 5000, self.cam_x + 5000);
        }
        self.camera_mode = m;
    }

    /// Spawn a soldier of `kind` / `class` at (x, z) into the first free slot (event type 1).
    fn spawn(&mut self, kind: usize, class: usize, x: i32, z: i32, live: usize) -> bool {
        let Some(k) = (0..live).find(|&k| self.slots[k].state == 1) else { return false };
        let s = &mut self.slots[k];
        s.class = class % 5;
        s.spawn = (x, z);
        if s.kind == kind {
            s.state = 4;
        } else {
            s.wanted = kind;
            s.state = 2;
        }
        true
    }

    /// One 60 Hz frame, after the fight's step. `fighting`: the round is being fought (round state 1).
    pub fn step(&mut self, game: &mut Game, fighting: bool, round_won: bool) {
        self.events.clear();
        self.f += 1;
        // the score: 10 per point of damage the player deals to a CPU this frame, 20 for throw damage (the co-op
        // partner's count too)
        for h in &game.fight.hits {
            if kumite_sim::Fight::is_player(h.attacker) && !kumite_sim::Fight::is_player(h.victim) && !h.blocked {
                self.score += (if h.mask == 0 { 20 } else { 10 }) * h.damage.max(0) as u32;
            }
        }
        // with no soldier about, the player faces the way on (the stage runs to +x, the soldiers come in from
        // ahead): turning round and walking on need no backward steps
        let p = game.fight.fighters[0].root();
        game.fight.idle_target = Some(Vec3::new(p.x + 100_000.0, 0.0, p.z));
        self.run_state(game, fighting, round_won);
        self.run_stream(game);
        for k in 0..self.slots.len() {
            self.run_slot(game, k);
        }
        self.run_chickens(game);
        self.retarget(game);
        self.bounds(game);
        self.update_camera(game);
        // the chickens a CPU player can reach: not behind the camera's left, nor past its right while it stays
        // shortcut: the flat stages' frustum edges at the middle line (the slanted one's differ a little), per stage
        // if a CPU player is seen to miss a chicken there
        let [_, lo, _, hi] = self.data.constants.bounds.human_x[0];
        let reach = |c: &&Chicken| c.state == 1 && c.x as f32 >= self.cam_x as f32 + lo && (self.camera_mode != 1 || c.x as f32 <= self.cam_x as f32 + hi);
        game.pickups = self.chickens.iter().filter(reach).map(|c| Vec3::new(c.x as f32, 0.0, -c.z as f32)).collect();
        self.score = self.score.min(99_999_990);
        self.hiscore = self.hiscore.max(self.score);
        game.fight.clock = game.fight.clock.min(CLOCK_CAP);
        // the countdown voice: 5, 4, 3, 2, 1
        if fighting {
            let cd = &self.data.constants.countdown;
            if let Some((_, &c)) = cd.frames.iter().zip(&cd.codes).find(|(&t, _)| t == game.fight.clock) {
                self.events.push(ForceEvent::Sound(c));
            }
        }
    }

    fn run_state(&mut self, game: &mut Game, fighting: bool, round_won: bool) {
        let data = self.data.clone();
        let i = self.index;
        match self.state {
            0 => {
                if !fighting {
                    return;
                }
                let script = data.script(self.extra, self.index);
                while let Some(e) = script.get(self.ev) {
                    if self.progress() < e.x {
                        break;
                    }
                    self.ev += 1;
                    if self.run_event(game, e) {
                        break;
                    }
                }
            }
            1 => {
                let done = if self.wait_n < 0 { self.slots.iter().all(|s| s.state == 1) } else { self.kills >= self.wait_n };
                if done {
                    self.state = 0;
                }
            }
            3 => {
                if self.f > self.t {
                    (self.t, self.state) = (self.f + 2, 4);
                }
            }
            4 => {
                if self.f > self.t {
                    self.state = 5;
                    match self.extra.and_then(|e| data.extra_stages.get(e)) {
                        Some(x) => self.events.push(ForceEvent::LoadBosses { pool: x.boss_pool.clone(), n: x.bosses }),
                        None => {
                            let (ch, cos) = data.boss(self.selection, self.player_char, i);
                            self.events.push(ForceEvent::LoadBoss(ch, cos));
                        }
                    }
                }
            }
            6 => {
                // bosses in together: the round is won when the last of them is down
                if self.bosses_in > 1 && fighting && !round_won {
                    let down = |f: Option<&kumite_sim::Fighter>| f.is_none_or(|f| f.health <= 0);
                    if (0..self.slots.len()).filter(|&k| self.slots[k].state == 5 || k < 2).all(|k| down(self.fighter_ref(game, k))) {
                        game.fight.state = kumite_sim::RoundState::Ko { winner: 0 };
                    }
                }
                self.elapsed = self.clock0.saturating_sub(game.fight.clock);
                let target = data.boss_target_frames.get(i).copied().unwrap_or(0) as u32;
                self.bonus_time = target.saturating_sub(self.elapsed);
                if round_won {
                    let best = game.players().into_iter().filter_map(|i| game.fight.actor(i)).map(|p| p.health).max().unwrap_or(0);
                    self.clear_bonus = 100 * (best.max(0) / ONE) as u32;
                    let last = data.final_index().saturating_sub(1);
                    if i == last && self.extra.is_none() {
                        if (self.keys as usize) < last {
                            self.key_earned = true;
                        } else if self.keys as usize == last {
                            self.final_challenge = true;
                        }
                    }
                    (self.t, self.state) = (0, 7);
                }
            }
            7 => self.tally(game),
            8 => {
                // the final stage: its character in slot 0 from the start (the app has loaded it)
                self.set_camera_mode(2);
                self.boss_phase = true;
                game.fight.ko_rule = kumite_sim::KoRule::Both;
                self.clock0 = game.fight.clock;
                self.state = 9;
            }
            9 => {
                self.elapsed = self.clock0.saturating_sub(game.fight.clock);
                if round_won {
                    (self.t, self.state) = (0, 10);
                }
            }
            10 => {
                self.t += 1;
                if self.t > 180 {
                    self.tally_done = true;
                }
            }
            _ => {}
        }
    }

    /// One script event; true stops reading this frame.
    fn run_event(&mut self, game: &mut Game, e: &Event) -> bool {
        match e.kind {
            0 => self.set_camera_mode(e.p0 as u8),
            1 => {
                let live = self.live_slots(game);
                let _ = self.spawn(e.p0 as usize, (e.p1.rem_euclid(5)) as usize, self.cam_x + e.dx, e.z, live);
                // more players: as many again beside it (two a player)
                for extra in 1..live / 2 {
                    let z = e.z + if extra % 2 == 1 { 900 } else { -900 } * extra.div_ceil(2) as i32;
                    let _ = self.spawn(e.p0 as usize, (e.p1.rem_euclid(5)) as usize, self.cam_x + e.dx + 400 * extra as i32, z, live);
                }
            }
            2 => {
                let p = if e.p0 <= 48 { e.p0 as usize } else { 0 };
                self.stream = Some((p, e.p1));
            }
            3 => {
                (self.kills, self.wait_n, self.state) = (0, e.p0, 1);
                return true;
            }
            4 => self.spawn_chicken(self.cam_x + e.dx, e.z),
            6 => {
                // the stage is clear of soldiers: out of play, the music fades, on to the boss
                for k in 0..self.slots.len() {
                    self.slots[k].state = 1;
                    if let Some(f) = self.fighter(game, k) {
                        f.inactive = true;
                        f.health = 0;
                    }
                }
                self.events.push(ForceEvent::MusicFade(30));
                self.stream = None;
                (self.state, self.t) = (3, self.f + 60);
                return true;
            }
            _ => {}
        }
        false
    }

    /// The spawn stream (0x800B1FC4): from the camera's x + 5000, one per free slot, until `until`.
    fn run_stream(&mut self, game: &Game) {
        let live = self.live_slots(game);
        let Some((p, until)) = self.stream else { return };
        if self.progress() >= until {
            self.stream = None;
            return;
        }
        let Some(&[kind, class]) = self.data.stream.entries.get(p) else {
            self.stream = None;
            return;
        };
        if kind == 0x63 {
            return;
        }
        if self.spawn(kind as usize, class.rem_euclid(5) as usize, self.cam_x + 5000, 0, live) {
            self.stream = Some((p + 1, until));
        }
    }

    /// The slot machine (0x800B20C4).
    fn run_slot(&mut self, game: &mut Game, k: usize) {
        let i = self.index.min(self.data.final_index());
        let hp = self.data.health.get(i).map_or(20, |h| h[self.slots[k].class.min(4)]);
        let f_now = self.f;
        match self.slots[k].state {
            0 => {
                if let Some(f) = self.fighter(game, k) {
                    (f.inactive, f.health) = (true, 0);
                }
                self.slots[k].state = 1;
            }
            2 => (self.slots[k].timer, self.slots[k].state) = (f_now + 2, 3),
            3 => {
                if f_now > self.slots[k].timer {
                    let kind = self.slots[k].wanted;
                    self.slots[k].kind = kind;
                    let actor = self.soldiers[kind.min(1)].clone();
                    // (slots 2.. are the widened melee's actors 6..: `at` covers the first two only)
                    game.set_actor(self.slot_actor(k), &actor);
                    self.slots[k].state = 4;
                }
            }
            4 => {
                let (x, z) = self.slots[k].spawn;
                let player = game.fight.fighters[0].root();
                if let Some(f) = self.fighter(game, k) {
                    f.inactive = false;
                    f.max_health = hp;
                    f.health = hp * ONE;
                    f.position = Vec3::new(x as f32, 0.0, -z as f32);
                    let to = player - f.position;
                    f.yaw = to.x.datan2(to.z);
                    f.root_yaw = f.yaw;
                    f.offset = Vec3::ZERO;
                }
                self.slots[k].state = 5;
            }
            5 => {
                let dead = self.fighter_ref(game, k).is_none_or(|f| f.health <= 0);
                if dead {
                    let s = &mut self.slots[k];
                    (s.state, s.timer) = (6, f_now + 60);
                    // the time bonus for the kill: the second goes up by its class's, the sub-second refilled
                    let secs = self.data.classes.get(s.class).map_or(0, |c| c.seconds);
                    if secs > 0 {
                        let c = game.fight.clock;
                        game.fight.clock = c + 59 - c % 60 + 60 * secs as u32;
                        let at = self.fighter_ref(game, k).map_or(Vec3::ZERO, |f| f.root());
                        self.slots[k].popup = Some((secs, at, 60));
                    }
                    self.kills_total += 1;
                }
            }
            6 => {
                if f_now > self.slots[k].timer {
                    (self.slots[k].state, self.slots[k].timer) = (7, f_now + 40);
                }
            }
            // blinking, then gone
            7 if f_now > self.slots[k].timer => {
                if let Some(f) = self.fighter(game, k) {
                    f.inactive = true;
                }
                self.slots[k].state = 1;
                self.kills += 1;
            }
            _ => {}
        }
        if let Some((s, at, n)) = self.slots[k].popup {
            self.slots[k].popup = (n > 1).then_some((s, at, n - 1));
        }
    }

    /// Slot `k` is drawn this frame (state 4..6, blinking in 7).
    pub fn visible(&self, k: usize) -> bool {
        match self.slots[k].state {
            4..=6 => true,
            7 => self.f & 1 == 1,
            _ => false,
        }
    }

    /// Which actor position shows slot `k` (1 or 2; slots 2.. actors 6..).
    pub fn slot_actor(&self, k: usize) -> usize {
        if k < 2 {
            self.at[k]
        } else {
            4 + k
        }
    }

    fn spawn_chicken(&mut self, x: i32, z: i32) {
        if let Some(c) = self.chickens.iter_mut().find(|c| c.state == 0) {
            *c = Chicken { state: 1, x, z, end: 0, at: Vec3::ZERO };
        }
    }

    /// The pickups (0x800B3E90): picked up within 500 of the player (+50 health, not capped here); in co-op by
    /// either player.
    fn run_chickens(&mut self, game: &mut Game) {
        for c in &mut self.chickens {
            match c.state {
                1 => {
                    if c.x < self.cam_x - 5000 {
                        c.state = 0;
                        continue;
                    }
                    for i in game.players() {
                        let Some(f) = game.fight.actor_mut(i) else { continue };
                        let p = f.root();
                        let (px, pz) = (p.x as i32, -p.z as i32);
                        if (px - c.x).abs() <= 500 && (pz - c.z).abs() <= 500 && f.health > 0 && f.air.is_none() {
                            f.health += 50 * ONE;
                            self.events.push(ForceEvent::Sound(self.data.constants.chicken.sfx));
                            (c.state, c.end, c.at) = (2, self.f + 60, p);
                            break;
                        }
                    }
                }
                2 => {
                    if self.f + 15 == c.end {
                        self.events.push(ForceEvent::Sound(self.data.constants.chicken.voice));
                    }
                    if self.f >= c.end {
                        c.state = 0;
                    }
                }
                _ => {}
            }
        }
    }

    /// Frames left of a chicken's +LIFE! text.
    pub fn life_text(&self, c: &Chicken) -> u32 {
        c.end.saturating_sub(self.f) as u32
    }

    /// The player's target (0x800B54A8): the soldier on screen, the nearer one, or by where they stand.
    fn retarget(&mut self, game: &mut Game) {
        self.target_cd = (self.target_cd - 1).max(0);
        let active = |g: &Game, pos: usize| match pos {
            1 => !g.fight.fighters[1].inactive,
            _ => g.fight.extra.as_ref().is_some_and(|f| !f.inactive),
        };
        // the target is in position 1; the other in 2
        let (a_on, b_on) = (active(game, 1), active(game, 2));
        let swap = if !a_on {
            b_on
        } else if !b_on || self.target_cd > 0 {
            false
        } else {
            let p = game.fight.fighters[0].root();
            let d = |f: &kumite_sim::Fighter| glam::Vec2::new(f.root().x - p.x, f.root().z - p.z).length();
            let (da, db) = (d(&game.fight.fighters[1]), game.fight.extra.as_deref().map_or(f32::MAX, d));
            if da >= 2500.0 && db >= 2500.0 {
                false
            } else {
                (da - db).abs() >= 400.0 && db < da
            }
        };
        if swap {
            game.swap_target();
            self.at = [self.at[1], self.at[0]];
            self.target_cd = 120;
            self.events.push(ForceEvent::Swapped);
        }
    }

    /// The player's movement limits (0x80043240, force.json `constants.bounds`): the camera's frustum in x, the back
    /// and front z; the soldiers near the camera. Applied to the body as drawn (the root): the origin moves with it.
    fn bounds(&self, game: &mut Game) {
        let org = self.cam_x as f32;
        let bd = &self.data.constants.bounds;
        let slanted = self.extra.is_none() && self.data.stages.get(self.index).is_some_and(|s| s.stage == bd.slanted_stage);
        // an extra stage: its route's free half width (the body's room taken off)
        let side = self.route().map(|r| r.half_width * UNITS);
        let [a, b, c, d] = bd.human_x[usize::from(slanted)];
        for i in game.players() {
            let Some(f) = game.fight.actor_mut(i) else { continue };
            let root = f.root();
            // game z = −simulation z
            let z0 = -root.z;
            let front = if slanted { bd.front_slanted[0] * (root.x - org) + bd.front_slanted[1] } else { bd.front_z };
            let z = match side {
                Some(w) => z0.clamp(-w, w),
                None => z0.clamp(front, bd.back_z),
            };
            let (lo, hi) = (a * z / 1000.0 + b, c * z / 1000.0 + d);
            let x = root.x.clamp(org + lo, org + hi);
            f.position.x += x - root.x;
            f.position.z -= z - z0;
        }
        let foes: Vec<usize> = (0..game.fight.actor_count()).filter(|&i| !kumite_sim::Fight::is_player(i)).collect();
        for i in foes {
            let Some(f) = game.fight.actor_mut(i) else { continue };
            let r = f.root();
            f.position.x += r.x.clamp(org - bd.cpu_x, org + bd.cpu_x) - r.x;
            if let Some(w) = side {
                f.position.z += r.z.clamp(-w, w) - r.z;
            }
        }
    }

    /// The overlay camera (0x800B5020): it only scrolls forward (mode 0), stays (1) or keeps to an arena of
    /// ±5000 (2); its pitch keeps the player's feet between rows 188 and 212.
    fn update_camera(&mut self, game: &mut Game) {
        // (co-op: the standing players' middle)
        let f = &game.fight;
        let players: Vec<Vec3> = game.players().into_iter().filter_map(|i| f.actor(i)).filter(|p| p.health > 0).map(|p| p.root()).collect();
        let p = if players.is_empty() { f.fighters[0].root() } else { players.iter().copied().sum::<Vec3>() / players.len() as f32 };
        let (px, pz) = (p.x as i64, -p.z as i64);
        let dx = px - self.cam_x as i64;
        let dy = 3500i64;
        let k = (-(self.pitch as i64) >> 6) & 0xFFF;
        let ang = k as f64 * std::f64::consts::TAU / 4096.0;
        let (sp, cp) = ((ang.dsin() * 4096.0) as i64, (ang.dcos() * 4096.0) as i64);
        let dz = pz + 7000;
        let depth = (cp * dz - sp * dy) / 4096;
        let y2 = (sp * dz + cp * dy) / 4096;
        if depth != 0 {
            let sy = (2500 * y2 / depth) / 3;
            let sx = 500 * dx / depth;
            let mut tx = self.cam_x as i64;
            if self.camera_mode == 2 && sx < self.margins[0] as i64 {
                tx = self.cam_x as i64 - 5 * (self.margins[0] as i64 - sx);
            }
            if (self.camera_mode == 0 || self.camera_mode == 2) && sx > self.margins[1] as i64 {
                tx = self.cam_x as i64 + 5 * (sx - self.margins[1] as i64);
            }
            if (sx > 0 && (self.cam_x as i64) < tx) || (sx < 0 && tx < self.cam_x as i64) {
                if (tx - self.cam_x as i64).abs() > 300 {
                    log::debug!("force camera jump {} -> {tx}: sx {sx} depth {depth} dx {dx} pz {pz}", self.cam_x);
                }
                self.cam_x = tx as i32;
            }
            if self.camera_mode == 2 {
                self.cam_x = self.cam_x.clamp(self.arena.0, self.arena.1);
            }
            if sy >= 213 {
                self.pitch_target = self.pitch + 728;
            } else if sy < 188 {
                self.pitch_target = self.pitch - 728;
            }
        }
        if self.pitch != self.pitch_target {
            self.pitch -= (self.pitch - self.pitch_target) / 8;
        }
        self.pitch = self.pitch.clamp(0x2AAA, 0x10000);
        if self.f.is_multiple_of(60) {
            log::debug!(
                "force camera x {} mode {} pitch {:#x}; player root {:?} position {:?} state {} ev {}",
                self.cam_x,
                self.camera_mode,
                self.pitch,
                game.fight.fighters[0].root(),
                game.fight.fighters[0].position,
                self.state,
                self.ev
            );
        }
        self.apply_camera(game);
    }

    fn apply_camera(&self, game: &mut Game) {
        game.camera.view = View { eye: [self.cam_x as i64, -3500, -7000], pitch: (self.pitch >> 6) as i64, yaw: 0, h: 500 };
    }

    /// The boss is loaded (match state 0x15, 0x800B2658): in slot 0 (the target), 4096 ahead of the camera,
    /// its stage's boss health; the boss phase begins.
    pub fn set_boss(&mut self, game: &mut Game, actor: &Actor, boss: (usize, usize)) {
        let i = self.index.min(self.data.final_index());
        if self.at[0] != 1 {
            game.swap_target();
            self.at = [1, 2];
            self.events.push(ForceEvent::Swapped);
        }
        game.set_actor(1, actor);
        let hp = self.data.health.get(i).map_or(130, |h| h[4]);
        let x = self.cam_x as f32 + 4096.0;
        let player = game.fight.fighters[0].root();
        let f = &mut game.fight.fighters[1];
        (f.inactive, f.max_health, f.health) = (false, hp, hp * ONE);
        f.position = Vec3::new(x, 0.0, 0.0);
        let to = player - f.position;
        f.yaw = to.x.datan2(to.z);
        f.root_yaw = f.yaw;
        if let Some(e) = game.fight.extra.as_deref_mut() {
            e.inactive = true;
        }
        self.slots[0] = Slot { state: 5, class: 4, kind: 0, ..Default::default() };
        for k in 1..self.slots.len() {
            self.slots[k].state = 1;
            if k >= 2 {
                if let Some(f) = self.fighter(game, k) {
                    (f.inactive, f.health) = (true, 0);
                }
            }
        }
        game.fight.ko_rule = kumite_sim::KoRule::Both;
        self.boss_phase = true;
        self.boss = Some(boss);
        self.clock0 = game.fight.clock;
        self.state = 6;
        self.events.push(ForceEvent::Boss(boss.0, boss.1));
    }

    /// An extra stage's bosses come in together: the first as `set_boss` places it, the others in the other slot
    /// beside it (on the other side of the line), each at its stage's boss health; all must go down.
    pub fn set_bosses(&mut self, game: &mut Game, actors: &[Actor], bosses: &[(usize, usize)]) {
        let (Some(first), Some(&b0)) = (actors.first(), bosses.first()) else { return };
        self.set_boss(game, first, b0);
        let i = self.index.min(self.data.final_index());
        // the others beside it, spread across the line (actor 2, then the wider melee's 6..)
        let hp = self.data.health.get(i).map_or(130, |h| h[4]);
        let x = self.cam_x as f32 + 4096.0;
        let side = self.route().map_or(1100.0, |r| (r.half_width * UNITS * 0.6).min(1100.0));
        let player = game.fight.fighters[0].root();
        let n = actors.len().min(bosses.len()).min(self.slots.len());
        if n > 1 {
            game.fight.fighters[1].position.z = -side;
        }
        for (j, actor) in actors.iter().enumerate().take(n).skip(1) {
            let slot = j;
            let actor_id = self.slot_actor(slot);
            game.set_actor(actor_id, actor);
            let z = if j % 2 == 1 { side } else { -side - 900.0 * (j / 2) as f32 };
            if let Some(f) = game.fight.actor_mut(actor_id) {
                (f.inactive, f.max_health, f.health) = (false, hp, hp * ONE);
                f.position = Vec3::new(x + 600.0 * j as f32, 0.0, z);
                let to = player - f.position;
                f.yaw = to.x.datan2(to.z);
                f.root_yaw = f.yaw;
            }
            self.slots[slot] = Slot { state: 5, class: 4, kind: slot % 2, ..Default::default() };
        }
        if n > 1 {
            // (the stage logic ends the round: the first boss down alone must not)
            game.fight.ko_rule = kumite_sim::KoRule::PlayerOnly;
            self.bosses_in = n;
        }
    }

    /// The clear tally (S7, 0x800B19D8): the stage bonus at 30, the clear bonus at 60, the time at 90; at 120
    /// the bonus time, the key or the final challenge.
    fn tally(&mut self, game: &mut Game) {
        self.t += 1;
        let i = self.index;
        let TallySounds { sfx, key_sfx } = self.data.constants.tally;
        match self.t {
            30 => {
                self.score += self.data.stage_bonus.get(i).copied().unwrap_or(0);
                self.events.push(ForceEvent::Sound(sfx));
            }
            60 => {
                self.score += self.clear_bonus;
                self.events.push(ForceEvent::Sound(sfx));
            }
            90 => self.events.push(ForceEvent::Sound(sfx)),
            120 => {
                if self.key_earned {
                    self.keys += 1;
                    self.events.push(ForceEvent::Sound(key_sfx));
                } else if self.final_challenge {
                    self.events.push(ForceEvent::Sound(key_sfx));
                } else if i + 1 < self.data.final_index() && self.bonus_time > 0 {
                    game.fight.clock += self.bonus_time;
                    self.events.push(ForceEvent::Sound(sfx));
                }
                self.tally_done = true;
            }
            _ => {}
        }
    }

    /// Testing: a chicken at game (x, z) (as script event type 4).
    pub fn drop_chicken(&mut self, x: i32, z: i32) {
        self.spawn_chicken(x, z);
    }

    /// Testing: the stage cleared, its tally from the start.
    pub fn start_tally(&mut self) {
        (self.state, self.t) = (7, 0);
    }

    /// Testing: the soldiers are done, the boss is coming (as the clear event: the original's at x = 30720).
    pub fn skip_to_boss(&mut self, game: &mut Game) {
        let script = self.script();
        let clear = script.iter().find(|e| e.kind == 6).map_or(30720, |e| e.x);
        self.ev = script.iter().position(|e| e.kind == 6).unwrap_or(script.len());
        self.cam_x = clear;
        game.fight.fighters[0].position.x = clear as f32;
        (self.state, self.t) = (3, self.f + 60);
        self.set_camera_mode(2);
    }

    /// The tally's frame counter (for its texts).
    pub fn tally_frame(&self) -> u64 {
        self.t
    }

    /// The slot k soldier's popup, for the HUD: seconds, place (metres), frames left.
    pub fn popups(&self) -> impl Iterator<Item = (i32, Vec3, u32)> + '_ {
        self.slots.iter().filter_map(|s| s.popup.map(|(n, at, f)| (n, at / UNITS, f)))
    }
}
