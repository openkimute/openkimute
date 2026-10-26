//! A versus match: rounds (round machine 0x8003CB84: round call with the camera fly-in inside it, fight, 90
//! K.O. frames, the half-speed replay, the win pose or the loser shot, next round or match end), wins, win
//! poses and CPU players.

use crate::{CameraMode, Game};
use kumite_sim::ai::{Ai, AiData, AiRng, ForceSight, Sight};
use kumite_sim::camera::EndCam;
use kumite_sim::input::Pad;
use kumite_sim::RoundState;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// The round call (0x8003DAC4, sound.json `announcer.call_frames`): frames at which "ROUND", the number and
/// "FIGHT" are called; the fly-in (120 / 53 frames) plays inside it. The first round's call is longer.
pub const CALL_FIRST: [u32; 3] = [61, 100, 150];
pub const CALL_OTHER: [u32; 3] = [1, 40, 90];
/// Length of a later round's call ("FIGHT" ends it).
pub const CALL_FRAMES: u32 = CALL_OTHER[2];
/// Win motions (move ids 0xD67..0xD6A, rules.json `win_lose_motions`): picked by the frame counter's parity
/// or a held button (0x8002D45C).
pub const WIN_POSES: [usize; 4] = [0xD67, 0xD68, 0xD69, 0xD6A];

/// The fly-in variant counter (0x8009854C): never reset, so variants run on across rounds and matches
/// (the first round after start-up uses variant 1).
static INTRO_COUNTER: AtomicUsize = AtomicUsize::new(0);
/// Winner camera A / B toggle (0x800A049C) and the replay camera's variant toggle (0x800A04CC): B first.
static WINNER_TOGGLE: AtomicUsize = AtomicUsize::new(0);
static REPLAY_TOGGLE: AtomicUsize = AtomicUsize::new(0);
/// The camera counters as they are now (a replay records them: a bout starts from them).
pub fn counters() -> [usize; 3] {
    COUNTERS.map(|c| c.load(Ordering::Relaxed))
}

/// Set the camera counters (a replay's, before its bout is made; the ones before it, after).
pub fn set_counters(values: [usize; 3]) {
    for (c, v) in COUNTERS.iter().zip(values) {
        c.store(v, Ordering::Relaxed);
    }
}

/// One step's input to a match, as [`Bout::step`] takes it: its two pads, and the seats' pads set before it (Force's
/// and Ball's partner, Ball's other partner played by a person, 3P and 4P).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepInput {
    pub pads: [Pad; 2],
    pub partner: Pad,
    pub extra: Option<Pad>,
    pub more: [Option<Pad>; 2],
}

/// A match's inputs, step by step from its first (a replay). Filled chunks are shared by the match's copies (online,
/// the rollback copies the match every frame; a copy restored takes the tape back to its length).
#[derive(Clone, Default)]
pub struct Tape {
    chunks: Vec<Arc<[StepInput]>>,
    open: Vec<StepInput>,
}

impl Tape {
    const CHUNK: usize = 512;

    pub fn push(&mut self, input: StepInput) {
        self.open.push(input);
        if self.open.len() == Self::CHUNK {
            self.chunks.push(std::mem::take(&mut self.open).into());
        }
    }

