//! Deterministic 60 Hz fight simulation of the original, driven only by the converted JSON game data.
//!
//! The renderer is not involved: each frame the app passes the fighters' body (hurt) spheres, computed from
//! the animated skeleton, and reads back which move/animation frame each fighter is in.
//!
//! Units: game units (≈ 1 mm) in the converted assets' axes (x right, y up, z towards the camera).
//! See kumite_converter's reference (engine.md §7-8 and the rest) for the rules implemented here.

use crate::detmath::DetMath;
pub mod ai;
pub mod ball;
pub mod camera;
pub mod data;
pub mod detmath;
pub mod input;
pub mod melee;

use anyhow::{Context, Result};
use data::{Bank, Cancel, CancelKind, Move, MoveRef, Moveset, PushbackCurve, Reactions, Shared};
use glam::Vec3;
use input::{InputState, Pad};
use std::f32::consts::{FRAC_PI_2, PI};
use std::path::Path;
use std::sync::Arc;

/// Engine-fixed aliases (kumite_converter's reference engine.md §7.1).
pub const ALIAS_NEUTRAL: usize = 3;
/// Cancel transition "use this list's END entry" (walker 0x8002DE24).
const TRANSITION_USE_END: u32 = 0x2C;
/// An air hit: who hit whom, its final damage, the side it came from and the strike's downward speed.
/// A flight's start: the hit's damage and strike speed down, the landing height, the move left (motion, frames,
/// frame), the root's height and falling speed, a spike, extra drift.
struct Pop {
    damage: i32,
    strike_down: f32,
    target: f32,
    prev: (String, u32, u32),
    y0: f32,
    falling: f32,
    spike: bool,
    extra_drift: i32,
    kind: usize,
}

struct AirHit {
    victim: usize,
    attacker: usize,
    damage: i32,
    quadrant: usize,
    strike_down: f32,
    /// The reaction entry's angle parameter p0 (65536 a turn).
    angle: i32,
}

/// Air reaction aliases (0x8002EE94): the late fall, the two side falls, the air recovery.
const AIR_LATE: usize = 0xD62;
const AIR_SIDE: [usize; 2] = [0xD63, 0xD64];
const AIR_RECOVERY: usize = 0xD58;
/// Cancel transition 0x1C: get up (its entries are skipped for 24 frames after one is taken).
const TRANSITION_GETUP: u32 = 0x1C;
/// The game's angle unit (u16, 0x10000 = one turn) in radians.
const ANGLE: f32 = std::f32::consts::TAU / 65536.0;

/// An angle wrapped to -pi..pi.
fn wrap_pi(a: f32) -> f32 {
    let t = std::f32::consts::TAU;
    (a + std::f32::consts::PI).rem_euclid(t) - std::f32::consts::PI
}

/// Queue-time normalization of a transition (0x80030F68): attacks still ahead keep continuing, non-attacks drop
/// the facing variants. Bits 6-7 are kept.
fn normalize_transition(transition: u32, to: Option<&data::Move>, exec: u32) -> u32 {
    let Some(m) = to else { return transition };
    let (a0, a1, len) = (m.active[0], m.active[1], m.frames);
    let ahead = a0 != 0 && exec + 1 < a1 && a1 <= len;
    let t = match transition & 0x3F {
        2 if ahead => 0x14,
        0xC..=0xF if a0 == 0 => 0xB,
        0x14 | 0x15 if !ahead => 0xB,
        0x19 if ahead => 0x16,
        t => t,
    };
    (transition & 0xC0) | t
}

/// The transition of a move's END entry (its list's last entry), used when the animation runs out.
fn end_transition(set: &data::Moveset, mv: &data::Move) -> u32 {
    set.cancels.get(mv.cancel_start..).and_then(|l| l.iter().find(|c| c.kind == CancelKind::End)).map_or(6, |c| c.transition)
}

/// Cancel transitions 0x1A / 0x1B (0x80030424): continue the destination at the next frame.
const TRANSITION_CONTINUE: u32 = 0x1A;
/// The K.O. collapse (common_219, 80 frames).
const KO_COLLAPSE: usize = 0xD66;
/// The alias both fighters fall back to for a move neither set has (move-properties.md §3.1).
const MISSING_FALLBACK: usize = 0xDC5;
const TRANSITION_CONTINUE_2: u32 = 0x1B;
/// Cancel transition "throw / hold link" (0x800306B8): pairs the two fighters.
pub const TRANSITION_LINK: u32 = 0x21;
/// Gravity of a juggled fighter, units per frame² ×2 (y(t) = vy·t − 3t², 0x8002F074).
const AIR_GRAVITY: f32 = 3.0;
/// 16.16 fixed point.
pub const ONE: i32 = 1 << 16;
/// Closest the two fighters' roots may ever come (two root push spheres of radius 300, rules.json); the
/// limb push spheres usually keep them further apart.
pub const PUSH_DISTANCE: f32 = 600.0;

/// A fighter's collision volumes for one frame (world space, game units).
#[derive(Clone, Debug, Default)]
pub struct BodyVolumes {
    /// 14 hurt spheres (index = damage zone).
    pub hurt: Vec<Sphere>,
    /// 8 body push spheres (head, forearms, root, shins, feet).
    pub push: Vec<Sphere>,
}
/// Round clock for the default option (40 s).
pub const ROUND_FRAMES: u32 = 40 * 60;

/// Data shared by every fight: the common move set, the EXE's shared command lists, reactions.
pub struct GameData {
    pub common: Moveset,
    pub shared: Shared,
    pub reactions: Reactions,
    pub zone_percent: Vec<i32>,
    pub max_health: i32,
    /// Juggle damage: first air hit, later air hits (percent; rules.json `juggle`).
    pub juggle_percent: [i32; 2],
    pub characters: CharacterRules,
    pub moves: MoveRules,
    pub ball: BallRules,
}

/// The rules the code keeps for some moves (rules.json `moves`).
#[derive(Clone, Debug, Default)]
pub struct MoveRules {
    /// A hit by a move entered by this alias holds the victim these frames, not the move's hit-stop (0x80044310).
    pub long_hitstop: Option<(usize, u32)>,
    /// The attacker's moves that spike an airborne victim (0x8002EE94).
    pub air_spikes: Vec<AirSpike>,
}

/// Ball's rules for the fighters and its sounds (ball.json `constants`).
#[derive(Clone, Debug, Default)]
pub struct BallRules {
    /// The moves pushed back, and those ending early (0x800B068C).
    pub pushes: Vec<BallPush>,
    /// This move set's unblockable or throw-type hit on the ball (in any alias but this one) takes the other side's
    /// charge from its own health (0x800B16F4).
    pub self_charge: Option<(i64, usize)>,
    pub sounds: BallSounds,
}

/// The system sounds Ball plays (SePlay codes): the serve, the drop after a point, a bounce, the "!" pop-up, the
/// smash, the Attack Ball hitting a fighter (at 50+ power, below; guarded), a bump.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct BallSounds {
    pub serve: u16,
    pub drop: u16,
    pub bounce: u16,
    pub pop: u16,
    pub smash: u16,
    pub hit: [u16; 2],
    pub guarded: u16,
    pub bump: u16,
}

/// An attacker's move set and alias that spikes; not with a cancel into one of `unless_queued` queued (which also
/// skips the strike-down test).
#[derive(Clone, Debug, serde::Deserialize)]
pub struct AirSpike {
    pub moveset: i64,
    pub alias: usize,
    #[serde(default)]
    pub unless_queued: Vec<usize>,
}

/// Ball: these aliases push back `push` units a frame over `frames`, and end at frame `cancel` (0: their own end).
#[derive(Clone, Debug, serde::Deserialize)]
pub struct BallPush {
    pub aliases: Vec<usize>,
    pub push: f32,
    pub frames: [u32; 2],
    pub cancel: u32,
}

impl BallRules {
    /// The frame a move entered by `alias` ends at.
    pub fn end(&self, alias: usize) -> Option<u32> {
        self.pushes.iter().find(|p| p.cancel != 0 && p.aliases.contains(&alias)).map(|p| p.cancel)
    }
}

/// The rules the code keeps for some characters (rules.json `characters`): character ids (+0x18), move set ids.
#[derive(Clone, Debug, Default)]
pub struct CharacterRules {
    /// Thrown only by the same character (0x800404E8).
    pub unthrowable: Vec<i64>,
    /// Nothing reverses them (0x8002D8CC).
    pub unreversible: Vec<i64>,
    /// Each of the pair who is one shortens the throw distance by 40 (0x800452CC).
    pub throw_bulk: Vec<i64>,
    /// Their sidesteps lean 0x71C, not 0x11C7, past 135° off the opponent.
    pub sidestep_lean: Vec<i64>,
    /// Against these move sets a juggle flight holds through the hit-stop (0x8003F248).
    pub hitstop_still: Vec<i64>,
    /// Their heads do not turn sideways (0x80039074).
    pub head_without_yaw: Vec<i64>,
}

impl GameData {
    pub fn load(gamedata: &Path) -> Result<Self> {
        let rules: serde_json::Value = data::load(&gamedata.join("rules.json"))?;
        let zone_percent = rules["damage_zones"]["percent"].as_array().context("rules.json: damage_zones.percent")?.iter().map(|v| v.as_i64().unwrap_or(100) as i32).collect();
        let max_health = rules["health"]["default_max"].as_i64().unwrap_or(130) as i32;
        let pct = |k: &str, d: i64| rules["juggle"][k].as_i64().unwrap_or(d) as i32;
        let juggle_percent = [pct("first_air_hit_percent", 80), pct("later_air_hits_percent", 50)];
        let c = &rules["characters"];
        anyhow::ensure!(c.is_object(), "rules.json: no characters (convert again)");
        let ids = |k: &str, list: &str| c[k][list].as_array().into_iter().flatten().filter_map(|v| v.as_i64()).collect();
        let characters = CharacterRules {
            unthrowable: ids("unthrowable", "characters"),
            unreversible: ids("unreversible", "characters"),
            throw_bulk: ids("throw_bulk", "characters"),
            sidestep_lean: ids("sidestep_lean", "characters"),
            hitstop_still: ids("hitstop_flight", "not_against_movesets"),
            head_without_yaw: ids("head_without_yaw", "characters"),
        };
        let m = &rules["moves"];
        anyhow::ensure!(m.is_object(), "rules.json: no moves (convert again)");
        let hitstop = &m["long_hitstop"];
        let ball: serde_json::Value = data::load(&gamedata.join("ball.json"))?;
        let moves = MoveRules {
            long_hitstop: hitstop["alias"].as_u64().zip(hitstop["frames"].as_u64()).map(|(a, f)| (a as usize, f as u32)),
            air_spikes: serde_json::from_value(m["air_spikes"]["moves"].clone()).context("rules.json: moves.air_spikes")?,
        };
        let (bm, charge) = (&ball["constants"]["moves"], &ball["constants"]["moves"]["self_charge"]);
        let ball = BallRules {
            pushes: serde_json::from_value(bm["push_back"].clone()).context("ball.json: constants.moves.push_back")?,
            self_charge: charge["moveset"].as_i64().zip(charge["except_alias"].as_u64()).map(|(m, a)| (m, a as usize)),
            sounds: serde_json::from_value(ball["constants"]["sounds"].clone()).context("ball.json: constants.sounds")?,
        };
        Ok(Self {
            common: data::load(&gamedata.join("movesets/common.json"))?,
            shared: data::load(&gamedata.join("shared.json"))?,
            reactions: data::load(&gamedata.join("reactions.json"))?,
            zone_percent,
            max_health,
            juggle_percent,
            characters,
            moves,
            ball,
        })
    }
}

/// A body sphere for hit detection (centre in game units, world space).
#[derive(Clone, Copy, Debug)]
pub struct Sphere {
    pub centre: Vec3,
    pub radius: f32,
    pub zone: usize,
}

/// Per-animation root motion (game units, root translation per frame) supplied by the app from the glTF.
pub trait Animations {
    /// `partner`: the animation belongs to the opponent's character (a borrowed throw-victim move).
    fn root(&self, anim: &str, frame: u32, partner: bool) -> Option<Vec3>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Current {
    pub bank: Bank,
    pub index: usize,
}

/// Posture word bits (actor +0x60, kumite_converter's reference move-properties.md §4.1).
pub mod posture {
    pub const CROUCHING: u32 = 0x01;
    pub const STANDING: u32 = 0x02;
    pub const LYING: u32 = 0x04;
    pub const CROUCH_GUARD: u32 = 0x08;
    pub const STAND_GUARD: u32 = 0x10;
    pub const GUARDS: u32 = CROUCH_GUARD | STAND_GUARD;
    /// The guard bits only count while the guard input flag (+0xCA) is set.
    pub const GUARD_NEEDS_INPUT: u32 = 0x100;
    pub const LYING_FACEDOWN: u32 = 0x200;
    pub const AIRBORNE: u32 = 0x400;
    pub const STANDING_GUARD: u32 = 0x1052;
    pub const CROUCHING_GUARD: u32 = 0x2829;
}

/// A visual effect a move's timed property fires (move-properties.md §1.3 ops 0x01..0x0A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveEffect {
    /// Screen shake 0 small / 1 medium / 2 large (ops 1..3; scripts in camera.json `shake`).
    Shake(u8),
    /// Ground impact at the root (op 4: falls, slams, throws).
    GroundImpact,
    /// A juggle's landing hitting the ground (0x8003F350): the same impact, not the move's op (no rumble of its own).
    LandingImpact,
    /// Effect at a bone slot (ops 6..0x0A: 6/7 the pad-owned spark ring, 8/9/0xA neutral kinds 0/1/3).
    AtBone { op: u32, slot: u32 },
}

/// A running pushback (mover 0x8003F798): the curve, its frame counter and the world direction.
#[derive(Clone, Debug)]
pub struct Pushback {
    pub curve: PushbackCurve,
    pub frame: i32,
    pub direction: glam::Vec2,
}

/// What a fighter knows about the opponent when its command list is checked (updated every frame).
#[derive(Clone, Copy, Debug, Default)]
pub struct Opponent {
    pub position: Vec3,
    pub side: i32,
    /// Its posture word, facing and whether it is in the air (throw position codes).
    pub posture: u32,
    pub yaw: f32,
    /// Its root (position + root motion) and evasive timer (+0x11C).
    pub root: Vec3,
    pub evasive: u32,
    /// Its attack mask (+0x64), frame, last active frame, jump window start (record +0x19) and whether it can
    /// be thrown this frame (+0xDF clear).
    pub mask: u32,
    pub frame: u32,
    pub active_end: u32,
    pub jump_start: u32,
    pub unthrowable: bool,
    pub airborne: bool,
    /// Its moveset and character (cancel `req` gates).
    pub moveset_id: Option<i64>,
    pub character: Option<i64>,
}

/// A juggled fighter's flight (0x8002F074): height y(t) = y0 + vy·t − 3t² (game units, up), landing at
/// `land` (the fall animation's root height at its hold frame).
#[derive(Clone, Copy, Debug)]
pub struct Air {
    /// Horizontal drift away from the attacker (+0x98, game units a frame).
    pub drift: f32,
    /// The root's height and its downward speed (+0x9C, units a frame): each frame the speed grows by 6 and the
    /// height drops by it (0x8003F238), so y = y0 + vy·t − 3t² − 3t.
    pub y: f32,
    pub down: f32,
    /// The air table's landing height (+0x96): it lands once falling at or below it.
    pub target: f32,
    /// A spike (its landing shakes harder).
    pub spike: bool,
    /// The air table entry it flies by (+0x90; 5 = the air setup, 0xD57).
    pub kind: usize,
}

impl Air {
    pub fn height(&self) -> f32 {
        self.y
    }
}

