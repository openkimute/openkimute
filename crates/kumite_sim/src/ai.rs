//! CPU opponent: the game's AI routine 0x80059890, frame by frame (kumite_converter's reference ai.md), driven by the converted
//! `gamedata/ai.json` tables.
//!
//! Like the original it has no per-character move lists: each frame it collects the entries of its current
//! move's command list (and of the END link's next move) that it could input now, marks a pool of them with
//! the predicate of the situation (guard moves, interrupts, throws, pokes, okizeme ...) and inputs one of them
//! through the normal input parser. Everything else is a pad word written by the decision tree (walks, guards,
//! sidesteps, canned motions). Probabilities are out of 4096 and drawn in the original order from the game's two
//! random streams ([`AiRng`]); the state block, the profile copy and its run-time changes (learning, rage, the
//! low-HP zeroing) are kept as the game keeps them.
//!
//! Not ported (never reached outside these modes): Ball / Force hooks and the attract demo's
//! frame skip and halved cooldowns. (A CPU Force player, OpenKumite's own, plays the stage over the routine: chickens,
//! the way on, the soldiers around it; [`ForceSight`].)

use crate::data::{Cancel, CancelKind, Move};
use crate::input::Pad;
use crate::{Angles, Current, Fighter, GameData, ONE};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;

/// A profile (0x800231AC, 110 bytes per difficulty and level; ai.md §5): the fields the routine reads, by their
/// offset. Probabilities are out of 4096. The CPU keeps a copy that learning and rage change.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Profile {
    /// P00..P03 (u8): wake-up tap intervals.
    pub wake_up_intervals: [i32; 4],
    /// P04, P06..P0C: attack cooldown base and its four random additions.
    pub attack_cooldown_base: i32,
    pub attack_cooldown_0: i32,
    pub attack_cooldown_1: i32,
    pub attack_cooldown_2: i32,
    pub attack_cooldown_3: i32,
    /// P0E: chain throw length.
    pub max_string_length: i32,
    /// P10, P12, P14, P16: throw attempt, throw escape, chain throw continue, throw kind (two-stage throws).
    pub throw_attempt_prob: i32,
    pub throw_escape_prob: i32,
    pub string_continue_prob: i32,
    pub throw_select_prob: i32,
    /// P18, P1A: guard-aware attacks, fast pokes.
    pub guard_aware_attack_prob: i32,
    pub fast_poke_prob: i32,
    /// P1C / P1E: guard and its learning cap.
    pub guard_prob: i32,
    pub guard_prob_cap: i32,
    /// P20 / P22: counter attack and its cap (P22 also against a move that hit me, and against hops).
    pub counter_attack_prob: i32,
    pub counter_attack_cap: i32,
    /// P24: sidestep.
    pub sidestep_prob: i32,
    /// P26 / P28, P2A / P2C: interrupt and guard inside his string, with their caps.
    pub in_string_interrupt_prob: i32,
    pub in_string_interrupt_cap: i32,
    pub in_string_guard_prob: i32,
    pub in_string_guard_cap: i32,
    /// P32, P36: consecutive attack limits (the larger one counts).
    pub consecutive_attack_limit_a: i32,
    pub consecutive_attack_limit_b: i32,
    /// P3A: evade plans and the per-character handlers.
    pub evade_and_handler_prob: i32,
    /// P3C: pressure on a stunned opponent.
    pub pressure_on_stun_prob: i32,
    /// P46 (0 in every profile: the tests "draw >= P46" always pass), P48 anti-air (0 everywhere).
    pub gate_always_zero: i32,
    pub anti_air_prob: i32,
    /// P4C: keep a walk / dash back.
    pub keep_walk_back_prob: i32,
    /// P5A: parries and reversals; P5E: throw interrupt.
    pub parry_prob: i32,
    pub counter_throw_startup_prob: i32,
    /// P62: recover from a stun.
    pub stun_recovery_prob: i32,
}

impl Profile {
    /// P[06 + 2k].
    fn cooldown(&self, k: u32) -> i32 {
        [self.attack_cooldown_0, self.attack_cooldown_1, self.attack_cooldown_2, self.attack_cooldown_3][k as usize & 3]
    }
}

#[derive(Deserialize)]
struct Json {
    profiles: Entries<ProfileEntry>,
    spacing: Entries<SpacingEntry>,
    zone_masks: ZoneMasks,
    per_moveset: Entries<PerMovesetEntry>,
    handlers: Handlers,
    canned_sequences: Entries<CannedEntry>,
    motion_records: Entries<MotionRecord>,
    boss_punish_pads: PunishPads,
    evade_choices: Vec<u8>,
    wakeup_categories: Vec<u32>,
    rules: Rules,
}
#[derive(Deserialize)]
struct Entries<T> {
    entries: Vec<T>,
}
#[derive(Deserialize)]
struct ProfileEntry {
    difficulty: usize,
    level: usize,
    fields: Profile,
}
#[derive(Deserialize)]
struct SpacingEntry {
    character: usize,
    values: [i32; 6],
}
#[derive(Deserialize)]
struct ZoneMasks {
    my_zone: [u32; 6],
    his_zone: [u32; 6],
}
#[derive(Deserialize)]
struct Words {
    words: Vec<u16>,
}
#[derive(Deserialize)]
struct Prelude {
    prob: i32,
    alias: usize,
    sequence: Words,
}
#[derive(Deserialize)]
struct PerMovesetEntry {
    moveset: usize,
    can_parry: i32,
    high_mid_reversal: Option<Words>,
    low_parry: Option<Words>,
    combo_preludes: Vec<Prelude>,
}
#[derive(Deserialize)]
struct Handlers {
    entries: Vec<HandlerEntry>,
    /// By routine address: what it does and the aliases its code compares.
    routines: std::collections::BTreeMap<String, Routine>,
}
#[derive(Deserialize)]
struct HandlerEntry {
    moveset: i64,
    vs_handler: Option<String>,
    self_handler: Option<String>,
}
#[derive(Deserialize)]
struct CannedEntry {
    id: String,
    words: Vec<u16>,
}
#[derive(Deserialize)]
struct MotionRecord {
    cmd: u32,
    steps: Vec<u16>,
}
#[derive(Deserialize)]
struct PunishPads {
    pad_words: Vec<u16>,
}

/// The per-moveset table 0x80022D14: can parry, the canned high/mid reversal and low parry inputs, and the
/// combo preludes `{prob, alias, input}` (the input is played from its second word).
#[derive(Clone, Debug, Default)]
struct Perms {
    can_parry: bool,
    hi_seq: Option<Vec<u16>>,
    lo_seq: Option<Vec<u16>>,
    preludes: Vec<(i32, usize, Vec<u16>)>,
}

/// The per-moveset handlers (table 0x80023134 holds their code addresses): what one does (ai.json
/// `handlers.routines`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Handler {
    /// 0x800628C4: walk back from his `attack`, or attack before its frame 15.
    BackOff,
    /// 0x8006251C: canned throws when `reach` can be input.
    CannedThrows,
    /// 0x80061B88: reactions to his special moves.
    SpecialMoves,
    /// 0x8006299C: forward out of my `stance`.
    StanceDash,
    /// 0x80061EF0: dodges of his `breath` and `low` fire.
    FireDodge,
    /// 0x80062940: attack now against his `attack` up close, back off from far.
    CloseIn,
    /// The stubs (0x80062994, 0x80062928 ...): "not handled".
    #[default]
    #[serde(other)]
    Stub,
}

/// A handler routine: its kind and the aliases its code compares, by their role.
#[derive(Clone, Debug, Default, Deserialize)]
struct Routine {
    kind: Handler,
    #[serde(default)]
    aliases: std::collections::BTreeMap<String, Vec<usize>>,
}

impl Routine {
    fn is(&self, role: &str, alias: usize) -> bool {
        self.aliases.get(role).is_some_and(|a| a.contains(&alias))
    }
}

/// Moves the routine names: these aliases, of these move sets (any without).
#[derive(Deserialize)]
struct Moves {
    #[serde(default)]
    movesets: Option<Vec<i64>>,
    aliases: Vec<usize>,
}

impl Moves {
    fn has(&self, moveset: i64, alias: usize) -> bool {
        self.movesets.as_ref().is_none_or(|m| m.contains(&moveset)) && self.aliases.contains(&alias)
    }
}

#[derive(Deserialize)]
struct MovesetAlias {
    moveset: i64,
    alias: usize,
}
#[derive(Deserialize)]
struct MovesetDescriptor {
    moveset: i64,
    descriptor: u32,
}
#[derive(Deserialize)]
struct Movesets {
    movesets: Vec<i64>,
}
#[derive(Deserialize)]
struct Aliases {
    aliases: Vec<usize>,
}
#[derive(Deserialize)]
struct LateParry {
    moveset: i64,
    until: [i32; 2],
    alias: usize,
}
#[derive(Deserialize)]
struct OneAlias {
    alias: usize,
}
#[derive(Deserialize)]
struct OneMoveset {
    moveset: i64,
}
#[derive(Deserialize)]
struct Closer {
    moveset: i64,
    distance: i32,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Allow {
    Never,
    Coin,
    NoCoin,
}
/// A chain throw's next part to these aliases: against these move sets (everyone without) never, only on the coin,
/// or only without it.
#[derive(Deserialize)]
struct ChainRule {
    aliases: Vec<usize>,
    #[serde(default)]
    against: Option<Vec<i64>>,
    allow: Allow,
}

/// The routine's tests of particular move sets and aliases (ai.json `rules`).
#[derive(Deserialize)]
struct Rules {
    /// His fire move (+0x200: no stance escape, the walks go on) and his low fire (+0x201: his attack word gains
    /// 0x10000; the wake-up backs off).
    fire: MovesetDescriptor,
    low_fire: MovesetDescriptor,
    /// +0x88: his stance (no reactions, no okizeme).
    stance: MovesetAlias,
    chain_throws: Entries<ChainRule>,
    /// His throws the victim always tries to escape.
    escaped_throws: Entries<Moves>,
    /// My wake-up category 4 is forward, not back.
    wake_forward: Movesets,
    /// Turning round by my facing, not by his zone.
    turn_by_facing: Movesets,
    /// Reaction 5's parries (left out of the rare neutral pick).
    parries: Aliases,
    /// Reaction 6: a move set's parry while his attack is `until` frames away.
    late_parry: LateParry,
    /// His alias that takes 2 off my cooldown.
    counter_alias: OneAlias,
    /// No throws against him.
    unthrown: OneMoveset,
    /// The evade plans take him as `distance` nearer.
    evade_closer: Closer,
}

/// Everything the AI reads from ai.json.
pub struct AiData {
    /// [difficulty 0..3][level 0..10]
    profiles: Vec<Vec<Profile>>,
    /// Reach thresholds per character id (0x80098260): four zone boundaries, unused, the body offset.
    spacing: Vec<[i32; 6]>,
    /// "Can I reach him from my zone" (0x80098230) and "can his move reach me" (0x80098248).
    zm: [u32; 6],
    zm2: [u32; 6],
    perms: Vec<Perms>,
    /// (moveset, vs handler, self handler) in table order: indices of `routines`.
    handlers: Vec<(i64, Option<usize>, Option<usize>)>,
    routines: Vec<Routine>,
    rules: Rules,
    /// Canned inputs: the sidesteps (0x8002306E + side * 0x18), the backdash 0x80022C46, okizeme 0x80022C2A,
    /// One character's throws (0x800230DE.. standing, 0x800230A6.. crouching, 0x800230C2.. downed).
    sidestep: [Vec<u16>; 2],
    backdash: Vec<u16>,
    okizeme: Vec<u16>,
    throws_stand: Vec<Vec<u16>>,
    throws_crouch: Vec<Vec<u16>>,
    throws_down: Vec<Vec<u16>>,
    /// Steps of the dash / backdash commands 0xC001 / 0xC002 (0x8002CD7C).
    dash_records: Vec<(u32, Vec<u16>)>,
    /// Pad words of the last boss's punish (0x8002309C).
    punish_pads: Vec<u16>,
    /// Evade plans (0x80022E80) and wake-up categories (0x80022E74).
    evade: Vec<u8>,
    wakecat: Vec<u32>,
}

/// The most characters or movesets ai.json's tables may name.
const MAX_ROWS: usize = 1024;

impl AiData {
    pub fn load(path: &Path) -> Result<Self> {
        let j: Json = crate::data::load(path)?;
        let mut profiles = vec![vec![Profile::default(); 10]; 3];
        for e in j.profiles.entries {
            if e.difficulty < 3 && e.level < 10 {
                profiles[e.difficulty][e.level] = e.fields;
            }
        }
        // (a table's rows by character and moveset: a number past any is a bad file, skipped, not memory asked for)
        let mut spacing = Vec::new();
        for e in j.spacing.entries.into_iter().filter(|e| e.character < MAX_ROWS) {
            if e.character >= spacing.len() {
                spacing.resize(e.character + 1, [0; 6]);
            }
            spacing[e.character] = e.values;
        }
        let mut perms = Vec::new();
        for e in j.per_moveset.entries.into_iter().filter(|e| e.moveset < MAX_ROWS) {
            if e.moveset >= perms.len() {
                perms.resize(e.moveset + 1, Perms::default());
            }
            perms[e.moveset] = Perms {
                can_parry: e.can_parry != 0,
                hi_seq: e.high_mid_reversal.map(|w| w.words),
                lo_seq: e.low_parry.map(|w| w.words),
                preludes: e.combo_preludes.into_iter().map(|p| (p.prob, p.alias, p.sequence.words)).collect(),
            };
        }
        // (a routine the data does not describe: a stub)
        let mut routines = vec![Routine::default()];
        // (addresses compared as numbers: "0x800628C4" and "800628c4" are one)
        let key = |a: &str| u32::from_str_radix(a.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).ok();
        let mut index = std::collections::BTreeMap::new();
        for (address, r) in j.handlers.routines {
            if let Some(k) = key(&address) {
                index.insert(k, routines.len());
                routines.push(r);
            }
        }
        let of = |a: &Option<String>| a.as_ref().map(|a| key(a).and_then(|k| index.get(&k).copied()).unwrap_or(0));
        let handlers = j.handlers.entries.iter().map(|e| (e.moveset, of(&e.vs_handler), of(&e.self_handler))).collect();
        let canned = |id: &str| -> Result<Vec<u16>> { j.canned_sequences.entries.iter().find(|e| e.id == id).map(|e| e.words.clone()).with_context(|| format!("ai.json: canned sequence {id}")) };
        let group =
            |prefix: &str| -> Vec<Vec<u16>> { j.canned_sequences.entries.iter().filter(|e| e.id.strip_prefix(prefix).is_some_and(|n| n.parse::<u32>().is_ok())).map(|e| e.words.clone()).collect() };
        Ok(Self {
            profiles,
            spacing,
            zm: j.zone_masks.my_zone,
            zm2: j.zone_masks.his_zone,
            perms,
            handlers,
            routines,
            rules: j.rules,
            sidestep: [canned("sidestep_0")?, canned("sidestep_1")?],
            backdash: canned("backdash")?,
            okizeme: canned("okizeme")?,
            throws_stand: group("throws_stand_"),
            throws_crouch: group("throws_crouch_"),
            throws_down: group("throws_down_"),
            dash_records: j.motion_records.entries.into_iter().map(|r| (r.cmd, r.steps)).collect(),
            punish_pads: j.boss_punish_pads.pad_words,
            evade: j.evade_choices,
            wakecat: j.wakeup_categories,
        })
    }

    pub fn profile(&self, difficulty: usize, level: usize) -> &Profile {
        &self.profiles[difficulty.min(2)][level.min(9)]
    }

    /// The handlers for a moveset (0x80062868: the first table entry with it): against it, and its own.
    fn handlers_of(&self, moveset: Option<i64>) -> (Option<usize>, Option<usize>) {
        self.handlers.iter().find(|h| Some(h.0) == moveset).map_or((None, None), |h| (h.1, h.2))
    }

    /// A throw (`moveset`, `alias`) the victim always tries to escape (0x8005AB28).
    fn escapable(&self, moveset: i64, alias: usize) -> bool {
        self.rules.escaped_throws.entries.iter().any(|m| m.has(moveset, alias))
    }

    /// A chain throw may go on to `dest` against `op_moveset`, with the coin (`lcg & 0x70`) (0x8005A8E0).
    fn chain_ok(&self, dest: usize, op_moveset: i64, coin: u32) -> bool {
        match self.rules.chain_throws.entries.iter().find(|c| c.aliases.contains(&dest)) {
            Some(c) if c.against.as_ref().is_none_or(|a| a.contains(&op_moveset)) => match c.allow {
                Allow::Never => false,
                Allow::Coin => coin != 0,
                Allow::NoCoin => coin == 0,
            },
            _ => true,
        }
    }

    /// Reach thresholds of a character (`+0x18`).
    fn thresholds(&self, character: Option<i64>) -> [i32; 6] {
        let c = character.unwrap_or(0).max(0) as usize;
        self.spacing.get(c).or(self.spacing.last()).copied().unwrap_or_default()
    }
}

/// The game's two random streams, shared by both CPUs and kept for the whole match: the AI LCG (0x800ADCA0,
/// `x = x*5 + 3`, never seeded: 0 at boot) and libc `rand()` (0x8007A318, seeded once at boot).
#[derive(Clone, Debug)]
pub struct AiRng {
    lcg: u32,
    rand: u32,
}

impl AiRng {
    /// `seed`: the libc state (OpenKumite seeds it per bout, so equal inputs give equal fights).
    pub fn new(seed: u32) -> Self {
        Self { lcg: 0, rand: seed }
    }

    /// Step the AI LCG, returning its old value (the tests use the old value).
    fn lcg(&mut self) -> u32 {
        let o = self.lcg;
        self.lcg = o.wrapping_mul(5).wrapping_add(3);
        o
    }

    /// libc rand(): 0..0x7FFF.
    fn rand(&mut self) -> i32 {
        self.rand = self.rand.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039);
        ((self.rand >> 16) & 0x7FFF) as i32
    }

    /// 0x800566E8: 0..n from both streams, then the LCG steps.
    fn randint(&mut self, n: i32) -> i32 {
        let r = self.rand() as u32;
        let v = n.wrapping_mul((r.wrapping_add(self.lcg) & 0x7FFF) as i32);
        self.lcg();
        (if v < 0 { v + 0x7FFF } else { v }) >> 15
    }

    /// A profile test: `(lcg & 0xFFF) < p`.
    fn chance(&mut self, p: i32) -> bool {
        ((self.lcg() & 0xFFF) as i32) < p
    }
}

/// The record fields of a move the AI reads (`rec4` posture word, `rec8` attack word, `as`/`ae` active window,
/// `js`/`je` jump window, `w14` the signed damage word, `f24` flags), with the first raw entry of its list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rec {
    id: Option<Current>,
    rec4: u32,
    rec8: u32,
    as_: i32,
    ae: i32,
    js: i32,
    je: i32,
    w14: i32,
    f24: u32,
    next_alias: usize,
    desc0: u32,
    l0_cmd: u32,
    l0_cond: u32,
    l0_we: i32,
}