    pub fn len(&self) -> usize {
        self.chunks.len() * Self::CHUNK + self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn to_vec(&self) -> Vec<StepInput> {
        self.chunks.iter().flat_map(|c| c.iter().copied()).chain(self.open.iter().copied()).collect()
    }
}

/// The three, in this order: a bout counts on its own copy (rolled back with it: online, every computer counts
/// alike) and writes it back for the next bout. Online the statics then hold a mispredicted frame's count at times:
/// a bout must never be made inside a rollback session, only between matches, after [`reset_camera_counters`].
static COUNTERS: [&AtomicUsize; 3] = [&INTRO_COUNTER, &WINNER_TOGGLE, &REPLAY_TOGGLE];
const INTRO: usize = 0;
const WINNER: usize = 1;
const REPLAY: usize = 2;

/// Online, before a match's bout is made: the camera counters from zero on every computer (each has its own
/// offline history; the intro, replay and winner shots would differ).
pub fn reset_camera_counters() {
    for c in COUNTERS {
        c.store(0, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Free fights (practice, sandbox): the camera fly-in, then the fight.
    Intro,
    /// "Round N" ... "Fight!" with the fly-in inside: fighters wait.
    Call,
    Fight,
    /// The 90 frames after a K.O. or time up (fight camera; a human winner can still move). `winner` None = draw.
    RoundOver {
        winner: Option<usize>,
    },
    /// The half-speed replay of the K.O.
    Replay {
        winner: Option<usize>,
    },
    /// The win pose on the winner camera, the loser shot (a CPU won the match), or the draw's wait.
    Result {
        winner: Option<usize>,
    },
    MatchOver {
        winner: Option<usize>,
    },
    /// The match stopped between rounds for a cut-scene (the boss-change scene); the app continues it.
    Interlude,
}

/// Something to show or announce this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Announcement {
    /// "ROUND" (with its number shown).
    Round(u32),
    /// The spoken number after "ROUND".
    RoundNumber(u32),
    FinalRound,
    Fight,
    // Round result: one of these, in the game's order (0x8003DEC0): time up, perfect, double K.O., great
    // (the winner had less than 5 % left), K.O.
    TimeUp,
    Perfect,
    DoubleKo,
    Great,
    Ko,
    /// Match winner (player index).
    Wins(usize),
    Draw,
}

/// The replay in progress: play position in half frames over the record, its end.
#[derive(Clone, Copy, Debug)]
struct ReplayRun {
    pos: usize,
    end: usize,
    /// Ball: where each pass starts and the passes left after this one (all but the last at full speed).
    start: usize,
    passes_left: u32,
}

/// How the result phase ends (0x8003D27C).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultKind {
    /// The winner's motion ends it.
    Pose(usize),
    /// The loser shot: 60 frames.
    Loser,
    /// No winner: 75 frames.
    Draw,
}

#[derive(Clone)]
pub struct Bout {
    pub game: Game,
    pub wins: [u32; 2],
    pub rounds_to_win: u32,
    /// 1-based round number.
    pub round: u32,
    pub phase: Phase,
    /// Frames spent in the current phase.
    pub phase_frame: u32,
    /// CPU controller per player (None = pad input).
    pub cpu: [Option<Ai>; 2],
    ai_data: Arc<AiData>,
    /// The game's random streams the CPUs draw from (shared by both, kept for the whole match).
    ai_rng: AiRng,
    /// Announcements made during the last step.
    pub events: Vec<Announcement>,
    /// Round fly-ins on (off: straight to the round call).
    pub intros: bool,
    /// Free fight (sandbox, practice): no round call, the round never ends; the fight just goes on.
    pub free: bool,
    rng: u32,
    /// The K.O.: its frame number in the record (counted from the start) and the attacker's frame in its move
    /// (the replay starts 30 frames before that move did).
    ko_at: Option<(u64, u32)>,
    replay: Option<ReplayRun>,
    /// START was pressed during the replay (it ends; with rounds to go the pose is skipped too).
    skip: bool,
    skipped: bool,
    result: ResultKind,
    /// The pads of the last step (a held button picks the win pose).
    last_pads: [Pad; 2],
    /// Total frames stepped (the win pose parity).
    frames: u64,
    /// Stop for the boss-change scene once the human has won a round (0x80051204): set by the app at the arcade's
    /// last stage against its boss.
    pub interlude_check: bool,
    /// Force: the other soldier's CPU, and no replay or result after the K.O. (the app takes over).
    pub extra_cpu: Option<Ai>,
    pub force: bool,
    /// Force's co-op: the second player's pad (the partner's, while the round is fought).
    pub partner_pad: Pad,
    /// Ball 2 ON 2: the players' side's partner's CPU (the other side's is `extra_cpu`).
    pub partner_cpu: Option<Ai>,
    /// Ball 2 ON 2: the other side's partner (actor 2) played by a person: its pad.
    pub extra_human: Option<Pad>,
    /// Force for more than two: actors 4.. — the CPU of each soldier or boss there, the pad of each player
    /// (3P, 4P).
    pub more_cpu: Vec<Option<Ai>>,
    pub more_human: Vec<Option<Pad>>,
    /// The match's inputs recorded as it is played (a replay), when on.
    pub tape: Option<Tape>,
    /// A replay played: its inputs, and the next step's.
    playback: Option<(Arc<[StepInput]>, usize)>,
    /// The camera counters (intro variant, winner camera, replay camera): see [`COUNTERS`].
    counters: [usize; 3],
}

impl Bout {
    pub fn new(game: Game, ai_data: Arc<AiData>, rounds_to_win: u32, intros: bool) -> Self {
        let mut b = Self {
            game,
            wins: [0; 2],
            rounds_to_win: rounds_to_win.max(1),
            round: 0,
            phase: Phase::Intro,
            phase_frame: 0,
            cpu: [None, None],
            ai_data,
            // (the game seeds libc rand once at boot; a fixed seed keeps equal inputs giving equal fights)
            ai_rng: AiRng::new(0x2545_F491),
            events: Vec::new(),
            intros,
            free: false,
            rng: 0x1234_5678,
            ko_at: None,
            replay: None,
            skip: false,
            skipped: false,
            result: ResultKind::Draw,
            last_pads: [Pad::default(); 2],
            counters: COUNTERS.map(|c| c.load(Ordering::Relaxed)),
            frames: 0,
            interlude_check: false,
            extra_cpu: None,
            force: false,
            partner_pad: Pad::default(),
            partner_cpu: None,
            extra_human: None,
            more_cpu: Vec::new(),
            more_human: Vec::new(),
            tape: None,
            playback: None,
        };
        b.next_round();
        b
    }