#[derive(Clone)]
pub struct Fighter {
    pub moveset: Arc<Moveset>,
    pub input: InputState,
    pub current: Current,
    /// Frame within the current move, starting at 1 like the game's actor+0x58.
    pub frame: u32,
    /// Root position on the floor (game units) and facing (radians; 0 = +z).
    pub position: Vec3,
    pub yaw: f32,
    /// Root-motion basis (+0x0E): the yaw the animation's root motion is laid out along. Mode 0 keeps the body
    /// facing on it; the other tracking modes turn the body away from it.
    pub root_yaw: f32,
    /// Per-frame facing rule set by the commit (+0xB9, driver 0x8003F890), its accumulated turn (+0x36), the
    /// frames since the move was entered (+0x5C) and the "evasive" timer (+0x11C: 9 after starting a move with
    /// flags24 bit 0x800, sidesteps / get-ups / throw attempts).
    mode: u8,
    acc: f32,
    since: u32,
    pub evasive: u32,
    /// Root position at the commit (+0xAC/+0xB0, ground rolls).
    commit_root: Vec3,
    /// Root pin (+0xB8): 2 after a landing (only the root's height is added to the origin) until the next move.
    pin: u8,
    /// The current move commits its root into the origin when it is left (flags24 0x10000000).
    commits_root: bool,
    /// Sign of the current move's lateral root motion (sidestep facing offset, mode 0xA).
    lateral: f32,
    pub health: i32,
    /// Full health in whole points (the VS handicap scales it: 70..140 %).
    pub max_health: i32,
    /// Character id (cancel `req` gates 0x46..; set by the game layer).
    pub character: Option<i64>,
    /// Queued cancel: (destination, frame at which it takes over, transition).
    queued: Option<(Current, u32, u32)>,
    /// The queued cancel came from the END look-ahead (a buffered attack: it starts at frame 1).
    queued_buffered: bool,
    /// Frame direction: 1, or -1 while a move plays backwards (transitions 7 / 8).
    frame_step: i32,
    /// Auto-turn (+0x114/+0x116): (radians per frame, frames left).
    turn: Option<(f32, u32)>,
    /// 0x1C (get-up) entries are skipped while this counts down (0x80030494, +0x128 = 24).
    getup_lock: u32,
    /// Horizontal root-motion offset of the current move (world, game units): shown on top of `position`
    /// and committed to it when the move ends or is cancelled.
    pub offset: Vec3,
    /// The current attack already connected (one hit per move).
    pub connected: bool,
    /// How the current attack's contact went: hit (true) or blocked (false), and whether it was a counter hit
    /// (conds 0x1A..0x1C, 0x39).
    pub landed: Option<bool>,
    pub landed_counter: bool,
    /// Knockdown stun pool (+0x118) and reaction timer (+0x11A): the time on the ground after a juggle, drained
    /// faster by pressing buttons (cond 0x31 / 0x32).
    pub stun: u32,
    pub stun_timer: u32,
    /// Ki charge timer (+0x122): no guard, counter hits and chip while it runs (cond 0x43 / 0x44).
    pub ki: u32,
    /// A reversal's damage (+0xEA: the reversed attack's / 2 + 25): it replaces the next move's damage at the
    /// commit (`damage_override`, used by its timed damage).
    reversal_damage: Option<i32>,
    damage_override: Option<i32>,
    pub hit_flash: u32,
    /// The fighter's side (actor +0x1278): where its pelvis falls on the screen, left negative, set as each frame
    /// is drawn (0x80037924; the round's start seeds −100 / +100 before the first). The one further right has the
    /// pad's left as forward (0x80046234 / 0x8002C580).
    pub side: i32,
    /// Screen side (+0xC6): on the right of the opponent on screen (recomputed every frame).
    on_right: bool,
    pub opponent: Opponent,
    /// Guard input flag (+0xCA): set for human players (+0xC5 == 0), clear for the CPU. While set, the
    /// "needs guard input" stances (neutral stand, crouch) keep their guard, so standing still blocks highs and
    /// mids and crouching blocks lows; without it only guard moves (walking back, crouch guard) block.
    pub guard_input: bool,
    /// Pushback from the last hit or block.
    pub pushback: Option<Pushback>,
    /// Juggle flight, and the juggle counter (+0x94: air hits taken since landing).
    pub air: Option<Air>,
    pub juggle: u32,
    /// The jump drift's fractions (0x8009E600 / 0x8009E610 per player, 1/256 units).
    air_frac: [i32; 2],
    /// Hit-stop: frames the move is frozen (attacker record +0x2C).
    pub freeze: u32,
    /// Paired with the opponent by a throw / hold (+0xC4); `thrown` = this fighter is the victim (+0x74 < 0).
    pub locked: bool,
    pub thrown: bool,
    /// Throw escape latch (+0xE0..0xE2): the first new press while thrown: 1 = button 1, 2 = button 2,
    /// 3 = 1+2, 0 = something else (locked out).
    pub escape: Option<u8>,
    /// The transition the current move started with (0x21 links a throw), set for the step it started on.
    pub started_with: Option<u32>,
    /// Root height of the current animation frame (game units), for juggles.
    pub anim_root_y: f32,
    /// The root's height last frame and its downward speed (0x80040A0C, +0x3C8): a pop is cut by it.
    last_height: f32,
    pub root_fall: f32,
    /// A timed "damage the opponent" property fired this frame (op 0x0B, throws): the amount.
    pub deal_damage: Option<i32>,
    /// Repeated side / back hits (0x80031348): the count of same-type hits in a row (+0x124), the time left to
    /// chain the next (+0x126) and the last type (+0x13A); each adds 40·count + 10 to the pushback's impulse frames.
    pub side_chain: u32,
    /// The guard posture a guard-by-input move last wrote (it stays until the move restarts).
    held_guard: Option<u32>,
    /// Frames a contact leaves it unhittable (+0x104: 4 after a damaging or airborne contact, cleared by any move
    /// change; it only matters when no reaction follows).
    invulnerable: u32,
    /// It landed from a flight this frame (+0xDD / +0xDC): 1 a juggle, 2 the air setup's (0xD57).
    pub landing: u8,
    /// An air recovery was taken in this airtime (+0x8E; cond 0x42 allows one).
    air_recovered: bool,
    side_timer: u32,
    side_last: u32,
    /// Timed ops 0x0C / 0x0D this frame: damage to itself, less heals (0x800449B4).
    pub self_damage: i32,
    /// Guard while recovering (+0xB6): set by transitions 0x1A (1) / 0x1B (2), cleared by any other commit;
    /// back then guards standing, down-back crouching (0x80040598), and 2 also adds 1 to attacks' masks.
    pub recover_guard: u8,
    /// The opponent's move set while playing its moves (`Bank::Partner`).
    pub partner: Option<Arc<Moveset>>,
    /// Visual effects the current move fired this frame.
    pub effects: Vec<MoveEffect>,
    /// Alias (move id) the current move was entered by (+0xA0), and the queued cancel's.
    pub alias: usize,
    queued_alias: usize,
    /// Throw chain stage (+0x74): n > 0 = the thrower in its n-th part, n < 0 = the victim; set by the link
    /// commits (transitions 0x21 / 0x22, 0x800306B8 / 0x800307DC), cleared by the other transitions.
    pub chain: i32,
    /// Struck (hit or blocked) in this frame's hit pass (+0xCE) and during the current move (+0x87,
    /// 0x800442D8); the own attack reached its last active frame without contact (+0xD2, 0x80044374).
    pub struck: bool,
    pub struck_in_move: bool,
    pub whiffed: bool,
    /// The body push moved the pair apart this frame (+0x3E8..+0x3F0 non-zero).
    pub pushed: bool,
    /// Ball's back-steps: the END look-ahead opens from this frame (0x800B6678 +5, EXE 0x8002DB9C).
    pub early_end: Option<u32>,
    /// Out of play (+0xC2): a Force slot with no soldier in it; not stepped, hit or pushed.
    pub inactive: bool,
}

/// Facing angles in game units (u16 turn = 0x10000), as the actor keeps them (0x80043A94): `off` (+0x2E) the
/// body's facing off the direction to the opponent, `turn` (+0x30) its size, `quadrant` (+0x32) where the
/// opponent is (0 front, 2 behind); `opp_*` (+0x3C / +0x3E / +0x40) the same seen from the opponent.
#[derive(Clone, Copy, Debug, Default)]
pub struct Angles {
    pub off: i32,
    pub turn: i32,
    pub quadrant: u32,
    pub opp_off: i32,
    pub opp_turn: i32,
    pub opp_quadrant: u32,
}

impl Fighter {
    pub fn new(moveset: Arc<Moveset>, max_health: i32, position: Vec3, yaw: f32, side: i32) -> Self {
        let mut f = Self {
            moveset,
            input: InputState::default(),
            current: Current { bank: Bank::Own, index: 0 },
            frame: 1,
            position,
            yaw,
            root_yaw: yaw,
            mode: 0,
            acc: 0.0,
            since: 1,
            evasive: 0,
            commit_root: position,
            pin: 0,
            commits_root: false,
            lateral: 0.0,
            health: max_health * ONE,
            max_health,
            character: None,
            queued: None,
            queued_buffered: false,
            frame_step: 1,
            turn: None,
            getup_lock: 0,
            offset: Vec3::ZERO,
            connected: false,
            landed: None,
            landed_counter: false,
            stun: 0,
            stun_timer: 0,
            ki: 0,
            reversal_damage: None,
            damage_override: None,
            hit_flash: 0,
            side,
            on_right: side > 0,
            opponent: Opponent::default(),
            guard_input: true,
            pushback: None,
            air: None,
            juggle: 0,
            air_frac: [0; 2],
            freeze: 0,
            locked: false,
            thrown: false,
            escape: None,
            started_with: None,
            anim_root_y: 0.0,
            last_height: 0.0,
            root_fall: 0.0,
            deal_damage: None,
            self_damage: 0,
            side_chain: 0,
            air_recovered: false,
            landing: 0,
            invulnerable: 0,
            held_guard: None,
            side_timer: 0,
            side_last: 0,
            recover_guard: 0,
            partner: None,
            effects: Vec::new(),
            alias: ALIAS_NEUTRAL,
            queued_alias: ALIAS_NEUTRAL,
            chain: 0,
            struck: false,
            struck_in_move: false,
            whiffed: false,
            pushed: false,
            early_end: None,
            inactive: false,
        };
        f.current = f.resolve(ALIAS_NEUTRAL).unwrap_or(f.current);
        f
    }

    /// Move id -> move. While playing a partner move, ids resolve in the partner's table (its own moves stay
    /// partner moves) until they reach a common move.
    /// An alias in this fighter's merged table: its own entry, else the opponent's (see [`Fighter::borrow`]).
    pub(crate) fn resolve(&self, alias: usize) -> Option<Current> {
        let r: &MoveRef = self.moveset.aliases.as_ref()?.get(alias)?;
        match r.bank {
            Bank::None | Bank::Partner => self.borrow(alias),
            bank => Some(Current { bank, index: r.index? }),
        }
    }

    /// An alias this fighter's set lacks (header +0x18 bitmap, merged at match load by 0x8002D07C): the
    /// opponent's entry (its own record, played from its file, or a common one); when both lack it, the own
    /// entry of alias 0xDC5.
    fn borrow(&self, alias: usize) -> Option<Current> {
        if let Some(r) = self.partner.as_ref().and_then(|p| p.aliases.as_ref()).and_then(|a| a.get(alias)) {
            match r.bank {
                Bank::Own => return Some(Current { bank: Bank::Partner, index: r.index? }),
                Bank::Common => return Some(Current { bank: Bank::Common, index: r.index? }),
                _ => {}
            }
        }
        if alias == MISSING_FALLBACK {
            return None;
        }
        let r = self.moveset.aliases.as_ref()?.get(MISSING_FALLBACK)?;
        match r.bank {
            Bank::Own | Bank::Common => Some(Current { bank: r.bank, index: r.index? }),
            _ => None,
        }
    }

    /// Start the opponent's move `alias` on this fighter (throw victims).
    pub fn play_partner_alias(&mut self, alias: usize, partner: Arc<Moveset>) -> bool {
        let r = partner.aliases.as_ref().and_then(|a| a.get(alias)).copied();
        self.partner = Some(partner);
        let to = match r {
            Some(MoveRef { bank: Bank::Own, index: Some(i) }) => Current { bank: Bank::Partner, index: i },
            Some(MoveRef { bank: Bank::Common, index: Some(i) }) => Current { bank: Bank::Common, index: i },
            _ => return self.play_alias(alias),
        };
        self.start(to);
        self.alias = alias;
        true
    }

    /// The move set a bank refers to.
    pub fn moves<'a>(&'a self, gd: &'a GameData, bank: Bank) -> &'a Moveset {
        match bank {
            Bank::Common => &gd.common,
            Bank::Partner => self.partner.as_deref().unwrap_or(&self.moveset),
            _ => &self.moveset,
        }
    }

    pub fn current_move<'a>(&'a self, gd: &'a GameData) -> &'a Move {
        &self.moves(gd, self.current.bank).moves[self.current.index]
    }

