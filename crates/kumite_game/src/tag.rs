//! Tag brawl (OpenKumite's own mode): all four fighters on the stage at once ([`kumite_sim::melee::Melee`]), in two
//! teams of two or each for themselves; each one a human or a CPU. Rounds as in versus (the round call with the
//! fly-in, the fight, the K.O. time, the winners' poses), best of `rounds_to_win`.

use kumite_sim::detmath::DetMath;
use std::sync::Arc;

use anyhow::Result;
use glam::{Mat3, Mat4, Vec3};
use kumite_assets::Assets;
use kumite_sim::ai::{Ai, AiData, AiRng, Sight};
use kumite_sim::camera::{CameraData, FightCamera, Sample, View};
use kumite_sim::input::Pad;
use kumite_sim::melee::{Melee, MeleeState};
use kumite_sim::{Animations, BodyVolumes, GameData};

use crate::bout::{Announcement, CALL_FIRST, CALL_OTHER};
use crate::{face, head, shown_pose, turn_head, volumes, Actor, Body, FighterAnims, Snapshot, UNITS};

/// The winners' poses last at most this long (frames).
const POSE_FRAMES: u32 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagPhase {
    /// "Round N" ... "Fight!" with the fly-in: everyone waits.
    Call,
    Fight,
    /// The K.O. time (the fight camera; nobody is controlled).
    RoundOver {
        winner: Option<usize>,
    },
    /// The winning team's poses.
    Result {
        winner: Option<usize>,
    },
    MatchOver {
        winner: Option<usize>,
    },
}

/// One slot's setup: model record, character id, CPU level (None = a human).
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub record: usize,
    pub cpu: Option<(usize, usize)>,
}

#[derive(Clone)]
pub struct TagGame {
    pub melee: Melee,
    pub bodies: Vec<Arc<Body>>,
    pub characters: Vec<Option<i64>>,
    pub camera: FightCamera,
    pub cpu: Vec<Option<Ai>>,
    ai_data: Arc<AiData>,
    ai_rng: AiRng,
    pub heads: Vec<head::HeadLook>,
    pub faces: Vec<face::Face>,
    pub face_data: Arc<face::FaceData>,
    face_rand: u32,
    /// Rounds won per team.
    pub wins: Vec<u32>,
    /// Each actor's team (two teams: 0, 0, 1, 1; each for themselves: 0, 1, 2, 3).
    pub teams: Vec<usize>,
    pub rounds_to_win: u32,
    pub round: u32,
    pub phase: TagPhase,
    pub phase_frame: u32,
    /// Announcements made during the last step.
    pub events: Vec<Announcement>,
    /// Which actors are drawn.
    pub visible: Vec<bool>,
    rng: u32,
    intro_counter: usize,
    /// The camera's framing axis of the last frame (it never flips by more than a right angle at once).
    axis: Vec3,
    /// The match's inputs recorded as it is played (a replay), when on: the four pads as a bout's step input's
    /// (pads, partner, extra).
    pub tape: Option<crate::bout::Tape>,
    /// A replay played: its inputs, and the next step's.
    playback: Option<(Arc<[crate::bout::StepInput]>, usize)>,
}

impl TagGame {
    pub fn load(assets: &Assets, slots: [Slot; 4], teams: Vec<usize>, ai_data: Arc<AiData>, rounds_to_win: u32) -> Result<Self> {
        let gd = assets.root.join("gamedata");
        let data = Arc::new(GameData::load(&gd)?);
        let actors: Vec<Actor> = slots.iter().map(|s| Actor::load(assets, s.record)).collect::<Result<_>>()?;
        let sets: [Arc<kumite_sim::data::Moveset>; 4] = std::array::from_fn(|i| actors[i].moveset.clone());
        let camera_data = Arc::new(CameraData::load(&gd.join("camera.json"))?);
        let mut g = Self {
            melee: Melee::with_teams(data, sets, teams.clone()),
            bodies: actors.iter().map(|a| a.body.clone()).collect(),
            characters: actors.iter().map(|a| a.character).collect(),
            camera: FightCamera::new(camera_data, false, 0, true),
            cpu: slots.iter().enumerate().map(|(i, s)| s.cpu.map(|(d, l)| Ai::new(d, l, usize::from(i >= 2)))).collect(),
            ai_data,
            ai_rng: AiRng::new(0x2545_F491),
            heads: vec![Default::default(); 4],
            faces: vec![Default::default(); 4],
            face_data: Arc::new(face::FaceData::load(&gd)),
            face_rand: 1,
            wins: vec![0; teams.iter().max().map_or(1, |t| t + 1)],
            teams,
            rounds_to_win: rounds_to_win.max(1),
            round: 0,
            phase: TagPhase::Call,
            phase_frame: 0,
            events: Vec::new(),
            visible: vec![true; 4],
            rng: 0x1234_5678,
            intro_counter: 0,
            axis: Vec3::X,
            tape: None,
            playback: None,
        };
        g.next_round();
        Ok(g)
    }