    /// Go on with a match after a cut-scene swapped the opponent (0x8002A384 / 0x8002A3E0): the round number and
    /// the wins are kept (all reset when one round wins a match); the next round starts.
    pub fn resume_match(&mut self, round: u32, wins: [u32; 2]) {
        if self.rounds_to_win > 1 {
            (self.round, self.wins) = (round, wins);
        } else {
            (self.round, self.wins) = (0, [0; 2]);
        }
        self.next_round();
    }

    /// Play the match from these inputs (a replay): each step takes the next, whatever is passed to it.
    pub fn play(&mut self, inputs: Arc<[StepInput]>) {
        self.playback = Some((inputs, 0));
    }

    /// The steps played from a replay's inputs so far.
    pub fn played(&self) -> usize {
        self.playback.as_ref().map_or(0, |p| p.1)
    }

    /// This step's people's input: the replay's (its seats' pads set), or `pads` with the seats as set, recorded on
    /// the tape. (The CPUs decide after, from the match: in a replay as when it was played.)
    fn taped(&mut self, pads: [Pad; 2]) -> [Pad; 2] {
        if let Some((inputs, at)) = self.playback.as_mut() {
            let s = inputs.get(*at).copied().unwrap_or_default();
            *at += 1;
            (self.partner_pad, self.extra_human) = (s.partner, s.extra);
            for (slot, pad) in self.more_human.iter_mut().zip(s.more) {
                *slot = pad;
            }
            return s.pads;
        }
        if let Some(tape) = self.tape.as_mut() {
            let more = std::array::from_fn(|k| self.more_human.get(k).copied().flatten());
            tape.push(StepInput { pads, partner: self.partner_pad, extra: self.extra_human, more });
        }
        pads
    }

    /// Make player `i` CPU-controlled (difficulty 0..2 = EASY/MEDIUM/HARD, level 0..9), or human.
    pub fn set_cpu(&mut self, i: usize, ai: Option<(usize, usize)>) {
        self.cpu[i] = ai.map(|(d, l)| Ai::new(d, l, i));
        // the guard input flag (+0xCA) is set for human players only: a CPU guards with guard moves and pads
        self.game.fight.fighters[i].guard_input = ai.is_none();
    }

    /// A free fight: the round starts at once (after the fly-in if any) and never ends.
    pub fn free_fight(mut self) -> Self {
        self.free = true;
        self.events.clear();
        self.phase = if self.game.camera.in_intro() { Phase::Intro } else { Phase::Fight };
        self
    }

    /// Counter `k`'s value, then counted on (here, and for the next bout).
    fn count(&mut self, k: usize) -> usize {
        let v = self.counters[k];
        self.counters[k] = v.wrapping_add(1);
        COUNTERS[k].store(self.counters[k], Ordering::Relaxed);
        v
    }