impl Rec {
    fn of(gd: &GameData, f: &Fighter, to: Current) -> Option<Self> {
        let set = f.moves(gd, to.bank);
        let m: &Move = set.moves.get(to.index)?;
        let l0 = set.cancels.get(m.cancel_start);
        Some(Self {
            id: Some(to),
            rec4: m.posture.raw,
            rec8: m.attack_mask.raw,
            as_: m.active[0] as i32,
            ae: m.active[1] as i32,
            js: m.bytes_18.jump_start as i32,
            je: m.bytes_18.jump_end as i32,
            w14: (((m.damage_type << 14) | (m.damage as u32 & 0x3FFF)) as u16 as i16) as i32,
            f24: m.flags24.raw,
            next_alias: m.next.alias,
            desc0: m.desc0.unwrap_or(0),
            l0_cmd: l0.map_or(0xC000, |c| c.cmd),
            l0_cond: l0.map_or(0, |c| c.cond),
            l0_we: l0.map_or(0, |c| c.win_end as i32),
        })
    }

    fn low16(&self) -> u32 {
        self.rec8 & 0xFFFF
    }

    /// The attack word's range class: no range bit counts as close + mid.
    fn range_class(&self) -> u32 {
        (self.rec8 & 0xFFFF_0000) | if self.rec8 & 0x1C_0000 == 0 { 0xC_0000 } else { 0 }
    }
}

/// A command entry (12 bytes): command, gate, condition, destination alias, transition, window, extra byte
/// (for a CALL: the shared sub-list's length).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ent {
    cmd: u32,
    req: u32,
    cond: u32,
    param: u32,
    dest: usize,
    trans: u32,
    ws: i32,
    we: i32,
    extra: usize,
}

impl Ent {
    fn of(c: &Cancel) -> Self {
        let cmd = match c.kind {
            CancelKind::End => 0xC000,
            _ => c.cmd,
        };
        Self { cmd, req: c.req, cond: c.cond, param: c.param, dest: c.dest, trans: c.transition, ws: c.win_start as i32, we: c.win_end as i32, extra: c.exec as usize }
    }
}

/// A candidate (table 0x8009F978): the move an entry leads to, the entry, 0 candidate / 1 in the pool /
/// -1 excluded.
#[derive(Clone, Copy, Debug)]
struct Cand {
    mv: Rec,
    e: Ent,
    state: i8,
}

/// What a CPU sees this frame.
pub struct Sight<'a> {
    pub gd: &'a GameData,
    pub me: &'a Fighter,
    pub op: &'a Fighter,
    /// The opponent is to the right on the pad (forward = right).
    pub facing_right: bool,
    /// The opponent's chest bone height above the floor (game units, `+0x994` with up positive).
    pub opp_chest: f32,
    /// Ball: the ball projected on the line between the fighters (record 8 0x800B4494).
    pub ball: Option<crate::ball::BallSight>,
    /// A Force player seat: the stage around it (None: the game's CPU, the soldiers' too).
    pub force: Option<ForceSight<'a>>,
}

/// What a CPU Force player sees of the stage (OpenKumite's own: the game's Force player is never a CPU).
pub struct ForceSight<'a> {
    /// Every actor (players [`crate::Fight::is_player`], soldiers and bosses the others) and the way on
    /// (`screen_right`).
    pub fight: &'a crate::Fight,
    /// My actor index.
    pub me: usize,
    /// The roast chickens within reach (simulation positions).
    pub pickups: &'a [glam::Vec3],
}

/// Per-frame view of the two actors.
struct Cx<'a> {
    data: &'a AiData,
    gd: &'a GameData,
    me: &'a Fighter,
    op: &'a Fighter,
    rng: &'a mut AiRng,
    /// My and his current move.
    m: Rec,
    h: Rec,
    a: Angles,
    mf: i32,
    hf: i32,
    /// `+0xF8`: distance between the roots.
    dist: i32,
    me_air: bool,
    op_air: bool,
    my_moveset: i64,
    op_moveset: i64,
    opp_chest: f32,
    ball: Option<crate::ball::BallSight>,
}

impl Cx<'_> {
    fn lcg(&mut self) -> u32 {
        self.rng.lcg()
    }
    fn chance(&mut self, p: i32) -> bool {
        self.rng.chance(p)
    }
    fn randint(&mut self, n: i32) -> i32 {
        self.rng.randint(n)
    }
    fn rand(&mut self) -> i32 {
        self.rng.rand()
    }

    /// `rec(alias)`: the record an alias resolves to in my alias table.
    fn rec(&self, alias: usize) -> Option<Rec> {
        Rec::of(self.gd, self.me, self.me.resolve(alias)?)
    }

    fn in_window(&self, e: &Ent) -> bool {
        e.ws <= self.mf && self.mf <= e.we
    }

    /// Gate byte (0x8002DF00): my moveset is / is not, his moveset is, my character is / is not; 0x74.. never.
    fn req_ok(&self, e: &Ent) -> bool {
        let r = e.req as i64;
        let ch = self.me.character.unwrap_or(-1);
        match r {
            0 => true,
            0x01..=0x17 => self.my_moveset == r - 1,
            0x18..=0x2E => self.my_moveset != r - 0x18,
            0x2F..=0x45 => self.op_moveset == r - 0x2F,
            0x46..=0x5C => ch == r - 0x46,
            0x5D..=0x73 => ch != r - 0x5D,
            _ => false,
        }
    }

    /// The engine's condition test (0x8002E0BC), as the player's command lists use it.
    fn engine_cond(&self, e: &Ent) -> bool {
        self.me.condition(self.gd, e.cond, e.param)
    }

    /// The top-level entries of a move's list up to and including its END entry (cmd 0xC000).
    fn top(&self, to: Current) -> Vec<Ent> {
        let set = self.me.moves(self.gd, to.bank);
        let Some(m) = set.moves.get(to.index) else { return vec![] };
        let mut out = Vec::new();
        for c in set.cancels.get(m.cancel_start..).into_iter().flatten() {
            out.push(Ent::of(c));
            if c.kind == CancelKind::End {
                break;
            }
        }
        out
    }

    /// The shared sub-list entry `k` (0x80017C78).
    fn shared(&self, k: usize) -> Option<Ent> {
        self.gd.shared.shared_cancels.entries.get(k).map(Ent::of)
    }

    /// A move's list with its CALL entries replaced by their shared sub-lists, up to the END entry.
    fn expanded(&self, to: Current) -> Vec<Ent> {
        let mut out = Vec::new();
        for t in self.top(to) {
            match t.cmd {
                0xC000 => break,
                0xC00C => out.extend((t.dest..t.dest + t.extra).filter_map(|k| self.shared(k))),
                _ => out.push(t),
            }
        }
        out
    }

    fn my_list(&self) -> Vec<Ent> {
        self.expanded(self.me.current)
    }

    /// My move `to` puts him in the air: its reaction on him standing in front is airborne (a launcher).
    fn launches(&self, to: Option<Current>) -> bool {
        let Some(m) = to.and_then(|to| self.me.moves(self.gd, to.bank).moves.get(to.index)) else { return false };
        let victim = if m.reaction.is_throw() { None } else { self.gd.reactions.entries.get(m.reaction.index).and_then(|e| e.victims.first()) };
        victim.and_then(|&a| Rec::of(self.gd, self.op, self.op.resolve(a)?)).is_some_and(|v| v.rec4 & 0x400 != 0)
    }

    /// He is in a throw the victim always tries to escape (and the evade plans react to).
    fn escapable(&self) -> bool {
        self.data.escapable(self.op_moveset, self.op.alias)
    }
}

/// Direction pad words by numpad digit (0x80023050), side-0 frame: 0x2000 forward, 0x8000 back.
const DIRPAD: [u16; 10] = [0, 0xC000, 0x4000, 0x6000, 0x8000, 0, 0x2000, 0x9000, 0x1000, 0x3000];
/// Button pad bits of buttons 1..4 (0x80023064).
const BTN: [u16; 4] = [0x80, 0x10, 0x40, 0x20];
/// The CPU output word of a step word (0x800617DC: table 0x8002301C; the "any of" bit is ignored).
fn word_to_pad(w: u16) -> u16 {
    let d = DIRPAD.get((w & 0xF) as usize).copied().unwrap_or(0);
    (0..4).filter(|b| w & (0x100 << b) != 0).fold(d, |p, b| p | BTN[b])
}

/// The pad of a CPU output word (forward = towards the opponent).
fn to_pad(out: u16, facing_right: bool) -> Pad {
    let (fwd, back) = (out & 0x2000 != 0, out & 0x8000 != 0);
    let (right, left) = if facing_right { (fwd, back) } else { (back, fwd) };
    let buttons = BTN.iter().enumerate().filter(|(_, &b)| out & b != 0).fold(0u8, |a, (k, _)| a | 1 << k);
    Pad { up: out & 0x1000 != 0, down: out & 0x4000 != 0, left, right, buttons, start: false }
}

/// A CPU Force player's own moves over the routine (OpenKumite's own; None: the routine fights its foe): a chicken
/// when hurt (the lowest player hurt takes the nearest), on along the stage while no soldier is near (alone along
/// its middle line, in co-op a lane either side of it and no further than 600 ahead of the partner), out to the side from between two soldiers
/// still coming, to a soldier nobody takes on while the partner has my foe. Down or in the air, the routine gets
/// up. Draws nothing from the random streams.
fn force_move(sight: &Sight, s: &ForceSight) -> Option<Pad> {
    use crate::Fight;
    let (me, f) = (sight.me, s.fight);
    if me.current_move(sight.gd).posture.raw & 0x204 != 0 || me.airborne_flag(sight.gd) {
        return None;
    }
    let p = me.root();
    let flat = |v: glam::Vec3| glam::Vec3::new(v.x, 0.0, v.z);
    let dist2 = |a: glam::Vec3, b: glam::Vec3| flat(a - b).length_squared();
    let standing = |i: usize| f.actor(i).filter(|a| !a.inactive && a.health > 0);
    let pos = |i: usize| f.actor(i).map_or(p, Fighter::root);
    // a chicken (+50) when 50 down: the lowest of the players hurt takes its nearest first
    let hurt = |a: &Fighter| a.health <= (a.max_health - 50) * ONE;
    if hurt(me) {
        let mut players: Vec<usize> = (0..f.actor_count()).filter(|&i| Fight::is_player(i) && standing(i).is_some_and(hurt)).collect();
        players.sort_by_key(|&i| (standing(i).map_or(0, |a| a.health), i));
        // (one that cannot sidestep only reaches the chickens on its own line, within the pickup's 500)
        let deep = |c: &glam::Vec3| flat(*c - p).dot(glam::Vec3::Y.cross(f.screen_right)).abs();
        let mut left: Vec<glam::Vec3> = s.pickups.iter().copied().filter(|c| sidesteps(me, sight.gd) || deep(c) < 400.0).collect();
        for i in players {
            let Some(k) = (0..left.len()).min_by(|&a, &b| dist2(left[a], pos(i)).total_cmp(&dist2(left[b], pos(i)))) else { break };
            let c = left.swap_remove(k);
            if i == s.me {
                return Some(force_go(me, c, f));
            }
        }
    }
    let near: Vec<usize> = (0..f.actor_count()).filter(|&i| !Fight::is_player(i) && standing(i).is_some() && dist2(pos(i), p) < 2500.0 * 2500.0).collect();
    let partners: Vec<usize> = (0..f.actor_count()).filter(|&i| i != s.me && Fight::is_player(i) && standing(i).is_some()).collect();
    if near.is_empty() || sight.op.inactive {
        let ahead = |q: glam::Vec3| q.dot(f.screen_right);
        let on = partners.iter().map(|&i| ahead(pos(i)) + 600.0 - ahead(p)).fold(1500.0, f32::min);
        // (along the middle line: the players stay within the camera's view, narrow at the stage's front and back,
        // so a walk on by an edge stops at the view's side for good; in co-op a lane either side of it, 1P's and 2P's)
        let lane = if partners.is_empty() {
            0.0
        } else if s.me == 0 {
            -400.0
        } else {
            400.0
        };
        let from = glam::Vec3::new(p.x, 0.0, lane);
        return Some(force_go(me, from + f.screen_right * on, f));
    }
    // the two nearest soldiers on either side, both still over 700 away: out of their line, so both come from
    // in front (by the stage's edge, back towards its middle)
    let mut close: Vec<usize> = near.iter().copied().filter(|&i| dist2(pos(i), p) < 1800.0 * 1800.0).collect();
    close.sort_by(|&a, &b| dist2(pos(a), p).total_cmp(&dist2(pos(b), p)));
    if let [a, b, ..] = close[..] {
        let (da, db) = (flat(pos(a) - p), flat(pos(b) - p));
        if da.dot(db) < -0.5 * da.length() * db.length() && da.length() > 700.0 {
            let side = flat(pos(b) - pos(a)).cross(glam::Vec3::Y);
            let away = if p.z.abs() > 1200.0 { -p.z * side.z } else { flat(p - pos(a)).dot(side) };
            let side = if away >= 0.0 { side } else { -side };
            return Some(force_go(me, p + side * (800.0 / side.length().max(1.0)), f));
        }
    }
    // co-op: a partner nearer my foe takes it on while a soldier near is nobody's: that one is mine
    let foe = f.opponent_index(s.me);
    if partners.iter().any(|&i| f.opponent_index(i) == foe && dist2(pos(i), pos(foe)) < dist2(p, pos(foe))) {
        let free = near.iter().copied().filter(|&i| i != foe && partners.iter().all(|&j| f.opponent_index(j) != i));
        if let Some(i) = free.min_by(|&a, &b| dist2(pos(a), p).total_cmp(&dist2(pos(b), p))) {
            return Some(force_go(me, pos(i), f));
        }
    }
    None
}

/// Tekken 2's movesets (their own moves `t2…`) have no sidestep: up jumps, down crouches; they only walk along.
pub fn sidesteps(me: &Fighter, gd: &GameData) -> bool {
    !me.moves(gd, crate::data::Bank::Own).moves.first().is_some_and(|m| m.anim.starts_with("t2"))
}

/// The pad that takes a Force player towards `to`: walks and runs along the stage, sidesteps (a tap every
/// sixth frame: held up jumps, held down crouches) across it.
fn force_go(me: &Fighter, to: glam::Vec3, f: &crate::Fight) -> Pad {
    let d = to - me.root();
    // (the screen's right and into the screen: up)
    let (along, deep) = (d.dot(f.screen_right), d.dot(glam::Vec3::Y.cross(f.screen_right)));
    if sidesteps(me, &f.data) && deep.abs() > 300.0 && deep.abs() > along.abs() * 0.5 {
        let tap = f.frame.is_multiple_of(6);
        return Pad { up: tap && deep > 0.0, down: tap && deep < 0.0, ..Pad::default() };
    }
    Pad { right: along > 100.0, left: along < -100.0, ..Pad::default() }
}

/// Where control goes after a block of the routine: the output (`LAB_80060E6C`) or the movement chooser.
enum Flow {
    Fin,
    Movement,
}

/// One CPU's state block (0x8009F318 + slot * 0x330): kept across rounds; the round setup resets part of it.
#[derive(Clone)]
pub struct Ai {
    /// Global difficulty (0x800AE208) and level (0x800AE211) the profile is taken from.
    pub difficulty: usize,
    pub level: usize,
    /// How it plays on top of the game's routine (OpenKumite's own; [`Style::Classic`]: as the game).
    style: Style,
    /// The style played now (a Mixed seat's current pick) and, for Mixed, the frames until the next pick.
    playing: Style,
    mixt: i32,
    /// Statistics: chain-throw continuations and reaction picks (4.M / 4.O), sidesteps started.
    pub followups: u32,
    pub sidesteps: u32,
    /// The round setup (0x80059654) has to run before the next frame.
    pending: bool,
    /// +0x00: the actor slot.
    slot: i32,
    p: Profile,
    /// +0x02: frames my frame number changed (bit 0 is a coin).
    ticks: i32,
    /// +0x04 / +0x06: the output word (persists: a frame that writes nothing repeats it) and the last one.
    out: u16,
    prev_out: u16,
    /// +0x08: the canned / motion input being played, one word a frame.
    seq: VecDeque<u16>,
    /// +0x14 / +0x18: my zone (0..5, "can I reach him"), his zone (1..5).
    myzone: i32,
    hiszone: i32,
    /// +0x1C / +0x20 / +0x24: distance, last frame's (+ Ball), the change.
    dist: i32,
    dist_prev: i32,
    ddelta: i32,
    /// +0x28: my move changed (or its frame went back) this frame.
    mchg: bool,
    /// +0x2A: accumulator while in zone 4 / 5.
    faracc: i32,
    /// +0x2C: frames in the current move.
    inmove: i32,
    /// +0x2E: my facing off the direction to him (+0x30 of the actor).
    turn: i32,
    hp_prev: i32,
    /// +0x34: my frame last frame.
    fprev: i32,
    /// +0x38 walk timer, +0x3A run (2 = running), +0x3C crouching last frame, +0x40 movement cooldown,
    /// +0x42 combo prelude cooldown, +0x44 crouch dash, +0x46 jump attack, +0x48 hold direction, +0x4A crouch
    /// timer, +0x4C hold the crouch after a crouching pick.
    walk38: i32,
    run3a: i32,
    crouchf: bool,
    mvcd40: i32,
    combocd: i32,
    cdash: i32,
    jumpt: i32,
    hold48: i32,
    croucht: i32,
    chold: i32,
    /// +0x4E: my frame (for `mchg`).
    frame_prev: i32,
    /// +0x50: my move is an attack; +0x52: a stance move before its end; +0x54: 30 frames after my attack whiffed.
    atk: bool,
    stance: bool,
    whiff54: i32,
    /// +0x58: my move last frame.
    move_prev: Option<Current>,
    /// +0x5C: the move of the last command input this frame.
    last_rec: Option<Rec>,
    /// +0x64: the condition shortcut bits (back turned, sides, a high coming, opponent in front).
    bits64: u32,
    /// +0x68 attack cooldown, +0x6A already guarded, +0x6C delayed punish, +0x6E wake tap timer.
    cd: i32,
    safe: bool,
    pwait: i32,
    waket: i32,
    /// +0x70 / +0x72 and the table 0x8009F978: candidates and the pool count.
    cand: Vec<Cand>,
    npool: i32,
    /// +0x74 / +0x78: handlers for his moveset and mine (`AiData::routines`); +0x7C: the low / ground move filter
    /// (0x800613CC).
    h_vs: Option<usize>,
    h_self: Option<usize>,
    filter: bool,
    /// +0x80: frames he is in start-up (more than 3 before active); +0x84: frames I attack while he recovers.
    hstart: i32,
    mrec: i32,
    /// +0x88: he is in his stance (ai.json `rules.stance`); +0x8A wake lock / handler gate; +0x8C handler cooldown.
    op_stance: bool,
    wlock: i32,
    hcd: i32,
    /// +0x8E / +0x8F: attack now (next frame / this frame). (+0x90 is never set, so +0x91 is always 0.)
    now_n: bool,
    now: bool,
    /// +0x92 guard latch, +0x94 evade plan.
    glatch: u8,
    evade: u8,
    /// +0x96 chain-throw stage, +0x9C the frame the chain entry is input, +0xA0 the move decided on, +0xBC the
    /// entry.
    chain: i32,
    chain_fire: i32,
    chain_move: Option<Current>,
    chain_entry: Option<Ent>,
    /// +0xC0 / +0xC2, +0xD8 / +0xDA, +0x1A0 / +0x1A2: follow-up lists C (string starters), B, D (flag, count);
    /// their executors never fire, the collection still holds frames. +0xD4: exclude string starters.
    lc_c: i32,
    nc_c: i32,
    lc_b: i32,
    nc_b: i32,
    lc_d: i32,
    nc_d: i32,
    nostart: bool,
    /// +0x1D2 / +0x1D4: my attacks that reached their first active frame, and the limit.
    natk: i32,
    natk_max: i32,
    /// +0x1DC / +0x1FC: his moves that took HP from me (ring of 8).
    ring: [Option<Current>; 8],
    ringi: usize,
    /// +0x1FE rage timer, +0x202 rage.
    raget: i32,
    rage: bool,
    /// +0x200 / +0x201: the two fire characters in a fire move (descriptor byte 0x18 / 0x1A).
    fire18: bool,
    fire1a: bool,
    /// +0x208: he is down and hittable.
    odown: bool,
    /// +0x20A frames until his attack is active (999 none), +0x20C it is coming, +0x20E frames in a hop,
    /// +0x214 his throw is not over, +0x216 his attacks in a row (-1 idle, cap 10), +0x21A he is high in the air.
    until: i32,
    inc: bool,
    hopt: i32,
    hthrow: bool,
    hseq: i32,
    ohigh: bool,
    /// +0x220.. my and his record words: rec8, rec4, low16(rec8) (his rec8 with 0x10000 for the second fire character's
    /// fire).
    m8: u32,
    m4: u32,
    mmask: u32,
    h8: u32,
    h4: u32,
    hmask: u32,
    /// +0x314 / +0x320: reach thresholds of my and his character.
    thr_me: [i32; 6],
    thr_op: [i32; 6],
}