    /// The glTF animation and frame to display (0-based).
    pub fn animation<'a>(&'a self, gd: &'a GameData) -> (&'a str, u32) {
        (&self.current_move(gd).anim, self.frame.saturating_sub(1))
    }

    /// Start a move now (debug tools: play any move of the character's set or the common set).
    pub fn play(&mut self, to: Current) {
        self.start(to);
    }

    /// Start the move an alias (move id) points to; false when the alias is empty for this character.
    pub fn play_alias(&mut self, alias: usize) -> bool {
        match self.resolve(alias) {
            Some(to) => {
                self.start(to);
                self.alias = alias;
                true
            }
            None => false,
        }
    }

    /// The cancel queued by the command list: (destination, frame at which it takes over; 255 = at the end).
    pub fn queued(&self) -> Option<(Current, u32)> {
        self.queued.map(|q| (q.0, q.1))
    }

    /// Start a move outside the command lists (round start, debug tools): it faces the opponent.
    fn start(&mut self, to: Current) {
        self.start_facing(to, self.commits_root);
        self.chain = 0;
        if !self.locked {
            if let Some(w) = self.toward() {
                self.yaw = w;
            }
        }
        self.root_yaw = self.yaw;
        self.mode = 0;
    }

    /// The per-frame facing mode (+0xB9) and the auto-turn frames left (debug overlay).
    pub fn facing_mode(&self) -> (u8, u32) {
        (self.mode, self.turn.map_or(0, |t| t.1))
    }

    /// Angle from my root to the opponent's root (+0x2A), when they are apart.
    fn toward(&self) -> Option<f32> {
        let d = self.opponent.root - self.root();
        (d.x * d.x + d.z * d.z > 1.0).then(|| d.x.datan2(d.z))
    }

    /// Facing geometry (0x80043A94): the angle to the opponent, how far the body is turned off it (+0x30), and
    /// the target facing (+0x34): the opponent's direction, or its opposite while more than 90° off (a
    /// back-turned fighter stays back-turned).
    fn geometry(&self) -> (f32, f32, f32) {
        let a = self.toward().unwrap_or(self.yaw);
        let off = wrap_pi(self.yaw - a).abs();
        (a, off, if off < FRAC_PI_2 { a } else { a + PI })
    }

    /// Commit a cancel or END link with its transition type (0x8002F3AC, handlers 0x80019D40): where the new move
    /// starts (frame 1, the next frame, or backwards), the root kept in place, and its facing: the yaws at the
    /// commit, an auto-turn and the per-frame tracking mode (facing_re §2.2). `end`: an END link.
    fn commit(&mut self, gd: &GameData, to: Current, alias: usize, transition: u32, end: bool, anims: &dyn Animations) {
        let buffered = std::mem::take(&mut self.queued_buffered);
        let t = transition & 0x3F;
        self.recover_guard = match t {
            TRANSITION_CONTINUE => 1,
            TRANSITION_CONTINUE_2 => 2,
            _ => 0,
        };
        // the throw chain stage (+0x74): the thrower's link counts up (a victim breaking free starts at 1), the
        // victim's counts down; continue / commit-only / plain-cut types keep it, the others clear it
        self.chain = match t {
            0x21 => {
                if self.chain < 0 {
                    1
                } else {
                    self.chain + 1
                }
            }
            0x22 => {
                if self.chain > 0 {
                    -1
                } else {
                    self.chain - 1
                }
            }
            0x11 | 0x1A | 0x1B | 0x1F | 0x20 | 0x2C.. => self.chain,
            _ => 0,
        };
        let (old_turn, old_len, old_anim, old_flags) = {
            let m = self.current_move(gd);
            (m.next.hi, m.frames.max(1), m.anim.clone(), m.flags24.raw)
        };
        let (old_frame, old_height, old_falling) = (self.frame, self.position.y + self.anim_root_y, self.air.map_or(self.root_fall, |a| a.down));
        let to_mv = self.moves(gd, to.bank).moves.get(to.index).cloned();
        let len = to_mv.as_ref().map_or(1, |m| m.frames.max(1));
        let (a0, a1) = to_mv.as_ref().map_or((0, 0), |m| (m.active[0], m.active[1]));
        // continue the frame number unless buffered or the destination's last active frame has passed (0x80031070)
        let cont = !buffered && (a0 == 0 || (a0 <= len && self.frame <= a1));
        let (root, queued, turn) = (self.root(), self.queued, self.turn);
        // the origin (0x80019D40 handlers): set to the last root for types 0 / 0x1F / 0x20 / 0x2C.. and the
        // rebasing continue types, kept for 0x1A / 0x1B / 0x11 / 4 / 5, else set only when the move left commits its
        // root (flags24 0x10000000); a throw link takes the partner's (Fight::link)
        let commit_origin = match t {
            0 | 0x1F | 0x20 | 0x2C.. | 1 | 2 | 3 | 8 | 0x14 | 0x15 | 0x16 | 0x19 => true,
            4 | 5 => false,
            _ => self.commits_root,
        };
        let rebase = matches!(t, 1 | 2 | 3 | 8 | 0x14 | 0x15 | 0x16 | 0x19);
        match t {
            TRANSITION_CONTINUE | TRANSITION_CONTINUE_2 => {
                // 0x80030424: the new move goes on at the next frame, root kept (hit reactions point these at
                // themselves: the frame where guarding works again)
                self.current = to;
                self.frame = (self.frame + 1).min(len);
                self.queued = None;
            }
            0x11 => {
                // only the logic move changes
                self.current = to;
                self.queued = None;
            }
            1 | 2 | 3 | 0x12..=0x16 | 0x19 | 0x1D => {
                // continue at the next frame (or 1), the root rebased so it does not jump (+0xBE)
                let f = if cont { (self.frame + 1).min(len) } else { 1 };
                self.start_facing(to, commit_origin);
                self.frame = f;
            }
            0x1E => {
                self.start_facing(to, commit_origin);
                self.frame = queued.map_or(1, |q| q.1).clamp(1, len);
            }
            7 | 8 => {
                // played backwards: from the end (7), or from the previous frame (8)
                let f = if t == 7 { len } else { self.frame.saturating_sub(1).max(1) };
                self.start_facing(to, commit_origin);
                self.frame = f;
                self.frame_step = -1;
            }
            _ => self.start_facing(to, commit_origin),
        }
        if t == TRANSITION_GETUP {
            self.getup_lock = 24;
        }
        // facing: bit 7 turns around first; END and buffered links add the old animation's net turn (+0x12)
        if t != 0x11 {
            self.turn = turn;
            self.mode = 0;
            self.acc = 0.0;
        }
        if transition & 0x80 != 0 {
            self.yaw += PI;
            self.root_yaw += PI;
        }
        if (end || buffered) && t != 0x22 && old_turn != 0 {
            self.yaw += old_turn as f32 * ANGLE;
            self.root_yaw = self.yaw;
        }
        let (a2a, off, target) = {
            let d = self.opponent.root - root;
            let a = if d.x * d.x + d.z * d.z > 1.0 { d.x.datan2(d.z) } else { self.yaw };
            let off = wrap_pi(self.yaw - a).abs();
            (a, off, if off < FRAC_PI_2 { a } else { a + PI })
        };
        let near = off < 0x4FA4 as f32 * ANGLE; // 112°
        let n_att = a0.saturating_sub(1).max(1);
        let n_b = if a0 != 0 { n_att } else { old_len.min(16) };
        let yaw = self.yaw;
        let auto = |to: f32, n: u32| Some((wrap_pi(to - yaw) / n as f32, n));
        match t {
            0 | 0x1F | 0x20 | 0x2C.. if !self.locked => {
                self.yaw = a2a;
                self.root_yaw = a2a;
            }
            1 | 6 | 7 | 8 | TRANSITION_CONTINUE | TRANSITION_CONTINUE_2 | 0x17 | 0x1C | 0x1D | 0x25 | 0x27..=0x2B => self.root_yaw = yaw,
            4 | 5 | 0xA => self.root_yaw = yaw,
            9 | 0xB | 0xF | 0x18 | 0x1E | 0x23 | 0x24 | 0x26 => self.root_yaw = target,
            0xC..=0xE => self.root_yaw = if near { a2a } else { target },
            0x10 | 0x12 | 0x13 => self.root_yaw = a2a,
            _ => {}
        }
        match t {
            1..=3 => self.mode = 0xB,
            4 => {
                self.mode = 0xB;
                if near {
                    self.turn = auto(a2a, n_att);
                }
            }
            5 | 0x14 => (self.mode, self.turn) = (if near { 4 } else { 6 }, None),
            0x15 => (self.mode, self.turn) = (if near { 2 } else { 6 }, None),
            9 => (self.mode, self.turn) = (5, None),
            0xA | 0xB => (self.mode, self.turn) = (0xB, auto(target, n_b)),
            0xC..=0xE => (self.mode, self.turn) = (if near { [4, 3, 2][t as usize - 0xC] } else { 6 }, None),
            0xF => (self.mode, self.turn) = (1, None),
            0x10 | 0x12 | 0x13 => {
                (self.mode, self.turn) = (0xB, auto(a2a, n_att));
                if t == 0x13 {
                    self.yaw += PI;
                }
            }
            0x16 => (self.mode, self.turn) = (8, None),
            0x18 | 0x19 => self.mode = 8,
            0x17 => self.mode = 9,
            0x1C | 0x1D => (self.mode, self.turn) = (0xA, None),
            0x1E | 0x24 | 0x25 | 0x27..=0x2B => (self.mode, self.turn) = (0xB, None),
            0x23 | 0x26 => (self.mode, self.turn) = (7, None),
            _ => {}
        }
        if rebase {
            // +0xBE (0x8003A9C8): the origin goes back by the new clip's root at the previous frame, so the root
            // stays where it was and moves on by the clip's own delta
            let (anim, af) = self.animation(gd);
            let prev = (af as i32 - self.frame_step).max(0) as u32;
            let partner = self.current.bank == Bank::Partner;
            if let (Some(r), Some(rp)) = (anims.root(anim, af, partner), anims.root(anim, prev, partner)) {
                let (s, c) = self.root_yaw.dsin_cos();
                let turn = |d: Vec3| Vec3::new(d.x * c + d.z * s, 0.0, -d.x * s + d.z * c);
                self.position = Vec3::new(root.x, self.position.y, root.z) - turn(rp);
                self.offset = turn(r);
            }
        }
        self.commits_root = self.current_move(gd).flags24.raw & 0x1000_0000 != 0;
        if t != 0x11 {
            let same = self.current_move(gd).anim == old_anim;
            self.since = if same && !buffered { self.frame } else { 1 };
        }
        if to_mv.as_ref().is_some_and(|m| m.flags24.raw & 0x800 != 0) {
            self.evasive = 9;
        }
        self.damage_override = self.reversal_damage.take();
        if old_flags & 0x10000 != 0 {
            self.ki = 120; // ki charge: 120 frames from the end of the charge move (0x8002F94C)
        }
        if t == 0x1E {
            // an air setup (0x80030574: one character's rebound, the air recoveries): a flight to the air table's entry
            // 5 height, 10 more drift, counted as a juggle, the stun pool as an air hit's, one per airtime
            let target = gd.reactions.air_reactions.entries.get(5).map_or(1357.0, |a| a.target_height as f32);
            self.stun = if old_flags & 0x1000 != 0 { 0 } else { (self.stun / 2).min(0x42) };
            let pop = Pop { damage: 0, strike_down: 0.0, target, prev: (old_anim.clone(), old_len, old_frame), y0: old_height, falling: old_falling, spike: false, extra_drift: 10, kind: 5 };
            self.air_pop(gd, pop);
            self.air_recovered = true;
        } else {
            // a move change not caused by a hit ends the flight and the juggle count (0x8002F7B8)
            self.air = None;
            self.juggle = 0;
        }
        self.commit_root = self.root();
        self.lateral = 0.0;
        if self.mode == 0xA {
            let (anim, _) = self.animation(gd);
            let partner = self.current.bank == Bank::Partner;
            if let (Some(r), Some(r0)) = (anims.root(anim, len - 1, partner), anims.root(anim, 0, partner)) {
                self.lateral = (r.x - r0.x).signum();
            }
        }
        self.started_with = Some(transition);
        self.alias = alias;
    }

    /// Per-frame facing (driver 0x8003F098, after both fighters' move logic): the auto-turn, then the tracking
    /// mode the commit chose (switch 0x8003F890, facing_re §3).
    fn track(&mut self, gd: &GameData) {
        let (a2a, off, target) = self.geometry();
        if let Some((step, left)) = self.turn {
            self.yaw += step;
            if self.locked && !self.thrown {
                self.root_yaw = self.yaw;
            }
            self.turn = (left > 1).then_some((step, left - 1));
        }
        let mv = self.current_move(gd);
        let (a0, a1, len) = (mv.active[0], mv.active[1], mv.frames.max(1));
        let attack = a0 != 0 && mv.attack.is_some();
        let (j0, j1) = (mv.bytes_18.jump_start, mv.bytes_18.jump_end);
        let frame = self.frame;
        let airborne = self.juggle != 0 || self.air.is_some() || (j0 != 0 && (j0..=j1).contains(&frame));
        let n = (a0 as i32 + 1 - frame as i32).max(1) as f32;
        let deg = |d: f32| d.to_radians();
        let clamp = |x: f32, l: f32| x.clamp(-l, l);
        let opp_evasive = self.opponent.evasive != 0;
        let homing = opp_evasive && attack && self.since < 9;
        let to_opp = wrap_pi(a2a - self.yaw);
        let to_target = wrap_pi(target - self.yaw);
        match self.mode {
            0 => self.yaw = self.root_yaw,
            1 => {
                let s = self.cap(clamp(to_target / n, deg(30.0)), PI);
                self.yaw += s;
                self.root_yaw = self.yaw;
                self.fin(s, a0);
            }
            // homing: an attack starting while the opponent has just started a sidestep (or another evasive move)
            2 | 3 | 5 if homing => {
                let m = a0.saturating_sub(frame).max(1);
                self.turn = Some((clamp(to_opp / m as f32, 0x9F4 as f32 * ANGLE), m));
                self.mode = 0xC;
            }
            2 | 3 => {
                let mut k = deg(if airborne {
                    1.0
                } else if self.since < 8 {
                    3.0
                } else {
                    14.0
                });
                if self.mode == 3 {
                    k /= 2.0;
                }
                let s = self.cap(clamp(to_opp / n, k), deg(120.0));
                self.yaw += s;
                self.fin(s, a0);
            }
            // linear moves: no tracking while an auto-turn runs, or against a sidestep
            4 if self.turn.is_none() && !(opp_evasive && attack) => {
                let k = deg(if airborne {
                    1.0
                } else if self.since < 8 {
                    2.0
                } else {
                    3.0
                });
                let s = self.cap(clamp(to_opp / n, k), deg(20.0));
                self.yaw += s;
                self.fin(s, a0);
            }
            5 => {
                let k = deg(if airborne {
                    4.0
                } else if self.since < 8 {
                    3.0
                } else {
                    100.0
                });
                let s = self.cap(clamp(to_opp / n, k), deg(210.0));
                self.yaw += s;
                self.fin(s, a0);
            }
            // after start-up: re-aim every 5 frames at ≤ 3°/frame (0x8003FD80)
            6 => {
                if self.connected && j0 != 0 && !airborne && frame >= a1 {
                    self.turn = None;
                } else if frame > a0 && (frame - a0).is_multiple_of(5) && off < deg(120.0) {
                    let left = len as i32 - frame as i32;
                    let m = if left < 6 { 5 } else { left as u32 };
                    self.turn = Some((clamp(to_target / m as f32, 0x222 as f32 * ANGLE), m));
                }
            }
            // guarding turns toward the attacker
            7 => {
                let t = if j1 != 0 { j1 } else { len };
                if frame <= t {
                    self.yaw += clamp(to_target * frame as f32 / t as f32, 0x38E as f32 * ANGLE);
                }
            }
            // walks and dashes track all the way
            8 => self.yaw += clamp(to_target * self.since.min(8) as f32 / 8.0, 0x71C as f32 * ANGLE),
            // ground rolls: the basis turns back toward where the roll started
            9 => {
                let dist = (self.opponent.root - self.root()).length();
                let d = self.commit_root - self.root();
                if dist >= 0x4B0 as f32 && d.x * d.x + d.z * d.z > 1.0 {
                    let mut a = d.x.datan2(d.z);
                    if wrap_pi(a - self.root_yaw).abs() > FRAC_PI_2 {
                        a += PI;
                    }
                    self.root_yaw += clamp(wrap_pi(a - self.root_yaw) * frame as f32 / len as f32, deg(8.0));
                    self.yaw = self.root_yaw;
                }
            }
            // sidesteps (and get-ups): face the opponent's live direction every frame, leaning up to 25° into
            // the step
            0xA if frame <= len => {
                let o = if self.character.is_some_and(|c| gd.characters.sidestep_lean.contains(&c)) && off >= deg(135.0) { 0x71C } else { 0x11C7 };
                self.yaw = target + self.lateral * (o as f32 * ANGLE) * frame as f32 / len as f32;
                self.root_yaw = self.yaw;
            }
            _ => {}
        }
        self.evasive = self.evasive.saturating_sub(1);
    }

    /// Limit a tracking step so the move's total turn stays within `limit`; reaching it ends the tracking (mode 6).
    fn cap(&mut self, step: f32, limit: f32) -> f32 {
        if self.acc.abs() + step.abs() > limit {
            self.mode = 6;
            step.signum() * (limit - self.acc.abs()).max(0.0)
        } else {
            step
        }
    }

    /// Count a tracking step; the start-up tracking ends at the first active frame.
    fn fin(&mut self, step: f32, a0: u32) {
        self.acc += step;
        if self.frame >= a0 {
            self.mode = 6;
        }
    }

    /// Start a hit reaction: it keeps the victim's facing (a hit from behind plays the back reaction), as the
    /// reaction transitions do (0x25 / 0x27..0x2B).
    fn start_reaction(&mut self, to: Current, alias: usize) {
        self.start_facing(to, self.commits_root);
        self.alias = alias;
        self.chain = 0;
        self.root_yaw = self.yaw;
        (self.mode, self.acc, self.since) = (0xB, 0.0, 1);
    }

    /// Switch to a move at frame 1; `commit`: the origin moves to the root of the last frame shown.
    fn start_facing(&mut self, to: Current, commit: bool) {
        self.recover_guard = 0;
        self.invulnerable = 0;
        self.held_guard = None;
        if commit {
            self.position += Vec3::new(self.offset.x, 0.0, self.offset.z);
        }
        self.offset = Vec3::ZERO;
        self.pin = 0;
        self.escape = None;
        self.current = to;
        self.frame = 1;
        self.frame_step = 1;
        self.turn = None;
        self.queued = None;
        self.queued_buffered = false;
        self.connected = false;
        self.landed = None;
        self.landed_counter = false;
        self.struck_in_move = false;
    }

    /// Jumps and hops drift (0x8003F3A8): from the frame before the jump window to its end, the move's +0x16
    /// moves the fighter along its facing by drift · (sin, cos) / 2048 a frame (negative: forward), the fractions
    /// carried over in 1/256 units.
    fn drift(&mut self, gd: &GameData) {
        let mv = self.current_move(gd);
        let (j0, j1, v) = (mv.bytes_18.jump_start, mv.bytes_18.jump_end, mv.air_drift);
        if j0 == 0 || v == 0 || self.frame + 1 < j0 || self.frame > j1 {
            return;
        }
        let (s, c) = self.yaw.dsin_cos();
        let table = |x: f32| (x * 4096.0).round() as i64;
        for (k, t) in [table(s), table(c)].into_iter().enumerate() {
            // (v * t) / 32768 truncated, then the whole units out and the fraction kept
            self.air_frac[k] += (v as i64 * t / 32768) as i32;
            let whole = self.air_frac[k] >> 8;
            self.air_frac[k] -= whole << 8;
            if k == 0 {
                self.position.x -= whole as f32;
            } else {
                self.position.z -= whole as f32;
            }
        }
    }

    /// The move this fighter goes to next (the queued one, else the current move's next alias) is a lying one
    /// (posture 0x204; the practice dummy's 0x800B82E4).
    pub fn next_is_lying(&self, gd: &GameData) -> bool {
        let next = self.queued.map(|q| q.0).or_else(|| self.resolve(self.current_move(gd).next.alias));
        next.is_some_and(|to| self.moves(gd, to.bank).moves.get(to.index).is_some_and(|m| m.posture.raw & 0x204 != 0))
    }

    /// The flight's pop (0x8002F074) on the move just started: vy from the damage, the juggle count and the speeds
    /// down, then stretched so the fall animation reaches its hold frame before the landing, from frame 1 (a spike:
    /// straight into the floor from frame 6).
    fn air_pop(&mut self, gd: &GameData, pop: Pop) {
        let f = self;
        let Pop { damage, strike_down, target, prev: (old_motion, old_frames, old_frame), y0, falling, spike, extra_drift, kind } = pop;
        let n = f.juggle as i32;
        let mv = f.current_move(gd);
        let (hold, lying, same_clip) = (mv.bytes_18.hold_frame as i32, mv.posture.raw & 4 != 0, mv.anim == old_motion);
        let hold = if hold > 0 { hold } else { old_frames as i32 };
        // the pop (0x8002F074): vy from the damage, the juggle count and the speeds down; then stretched so the
        // fall animation reaches its hold frame before the landing, starting from frame 1
        let mut vy = (4 * damage + 40 - 7 * n - (((falling + strike_down) as i32) >> 3)).clamp(-100, 100);
        let (mut start, mut t);
        if spike {
            vy = -400; // straight into the floor: it lands the next frame (0x8002F328)
            start = 6;
        } else {
            let disc = ((vy * vy) as f32 + 12.0 * (target - y0)).abs().sqrt() as i32;
            t = (vy + disc) / 6;
            if t <= 0 {
                t = 1;
            }
            start = (hold - t + 1).max(1);
            let frame = old_frame as i32;
            if !lying && start != 1 {
                t = t - 1 + start + 1;
                start = 1;
            } else if n == 0 {
                t += 10;
                start = (hold - t + 1).max(1);
                t += 1;
            } else if same_clip {
                let d = frame - start;
                if d >= 10 {
                    t += 1;
                } else if d >= 0 || start - frame < 10 {
                    let s0 = (frame - 10).max(1);
                    t += start - s0;
                    start = s0;
                    t += 1;
                } else {
                    t += 1;
                }
            } else {
                t += 1;
            }
            if y0 + ((vy * t - 3 * t * t) as f32) < target {
                vy = ((target - y0) as i32 + 3 * t * t) / t;
            }
        }
        // drift away from the attacker for the whole flight (0x8002F074)
        let drift = (damage.abs() + 7 * (n + 1) + 10 + extra_drift).clamp(0, 100) as f32;
        f.frame = start.max(1) as u32;
        f.air = Some(Air { drift, y: y0, down: -vy as f32, target, spike, kind });
        f.juggle += 1;
        f.pushback = None;
    }

    /// Where the fighter's root is drawn and its attacks originate: position + current root-motion offset.
    pub fn root(&self) -> Vec3 {
        self.position + self.offset
    }

    /// The current move's END link (its list's END entry), without trying the other entries.
    fn end_link(&self, gd: &GameData) -> Option<usize> {
        let mv = self.current_move(gd);
        let cancels = &self.moves(gd, self.current.bank).cancels;
        cancels.get(mv.cancel_start..)?.iter().find(|c| c.kind == CancelKind::End).map(|c| c.dest)
    }

    /// Walk the current move's command list (EXE 0x8002DE24), queueing the first entry whose input,
    /// frame window and condition match. Returns the END link when the list reaches it.
    fn walk_cancels(&mut self, gd: &GameData) -> Option<usize> {
        let mv = self.current_move(gd);
        let (cancels, start) = (&self.moves(gd, self.current.bank).cancels, mv.cancel_start);
        let mut end_alias = None;
        let mut end_entry: Option<Cancel> = None;
        let mut hit: Option<(usize, u32, u32)> = None;
        // a matching entry with transition 0x2C skips straight to the END entry: nothing is queued
        let mut skip = false;
        let mut i = start;
        while i < cancels.len() && hit.is_none() {
            let c = &cancels[i];
            match c.kind {
                CancelKind::End => {
                    end_alias = Some(c.dest);
                    end_entry = Some(c.clone());
                    break;
                }
                _ if c.transition & 0x3F == TRANSITION_GETUP && self.getup_lock > 0 => {}
                _ if skip => {}
                CancelKind::Call => {
                    // shared sub-list in the EXE, until its RETURN
                    let shared = &gd.shared.shared_cancels.entries;
                    let mut j = c.dest;
                    while j < shared.len() && shared[j].kind != CancelKind::Return {
                        if let Some(h) = self.try_cancel(gd, &shared[j]) {
                            if shared[j].transition == TRANSITION_USE_END {
                                skip = true;
                            } else {
                                hit = Some((h.0, h.1, shared[j].transition));
                            }
                            break;
                        }
                        j += 1;
                    }
                }
                _ => {
                    if let Some(h) = self.try_cancel(gd, c) {
                        if c.transition == TRANSITION_USE_END {
                            skip = true;
                        } else {
                            hit = Some((h.0, h.1, c.transition));
                        }
                    }
                }
            }
            i += 1;
        }
        if let Some((alias, exec, transition)) = hit {
            if let Some(to) = self.resolve(alias) {
                let exec = if exec == 255 { 255 } else { exec };
                let t = normalize_transition(transition, self.moves(gd, to.bank).moves.get(to.index), exec);
                self.queued = Some((to, exec, t));
                self.queued_alias = alias;
                self.queued_buffered = false;
            }
        } else if let Some(e) = end_entry.filter(|_| self.frame_step > 0) {
            // END look-ahead (0x8002DB0C): from 5 frames before the END window until it closes, an attack of the
            // END destination's own list is buffered and takes over at the window's first frame
            let early = self.early_end.is_some_and(|f| self.frame >= f);
            if (self.frame + 5 >= e.win_start || early) && self.frame <= e.win_end && self.input.pressed != 0 {
                if let Some(dest) = self.resolve(e.dest) {
                    // the next list's back-turned conditions see the facing after this animation's net turn
                    let saved = (self.current, self.frame, self.yaw);
                    self.yaw += self.current_move(gd).next.hi as f32 * ANGLE;
                    (self.current, self.frame) = (dest, 1);
                    let found = self.first_cancel(gd);
                    (self.current, self.frame, self.yaw) = saved;
                    if let Some((alias, _, transition)) = found {
                        if let Some(to) = self.resolve(alias) {
                            self.queued = Some((to, e.win_start.max(self.frame), transition));
                            self.queued_alias = alias;
                            self.queued_buffered = true;
                        }
                    }
                }
            }
        }
        end_alias
    }

    /// Character gate (`req`, kumite_converter's reference move-properties.md §2.2): only the "this moveset" gates for
    /// now. The entry's `req` byte (0x8002DF00): own moveset is / is not, the opponent's moveset is, own character is /
    /// is not.
    pub(crate) fn passes_gate(&self, c: &Cancel) -> bool {
        let r = c.req as i64;
        let ms = self.moveset.moveset_id;
        match r {
            0 => true,
            0x01..=0x17 => ms == Some(r - 1),
            0x18..=0x2E => ms != Some(r - 0x18),
            0x2F..=0x45 => self.opponent.moveset_id == Some(r - 0x2F),
            0x46..=0x5C => self.character == Some(r - 0x46),
            _ => self.character != Some(r - 0x5D),
        }
    }

    /// The first matching command of the current move's list (the END look-ahead tries the next move's), not
    /// queueing anything.
    fn first_cancel(&self, gd: &GameData) -> Option<(usize, u32, u32)> {
        let mv = self.current_move(gd);
        let cancels = &self.moves(gd, self.current.bank).cancels;
        for c in cancels.get(mv.cancel_start..)? {
            match c.kind {
                CancelKind::End => return None,
                CancelKind::Call => {
                    let shared = &gd.shared.shared_cancels.entries;
                    for e in shared.get(c.dest..).into_iter().flatten().take_while(|e| e.kind != CancelKind::Return) {
                        if let Some((d, x)) = self.try_cancel(gd, e) {
                            return Some((d, x, e.transition));
                        }
                    }
                }
                _ => {
                    if let Some((d, x)) = self.try_cancel(gd, c) {
                        return Some((d, x, c.transition));
                    }
                }
            }
        }
        None
    }

    fn try_cancel(&self, gd: &GameData, c: &Cancel) -> Option<(usize, u32)> {
        if self.frame < c.win_start || self.frame > c.win_end {
            return None;
        }
        if !self.passes_gate(c) {
            return None;
        }
        if !self.condition(gd, c.cond, c.param) {
            return None;
        }
        let ok = match c.kind {
            CancelKind::Command => self.input.matches_command(c.cmd),
            CancelKind::Sequence => gd.shared.sequences.entries.iter().find(|s| s.cmd == c.cmd).is_some_and(|s| self.input.matches_sequence(s)),
            CancelKind::Dash => self.input.matches_dash(true),
            CancelKind::Backdash => self.input.matches_dash(false),
            _ => false,
        };
        ok.then_some((c.dest, c.exec))
    }

    /// Current posture word (actor +0x60): the move's posture, with the guard bits cleared when the stance
    /// needs the guard input and the flag is off (0x8002F70C); moves flagged "guard by input" (+0x24 bit 0x4000)
    /// take their guard from the stick (0x80040514).
    pub fn posture(&self, gd: &GameData) -> u32 {
        let mv = self.current_move(gd);
        let raw = mv.posture.raw & 0xFFFF;
        if mv.flags24.raw & 0x4000 != 0 && self.ki == 0 {
            if let Some(g) = self.guard_posture(gd) {
                return g;
            }
            // without the input the guard written last in this move stays (0x80040564)
            if let Some(g) = self.held_guard {
                return g;
            }
        }
        // recovering from a hit or block past its 0x1A point: back guards standing, down-back crouching
        if self.recover_guard != 0 && mv.flags24.raw & 0x4000 == 0 {
            match self.input.dir {
                4 => return posture::STANDING_GUARD,
                1 => return posture::CROUCHING_GUARD,
                _ => {}
            }
        }
        if self.ki > 0 || (raw & posture::GUARD_NEEDS_INPUT != 0 && !self.guard_input) {
            raw & !posture::GUARDS
        } else {
            raw
        }
    }

    /// A guard-by-input move's guard from the stick this frame: back stands guarding, down-back crouches.
    fn guard_posture(&self, gd: &GameData) -> Option<u32> {
        let mv = self.current_move(gd);
        if mv.flags24.raw & 0x4000 == 0 || self.ki != 0 {
            return None;
        }
        let d = self.input.dir;
        let (stand, crouch) = if self.guard_input { (d == 4 || d == 5, d == 1 || d == 2) } else { (d == 4, d == 1) };
        if stand {
            Some(posture::STANDING_GUARD)
        } else if crouch {
            Some(posture::CROUCHING_GUARD)
        } else {
            None
        }
    }

    /// Throw position code (0x8003115C, reactions.md §5.1): only after the own (throw) attack made contact
    /// (+0xD1), against a grounded or standing opponent that is not in the air, while facing it within 60°.
    /// Standing 2 front / 3 back / 4, 5 sides; crouching 6..9; lying face-up 0xA..0xD, face-down 0xE..0x11.
    pub fn throw_code(&self) -> Option<u32> {
        if !self.connected || self.opponent.airborne {
            return None;
        }
        if self.opponent.unthrowable {
            return None;
        }
        let to = self.opponent.root - self.root();
        let to = glam::Vec2::new(to.x, to.z).normalize_or_zero();
        let forward = glam::Vec2::new(self.yaw.dsin(), self.yaw.dcos());
        let p = self.opponent.posture;
        // the opponent's orientation towards me (+0x40): front, back, side 1, side 3
        let quadrant = direction_quadrant(self.opponent.yaw, self.opponent.root, self.root());
        if forward.dot(to) <= -0.5 {
            // back turned to a standing opponent (more than 120° off): grabs from behind, 0x12 facing / 0x13 back
            return (p & posture::STANDING != 0).then_some([Some(0x12), None, Some(0x13), None][quadrant]).flatten();
        }
        if forward.dot(to) < 0.5 {
            return None; // more than 60° off (+0x30 >= 0x2AAA)
        }
        let base = if p & 0x80 != 0 {
            if p & 0x200 != 0 {
                0xE
            } else {
                0xA
            }
        } else if p & 0x40 != 0 {
            2
        } else if p & 0x20 != 0 {
            6
        } else {
            return None;
        };
        Some(base + [0, 2, 1, 3][quadrant])
    }

    /// Airborne (+0xDB, 0x800402E0): juggled, or within the move's jump window (record +0x19..+0x1A).
    pub fn airborne_flag(&self, gd: &GameData) -> bool {
        let b = &self.current_move(gd).bytes_18;
        self.juggle != 0 || (b.jump_start != 0 && (b.jump_start..=b.jump_end).contains(&self.frame))
    }

    /// Cannot be thrown this frame (+0xDF, 0x800404C0): the move says so (flags24 bit 0x400), or an attack is
    /// on its last 3 start-up frames or its first active one.
    pub fn unthrowable(&self, gd: &GameData) -> bool {
        let m = self.current_move(gd);
        let a0 = m.active[0];
        m.flags24.raw & 0x400 != 0 || (m.damage != 0 && m.damage_type != 1 && a0 != 0 && (a0.saturating_sub(3).max(1)..=a0).contains(&self.frame))
    }

    /// The facing angles the actor keeps (0x80043A94), in game units, from where both fighters are now.
    pub fn angles(&self) -> Angles {
        // (the same angles as `condition` uses: OpenKumite's z axis is flipped, so they are taken the other way)
        let units = |r: f32| (r / ANGLE).round() as i32 as i16 as i32;
        let (a2a, _, _) = self.geometry();
        let off = units(wrap_pi(a2a - self.yaw));
        let o = &self.opponent;
        let d = self.root() - o.root;
        let opp_off = units(wrap_pi(d.x.datan2(d.z) - o.yaw));
        // +0x30 = |+0x2E| (one's complement for negatives), +0x32 = the quadrant of +0x2E + 45°
        let turn = |x: i32| if x < 0 { !x } else { x };
        let quadrant = |x: i32| ((x + 0x2000) & 0xFFFF) as u32 / 0x4000;
        Angles { off, turn: turn(off), quadrant: quadrant(off), opp_off, opp_turn: turn(opp_off), opp_quadrant: quadrant(opp_off) }
    }

    /// Command-list condition (kumite_converter's reference move-properties.md §3). Only the conditions OpenKumite can
    /// answer yet are evaluated; the others are treated as false, so their entries are skipped.
    pub(crate) fn condition(&self, gd: &GameData, cond: u32, param: u32) -> bool {
        let d = self.opponent.root - self.root();
        let distance = (d.x * d.x + d.z * d.z).sqrt(); // +0xF8
        let c6 = self.on_right; // +0xC6
                                // the body's turn off the opponent's direction (+0x2E, -180°..180°) and whether the
                                // back is turned (+0x30 over 90°) (+0x2E in the game's angle sense: OpenKumite's z axis
                                // is flipped, so the sign turns over)
        let (a2a, _, _) = self.geometry();
        let off = wrap_pi(a2a - self.yaw) / ANGLE;
        let back = off.abs() > 0x4000 as f32;
        let quadrant_of = |off: f32| ((off + 0x2000 as f32).rem_euclid(65536.0) / 0x4000 as f32) as u32;
        let quadrant = quadrant_of(off);
        let opp_quadrant = {
            let o = self.opponent;
            let d = self.root() - o.root;
            quadrant_of(wrap_pi(d.x.datan2(d.z) - o.yaw) / ANGLE)
        };
        let throw = self.throw_code();
        let o = &self.opponent;
        // the throw distance (+0xF4): the pair distance less 40 for each of the pair of bulk (the large characters;
        // 0x800452CC)
        let big = |c: Option<i64>| c.is_some_and(|c| gd.characters.throw_bulk.contains(&c));
        let throw_distance = distance - 40.0 * (u8::from(big(self.character)) + u8::from(big(o.character))) as f32;
        match cond {
            0 => true,
            0x18 => distance <= param as f32,
            0x19 => distance >= param as f32,
            // throw position pairs (standing / crouching of one side) within a distance
            0x14..=0x17 => throw.is_some_and(|c| c == cond - 0x12 || c == cond - 0xE) && throw_distance <= param as f32,
            // hit-confirm, blocked follow-ups
            0x1A => self.landed == Some(true),
            0x1B => self.landed == Some(false),
            // the own attack reached its last active frame without contact (+0xD2)
            0x1C => self.whiffed,
            0x22 => o.posture & 0x407 == 2,
            0x23 => o.posture & 0x407 == 1,
            0x2B => opp_quadrant == 0,
            0x2C => opp_quadrant == 1,
            0x2D => opp_quadrant == 3,
            0x2E => opp_quadrant == 2,
            // auto-duck under a high on its way
            0x30 => o.mask == 0x412 && o.frame <= o.active_end && distance < 0x700 as f32,
            // knocked down: the stun has drained (wake up) / not yet
            0x31 => self.stun == 0 && self.stun_timer == 0,
            0x32 => self.stun != 0 || self.stun_timer != 0,
            // opponent lying (ground attacks, pick-ups)
            0x35 => o.posture & posture::LYING != 0,
            0x36 => o.posture & posture::LYING == 0,
            0x37 => o.posture & posture::LYING != 0 && o.jump_start == 0 && distance <= param as f32,
            // my last hit was a counter hit and the opponent is on the ground
            0x39 => self.landed_counter && !o.airborne,
            // one air recovery per airtime (+0x8E)
            0x42 => !self.air_recovered && self.health > 0,
            0x43 => self.ki != 0,
            0x44 => self.ki == 0,
            // back-turned states (0x8002E41C..0x8002E6A8, facing_re §4)
            0x24 => back,
            0x25 => (0x4E38 as f32..0x8000 as f32).contains(&off),
            0x26 => (-0x8000 as f32..=(0xB1C6 - 0x10000) as f32).contains(&off),
            0x27 => quadrant == 0,
            0x28 => quadrant == 1,
            0x29 => quadrant == 3,
            0x2A => quadrant == 2,
            0x40 => !c6 && back,
            0x41 => c6 && back,
            // throw position codes (0x8003115C) against the distance +0xF4
            2..=0x13 => self.throw_code() == Some(cond) && throw_distance <= param as f32,
            // throw escapes: the first new press while being thrown (0x80040AD0)
            0x3A => self.escape == Some(1),
            0x3B => self.escape == Some(2),
            0x3C => self.escape == Some(3),
            0x3D => self.health <= 0, // KO'd: reaction lists branch to the collapse
            0x3E => !c6,
            0x3F => c6,
            _ => false,
        }
    }

    /// The reversal / parry entry of the current move's list that catches `opp`'s attack (0x8002D8B4,
    /// walker 0x8002E708): only conditions 0x45.. are tried here, in a window around this frame, and every one
    /// needs both fighters facing each other within 60°.
    fn reversal_entry(&self, gd: &GameData, opp: &Fighter) -> Option<Cancel> {
        let facing = |f: &Fighter, other: &Fighter| {
            let d = other.root() - f.root();
            d.x * d.x + d.z * d.z > 1.0 && wrap_pi(d.x.datan2(d.z) - f.yaw).abs() < 0x2AAA as f32 * ANGLE
        };
        if !facing(self, opp) || !facing(opp, self) {
            return None;
        }
        let om = opp.current_move(gd);
        let mask = om.attack_mask.mask;
        let catches = |c: &Cancel| -> bool {
            let limb = c.param == 0 || om.attack.as_ref().is_some_and(|a| a.header_bytes.iter().take(4).any(|&b| b == c.param));
            let reversible = om.flags24.raw & 0x800 == 0 && limb;
            match c.cond {
                0x45 => true,
                0x46 => reversible && mask == 0x412,
                0x47 => reversible && mask == 0x217,
                0x48 => reversible && mask == 0x10F,
                0x49 => reversible && (mask == 0x412 || mask == 0x217),
                // the attacker plays the move of alias `param`
                // the alias the opponent entered its move by (+0xA0)
                0x4A => opp.alias == c.param as usize,
                // its motion is of the family `param`
                0x4B => gd.shared.reversal_families.families.get(c.param as usize).is_some_and(|f| f.contains(&om.motion_tag)),
                _ => false,
            }
        };
        let ok =
            |c: &Cancel| c.cond >= 0x45 && (c.win_start..=c.win_end).contains(&self.frame) && self.passes_gate(c) && c.kind == CancelKind::Command && self.input.matches_command(c.cmd) && catches(c);
        let mv = self.current_move(gd);
        let cancels = &self.moves(gd, self.current.bank).cancels;
        for c in cancels.get(mv.cancel_start..).into_iter().flatten() {
            match c.kind {
                CancelKind::End => return None,
                CancelKind::Call => {
                    let shared = &gd.shared.shared_cancels.entries;
                    if let Some(e) = shared.get(c.dest..).into_iter().flatten().take_while(|e| e.kind != CancelKind::Return).find(|e| ok(e)) {
                        return Some(e.clone());
                    }
                }
                _ if ok(c) => return Some(c.clone()),
                _ => {}
            }
        }
        None
    }

    /// Advance one frame of move logic.
    fn step(&mut self, gd: &GameData, pad: Pad, facing_right: bool, anims: &dyn Animations) {
        self.started_with = None;
        self.landing = 0;
        self.deal_damage = None;
        self.self_damage = 0;
        self.effects.clear();
        self.input.update(pad, facing_right);
        self.on_right = !facing_right;
        self.commits_root = self.current_move(gd).flags24.raw & 0x1000_0000 != 0;
        // hit-stop: the move is frozen (input is still read, so presses are not lost); a juggle flight goes on,
        // except against the last boss's move set (0x8003F248)
        if self.freeze > 0 {
            self.freeze -= 1;
            if !self.opponent.moveset_id.is_some_and(|m| gd.characters.hitstop_still.contains(&m)) {
                self.fly();
            }
            return;
        }
        // knockdown stun (0x800403C0): the reaction timer runs down; in its last 30 frames the pool drains, faster
        // while buttons are pressed (2: 4 a frame, any other: 2)
        self.stun_timer = self.stun_timer.saturating_sub(1);
        if self.stun_timer < 31 && self.stun > 0 {
            let p = self.input.pressed;
            self.stun = self.stun.saturating_sub(
                1 + if p & 2 != 0 {
                    3
                } else if p != 0 {
                    1
                } else {
                    0
                },
            );
        }
        self.ki = self.ki.saturating_sub(1);
        self.invulnerable = self.invulnerable.saturating_sub(1);
        if let Some(g) = self.guard_posture(gd) {
            self.held_guard = Some(g);
        }
        // back on the ground: an air recovery may be taken again (0x8003F664)
        if self.air.is_none() && !self.airborne_flag(gd) {
            self.air_recovered = false;
        }
        // the side-hit chain's time runs out (none while airborne, 0x800407E0)
        self.side_timer = if self.airborne_flag(gd) { 0 } else { self.side_timer.saturating_sub(1) };
        if self.side_timer == 0 {
            self.side_chain = 0;
        }
        // throw escape: the first new press while being thrown is latched (0x80040AD0)
        // (only 1 and 2 count: both new on the same frame, else 1, else 2; a kick or a direction does not lock the
        // escape out)
        if self.thrown && self.escape.is_none() {
            let p = self.input.pressed;
            self.escape = if p & 3 == 3 {
                Some(3)
            } else if p & 1 != 0 {
                Some(1)
            } else if p & 2 != 0 {
                Some(2)
            } else {
                None
            };
        }
        // the walk only runs while nothing is queued (0x8002DA14); the END link still applies at the end
        let end_alias = if self.queued.is_none() { self.walk_cancels(gd) } else { self.end_link(gd) };
        let mv = self.current_move(gd);
        let (mv_frames, next_alias, hold) = (mv.frames.max(1), mv.next.alias, mv.bytes_18.hold_frame);
        // a queued move takes over at its exec frame (255 = when the animation ends)
        self.getup_lock = self.getup_lock.saturating_sub(1);
        self.since += 1;
        if let Some((to, exec, transition)) = self.queued {
            let reached = if self.frame_step > 0 { exec != 255 && self.frame >= exec } else { self.frame as i64 <= exec as i64 };
            if reached {
                self.commit(gd, to, self.queued_alias, transition, false, anims);
                return;
            }
        }
        // a juggled fighter's fall animation holds its hold frame until it lands
        let holding = self.air.is_some() && hold > 0 && self.frame >= hold;
        if !holding {
            self.frame = (self.frame as i32 + self.frame_step).max(0) as u32;
        }
        if self.frame > mv_frames || self.frame == 0 {
            let queued = self.queued;
            match queued {
                Some((to, _, transition)) => self.commit(gd, to, self.queued_alias, transition, false, anims),
                None => match self.resolve(end_alias.unwrap_or(next_alias)) {
                    Some(to) => {
                        let t = end_transition(self.moves(gd, self.current.bank), self.current_move(gd));
                        self.commit(gd, to, end_alias.unwrap_or(next_alias), t, true, anims);
                    }
                    None => self.frame = 1,
                },
            }
        }
        // timed property 0x0B: damage the opponent (throws, at the frame the animation slams): its argument, or
        // the move's own damage when the argument is 0 (move-properties.md §1.3)
        let frame = self.frame;
        let mv = self.current_move(gd);
        let damage = self.damage_override.unwrap_or(mv.damage);
        let deal = mv.properties.iter().flatten().find(|p| p.op == 0x0B && p.frame == frame).map(|p| if p.arg != 0 { p.arg as i32 } else { damage });
        // timed ops 0x0C / 0x0D: the move damages itself (arg 0: its own damage) or heals
        let own: i32 = mv
            .properties
            .iter()
            .flatten()
            .filter(|p| p.frame == frame)
            .map(|p| match p.op {
                0x0C => {
                    if p.arg != 0 {
                        p.arg as i32
                    } else {
                        damage
                    }
                }
                0x0D => -(p.arg as i32),
                _ => 0,
            })
            .sum();
        // timed effects of this frame (shakes, ground impacts, effects at bones)
        let effects: Vec<MoveEffect> = mv
            .properties
            .iter()
            .flatten()
            .filter(|p| p.frame == frame)
            .filter_map(|p| match p.op {
                1..=3 => Some(MoveEffect::Shake(p.op as u8 - 1)),
                4 => Some(MoveEffect::GroundImpact),
                6..=0x0A => Some(MoveEffect::AtBone { op: p.op, slot: p.arg }),
                _ => None,
            })
            .collect();
        self.deal_damage = deal;
        self.self_damage = own;
        self.effects = effects;
        // pushback from a hit or block, along its direction
        if let Some(p) = &mut self.pushback {
            p.frame += 1;
            let c = &p.curve;
            let mut step = if p.frame <= c.constant_frames { c.velocity } else { 0 };
            if (1..=8).contains(&p.frame) {
                step += c.impulses[p.frame as usize - 1];
                if self.side_chain > 0 {
                    step += 40 * self.side_chain as i32 + 10;
                }
            }
            self.position += Vec3::new(p.direction.x, 0.0, p.direction.y) * step as f32;
            if p.frame >= c.constant_frames.max(8) {
                self.pushback = None;
            }
        }
        // root motion (0x8003AAF4): the clip's root as stored, turned by the root basis, on top of the origin; a
        // root pinned after a landing adds only its height
        let (anim, f) = self.animation(gd);
        let partner = self.current.bank == Bank::Partner;
        if let Some(r) = anims.root(anim, f, partner) {
            let (s, c) = self.root_yaw.dsin_cos();
            self.offset = if self.pin == 0 { Vec3::new(r.x * c + r.z * s, 0.0, -r.x * s + r.z * c) } else { Vec3::ZERO };
            self.anim_root_y = r.y;
        }
        self.fly();
        if self.air.is_none() {
            self.drift(gd);
        }
        let height = self.position.y + self.anim_root_y;
        self.root_fall = self.last_height - height;
        self.last_height = height;
        if self.air.is_none() && self.current_move(gd).posture.raw & posture::AIRBORNE == 0 {
            self.juggle = 0; // landed: the juggle counter starts again
        }
        self.hit_flash = self.hit_flash.saturating_sub(1);
    }

    /// Juggle flight (0x8003F238): the physics height replaces the animation's root height until landing, with
    /// the drift away from the attacker.
    fn fly(&mut self) {
        let root = self.root();
        if let Some(air) = &mut self.air {
            air.down += 2.0 * AIR_GRAVITY;
            air.y -= air.down;
            let away = root - self.opponent.root;
            let away = glam::Vec2::new(away.x, away.z).normalize_or_zero() * air.drift;
            self.position += Vec3::new(away.x, 0.0, away.y);
            let y = air.y;
            // it lands only while falling, at or below the landing height
            let (spike, kind) = (air.spike, air.kind);
            if air.down > 0.0 && y <= air.target {
                // landing (0x8003F3A4): the origin snaps to the root, which stays pinned there until the next move
                self.air = None;
                self.position = Vec3::new(root.x, 0.0, root.z);
                self.offset = Vec3::ZERO;
                self.pin = 2;
                self.landing = if kind == 5 { 2 } else { 1 };
                // every juggle landing hits the ground and shakes the screen, a spike harder (0x8003F350)
                self.effects.push(MoveEffect::LandingImpact);
                self.effects.push(MoveEffect::Shake(0));
                if spike {
                    self.effects.push(MoveEffect::Shake(1));
                }
            } else {
                self.position.y = y - self.anim_root_y;
            }
        }
    }

    /// World-space attack segments for this frame. Attack paths are in the game's actor space, which is the pose space
    /// turned 180° (kumite_converter's reference attack-paths.md §9); with the original hardware → glTF axis change
    /// that makes glTF = (−x, −y, +z). Then the fighter's facing and position.
    pub fn attack_segments(&self, gd: &GameData) -> Vec<(Vec3, Vec3)> {
        let mv = self.current_move(gd);
        let Some(a) = &mv.attack else { return vec![] };
        if self.connected || self.frame < mv.active[0] || self.frame > mv.active[1] {
            return vec![];
        }
        let turn = |v: Vec3, yaw: f32| {
            let (s, c) = yaw.dsin_cos();
            Vec3::new(v.x * c + v.z * s, v.y, -v.x * s + v.z * c)
        };
        let root = self.root();
        let to_world = |p: [i32; 3]| {
            let v = Vec3::new(-p[0] as f32, -p[1] as f32, p[2] as f32);
            // relative to the actor position (0x8006A230) in the root basis (the move's root lean is already
            // baked into the path), then turned about the root by the body's facing off that basis (0x8006A428)
            let p = self.position + turn(v, self.root_yaw);
            root + turn(p - root, self.yaw - self.root_yaw)
        };
        a.frames.iter().filter(|f| f.frame == self.frame).flat_map(|f| f.segments.iter().map(|s| (to_world(s[0]), to_world(s[1])))).collect()
    }
}