    fn next_round(&mut self) {
        self.round += 1;
        // the CPUs' round setup (0x80059654) runs again
        for ai in self.cpu.iter_mut().flatten() {
            ai.new_round();
        }
        let variant = (self.count(INTRO) + 1) % 10;
        let intro = self.intros.then_some((variant, self.round == 1));
        self.game.start_round(intro);
        self.phase = if self.free && self.game.camera.in_intro() {
            Phase::Intro
        } else if self.free {
            Phase::Fight
        } else {
            Phase::Call
        };
        self.phase_frame = 0;
        self.ko_at = None;
        self.replay = None;
        self.skip = false;
        self.skipped = false;
    }

    /// The final round: the round limit of a best-of-N match (2N - 1) is reached (0x8003DB00).
    pub fn is_final_round(&self) -> bool {
        self.round >= 2 * self.rounds_to_win - 1
    }

    /// The round call's frames for this round.
    fn call(&self) -> [u32; 3] {
        if self.round == 1 {
            CALL_FIRST
        } else {
            CALL_OTHER
        }
    }

    /// The match is over and its end shown (the app moves on to the next stage, continue or select).
    pub fn finished(&self) -> bool {
        matches!(self.phase, Phase::MatchOver { .. })
    }

    /// The match is decided (someone has the wins it needs).
    fn decided(&self) -> bool {
        self.wins.iter().any(|&w| w >= self.rounds_to_win)
    }

    /// START: skips the replay (and, with rounds to go, the win pose), or ends the win pose.
    pub fn skip(&mut self) {
        match self.phase {
            Phase::Replay { .. } => self.skip = true,
            Phase::Result { .. } if self.phase_frame >= 1 => self.skip = true,
            _ => {}
        }
    }

    /// While the replay plays: the record index shown, and whether this step reached it (the game advances
    /// effects and recorded sounds on even half-frames only, 0x8003368C).
    pub fn replay_frame(&self) -> Option<(usize, bool)> {
        match (self.phase, self.replay) {
            (Phase::Replay { .. }, Some(r)) => Some((r.pos / 2, r.pos % 2 == 0)),
            _ => None,
        }
    }

    /// The replay's blinking "REPLAY" is on (16 frames on, 16 off).
    pub fn replay_text(&self) -> bool {
        matches!(self.phase, Phase::Replay { .. }) && self.frames & 0x10 != 0
    }

    fn random(&mut self, n: i64) -> i64 {
        self.rng = self.rng.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (((self.rng >> 16) as i64 & 0x7FFF) * n.max(1)) >> 15
    }