/// How an AI seat plays, on top of the game's routine (OpenKumite's own: a demo's or a test's players). Each
/// leans the routine's choices its way; [`Style::Classic`] is the game's CPU as it is. Deterministic: whatever it
/// draws comes from the AI's own random streams, so a replay plays it again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Style {
    /// The game's CPU, unchanged.
    #[default]
    Classic,
    /// Closes in and attacks all the time: short cooldowns, strings, little guarding.
    Rushdown,
    /// Guards, waits for whiffs and punishes them.
    Defensive,
    /// Baits and counter-hits: steps in and out of reach, attacks into the other's start-up.
    Counter,
    /// Throws whenever in reach, escapes throws.
    Thrower,
    /// Sidesteps and moves around, attacks from the side and the back.
    Evasive,
    /// Launches and juggles, presses a fallen opponent.
    Juggler,
    /// Changes style every so often within a match.
    Mixed,
}

impl Style {
    pub const ALL: [Style; 8] = [Style::Classic, Style::Rushdown, Style::Defensive, Style::Counter, Style::Thrower, Style::Evasive, Style::Juggler, Style::Mixed];

    /// Its name on the command line and in a replay's title.
    pub fn name(self) -> &'static str {
        match self {
            Style::Classic => "classic",
            Style::Rushdown => "rushdown",
            Style::Defensive => "defensive",
            Style::Counter => "counter",
            Style::Thrower => "thrower",
            Style::Evasive => "evasive",
            Style::Juggler => "juggler",
            Style::Mixed => "mixed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.name().eq_ignore_ascii_case(s.trim()))
    }
}

impl Ai {
    /// This AI playing `style`.
    pub fn with_style(mut self, style: Style) -> Self {
        (self.style, self.playing) = (style, style);
        self
    }

    pub fn style(&self) -> Style {
        self.style
    }

    /// A CPU player in actor slot `slot` (the state block is zero at boot); `difficulty` 0..2, `level` 0..9.
    pub fn new(difficulty: usize, level: usize, slot: usize) -> Self {
        Self {
            difficulty,
            level,
            style: Style::Classic,
            playing: Style::Classic,
            mixt: 0,
            followups: 0,
            sidesteps: 0,
            pending: true,
            slot: slot as i32,
            p: Profile::default(),
            ticks: 0,
            out: 0,
            prev_out: 0,
            seq: VecDeque::new(),
            myzone: 0,
            hiszone: 0,
            dist: 0,
            dist_prev: 0,
            ddelta: 0,
            mchg: false,
            faracc: 0,
            inmove: 0,
            turn: 0,
            hp_prev: 0,
            fprev: 0,
            walk38: 0,
            run3a: 0,
            crouchf: false,
            mvcd40: 0,
            combocd: 0,
            cdash: 0,
            jumpt: 0,
            hold48: 0,
            croucht: 0,
            chold: 0,
            frame_prev: 0,
            atk: false,
            stance: false,
            whiff54: 0,
            move_prev: None,
            last_rec: None,
            bits64: 0,
            cd: 0,
            safe: false,
            pwait: 0,
            waket: 0,
            cand: Vec::with_capacity(180),
            npool: 0,
            h_vs: None,
            h_self: None,
            filter: false,
            hstart: 0,
            mrec: 0,
            op_stance: false,
            wlock: 0,
            hcd: 0,
            now_n: false,
            now: false,
            glatch: 0,
            evade: 0,
            chain: 0,
            chain_fire: 0,
            chain_move: None,
            chain_entry: None,
            lc_c: 0,
            nc_c: 0,
            lc_b: 0,
            nc_b: 0,
            lc_d: 0,
            nc_d: 0,
            nostart: false,
            natk: 0,
            natk_max: 0,
            ring: [None; 8],
            ringi: 0,
            raget: 0,
            rage: false,
            fire18: false,
            fire1a: false,
            odown: false,
            until: 0,
            inc: false,
            hopt: 0,
            hthrow: false,
            hseq: 0,
            ohigh: false,
            m8: 0,
            m4: 0,
            mmask: 0,
            h8: 0,
            h4: 0,
            hmask: 0,
            thr_me: [0; 6],
            thr_op: [0; 6],
        }
    }

    /// A new round: the setup runs before the CPU's next frame.
    pub fn new_round(&mut self) {
        self.pending = true;
    }