/// Segment against a hurt sphere, the game's way (0x8004770C): a height-band clip, then a horizontal
/// distance test, so each sphere acts like a short vertical cylinder.
fn segment_hits(a: Vec3, b: Vec3, s: &Sphere) -> bool {
    let (lo, hi) = (s.centre.y - s.radius, s.centre.y + s.radius);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    let dy = b.y - a.y;
    if dy.abs() < 1e-6 {
        if a.y < lo || a.y > hi {
            return false;
        }
    } else {
        let (ta, tb) = ((lo - a.y) / dy, (hi - a.y) / dy);
        t0 = t0.max(ta.min(tb));
        t1 = t1.min(ta.max(tb));
        if t0 > t1 {
            return false;
        }
    }
    // closest horizontal distance on the clipped part
    let p = |t: f32| glam::Vec2::new(a.x + (b.x - a.x) * t, a.z + (b.z - a.z) * t);
    let (p0, p1, c) = (p(t0), p(t1), glam::Vec2::new(s.centre.x, s.centre.z));
    let d = p1 - p0;
    let t = if d.length_squared() > 0.0 { ((c - p0).dot(d) / d.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
    (p0 + d * t).distance(c) <= s.radius
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundState {
    Fighting,
    Ko { winner: usize },
    TimeUp { winner: Option<usize> },
}

/// A hit that landed (or was blocked) this frame.
#[derive(Clone, Debug)]
pub struct HitEvent {
    pub attacker: usize,
    pub anim: String,
    pub zone: usize,
    pub damage: i32,
    /// The move's own damage before zone, counter and juggle scaling (practice mode's DMG percent).
    pub base_damage: i32,
    /// The move's attack mask (0x412 high, 0x217 / 0x31F / 0x51F mid, 0x10F low, 0x607 / 0x706 special).
    pub mask: u32,
    pub blocked: bool,
    /// It hit during the victim's own attack start-up (counter hit).
    pub counter: bool,
    /// A close-range sweet spot (+0xD4, practice's CLEAN HIT): 1.5x damage, its own reaction.
    pub clean: bool,
    /// The strike's travel this frame (game units, world): its direction throws the hit spark.
    pub strike: Vec3,
    /// The attack's first limb byte (descriptor byte 0: 0x18.. are fire / projectile attacks).
    pub limb: u32,
    /// The attacker was ki-charged (its heavy-hit sparks are blue).
    pub ki: bool,
    /// Where it struck (game units, world; the closest point of the strike to the struck sphere).
    pub point: Vec3,
    pub victim: usize,
    /// Victim's health afterwards (whole points).
    pub health: i32,
    /// A throw's (or hold's) damage (timed property 0x0B, +0xCC), not a strike.
    pub throw: bool,
}

/// A one-round versus fight between two fighters.
#[derive(Clone)]
pub struct Fight {
    pub data: Arc<GameData>,
    pub fighters: [Fighter; 2],
    pub clock: u32,
    pub state: RoundState,
    pub frame: u64,
    /// World direction that points right on screen (from the camera): decides which way is forward on the
    /// pad for each player.
    pub screen_right: Vec3,
    /// Hits that landed during the last step.
    pub hits: Vec<HitEvent>,
    /// The moves a throw / hold linked (per fighter); the pair stays locked until both have left them.
    pub link: Option<[Current; 2]>,
    /// OPTION "GUARD DAMAGE" (0x800AFA58): blocked hits chip.
    pub guard_damage: bool,
    /// The K.O. collapse (ctx+0xB, 0x800AFA93, set by Arcade / VS / Team / Time Attack / Survival): a clean hit's
    /// K.O. plays alias 0xD66 at once; without it the K.O. falls through the air table.
    pub ko_collapse: bool,
    /// Ball: the ball, and with it the Ball rules (no damage between the fighters, the court).
    pub ball: Option<ball::Ball>,
    /// Force for more than two (OpenKumite's own): actors 4.. (4, 5 the players 3P and 4P, 6.. soldiers and
    /// bosses), in the co-op melee ([`Fight::widen_coop`]).
    pub more: Vec<Fighter>,
    /// The Ball rules without the ball (the 2 ON 2 melee's pair fights: no damage between the fighters).
    pub ball_rules: bool,
    /// Force: the third actor, the soldier the player is not facing (it targets the player too).
    pub extra: Option<Box<Fighter>>,
    /// Which deaths end the round (Force: only the player's while soldiers come, 0x8003E370).
    pub ko_rule: KoRule,
    /// Force's co-op (OpenKumite's own): the second player (actor 3), on the player's side.
    pub partner: Option<Box<Fighter>>,
    /// With a partner, the four actors (0 the player, 1 the target, 2 the extra, 3 the partner) fight as a melee:
    /// everyone takes on the nearest foe. Its actors are placeholders between steps.
    pub coop: Option<Box<melee::Melee>>,
    /// An arena with walls (OpenKumite's extra levels): the fighters stay on its free cells.
    pub walls: Option<Arc<Walls>>,
    /// Where the player turns while no one is there to fight (fighter 1 and the extra out of play): Force's
    /// way on through the stage.
    pub idle_target: Option<Vec3>,
}

/// An arena's walk grid (metres, x along a row, rows along z): cells a fighter's root cannot enter.
#[derive(Clone, Debug)]
pub struct Walls {
    pub origin: [f32; 2],
    pub cell: f32,
    pub size: [usize; 2],
    /// Row-major, one bit a cell (least significant first), set = blocked.
    pub rows: Vec<Vec<u8>>,
}

impl Walls {
    /// Whether a point (game units) is in a blocked cell (off the grid counts as blocked).
    pub fn blocked(&self, p: Vec3) -> bool {
        let x = ((p.x / 1000.0 - self.origin[0]) / self.cell).floor();
        let z = ((p.z / 1000.0 - self.origin[1]) / self.cell).floor();
        if x < 0.0 || z < 0.0 || x >= self.size[0] as f32 || z >= self.size[1] as f32 {
            return true;
        }
        let (x, z) = (x as usize, z as usize);
        self.rows.get(z).and_then(|r| r.get(x / 8)).is_none_or(|b| b >> (x % 8) & 1 != 0)
    }
}

/// Who can be K.O.'d to end the round.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KoRule {
    #[default]
    Both,
    /// Only fighter 0 (Force's soldier phase: soldiers dying never ends it).
    PlayerOnly,
}

/// What a fighter sees of another (the snapshot `Fighter::opponent` holds).
fn sight_of(o: &Fighter, gd: &GameData) -> Opponent {
    let posture = o.posture(gd);
    let airborne = posture & posture::AIRBORNE != 0 || o.air.is_some() || o.airborne_flag(gd);
    let m = o.current_move(gd);
    Opponent {
        position: o.position,
        side: o.side,
        posture,
        yaw: o.yaw,
        root: o.root(),
        evasive: o.evasive,
        mask: m.attack_mask.mask,
        frame: o.frame,
        active_end: m.active[1],
        jump_start: m.bytes_18.jump_start,
        unthrowable: o.unthrowable(gd),
        airborne,
        moveset_id: o.moveset.moveset_id,
        character: o.character,
    }
}

impl Fight {
    pub fn new(data: Arc<GameData>, p1: Arc<Moveset>, p2: Arc<Moveset>) -> Self {
        let h = data.max_health;
        let fighters = [
            // the start marks (0x8002BDB4): ∓1000 units
            Fighter::new(p1, h, Vec3::new(-1000.0, 0.0, 0.0), std::f32::consts::FRAC_PI_2, -100),
            Fighter::new(p2, h, Vec3::new(1000.0, 0.0, 0.0), -std::f32::consts::FRAC_PI_2, 100),
        ];
        Self {
            data,
            fighters,
            clock: ROUND_FRAMES,
            state: RoundState::Fighting,
            frame: 0,
            screen_right: Vec3::X,
            hits: Vec::new(),
            link: None,
            guard_damage: false,
            ko_collapse: true,
            ball: None,
            more: Vec::new(),
            ball_rules: false,
            extra: None,
            ko_rule: KoRule::Both,
            partner: None,
            coop: None,
            walls: None,
            idle_target: None,
        }
    }

    /// Whether fighter `i` has the opponent to its right on the pad (forward = right).
    pub fn facing_right(&self, i: usize) -> bool {
        let p1_left = self.p1_left();
        (i == 0) == p1_left
    }

    /// Whether P1 counts as the left one for the pads (0x8002C580's side, +0xC6 = own side >= the opponent's,
    /// 0x80046234): the sides are where the fighters' pelvises fell on the screen last frame (+0x1278), so forward
    /// is toward the opponent on the screen, after a throw over the shoulder too; Force (the extra, the co-op
    /// melee, the idle target) goes by where they stand against the screen's right.
    pub fn p1_left(&self) -> bool {
        let (a, b) = (&self.fighters[0], &self.fighters[1]);
        if self.extra.is_none() && self.coop.is_none() && self.idle_target.is_none() && a.side != b.side {
            return a.side < b.side;
        }
        (self.idle_point().unwrap_or_else(|| b.root()) - a.root()).dot(self.screen_right) >= 0.0
    }

    /// The idle target while no one is there to fight ([`Fight::idle_target`]).
    pub fn idle_point(&self) -> Option<Vec3> {
        self.idle_target.filter(|_| self.fighters[1].inactive && self.extra.as_ref().is_none_or(|e| e.inactive))
    }

    /// One 60 Hz step. `bodies[i]` = fighter i's collision volumes for the pose shown last frame.
    pub fn step(&mut self, pads: [Pad; 2], bodies: &[BodyVolumes; 2], anims: [&dyn Animations; 2]) {
        self.step_with(pads, bodies, anims, None);
    }

    /// Actor `i`: 0, 1 the fighters, 2 Force's extra, 3 the co-op partner.
    pub fn actor(&self, i: usize) -> Option<&Fighter> {
        match i {
            0 | 1 => Some(&self.fighters[i]),
            2 => self.extra.as_deref(),
            3 => self.partner.as_deref(),
            _ => self.more.get(i - 4),
        }
    }

    pub fn actor_mut(&mut self, i: usize) -> Option<&mut Fighter> {
        match i {
            0 | 1 => Some(&mut self.fighters[i]),
            2 => self.extra.as_deref_mut(),
            3 => self.partner.as_deref_mut(),
            _ => self.more.get_mut(i - 4),
        }
    }

    /// How many actors there are places for (the fighters, the extra, the partner, the wider melee's).
    pub fn actor_count(&self) -> usize {
        4 + self.more.len()
    }

    /// The Ball rules hold (a ball in play, or the 2 ON 2 melee's pair fights).
    fn ball_court(&self) -> bool {
        self.ball.is_some() || self.ball_rules
    }

    /// Ball 2 ON 2: the four on the court (the players' side: actors 0 and 3; the other: 1 and 2).
    pub fn ball_doubles(&self) -> bool {
        self.ball_court() && self.coop.is_some() && self.partner.is_some() && self.extra.is_some()
    }

    /// Whom actor `i` fights (as [`Fight::actor`] indices): the co-op melee's pick, else the pair's other, and the
    /// player for the extra.
    pub fn opponent_index(&self, i: usize) -> usize {
        match &self.coop {
            Some(m) if self.partner.is_some() => m.facing(i),
            _ => match i {
                0 => 1,
                _ => 0,
            },
        }
    }

    /// Whether actor `i` has its opponent to its right on the pad.
    pub fn actor_facing_right(&self, i: usize) -> bool {
        let (Some(me), Some(op)) = (self.actor(i), self.actor(self.opponent_index(i))) else { return true };
        if self.coop.is_none() && i < 2 {
            return self.facing_right(i);
        }
        // (no foe in play: the way on, as the co-op melee turns it)
        let to = self.idle_target.filter(|_| op.inactive).unwrap_or_else(|| op.root());
        (to - me.root()).dot(self.screen_right) >= 0.0
    }

    /// Force's co-op: the partner joins (at `partner`'s place); from now on the four fight as a melee.
    pub fn join_partner(&mut self, partner: Fighter) {
        let Some(extra) = self.extra.as_deref() else { return };
        let actors = vec![self.fighters[0].clone(), self.fighters[1].clone(), extra.clone(), partner.clone()];
        self.coop = Some(Box::new(melee::Melee::with_actors(self.data.clone(), actors, vec![0, 1, 1, 0])));
        self.partner = Some(Box::new(partner));
    }

    /// Force's soldiers traded places (fighter 1 and the extra): the co-op melee's records follow.
    pub fn swapped_target(&mut self) {
        if let Some(m) = self.coop.as_mut() {
            m.swap_actors(1, 2);
        }
    }

    /// One co-op step (Force with a partner): the four actors through the melee (targets, throws, the
    /// push, hits), then the round's end: the players lose when both are down; the round is won when fighter 1
    /// falls under [`KoRule::Both`].
    pub fn step_coop(&mut self, pads: &[Pad], bodies: &[&BodyVolumes], anims: &[&dyn Animations]) {
        let (Some(mut m), Some(mut partner), Some(mut extra)) = (self.coop.take(), self.partner.take(), self.extra.take()) else { return };
        self.frame += 1;
        self.hits.clear();
        m.fight.frame = self.frame - 1;
        (m.fight.screen_right, m.fight.idle_target) = (self.screen_right, self.idle_target);
        (m.fight.guard_damage, m.fight.ko_collapse) = (self.guard_damage, self.ko_collapse);
        m.state = melee::MeleeState::Fighting;
        m.fight.ball_rules = self.ball.is_some();
        let n = m.actors.len();
        let fighting = self.state == RoundState::Fighting;
        let pads: Vec<Pad> = (0..n).map(|k| if fighting { pads.get(k).copied().unwrap_or_default() } else { Pad::default() }).collect();
        // the actors into the melee's places (they are placeholders there between steps), and back after
        let swap = |fighters: &mut [Fighter; 2], extra: &mut Fighter, partner: &mut Fighter, more: &mut [Fighter], m: &mut melee::Melee| {
            let [f0, f1] = fighters;
            let slots = [f0, f1, extra, partner].into_iter().chain(more.iter_mut());
            for (k, f) in slots.enumerate().take(m.actors.len()) {
                std::mem::swap(f, &mut m.actors[k]);
            }
        };
        swap(&mut self.fighters, &mut extra, &mut partner, &mut self.more, &mut m);
        let none = BodyVolumes { hurt: vec![], push: vec![] };
        let vols: Vec<&BodyVolumes> = (0..n).map(|k| bodies.get(k).copied().unwrap_or(&none)).collect();
        let fallback = anims[0];
        let anims: Vec<&dyn Animations> = (0..n).map(|k| anims.get(k).copied().unwrap_or(fallback)).collect();
        m.step(&pads, &vols, &anims);
        swap(&mut self.fighters, &mut extra, &mut partner, &mut self.more, &mut m);
        self.hits = std::mem::take(&mut m.hits);
        let linked = !m.links.is_empty();
        (self.coop, self.partner, self.extra) = (Some(m), Some(partner), Some(extra));
        // Ball 2 ON 2: the court for the four, then the ball
        if self.ball.is_some() {
            self.ball_fighters();
            let gd = self.data.clone();
            self.ball_step(&gd, bodies);
        }
        if self.state == RoundState::Fighting && !linked {
            self.clock = self.clock.saturating_sub(1);
            // the players (0, the partner, Force's 3P and 4P) are down when all of them are
            let players: Vec<&Fighter> = self.player_actors().into_iter().filter_map(|i| self.actor(i)).collect();
            let players_down = players.iter().all(|p| p.health <= 0);
            // (Ball 2 ON 2: the other side is down when both of it are)
            let others_down = self.fighters[1].health <= 0 && (self.ball.is_none() || self.extra.as_ref().is_none_or(|e| e.health <= 0));
            if players_down {
                self.state = RoundState::Ko { winner: 1 };
            } else if self.ko_rule == KoRule::Both && others_down {
                self.state = RoundState::Ko { winner: 0 };
            } else if self.clock == 0 {
                let (a, b) = (players.iter().map(|p| p.health).max().unwrap_or(0), self.fighters[1].health);
                self.state = RoundState::TimeUp { winner: if a == b { None } else { Some(if a > b { 0 } else { 1 }) } };
            }
        }
    }

    /// The players' actors: 0, the co-op partner (3), and Force's 3P and 4P (4, 5) when the melee is that
    /// wide. (A place nobody plays holds an inactive fighter at no health.)
    pub fn player_actors(&self) -> Vec<usize> {
        let mut v = vec![0];
        if self.partner.is_some() {
            v.push(3);
        }
        v.extend((4..6).filter(|&i| i < 4 + self.more.len()));
        v
    }

    /// Actor `i` is on the players' side (0, 3, 4, 5).
    pub fn is_player(i: usize) -> bool {
        matches!(i, 0 | 3 | 4 | 5)
    }

    /// Force for more than two (OpenKumite's own): the co-op melee widened to `n` actors (at most 12: 4 and
    /// 5 the players 3P and 4P, 6.. more soldiers and bosses), the new places held by inactive fighters until
    /// filled. Starts the co-op melee (with an empty partner's place) when there is none.
    pub fn widen_coop(&mut self, n: usize) {
        let placeholder = |f: &Fighter| {
            let mut p = f.clone();
            (p.inactive, p.health) = (true, 0);
            p
        };
        if self.extra.is_none() {
            self.extra = Some(Box::new(placeholder(&self.fighters[1])));
        }
        if self.coop.is_none() || self.partner.is_none() {
            let p = placeholder(&self.fighters[0]);
            self.join_partner(p);
        }
        while 4 + self.more.len() < n.min(12) {
            let i = 4 + self.more.len();
            let f = placeholder(&self.fighters[usize::from(i >= 6)]);
            if let Some(m) = self.coop.as_mut() {
                m.push_actor(f.clone(), usize::from(!Self::is_player(i)));
            }
            self.more.push(f);
        }
    }

    /// Swap the extra actor (Force's other soldier) in as fighter 1 (and back).
    fn swap_extra(&mut self) {
        if let Some(e) = self.extra.as_mut() {
            std::mem::swap(&mut self.fighters[1], e);
        }
    }

    /// One step with Force's extra actor: its pad, volumes and animations.
    pub fn step_with(&mut self, pads: [Pad; 2], bodies: &[BodyVolumes; 2], anims: [&dyn Animations; 2], extra: Option<(Pad, &BodyVolumes, &dyn Animations)>) {
        self.frame += 1;
        self.hits.clear();
        let roots_before = self.fighters.each_ref().map(Fighter::root);
        let gd = self.data.clone();
        // no one to fight: the player faces the idle target
        let idle = self.idle_point();
        // ... and, standing in its stance, turns to face it (at most 22.5 degrees a frame)
        if let Some(t) = idle {
            let f = &mut self.fighters[0];
            let d = t - f.root();
            let off = wrap_pi(d.x.datan2(d.z) - f.yaw);
            if f.alias == ALIAS_NEUTRAL && !f.inactive && off.abs() > 0.01 {
                f.yaw += off.clamp(-PI / 8.0, PI / 8.0);
                f.root_yaw = f.yaw;
            }
        }
        let p1_left = self.p1_left();
        for i in 0..2 {
            self.fighters[i].opponent = sight_of(&self.fighters[1 - i], &gd);
            if let (Some(t), 0) = (idle, i) {
                let o = &mut self.fighters[0].opponent;
                (o.root, o.position, o.evasive) = (t, t, 0);
            }
            // a character the data marks cannot be thrown but by its own kind (0x800404E8)
            let o = self.fighters[1 - i].character;
            if o.is_some_and(|c| gd.characters.unthrowable.contains(&c)) && self.fighters[i].character != o {
                self.fighters[i].opponent.unthrowable = true;
            }
        }
        if let Some(e) = self.extra.as_mut() {
            e.opponent = sight_of(&self.fighters[0], &gd);
        }
        // the moves a set lacks come from the opponent's set (0x8002D07C): keep each fighter's partner set on its
        // opponent, except while it plays a move from another one
        for i in 0..2 {
            let other = self.fighters[1 - i].moveset.clone();
            let f = &mut self.fighters[i];
            if f.current.bank != Bank::Partner {
                f.partner = Some(other);
            }
        }
        if let Some(e) = self.extra.as_mut() {
            if e.current.bank != Bank::Partner {
                e.partner = Some(self.fighters[0].moveset.clone());
            }
        }
        let frozen = self.fighters.each_ref().map(|f| f.freeze > 0);
        let ball = self.ball_court();
        for f in &mut self.fighters {
            f.early_end = if ball { gd.ball.end(f.alias) } else { None };
        }
        for i in 0..2 {
            if self.fighters[i].inactive {
                continue;
            }
            let facing_right = (i == 0) == p1_left;
            // after a K.O. or time up the winner keeps its pad (it can still hit the falling loser); the loser's and a
            // draw's are dropped
            let controls = match self.state {
                RoundState::Fighting => true,
                RoundState::Ko { winner } => winner == i,
                RoundState::TimeUp { winner } => winner == Some(i),
            };
            let pad = if controls { pads[i] } else { Pad::default() };
            self.fighters[i].step(&gd, pad, facing_right, anims[i]);
        }
        let screen_right = self.screen_right;
        let player_root = self.fighters[0].root();
        if let (Some(e), Some((pad, _, anim))) = (self.extra.as_mut(), extra) {
            if !e.inactive {
                // like fighter 1: facing right when the player is to its right
                let facing_right = (e.root() - player_root).dot(screen_right) < 0.0;
                let pad = if self.state == RoundState::Fighting { pad } else { Pad::default() };
                e.step(&gd, pad, facing_right, anim);
            }
        }
        // a soldier that starts a throw on the player becomes the pair
        if self.extra.as_ref().is_some_and(|e| !e.inactive && e.started_with == Some(TRANSITION_LINK)) && self.link.is_none() {
            self.swap_extra();
        }
        // throws and holds: a move started with the link transition pairs the fighters. Two throws connecting on
        // the same frame: the frame counter's parity picks the winner, P1 on even frames (0x800455A0)
        let mut linking: Vec<usize> = (0..2).filter(|&i| self.fighters[i].started_with == Some(TRANSITION_LINK)).collect();
        if linking.len() == 2 && !self.fighters.iter().any(|f| f.thrown) {
            linking = vec![(self.frame % 2) as usize];
        }
        for i in linking {
            self.link(&gd, i);
        }
        // facing, after both fighters' logic, against where the opponent is now
        let player = sight_of(&self.fighters[0], &gd);
        if let Some(e) = self.extra.as_mut().filter(|e| !e.inactive) {
            (e.opponent.root, e.opponent.evasive) = (player.root, player.evasive);
            if e.freeze == 0 {
                e.track(&gd);
            }
        }
        for (i, frozen) in frozen.into_iter().enumerate() {
            let o = &self.fighters[1 - i];
            let (root, evasive) = match idle {
                Some(t) if i == 0 => (t, 0),
                _ => (o.root(), o.evasive),
            };
            let f = &mut self.fighters[i];
            (f.opponent.root, f.opponent.evasive) = (root, evasive);
            if !frozen {
                f.track(&gd);
            }
        }
        self.throw_damage(&gd);
        if self.ball.is_some() {
            self.ball_fighters();
        }
        if let Some(link) = self.link {
            if self.fighters[0].current != link[0] && self.fighters[1].current != link[1] {
                self.link = None;
                for f in &mut self.fighters {
                    f.locked = false;
                    f.thrown = false;
                }
            }
        }
        // the hit pass's per-frame flags (cleared at 0x80044008) and the body push's
        for f in &mut self.fighters {
            (f.struck, f.whiffed, f.pushed) = (false, false, false);
        }
        let pair = !self.fighters[0].inactive && !self.fighters[1].inactive;
        let extra_on = self.extra.as_ref().is_some_and(|e| !e.inactive) && !self.fighters[0].inactive;
        if self.link.is_none() && pair {
            self.separate(bodies.each_ref());
        }
        if let (true, Some((_, body, _))) = (extra_on && self.link.is_none(), extra) {
            self.swap_extra();
            self.separate([&bodies[0], body]);
            self.swap_extra();
        }
        // (hits are still detected after a K.O. or time-up: a falling body can be struck again)
        if self.link.is_none() && pair {
            self.resolve_hits(&gd, bodies.each_ref(), anims);
        }
        // the other soldier's hits on the player and the player's on it, as a pair of their own (actor 2)
        if let (true, Some((_, body, anim))) = (extra_on && self.state == RoundState::Fighting && self.link.is_none(), extra) {
            let before = self.hits.len();
            self.swap_extra();
            self.resolve_hits(&gd, [&bodies[0], body], [anims[0], anim]);
            self.swap_extra();
            for h in &mut self.hits[before..] {
                if h.attacker == 1 {
                    h.attacker = 2;
                }
                if h.victim == 1 {
                    h.victim = 2;
                }
            }
        }
        // the ball after the hit pass (it keeps flying after the K.O.)
        self.ball_step(&gd, &[&bodies[0], &bodies[1]]);
        // a fighter stopped by a wall: the push between the two moves the other one instead
        if self.keep_off_walls(roots_before) && self.link.is_none() && pair {
            self.separate(bodies.each_ref());
            self.keep_off_walls(roots_before);
        }
        if self.state == RoundState::Fighting && self.link.is_none() {
            self.clock = self.clock.saturating_sub(1);
            let ko_candidates = if self.ko_rule == KoRule::PlayerOnly { 0..1 } else { 0..2 };
            if let Some(loser) = ko_candidates.into_iter().find(|&i| self.fighters[i].health <= 0) {
                self.state = RoundState::Ko { winner: 1 - loser };
            } else if self.clock == 0 {
                let (a, b) = (self.fighters[0].health, self.fighters[1].health);
                self.state = RoundState::TimeUp { winner: if a == b { None } else { Some(if a > b { 0 } else { 1 }) } };
            }
        }
    }

    /// An arena with walls: a fighter whose root moved into a blocked cell stays where it was (on the floor plan;
    /// its height moves on). Whether one was stopped.
    fn keep_off_walls(&mut self, before: [Vec3; 2]) -> bool {
        let Some(w) = self.walls.clone() else { return false };
        let mut stopped = false;
        for (f, b) in self.fighters.iter_mut().zip(before) {
            let now = f.root();
            if !w.blocked(now) || w.blocked(b) {
                continue;
            }
            f.position += Vec3::new(b.x - now.x, 0.0, b.z - now.z);
            stopped = true;
        }
        stopped
    }

    /// Throw / hold link (0x80045550, 0x800306B8, reactions.md §5.2): fighter `i` started a link move. A
    /// thrower moves onto the victim's root and takes a facing set by the victim's orientation; the victim
    /// enters the alias the thrower's move names in the throw-victim table, at the same place and facing, so
    /// the two paired animations play from one origin. A thrown fighter that breaks free sends the thrower into
    /// the matching break animation the same way.
    fn link(&mut self, gd: &GameData, i: usize) {
        let j = 1 - i;
        let mv = self.fighters[i].current_move(gd).clone();
        // the thrown fighter starting a link move is breaking free
        let breaking = self.fighters[i].thrown;
        // the next part of a chain throw (0x800306B8): a thrower already linked re-snaps onto the partner's root and
        // facing only when the part says so (flags24 0x80000); otherwise it keeps its place
        let relink = !breaking && self.link.is_some() && self.fighters[i].locked;
        if relink {
            if mv.flags24.raw & 0x80000 != 0 {
                let (yaw, root) = (self.fighters[j].root_yaw, self.fighters[j].root());
                let fi = &mut self.fighters[i];
                (fi.yaw, fi.root_yaw) = (yaw, yaw);
                fi.position = Vec3::new(root.x, 0.0, root.z);
            }
        } else if !breaking {
            let (fi, fj) = (&self.fighters[i], &self.fighters[j]);
            let quadrant = direction_quadrant(fj.yaw, fj.root(), fi.root());
            let target = fj.root();
            let to = target - fi.position;
            let yaw = to.x.datan2(to.z) + [0.0, -FRAC_PI_2, PI, FRAC_PI_2][quadrant];
            let fi = &mut self.fighters[i];
            fi.position = Vec3::new(target.x, 0.0, target.z);
            fi.yaw = yaw;
            fi.root_yaw = yaw;
        } else {
            let (yaw, root) = (self.fighters[j].yaw, self.fighters[j].root());
            let fi = &mut self.fighters[i];
            fi.yaw = yaw;
            fi.root_yaw = yaw;
            fi.position = Vec3::new(root.x, 0.0, root.z);
        }
        let victim_alias = mv.reaction.is_throw().then(|| gd.reactions.throw_victims.entries.get(mv.reaction.index).copied()).flatten();
        let (yaw, position) = (self.fighters[i].yaw, self.fighters[i].position);
        self.fighters[i].locked = true;
        self.fighters[i].thrown = false;
        // the linking move's victim alias is in its owner's table: the thrower's (or, for a break, the set the
        // breaking fighter is playing from)
        let fi = &self.fighters[i];
        let owner = match (fi.current.bank, &fi.partner) {
            (Bank::Partner, Some(p)) => p.clone(),
            _ => fi.moveset.clone(),
        };
        let fj = &mut self.fighters[j];
        fj.locked = true;
        // the victim link (transition 0x22, 0x800307DC) counts the victim's chain stage down
        let chain = fj.chain;
        if let Some(alias) = victim_alias {
            let started = if Arc::ptr_eq(&owner, &fj.moveset) || *owner.moveset_id.as_ref().unwrap_or(&-1) == fj.moveset.moveset_id.unwrap_or(-2) {
                fj.play_alias(alias)
            } else {
                fj.play_partner_alias(alias, owner)
            };
            if started {
                // a thrown fighter stays down (0x80030910): the stun timer runs over the thrower's hold frame plus
                // the victim move (else the thrower's move), the pool from its damage word (a throw's type bits:
                // the most, 0x42)
                let hold = mv.bytes_18.hold_frame;
                fj.stun_timer = if hold != 0 { hold + fj.current_move(gd).frames } else { mv.frames }.min(300);
                let word = (mv.damage as u32 & 0x3FFF) | (mv.damage_type << 14);
                fj.stun = (0x22 + word * 32 / 45).clamp(0x22, 0x42);
                fj.chain = if chain > 0 { -1 } else { chain - 1 };
                fj.yaw = yaw;
                fj.root_yaw = yaw;
                fj.position = position;
                fj.offset = Vec3::ZERO;
                fj.pushback = None;
                fj.air = None;
            }
        }
        fj.thrown = !breaking;
        // the thrower's own pushback stops too (0x800307B4)
        self.fighters[i].pushback = None;
        self.link = Some(std::array::from_fn(|k| self.fighters[k].current));
        log::info!("P{} {} P{} ({} -> {:?})", i + 1, if breaking { "breaks free of" } else { "links" }, j + 1, mv.anim, victim_alias);
    }

    /// Throw damage: the thrower's timed property 0x0B takes its move damage off the locked partner
    /// (0x800449B4; no zone, counter or juggle scaling).
    fn throw_damage(&mut self, gd: &GameData) {
        // the moves' own damage to themselves and heals (timed ops 0x0C / 0x0D)
        if !self.ball_court() {
            for f in &mut self.fighters {
                if f.self_damage != 0 {
                    f.health = (f.health - f.self_damage * ONE).clamp(0, f.max_health * ONE);
                }
            }
        }
        self.throw_hits(gd);
    }

    /// The locked partners' throw damage (timed property 0x0B) of the pair in the slots.
    fn throw_hits(&mut self, gd: &GameData) {
        for i in 0..2 {
            let Some(damage) = self.fighters[i].deal_damage.filter(|_| self.fighters[i].locked) else { continue };
            let anim = self.fighters[i].current_move(gd).anim.clone();
            let ball = self.ball_court();
            let v = &mut self.fighters[1 - i];
            if !ball {
                v.health = (v.health - damage * ONE).clamp(0, v.max_health * ONE);
            }
            v.hit_flash = 8;
            log::info!("P{} takes {damage} from P{} {anim} (throw) -> health {}", 2 - i, i + 1, v.health / ONE);
            let point = v.root() + Vec3::Y * 900.0;
            self.hits.push(HitEvent {
                attacker: i,
                anim,
                zone: 0,
                damage,
                base_damage: damage,
                mask: 0,
                blocked: false,
                counter: false,
                clean: false,
                strike: Vec3::ZERO,
                limb: 0,
                ki: false,
                point,
                victim: 1 - i,
                health: v.health / ONE,
                throw: true,
            });
        }
    }

    /// Body collision (EXE push spheres, actor+0x324): every pair of push spheres that overlaps horizontally
    /// (and in height) pushes the two actors apart along the line between them, half each. The actor
    /// positions (not the animation's root lean) are moved; their roots never come closer than
    /// [`PUSH_DISTANCE`].
    fn separate(&mut self, bodies: [&BodyVolumes; 2]) {
        // (between the drawn roots; the push moves the origins)
        let (a, b) = (self.fighters[0].root(), self.fighters[1].root());
        let d = glam::Vec2::new(b.x - a.x, b.z - a.z);
        let gap = d.length();
        let dir = if gap > 1e-3 { d / gap } else { glam::Vec2::X };
        let mut overlap = (PUSH_DISTANCE - gap).max(0.0);
        for p in &bodies[0].push {
            for q in &bodies[1].push {
                let reach = p.radius + q.radius;
                if (p.centre.y - q.centre.y).abs() >= reach {
                    continue;
                }
                // overlap measured along the actors' separation axis
                let along = (q.centre.x - p.centre.x) * dir.x + (q.centre.z - p.centre.z) * dir.y;
                let side = glam::Vec2::new(q.centre.x - p.centre.x, q.centre.z - p.centre.z).length_squared() - along * along;
                if side < reach * reach {
                    overlap = overlap.max((reach * reach - side).sqrt() - along);
                }
            }
        }
        if overlap > 0.0 {
            let push = Vec3::new(dir.x, 0.0, dir.y) * (overlap * 0.5);
            self.fighters[0].position -= push;
            self.fighters[1].position += push;
            for f in &mut self.fighters {
                f.pushed = true;
            }
        }
    }

    /// Both directions are tested before any reaction is applied, so trades hit both fighters.
    fn resolve_hits(&mut self, gd: &GameData, bodies: [&BodyVolumes; 2], anims: [&dyn Animations; 2]) {
        let mut hits = Vec::new();
        // the strike's downward speed: segment 0's travel this frame (units, down positive), for spikes
        let mut strike_down = [0.0f32; 2];
        // postures before any reaction of this frame is applied (a trade checks both against the same state)
        let postures = [self.fighters[0].posture(gd), self.fighters[1].posture(gd)];
        for a in 0..2 {
            let segs = self.fighters[a].attack_segments(gd);
            strike_down[a] = segs.first().map_or(0.0, |(p, q)| p.y - q.y);
            if log::log_enabled!(log::Level::Debug) {
                for (p, q) in &segs {
                    let near = bodies[1 - a].hurt.iter().map(|s| (s.zone, s.centre.distance(*q), s.centre)).min_by(|x, y| x.1.total_cmp(&y.1));
                    log::debug!("P{} segment {p:?} -> {q:?}; nearest sphere {near:?}; P{} at {:?}", a + 1, 2 - a, self.fighters[1 - a].position);
                }
            }
            // the hit mask's low bits must match the victim's posture: highs whiff over crouchers (0x800442A4); a
            // victim recovering after a 0x1B point is hit as if crouching too (0x800441B8)
            let mut mask = self.fighters[a].current_move(gd).attack_mask.mask;
            let vf = &self.fighters[1 - a];
            // (no detection while the victim is still unhittable from a contact, 0x80047CDC)
            if vf.invulnerable > 0 {
                continue;
            }
            if vf.recover_guard == 2 {
                mask |= 1;
            }
            // lows (0x10F) pass under a neutral jump (state 0xC) in its jump window less 5 frames, or while
            // airborne (0x8004412C)
            let vm = vf.current_move(gd);
            if mask == 0x10F && (vm.posture.raw >> 11) & 0x1F == 0xC {
                let b = &vm.bytes_18;
                if (b.jump_start != 0 && vf.frame >= b.jump_start && vf.frame + 5 <= b.jump_end) || vf.airborne_flag(gd) {
                    continue;
                }
            }
            if mask & postures[1 - a] & 7 == 0 {
                continue;
            }
            // segment by segment, each against spheres 0..13 in order: the first contact decides the zone (0x80047CBC)
            let contact = segs.iter().find_map(|(p, q)| bodies[1 - a].hurt.iter().find(|s| segment_hits(*p, *q, s)).map(|s| (s, *p, *q)));
            if let Some((s, p, q)) = contact {
                // the strike's closest point to the sphere centre, pulled onto the sphere surface
                let d = q - p;
                let t = if d.length_squared() > 0.0 { ((s.centre - p).dot(d) / d.length_squared()).clamp(0.0, 1.0) } else { 1.0 };
                let on_seg = p + d * t;
                let point = s.centre + (on_seg - s.centre).clamp_length_max(s.radius);
                hits.push((a, s.zone, point, q - p));
            }
        }
        // an attack on its last active frame without contact whiffed (+0xD2, 0x80044374)
        for a in 0..2 {
            let f = &self.fighters[a];
            let m = f.current_move(gd);
            if m.active[0] != 0 && f.frame == m.active[1] && !f.connected && !hits.iter().any(|h| h.0 == a) {
                self.fighters[a].whiffed = true;
            }
        }
        // reversals and parries (0x80045104), before any damage, P1 first on odd frames: a struck fighter whose list
        // catches the attack cancels the contact and goes into its reversal (the unthrowable character cannot be
        // reversed)
        let order = if self.frame & 1 == 1 { [0, 1] } else { [1, 0] };
        for v in order {
            let a = 1 - v;
            if !hits.iter().any(|h| h.0 == a) || self.fighters[a].character.is_some_and(|c| gd.characters.unreversible.contains(&c)) {
                continue;
            }
            let Some(e) = self.fighters[v].reversal_entry(gd, &self.fighters[a]) else { continue };
            hits.retain(|h| h.0 != a);
            let am = self.fighters[a].current_move(gd);
            let (damage, anim) = (if am.damage_type == 1 { 0 } else { am.damage }, am.anim.clone());
            self.fighters[a].connected = false;
            let fv = &mut self.fighters[v];
            let Some(to) = fv.resolve(e.dest) else { continue };
            let t = normalize_transition(e.transition & 0x3F, fv.moves(gd, to.bank).moves.get(to.index), e.exec);
            fv.reversal_damage = Some(damage / 2 + 25);
            log::info!("P{} reverses P{}'s {anim} (cond {:#x})", v + 1, a + 1, e.cond);
            if e.exec == 255 || fv.frame < e.exec {
                fv.queued = Some((to, e.exec, t));
                fv.queued_alias = e.dest;
                fv.queued_buffered = false;
            } else {
                fv.commit(gd, to, e.dest, t, false, anims[v]);
                if t == TRANSITION_LINK {
                    self.link(gd, v);
                }
            }
        }
        if hits.is_empty() {
            return;
        }
        // counter hits are judged on the victims' moves before this frame's reactions
        // (throw attempts have no damage here: +0x5E is zeroed for damage type 1); a ki charge makes every hit by
        // or on the charger one
        let in_startup: [bool; 2] = std::array::from_fn(|k| {
            let f = &self.fighters[k];
            let m = f.current_move(gd);
            m.damage != 0 && m.damage_type != 1 && m.active[0] > 0 && f.frame < m.active[0]
        });
        let ki = [self.fighters[0].ki > 0, self.fighters[1].ki > 0];
        // every hit is judged on the moves as they were before any of this frame's reactions (0x8004439C runs
        // before 0x800451B8 queues them): in a trade both take their damage and play their own reactions
        let moves = [self.fighters[0].current_move(gd).clone(), self.fighters[1].current_move(gd).clone()];
        // victims in the frame's actor order (P1 first on odd frames, 0x800451B8): one that dies this frame leaves the
        // other's reaction out (0x80045330, +0xB7)
        hits.sort_by_key(|h| order.iter().position(|&o| o == 1 - h.0));
        let mut died = false;
        for (a, zone, point, strike) in hits {
            let mv = &moves[a];
            self.fighters[a].connected = true;
            // contact, hit or block (0x800442D8): struck this frame and in this move
            self.fighters[1 - a].struck = true;
            if mv.damage != 0 || self.fighters[1 - a].airborne_flag(gd) {
                self.fighters[1 - a].invulnerable = 4;
            }
            if mv.damage_type == 1 {
                self.fighters[1 - a].struck_in_move = true;
                // a throw attempt: contact only (+0xD1); its command list links the throw (reactions.md §5.1)
                continue;
            }
            let v = 1 - a;
            let victim_posture = postures[v];
            // (the actual roots: a fighter's position lags behind by its move's root motion)
            let to_attacker = self.fighters[a].root() - self.fighters[v].root();
            let distance = glam::Vec2::new(to_attacker.x, to_attacker.z).length();
            let quadrant = direction_quadrant(self.fighters[v].yaw, self.fighters[v].root(), self.fighters[a].root());
            let facing = Vec3::new(self.fighters[v].yaw.dsin(), 0.0, self.fighters[v].yaw.dcos());
            let airborne = victim_posture & posture::AIRBORNE != 0 || self.fighters[v].air.is_some() || self.fighters[v].airborne_flag(gd);
            let lying = victim_posture & posture::LYING != 0;
            // blocked: a guard bit of the mask matches the victim's guard, and the victim faces the attacker; not in
            // the air (the jump landings' guard postures do not guard, 0x80040640)
            let blocked = mv.attack_mask.mask & victim_posture & posture::GUARDS != 0 && facing.dot(to_attacker) > 0.0 && !self.fighters[v].airborne_flag(gd);
            // close range (record +0x34): its own reaction and 1.5x damage without zone scaling
            let close = gd.reactions.close_range.entries.get(mv.close_range).filter(|c| mv.close_range != 0 && distance < c.max_distance as f32);
            let mut blocked = blocked;
            let counter = !blocked && (in_startup[v] || ki[0] || ki[1]);
            // chip (0x80044BD8): with GUARD DAMAGE on, or from a charging attacker; a chip KO becomes a clean hit
            // but keeps the chip's damage (0x80044730)
            let chip = blocked && (self.guard_damage || ki[a]);
            let chip_ko = chip && self.fighters[v].health <= (mv.damage / 10 + 2) * ONE;
            let mut damage = if blocked {
                if chip {
                    mv.damage / 10 + 2
                } else {
                    0
                }
            } else if close.is_some() {
                mv.damage + mv.damage / 2
            } else {
                mv.damage * gd.zone_percent.get(zone).copied().unwrap_or(100) / 100
            };
            if counter {
                // D * 12/10 + (3 * (3 * |victim's move damage| / 10) / 10) * f / max(startup, f), truncating each
                // step (0x80044C94); during a ki charge the bonus is 0.3 * |victim's move damage|
                let vm = &moves[v];
                let f = self.fighters[v].frame as i32;
                let dv = vm.damage.abs();
                let startup = vm.active[0] as i32;
                let bonus = if ki[0] || ki[1] { dv * 3 / 10 } else { 3 * (3 * dv / 10) / 10 * f / if startup == 0 || startup < f { f.max(1) } else { startup } };
                damage = damage * 12 / 10 + bonus;
            }
            if chip_ko {
                blocked = false;
            }
            self.fighters[a].landed = Some(!blocked);
            self.fighters[a].landed_counter = counter;
            if airborne && !blocked {
                // juggle damage: the first air hit 80 %, later ones 50 %
                damage = damage * if self.fighters[v].juggle == 0 { gd.juggle_percent[0] } else { gd.juggle_percent[1] } / 100;
            }
            // Ball: the hit plays but takes no health (0x800446C0 gives it back); health stays within 0..max
            if !self.ball_court() {
                let fv = &mut self.fighters[v];
                fv.health = (fv.health - damage * ONE).clamp(0, fv.max_health * ONE);
            }
            let ko = self.fighters[v].health <= 0;
            let skip_reaction = died && !blocked;
            died |= ko && !blocked;
            self.fighters[v].hit_flash = if blocked { 0 } else { 8 };
            // the reaction is queued and committed at the next frame's advance (0x8002EBA4 -> 0x8002F3AC), then
            // held for the hit-stop (none on a K.O., 0x8002F8E4): the victim's frame 2 comes at contact + 1 +
            // max(hit-stop, 1)
            // (one alias holds its own frames, 0x80044314)
            let hitstop = match gd.moves.long_hitstop {
                Some((alias, frames)) if self.fighters[a].alias == alias => frames,
                _ => mv.hitstop,
            };
            if !skip_reaction {
                self.fighters[v].freeze = if ko { 1 } else { hitstop.max(1) };
            }
            // a clean hit ends both fighters' ki charges (0x8004078C) and the knockdown stun's hold (0x800403E4)
            if !blocked {
                self.fighters[0].ki = 0;
                self.fighters[1].ki = 0;
            }
            let entry_index = close.map_or(mv.reaction.index, |c| c.reaction_id);
            let entry = if mv.reaction.is_throw() && close.is_none() { None } else { gd.reactions.entries.get(entry_index) };
            let param = |k: usize| entry.and_then(|e| e.params.get(k).copied()).unwrap_or(0);
            // reaction slot (0x8002EBA4): block, air table, lying, counter, then by direction; st = 0 standing
            let st = usize::from(victim_posture & posture::STANDING == 0);
            let mut pushback = None;
            if skip_reaction {
                // (the survivor of a trade that killed the other: no reaction)
            } else if airborne && !blocked && self.juggle_hit(gd, AirHit { victim: v, attacker: a, damage, quadrant, strike_down: strike_down[a], angle: param(0) }, anims[v]) {
                // a spike slides on its own
            } else if airborne && !blocked {
                // air hits use the fixed air pushback curve, straight away from the attacker
                let samples = &gd.reactions.air_pushback_curve.samples;
                let mut impulses = [0; 8];
                for (k, s) in samples.iter().take(8).enumerate() {
                    impulses[k] = *s;
                }
                let away = -glam::Vec2::new(to_attacker.x, to_attacker.z).normalize_or_zero();
                let curve = PushbackCurve { index: 0, constant_frames: 0, velocity: 0, impulses };
                self.fighters[v].pushback = Some(Pushback { curve, frame: 0, direction: away });
            } else {
                let (slot, curve, angle) = if blocked {
                    (10 + st, param(6), param(0))
                } else if lying {
                    (if victim_posture & posture::LYING_FACEDOWN != 0 { 13 } else { 12 }, param(5), param(4))
                } else if counter {
                    (2 + st, param(3), param(0))
                } else {
                    ([0, 4, 8, 6][quadrant] + st, param(1), param(0))
                };
                // side and back hits of the same type in a row push further (0x80031348; 0x27 side A, 0x28 side B,
                // 0x2B back)
                if !blocked && !lying && !counter && quadrant != 0 {
                    let t = [0x2A, 0x27, 0x2B, 0x28][quadrant];
                    let fv = &mut self.fighters[v];
                    fv.side_chain = if fv.side_timer > 0 && fv.side_last == t { fv.side_chain + 1 } else { 0 };
                    fv.side_last = t;
                }
                // the knockdown stun pool (0x80030C8C / 0x80030D08): counter and directional hits add to it (none
                // after a 0x1000 move), ground hits empty it (0x80030C6C)
                if !blocked {
                    let fv = &mut self.fighters[v];
                    fv.stun = if slot >= 12 || moves[v].flags24.raw & 0x1000 != 0 { 0 } else { (damage.unsigned_abs() + fv.stun / 2).min(0x42) };
                }
                if let Some(&alias) = entry.and_then(|e| e.victims.get(slot)) {
                    if let Some(to) = self.fighters[v].resolve(alias) {
                        let fv = &mut self.fighters[v];
                        fv.start_reaction(to, alias);
                        // blocks and counter hits (0x800309E4, 0x80030C8C): the root basis on the attacker, and the
                        // body turning to it while it reels (mode 7)
                        if blocked || counter {
                            fv.root_yaw = fv.geometry().2;
                            fv.mode = 7;
                        }
                        if !blocked && !lying && !counter && quadrant != 0 {
                            fv.side_timer = fv.current_move(gd).frames + 10;
                        }
                    }
                }
                pushback = Some((curve, angle));
                // a clean hit's K.O. (0x80045420): in the main modes the collapse 0xD66 at once (committed this frame,
                // guard cleared) unless the reaction is a lying one or has a jump window, or its next move is the
                // stance anyway; elsewhere a reaction going back to the stance falls through the air table
                let (to_stance, lying_or_air) = {
                    let m = self.fighters[v].current_move(gd);
                    (m.next.alias == 3 || m.next.alias == 0x28, m.posture.raw & 4 != 0 || m.bytes_18.jump_start != 0)
                };
                if ko && !blocked {
                    if self.ko_collapse && (to_stance || !lying_or_air) {
                        let fv = &mut self.fighters[v];
                        if let Some(to) = fv.resolve(KO_COLLAPSE) {
                            fv.start_reaction(to, KO_COLLAPSE);
                            fv.freeze = 0;
                        }
                    } else if !self.ko_collapse && to_stance && !lying {
                        self.juggle_hit(gd, AirHit { victim: v, attacker: a, damage, quadrant, strike_down: strike_down[a], angle: param(0) }, anims[v]);
                        pushback = None;
                    }
                }
            }
            if !blocked {
                self.fighters[v].stun_timer = 0;
            }
            // pushback: away from the attacker turned by the angle parameter (65536 per turn)
            if let Some((curve_ref, angle)) = pushback {
                let away = -glam::Vec2::new(to_attacker.x, to_attacker.z).normalize_or_zero();
                let turn = angle as f32 * std::f32::consts::TAU / 65536.0;
                let direction = glam::Vec2::from_angle(turn).rotate(away);
                self.fighters[v].pushback = gd.reactions.pushback_curves.entries.iter().find(|c| c.index as i32 == curve_ref).map(|curve| Pushback { curve: curve.clone(), frame: 0, direction });
            }
            self.fighters[v].struck_in_move = true;
            let what = if blocked {
                "blocks"
            } else if counter {
                "takes a counter hit of"
            } else if airborne {
                "is juggled for"
            } else {
                "takes"
            };
            log::info!("P{} {what} {} from P{} {} (zone {zone}) -> health {}", v + 1, damage, a + 1, mv.anim, self.fighters[v].health / ONE);
            let (base_damage, mask) = (mv.damage, mv.attack_mask.mask);
            self.hits.push(HitEvent {
                attacker: a,
                anim: mv.anim.clone(),
                zone,
                damage,
                base_damage,
                mask,
                blocked,
                counter,
                clean: close.is_some() && !blocked,
                strike,
                limb: mv.attack.as_ref().and_then(|x| x.header_bytes.first().copied()).unwrap_or(0),
                ki: ki[a],
                point,
                victim: v,
                health: self.fighters[v].health / ONE,
                throw: false,
            });
        }
    }

    /// A hit on a fighter in the air (0x8002EE94, 0x8002F074): the common air reaction (side hits use their
    /// own), popped up again: vy = clamp(4·dmg + 40 − 7·n + falling speed / 8, ±100), at least up to the
    /// reaction's target height; the fall animation is phased so its hold frame comes at the landing.
    /// Returns whether it was a spike (straight into the floor, its own slide instead of the air pushback).
    fn juggle_hit(&mut self, gd: &GameData, hit: AirHit, _anims: &dyn Animations) -> bool {
        let AirHit { victim: v, attacker: a, damage, quadrant, strike_down, angle } = hit;
        let table = &gd.reactions.air_reactions.entries;
        // the air reaction (0x8002EE94): a late fall (low, or already falling sideways), a spike (the striking limb
        // moving down 110+ units this frame, or two moves the code names), else by the side hit
        let (vf, af) = (&self.fighters[v], &self.fighters[a]);
        let plays = |f: &Fighter, alias: usize| f.resolve(alias) == Some(f.current);
        let queued = |f: &Fighter, alias: usize| f.queued.is_some_and(|q| f.resolve(alias) == Some(q.0));
        let late = plays(vf, AIR_LATE) || ((plays(vf, AIR_SIDE[0]) || plays(vf, AIR_SIDE[1])) && vf.frame >= 19);
        let index = if late {
            if plays(vf, AIR_LATE) || (vf.position.y + vf.anim_root_y).abs() < 0x400 as f32 {
                2
            } else {
                0
            }
        } else {
            let ms = af.moveset.moveset_id;
            let rule = gd.moves.air_spikes.iter().find(|s| ms == Some(s.moveset) && plays(af, s.alias));
            let normal = rule.is_some_and(|s| s.unless_queued.iter().any(|&q| queued(af, q)));
            if !normal && (rule.is_some() || strike_down >= 0x6E as f32) {
                1
            } else if plays(vf, AIR_RECOVERY) {
                0
            } else {
                match quadrant {
                    3 => 3,
                    1 => 4,
                    _ => 0,
                }
            }
        };
        let spike = index == 1;
        let Some(air) = table.get(index).or_else(|| table.first()) else { return false };
        let f = &mut self.fighters[v];
        // knockdown stun (0x80030AF8): this hit's damage plus half the pool left (none after a 0x1000 move)
        f.stun = if f.current_move(gd).flags24.raw & 0x1000 != 0 { 0 } else { (damage.unsigned_abs() + f.stun / 2).min(0x42) };
        let (old_motion, old_frames, old_frame) = {
            let m = f.current_move(gd);
            (m.anim.clone(), m.frames, f.frame)
        };
        let y0 = f.position.y + f.anim_root_y;
        // the root's falling speed (in a flight its speed, else the animation's) and the strike's push down
        let falling = f.air.map_or(f.root_fall, |a| a.down);
        let target = air.target_height as f32;
        let Some(to) = f.resolve(air.alias) else { return false };
        let left_1000 = f.current_move(gd).flags24.raw & 0x1000 != 0;
        let toward = f.geometry().2;
        f.start_reaction(to, air.alias);
        // the fall's root travel runs along the line to the attacker (0x80030A7C), turned by p0 after a 0x1000 move
        f.root_yaw = toward + if left_1000 { angle as f32 * std::f32::consts::TAU / 65536.0 } else { 0.0 };
        let mv_frames = f.current_move(gd).frames;
        f.air_pop(gd, Pop { damage, strike_down, target, prev: (old_motion, old_frames, old_frame), y0, falling, spike, extra_drift: 0, kind: index });
        if spike {
            // it slides 40 units a frame away from the attacker for the rest of the animation
            let away = f.root() - self.fighters[a].root();
            let direction = glam::Vec2::new(away.x, away.z).normalize_or_zero();
            // (with the air curve's impulses on top, 0x8002F2F8)
            let mut impulses = [0; 8];
            for (k, v) in gd.reactions.air_pushback_curve.samples.iter().take(8).enumerate() {
                impulses[k] = *v;
            }
            let curve = PushbackCurve { index: 0, constant_frames: mv_frames as i32 - 6, velocity: 40, impulses };
            self.fighters[v].pushback = Some(Pushback { curve, frame: 0, direction });
        }
        spike
    }
}

/// Where `other` is, seen from a fighter facing `yaw` at `from` (0x80043A94, reactions.md §1.2): 0 front
/// (±45°), 1 side A, 2 back, 3 side B. Angles are mirrored into the game's convention (OpenKumite's z axis is
/// flipped); which side is A or B is not established.
pub fn direction_quadrant(yaw: f32, from: Vec3, other: Vec3) -> usize {
    let d = other - from;
    let to = d.x.datan2(d.z);
    let diff = (to - yaw + std::f32::consts::FRAC_PI_4).rem_euclid(std::f32::consts::TAU);
    ((diff / FRAC_PI_2) as usize).min(3)
}