    /// One 60 Hz step. `pads` are used for the human players; CPU players think for themselves.
    pub fn step(&mut self, pads: [Pad; 2]) {
        let pads = self.taped(pads);
        self.events.clear();
        // START pressed (online: from the pads, every computer on the same frame) skips the replay or the win pose
        let start = |p: &[Pad; 2]| p.iter().any(|p| p.start);
        if start(&pads) && !start(&self.last_pads) && matches!(self.phase, Phase::Replay { .. } | Phase::Result { .. }) {
            self.skip();
        }
        self.phase_frame += 1;
        self.frames += 1;
        self.last_pads = pads;
        // the heads follow the opponent only while the round is on (round state 2), and not Force's
        // CPU soldiers
        self.game.head_live = self.phase == Phase::Fight;
        self.game.head_off = std::array::from_fn(|i| self.force && i == 1 && self.cpu[i].is_some());
        self.game.partner_pad = if self.phase == Phase::Fight { self.partner_pad } else { Pad::default() };
        if let (Some(pad), None) = (self.extra_human, self.extra_cpu.as_ref()) {
            self.game.extra_pad = if self.phase == Phase::Fight { pad } else { Pad::default() };
        }
        for (k, pad) in self.more_human.iter().enumerate() {
            if let (Some(pad), Some(slot)) = (pad, self.game.more_pad.get_mut(k)) {
                *slot = if self.phase == Phase::Fight { *pad } else { Pad::default() };
            }
        }
        match self.phase {
            Phase::Intro => {
                self.game.step([Pad::default(); 2]);
                if !self.game.camera.in_intro() {
                    self.phase = Phase::Fight;
                    self.phase_frame = 0;
                }
            }
            Phase::Call => {
                // the round's start moves (0x8002D31C), picked on its first frame: round 1 an intro (a held □ / △
                // picks alias 0, × alias 2, else a coin flip, P1 first; the CPU's idle pad always flips), later
                // rounds alias 1. Inputs are off for the whole call (input mode 0); each start move's END link takes
                // the fighter to its stance by the FIGHT cue
                if self.phase_frame == 1 {
                    let aliases: [usize; 2] = std::array::from_fn(|i| {
                        let b = if self.cpu[i].is_some() { 0 } else { pads[i].buttons };
                        if self.round > 1 {
                            1
                        } else if b & 0b0011 != 0 {
                            0
                        } else if b & 0b0100 != 0 {
                            2
                        } else if self.random(2) == 1 {
                            0
                        } else {
                            2
                        }
                    });
                    self.game.set_start_moves(aliases);
                }
                // fighters play them while the round is called (the fly-in plays inside the call); the clock waits
                let clock = self.game.fight.clock;
                self.game.step([Pad::default(); 2]);
                self.game.fight.clock = clock;
                let [round, number, fight] = self.call();
                let f = self.phase_frame;
                if f == round {
                    self.events.push(if self.is_final_round() { Announcement::FinalRound } else { Announcement::Round(self.round) });
                } else if f == number && !self.is_final_round() {
                    self.events.push(Announcement::RoundNumber(self.round));
                } else if f >= fight {
                    self.phase = Phase::Fight;
                    self.phase_frame = 0;
                    self.events.push(Announcement::Fight);
                }
            }
            Phase::Fight => {
                let pads = self.input(pads);
                self.game.step(pads);
                if self.free {
                    return;
                }
                match self.game.fight.state {
                    RoundState::Fighting => {}
                    RoundState::Ko { winner } => {
                        let double = self.game.fight.fighters.iter().all(|f| f.health <= 0);
                        // the lethal hit's attacker: the replay starts 30 frames before its move did
                        let attacker = self.game.fight.hits.last().map_or(winner, |h| h.attacker);
                        let frame = self.game.fight.actor(attacker).map_or(0, |f| f.frame);
                        self.ko_at = Some((self.game.recorded_total.saturating_sub(1), frame));
                        self.round_over(if double { None } else { Some(winner) }, false);
                    }
                    RoundState::TimeUp { winner } => self.round_over(winner, true),
                }
            }
            Phase::RoundOver { winner } => {
                // a human winner keeps control for these 90 frames; the loser and a CPU winner stand by
                let mut p = [Pad::default(); 2];
                if let Some(w) = winner {
                    if self.cpu[w].is_none() && self.ko_at.is_some() {
                        p[w] = pads[w];
                    }
                }
                self.game.step(p);
                if self.phase_frame >= self.game.camera.data().end.ko_frames {
                    if self.force {
                        self.phase = Phase::MatchOver { winner };
                        self.phase_frame = 0;
                    } else if self.ko_at.is_some() {
                        self.start_replay(winner);
                    } else {
                        self.start_result(winner);
                    }
                }
            }
            Phase::Replay { winner } => {
                let Some(mut r) = self.replay else {
                    self.start_result(winner);
                    return;
                };
                // Ball's earlier passes run at full speed, the last at half (0x80032398)
                r.pos += if r.passes_left > 0 { 2 } else { 1 };
                if !self.skip && r.pos >= r.end && r.passes_left > 0 {
                    r.passes_left -= 1;
                    r.pos = r.start;
                }
                if self.skip || r.pos >= r.end {
                    self.skipped = self.skip;
                    self.skip = false;
                    self.game.show(None);
                    self.start_result(winner);
                    return;
                }
                self.replay = Some(r);
                self.game.show(Some((r.pos / 2, r.pos % 2 == 1)));
            }
            Phase::Result { winner } => {
                self.game.step([Pad::default(); 2]);
                let end = &self.game.camera.data().end;
                let (cap, loser_frames, draw_frames) = (end.pose_cap, end.loser_frames, end.draw_frames);
                let done = match self.result {
                    ResultKind::Pose(w) => {
                        let f = &self.game.fight.fighters[w];
                        let mv = f.current_move(&self.game.fight.data);
                        !self.posing(w) || f.frame + 1 >= mv.frames
                    }
                    // Ball's loser shot lasts 240 frames
                    ResultKind::Loser => self.phase_frame >= if self.game.ball.is_some() { 240 } else { loser_frames },
                    ResultKind::Draw => self.phase_frame >= draw_frames,
                };
                if done || self.skip || self.phase_frame >= cap {
                    self.skip = false;
                    self.end_result(winner);
                }
            }
            Phase::MatchOver { .. } => self.game.step([Pad::default(); 2]),
            Phase::Interlude => {}
        }
    }