    /// The round setup (0x80059654 -> 0x800593C8) if one is pending. The game sets up both CPUs (slot order)
    /// before either thinks: call this for both, then [`Ai::think`].
    pub fn prepare(&mut self, data: &AiData, rng: &mut AiRng, s: &Sight) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        self.ticks = self.slot;
        self.out = 0;
        self.prev_out = 0;
        self.fprev = -1;
        self.crouchf = false;
        self.croucht = -1;
        self.chold = 0;
        self.wlock = -1;
        self.hold48 = -1;
        self.walk38 = -1;
        self.run3a = 0;
        self.move_prev = None;
        self.chain_move = None;
        self.whiff54 = -1;
        self.cdash = -1;
        self.jumpt = -1;
        self.frame_prev = -1;
        self.inmove = 0;
        self.faracc = 0;
        self.combocd = -1;
        self.hcd = -1;
        self.raget = -1;
        self.pwait = -1;
        self.safe = false;
        self.npool = 0;
        self.cand.clear();
        self.lc_c = 0;
        self.nc_c = 0;
        self.hp_prev = s.me.health;
        self.nostart = false;
        self.lc_b = 0;
        self.nc_b = 0;
        self.lc_d = 0;
        self.nc_d = 0;
        self.natk = 0;
        self.chain = 0;
        self.chain_fire = -1;
        self.chain_entry = None;
        self.waket = 0;
        self.seq.clear();
        self.hseq = -1;
        self.ringi = 0;
        self.ring = [None; 8];
        (self.now_n, self.now, self.glatch, self.evade) = (false, false, 0, 0);
        if self.style == Style::Mixed {
            self.mix(rng);
        }
        self.load_profile(data, s.me, s.op, -1, -1);
        self.h_vs = data.handlers_of(s.op.moveset.moveset_id).0;
        self.h_self = data.handlers_of(s.me.moveset.moveset_id).1;
        self.cd = self.p.attack_cooldown_0;
        self.mvcd40 = 30 + rng.randint(30);
    }

    /// 0x80061488: the profile copy for (difficulty, level); arguments below 1 take the global ones.
    fn load_profile(&mut self, data: &AiData, me: &Fighter, op: &Fighter, diff: i32, level: i32) {
        let d = if diff < 1 { self.difficulty as i32 } else { diff };
        let l = if level < 1 { self.level as i32 } else { level };
        self.p = data.profile(d.clamp(0, 2) as usize, l.clamp(0, 9) as usize).clone();
        self.thr_me = data.thresholds(me.character);
        self.thr_op = data.thresholds(op.character);
        self.natk_max = self.p.consecutive_attack_limit_a.max(self.p.consecutive_attack_limit_b);
        self.lean();
    }

    /// Mixed: the next style to play (one of the others) and for how long (5..10 s).
    fn mix(&mut self, rng: &mut AiRng) {
        self.playing = Style::ALL.get(1 + rng.randint(6) as usize).copied().unwrap_or_default();
        self.mixt = 300 + rng.randint(300);
    }

    /// The style's lean on the profile copy (OpenKumite's own; Classic keeps the game's).
    fn lean(&mut self) {
        let p = &mut self.p;
        let at_least = |v: &mut i32, to: i32| *v = (*v).max(to);
        match self.playing {
            Style::Classic | Style::Mixed => {}
            Style::Rushdown => {
                // short cooldowns (none in a quarter: the next string part at once), long strings, never backing off
                (p.attack_cooldown_base, p.attack_cooldown_0) = (0, 0);
                for c in [&mut p.attack_cooldown_1, &mut p.attack_cooldown_2, &mut p.attack_cooldown_3] {
                    *c /= 3;
                }
                at_least(&mut p.fast_poke_prob, 1024);
                at_least(&mut p.guard_aware_attack_prob, 1024);
                at_least(&mut p.pressure_on_stun_prob, 4096);
                p.keep_walk_back_prob = 0;
                self.natk_max += 6;
            }
            Style::Defensive => {
                // guard nearly everything, punish whiffs, attack less
                for g in [&mut p.guard_prob, &mut p.guard_prob_cap] {
                    at_least(g, 2867);
                }
                for g in [&mut p.in_string_guard_prob, &mut p.in_string_guard_cap] {
                    at_least(g, 3891);
                }
                for c in [&mut p.counter_attack_prob, &mut p.counter_attack_cap] {
                    at_least(c, 2048);
                }
                at_least(&mut p.parry_prob, 409);
                at_least(&mut p.throw_escape_prob, 3072);
                at_least(&mut p.keep_walk_back_prob, 2048);
                p.throw_attempt_prob /= 2;
                for c in [&mut p.attack_cooldown_0, &mut p.attack_cooldown_1, &mut p.attack_cooldown_2, &mut p.attack_cooldown_3] {
                    *c *= 2;
                }
            }
            Style::Counter => {
                // interrupt his attacks and strings, parry, rarely start anything
                for c in [&mut p.counter_attack_prob, &mut p.counter_attack_cap] {
                    at_least(c, 2048);
                }
                for c in [&mut p.in_string_interrupt_prob, &mut p.in_string_interrupt_cap] {
                    at_least(c, 3072);
                }
                at_least(&mut p.parry_prob, 1024);
                at_least(&mut p.keep_walk_back_prob, 4096);
                for c in [&mut p.attack_cooldown_0, &mut p.attack_cooldown_1, &mut p.attack_cooldown_2, &mut p.attack_cooldown_3] {
                    *c *= 2;
                }
            }
            Style::Thrower => {
                // throw whenever in reach, escape every throw, chain throws to their end
                p.throw_attempt_prob = 4096;
                p.throw_escape_prob = 4096;
                p.string_continue_prob = 4096;
                at_least(&mut p.throw_select_prob, 2867);
                p.max_string_length += 2;
            }
            Style::Evasive => {
                // sidestep his attacks, evade, keep moving; guard what is not dodged
                p.sidestep_prob = 4096;
                p.evade_and_handler_prob = 4096;
                at_least(&mut p.keep_walk_back_prob, 1024);
                for g in [&mut p.guard_prob, &mut p.guard_prob_cap] {
                    at_least(g, 1024);
                }
            }
            Style::Juggler => {
                // guard and punish (with a launcher), keep a stunned or fallen opponent under pressure, long juggles
                for g in [&mut p.guard_prob, &mut p.guard_prob_cap] {
                    at_least(g, 2048);
                }
                for c in [&mut p.counter_attack_prob, &mut p.counter_attack_cap] {
                    at_least(c, 1024);
                }
                p.pressure_on_stun_prob = 4096;
                self.natk_max += 4;
            }
        }
    }

    /// The pad for this frame (0x80059890). The game calls it only while the CPU has HP left.
    pub fn think(&mut self, data: &AiData, rng: &mut AiRng, s: &Sight) -> Pad {
        self.prepare(data, rng, s);
        if let Some(pad) = s.force.as_ref().and_then(|f| force_move(s, f)) {
            return pad;
        }
        let (gd, me, op) = (s.gd, s.me, s.op);
        let (m, h) = (Rec::of(gd, me, me.current).unwrap_or_default(), Rec::of(gd, op, op.current).unwrap_or_default());
        let d = op.root() - me.root();
        let mut cx = Cx {
            data,
            gd,
            me,
            op,
            rng,
            m,
            h,
            a: me.angles(),
            mf: me.frame as i32,
            hf: op.frame as i32,
            dist: (d.x * d.x + d.z * d.z).sqrt() as i32,
            me_air: me.airborne_flag(gd),
            op_air: op.airborne_flag(gd),
            my_moveset: me.moveset.moveset_id.unwrap_or(-1),
            op_moveset: op.moveset.moveset_id.unwrap_or(-1),
            opp_chest: s.opp_chest,
            ball: s.ball,
        };
        self.run(&mut cx);
        self.post(&mut cx);
        to_pad(self.out, s.facing_right)
    }

    /// 0x80056748: stop walking / running / holding.
    fn reset_mv(&mut self) {
        self.mvcd40 = 0x28;
        self.run3a = 0;
        self.walk38 = 0;
        self.hold48 = -1;
    }

    /// 0x80059890 up to the output block (every `return` = `LAB_80060E6C`).
    fn run(&mut self, cx: &mut Cx) {
        let (m, h) = (cx.m, cx.h);
        // ---- 4.A bookkeeping [0x800598C4..0x80059B04]
        self.last_rec = None;
        if cx.mf != self.fprev {
            self.ticks += 1;
        }
        self.fprev = cx.mf;
        (self.m4, self.m8, self.mmask) = (m.rec4, m.rec8, m.low16());
        (self.h4, self.h8, self.hmask) = (h.rec4, h.rec8, h.low16());
        self.dist = cx.dist;
        let a = cx.a;
        let mut b = (a.turn > 0x4000) as u32;
        self.turn = a.turn;
        if (a.off as u16).wrapping_sub(0x4E38) < 0x31C8 {
            b |= 2;
        }
        if ((a.off + 0x8000) & 0xFFFF) < 0x31C7 {
            b |= 4;
        }
        // (0x800AFA3C, the distance the camera keeps)
        if h.low16() == 0x412 && cx.hf <= h.ae && cx.dist < 0x700 {
            b |= 8;
        }
        if a.quadrant == 0 {
            b |= 0x10;
        }
        (self.fire18, self.fire1a, self.bits64) = (false, false, b);
        let r = &cx.data.rules;
        if cx.op_moveset == r.fire.moveset && h.desc0 == r.fire.descriptor {
            self.fire18 = true;
        } else if cx.op_moveset == r.low_fire.moveset && h.desc0 == r.low_fire.descriptor {
            self.fire1a = true;
            self.h8 |= 0x10000;
        }

        // ---- 4.B a canned / motion input being played pre-empts everything
        if let Some(w) = self.seq.pop_front() {
            self.out = word_to_pad(w);
            return;
        }
        // ---- 4.C (modes 0 / 1, demo frame skip, Force hold: not used here)

        // ---- 4.D bookkeeping 2 [0x80059BD0..0x8005A6A0]
        let (mas, mae, mf) = (m.as_, m.ae, cx.mf);
        let (has, hae, hf) = (h.as_, h.ae, cx.hf);
        self.filter = false;
        if m.f24 & 0x2000 == 0 {
            self.nostart = false;
        }
        self.now = std::mem::take(&mut self.now_n);
        if cx.op.health <= cx.op.max_health * ONE / 10 {
            // he is nearly out: no pokes, no counters
            (self.p.guard_aware_attack_prob, self.p.fast_poke_prob, self.p.counter_attack_prob, self.p.counter_attack_cap) = (0, 0, 0, 0);
        }
        if (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero && self.turn > 0x1FFF && a.opp_turn < 0x6001 && self.hiszone < 3 {
            self.now = true;
        }
        // Evasive: at his side or back, near: attack now
        if self.playing == Style::Evasive && a.opp_quadrant != 0 && self.hiszone < 3 {
            self.now = true;
        }
        self.mchg = m.id != self.move_prev || mf < self.frame_prev;
        self.inmove += 1;
        if self.move_prev != m.id {
            self.move_prev = m.id;
            self.inmove = 0;
        }
        self.frame_prev = mf;
        if self.m4 & 0x40_0000 != 0 {
            self.reset_mv();
        }
        // my attacks that reached their first active frame
        if mas == 0 {
            if self.m8 & 0x8_0000 == 0 && !cx.op.struck_in_move {
                self.natk = 0;
            }
        } else if mas == mf {
            self.natk += 1;
        }
        // his attacks in a row (my +0x87)
        if has == 0 {
            if self.h8 & 0x8_0000 == 0 && !cx.me.struck_in_move {
                self.hseq = -1;
            }
        } else if has == hf {
            self.hseq = (self.hseq + 1).min(10);
        }
        // distance and my zone; in Ball the distance to the ball when that is closer (0x800B4644)
        let mut d = self.dist;
        if let Some(b) = cx.ball {
            let bd = b.distance(usize::from(cx.me.side > 0));
            if bd < d {
                d = bd;
            }
        }
        self.ddelta = d - self.dist_prev;
        self.dist_prev = d;
        let r = d - self.thr_op[5] - 0x80;
        if r < self.thr_me[0] {
            if !cx.me.pushed && !cx.op.pushed {
                self.myzone = 1;
            } else {
                self.myzone = 0;
                if !self.atk {
                    self.cd -= 1;
                }
            }
            if !self.atk {
                self.cd -= 1;
            }
            self.faracc = 0;
        } else if r < self.thr_me[1] {
            (self.myzone, self.faracc) = (2, 0);
        } else if r < self.thr_me[2] {
            (self.myzone, self.faracc) = (3, 0);
        } else if r < self.thr_me[3] {
            self.myzone = 4;
            let k = cx.randint(2);
            self.faracc += 4 + k;
        } else {
            self.myzone = 5;
            let k = cx.randint(3);
            self.faracc += 5 + k;
        }
        // (`atk` is still last frame's)
        if !self.atk || cx.op_air {
            self.cd -= 1;
        }
        // his zone
        let mut r2 = self.dist - self.thr_me[5];
        if self.ddelta < 0 {
            r2 += self.ddelta;
        }
        let t = &self.thr_op;
        self.hiszone = if r2 < t[0] + 0x80 {
            1
        } else if r2 < t[1] + 0x100 {
            2
        } else if r2 < t[2] + 0x200 {
            3
        } else if r2 < t[3] + 0x280 {
            4
        } else {
            5
        };
        // his attack timing
        if has != 0 || self.h4 & 0x8_0000 == 0 {
            self.hopt = 0;
        } else {
            self.hopt += 1;
            if h.next_alias == cx.op.alias {
                self.hopt = 0;
            }
        }
        self.inc = self.hopt > 0 || (has != 0 && hf <= hae + 1);
        self.hstart = if !self.inc || has - 3 <= hf { 0 } else { self.hstart + 1 };
        self.mrec = if mas == 0 || hf <= hae + 1 { 0 } else { self.mrec + 1 };
        if !self.inc {
            self.pwait = -1;
        } else if self.pwait > 0 {
            self.pwait -= 1;
        }
        if !self.inc && self.h4 & 0x20_0000 != 0 {
            self.inc = true;
        }
        self.hthrow = self.h8 & 0x40_0000 != 0 && (has == 0 || hf <= hae);
        if !self.inc && !self.hthrow {
            self.until = 999;
            self.glatch = 0;
        } else if has == 0 {
            // the first raw entry of his list
            self.until = h.l0_we - hf + 1;
        } else if hf <= hae && ((has - hf) as i16) < 0x65 {
            self.until = has - hf;
        } else {
            self.until = if hf <= hae { 0x3E6 } else { 0x3E5 };
            self.hthrow = false;
            self.inc = false;
        }
        self.ohigh = cx.opp_chest > 0xC1B as f32;
        if a.opp_turn > 0x3000 || (self.h4 & 0x20_0000 == 0 && self.hmask != 0x607 && self.hmask != 0x706) {
            self.evade = 0;
        }
        if self.whiff54 >= 0 && self.hiszone < 2 && (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero {
            self.glatch = 1;
        }
        self.odown = self.h4 & 0x204 != 0 && self.h4 & 0xFFFF != 0x4C02 && !cx.op_air;
        self.op_stance = cx.op_moveset == cx.data.rules.stance.moveset && cx.op.alias == cx.data.rules.stance.alias;
        if self.op_stance {
            self.hseq = -1;
        }
        self.safe = self.already_safe();
        // crouch timer
        if self.m4 & 1 == 0 {
            self.croucht = -1;
        } else if self.crouchf {
            self.croucht -= 1;
        } else if m.f24 & 0x1000 == 0 {
            let (a, b, c) = (cx.lcg(), cx.lcg(), cx.lcg());
            self.croucht = ((a & 7) + (b & 0xF) + 20 + (c & 7)) as i32;
        }
        self.atk = mas != 0;
        self.stance = self.m4 & 0x20_0000 != 0 && self.m8 & 0x20_0000 != 0 && mf <= mae;

        // ---- 4.E throw chain stage (actor +0x74) [0x8005A6A0..0x8005AD54]
        if cx.me.chain == 0 {
            (self.chain_move, self.chain, self.chain_fire, self.chain_entry) = (None, 0, -1, None);
        } else {
            self.reset_mv();
            let same = m.id == self.chain_move;
            if cx.me.chain < 0 {
                // being thrown: one escape try per victim move, forced against some throws
                if same {
                    self.out = 0;
                    return;
                }
                self.chain -= 1;
                self.chain_move = m.id;
                if cx.escapable() || cx.chance(self.p.throw_escape_prob) {
                    let list: Vec<Ent> = cx.my_list().into_iter().take(48).collect();
                    let mut v = 0;
                    if !list.is_empty() {
                        let e = list[cx.randint(list.len() as i32) as usize];
                        v = match e.cond {
                            0x3A | 0x3C => 1,
                            0x3B => 2,
                            _ => e.cmd,
                        };
                    }
                    if v != 0 {
                        self.cmd_to_pad(cx, v, None);
                    } else {
                        self.out = 0;
                    }
                }
                return;
            }
            // the thrower: the next part of a chain throw, input at its window's start + 1
            if !same && self.chain >= 0 && self.chain < self.p.max_string_length - 1 && cx.chance(self.p.string_continue_prob) {
                let mut list = Vec::new();
                for e in cx.my_list() {
                    if !cx.req_ok(&e) {
                        continue;
                    }
                    let coin = cx.lcg() & 0x70;
                    if cx.data.chain_ok(e.dest, cx.op_moveset, coin) && (e.cond == 0 || cx.engine_cond(&e)) {
                        list.push(e);
                        if list.len() >= 6 {
                            break;
                        }
                    }
                }
                if list.is_empty() {
                    (self.chain, self.chain_fire, self.chain_entry) = (-1, -1, None);
                } else {
                    self.chain += 1;
                    self.chain_move = m.id;
                    let e = list[cx.randint(list.len() as i32) as usize];
                    self.chain_fire = e.ws + 1;
                    self.chain_entry = Some(e);
                }
            }
            match self.chain_entry {
                Some(e) if cx.mf == self.chain_fire => {
                    let r = cx.rec(e.dest);
                    self.cmd_to_pad(cx, e.cmd, r);
                    self.followups += 1;
                }
                _ => self.out = 0,
            }
            return;
        }

        // ---- 4.F stun recovery [0x8005AD58..0x8005AE94]
        if self.m4 & 0xFFFF == 0x4C02 && m.as_ == 0 {
            self.reset_mv();
            self.npool = 0;
            self.move_prev = m.id;
            self.build(cx, true);
            if !self.cand.is_empty() && cx.chance(self.p.stun_recovery_prob) && self.mark(|c| c.e.trans == 0x1C || c.e.trans == 0x1D) > 0 {
                self.pick(cx);
                return;
            }
            self.out = 0;
            return;
        }
        // a buffered move is pending: the output stays
        if cx.me.queued().is_some() {
            return;
        }

        // ---- 4.G lying / wake-up [0x8005AE94..0x8005B594]
        if !cx.me_air && self.m4 & 0x404 != 0 && self.wake_up(cx) {
            return;
        }

        // ---- 4.H per-character handlers
        if self.call_handler(cx, true) || self.call_handler(cx, false) {
            return;
        }

        // ---- 4.I whiff punish / hold when already guarded
        if cx.op.whiffed && (self.playing == Style::Defensive || ((cx.lcg() & 0xFFF) as i32) < self.p.counter_attack_prob + self.hseq * self.difficulty as i32 * 30) {
            self.now = true;
        } else if (self.pwait >= 0 || h.as_ == 0 || h.as_ - 30 < cx.hf) && self.safe {
            self.out = self.prev_out & 0xF000;
            return;
        }

        // ---- 4.J follow-up list collection (their executors never fire) [0x8005B648..0x8005BE14]
        self.npool = 0;
        self.cand.clear();
        let mut handled = false;
        if self.mchg {
            self.move_prev = m.id;
            self.frame_prev = cx.mf;
            if self.lc_c >= 1 {
                // list C: string starters, up to 4
                let mut n = 0;
                for e in cx.my_list() {
                    if cx.req_ok(&e) && cx.rec(e.dest).is_some_and(|r| r.f24 & 0x2000 != 0) {
                        n += 1;
                        if n >= 4 {
                            break;
                        }
                    }
                }
                self.nc_c = n;
                handled = true;
                if n < 1 {
                    (self.nc_c, self.lc_c) = (0, 0);
                } else {
                    self.out = self.prev_out & 0xF000;
                }
            } else if self.lc_b >= 1 {
                // list B, up to 48
                cx.lcg();
                let zm = cx.data.zm[self.myzone as usize];
                let mut n = 0;
                for e in cx.my_list() {
                    if !cx.req_ok(&e) {
                        continue;
                    }
                    let Some(r) = cx.rec(e.dest) else { continue };
                    if (self.odown && r.rec8 & 0x40_0000 != 0) || r.rec4 & 0x2_0000 != 0 {
                        continue;
                    }
                    if r.rec8 & 0x20_0000 == 0 && r.rec4 & 0x8_0000 == 0 {
                        if r.rec8 & 0x40_0000 == 0 {
                            continue;
                        }
                        if (cx.lcg() & 0x7FFF) as i32 <= self.p.throw_select_prob {
                            continue;
                        }
                    }
                    if r.range_class() & zm != 0 {
                        n += 1;
                        if n >= 48 {
                            break;
                        }
                    }
                }
                self.nc_b = n;
                handled = true;
                if n > 0 {
                    self.out = self.prev_out & 0xF000;
                } else {
                    (self.nc_b, self.lc_b) = (0, 0);
                }
            } else if !self.stance {
                if m.as_ == 0 {
                    (self.nc_d, self.nc_b, self.nc_c, self.lc_d, self.lc_b, self.lc_c) = (0, 0, 0, 0, 0, 0);
                } else {
                    // list D: entries open from frame 1 to their extra byte
                    let mut n = 0;
                    for e in cx.my_list() {
                        if e.ws == 1 && e.we == e.extra as i32 && cx.req_ok(&e) {
                            let starter = cx.rec(e.dest).is_some_and(|r| r.f24 & 0x2000 != 0);
                            if !starter || (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero {
                                n += 1;
                                if n >= 10 {
                                    break;
                                }
                            }
                        }
                    }
                    self.nc_d = n;
                    handled = true;
                    if n < 1 {
                        (self.nc_d, self.lc_d) = (0, 0);
                    } else {
                        self.out = self.prev_out & 0xF000;
                        self.lc_d += 1;
                    }
                }
            }
        }
        self.frame_prev = cx.mf;

        // ---- 4.K candidates; a move held for over 220 frames takes anything [0x8005BE14..0x8005BF2C]
        if !handled {
            self.build(cx, false);
            if self.inmove > 220 && self.mark(|_| true) > 0 {
                self.pick(cx);
                handled = true;
            }
        }
        if handled {
            return;
        }
        // Ball: the ball behind me, a backdash towards it (0x800B46A0)
        if cx.ball.is_some_and(|b| b.behind(usize::from(cx.me.side > 0))) {
            let back = cx.data.backdash.clone();
            self.play(&back);
            return;
        }

        // ---- 4.L stance escape
        if self.stance && !self.cand.is_empty() && !self.fire18 {
            if (self.until < m.as_ - cx.mf || self.myzone > 2 || cx.a.quadrant == 2) && self.mark(|_| true) > 0 && self.pick(cx) >= 0 {
                self.cd = -1;
            }
            return;
        }

        // ---- 4.M inside his string [0x8005C054..0x8005C224]
        if self.inc && self.hseq >= 0 && self.hiszone < 4 {
            if (self.now || cx.chance(self.p.in_string_interrupt_prob)) && self.until > 1 && self.until < 30 && self.pool_interrupt(cx) > 0 && self.pick(cx) >= 0 {
                self.followups += 1;
                return;
            }
            if !self.atk && self.he_attacks() && (self.glatch != 0 || cx.chance(self.p.in_string_guard_prob)) {
                self.guard_pad(cx);
                return;
            }
            if cx.chance(self.p.sidestep_prob) && self.sidestep_ok() && self.hiszone > 1 {
                self.sidestep(cx, -1);
                self.cd -= 10;
                return;
            }
        }

        // ---- 4.N / 4.O the reaction to his first attack [0x8005C224..0x8005D4B8]
        let r = self.reaction(cx);
        let (mut act, mut n) = (false, 0);
        match r {
            1 => {
                n = if !self.inc {
                    self.pool_fast_poke(cx)
                } else if self.hmask == 0x607 || self.hmask == 0x706 || self.h4 & 0x20_0001 != 0 {
                    self.pool_interrupt_nohigh(cx)
                } else {
                    self.pool_interrupt(cx)
                };
                if self.hmask == 0x412 {
                    let zm = cx.data.zm[self.myzone as usize];
                    n += self.mark(|c| c.mv.as_ != 0 && c.mv.rec4 & 0x2_0001 == 1 && c.mv.range_class() & zm != 0);
                }
                act = n > 0;
            }
            2 => {
                n = self.pool_guard(cx);
                act = true;
                if n <= 0 {
                    self.guard_pad(cx);
                }
            }
            3 => act = self.evade(cx),
            4 => {
                self.sidestep(cx, -1);
                act = true;
                self.cd -= 10;
            }
            5 => {
                let parries = &cx.data.rules.parries.aliases;
                n = self.mark(|c| parries.contains(&c.e.dest));
                act = n > 0;
            }
            6 => {
                let late = cx.data.rules.late_parry.alias;
                n = self.mark(|c| c.e.dest == late);
                act = n > 0;
            }
            7 | 8 => {
                let p = cx.data.perms.get(cx.my_moveset.max(0) as usize);
                if let Some(seq) = p.and_then(|p| if r == 7 { p.hi_seq.as_ref() } else { p.lo_seq.as_ref() }) {
                    self.play(seq);
                }
                act = true;
            }
            9 => {
                n = self.mark(|c| c.mv.rec4 & 1 == 1 && c.mv.as_ == 0);
                if n > 0 {
                    act = true;
                    self.now_n = true;
                    self.cd = -1;
                }
            }
            _ => {}
        }
        if act && n > 0 && self.pick(cx) >= 0 {
            self.reset_mv();
            self.followups += 1;
        }
        if act {
            return;
        }

        // ---- 4.P the dead executors' state side effects
        if self.lc_c > 0 {
            (self.lc_c, self.nc_c, self.nostart) = (0, 0, true);
        }
        if self.lc_b > 0 {
            (self.lc_b, self.nc_b) = (0, 0);
        }
        if self.lc_d >= 0 {
            (self.lc_d, self.nc_d) = (0, 0);
        }

        // ---- 4.Q anti-air / back turned / turning round [0x8005DEE0..0x8005E278]
        if !self.atk && self.turning(cx) {
            return;
        }

        // ---- 4.S neutral offense, 4.T movement
        if let Flow::Movement = self.neutral(cx) {
            self.movement(cx);
        }
    }

    /// 4.G: lying on the ground. Returns whether the frame is done.
    fn wake_up(&mut self, cx: &mut Cx) -> bool {
        let h = cx.h;
        if cx.my_list().iter().any(|e| e.cond == 0x31) {
            // the stun has to drain first: tap 2 every P[00 + rand & 3] + 1 frames
            self.waket -= 1;
            if self.waket >= 0 {
                self.out = 0;
            } else {
                self.waket = self.p.wake_up_intervals[(cx.lcg() & 3) as usize];
                self.out = 0x10;
            }
            return true;
        }
        self.npool = 0;
        self.move_prev = cx.m.id;
        self.build(cx, true);
        if self.cand.is_empty() {
            self.out = 0;
            return true;
        }
        self.reset_mv();
        self.wlock = 16;
        if self.fire1a && cx.me.posture(cx.gd) & 0x200 != 0 {
            self.out = 0x8000;
            return true;
        }
        if ((self.h4 & 0x20_0000 != 0 && h.as_ == 0) || (self.hmask == 0x607 && self.h8 & 0x1_0000 == 0 && self.until < 29)) && cx.hf < h.ae {
            self.out = 0;
            return true;
        }
        // the categories that suit the situation: 1 up, 2 1 / d+1, 4 back, 8 general, 0x10 get-up attacks,
        // 0x20 spring moves
        let mask: u32 = if h.as_ == 0 {
            if self.myzone > 2 {
                if self.hiszone < 4 {
                    0x3F
                } else {
                    7
                }
            } else if self.ticks & 1 == 0 {
                0
            } else if self.cd < 0 && cx.lcg() & 0x3F == 0 {
                0x38
            } else {
                7
            }
        } else if self.until < 12 && cx.hf <= h.ae {
            if self.hmask == 0x10F || self.h8 & 0x1_0000 != 0 {
                if self.ticks & 1 == 0 {
                    0
                } else if cx.lcg() & 2 == 0 {
                    if h.js != 0 || self.turn > 0x4000 || self.h4 & 0x20_0000 != 0 {
                        if cx.lcg() & 0x3F == 0 {
                            4
                        } else {
                            2
                        }
                    } else {
                        4
                    }
                } else {
                    0x38
                }
            } else if self.hiszone < 3 && (self.hiszone < 2 || self.ddelta < 11) {
                if cx.lcg() & 0xF != 0 {
                    0x3F
                } else {
                    0
                }
            } else {
                1
            }
        } else if self.myzone < 2 {
            if self.h4 & 2 != 0 {
                0x38
            } else {
                0x28
            }
        } else {
            0
        };
        if mask == 0 {
            self.out = 0;
            return true;
        }
        let cat = loop {
            let c = cx.data.wakecat.get(cx.randint(6) as usize).copied().unwrap_or(0);
            if mask & c != 0 {
                break c;
            }
        };
        match cat {
            1 => {
                self.out = 0x1000;
                return true;
            }
            2 => {
                self.out = if self.ticks & 7 == 0 { 0x4080 } else { 0x80 };
                return true;
            }
            // back (some move sets: forward)
            4 => {
                self.out = if cx.data.rules.wake_forward.movesets.contains(&cx.my_moveset) { 0x2000 } else { 0x8000 };
                return true;
            }
            8 => {
                self.pool_general(cx);
            }
            0x10 => {
                self.mark(|c| c.mv.rec8 & 0x1_0000 != 0 || c.mv.low16() == 0x10F);
            }
            _ => {
                self.mark(|c| c.e.cmd == 0xC001 || (c.mv.rec4 & 0xA_0000 == 0x8_0000 && c.mv.rec4 & 0x60_0000 == 0));
            }
        }
        if self.npool > 0 {
            self.pick(cx);
        }
        true
    }

    /// 4.H: one handler (0x8005B598). Returns whether the frame is done.
    fn call_handler(&mut self, cx: &mut Cx, vs: bool) -> bool {
        if self.wlock >= 0 || !cx.chance(self.p.evade_and_handler_prob) {
            return false;
        }
        let Some(k) = (if vs { self.h_vs } else { self.h_self }) else { return false };
        let data = cx.data;
        let Some(f) = data.routines.get(k) else { return false };
        match self.handler(cx, f) {
            -2 => true,
            -1 => false,
            v => {
                if v == 0x2000 || v == 0x8000 {
                    (self.walk38, self.run3a, self.mvcd40) = (16, 0, 0x38);
                }
                self.out = v as u16;
                true
            }
        }
    }

    /// 4.Q (only while not attacking). Returns whether the frame is done.
    fn turning(&mut self, cx: &mut Cx) -> bool {
        let h = cx.h;
        // anti-air (P48 is 0 in every profile, so it never fires)
        let mut v = 0u16;
        let a = h.js == 0 || self.hiszone > 1 || (cx.lcg() & 0xFFF) as i32 >= self.p.anti_air_prob;
        let b = (self.turn > 0x3FFF || {
            v = 0x6000;
            self.ddelta > -11
        }) && (h.je - cx.hf < 21 || {
            v = 0x2000;
            self.turn < 0x4001
        });
        if !(a || b) {
            self.out = v;
            return true;
        }
        if self.turn < 0x6001 || self.hiszone > 1 {
            // turn round toward him, more likely the more I'm turned away
            if self.turn > 0x1000 && ((cx.lcg() & 0x7FFF) as i32) < self.turn && (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero {
                (self.walk38, self.run3a, self.mvcd40) = (8, 0, 0x30);
                if cx.data.rules.turn_by_facing.movesets.contains(&cx.my_moveset) {
                    self.out = if self.turn > 0x3FFF { 0x8000 } else { 0x2000 };
                } else if !self.inc || cx.lcg() & 7 != 0 {
                    self.out = if self.hiszone < 3 && self.ticks & 1 != 0 { 0x8000 } else { 0x2000 };
                } else {
                    self.guard_pad(cx);
                }
                return true;
            }
        } else {
            // back turned more than 135° with him in zone 1
            if cx.op_air {
                self.out = 0;
                return true;
            }
            if self.odown {
                self.out = if self.ticks & 1 != 0 { 0x4040 } else { 0x4020 };
                return true;
            }
            if (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero {
                self.cd = -2;
            }
        }
        false
    }

    /// 4.N: which reaction to his attack (0 none, 1 counter, 2 guard, 3 evade, 4 sidestep, 5 parry, 6 a move set's
    /// late parry, 7 / 8 canned reversal / low parry, 9 duck).
    fn reaction(&mut self, cx: &mut Cx) -> u32 {
        let h = cx.h;
        if self.hseq >= 0 || self.op_stance {
            return 0;
        }
        if self.pwait == 0 && !self.atk {
            return 2; // the punish wait ran out: guard
        }
        if self.evade == 0 && (!self.inc || self.hiszone >= 4) {
            return 0;
        }
        if (h.as_ != 0 && cx.hf > h.ae) || cx.a.opp_quadrant == 2 {
            self.evade = 2;
        }
        if self.evade == 0 && (self.hmask == 0x607 || self.hmask == 0x706 || self.h4 & 0x20_0000 != 0) && cx.chance(self.p.evade_and_handler_prob) {
            self.evade = 1;
        }
        if self.hcd < 0 && self.evade != 2 && self.evade != 0 {
            return 3;
        }
        // (ZM with his zone, not ZM2)
        let reach_ok = self.inc && !self.safe && cx.a.opp_turn < 0x5000 && self.dist < 0xB87 && h.range_class() & cx.data.zm[self.hiszone as usize] != 0;
        let none = Perms::default();
        let perms = cx.data.perms.get(cx.my_moveset.max(0) as usize).unwrap_or(&none);
        let until = self.until;
        if (until as u16).wrapping_sub(8) < 2 && self.mmask == 0 && !cx.me_air && perms.can_parry && h.f24 & 0x800 == 0 && !cx.op_air && reach_ok && cx.chance(self.p.parry_prob) {
            return 5;
        }
        let late = &cx.data.rules.late_parry;
        if cx.my_moveset == late.moveset && (late.until[0]..=late.until[1]).contains(&until) && self.mmask == 0 && !cx.me_air && reach_ok && cx.chance(self.p.parry_prob) {
            return 6;
        }
        if (until as u16).wrapping_sub(4) < 7 && self.mmask == 0 && !cx.me_air && !cx.op_air && reach_ok && cx.chance(self.p.parry_prob) {
            if matches!(self.hmask, 0x412 | 0x217 | 0x607 | 0x706) && perms.hi_seq.is_some() {
                return 7;
            }
            if self.hmask == 0x10F && perms.lo_seq.is_some() {
                return 8;
            }
        }
        if self.h8 & 0x40_0000 != 0 {
            if cx.escapable() && cx.chance(self.p.evade_and_handler_prob) {
                return 1;
            }
            if until <= 29 && cx.chance(self.p.counter_throw_startup_prob) {
                self.glatch = 2;
                return 2;
            }
        }
        if self.he_attacks() {
            if self.safe || self.atk {
                return 0;
            }
            if self.glatch != 0 {
                return 2;
            }
            if cx.chance(self.p.guard_prob) || (self.hopt > 30 && cx.chance(self.p.guard_prob_cap)) {
                self.glatch = 5;
                return 2;
            }
        }
        if (self.pwait > 8 && cx.lcg() & 0x1F == 0) || (self.rage && cx.lcg() & 0xF == 0) {
            return 1;
        }
        // countering a move that hit me
        if self.ring[0].is_some() && self.ring[0] == h.id && cx.chance(self.p.counter_attack_cap) {
            return 1;
        }
        // (a hop of 31+ frames: P22 counters too)
        let counter = cx.chance(self.p.counter_attack_prob) || (self.hopt >= 31 && cx.chance(self.p.counter_attack_cap));
        if !counter {
            // the sidestep test (LAB_8005CACC)
            if !self.sidestep_ok() || self.hiszone != 2 {
                return 0;
            }
            // (Evasive: any attack, not only highs)
            if self.m4 & 1 != 0 || cx.me_air || self.atk || (self.hmask != 0x412 && self.playing != Style::Evasive) {
                return 0;
            }
            return if cx.chance(self.p.sidestep_prob) { 4 } else { 0 };
        }
        if self.hmask != 0x412 {
            return 1;
        }
        // 1/64: duck under the high
        if cx.lcg() & 0x3F == 0 {
            9
        } else {
            1
        }
    }

    /// He attacks (or hops): an attack mask other than none / 0x607.
    fn he_attacks(&self) -> bool {
        let l = self.h8 & 0xFFFF;
        (l != 0 && l != 0x607) || self.hopt > 0
    }

    /// A sidestep could dodge: 9..11 frames before his attack, facing him, not in a throw-type move.
    fn sidestep_ok(&self) -> bool {
        (self.until as u16).wrapping_sub(9) < 3 && self.turn < 0x4000 && self.m4 & 0x40_0000 == 0
    }

    /// The guard pad: back against mids (0x217), down-back otherwise, if he is in front.
    fn guard_pad(&mut self, cx: &Cx) {
        self.out = if cx.a.quadrant == 0 {
            self.reset_mv();
            if self.hmask == 0x217 {
                0x8000
            } else {
                0xC000
            }
        } else {
            0
        };
    }

    /// 4.S: the neutral attack decision [0x8005E304..0x800601C0].
    fn neutral(&mut self, cx: &mut Cx) -> Flow {
        let h = cx.h;
        if self.chold > 0 {
            self.chold -= 1;
            self.out = self.prev_out & 0xF000;
            return Flow::Fin;
        }
        if self.cdash >= 5 {
            self.out = self.prev_out & 0xF000;
            return Flow::Fin;
        }
        if !(self.myzone < 4 && !self.ohigh && self.natk < self.natk_max) {
            return Flow::Movement;
        }
        // approach first (Rushdown: the attacks that reach from there too)
        if self.myzone > 2 && self.playing != Style::Rushdown && cx.randint(12) != 0 {
            return Flow::Movement;
        }
        if self.myzone > 1 && cx.randint(8) == 0 {
            return Flow::Movement;
        }
        if self.run3a != 0 {
            if !(self.pwait < 1 && (!self.inc || self.hiszone > 3)) {
                self.out = 0x8000; // stop running
                return Flow::Fin;
            }
            if self.h4 & 4 != 0 && cx.randint(4096) < 0x100 {
                return Flow::Movement;
            }
        }
        // ---- the attack decision (LAB_8005E4D8)
        let mut cd = self.cd;
        let mut throw_p = self.p.throw_attempt_prob;
        if self.nostart {
            let mut k = 0;
            for c in self.cand.iter_mut() {
                if c.state >= 0 && c.mv.f24 & 0x2000 != 0 {
                    if c.state > 0 {
                        k += 1;
                    }
                    c.state = -1;
                }
            }
            self.npool -= k;
        }
        if self.hstart >= 26 && self.pwait < 0 && !self.safe {
            let a = cx.lcg();
            self.pwait = (self.until - ((a & 7) as i32 + 4)).max(3);
        }
        let p46 = self.p.gate_always_zero;
        let mut to_pick = false;
        let n: i32 = 'decide: {
            if self.pwait >= 1 {
                break 'decide 0; // waiting to punish
            }
            if self.now || (self.hstart > 25 && (cx.lcg() & 0xFFF) as i32 >= p46 / 2) || ((self.mrec - 1) as u32) < 19 && (cx.lcg() & 0xFFF) as i32 >= p46 {
                cd = -1;
            }
            if cx.chance(self.p.anti_air_prob) && cx.op_air && self.h4 & 0xFFFF != 0x4C02 {
                let (js, je, f) = (h.js, h.je, cx.hf);
                if js + (je - js) / 2 <= f && f <= je && self.turn < 0x4001 && self.myzone == 2 {
                    self.cd = -1;
                }
            }
            if cx.op.alias == cx.data.rules.counter_alias.alias && self.myzone < 3 && cx.chance(self.p.counter_attack_prob) {
                self.cd -= 2;
            }
            if self.h4 & 0x605 != 0 || cx.op_air || cx.op.unthrowable(cx.gd) || cx.op_moveset == cx.data.rules.unthrown.moveset || self.until < 10 {
                throw_p = 0;
            }
            // Ball: the ball just in front, no throws but an attack (0x800B4720)
            if cx.ball.is_some_and(|b| b.in_front(usize::from(cx.me.side > 0))) {
                break 'decide self.pool_general(cx);
            }
            // Juggler: he is in the air, not attacking: the fast attacks that reach him, now
            if self.playing == Style::Juggler && self.h4 & 0x400 != 0 && h.as_ == 0 {
                let mut n = self.pool_fast_poke(cx);
                if n == 0 {
                    n = self.pool_general(cx);
                }
                break 'decide n;
            }
            if self.odown && !self.op_stance {
                break 'decide self.okizeme(cx, &mut cd);
            }
            if self.h4 & 0xFFFF == 0x4C02 && cx.chance(self.p.pressure_on_stun_prob) {
                break 'decide self.stun_pressure(cx);
            }
            if self.jumpt >= 0 {
                let n = self.mark(|c| c.mv.as_ != 0 && (c.mv.rec4 & 0x8_0000 != 0 || c.mv.rec8 & 0x20_0000 != 0) && c.mv.rec4 & 0x2_0000 == 0);
                if n > 0 {
                    self.jumpt = -1;
                }
                break 'decide n;
            }
            if self.hopt != 0 {
                if !self.now && (cx.lcg() & 0xFFF) as i32 >= self.p.counter_attack_prob {
                    if cd < 1 {
                        break 'decide if self.h4 & 1 == 0 { self.pool_general(cx) } else { self.pool_stepin(cx) };
                    }
                    break 'decide 0;
                }
                break 'decide self.pool_counter(cx);
            }
            if (self.inc || self.hthrow) && !self.op_stance {
                if self.now || cx.chance(self.p.counter_attack_prob) {
                    break 'decide self.pool_counter(cx);
                }
                if cd < 1 {
                    break 'decide if self.h4 & 1 != 0 { self.pool_stepin(cx) } else { self.pool_general(cx) };
                }
                break 'decide 0;
            }
            if cd < 1 {
                break 'decide self.offense(cx, throw_p);
            }
            // cooldown running: a rare crouch (4 / 4096)
            if self.myzone > 2 || self.m4 & 1 != 0 || (cx.lcg() & 0xFFF) > 3 {
                break 'decide 0;
            }
            let n = self.mark(|c| c.mv.rec4 & 1 == 1 && c.mv.as_ == 0);
            if n >= 1 {
                to_pick = true; // skips the desperation test
                break 'decide n;
            }
            0
        };
        let mut n = n;
        if !to_pick {
            // desperation moves at 10 % HP
            if n == 0 && self.hiszone > 2 && cx.me.ki == 0 && cx.me.health <= cx.me.max_health * ONE / 10 && cx.lcg() & 3 == 0 && cx.chance(self.p.counter_attack_prob) {
                n = self.mark(|c| c.mv.f24 & 0x1_0000 != 0);
            }
            if n < 0 {
                self.reset_mv(); // something canned was started
                return Flow::Fin;
            }
        }
        // PICK (from the whole pool, not just `n`)
        let idx = self.pick(cx);
        if idx < 0 {
            return Flow::Movement;
        }
        self.reset_mv();
        let mvp = self.cand[idx as usize].mv;
        if mvp.f24 & 0x2000 != 0 {
            self.lc_c = 1;
        } else if (self.m4 & 0x8_0000 != 0 && cx.lcg() & 0x3F < 0x20) || (mvp.rec4 & 0x8_0000 != 0 && cx.lcg() & 0x3F < 0x30) {
            self.lc_b = 1;
        }
        if self.last_rec.is_some_and(|r| matches!(r.rec4 & 0xFFFF, 0x2021 | 0x2829)) {
            self.chold = 12;
        }
        Flow::Fin
    }

    /// 5.8: the cooldown is over [0x8005F3EC..0x8005FFF0]. Returns the pool count, or -1 when a canned input
    /// was started.
    fn offense(&mut self, cx: &mut Cx, throw_p: i32) -> i32 {
        if self.cdash >= 0 {
            // release the crouch dash into a hop move
            let n = self.mark(|c| c.mv.rec4 & 0xA_0000 == 0x8_0000 && c.mv.js == 0);
            self.cdash = -1;
            return n;
        }
        // Juggler: a launcher, if one reaches, while he recovers from his attack
        if self.playing == Style::Juggler && self.myzone < 3 && cx.h.as_ != 0 && cx.hf > cx.h.ae {
            let zm = cx.data.zm[self.myzone as usize];
            let n = self.mark(|c| c.mv.as_ != 0 && c.mv.rec8 & 0x40_0000 == 0 && c.mv.range_class() & zm != 0 && cx.launches(c.mv.id));
            if n > 0 {
                return n;
            }
        }
        // throws: P10, then P16 picks a command throw (else a button throw)
        if cx.chance(throw_p) && self.myzone < 2 && cx.op_moveset != cx.data.rules.unthrown.moveset {
            let mut n = 0;
            let button_throw = |c: &Cand| c.mv.rec8 & 0x40_0000 != 0 && c.e.cmd & 0xFFE0 == 0;
            if cx.chance(self.p.throw_select_prob) {
                n = self.mark(|c| c.mv.rec8 & 0x40_0000 != 0 && c.e.cmd & 0xC200 != 0x200);
                if n < 1 && self.myzone < 2 {
                    n = self.mark(button_throw);
                }
            } else if self.myzone < 2 {
                n = self.mark(button_throw);
            }
            if n > 0 {
                return n;
            }
        }
        // the moveset's motion prelude (qcf / crouch dash / f, n)
        let none = Perms::default();
        let perms = cx.data.perms.get(cx.my_moveset.max(0) as usize).unwrap_or(&none);
        if let Some(first) = perms.preludes.first() {
            if self.combocd < 0 && !self.atk && self.m4 & 1 == 0 && cx.me.alias != first.1 && self.turn < 0x2001 && !cx.me_air && self.myzone < 3 && !self.odown {
                let u = (cx.lcg() & 0xFFF) as i32;
                if let Some((_, alias, seq)) = perms.preludes.iter().find(|p| u <= p.0) {
                    if self.reachable(cx, *alias) {
                        self.play(&seq[1.min(seq.len())..]);
                        self.lc_b = 1;
                        self.combocd = (cx.lcg() & 0xF) as i32 + 28;
                        return -1;
                    }
                }
            }
        }
        let crouch_na = |c: &Cand| c.mv.rec4 & 1 == 1 && c.mv.as_ == 0;
        let dash_hop = |c: &Cand| c.e.cmd == 0xC001 || (c.mv.rec4 & 0xA_0000 == 0x8_0000 && c.mv.rec4 & 0x60_0000 == 0);
        // Rushdown: fast pokes when they reach, else straight to the other attacks
        let rush = self.playing == Style::Rushdown;
        if rush {
            let k = self.pool_fast_poke(cx);
            if k > 0 {
                return k;
            }
        }
        let mut n;
        if self.myzone < 3 && !cx.me_air && self.m4 & 1 == 0 && (cx.lcg() & 0x3F) < 14 && !rush {
            n = self.mark(crouch_na); // crouch (crouch dash / d moves set-up)
        } else if (cx.lcg() & 0x3F) < 0x20 && !rush {
            n = if cx.lcg() & 1 != 0 { self.mark(dash_hop) } else { self.pool_punish_posture(cx) };
        } else {
            if self.cdash < 0 && self.jumpt < 0 && self.m4 & 1 == 0 && !cx.me_air && cx.chance(0x200) {
                let k = self.mark(crouch_na);
                if k > 0 {
                    self.cdash = 18;
                    self.cd = -1;
                    return k;
                }
            }
            n = if rush && self.myzone < 2 { 0 } else { self.mark(dash_hop) };
            if cx.chance(self.p.guard_aware_attack_prob) {
                n += self.pool_guard_aware(cx);
            } else if cx.chance(self.p.fast_poke_prob) {
                n += self.pool_fast_poke(cx);
            } else if self.h4 & 1 == 0 || cx.chance(self.p.gate_always_zero) {
                // he is not crouching
                if (cx.lcg() & 0x3F) < 10 {
                    n += self.pool_buttons(cx);
                } else {
                    if !cx.me_air && self.jumpt < 0 && cx.randint(4096) < 0x200 {
                        n = self.mark(|c| c.mv.js != 0 && c.mv.rec4 & 0x2_0000 == 0 && (c.mv.rec4 & 0x8_0000 != 0 || c.mv.rec8 & 0x20_0000 != 0));
                        if n > 0 {
                            let (a, b, c) = (cx.randint(4), cx.randint(4), cx.randint(4));
                            self.jumpt = a + b + c + 1;
                            return n;
                        }
                    }
                    if cx.randint(4096) < 10 {
                        let parries = &cx.data.rules.parries.aliases;
                        n += self.mark(|c| c.mv.as_ == 0 && c.mv.js == 0 && c.mv.rec4 & 0x2_0000 == 0 && !parries.contains(&c.e.dest));
                    } else {
                        n += self.pool_general(cx);
                    }
                }
            } else {
                n += self.pool_stepin(cx); // against a croucher: no highs, no throws
            }
        }
        // Rushdown: nothing of that here, any attack that reaches
        if n == 0 && self.playing == Style::Rushdown {
            n = self.pool_general(cx);
        }
        n
    }

    /// 5.9: he is down and hittable [0x8005E8F8..0x8005ED58].
    fn okizeme(&mut self, cx: &mut Cx, cd: &mut i32) -> i32 {
        if self.atk {
            return 0;
        }
        if (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero {
            *cd = -1;
        }
        if !(self.turn < 0x4001 || cx.lcg() & 0xF != 0) {
            return self.mark(|c| c.mv.rec4 & 0x2_0000 != 0); // back turned, 1/16: the turn-round moves
        }
        if *cd >= 0 {
            return 0;
        }
        let a = cx.lcg();
        let u = a & 0xFFF;
        let low_or_ground = |c: &Cand, low: bool| {
            let l = c.mv.low16();
            c.mv.as_ < 0x51 && (c.mv.rec8 & 0x1_0000 != 0 || l == 0x51F || (low && l == 0x10F)) && c.mv.rec8 & 0x40_0000 == 0
        };
        if u < 0x334 {
            // moves whose own list starts with a cond 0x35 entry
            self.mark(|c| c.mv.l0_cond == 0x35 && c.mv.rec8 & 0x40_0000 == 0)
        } else if u < 0x3AF {
            self.sidestep(cx, (a & 1) as i32);
            -1
        } else if u < 0x6E2 {
            self.mark(|c| low_or_ground(c, false))
        } else if u < 0x75D && self.m4 & 1 == 0 {
            self.mark(|c| c.mv.rec4 & 1 == 1 && c.mv.as_ == 0)
        } else if u < 0x829 {
            self.mark(|c| c.e.cmd == 0xC002)
        } else if ((self.myzone - 2) as u32) < 2 && u < 0x9C3 {
            let seq = cx.data.okizeme.clone();
            self.play(&seq); // u/f, u/f+2
            -1
        } else {
            self.mark(|c| low_or_ground(c, true))
        }
    }

    /// 5.10: he is stunned (0x4C02) [0x8005ED58..0x8005F0F0].
    fn stun_pressure(&mut self, cx: &mut Cx) -> i32 {
        let ground_na = |c: &Cand| c.mv.rec8 & 0x1_0000 != 0 && c.mv.as_ == 0 && c.mv.js == 0;
        let mut n = 0;
        if self.myzone < 3 {
            if self.myzone > 1 {
                n = if cx.lcg() & 0xF == 0 { self.mark(|c| c.e.cmd == 0xC001) } else { self.mark(ground_na) };
            }
        } else if self.ddelta < 0x33 && cx.lcg() & 7 == 0 {
            n = self.mark(ground_na);
        } else {
            n = self.mark(|c| c.e.cmd == 0xC001);
            if n > 0 {
                (self.run3a, self.walk38, self.mvcd40) = (2, 250, 310);
            }
        }
        if self.myzone < 4 && !self.ohigh && (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero {
            self.cd = -1;
            n += if !cx.op_air && (cx.lcg() & 0x3F) < 0x20 && !self.filter { self.pool_punish_nohigh(cx) } else { self.pool_punish_posture(cx) };
        }
        n
    }

    /// 5.11: evade plans against unblockables and throw-type moves [0x8005CEDC..0x8005D244]. Returns
    /// "handled"; a plan that marks a pool without handling leaves it to the neutral pick.
    fn evade(&mut self, cx: &mut Cx) -> bool {
        let h = cx.h;
        if self.ohigh {
            return false;
        }
        let mut d = self.dist;
        if self.evade == 1 {
            self.evade = if self.myzone < 3 && (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero {
                3
            } else {
                let k = cx.randint(7) as usize;
                cx.data.evade.get(k).copied().unwrap_or(0)
            };
        }
        if cx.op_moveset == cx.data.rules.evade_closer.moveset {
            d += cx.data.rules.evade_closer.distance;
        }
        if !(self.evade == 1 || self.evade == 2) && d > 0xB86 {
            self.evade = 0;
            self.out = 0;
            return true;
        }
        let back = cx.data.backdash.clone();
        match self.evade {
            3 => {
                self.reset_mv();
                if h.as_ != 0 && cx.hf > h.ae {
                    return false;
                }
                if self.until < 29 {
                    if self.until < 9 {
                        self.out = if d <= 0xB86 && cx.a.opp_turn < 0x4000 { 0x8000 } else { 0 };
                        return true;
                    }
                    if d > 0xB86 {
                        self.out = 0;
                        return true;
                    }
                    if self.myzone < 3 {
                        if self.pool_stepin(cx) > 0 {
                            self.cd = -1;
                            return false;
                        }
                        return true;
                    }
                    if cx.a.opp_turn > 0x3FFF {
                        return false;
                    }
                    self.evade = 4;
                    self.out = 0x8000;
                    return true;
                }
                if self.myzone < 3 {
                    return self.pool_stepin(cx) < 1;
                }
                // 0x800592FC
                self.out = if self.ticks & 1 == 0 { 0x2000 } else { 0 };
                self.cd = (cx.lcg() & 0xF) as i32;
                true
            }
            4 => {
                self.reset_mv();
                if cx.a.opp_turn < 0x4001 || self.hiszone < 2 {
                    if d < 0xB87 {
                        if self.until < 9 || self.myzone > 1 {
                            self.play(&back);
                            return true;
                        }
                        if self.pool_stepin(cx) > 0 {
                            self.cd = -1;
                            return false;
                        }
                        return true;
                    }
                    self.out = 0;
                    return true;
                }
                self.out = 0x2000;
                true
            }
            5 => {
                if self.m4 & 0x40_0000 != 0 || cx.lcg() & 7 != 0 || self.until < 7 {
                    self.out = 0x2000; // (later blocks may overwrite it)
                    return false;
                }
                if cx.a.opp_turn > 0x3FFF {
                    self.evade = 3;
                }
                self.sidestep(cx, -1);
                true
            }
            6 => {
                self.reset_mv();
                if d < 0xB87 {
                    if self.until < 29 {
                        if self.hiszone > 1 || self.until < 8 {
                            self.play(&back);
                        } else {
                            // jump up-forward
                            self.out = 0x3000;
                            self.hold48 = 20;
                        }
                    } else {
                        self.out = if self.hiszone < 1 { 0 } else { 0x8000 };
                    }
                } else {
                    self.out = 0;
                }
                true
            }
            _ => false,
        }
    }

    /// 5.12 (+0x6A): my current posture already guards his attack.
    fn already_safe(&self) -> bool {
        let p = self.m4 & 0xFFFF;
        if !(self.dist < 0xB87 && !self.op_stance) {
            return false;
        }
        if self.hthrow {
            return self.hmask != 0x217 && (p == 0x2021 || p == 0x2829);
        }
        if !self.inc || self.turn > 0x3000 {
            return false;
        }
        match self.hmask {
            0x412 => matches!(p, 0x2021 | 0x2829 | 0x1052),
            0x217 => p == 0x1052,
            0x10F => p == 0x2829,
            0x31F => matches!(p, 0x2829 | 0x1052),
            0x706 => matches!(p, 0x2829 | 0x2021),
            _ => false,
        }
    }

    /// 4.T: walking, dashing, running, sidestepping (LAB_80060304) [0x80060304..0x80060E68].
    fn movement(&mut self, cx: &mut Cx) {
        let h = cx.h;
        let mut moved = false;
        'mv: {
            if self.wlock >= 0 {
                break 'mv;
            }
            if self.walk38 < 0 {
                if self.atk || self.croucht >= 0 || self.hold48 >= 0 || self.jumpt >= 0 {
                    break 'mv;
                }
            } else if (!self.fire18 || self.dist > 7000) && self.myzone > 0 {
                // a walk / run is in progress: stop for an attack that reaches me
                let a = cx.lcg();
                let stop =
                    (a & 0xFFF) as i32 >= self.p.gate_always_zero && self.inc && !self.safe && cx.a.opp_turn < 0x5000 && self.dist < 0xB87 && h.range_class() & cx.data.zm2[self.hiszone as usize] != 0;
                if stop {
                    self.reset_mv();
                } else {
                    if self.run3a == 2 {
                        if self.hiszone < 3 && cx.chance(0x20) {
                            self.reset_mv();
                            self.out = 0x8000;
                        } else {
                            self.out = 0x2000;
                        }
                    } else {
                        if self.cd < 0x24 && self.myzone == 2 && cx.lcg() & 0xF == 0 && self.mark(|c| c.e.cmd == 0xC001) > 0 && self.pick(cx) >= 0 {
                            (self.run3a, self.walk38, self.mvcd40) = (0, 0, 0x28);
                            moved = true;
                            break 'mv;
                        }
                        self.out = self.prev_out & 0xF000; // keep holding the walk direction
                    }
                    moved = true;
                    break 'mv;
                }
            } else {
                self.reset_mv(); // then idle: the movement cooldown is 0x28 now
            }
            // CHOOSE
            // (Rushdown, Thrower: no pause while out of reach)
            if (self.mvcd40 >= 0 && !(matches!(self.playing, Style::Rushdown | Style::Thrower) && self.myzone > 1)) || self.hold48 >= 0 {
                break 'mv;
            }
            let u = cx.randint(4096);
            let mut mode = 0;
            if !self.fire18 {
                if self.cd < 0x24 {
                    // attacking soon: by my zone
                    mode = match self.myzone {
                        2 => {
                            if u < 122 {
                                5
                            } else if u < 491 {
                                1
                            } else if u < 532 {
                                3
                            } else if u < self.p.sidestep_prob * 4096 / 1000 {
                                8
                            } else {
                                0
                            }
                        }
                        3 => {
                            if u < 1638 {
                                2
                            } else if u < 2867 {
                                1
                            } else {
                                0
                            }
                        }
                        4 => {
                            if u < 204 {
                                2
                            } else if u < 1433 {
                                4
                            } else {
                                1
                            }
                        }
                        5 => {
                            if self.faracc < 721 && !self.odown && u > 285 {
                                1
                            } else {
                                4
                            }
                        }
                        _ => {
                            if u < 20 {
                                3
                            } else {
                                0
                            }
                        }
                    };
                } else {
                    // by his zone
                    mode = match self.hiszone {
                        2 => {
                            if u < 40 {
                                2
                            } else if u < 368 {
                                3
                            } else {
                                0
                            }
                        }
                        3 => {
                            if u < 491 {
                                2
                            } else if u < 532 {
                                5
                            } else if u < 536 {
                                3
                            } else if u < 557 {
                                1
                            } else if u < 577 && self.m4 & 2 != 0 {
                                8
                            } else {
                                0
                            }
                        }
                        4 => {
                            if u < 128 {
                                2
                            } else if u < 144 {
                                5
                            } else if u < 192 {
                                8
                            } else if u < 960 {
                                1
                            } else {
                                0
                            }
                        }
                        5 if self.faracc < 721 && !self.odown && u > 80 => {
                            if u < 204 {
                                2
                            } else if u < 1024 {
                                1
                            } else if u < 1515 {
                                5
                            } else {
                                0
                            }
                        }
                        5 => 4,
                        _ => {
                            if u < 409 {
                                6
                            } else if u < 819 {
                                3
                            } else {
                                0
                            }
                        }
                    };
                    if (mode == 3 || mode == 6) && (cx.lcg() & 0xFFF) as i32 >= self.p.keep_walk_back_prob {
                        mode = 0;
                    }
                }
                if (mode == 3 || mode == 6) && (self.h4 & 0x8_0000 != 0 || self.hmask != 0) && cx.chance(self.p.gate_always_zero) {
                    mode = 0;
                }
                mode = self.lean_mode(mode, u);
            }
            let fwd = if self.crouchf { 0x6000 } else { 0x2000 };
            match mode {
                0 => {}
                1 => {
                    // dash
                    if self.mark(|c| c.e.cmd == 0xC001) < 1 {
                        self.walk_forward(cx, fwd);
                        moved = true;
                    } else {
                        (self.run3a, self.walk38, self.mvcd40) = (0, 0, 0x28);
                        moved = self.pick(cx) >= 0;
                    }
                }
                2 => {
                    self.walk_forward(cx, fwd);
                    moved = true;
                }
                3 => {
                    self.walk_back(cx);
                    moved = true;
                }
                4 => {
                    // run
                    if self.mark(|c| c.e.cmd == 0xC001) < 1 {
                        self.long_forward(cx, fwd);
                        moved = true;
                    } else {
                        (self.run3a, self.walk38, self.mvcd40) = (2, 250, 310);
                        moved = self.pick(cx) >= 0;
                    }
                }
                5 => {
                    self.long_forward(cx, fwd);
                    moved = true;
                }
                6 => {
                    // backdash
                    if self.mark(|c| c.e.cmd == 0xC002) < 1 {
                        self.walk_back(cx);
                        moved = true;
                    } else {
                        self.mvcd40 = 0x3C;
                        moved = self.pick(cx) >= 0;
                    }
                }
                _ => {
                    // 8: sidestep; the pick usually fails and the idle pad below replaces the first word (the
                    // sequence goes on next frame)
                    if self.hiszone > 2 || self.until > 10 {
                        self.sidestep(cx, -1);
                    }
                    moved = self.pick(cx) >= 0;
                }
            }
        }
        if !moved {
            // idle pad (LAB_80060E20)
            self.out = if self.croucht >= 0 {
                0x4000
            } else if self.hold48 >= 0 {
                self.prev_out & 0xF000
            } else {
                0
            };
        }
    }

    /// The style's lean on the movement mode (`u` the chooser's draw; 1 dash, 2 walk in, 3 walk back, 4 run,
    /// 8 sidestep).
    fn lean_mode(&self, mode: i32, u: i32) -> i32 {
        match self.playing {
            // in, never back
            Style::Rushdown if self.myzone > 2 => 4,
            Style::Rushdown if self.myzone > 1 && matches!(mode, 0 | 2 | 3 | 6 | 8) => 1,
            Style::Thrower if self.myzone > 1 && matches!(mode, 0 | 3 | 6 | 8) => 2,
            Style::Juggler if self.odown && self.myzone > 1 => 2,
            // at the edge of his reach: out of it when in, back in when out
            Style::Counter => match self.hiszone {
                1 | 2 if u < 1536 => 3,
                3 | 4 if u < 1024 => 2,
                _ => mode,
            },
            Style::Evasive if (2..4).contains(&self.hiszone) && mode == 0 && u < 2048 => 8,
            _ => mode,
        }
    }

    fn walk_forward(&mut self, cx: &mut Cx, fwd: u16) {
        let (k, a, b) = match self.myzone {
            0..=2 => (cx.randint(8), 4, 44),
            3 => (cx.randint(10), 10, 50),
            _ => (cx.randint(15), 15, 55),
        };
        (self.run3a, self.walk38, self.mvcd40) = (0, a + k, b + k);
        self.out = fwd;
    }

    fn walk_back(&mut self, cx: &mut Cx) {
        let k = cx.randint(5);
        (self.walk38, self.run3a, self.mvcd40) = (20 + k, 0, 60 + k);
        self.out = if self.crouchf { 0xC000 } else { 0x8000 };
    }

    fn long_forward(&mut self, cx: &mut Cx, fwd: u16) {
        let k = cx.randint(20);
        (self.walk38, self.run3a, self.mvcd40) = (40 + k, 0, 80 + k);
        self.out = fwd;
    }

    /// 4.W: post-processing and the output (LAB_80060E6C).
    fn post(&mut self, cx: &mut Cx) {
        self.crouchf = self.m4 & 0x205 == 1;
        if let Some(r) = self.last_rec {
            if self.hold48 < 0 && r.js != 0 {
                self.hold48 = r.js;
            }
        }
        for t in [&mut self.hold48, &mut self.walk38, &mut self.mvcd40, &mut self.whiff54, &mut self.jumpt, &mut self.cdash, &mut self.combocd, &mut self.wlock, &mut self.hcd] {
            if *t >= 0 {
                *t -= 1;
            }
        }
        if self.raget == 0 {
            // the rage is over: the normal profile again (what was learnt is lost)
            self.load_profile(cx.data, cx.me, cx.op, -1, -1);
        }
        if self.raget >= 0 {
            self.raget -= 1;
        }
        let (me, op) = (cx.me, cx.op);
        if me.struck {
            self.reset_mv();
            (self.lc_b, self.lc_c, self.lc_d) = (0, 0, 0);
        }
        if me.whiffed && self.hiszone < 3 {
            self.whiff54 = 30;
        }
        if op.whiffed && self.myzone < 2 && cx.chance(self.p.counter_attack_prob) {
            self.cd -= 3;
        }
        if me.health < self.hp_prev && (cx.lcg() & 0xFFF) as i32 >= self.p.gate_always_zero {
            self.cd -= 3;
            // rage: hit a third time by the same move -> the level 9 profile for 600 frames
            self.rage = false;
            let his = cx.h.id;
            if self.ring.iter().filter(|&&x| x.is_some() && x == his).count() > 1 {
                if self.raget < 1 {
                    self.load_profile(cx.data, me, op, 0, 9);
                }
                self.raget = 600;
                self.rage = true;
            }
            self.ring[self.ringi] = his;
            self.ringi = (self.ringi + 1) & 7;
        }
        self.hp_prev = me.health;
        // Mixed: another style when its time is up (not in a rage: the rage profile stays)
        if self.style == Style::Mixed && self.raget < 0 {
            self.mixt -= 1;
            if self.mixt < 0 {
                self.mix(cx.rng);
                self.load_profile(cx.data, me, op, -1, -1);
            }
        }
        if me.struck {
            // learning: guard and counter more, and more so inside his strings
            self.reset_mv();
            let p = &mut self.p;
            p.guard_prob = (p.guard_prob + 10).min(p.guard_prob_cap);
            p.counter_attack_prob = (p.counter_attack_prob + 10).min(p.counter_attack_cap);
            if self.hseq > 0 {
                p.in_string_guard_prob = (p.in_string_guard_prob + 40).min(p.in_string_guard_cap);
                p.in_string_interrupt_prob = (p.in_string_interrupt_prob + 20).min(p.in_string_interrupt_cap);
            }
        }
        self.prev_out = self.out;
    }

    // ---------------------------------------------------------------- candidates

    /// 0x80056764: the candidates (entries usable now) of my list; without `end_links` also the entries of the
    /// END link's next move usable on its first frame, then the exclusions and the filter.
    fn build(&mut self, cx: &mut Cx, end_links: bool) {
        self.cand.clear();
        let cur = cx.me.current;
        for e in cx.my_list() {
            if !cx.in_window(&e) || !cx.req_ok(&e) || !self.cond_ok(cx, &e) {
                continue;
            }
            if self.cand.len() >= 180 {
                break;
            }
            // (an alias that does not resolve would be a garbage record in the game; none is known in the data)
            if let Some(mv) = cx.rec(e.dest) {
                self.cand.push(Cand { mv, e, state: 0 });
            }
        }
        if end_links {
            return;
        }
        let top = cx.top(cur);
        if let Some(t) = top.last().filter(|t| t.cmd == 0xC000 && cx.in_window(t)) {
            if let Some(nx) = cx.me.resolve(t.dest).and_then(|c| Rec::of(cx.gd, cx.me, c)) {
                if nx.id != cx.m.id && nx.l0_cmd != 0xC000 {
                    let next = cx.top(nx.id.unwrap_or(cur));
                    // the plain entries are tested on a stale entry pointer (the list start, or the RETURN after the
                    // last shared sub-list): a compiler artefact kept on purpose
                    let mut stale = next[0];
                    let frame = cx.mf;
                    let push = |s: &mut Self, e: Ent| -> bool {
                        if s.cand.len() >= 180 {
                            return false;
                        }
                        if let Some(mv) = cx.rec(e.dest) {
                            s.cand.push(Cand { mv, e, state: 0 });
                        }
                        true
                    };
                    'all: for top in next.iter().take_while(|t| t.cmd != 0xC000) {
                        if top.cmd == 0xC00C {
                            for k in top.dest..top.dest + top.extra {
                                let Some(e) = cx.shared(k) else { continue };
                                if e.ws < 2 && frame <= e.we && cx.req_ok(&e) && self.link_cond(cx, &e, &e) && !push(self, e) {
                                    break 'all;
                                }
                            }
                            if let Some(r) = cx.shared(top.dest + top.extra) {
                                stale = r;
                            }
                        } else if top.ws < 2 && frame <= top.we && cx.req_ok(top) && self.link_cond(cx, top, &stale) && !push(self, *top) {
                            break 'all;
                        }
                    }
                }
            }
        }
        for c in &mut self.cand {
            if c.mv.rec4 & 0x80_0000 != 0 {
                c.state = -1;
            }
        }
        if self.filter {
            self.apply_filter();
        }
    }

    /// The condition shortcuts the AI uses for `$ % & 0 '` (from `bits64`), else the engine's test.
    fn cond_ok(&self, cx: &Cx, e: &Ent) -> bool {
        let b = self.bits64;
        match e.cond {
            0 => true,
            0x24 => b & 1 != 0,
            0x25 => b & 2 != 0,
            0x26 => b & 4 != 0,
            0x30 => b & 8 != 0,
            0x27 => b & 0x10 != 0,
            _ => cx.engine_cond(e),
        }
    }

    /// The END-link entries' condition: `& 0 '` accepted untested; the engine's test runs on `test`.
    fn link_cond(&self, cx: &Cx, e: &Ent, test: &Ent) -> bool {
        match e.cond {
            0 => true,
            0x24 => self.bits64 & 1 != 0,
            0x25 => self.bits64 & 2 != 0,
            0x26 | 0x30 | 0x27 => true,
            _ => cx.engine_cond(test),
        }
    }

    /// 0x800613CC: exclude low / ground moves.
    fn apply_filter(&mut self) -> i32 {
        let mut k = 0;
        for c in &mut self.cand {
            if c.state >= 0 && (c.mv.rec8 & 0x1_0000 != 0 || c.mv.low16() == 0x10F) {
                if c.state > 0 {
                    k += 1;
                }
                c.state = -1;
            }
        }
        self.npool -= k;
        k
    }

    /// Put the candidates passing `pred` into the pool.
    fn mark(&mut self, pred: impl Fn(&Cand) -> bool) -> i32 {
        let mut k = 0;
        for c in &mut self.cand {
            if c.state == 0 && pred(c) {
                c.state = 1;
                k += 1;
            }
        }
        self.npool += k;
        k
    }

    /// 0x80058A84: input one pool entry at random (on odd ticks counted from the start, where the first entry
    /// is twice as likely and the last never picked; on even ticks from the end). Returns its index or -1.
    fn pick(&mut self, cx: &mut Cx) -> i32 {
        if self.npool <= 0 {
            return -1;
        }
        let mut k = cx.randint(self.npool);
        let n = self.cand.len() as i32;
        let idx = if self.ticks & 1 == 0 {
            let mut i = n - 1;
            loop {
                if i < 0 {
                    break 0;
                }
                if self.cand[i as usize].state > 0 {
                    k -= 1;
                    if k < 0 {
                        break i;
                    }
                }
                i -= 1;
            }
        } else {
            let mut i = 0;
            loop {
                if i >= n {
                    break n - 1;
                }
                if self.cand[i as usize].state > 0 {
                    k -= 1;
                    if k <= 0 {
                        break i;
                    }
                }
                i += 1;
            }
        };
        if idx < 0 || idx >= n {
            return -1;
        }
        let c = self.cand[idx as usize];
        self.cmd_to_pad(cx, c.e.cmd, Some(c.mv));
        // 0x80058958: an attack restarts the cooldown and stops the movement
        if c.mv.rec8 & 0x40_0000 != 0 {
            self.chain += 1;
        }
        if c.mv.as_ != 0 {
            let k = cx.lcg() & 3;
            self.cd = self.p.attack_cooldown_base + self.p.cooldown(k);
            self.cdash = -1;
            if self.jumpt > 0 {
                self.jumpt = -1;
            }
            (self.run3a, self.walk38, self.mvcd40, self.hold48) = (0, 0, 0x28, -1);
        }
        idx
    }

    /// 0x800618C0: the output for a command: a single-frame command now (every listed button), a motion input
    /// played a word a frame. Anything else writes nothing.
    fn cmd_to_pad(&mut self, cx: &Cx, cmd: u32, rec: Option<Rec>) {
        let cmd = cmd & 0xFFFF;
        if cmd & 0xC000 == 0 {
            let dir = (1..=9).rev().find(|&n| cmd & (1 << (4 + n)) != 0).unwrap_or(0);
            self.out = (0..4).filter(|b| cmd & (1 << b) != 0).fold(DIRPAD[dir], |p, b| p | BTN[b]);
            self.last_rec = rec;
            return;
        }
        if cmd & 0xC000 != 0xC000 {
            return;
        }
        // 0x8002CD7C: the dash records, the motion input tables
        let steps: Vec<u16> = match cmd {
            0xC001 | 0xC002 => match cx.data.dash_records.iter().find(|r| r.0 == cmd) {
                Some(r) => r.1.clone(),
                None => return,
            },
            0xC00E..=0xC04C | 0xC7FF..=0xC827 => match cx.gd.shared.sequences.entries.iter().find(|s| s.cmd == cmd) {
                Some(s) => s.steps.iter().map(|st| st.raw as u16).collect(),
                None => return,
            },
            _ => return,
        };
        self.seq = steps.iter().skip(1).copied().take_while(|&w| w != 0).collect();
        self.out = steps.first().map_or(0, |&w| word_to_pad(w));
        self.last_rec = rec;
    }

    /// 0x8006183C: play a canned input: its first word now, the rest a word a frame until a 0.
    fn play(&mut self, seq: &[u16]) {
        self.out = seq.first().map_or(0, |&w| word_to_pad(w));
        self.seq = seq.iter().skip(1).copied().take_while(|&w| w != 0).collect();
    }

    /// 0x80061A58: a sidestep; side -1 picks it from where he faces (random when ambiguous), flipped by the
    /// start sides.
    fn sidestep(&mut self, cx: &mut Cx, side: i32) {
        let side = if side == -1 {
            self.sidestep_side(cx).unwrap_or_else(|| {
                let s = (cx.rand() >> 4) & 1;
                if cx.op.side < cx.me.side {
                    1 - s
                } else {
                    s
                }
            })
        } else {
            side
        };
        let seq = cx.data.sidestep[(side & 1) as usize].clone();
        self.play(&seq);
        self.reset_mv();
        self.sidesteps += 1;
    }

    /// The sidestep side rule without the random fallback.
    fn sidestep_side(&self, cx: &Cx) -> Option<i32> {
        let a = cx.a.opp_off as u16;
        let side = if a.wrapping_sub(0x101) <= 0x7EFE {
            0
        } else if (a as u32 + 0x7FFF) & 0xFFFF <= 0x7EFD {
            1
        } else {
            return None;
        };
        Some(if cx.op.side < cx.me.side { 1 - side } else { side })
    }

    /// 0x80058BE8: `alias` can be input from my list now (or on the END link's next move's first frame).
    fn reachable(&self, cx: &Cx, alias: usize) -> bool {
        let top = cx.top(cx.me.current);
        let Some(&first) = top.first() else { return false };
        // `last` follows the walk: the entry after the one tested (the END entry, or a sub-list's RETURN)
        let mut last = first;
        for (i, t) in top.iter().enumerate().take_while(|(_, t)| t.cmd != 0xC000) {
            if t.cmd == 0xC00C {
                for k in t.dest..t.dest + t.extra {
                    let Some(e) = cx.shared(k) else { continue };
                    last = cx.shared(k + 1).unwrap_or(e);
                    if e.dest == alias && cx.in_window(&e) && cx.req_ok(&e) && self.cond_ok(cx, &e) {
                        return true;
                    }
                }
            } else {
                last = top.get(i + 1).copied().unwrap_or(*t);
                if t.dest == alias && cx.in_window(t) && cx.req_ok(t) && self.cond_ok(cx, t) {
                    return true;
                }
            }
        }
        let Some(nx) = cx.me.resolve(last.dest) else { return false };
        if Some(nx) == cx.m.id || !cx.in_window(&last) {
            return false;
        }
        cx.expanded(nx).iter().any(|e| e.dest == alias && e.ws == 1 && cx.in_window(e) && cx.req_ok(e) && self.cond_ok(cx, e))
    }

    /// 0x80057018: a usable entry of my list leads to a throw.
    fn throw_available(&self, cx: &Cx) -> bool {
        cx.my_list().iter().any(|e| cx.in_window(e) && cx.req_ok(e) && self.cond_ok(cx, e) && cx.rec(e.dest).is_some_and(|r| r.rec8 & 0x40_0000 != 0))
    }

    // ---------------------------------------------------------------- pools (5.6)

    /// 0x800572A4: dashes, hops and in-range attacks (no throws, no jumps).
    fn pool_general(&mut self, cx: &Cx) -> i32 {
        let zm = cx.data.zm[self.myzone as usize];
        self.mark(|c| {
            let m = &c.mv;
            let iz = m.range_class() & zm != 0;
            (m.js == 0 || m.rec4 & 0x8_0000 != 0 || m.rec8 & 0x20_0000 != 0)
                && (c.e.cmd == 0xC001 || m.rec4 & 0x8_0000 != 0 || (m.as_ != 0 && m.rec8 & 0x40_0000 == 0 && m.rec4 & 0x2_0000 == 0 && (iz || m.rec4 & 0x1_0000 != 0)))
        })
    }

    /// 0x800573F4: in-range attacks without highs (0x412) or 0x706.
    fn pool_stepin(&mut self, cx: &Cx) -> i32 {
        let zm = cx.data.zm[self.myzone as usize];
        self.mark(|c| {
            let m = &c.mv;
            let iz = m.range_class() & zm != 0;
            (m.js == 0 || m.rec4 & 0x8_0000 != 0 || m.rec8 & 0x20_0000 != 0)
                && m.as_ != 0
                && !matches!(m.low16(), 0x412 | 0x706)
                && m.rec8 & 0x40_0000 == 0
                && m.rec4 & 0x2_0000 == 0
                && (iz || m.rec4 & 0x1_0000 != 0)
        })
    }

    /// 0x80057538: fast pokes (active before frame 14).
    fn pool_fast_poke(&mut self, cx: &Cx) -> i32 {
        let zm = cx.data.zm[self.myzone as usize];
        self.mark(|c| {
            let m = &c.mv;
            m.w14 > 0 && m.as_ != 0 && m.js == 0 && m.rec8 & 0x40_0000 == 0 && m.as_ < 14 && m.rec4 & 0x2_0000 == 0 && (m.range_class() & zm != 0 || m.rec4 & 0x1_0000 != 0)
        })
    }

    /// 0x8005766C: attacks his posture does not guard.
    fn pool_guard_aware(&mut self, cx: &Cx) -> i32 {
        let o = self.h4;
        let zm = cx.data.zm[self.myzone as usize];
        self.mark(|c| {
            let m = &c.mv;
            let l = m.low16();
            let t = if o & 2 != 0 {
                l != 0x412
            } else if o & 1 != 0 {
                !matches!(l, 0x412 | 0x10F)
            } else if o & 4 != 0 {
                !matches!(l, 0x412 | 0x217)
            } else {
                true
            };
            m.rec4 & 0x2_0000 == 0 && m.as_ != 0 && m.rec8 & 0x40_0000 == 0 && m.js == 0 && m.w14 >= 0 && m.range_class() & zm != 0 && t
        })
    }

    /// 0x800577DC: attacks that suit his posture.
    fn pool_punish_posture(&mut self, cx: &Cx) -> i32 {
        let o = self.h4;
        let zm = cx.data.zm[self.myzone as usize];
        self.mark(|c| {
            let m = &c.mv;
            let l = m.low16();
            let p = if o & 0xFFFF == 0x4C02 {
                true
            } else if o & 0x400 != 0 {
                matches!(l, 0x412 | 0x217 | 0x31F)
            } else if o & 4 != 0 {
                matches!(l, 0x10F | 0x51F)
            } else if o & 1 != 0 {
                matches!(l, 0x217 | 0x10F)
            } else if o & 2 != 0 {
                matches!(l, 0x10F | 0x217 | 0x412)
            } else {
                false
            };
            m.as_ != 0 && m.js == 0 && m.rec8 & 0x40_0000 == 0 && m.rec4 & 0x2_0000 == 0 && m.range_class() & zm != 0 && p
        })
    }

    /// 0x800579A0: as `pool_punish_posture` without highs.
    fn pool_punish_nohigh(&mut self, cx: &Cx) -> i32 {
        let o = self.h4;
        let zm = cx.data.zm[self.myzone as usize];
        self.mark(|c| {
            let m = &c.mv;
            let l = m.low16();
            let p = l != 0x412
                && if o & 0xFFFF == 0x4C02 {
                    true
                } else if o & 0x400 != 0 {
                    matches!(l, 0x217 | 0x31F)
                } else if o & 4 != 0 {
                    matches!(l, 0x10F | 0x51F)
                } else if o & 1 != 0 {
                    matches!(l, 0x217 | 0x10F)
                } else if o & 2 != 0 {
                    matches!(l, 0x10F | 0x217 | 0x412)
                } else {
                    false
                };
            m.as_ != 0 && m.js == 0 && m.rec8 & 0x40_0000 == 0 && m.rec4 & 0x2_0000 == 0 && m.range_class() & zm != 0 && p
        })
    }

    /// 0x80057B60: button-only attacks.
    fn pool_buttons(&mut self, cx: &Cx) -> i32 {
        let zm = cx.data.zm[self.myzone as usize];
        self.mark(|c| {
            let m = &c.mv;
            m.as_ != 0 && c.e.cmd & 0x3FE0 == 0 && m.rec4 & 0x2_0000 == 0 && m.w14 >= 0 && m.rec8 & 0x40_0000 == 0 && m.js == 0 && m.range_class() & zm != 0
        })
    }

    /// 0x80057D58: attacks that are active before his.
    fn pool_interrupt(&mut self, cx: &Cx) -> i32 {
        let (zm, until) = (cx.data.zm[self.myzone as usize], self.until);
        self.mark(|c| {
            let m = &c.mv;
            m.rec8 & 0x40_0000 == 0 && m.w14 > 0 && m.as_ != 0 && (m.js == 0 || m.rec4 & 0x8_0000 != 0) && m.as_ < until && m.rec4 & 0x2_0000 == 0 && m.range_class() & zm != 0
        })
    }

    /// 0x80057E94: as `pool_interrupt` without highs (and without the jump test).
    fn pool_interrupt_nohigh(&mut self, cx: &Cx) -> i32 {
        let (zm, until) = (cx.data.zm[self.myzone as usize], self.until);
        self.mark(|c| {
            let m = &c.mv;
            m.rec8 & 0x40_0000 == 0 && m.w14 > 0 && m.as_ != 0 && m.as_ < until && m.low16() != 0x412 && m.rec4 & 0x2_0000 == 0 && m.range_class() & zm != 0
        })
    }

    /// 0x800587D8: counter attacks: sometimes a dash in (zone 2: 32 / 4096, zone 3: 1024 / 4096), then the
    /// interrupts (without highs against a croucher).
    fn pool_counter(&mut self, cx: &mut Cx) -> i32 {
        let u = (cx.lcg() >> 1) & 0xFFF;
        let mut n = 0;
        if (self.myzone == 2 && u <= 0x1F) || (self.myzone == 3 && u <= 0x3FF) {
            n = self.mark(|c| c.e.cmd == 0xC001);
        }
        n + if self.h4 & 1 == 0 { self.pool_interrupt(cx) } else { self.pool_interrupt_nohigh(cx) }
    }

    /// 0x80057FB8: guard moves (walking back, crouch guard) for his attack's height.
    fn pool_guard(&mut self, cx: &mut Cx) -> i32 {
        if self.dist > 0xB86 {
            return 0;
        }
        let back = |c: &Cand| c.e.cmd == 0xC002;
        let crouch = |c: &Cand| c.mv.rec4 & 1 == 1 && c.mv.as_ == 0;
        if self.hthrow {
            let mut n = 0;
            if self.hmask != 0x217 {
                n = self.mark(crouch);
            }
            if self.hiszone < 3 {
                return n;
            }
            return n + self.mark(back);
        }
        let mut n = 0;
        if self.inc {
            match self.hmask {
                0x31F => n = self.mark(|c| !back(c) && c.mv.rec4 & 0x100 == 0 && matches!(c.mv.rec4 & 0xFFFF, 0x1052 | 0x2829)),
                0x10F => {
                    n = self.mark(|c| c.mv.rec4 & 0xFFFF == 0x2829);
                    if self.hiszone >= 3 {
                        n += self.mark(back);
                    }
                }
                0x217 => n = self.mark(|c| !back(c) && c.mv.rec4 & 0xFFFF == 0x1052),
                0x412 => {
                    if matches!(cx.me.posture(cx.gd) & 0xFFFF, 0x2021 | 0x2829) {
                        n = self.mark(crouch);
                    } else {
                        n = self.mark(|c| !back(c) && c.mv.rec4 & 0xFFFF == 0x1052);
                        if (cx.lcg() & 0xFFF).is_multiple_of(50) {
                            n += self.mark(crouch); // 1/50: duck
                        }
                        if self.hiszone >= 3 {
                            n += self.mark(back);
                        }
                    }
                }
                0x706 => {
                    n = self.mark(crouch);
                    if self.hiszone >= 3 {
                        n += self.mark(back);
                    }
                }
                _ => {}
            }
        }
        if self.ddelta < -0x46 && self.hiszone < 3 {
            n = self.mark(back); // (replaces the count; earlier marks stay in the pool)
        }
        n
    }

    // ---------------------------------------------------------------- per-character handlers (§6)

    /// A handler's answer: -2 handled (it started something), -1 not handled, else the output word.
    fn handler(&mut self, cx: &mut Cx, f: &Routine) -> i32 {
        match f.kind {
            Handler::BackOff => self.back_off(cx, f),
            Handler::CannedThrows => self.canned_throws(cx, f),
            Handler::SpecialMoves => self.special_moves(cx, f),
            Handler::StanceDash => {
                let ok = f.is("stance", cx.me.alias) && ((cx.mf as u16).wrapping_sub(13) > 4 || self.until > 9);
                if !ok || cx.rand() & 0xF == 0 {
                    -1
                } else {
                    0x2000
                }
            }
            Handler::FireDodge => self.fire_dodge(cx, f),
            Handler::CloseIn => {
                if !f.is("attack", cx.op.alias) {
                    return -1;
                }
                if self.hiszone < 2 {
                    self.cd = -1;
                    return -2; // without starting anything: the output repeats
                }
                if self.hiszone < 4 {
                    -1
                } else {
                    0x8000
                }
            }
            Handler::Stub => -1,
        }
    }

    /// 0x800628C4
    fn back_off(&mut self, cx: &Cx, f: &Routine) -> i32 {
        if f.is("attack", cx.op.alias) && self.hiszone < 3 {
            if cx.hf > 15 {
                return 0x8000;
            }
            self.cd = -1;
        }
        -1
    }

    /// 0x8006251C: canned throws.
    fn canned_throws(&mut self, cx: &mut Cx, f: &Routine) -> i32 {
        let a = cx.lcg();
        if self.cd > 0 || (a & 0xFFF) as i32 >= self.p.throw_select_prob {
            return -1;
        }
        let me_idle = cx.m.as_ == 0;
        let (me_q, op_q) = (cx.a.quadrant, cx.a.opp_quadrant);
        let op_unthrowable = cx.op.unthrowable(cx.gd);
        if cx.op_moveset != cx.data.rules.unthrown.moveset
            && self.h4 & 2 != 0
            && self.myzone < 3
            && !cx.op_air
            && !op_unthrowable
            && me_idle
            && self.until > 8
            && me_q == 0
            && op_q == 0
            && cx.rand() & 7 == 0
            && f.aliases.get("reach").and_then(|a| a.first()).is_some_and(|&a| self.reachable(cx, a))
        {
            let k = ((cx.rand() & 0xFFF) % 3) as usize;
            if let Some(seq) = cx.data.throws_stand.get(k).cloned() {
                self.play(&seq);
            }
            self.lc_b = 1;
            return -2;
        }
        if self.h4 & 1 != 0 && self.myzone < 2 && !op_unthrowable && me_idle && self.until >= 9 && me_q == 0 && op_q == 0 && self.throw_available(cx) {
            let k = (cx.rand() & 1) as usize;
            if let Some(seq) = cx.data.throws_crouch.get(k).cloned() {
                self.play(&seq);
            }
            return -2;
        }
        if !self.odown || self.myzone > 1 || !(op_q == 0 || op_q == 2) || me_q != 0 || op_unthrowable || !me_idle || self.until < 9 || self.op_stance || !self.throw_available(cx) {
            return -1;
        }
        let k = (cx.rand() & 1) as usize;
        if let Some(seq) = cx.data.throws_down.get(k).cloned() {
            self.play(&seq);
        }
        -2
    }

    /// 0x80061B88: his special moves.
    fn special_moves(&mut self, cx: &mut Cx, r: &Routine) -> i32 {
        let (a, f, h) = (cx.op.alias, cx.hf, cx.h);
        if r.is("rush", a) {
            if self.hiszone < 3 {
                self.cd = -1;
            }
            return -1;
        }
        if r.is("stance", a) {
            if self.hiszone > 2 {
                return 0x2000;
            }
            self.hcd = 4;
            self.filter = true;
            self.cd = -1;
            return -1;
        }
        if r.is("spin", a) {
            if self.myzone < 1 {
                if h.as_ <= f && f <= h.ae {
                    return 0;
                }
                self.filter = true;
                self.cd = -1;
                self.hcd = 4;
                return -2;
            }
            // his own +0x30 in 0x2000..=0x6000
            let lim = if (cx.a.opp_turn as u16).wrapping_sub(0x2000) < 0x4001 {
                self.hcd = 20;
                if self.myzone < 2 {
                    self.filter = true;
                    self.cd = -1;
                    return -1;
                }
                1
            } else {
                if self.myzone < 3 {
                    if cx.rand() & 7 != 0 || self.m4 & 0x40_0001 != 0 {
                        return 0;
                    }
                    self.sidestep(cx, -1);
                    return -2;
                }
                self.hcd = 10;
                2
            };
            return if self.myzone <= lim { -1 } else { 0x2000 };
        }
        if r.is("ignore", a) {
            return -1;
        }
        if r.is("sweep", a) {
            if h.as_ == 0 || f <= h.ae {
                if cx.a.opp_turn > 0x1000 {
                    return if self.hiszone < 2 { -1 } else { 0x2000 };
                }
                if self.hiszone > 2 && (self.dist as u32) < 0xA8D {
                    let seq = cx.data.backdash.clone();
                    self.play(&seq);
                    return -2;
                }
                if self.hiszone < 2 && self.until > 10 {
                    return -1;
                }
                if self.hiszone < 3 && self.until > 10 {
                    let s = cx.rand() & 1;
                    self.sidestep(cx, s);
                    return -2;
                }
            } else {
                self.cd = -1;
            }
            return 0;
        }
        if r.is("flight", a) {
            if cx.a.opp_turn > 0x3FFF {
                if self.myzone <= 1 {
                    self.cd = -1;
                }
                return -1;
            }
            if self.myzone < 2 {
                self.cd = 1;
                return 0;
            }
            if self.myzone < 3 {
                self.filter = true;
                self.cd = -1;
                return -1;
            }
            return 0x2000;
        }
        -1
    }

    /// 0x80061EF0: his fire breath and his low fire.
    fn fire_dodge(&mut self, cx: &mut Cx, r: &Routine) -> i32 {
        let (f, as_, ae) = (cx.hf, cx.h.as_, cx.h.ae);
        // a sidestep side, if I'm not in a throw-type move and within 7000
        let ss = |s: &Self, cx: &Cx| if cx.m.rec4 & 0x40_0000 == 0 && (s.dist as u32) < 0x1B59 { s.sidestep_side(cx) } else { None };
        // (called twice: harmless)
        let step = |s: &mut Self, cx: &mut Cx, side: i32| {
            s.sidestep(cx, side);
            s.sidestep(cx, side);
            -2
        };
        let tail = |cx: &mut Cx| if cx.rand() & 7 == 0 { 0x2000 } else { 0 };
        if r.is("breath", cx.op.alias) {
            self.hcd = 8;
            if (f as u32).wrapping_sub(9) < 5 && self.myzone < 2 && cx.rand() & 0x1F == 0 {
                self.filter = true;
                self.cd = -1;
                return 0x2000;
            }
            if f < as_ - 30 {
                if self.myzone <= 1 {
                    self.cd = -1;
                    return 0x2000;
                }
                if self.myzone < 3 {
                    self.cd -= 4;
                    return 0x2000;
                }
                return match ss(self, cx) {
                    Some(s) => step(self, cx, s),
                    None => 0x2000,
                };
            }
            if f < as_ - 20 {
                if cx.a.turn > 0x37FF {
                    if self.myzone < 3 {
                        return 0x2000;
                    }
                    return match ss(self, cx) {
                        Some(s) => step(self, cx, s),
                        None => 0x2000,
                    };
                }
                if self.myzone < 3 {
                    return 0;
                }
                if self.myzone < 4 {
                    return 0x2000;
                }
                return tail(cx);
            }
            if f < ae {
                if cx.a.turn > 0x5FFF {
                    self.cd = 8;
                    return 0x8000;
                }
                if cx.a.turn > 0x37FF {
                    if self.myzone > 1 {
                        return 0x2000;
                    }
                    self.cd = 8;
                    return 0;
                }
                return match ss(self, cx) {
                    Some(s) => step(self, cx, s),
                    None => {
                        if cx.a.quadrant == 0 {
                            0x2000
                        } else {
                            0
                        }
                    }
                };
            }
            if f < ae + 40 {
                if self.myzone < 2 {
                    self.cd = -1;
                    return 0x2000;
                }
            } else if self.myzone < 3 {
                self.cd = -1;
                return 0x2000;
            }
            return tail(cx);
        }
        if r.is("low", cx.op.alias) {
            self.hcd = 8;
            if f < as_ - 8 {
                let mut s = None;
                if (cx.a.off as u16).wrapping_add(0x2000) < 0x4001 && cx.m.rec4 & 0x40_0000 == 0 {
                    s = Some((cx.me.side < cx.op.side) as i32);
                    if self.dist as u32 > 7000 {
                        s = None;
                    }
                }
                if self.myzone < 1 {
                    self.cd = -1;
                    return 0x2000;
                }
                if self.myzone < 3 {
                    return 0x2000;
                }
                if cx.rand() & 7 == 0 && cx.a.quadrant == 0 {
                    if let Some(s) = s {
                        self.sidestep(cx, s);
                        return -2;
                    }
                }
                return tail(cx);
            }
            if f >= ae {
                return -1;
            }
            if cx.a.quadrant == 0 {
                return if self.dist < 0x1B59 { 0x6000 } else { 0 }; // d/f
            }
            if self.myzone < 2 {
                let k = (cx.rand() & 3) as usize;
                return cx.data.punish_pads.get(k).copied().unwrap_or(0) as i32; // d/f+2, d/f+1, d/f+4, d/f+4
            }
            if self.myzone < 3 {
                return 0x6000;
            }
            return tail(cx);
        }
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Moveset;
    use crate::{Animations, BodyVolumes, Fight};
    use glam::Vec3;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn random_streams_follow_the_game() {
        let mut r = AiRng::new(1);
        // the AI LCG starts at 0 and tests use the old value
        assert_eq!(r.lcg(), 0);
        assert_eq!(r.lcg(), 3);
        assert_eq!(r.lcg(), 18);
        // libc rand
        let mut r = AiRng::new(1);
        assert_eq!(r.rand(), ((1u32.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039) >> 16) & 0x7FFF) as i32);
        // randint(n) stays below n and steps the LCG once
        let mut r = AiRng::new(7);
        for n in [1, 2, 7, 4096] {
            let before = r.lcg;
            let v = r.randint(n);
            assert!((0..n).contains(&v), "{v} of {n}");
            assert_eq!(r.lcg, before.wrapping_mul(5).wrapping_add(3));
        }
    }

    #[test]
    fn output_words_become_pads() {
        // u/f+2 in the side-0 frame
        assert_eq!(word_to_pad(0x209), 0x3010);
        let p = to_pad(0x3010, true);
        assert!(p.up && p.right && !p.left && p.buttons == 0b0010);
        // forward is left when the opponent is on the left
        let p = to_pad(0xC080, false);
        assert!(p.down && p.right && p.buttons == 1);
        // every button of a word, in its pad bit
        assert_eq!(to_pad(0x80 | 0x10 | 0x40 | 0x20, true).buttons, 0xF);
    }

    struct NoAnims;
    impl Animations for NoAnims {
        fn root(&self, _: &str, _: u32, _: bool) -> Option<Vec3> {
            None
        }
    }

    fn gamedata() -> Option<PathBuf> {
        let root = std::env::var_os("OPENKUMITE_ASSETS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets"));
        let g = root.join("gamedata");
        if kumite_fs::exists(&g.join("ai.json")) {
            Some(g)
        } else {
            eprintln!("skipping: no converted assets in {}", root.display());
            None
        }
    }

    /// The AI tables and a fight between two movesets (whose character ids equal the moveset ids), `gap` units
    /// apart; P2 is the CPU.
    fn setup(p1: i64, p2: i64, gap: f32) -> Option<(AiData, Fight)> {
        let g = gamedata()?;
        let gd = Arc::new(GameData::load(&g).unwrap());
        let ms = |id: i64| Arc::new(crate::data::load::<Moveset>(&g.join(format!("movesets/moveset_{id:02}.json"))).unwrap());
        let mut f = Fight::new(gd, ms(p1), ms(p2));
        for (k, c) in [p1, p2].into_iter().enumerate() {
            f.fighters[k].character = Some(c);
        }
        f.fighters[1].guard_input = false;
        f.fighters[0].position.x = -gap / 2.0;
        f.fighters[1].position.x = gap / 2.0;
        step(&mut f, [Pad::default(); 2]);
        Some((AiData::load(&g.join("ai.json")).unwrap(), f))
    }

    fn step(f: &mut Fight, pads: [Pad; 2]) {
        let bodies = [BodyVolumes::default(), BodyVolumes::default()];
        f.step(pads, &bodies, [&NoAnims, &NoAnims]);
    }

    /// P2's pad from its CPU.
    fn think(ai: &mut Ai, data: &AiData, rng: &mut AiRng, f: &Fight) -> Pad {
        let s = Sight { gd: &f.data, me: &f.fighters[1], op: &f.fighters[0], facing_right: f.facing_right(1), opp_chest: 1425.0, ball: None, force: None };
        ai.think(data, rng, &s)
    }

    fn rng(seed: u32) -> AiRng {
        AiRng { lcg: seed.wrapping_mul(0x9E37_79B9), rand: seed }
    }

    #[test]
    fn handlers_are_picked_by_moveset() {
        let Some(g) = gamedata() else { return };
        let data = AiData::load(&g.join("ai.json")).unwrap();
        use Handler::*;
        let kinds = |(a, b): (Option<usize>, Option<usize>)| (a.map(|k| data.routines[k].kind), b.map(|k| data.routines[k].kind));
        // (against it, its own)
        for (ms, want) in [
            (0, (Some(BackOff), None)),
            (1, (None, None)),
            (3, (None, Some(CannedThrows))),
            (4, (Some(SpecialMoves), Some(Stub))),
            (6, (None, Some(StanceDash))),
            (10, (Some(Stub), None)),
            (14, (Some(FireDodge), None)),
            (19, (Some(CloseIn), None)),
            (20, (None, None)),
        ] {
            assert_eq!(kinds(data.handlers_of(Some(ms))), want, "moveset {ms}");
        }
        // the aliases they compare
        let r = |ms: i64| &data.routines[data.handlers_of(Some(ms)).0.unwrap()];
        assert!(r(0).is("attack", 0x2C7) && r(14).is("breath", 0xDE4) && r(14).is("low", 0x16E) && r(19).is("attack", 0x137));
        assert_eq!(r(4).aliases["spin"], [0x459, 0x45A, 0x45B]);
        assert_eq!(r(4).aliases["flight"], [0x458, 0x49C, 0x49D, 0x49E]);
        assert!(data.routines[data.handlers_of(Some(3)).1.unwrap()].is("reach", 0x15));
        assert!(data.routines[data.handlers_of(Some(6)).1.unwrap()].is("stance", 0xDE3));
    }

    #[test]
    fn the_rules_of_particular_moves_are_the_routines() {
        // the tests 0x8005A8E0 and 0x8005AB28 make, as the code compares them
        let Some(g) = gamedata() else { return };
        let data = AiData::load(&g.join("ai.json")).unwrap();
        let chain = |dest: usize, ms: i64, coin: u32| match dest {
            0x3A7 => false,
            0xD41 => ms != 4 || coin != 0,
            0xD24 | 0xD32 => !(ms == 0 || ms == 13) || coin == 0,
            0xDE2 => !(ms == 0 && coin != 0),
            0xD25 | 0xD34 => !(ms == 0 || ms == 13),
            _ => true,
        };
        let escape = |ms: i64, alias: usize| matches!(alias, 0x189 | 0x18E | 0x17F | 0x181) || ((ms == 9 || ms == 0) && alias == 0x172) || (ms == 3 && alias == 0x152);
        for ms in -1..24 {
            for dest in [0x3A7, 0xD41, 0xD24, 0xD32, 0xDE2, 0xD25, 0xD34, 0x172, 0x152, 0x189, 0x18E, 0x17F, 0x181, 0x100] {
                for coin in [0, 0x10, 0x70] {
                    assert_eq!(data.chain_ok(dest, ms, coin), chain(dest, ms, coin), "chain to {dest:#X} against {ms}, coin {coin}");
                }
                assert_eq!(data.escapable(ms, dest), escape(ms, dest), "escape {dest:#X} of {ms}");
            }
        }
        let r = &data.rules;
        assert_eq!((r.late_parry.moveset, r.late_parry.until, r.late_parry.alias), (9, [22, 26], 0x22C));
        assert_eq!((r.fire.moveset, r.fire.descriptor, r.low_fire.moveset, r.low_fire.descriptor), (14, 0x18, 19, 0x1A));
        assert_eq!((r.stance.moveset, r.stance.alias, r.unthrown.moveset, r.counter_alias.alias), (4, 0x456, 19, 6));
        assert_eq!((r.evade_closer.moveset, r.evade_closer.distance, &r.parries.aliases[..]), (4, -300, &[0x1EE, 0x6A3][..]));
        assert_eq!((&r.wake_forward.movesets[..], &r.turn_by_facing.movesets[..]), (&[18][..], &[7][..]));
    }

    #[test]
    fn a_cpu_walks_away_from_pauls_stance_attack() {
        // vs the first character (0x800628C4): his alias 0x2C7 past frame 15 with me in his zones 1..2 -> walk back
        // (the handler sets the walk timer to 16)
        let Some((data, mut f)) = setup(0, 1, 900.0) else { return };
        assert!(f.fighters[0].play_alias(0x2C7));
        f.fighters[0].frame = 20;
        let right = f.facing_right(1);
        let walked = |f: &Fight| {
            (0..40)
                .filter(|&seed| {
                    let mut ai = Ai::new(2, 9, 1);
                    let p = think(&mut ai, &data, &mut rng(seed), f);
                    ai.walk38 == 15 && (if right { p.left } else { p.right }) && !p.down
                })
                .count()
        };
        // the handler runs with P3A (3891 / 4096)
        let n = walked(&f);
        assert!(n >= 30, "walked back in {n} of 40");
        // not from further away
        f.fighters[0].position.x -= 3000.0;
        step(&mut f, [Pad::default(); 2]);
        f.fighters[0].frame = 20;
        assert_eq!(walked(&f), 0);
    }

    #[test]
    fn a_thrown_cpu_tries_one_escape_per_victim_move() {
        // the second character's throw victim move 0xBBA: its list escapes with button 1 (cond 0x3A)
        let Some((data, mut f)) = setup(0, 1, 600.0) else { return };
        assert!(f.fighters[1].play_alias(0xBBA));
        f.fighters[1].chain = -1;
        // a throw the CPU always tries to escape
        f.fighters[0].alias = 0x189;
        for seed in 0..20 {
            let (mut ai, mut r) = (Ai::new(2, 9, 1), rng(seed));
            let first = think(&mut ai, &data, &mut r, &f);
            assert_eq!(first, Pad { buttons: 1, ..Pad::default() }, "seed {seed}");
            assert_eq!(think(&mut ai, &data, &mut r, &f), Pad::default(), "one try per victim move");
        }
        // other throws: with P12 (1433 / 4096 at HARD 9), and never at EASY 0 (P12 = 0)
        f.fighters[0].alias = ALIAS_STANCE;
        let tries = |d: usize, l: usize| (0..60).filter(|&seed| think(&mut Ai::new(d, l, 1), &data, &mut rng(seed), &f).buttons != 0).count();
        let hard = tries(2, 9);
        assert!((5..40).contains(&hard), "HARD 9 tried {hard} of 60");
        assert_eq!(tries(0, 0), 0);
    }

    const ALIAS_STANCE: usize = crate::ALIAS_NEUTRAL;

    #[test]
    fn a_downed_cpu_taps_2_at_its_profile_intervals() {
        // the lying move 0x99 waits for the stun to drain (cond 0x31): tap 2 every P[00 + rand & 3] + 1 frames
        let Some((data, mut f)) = setup(0, 1, 1500.0) else { return };
        assert!(f.fighters[1].play_alias(0x99));
        let (mut ai, mut r) = (Ai::new(2, 9, 1), rng(3));
        let pads: Vec<Pad> = (0..80).map(|_| think(&mut ai, &data, &mut r, &f)).collect();
        assert_eq!(pads[0].buttons, 2, "the first tap comes at once");
        let intervals = data.profile(2, 9).wake_up_intervals;
        let taps: Vec<usize> = (0..pads.len()).filter(|&k| pads[k].buttons == 2).collect();
        assert!(taps.len() > 10);
        for w in taps.windows(2) {
            assert!(intervals.contains(&(w[1] as i32 - w[0] as i32 - 1)), "gap {} not in {intervals:?}", w[1] - w[0] - 1);
        }
        assert!(pads.iter().all(|p| p.buttons & !2 == 0 && !p.up && !p.down && !p.left && !p.right));
    }

    #[test]
    fn a_cpu_reacts_to_an_incoming_mid() {
        // the first character's mid 0x109 (active on frame 15) from close range: the CPU guards it (back, or a guard
        // move) or interrupts it with a faster attack
        let Some((data, f0)) = setup(0, 1, 900.0) else { return };
        let (mut guards, mut counters, mut blocking) = (0, 0, 0);
        for seed in 0..30 {
            let mut f = f0.clone();
            assert!(f.fighters[0].play_alias(0x109));
            let (mut ai, mut r) = (Ai::new(2, 9, 1), rng(seed));
            let right = f.facing_right(1);
            let (mut guarded, mut attacked) = (false, false);
            while f.fighters[0].frame < 15 && f.fighters[0].alias == 0x109 {
                let p = think(&mut ai, &data, &mut r, &f);
                guarded |= (if right { p.left } else { p.right }) && !p.down;
                attacked |= p.buttons != 0;
                step(&mut f, [Pad::default(), p]);
            }
            // in a standing guard (a guard move: the CPU has no guard input) when it lands
            blocking += (f.fighters[1].posture(&f.data) & crate::posture::STAND_GUARD != 0) as i32;
            guards += guarded as i32;
            counters += (attacked && !guarded) as i32;
        }
        eprintln!("guards {guards}, counters {counters}, standing guard at the hit {blocking}");
        assert!(guards >= 10, "guarded {guards} of 30");
        assert!(blocking >= 5, "standing guard in {blocking} of 30");
        assert!(counters >= 3, "countered {counters} of 30");
    }
}