    fn random(&mut self, n: i64) -> i64 {
        self.rng = self.rng.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (((self.rng >> 16) as i64 & 0x7FFF) * n.max(1)) >> 15
    }

    pub fn is_final_round(&self) -> bool {
        // (the round that has to decide: every team one win short)
        self.round > self.wins.len() as u32 * (self.rounds_to_win - 1)
    }

    pub fn finished(&self) -> bool {
        matches!(self.phase, TagPhase::MatchOver { .. })
    }

    /// A team's members.
    pub fn members(&self, team: usize) -> impl Iterator<Item = usize> + '_ {
        (0..self.melee.actors.len()).filter(move |&i| self.melee.team[i] == team)
    }

    fn next_round(&mut self) {
        self.round += 1;
        for ai in self.cpu.iter_mut().flatten() {
            ai.new_round();
        }
        let data = self.melee.fight.data.clone();
        let sets: [Arc<kumite_sim::data::Moveset>; 4] = std::array::from_fn(|i| self.melee.actors[i].moveset.clone());
        let screen_right = self.melee.fight.screen_right;
        self.melee = Melee::with_teams(data, sets, self.teams.clone());
        self.melee.fight.screen_right = screen_right;
        for (i, f) in self.melee.actors.iter_mut().enumerate() {
            f.character = self.characters[i];
            f.guard_input = self.cpu[i].is_none();
        }
        self.intro_counter += 1;
        self.camera = FightCamera::new(self.camera.data().clone(), true, self.intro_counter % 10, self.round == 1);
        self.phase = TagPhase::Call;
        self.phase_frame = 0;
        self.visible = vec![true; 4];
        for h in &mut self.heads {
            h.reset();
            h.local = None;
        }
        self.step_camera();
    }

    /// The camera's framing axis: the direction everyone is spread along (the principal axis of the roots on the
    /// floor), turned to point from team 0 to team 1 and kept on last frame's side.
    fn framing_axis(&self) -> Vec3 {
        let a = &self.melee.actors;
        let roots: Vec<Vec3> = a.iter().map(|f| f.root()).collect();
        let mid = roots.iter().copied().sum::<Vec3>() / roots.len() as f32;
        let (mut xx, mut xz, mut zz) = (0.0f32, 0.0f32, 0.0f32);
        for r in &roots {
            let (x, z) = (r.x - mid.x, r.z - mid.z);
            (xx, xz, zz) = (xx + x * x, xz + x * z, zz + z * z);
        }
        let angle = 0.5 * (2.0 * xz).datan2(xx - zz);
        let mut axis = Vec3::new(angle.dcos(), 0.0, angle.dsin());
        let centre = |t: usize| {
            let m: Vec<Vec3> = self.members(t).map(|i| roots[i]).collect();
            m.iter().copied().sum::<Vec3>() / m.len().max(1) as f32
        };
        let teams = centre(1) - centre(0);
        // the teams' order decides the side while they stand apart (two teams); else last frame's
        let toward = if self.wins.len() == 2 && teams.dot(axis).abs() > 300.0 { teams } else { self.axis };
        if toward.dot(axis) < 0.0 {
            axis = -axis;
        }
        axis
    }

    /// The camera's two framing points: the extremes of everyone along the framing axis, team 0 to the left.
    fn samples(&self) -> [Sample; 2] {
        let a = &self.melee.actors;
        let mid = a.iter().map(|f| f.root()).sum::<Vec3>() / a.len() as f32;
        let axis = self.axis;
        let t: Vec<f32> = a.iter().map(|f| (f.root() - mid).dot(axis)).collect();
        let lo = t.iter().copied().fold(f32::MAX, f32::min).min(-1.0);
        let hi = t.iter().copied().fold(f32::MIN, f32::max).max(1.0);
        let chest: f32 = (0..a.len()).map(|i| self.chest(i)).sum::<f32>() / a.len() as f32;
        let at = |s: f32, side: i64| {
            let p = mid + axis * s;
            Sample { x: p.x.round() as i64, y: -(chest.round() as i64) - 290, z: -(p.z.round() as i64), side }
        };
        [at(lo, -100), at(hi, 100)]
    }

    /// What the camera frames (game units, on the floor): the middle of its two framing points, as a fight's is
    /// the fighters' midpoint. The stage's backdrop drum and floor follow it.
    pub fn framing_centre(&self) -> Vec3 {
        let [a, b] = self.framing_points();
        (a + b) / 2.0
    }

    /// The camera's two framing points on the floor (game units): the extremes of everyone along its axis, as a
    /// fight's two fighters.
    pub fn framing_points(&self) -> [Vec3; 2] {
        self.samples().map(|s| Vec3::new(s.x as f32, 0.0, -s.z as f32))
    }

    /// Actor `i`'s chest height (game units, up positive) in the pose shown.
    pub fn chest(&self, i: usize) -> f32 {
        let (pose, place) = self.pose(i);
        place.transform_point3(pose[self.bodies[i].chest].w_axis.truncate()).y * UNITS
    }

    fn step_camera(&mut self) -> View {
        let gd = self.melee.fight.data.clone();
        self.axis = self.framing_axis();
        let s = self.samples();
        // the move camera scripts of the pair the camera frames most: the first of each team
        let mc = [0, 2].map(|i| self.melee.actors[i].current_move(&gd).flags24.camera_script as i64);
        let v = self.camera.step(s, self.melee.fight.frame as i64, mc);
        let w = v.yaw as f32 * std::f32::consts::TAU / 4096.0;
        self.melee.fight.screen_right = Vec3::new(w.dcos(), 0.0, -w.dsin());
        v
    }

    pub fn snapshot(&self, i: usize) -> Snapshot {
        let f = &self.melee.actors[i];
        let partner = f.current.bank == kumite_sim::data::Bank::Partner;
        let (anim, frame) = f.animation(&self.melee.fight.data);
        let owner = if partner { &self.bodies[self.melee.facing(i)] } else { &self.bodies[i] };
        Snapshot { clip: owner.character.clip(anim).map(|c| (c, frame as usize)), partner, root: f.root(), yaw: f.yaw }
    }

    fn pose_animated(&self, i: usize) -> (Vec<Mat4>, Mat4) {
        shown_pose(&self.bodies[i], &self.bodies[self.melee.facing(i)], &self.snapshot(i))
    }

    /// Node world matrices (metres) and placement of actor `i`, the head turned when it follows.
    pub fn pose(&self, i: usize) -> (Vec<Mat4>, Mat4) {
        let (mut pose, place) = self.pose_animated(i);
        if let Some(local) = self.heads[i].local {
            turn_head(&self.bodies[i], &mut pose, local);
        }
        (pose, place)
    }

    pub fn spheres(&self) -> Vec<BodyVolumes> {
        (0..self.melee.actors.len())
            .map(|i| {
                let (pose, place) = self.pose(i);
                volumes(&self.bodies[i], &pose, place)
            })
            .collect()
    }

    /// The model id the face tables use.
    pub fn model_id(&self, i: usize) -> usize {
        self.bodies[i].model_record.saturating_sub(71) / 4
    }

    /// One 60 Hz step; `pads[i]` drives human actor `i`.
    /// A hash of the match's state (the actors' places, turns, health, moves and frames, the clock, the round,
    /// the wins): online play's check that every computer runs the same match.
    pub fn state_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |v: u64| {
            for b in v.to_le_bytes() {
                h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
            }
        };
        let m = &self.melee;
        eat(m.fight.frame);
        eat(m.clock as u64);
        eat(self.round as u64 | (self.phase_frame as u64) << 32);
        for w in &self.wins {
            eat(*w as u64);
        }
        for x in &m.actors {
            for v in [x.position.x, x.position.y, x.position.z, x.yaw, x.root_yaw, x.offset.x, x.offset.y, x.offset.z, x.anim_root_y] {
                eat(v.to_bits() as u64);
            }
            eat(x.health as u64);
            eat(x.current.index as u64 | (x.current.bank as u64) << 32);
            eat(x.frame as u64);
            eat(x.stun as u64 | (x.juggle as u64) << 32);
        }
        h
    }

    /// Play the match from these inputs (a replay): each step takes the next, whatever is passed to it.
    pub fn play(&mut self, inputs: Arc<[crate::bout::StepInput]>) {
        self.playback = Some((inputs, 0));
    }

    /// The steps played from a replay's inputs so far.
    pub fn played(&self) -> usize {
        self.playback.as_ref().map_or(0, |p| p.1)
    }

    /// This step's pads: the replay's, or `pads`, recorded on the tape.
    fn taped(&mut self, pads: [Pad; 4]) -> [Pad; 4] {
        if let Some((inputs, at)) = self.playback.as_mut() {
            let s = inputs.get(*at).copied().unwrap_or_default();
            *at += 1;
            return [s.pads[0], s.pads[1], s.partner, s.extra.unwrap_or_default()];
        }
        if let Some(tape) = self.tape.as_mut() {
            tape.push(crate::bout::StepInput { pads: [pads[0], pads[1]], partner: pads[2], extra: Some(pads[3]), more: [None; 2] });
        }
        pads
    }

    pub fn step(&mut self, pads: [Pad; 4]) {
        let pads = self.taped(pads);
        self.events.clear();
        self.phase_frame += 1;
        let idle = [Pad::default(); 4];
        match self.phase {
            TagPhase::Call => {
                if self.phase_frame == 1 {
                    let gd = self.melee.fight.data.clone();
                    for i in 0..4 {
                        let alias = if self.round > 1 {
                            1
                        } else if self.random(2) == 1 {
                            0
                        } else {
                            2
                        };
                        let f = &mut self.melee.actors[i];
                        if f.play_alias(alias) {
                            let len = f.current_move(&gd).frames;
                            f.frame = if len >= 151 { len - 149 } else { 1 };
                        }
                    }
                }
                let clock = self.melee.clock;
                self.sim(idle);
                self.melee.clock = clock;
                let [round, number, fight] = if self.round == 1 { CALL_FIRST } else { CALL_OTHER };
                let f = self.phase_frame;
                if f == round {
                    self.events.push(if self.is_final_round() { Announcement::FinalRound } else { Announcement::Round(self.round) });
                } else if f == number && !self.is_final_round() {
                    self.events.push(Announcement::RoundNumber(self.round));
                } else if f >= fight {
                    self.phase = TagPhase::Fight;
                    self.phase_frame = 0;
                    self.events.push(Announcement::Fight);
                }
            }
            TagPhase::Fight => {
                let pads = self.input(pads);
                self.sim(pads);
                let winner = match self.melee.state {
                    MeleeState::Fighting => return,
                    MeleeState::Ko { winner } => Some(winner),
                    MeleeState::DoubleKo => None,
                    MeleeState::TimeUp { winner } => winner,
                };
                let time_up = matches!(self.melee.state, MeleeState::TimeUp { .. });
                let full = |t: usize| self.members(t).all(|i| self.melee.actors[i].health >= self.melee.actors[i].max_health * kumite_sim::ONE);
                self.events.push(match winner {
                    _ if time_up => Announcement::TimeUp,
                    Some(w) if full(w) => Announcement::Perfect,
                    None => Announcement::DoubleKo,
                    Some(_) => Announcement::Ko,
                });
                match winner {
                    Some(w) => self.wins[w] += 1,
                    // (a draw: a round to both teams; each for themselves, to nobody)
                    None if self.wins.len() == 2 => self.wins.iter_mut().for_each(|w| *w += 1),
                    None => {}
                }
                self.phase = TagPhase::RoundOver { winner };
                self.phase_frame = 0;
            }
            TagPhase::RoundOver { winner } => {
                self.sim(idle);
                if self.phase_frame >= self.camera.data().end.ko_frames {
                    self.phase = TagPhase::Result { winner };
                    self.phase_frame = 0;
                    if let Some(w) = winner {
                        let poses = self.camera.data().end.motions.win.clone();
                        let pick = self.random(poses.len().max(1) as i64) as usize;
                        let members: Vec<usize> = self.members(w).filter(|&i| self.melee.actors[i].health > 0).collect();
                        for i in members {
                            let f = &mut self.melee.actors[i];
                            if !poses.get(pick).is_some_and(|&p| f.play_alias(p)) {
                                f.play_alias(crate::bout::WIN_POSES[0]);
                            }
                        }
                    }
                }
            }
            TagPhase::Result { winner } => {
                self.sim(idle);
                if self.phase_frame >= POSE_FRAMES {
                    if self.wins.iter().any(|&w| w >= self.rounds_to_win) {
                        let best = self.wins.iter().copied().max().unwrap_or(0);
                        let top: Vec<usize> = (0..self.wins.len()).filter(|&t| self.wins[t] == best).collect();
                        let w = (top.len() == 1).then(|| top[0]);
                        self.phase = TagPhase::MatchOver { winner: w };
                        self.phase_frame = 0;
                        self.events.push(w.map_or(Announcement::Draw, Announcement::Wins));
                    } else {
                        let _ = winner;
                        self.next_round();
                    }
                }
            }
            TagPhase::MatchOver { .. } => self.sim(idle),
        }
    }

    /// The simulation, faces, heads and camera for one frame.
    fn sim(&mut self, pads: [Pad; 4]) {
        let spheres = self.spheres();
        let facing: Vec<usize> = (0..4).map(|i| self.melee.facing(i)).collect();
        let bodies = &self.bodies;
        let anims: Vec<FighterAnims> = (0..4).map(|i| FighterAnims { own: &bodies[i], other: &bodies[facing[i]] }).collect();
        let anims: Vec<&dyn Animations> = anims.iter().map(|a| a as &dyn Animations).collect();
        self.melee.step(&pads, &spheres.iter().collect::<Vec<_>>(), &anims);
        self.step_faces();
        self.step_heads();
        self.step_camera();
    }

    /// Human pads, or each CPU's (only while it is standing), each against its own target.
    fn input(&mut self, pads: [Pad; 4]) -> [Pad; 4] {
        let mut out = pads;
        let chest: Vec<f32> = (0..4).map(|i| self.chest(i)).collect();
        let m = &self.melee;
        for (i, pad) in out.iter_mut().enumerate() {
            let Some(ai) = self.cpu[i].as_mut() else { continue };
            let o = m.facing(i);
            let s = Sight { gd: &m.fight.data, me: &m.actors[i], op: &m.actors[o], facing_right: m.facing_right(i), opp_chest: chest[o], ball: None, force: None };
            ai.prepare(&self.ai_data, &mut self.ai_rng, &s);
            *pad = if m.actors[i].health > 0 { ai.think(&self.ai_data, &mut self.ai_rng, &s) } else { Pad::default() };
        }
        out
    }

    /// Blinks and the moving pupils (as [`crate::Game`]'s), toward the opponent's head.
    fn step_faces(&mut self) {
        let c = Mat3::from_diagonal(Vec3::new(1.0, -1.0, -1.0));
        for i in 0..4 {
            let gd = &self.melee.fight.data;
            let m = self.melee.actors[i].current_move(gd);
            let flags = m.flags24.raw;
            let forced = flags & 0x4_0000 != 0 || (m.posture.raw & 4 != 0 && flags & 0x4000_0000 == 0);
            let seed = &mut self.face_rand;
            let mut rand = || {
                *seed = seed.wrapping_mul(0x41C6_4E6D).wrapping_add(12345);
                (*seed >> 16) & 0x7FFF
            };
            self.faces[i].blink(forced, &mut rand);
            if self.face_data.pupils_character.is_some_and(|c| self.characters[i] == Some(c)) {
                let head = |k: usize| {
                    let (pose, place) = self.pose_animated(k);
                    place * pose[self.bodies[k].chest]
                };
                let (own, opp) = (head(i), head(self.melee.facing(i)));
                let rot = c * Mat3::from_mat4(own) * c;
                let d = c * (opp.w_axis - own.w_axis).truncate() * UNITS;
                let l = rot.transpose() * d;
                let a = ((l.y.datan2(l.x) * 4096.0 / std::f32::consts::TAU).round() as i32).rem_euclid(4096);
                self.faces[i].pupils(a);
            }
        }
    }

    /// The heads turn to each actor's opponent while the round is on (as [`crate::Game`]'s).
    fn step_heads(&mut self) {
        let c = Mat3::from_diagonal(Vec3::new(1.0, -1.0, -1.0));
        let live = self.phase == TagPhase::Fight;
        for i in 0..4 {
            let gd = &self.melee.fight.data;
            let f = &self.melee.actors[i];
            let m = f.current_move(gd);
            let mut on = live && m.flags24.raw & 0x4000_0000 != 0;
            if on && m.active[0] != 0 {
                let from = (m.active[1] as i32).max(m.bytes_18.frame_count_ram as i32 - 0x11);
                on = f.frame as i32 >= from;
            }
            let no_yaw = self.characters[i].is_some_and(|c| gd.characters.head_without_yaw.contains(&c));
            let o = self.melee.facing(i);
            let world = |k: usize| {
                let (pose, place) = self.pose_animated(k);
                let b = &self.bodies[k];
                (place * pose[b.torso], place * pose[b.chest])
            };
            let frame = || {
                let (torso, head) = world(i);
                let (o_torso, o_head) = world(o);
                let rot = |m: Mat4| c * Mat3::from_mat4(m) * c;
                let pos = |m: Mat4| c * m.w_axis.truncate() * UNITS;
                let chest = rot(torso);
                head::Frame { chest, head: pos(head), head_local: chest.transpose() * rot(head), target: (pos(o_torso) + pos(o_head)) / 2.0, no_yaw }
            };
            let mut h = self.heads[i];
            h.step(on, frame);
            self.heads[i] = h;
        }
    }
}