    /// Fighter `w` is still in its win motion.
    fn posing(&self, w: usize) -> bool {
        let f = &self.game.fight.fighters[w];
        let aliases = f.moveset.aliases.as_ref();
        WIN_POSES.iter().any(|&p| aliases.and_then(|a| a.get(p)).is_some_and(|r| r.bank == f.current.bank && r.index == Some(f.current.index)))
    }

    /// The round is over (0x8003E474 sets the result flags, 0x8003DEC0 calls one of them).
    fn round_over(&mut self, winner: Option<usize>, time_up: bool) {
        // health in bar units of 0x1000 of each fighter's own full health, as the game compares them
        let units = |i: usize| {
            let f = &self.game.fight.fighters[i];
            (f.health.max(0) as i64 * 0x1000 / (f.max_health.max(1) * kumite_sim::ONE) as i64) as u32
        };
        let call = match winner {
            _ if time_up => Announcement::TimeUp,
            Some(w) if units(w) >= 0x1000 => Announcement::Perfect,
            None => Announcement::DoubleKo,
            Some(w) if units(w) < 0xCD => Announcement::Great,
            Some(_) => Announcement::Ko,
        };
        self.events.push(call);
        if time_up {
            self.ko_at = None; // no replay after a time up
        }
        match winner {
            Some(w) => self.wins[w] += 1,
            None => {
                // a draw scores for both (and can end the match level)
                self.wins[0] += 1;
                self.wins[1] += 1;
            }
        }
        self.phase = Phase::RoundOver { winner };
        self.phase_frame = 0;
    }

    /// The replay (0x80031E48): from 30 frames before the K.O. move started, at least 150 frames, at half speed,
    /// on the replay camera (variants alternate, B first).
    fn start_replay(&mut self, winner: Option<usize>) {
        let r = self.game.camera.data().end.replay.clone();
        let len = self.game.recorded.len();
        let Some((ko_total, frame)) = self.ko_at else { return self.start_result(winner) };
        // the record's index of the K.O. frame (the ring drops its oldest frames)
        let ko = ko_total as i64 - (self.game.recorded_total as i64 - len as i64);
        let mut start = ko - frame as i64 - r.pre_roll as i64;
        if (len as i64 - start) < r.min_frames as i64 && len >= r.min_frames as usize + 3 {
            start = len as i64 - r.min_frames as i64;
        }
        // Ball: always the last 30 frames (the ball's ring), in passes; none for 2 ON 2 (the ring keeps
        // the two fighters only)
        if self.game.fight.ball_doubles() {
            return self.start_result(winner);
        }
        let ball = self.game.ball.is_some();
        let start = if ball { len as i64 - 30 } else { start };
        let start = start.clamp(0, len.saturating_sub(2) as i64) as usize;
        if len < 2 {
            return self.start_result(winner);
        }
        let passes = self.game.fight.ball.as_ref().map_or(1, |b| b.replay_passes.max(1));
        let run = ReplayRun { pos: 2 * start, end: 2 * len - 2, start: 2 * start, passes_left: passes - 1 };
        self.replay = Some(run);
        self.game.show(Some((start, false)));
        let variant_a = self.count(REPLAY) % 2 == 1;
        let samples = self.game.replay_samples();
        let mut rng = self.rng;
        let mut random = |n: i64| {
            rng = rng.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (((rng >> 16) as i64 & 0x7FFF) * n.max(1)) >> 15
        };
        self.game.camera.start_replay(samples, variant_a, &mut random);
        self.rng = rng;
        // Ball keeps the live director through the replay
        self.game.camera_mode = if ball { CameraMode::Fight } else { CameraMode::Replay };
        self.game.step_camera_only();
        self.phase = Phase::Replay { winner };
        self.phase_frame = 0;
    }

    /// After the replay (0x8003D124 / 0x8003EA5C): the win pose on the winner camera, the loser shot when a CPU
    /// won the match, or the draw's wait. Only the posing fighter is drawn.
    fn start_result(&mut self, winner: Option<usize>) {
        self.replay = None;
        self.game.display = None;
        let decided = self.decided();
        if !decided && self.skipped {
            // START skipped the replay with rounds to go: no win pose
            return self.end_result(winner);
        }
        let data = self.game.camera.data().clone();
        let motions = data.end.motions.clone();
        self.phase = Phase::Result { winner };
        self.phase_frame = 0;
        match winner {
            Some(w) if !decided || self.cpu[w].is_none() => {
                // the win motion: the frame counter's parity, or a held button (□ △ × ○ -> 0xD67..0xD6A)
                let b = self.last_pads[w].buttons;
                let pick = if b & 1 != 0 {
                    0
                } else if b & 2 != 0 {
                    1
                } else if b & 4 != 0 {
                    2
                } else if b & 8 != 0 {
                    3
                } else {
                    (self.frames & 1) as usize
                };
                let f = &mut self.game.fight.fighters[w];
                if !f.play_alias(motions.win.get(pick).copied().unwrap_or(WIN_POSES[0])) {
                    f.play_alias(motions.win[0]);
                }
                let b_variant = self.count(WINNER).is_multiple_of(2);
                let p = self.game.subject(w);
                let mut rnd = |n: i64| self.random(n);
                let cam = EndCam::winner(&data, &p, b_variant, &mut rnd);
                self.game.camera_mode = CameraMode::End { cam, who: w };
                self.game.visible = [w == 0, w == 1];
                self.result = ResultKind::Pose(w);
            }
            Some(w) => {
                // a CPU won the match: the loser shot (overhead when knocked out, else the orbit)
                let l = 1 - w;
                let knocked_out = self.game.fight.fighters[l].health <= 0;
                self.game.fight.fighters[l].play_alias(if knocked_out { motions.lose_ko } else { motions.lose_time_up });
                let p = self.game.subject(l);
                let mut rnd = |n: i64| self.random(n);
                let cam = EndCam::loser(&data, &p, knocked_out, &mut rnd);
                self.game.camera_mode = CameraMode::End { cam, who: l };
                self.game.visible = [l == 0, l == 1];
                self.result = ResultKind::Loser;
            }
            None => {
                // a draw: the view after a double K.O.'s replay stays; after a time-up draw the fight camera runs
                if matches!(self.game.camera_mode, CameraMode::Replay) {
                    self.game.camera_mode = CameraMode::Frozen;
                }
                self.result = ResultKind::Draw;
            }
        }
        self.game.step_camera_only();
    }

    /// The result is over: the next round, or the match result.
    fn end_result(&mut self, _winner: Option<usize>) {
        // the human (the only one) has a round and the CPU is short of the match: the cut-scene
        if self.interlude_check {
            if let Some(h) = (0..2).find(|&i| self.cpu[i].is_none() && self.cpu[1 - i].is_some()) {
                if self.wins[h] > 0 && self.wins[1 - h] < self.rounds_to_win {
                    self.interlude_check = false;
                    self.phase = Phase::Interlude;
                    self.phase_frame = 0;
                    return;
                }
            }
        }
        if self.decided() {
            let winner = if self.wins[0] == self.wins[1] { None } else { Some(if self.wins[0] > self.wins[1] { 0 } else { 1 }) };
            self.phase = Phase::MatchOver { winner };
            self.phase_frame = 0;
            self.events.push(winner.map_or(Announcement::Draw, Announcement::Wins));
        } else {
            self.next_round();
        }
    }

    /// Human pads, or what each CPU player decides (0x8002C4C4: in slot order, only while it has HP left).
    fn input(&mut self, pads: [Pad; 2]) -> [Pad; 2] {
        let mut out = pads;
        if self.cpu.iter().all(Option::is_none) && self.partner_cpu.is_none() && self.extra_cpu.is_none() && self.more_cpu.iter().all(Option::is_none) {
            return out;
        }
        // the opponent's chest height (+0x994): bone slot 2 of the pose shown
        let chest = [0, 1].map(|i| if self.cpu[1 - i].is_some() { -(self.game.subject(i).chest as f32) } else { 0.0 });
        let f = &self.game.fight;
        let ball = f.ball_sight();
        // (Ball 2 ON 2: each sees the ball between itself and the foe it faces)
        let doubles = f.ball_doubles();
        // Force's co-op: each soldier fights its own pick of the two players
        let coop = f.coop.is_some() && f.partner.is_some();
        let chest_of = |k: usize| if k < 2 { chest[k] } else { 1425.0 };
        let sight = |i: usize| {
            let o = f.opponent_index(i);
            let op = f.actor(o).unwrap_or(&f.fighters[1 - i.min(1)]);
            let ball = if doubles { f.ball_sight_for(i) } else { ball };
            Sight {
                gd: &f.data,
                me: f.actor(i).unwrap_or(&f.fighters[0]),
                op,
                facing_right: f.actor_facing_right(i),
                opp_chest: if coop { chest_of(o).max(1425.0) } else { chest[1 - i.min(1)] },
                ball,
                // (a Force player seat: the stage around it)
                force: (self.force && kumite_sim::Fight::is_player(i)).then_some(ForceSight { fight: f, me: i, pickups: &self.game.pickups }),
            }
        };
        // the round setup of both CPUs comes before either thinks
        for (i, ai) in self.cpu.iter_mut().enumerate() {
            if let Some(ai) = ai {
                ai.prepare(&self.ai_data, &mut self.ai_rng, &sight(i));
            }
        }
        for (i, (pad, cpu)) in out.iter_mut().zip(self.cpu.iter_mut()).enumerate() {
            if let Some(ai) = cpu {
                *pad = if f.fighters[i].health > 0 { ai.think(&self.ai_data, &mut self.ai_rng, &sight(i)) } else { Pad::default() };
            }
        }
        // Force's other soldier: against the player (or, in co-op, its pick)
        let mut extra_pad = Pad::default();
        if let (Some(ai), Some(e)) = (self.extra_cpu.as_mut(), f.extra.as_deref().filter(|e| !e.inactive && e.health > 0)) {
            let s = if coop {
                sight(2)
            } else {
                let facing_right = (e.root() - f.fighters[0].root()).dot(f.screen_right) < 0.0;
                Sight { gd: &f.data, me: e, op: &f.fighters[0], facing_right, opp_chest: chest[0].max(1425.0), ball: None, force: None }
            };
            ai.prepare(&self.ai_data, &mut self.ai_rng, &s);
            extra_pad = ai.think(&self.ai_data, &mut self.ai_rng, &s);
        }
        self.game.extra_pad = extra_pad;
        // Ball 2 ON 2: the players' partner
        if let (Some(ai), Some(_)) = (self.partner_cpu.as_mut(), f.partner.as_deref().filter(|p| p.health > 0)) {
            let s = sight(3);
            ai.prepare(&self.ai_data, &mut self.ai_rng, &s);
            self.game.partner_pad = ai.think(&self.ai_data, &mut self.ai_rng, &s);
        }
        // Force for more than two: the soldiers and bosses of the wider melee
        for (k, cpu) in self.more_cpu.iter_mut().enumerate() {
            let (Some(ai), Some(_)) = (cpu.as_mut(), f.actor(4 + k).filter(|a| !a.inactive && a.health > 0)) else { continue };
            let s = sight(4 + k);
            ai.prepare(&self.ai_data, &mut self.ai_rng, &s);
            if let Some(pad) = self.game.more_pad.get_mut(k) {
                *pad = ai.think(&self.ai_data, &mut self.ai_rng, &s);
            }
        }
        out
    }

    /// Force: the player's target changed (the game swapped the soldiers): their CPUs go with them.
    pub fn swap_cpus(&mut self) {
        if let (Some(a), Some(b)) = (self.cpu[1].as_mut(), self.extra_cpu.as_mut()) {
            std::mem::swap(a, b);
        }
    }
}
