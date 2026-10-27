//! Game layer: ties the converted assets (glTF characters, JSON game data) to the fight simulation.
//! No GPU here, so fights can be run and tested headlessly.

use anyhow::{Context, Result};
use glam::{Mat3, Mat4, Quat, Vec3};
use kumite_assets::{Assets, Character};
use kumite_sim::data::{Moveset, MovesetIndex};
use kumite_sim::detmath::DetMath;
use kumite_sim::input::Pad;
use std::sync::Arc;
pub mod bout;
pub mod embu;
pub mod face;
pub mod force;
pub mod head;
pub mod rumble;
pub mod scene;
pub mod sound;
pub mod tag;
pub mod vfx;

use kumite_sim::camera::{CameraData, EndCam, FightCamera, Sample, Subject, View};
use kumite_sim::{Animations, BodyVolumes, Fight, GameData, Sphere};
use std::collections::VecDeque;

/// Converted glTF space is metres; the simulation works in game units (≈ mm).
pub const UNITS: f32 = 1000.0;
/// The replay records the last 300 frames.
pub const REPLAY_RING: usize = 300;

/// A fighter's converted character plus what the simulation needs from it.
pub struct Body {
    pub character: Character,
    pub model_record: usize,
    /// Root node (carries the root-motion translation track).
    pub root: usize,
    /// Hurt spheres: (node index, world-up offset in metres, radius in game units, zone).
    pub hurt: Vec<(usize, Vec3, f32, usize)>,
    /// Push spheres, same layout.
    pub push: Vec<(usize, Vec3, f32, usize)>,
    /// Bone slot 2 ("head" joint, the game's chest height +0x994): the camera samples its height.
    pub chest: usize,
    /// The "chest" joint (bone slot 1): the frame the head aims from.
    pub torso: usize,
    /// The "pelvis" joint (bone slot 11): its place on the screen is the fighter's side (+0x1278).
    pub pelvis: usize,
}

impl Body {
    /// Root translation of `anim` at `frame` (game units).
    fn root_of(&self, anim: &str, frame: u32) -> Option<Vec3> {
        let clip = &self.character.clips[self.character.clip(anim)?];
        let t = clip.translations[self.root].as_ref()?;
        Some(t[(frame as usize).min(t.len() - 1)] * UNITS)
    }
}

/// One fighter's animations: its own, and the opponent's for borrowed (throw-victim) moves.
struct FighterAnims<'a> {
    own: &'a Body,
    other: &'a Body,
}

impl Animations for FighterAnims<'_> {
    fn root(&self, anim: &str, frame: u32, partner: bool) -> Option<Vec3> {
        if partner {
            self.other.root_of(anim, frame)
        } else {
            self.own.root_of(anim, frame)
        }
    }
}

/// What one fighter showed on one frame: enough to pose and place it again (the replay's record).
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    /// Clip (in its owner's character) and frame.
    pub clip: Option<(usize, usize)>,
    /// The clip is the opponent's (a borrowed throw-victim move).
    pub partner: bool,
    pub root: Vec3,
    pub yaw: f32,
}

/// Frames a secondary joint takes to go over from the last clip's pose to the new one.
pub const SECONDARY_BLEND: u32 = 8;

/// The joints that hang or flap (the extra fighters' chains `chain*` and wing vertices `wing*`) carry their motion
/// baked into each clip, where the game keeps swinging them across moves. When the clip changes they go over from the
/// pose last shown to the new clip's over [`SECONDARY_BLEND`] frames (drawing only).
#[derive(Clone, Debug, Default)]
pub struct SecondaryBlend {
    clip: Option<(usize, bool)>,
    frame: u64,
    left: u32,
    from: Vec<(usize, Mat4)>,
    last: Vec<(usize, Mat4)>,
}

impl SecondaryBlend {
    /// `pose`: the character's node matrices for the clip shown at game frame `frame` (changed in place).
    pub fn apply(&mut self, character: &kumite_assets::Character, clip: Option<(usize, bool)>, frame: u64, pose: &mut [Mat4]) {
        let joints: Vec<usize> = character.nodes.iter().enumerate().filter(|(_, n)| n.name.starts_with("chain") || n.name.starts_with("wing")).map(|(i, _)| i).collect();
        if joints.is_empty() {
            return;
        }
        if frame != self.frame {
            self.frame = frame;
            if clip != self.clip {
                self.clip = clip;
                self.from = self.last.clone();
                self.left = SECONDARY_BLEND;
            } else {
                self.left = self.left.saturating_sub(1);
            }
        }
        if self.left > 0 {
            let t = 1.0 - self.left as f32 / (SECONDARY_BLEND + 1) as f32;
            for &(j, m) in &self.from {
                if let Some(p) = pose.get_mut(j) {
                    let (_, ra, ta) = m.to_scale_rotation_translation();
                    let (_, rb, tb) = p.to_scale_rotation_translation();
                    *p = Mat4::from_rotation_translation(ra.slerp(rb, t), ta.lerp(tb, t));
                }
            }
        }
        self.last = joints.iter().filter_map(|&j| pose.get(j).map(|m| (j, *m))).collect();
    }
}

/// Who drives the camera.
#[derive(Clone, Debug)]
pub enum CameraMode {
    /// The versus fight camera (fly-in, then the director).
    Fight,
    /// The replay camera (0x80069414), on the shown (replayed) poses.
    Replay,
    /// A round-end camera on fighter `who`.
    End { cam: EndCam, who: usize },
    /// The view stays where it is (after a double K.O.'s replay).
    Frozen,
}

/// A fight between two characters, loaded from the asset folder.
#[derive(Clone)]
pub struct Game {
    pub fight: Fight,
    /// The two characters' meshes, skeletons and volumes (shared: they never change during a fight).
    pub bodies: [Arc<Body>; 2],
    /// The versus fight camera, stepped with the fight (round-start fly-in first).
    pub camera: FightCamera,
    pub camera_mode: CameraMode,
    /// Fly-in settings for [`Game::reset_round`]: (variant, first round).
    intro: Option<(usize, bool)>,
    /// The last frames as shown (the replay ring, 300 frames), newest last.
    pub recorded: VecDeque<[Snapshot; 2]>,
    /// Frames recorded since the round started.
    pub recorded_total: u64,
    /// Shown instead of the live fighters (the replay).
    pub display: Option<[Snapshot; 2]>,
    /// Which fighters are drawn (the win pose shows only the winner, the loser shot only the loser).
    pub visible: [bool; 2],
    /// Full health per fighter in percent (the VS LIFE handicap, 70..140), applied at every round start.
    pub handicap: [i32; 2],
    /// Full health per fighter set by the battle type (0x800516CC: Team 170, Time Attack 130, Survival 140 ...);
    /// None: the default maximum with the handicap.
    pub max_health: [Option<i32>; 2],
    /// Health (16.16) a fighter starts the next round with when it carries over (Team, Survival: ctx+9); used
    /// once, when it is non-zero and at most the maximum, as 0x8002BE74 does.
    pub start_health: [Option<i32>; 2],
    /// The round clock in frames (the ROUND TIME option, (option + 2) * 600; Time Attack and Survival 40 s).
    pub round_frames: u32,
    /// Ball: the ball's data, its type, and the side that serves next (the loser of the last round).
    pub ball: Option<BallSetup>,
    /// Force's third actor's body (the fight's `extra` fighter) and its pad (from its CPU).
    pub extra_body: Option<Arc<Body>>,
    pub extra_pad: Pad,
    /// Force's co-op partner's body (the fight's `partner`, actor 3) and its pad (the second player's).
    pub partner_body: Option<Arc<Body>>,
    pub partner_pad: Pad,
    /// Force for more than two (OpenKumite's own): actors 4.. (3P, 4P, then more soldiers and bosses), their
    /// bodies (None: a place nobody fills) and pads.
    pub more_body: Vec<Option<Arc<Body>>>,
    pub more_pad: Vec<Pad>,
    /// Force: the roast chickens within reach (simulation positions; the stage's logic sets them), for a CPU player.
    pub pickups: Vec<Vec3>,
    /// Ball 2 ON 2: the partner of each side's fighter (the players' side's, then the other's), put on the
    /// court with them every round (actors 3 and 2).
    pub ball_team: Option<[Actor; 2]>,
    /// Which of the two partners a person plays (it guards by holding back).
    pub ball_team_human: [bool; 2],
    /// Ball's replay ring: the ball of the last 30 frames (EXE 0x800337A0), and the one shown in a replay.
    ball_ring: VecDeque<kumite_sim::ball::Ball>,
    pub display_ball: Option<kumite_sim::ball::Ball>,
    /// Each fighter's character id (roster.json selections; cancel `req` gates).
    characters: [Option<i64>; 2],
    /// The heads turning to the opponent (0x80039074): per fighter, while the round is on (`head_live`, the
    /// round state 2), not for Force's CPU soldiers (`head_off`, actor +0xC9).
    pub heads: [head::HeadLook; 2],
    pub head_live: bool,
    pub head_off: [bool; 2],
    /// Blinks and the moving pupils (faces.json), and the rand() stream the blinks draw from.
    pub faces: [face::Face; 2],
    /// The mode's K.O. style (Fight::ko_collapse), kept across rounds.
    pub ko_collapse: bool,
    pub face_data: Arc<face::FaceData>,
    face_rand: u32,
}

/// A character's body and move set by model record.
/// The files loading model `record` reads beyond the shared game data: its glTF and its moveset (what a browser
/// fetches ahead of a fight).
pub fn model_files(assets: &Assets, record: usize) -> Vec<std::path::PathBuf> {
    let gd = assets.root.join("gamedata");
    let mut files = vec![assets.root.join(format!("characters/model_{record:03}.glb"))];
    if let Ok(index) = kumite_sim::data::load::<MovesetIndex>(&gd.join("movesets/index.json")) {
        if let Some(e) = index.models.iter().find(|m| m.model_record == record) {
            files.push(gd.join("movesets").join(&e.moveset_file));
        }
    }
    files
}

/// `common`: the common set's move count.
fn load_body(assets: &Assets, record: usize, rules: &serde_json::Value, index: &MovesetIndex, common: usize) -> Result<(Body, Arc<Moveset>)> {
    let gd = assets.root.join("gamedata");
    let entry = index.models.iter().find(|m| m.model_record == record).with_context(|| format!("model {record} not in the moveset index"))?;
    let path = gd.join("movesets").join(&entry.moveset_file);
    let moveset = kumite_sim::data::load::<Moveset>(&path)?;
    moveset.check(common).with_context(|| format!("{}", path.display()))?;
    let moveset = Arc::new(moveset);
    let character = assets.character(record)?;
    let root = character.nodes.iter().position(|n| n.parent.is_none()).context("no root node")?;
    // the radii are per character id (0x8003F048: 0x80096FF0[+0x18]); the roster gives the model's character
    let roster: serde_json::Value = kumite_sim::data::load(&gd.join("roster.json")).unwrap_or_default();
    let id = roster["selections"]["entries"].as_array().into_iter().flatten().find(|e| e["model_record"].as_u64() == Some(record as u64)).and_then(|e| e["character"].as_u64()).unwrap_or(0);
    let hurt = sphere_layout(&rules["hurt_spheres"], &character, id)?;
    let push = sphere_layout(&rules["push_spheres"], &character, id)?;
    let chest = character.nodes.iter().position(|n| n.name == "head").context("no head joint")?;
    let torso = character.nodes.iter().position(|n| n.name == "chest").context("no chest joint")?;
    let pelvis = character.nodes.iter().position(|n| n.name == "pelvis").unwrap_or(root);
    Ok((Body { character, model_record: record, root, hurt, push, chest, torso, pelvis }, moveset))
}

/// A body ready to be swapped into a fight (Force's soldiers and bosses).
#[derive(Clone)]
pub struct Actor {
    pub body: Arc<Body>,
    pub moveset: Arc<Moveset>,
    pub character: Option<i64>,
}

impl Actor {
    pub fn load(assets: &Assets, record: usize) -> Result<Self> {
        let gd = assets.root.join("gamedata");
        let rules: serde_json::Value = kumite_sim::data::load(&gd.join("rules.json"))?;
        let index: MovesetIndex = kumite_sim::data::load(&gd.join("movesets/index.json"))?;
        let common = kumite_sim::data::move_count(&gd.join("movesets/common.json"))?;
        let (body, moveset) = load_body(assets, record, &rules, &index, common)?;
        let roster: serde_json::Value = kumite_sim::data::load(&gd.join("roster.json")).unwrap_or_default();
        let character = roster["selections"]["entries"].as_array().into_iter().flatten().find(|e| e["model_record"].as_u64() == Some(record as u64)).and_then(|e| e["character"].as_i64());
        Ok(Self { body: Arc::new(body), moveset, character })
    }
}

/// Ball's setup kept between rounds.
#[derive(Clone)]
pub struct BallSetup {
    pub data: Arc<kumite_sim::ball::BallData>,
    pub kind: usize,
    pub server: usize,
    /// Rounds started (the first serve waits longer).
    pub rounds: u32,
}

impl Game {
    pub fn load(assets: &Assets, models: [usize; 2]) -> Result<Self> {
        let gd = assets.root.join("gamedata");
        let rules: serde_json::Value = kumite_sim::data::load(&gd.join("rules.json"))?;
        let index: MovesetIndex = kumite_sim::data::load(&gd.join("movesets/index.json"))?;
        let data = Arc::new(GameData::load(&gd)?);
        let mut movesets = Vec::new();
        let mut bodies = Vec::new();
        for record in models {
            let (body, moveset) = load_body(assets, record, &rules, &index, data.common.moves.len())?;
            movesets.push(moveset);
            bodies.push(body);
        }
        let [b0, b1]: [Body; 2] = bodies.try_into().map_err(|_| anyhow::anyhow!("two fighters"))?;
        let camera_data = Arc::new(CameraData::load(&gd.join("camera.json"))?);
        let roster: serde_json::Value = kumite_sim::data::load(&gd.join("roster.json")).unwrap_or_default();
        let character = |m: usize| roster["selections"]["entries"].as_array().into_iter().flatten().find(|e| e["model_record"].as_u64() == Some(m as u64)).and_then(|e| e["character"].as_i64());
        let characters = [character(models[0]), character(models[1])];
        let mut game = Self {
            fight: Fight::new(data, movesets[0].clone(), movesets[1].clone()),
            bodies: [Arc::new(b0), Arc::new(b1)],
            camera: FightCamera::new(camera_data, false, 0, true),
            camera_mode: CameraMode::Fight,
            intro: None,
            recorded: VecDeque::new(),
            recorded_total: 0,
            display: None,
            visible: [true; 2],
            handicap: [100; 2],
            max_health: [None; 2],
            start_health: [None; 2],
            round_frames: kumite_sim::ROUND_FRAMES,
            ball: None,
            extra_body: None,
            extra_pad: Pad::default(),
            partner_body: None,
            partner_pad: Pad::default(),
            more_body: Vec::new(),
            more_pad: Vec::new(),
            pickups: Vec::new(),
            ball_team: None,
            ball_team_human: [false; 2],
            ball_ring: VecDeque::new(),
            display_ball: None,
            characters,
            heads: Default::default(),
            head_live: false,
            head_off: [false; 2],
            faces: Default::default(),
            ko_collapse: true,
            face_data: Arc::new(face::FaceData::load(&assets.root.join("gamedata"))),
            face_rand: 1,
        };
        for (f, c) in game.fight.fighters.iter_mut().zip(characters) {
            f.character = c;
        }
        game.step_camera();
        Ok(game)
    }

    /// Start with the round-start camera fly-in (variant = round counter % 10). While it plays the
    /// fighters stand in their stance, pads are ignored and the round clock does not run.
    pub fn with_intro(mut self, variant: usize, first_round: bool) -> Self {
        self.camera = FightCamera::new(self.camera.data().clone(), true, variant, first_round);
        self.intro = Some((variant, first_round));
        self.step_camera();
        self
    }

    /// Start the round again: both fighters back at their start positions with full health, fresh clock and
    /// camera (and the fly-in again if the game has one). Characters and stage stay loaded.
    pub fn reset_round(&mut self) {
        self.restart(self.intro);
    }

    /// Put each fighter into its round-start move (alias; 0x8002D31C): from frame 1, or with exactly 150
    /// frames left for longer clips. An empty alias leaves the fighter in its stance.
    pub fn set_start_moves(&mut self, aliases: [usize; 2]) {
        let gd = self.fight.data.clone();
        for (f, alias) in self.fight.fighters.iter_mut().zip(aliases) {
            if f.play_alias(alias) {
                let len = f.current_move(&gd).frames;
                f.frame = if len >= 151 { len - 149 } else { 1 };
            }
        }
    }

    /// Start a new round with the given fly-in (variant, first round), or none.
    pub fn start_round(&mut self, intro: Option<(usize, bool)>) {
        self.intro = intro;
        self.restart(intro);
    }

    fn restart(&mut self, intro_setting: Option<(usize, bool)>) {
        // Ball: the loser of the round serves the next (0x8003CE14)
        if let (Some(ball), kumite_sim::RoundState::Ko { winner }) = (self.ball.as_mut(), self.fight.state) {
            ball.server = 1 - winner;
        }
        let [a, b] = &self.fight.fighters;
        let (ma, mb) = (a.moveset.clone(), b.moveset.clone());
        let guard_input = [a.guard_input, b.guard_input];
        let walls = self.fight.walls.take();
        self.fight = Fight::new(self.fight.data.clone(), ma, mb);
        self.fight.ko_collapse = self.ko_collapse;
        self.fight.walls = walls;
        for (i, (f, g)) in self.fight.fighters.iter_mut().zip(guard_input).enumerate() {
            f.guard_input = g; // who is CPU-controlled stays the same
            f.max_health = self.max_health[i].unwrap_or(self.fight.data.max_health * self.handicap[i] / 100);
            f.character = self.characters[i];
            f.health = match self.start_health[i].take() {
                Some(h) if h > 0 && h <= f.max_health * kumite_sim::ONE => h,
                _ => f.max_health * kumite_sim::ONE,
            };
        }
        self.fight.clock = self.round_frames;
        if let Some(setup) = self.ball.as_mut() {
            // the fighters start on the back lines (0x800B07C8), the ball waits over the server's side
            for (i, f) in self.fight.fighters.iter_mut().enumerate() {
                f.position.x = if i == 0 { -kumite_sim::ball::FIGHTER_X } else { kumite_sim::ball::FIGHTER_X };
            }
            self.fight.ball = Some(kumite_sim::ball::Ball::new(&setup.data, setup.kind, setup.server, setup.rounds == 0));
            setup.rounds += 1;
        }
        if let (Some([partner, extra]), true) = (self.ball_team.clone(), self.ball.is_some()) {
            self.ball_doubles(&partner, &extra);
        }
        let (intro, (variant, first)) = (intro_setting.is_some(), intro_setting.unwrap_or((0, true)));
        self.camera = FightCamera::new(self.camera.data().clone(), intro, variant, first);
        self.camera_mode = CameraMode::Fight;
        self.recorded.clear();
        self.recorded_total = 0;
        self.ball_ring.clear();
        self.display_ball = None;
        self.display = None;
        self.visible = [true; 2];
        // the head state's round reset (0x80039034) writes player 1's entry twice: player 2's carries on
        self.heads[0].reset();
        for h in &mut self.heads {
            h.local = None;
        }
        self.step_camera();
    }

    /// What the camera samples from both fighters (0x8006465C), in game space (Y down, Z = −glTF Z):
    /// the root on the floor, chest height − 290, and the side each is on (its pelvis on the screen, +0x1278).
    pub fn camera_samples(&self) -> [Sample; 2] {
        std::array::from_fn(|i| {
            let (pose, place) = self.pose(i);
            let chest = place.transform_point3(pose[self.bodies[i].chest].w_axis.truncate()) * UNITS;
            let root = self.fight.fighters[i].root();
            Sample { x: root.x.round() as i64, y: -(chest.y.round() as i64) - 290, z: -(root.z.round() as i64), side: self.fight.fighters[i].side as i64 }
        })
    }

    /// The replay camera's samples (0x80069304): root x/z, and y = the higher of the chest and the root bone.
    pub fn replay_samples(&self) -> [Sample; 2] {
        std::array::from_fn(|i| {
            let s = self.subject(i);
            Sample { x: s.root[0], y: s.chest.min(s.root[1]), z: s.root[2], side: [-100, 100][i] }
        })
    }

    /// What an end camera needs from fighter `i` (game space, the shown pose): root bone, chest height,
    /// facing angle, large character (the data's large move set).
    pub fn subject(&self, i: usize) -> Subject {
        let (pose, place) = self.pose(i);
        let world = |node: usize| place.transform_point3(pose[node].w_axis.truncate()) * UNITS;
        let root = world(self.bodies[i].root);
        let chest = world(self.bodies[i].chest);
        let yaw = self.display.map_or(self.fight.fighters[i].yaw, |d| d[i].yaw);
        // the front in game XZ (Z flipped): (sin yaw, -cos yaw)
        let front = self.camera.data().ratan2((yaw.dsin() * 4096.0) as i64, (-yaw.dcos() * 4096.0) as i64) & 0xFFF;
        Subject {
            root: [root.x.round() as i64, -(root.y.round() as i64), -(root.z.round() as i64)],
            chest: -(chest.y.round() as i64),
            front,
            large: self.fight.fighters[i].moveset.moveset_id == Some(self.camera.data().large_moveset()),
        }
    }

    /// The root translation (game units) of fighter `i`'s animation frame, in the clip of its owner.
    pub fn anim_root(&self, i: usize) -> Option<Vec3> {
        let (anim, frame) = self.fight.fighters[i].animation(&self.fight.data);
        self.clip_owner(i).root_of(anim, frame)
    }

    /// What fighter `i` shows now (for the replay record).
    pub fn snapshot(&self, i: usize) -> Snapshot {
        let f = &self.fight.fighters[i];
        Snapshot { clip: self.clip(i), partner: f.current.bank == kumite_sim::data::Bank::Partner, root: f.root(), yaw: f.yaw }
    }

    /// The camera's frame, then each fighter's side for the next one: where its pelvis falls on the screen
    /// (0x80037924 projects bone 11 to +0x1278 as it draws; 0x80046234 compares the two for the pad's forward and
    /// the camera's framing).
    fn step_camera(&mut self) -> View {
        let v = self.step_view();
        self.project_sides(&v);
        v
    }

    /// The fighters' sides under view `v`: their pelvis's horizontal place on the screen (scaled to whole units).
    fn project_sides(&mut self, v: &View) {
        let (eye, forward, _) = view_camera(v);
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        for i in 0..2 {
            let (pose, place) = self.pose(i);
            let Some(m) = pose.get(self.bodies[i].pelvis) else { continue };
            let d = place.transform_point3(m.w_axis.truncate()) - eye;
            let depth = d.dot(forward);
            if depth > 1e-3 {
                self.fight.fighters[i].side = (d.dot(right) / depth * 10000.0).round() as i32;
            }
        }
    }

    fn step_view(&mut self) -> View {
        match &self.camera_mode {
            CameraMode::Fight => {}
            CameraMode::Replay => {
                let s = self.replay_samples();
                return self.camera.replay_step(s);
            }
            CameraMode::End { who, .. } => {
                let p = self.subject(*who);
                let data = self.camera.data().clone();
                if let CameraMode::End { cam, .. } = &mut self.camera_mode {
                    self.camera.view = cam.step(&data, &p);
                }
                return self.camera.view;
            }
            CameraMode::Frozen => return self.camera.view,
        }
        let gd = self.fight.data.clone();
        self.camera.ball = self.fight.ball.as_ref().map(|b| b.pos.map(|v| v as i64));
        let mc = std::array::from_fn(|i| self.fight.fighters[i].current_move(&gd).flags24.camera_script as i64);
        let v = self.camera.step(self.camera_samples(), self.fight.frame as i64, mc);
        // screen right in game space is (cos yaw, 0, sin yaw); Z flips into the simulation's axes
        let w = v.yaw as f32 * std::f32::consts::TAU / 4096.0;
        self.fight.screen_right = Vec3::new(w.dcos(), 0.0, -w.dsin());
        v
    }

    /// One 60 Hz step with both players' pads.
    pub fn step(&mut self, pads: [Pad; 2]) {
        let intro = self.camera.in_intro();
        let (pads, clock) = if intro { ([Pad::default(); 2], self.fight.clock) } else { (pads, 0) };
        if self.fight.coop.is_some() && self.fight.partner.is_some() {
            self.step_coop(pads);
        } else {
            let spheres = self.spheres();
            let [b0, b1] = &self.bodies;
            let (a0, a1) = (FighterAnims { own: b0, other: b1 }, FighterAnims { own: b1, other: b0 });
            // Force's other soldier
            let extra_spheres = self.actor_spheres(2);
            let extra_anims = self.extra_body.as_deref().map(|b| FighterAnims { own: b, other: b0 });
            let extra = match (&extra_spheres, &extra_anims) {
                (Some(sp), Some(an)) => Some((self.extra_pad, sp, an as &dyn Animations)),
                _ => None,
            };
            self.fight.step_with(pads, &spheres, [&a0 as &dyn Animations, &a1 as &dyn Animations], extra);
        }
        if intro {
            self.fight.clock = clock;
        }
        self.step_faces();
        self.step_heads();
        let shot = [self.snapshot(0), self.snapshot(1)];
        if self.recorded.len() >= REPLAY_RING {
            self.recorded.pop_front();
        }
        self.recorded.push_back(shot);
        if let Some(b) = &self.fight.ball {
            if self.ball_ring.len() >= 30 {
                self.ball_ring.pop_front();
            }
            self.ball_ring.push_back(b.clone());
        }
        self.recorded_total += 1;
        self.step_camera();
    }

    /// Force's co-op step: the four actors, each borrowing from the one it fights.
    fn step_coop(&mut self, pads: [Pad; 2]) {
        let n = self.fight.actor_count();
        let spheres: Vec<BodyVolumes> = (0..n).map(|k| self.actor_spheres(k).unwrap_or(BodyVolumes { hurt: vec![], push: vec![] })).collect();
        let bodies: Vec<Arc<Body>> = (0..n).map(|k| self.body_of(k).cloned().unwrap_or_else(|| self.bodies[0].clone())).collect();
        let anims: Vec<FighterAnims> = (0..n).map(|k| FighterAnims { own: &bodies[k], other: &bodies[self.fight.opponent_index(k).min(n - 1)] }).collect();
        let a: Vec<&dyn Animations> = anims.iter().map(|a| a as &dyn Animations).collect();
        let mut pads = vec![pads[0], pads[1], self.extra_pad, self.partner_pad];
        pads.extend((4..n).map(|k| self.more_pad.get(k - 4).copied().unwrap_or_default()));
        let refs: Vec<&BodyVolumes> = spheres.iter().collect();
        self.fight.step_coop(&pads, &refs, &a);
    }

    /// The players in the fight (Force): actor 0, the co-op partner (3), 3P and 4P (4, 5), those who joined.
    pub fn players(&self) -> Vec<usize> {
        let mut v = vec![0];
        if self.partner_body.is_some() && self.fight.partner.is_some() {
            v.push(3);
        }
        v.extend((4..6).filter(|&k| self.more_body.get(k - 4).is_some_and(Option::is_some)));
        v
    }

    /// Force for more than two: the melee and the bodies' places widened to `n` actors.
    pub fn widen_coop(&mut self, n: usize) {
        self.fight.widen_coop(n);
        let m = self.fight.more.len();
        self.more_body.resize(m, None);
        self.more_pad.resize(m, Pad::default());
    }

    /// Force: player `p` (2 = 3P, 3 = 4P) joins as `actor` beside the player (or, down, gets up there again
    /// at full health): actor 4 or 5 of the widened melee.
    pub fn join_player(&mut self, p: usize, actor: &Actor) {
        let k = 2 + p.clamp(2, 3);
        self.widen_coop(12);
        let max = self.fight.data.max_health;
        let lead = &self.fight.fighters[0];
        let pos = lead.position + Vec3::new(-400.0 * (p as f32 - 1.0), 0.0, if p == 2 { -900.0 } else { 1800.0 });
        let mut f = kumite_sim::Fighter::new(actor.moveset.clone(), max, pos, lead.yaw, lead.side);
        f.character = actor.character;
        f.guard_input = true;
        if let Some(m) = self.fight.coop.as_mut() {
            m.links.retain(|l| l.0 != k && l.1 != k);
        }
        self.fight.more[k - 4] = f;
        self.more_body[k - 4] = Some(actor.body.clone());
    }

    /// Ball 2 ON 2: each side's two on its back line, apart in depth; the partners (CPU) join the fight as
    /// actors 3 (the players' side) and 2, and from then the four play as the co-op melee.
    fn ball_doubles(&mut self, partner: &Actor, extra: &Actor) {
        let max = self.fight.data.max_health;
        let depth = kumite_sim::ball::COURT_Z_DOUBLES / 2.0;
        for f in &mut self.fight.fighters {
            f.position.z = -depth;
        }
        let beside = |leader: &kumite_sim::Fighter, a: &Actor, human: bool| {
            let mut f = kumite_sim::Fighter::new(a.moveset.clone(), max, Vec3::new(leader.position.x, leader.position.y, depth), leader.yaw, leader.side);
            f.character = a.character;
            f.guard_input = human;
            f
        };
        let human = self.ball_team_human;
        let (p, e) = (beside(&self.fight.fighters[0], partner, human[0]), beside(&self.fight.fighters[1], extra, human[1]));
        self.extra_body = Some(extra.body.clone());
        self.partner_body = Some(partner.body.clone());
        self.fight.extra = Some(Box::new(e));
        self.fight.join_partner(p);
    }

    /// Actor `k`'s body ([`Fight::actor`] indices).
    pub fn body_of(&self, k: usize) -> Option<&Arc<Body>> {
        match k {
            0 | 1 => Some(&self.bodies[k]),
            2 => self.extra_body.as_ref(),
            3 => self.partner_body.as_ref(),
            _ => self.more_body.get(k - 4).and_then(Option::as_ref),
        }
    }

    /// Force's co-op: the second player joins as `actor` beside the player (or, down, gets up there
    /// again at full health).
    pub fn join_partner(&mut self, actor: &Actor) {
        // (1P's full health, Force co-op's balance: 2P's the same)
        let max = self.fight.data.max_health * self.handicap[0] / 100;
        let p = &self.fight.fighters[0];
        let pos = p.position + Vec3::new(-400.0, 0.0, 900.0);
        let mut f = kumite_sim::Fighter::new(actor.moveset.clone(), max, pos, p.yaw, p.side);
        f.character = actor.character;
        f.guard_input = true;
        self.partner_body = Some(actor.body.clone());
        if self.fight.coop.is_some() && self.fight.partner.is_some() {
            if let Some(m) = self.fight.coop.as_mut() {
                m.links.retain(|l| l.0 != 3 && l.1 != 3);
            }
            self.fight.partner = Some(Box::new(f));
        } else {
            self.fight.join_partner(f);
        }
    }

    /// Show a recorded frame (or, between two, the roots averaged as the replay does on odd half-frames) and
    /// step the camera on it. None = live again.
    pub fn show(&mut self, frame: Option<(usize, bool)>) {
        // the ball's ring holds the record's last 30 frames
        self.display_ball = frame.and_then(|(k, _)| (k + self.ball_ring.len()).checked_sub(self.recorded.len()).and_then(|i| self.ball_ring.get(i).cloned()));
        self.display = frame.and_then(|(k, half)| {
            let a = *self.recorded.get(k)?;
            if !half {
                return Some(a);
            }
            let b = self.recorded.get(k + 1).copied().unwrap_or(a);
            Some(std::array::from_fn(|i| Snapshot { root: (a[i].root + b[i].root) / 2.0, ..a[i] }))
        });
        if self.display.is_some() {
            self.step_camera();
        }
    }

    /// Advance only the camera (the sim is paused: the replay, a frozen end view).
    pub fn step_camera_only(&mut self) {
        self.step_camera();
    }

    /// The character whose clips fighter `i` is playing: its own, or the opponent's for a borrowed move.
    fn clip_owner(&self, i: usize) -> &Body {
        if self.fight.fighters[i].current.bank == kumite_sim::data::Bank::Partner {
            self.body_of(self.fight.opponent_index(i)).unwrap_or(&self.bodies[1 - i])
        } else {
            &self.bodies[i]
        }
    }

    /// Animation clip index (in [`Game::clip_owner`]'s character) and frame currently shown for fighter `i`.
    pub fn clip(&self, i: usize) -> Option<(usize, usize)> {
        let (anim, frame) = self.fight.fighters[i].animation(&self.fight.data);
        Some((self.clip_owner(i).character.clip(anim)?, frame as usize))
    }

    /// Force: the third actor's pose and placement (its live state; no replays in Force).
    pub fn pose_extra(&self) -> Option<(Vec<Mat4>, Mat4)> {
        self.pose_actor(2)
    }

    /// Actor `k`'s pose and placement (2, 3: Force's extra and partner, live; 0, 1 as shown).
    pub fn pose_actor(&self, k: usize) -> Option<(Vec<Mat4>, Mat4)> {
        if k < 2 {
            return Some(self.pose(k));
        }
        let (f, body) = (self.fight.actor(k)?, self.body_of(k)?);
        let partner = f.current.bank == kumite_sim::data::Bank::Partner;
        let other = if partner { self.body_of(self.fight.opponent_index(k)).unwrap_or(body) } else { body };
        let (anim, frame) = f.animation(&self.fight.data);
        let shown = Snapshot { clip: other.character.clip(anim).map(|c| (c, frame as usize)), partner, root: f.root(), yaw: f.yaw };
        Some(shown_pose(body, other, &shown))
    }

    /// Actor `k`'s collision volumes.
    fn actor_spheres(&self, k: usize) -> Option<BodyVolumes> {
        let (pose, place) = self.pose_actor(k)?;
        Some(volumes(self.body_of(k)?, &pose, place))
    }

    /// Put `actor` in slot 1 (the player's target) or 2 (the extra), a fresh fighter where the old one stood.
    pub fn set_actor(&mut self, slot: usize, actor: &Actor) {
        let max = self.fight.data.max_health;
        let (pos, yaw, side) = match slot {
            1 => (self.fight.fighters[1].position, self.fight.fighters[1].yaw, self.fight.fighters[1].side),
            2 => self.fight.extra.as_ref().map_or((Vec3::ZERO, std::f32::consts::FRAC_PI_2, 100), |f| (f.position, f.yaw, f.side)),
            _ => self.fight.actor(slot).map_or((Vec3::ZERO, std::f32::consts::FRAC_PI_2, 100), |f| (f.position, f.yaw, f.side)),
        };
        let mut f = kumite_sim::Fighter::new(actor.moveset.clone(), max, pos, yaw, side);
        f.character = actor.character;
        f.guard_input = false;
        match slot {
            1 => {
                self.fight.fighters[1] = f;
                self.bodies[1] = actor.body.clone();
                self.characters[1] = actor.character;
            }
            2 => {
                self.fight.extra = Some(Box::new(f));
                self.extra_body = Some(actor.body.clone());
            }
            // (the widened melee's soldiers and bosses: actors 6..)
            k => {
                if let (Some(place), Some(body)) = (self.fight.more.get_mut(k - 4), self.more_body.get_mut(k - 4)) {
                    if let Some(m) = self.fight.coop.as_mut() {
                        m.links.retain(|l| l.0 != k && l.1 != k);
                    }
                    *place = f;
                    *body = Some(actor.body.clone());
                }
            }
        }
    }

    /// Swap the player's target with the other soldier (fighter 1 and the extra, with their bodies).
    pub fn swap_target(&mut self) {
        if let (Some(f), Some(b)) = (self.fight.extra.as_mut(), self.extra_body.as_mut()) {
            std::mem::swap(&mut self.fight.fighters[1], f);
            std::mem::swap(&mut self.bodies[1], b);
            self.characters[1] = self.fight.fighters[1].character;
            self.fight.swapped_target();
        }
    }

    /// A hash of the fight's state (the fighters' places, turns, health, moves and frames, the clock, the round, the
    /// ball): two runs of the same inputs that agree frame by frame have the same hashes (online play's determinism
    /// check).
    pub fn state_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |v: u64| {
            for b in v.to_le_bytes() {
                h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
            }
        };
        let f = &self.fight;
        eat(f.frame);
        eat(f.clock as u64);
        eat(match f.state {
            kumite_sim::RoundState::Fighting => 0,
            kumite_sim::RoundState::Ko { winner } => 1 + winner as u64,
            kumite_sim::RoundState::TimeUp { winner } => 4 + winner.map_or(0, |w| 1 + w as u64),
        });
        let fighters = f.fighters.iter().chain(f.extra.as_deref()).chain(f.partner.as_deref()).chain(&f.more);
        for x in fighters {
            for v in [x.position.x, x.position.y, x.position.z, x.yaw, x.root_yaw, x.offset.x, x.offset.y, x.offset.z, x.anim_root_y] {
                eat(v.to_bits() as u64);
            }
            eat(x.health as u64);
            eat(x.current.index as u64 | (x.current.bank as u64) << 32);
            eat(x.frame as u64);
            eat(x.stun as u64 | (x.juggle as u64) << 32);
        }
        if let Some(b) = &f.ball {
            for v in b.pos.iter().chain(&b.vel) {
                eat(*v as u64);
            }
            eat(b.state as u64);
        }
        h
    }

    /// The model id the face tables use ((model record − 71) / 4).
    pub fn model_id(&self, i: usize) -> usize {
        self.bodies[i].model_record.saturating_sub(71) / 4
    }

    /// Blinks (0x80034100, every frame; models without a blink rectangle show nothing) and the moving pupils
    /// (0x800397FC, from the skeleton before the head turns; not for Force's CPU fighters).
    fn step_faces(&mut self) {
        let c = Mat3::from_diagonal(Vec3::new(1.0, -1.0, -1.0));
        for i in 0..2 {
            let gd = &self.fight.data;
            let m = self.fight.fighters[i].current_move(gd);
            let flags = m.flags24.raw;
            let forced = flags & 0x4_0000 != 0 || (m.posture.raw & 4 != 0 && flags & 0x4000_0000 == 0);
            let seed = &mut self.face_rand;
            let mut rand = || {
                *seed = seed.wrapping_mul(0x41C6_4E6D).wrapping_add(12345);
                (*seed >> 16) & 0x7FFF
            };
            self.faces[i].blink(forced, &mut rand);
            if self.face_data.pupils_character.is_some_and(|c| self.characters[i] == Some(c)) && !self.head_off[i] {
                let head = |k: usize| {
                    let (pose, place) = self.pose_animated(k);
                    place * pose[self.bodies[k].chest]
                };
                let (own, opp) = (head(i), head(1 - i));
                let rot = c * Mat3::from_mat4(own) * c;
                let d = c * (opp.w_axis - own.w_axis).truncate() * UNITS;
                let l = rot.transpose() * d;
                let a = ((l.y.datan2(l.x) * 4096.0 / std::f32::consts::TAU).round() as i32).rem_euclid(4096);
                self.faces[i].pupils(a);
            }
        }
    }

    /// Each head's frame (0x8003B17C → 0x80039074): on while the round is live and the move lets the head
    /// follow (record +0x24 bit 30; an attack only from frame max(+0x2E, +0x18 − 17)), aimed at the
    /// opponent's chest and head.
    fn step_heads(&mut self) {
        // glTF (y up, metres) to game space (y down, z flipped, units)
        let c = Mat3::from_diagonal(Vec3::new(1.0, -1.0, -1.0));
        let world = |g: &Game, i: usize| {
            let (pose, place) = g.pose_animated(i);
            let b = &g.bodies[i];
            let at = |n: usize| place * pose[n];
            (at(b.torso), at(b.chest))
        };
        for i in 0..2 {
            let gd = &self.fight.data;
            let f = &self.fight.fighters[i];
            let m = f.current_move(gd);
            let mut on = self.head_live && !self.head_off[i] && m.flags24.raw & 0x4000_0000 != 0;
            if on && m.active[0] != 0 {
                let from = (m.active[1] as i32).max(m.bytes_18.frame_count_ram as i32 - 0x11);
                on = f.frame as i32 >= from;
            }
            let no_yaw = self.characters[i].is_some_and(|c| gd.characters.head_without_yaw.contains(&c));
            let frame = || {
                let (torso, head) = world(self, i);
                let (o_torso, o_head) = world(self, 1 - i);
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

    /// Node world matrices (metres) and the placement transform for fighter `i` (as shown: the replay's
    /// frame while one plays), the head turned to the opponent when it follows.
    pub fn pose(&self, i: usize) -> (Vec<Mat4>, Mat4) {
        let (mut pose, place) = self.pose_animated(i);
        if let (None, Some(local)) = (self.display, self.heads[i].local) {
            turn_head(&self.bodies[i], &mut pose, local);
        }
        (pose, place)
    }

    /// The clip fighter `i` shows (clip index, borrowed from the opponent), for [`SecondaryBlend`].
    pub fn shown_clip(&self, i: usize) -> Option<(usize, bool)> {
        let shown = self.display.map_or_else(|| self.snapshot(i), |d| d[i]);
        shown.clip.map(|(c, _)| (c, shown.partner))
    }

    /// The pose as animated, without the head turned (tests).
    #[doc(hidden)]
    pub fn pose_animated_for_tests(&self, i: usize) -> (Vec<Mat4>, Mat4) {
        self.pose_animated(i)
    }

    /// The pose as animated.
    fn pose_animated(&self, i: usize) -> (Vec<Mat4>, Mat4) {
        // (live, a borrowed clip is its owner's, as `clip` took it: in Force's co-op, any foe's)
        let (shown, other) = self.display.map_or_else(|| (self.snapshot(i), self.clip_owner(i)), |d| (d[i], &self.bodies[1 - i]));
        shown_pose(&self.bodies[i], other, &shown)
    }

    /// Collision volumes of both fighters for the pose currently shown (game units).
    pub fn spheres(&self) -> [BodyVolumes; 2] {
        std::array::from_fn(|i| {
            let (pose, place) = self.pose(i);
            volumes(&self.bodies[i], &pose, place)
        })
    }
}

/// A snapshot's pose (node world matrices in metres, as animated) and placement; `other` lends a borrowed clip.
pub fn shown_pose(body: &Body, other: &Body, shown: &Snapshot) -> (Vec<Mat4>, Mat4) {
    let place = placement(shown.root, shown.yaw);
    match shown.clip {
        Some((clip, frame)) if shown.partner => (body.character.pose_from(&other.character, clip, frame, true), place),
        Some((clip, frame)) => (body.character.pose(clip, frame, true), place),
        None => (body.character.pose(0, 0, true), place),
    }
}

/// Turn the head (and everything hanging from it: the second head section, hair) to `local`, its rotation in
/// the chest's frame (game axes).
pub fn turn_head(b: &Body, pose: &mut [Mat4], local: Mat3) {
    let c = Mat3::from_diagonal(Vec3::new(1.0, -1.0, -1.0));
    let old = pose[b.chest];
    let rot = Mat3::from_mat4(pose[b.torso]) * (c * local * c);
    let new = Mat4::from_cols(rot.x_axis.extend(0.0), rot.y_axis.extend(0.0), rot.z_axis.extend(0.0), old.w_axis);
    let delta = new * old.inverse();
    let nodes = &b.character.nodes;
    for (n, m) in pose.iter_mut().enumerate() {
        let mut p = Some(n);
        while let Some(k) = p {
            if k == b.chest {
                *m = if n == b.chest { new } else { delta * *m };
                break;
            }
            p = nodes[k].parent;
        }
    }
}

/// A body's collision volumes for a pose (game units).
pub fn volumes(body: &Body, pose: &[Mat4], place: Mat4) -> BodyVolumes {
    let make = |layout: &[(usize, Vec3, f32, usize)]| -> Vec<Sphere> {
        layout.iter().map(|&(node, offset, radius, zone)| Sphere { centre: (place.transform_point3(pose[node].w_axis.truncate()) + offset) * UNITS, radius, zone }).collect()
    };
    BodyVolumes { hurt: make(&body.hurt), push: make(&body.push) }
}

/// Render camera for a game view: eye and look direction in glTF metres (y up), vertical field of view from
/// the projection distance H and the 368x480 screen with its 7122/4096 Y-row scale (4:3 picture).
pub fn view_camera(v: &View) -> (Vec3, Vec3, f32) {
    let a = |u: i64| u as f32 * std::f32::consts::TAU / 4096.0;
    // a view straight down (the loser shot) keeps a hair of its yaw direction so the look-at has an up
    // pitch as a signed angle (4075 is a little up, not almost a full turn)
    let pitch = ((v.pitch + 2048) & 0xFFF) - 2048;
    let (p, w) = (a(pitch).clamp(-1.5698, 1.5698), a(v.yaw));
    let eye = Vec3::new(v.eye[0] as f32, -v.eye[1] as f32, -v.eye[2] as f32) / UNITS;
    // game forward (-sin w cos p, sin p, cos w cos p), Y and Z flipped into glTF
    let forward = Vec3::new(-w.dsin() * p.dcos(), -p.dsin(), -w.dcos() * p.dcos());
    (eye, forward, 2.0 * (240.0 / (v.h as f32 * 7122.0 / 4096.0)).atan())
}

/// The CPU AI tables (gamedata/ai.json).
pub fn ai_data(assets: &Assets) -> Result<Arc<kumite_sim::ai::AiData>> {
    Ok(Arc::new(kumite_sim::ai::AiData::load(&assets.root.join("gamedata/ai.json"))?))
}

/// Fighter placement (simulation position in game units, facing yaw) as a world transform in metres.
pub fn placement(position: Vec3, yaw: f32) -> Mat4 {
    Mat4::from_rotation_translation(Quat::from_rotation_y(yaw), position / UNITS)
}

/// Sphere layout from rules.json (`hurt_spheres` or `push_spheres`): each sphere follows a joint, with an
/// optional world-up offset; radii by character id (the first set when the section has one for everybody).
fn sphere_layout(section: &serde_json::Value, c: &Character, id: u64) -> Result<Vec<(usize, Vec3, f32, usize)>> {
    let layout = section["layout"].as_array().context("rules.json: sphere layout")?;
    let sets = section["per_character"].as_array().context("rules.json: sphere radii")?;
    let set = sets.iter().find(|e| e["character"].as_u64() == Some(id)).or(sets.first()).context("rules.json: sphere radii")?;
    let radii = set["radii"].as_array().context("rules.json: sphere radii")?;
    layout
        .iter()
        .map(|s| {
            let joint = s["gltf_joint"].as_str().unwrap_or_default();
            let node = c.nodes.iter().position(|n| n.name == joint).with_context(|| format!("no joint {joint}"))?;
            let o = &s["offset_gltf_m"];
            let f = |k: usize| o[k].as_f64().unwrap_or(0.0) as f32;
            let i = s["index"].as_u64().unwrap_or(0) as usize;
            Ok((node, Vec3::new(f(0), f(1), f(2)), radii.get(i).and_then(serde_json::Value::as_f64).unwrap_or(200.0) as f32, i))
        })
        .collect()
}

/// One pad as a script token: "n", "f", "d/b", "1+2", "d/f+1" (the inverse of [`parse_script`]).
pub fn format_pad(p: &Pad) -> String {
    let mut dir: Vec<&str> = Vec::new();
    // same priorities as the input decoder: up over down, right over left
    if p.up {
        dir.push("u");
    } else if p.down {
        dir.push("d");
    }
    if p.right {
        dir.push("f");
    } else if p.left {
        dir.push("b");
    }
    let buttons: Vec<String> = (0..4).filter(|b| p.buttons & (1 << b) != 0).map(|b| (b + 1).to_string()).collect();
    match (dir.is_empty(), buttons.is_empty()) {
        (true, true) => "n".into(),
        (false, true) => dir.join("/"),
        (true, false) => buttons.join("+"),
        (false, false) => format!("{}+{}", dir.join("/"), buttons.join("+")),
    }
}

/// Pads as a run-length script ("f*30,n*5,1,n*40") that [`parse_script`] reads back to the same pads
/// (for the player on the left; "f" = right on the pad).
pub fn format_script(pads: &[Pad]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pads.len() {
        let n = pads[i..].iter().take_while(|p| **p == pads[i]).count();
        let t = format_pad(&pads[i]);
        out.push(if n > 1 { format!("{t}*{n}") } else { t });
        i += n;
    }
    out.join(",")
}

/// The longest pad script: ten hours of frames (a bigger count is a typo, not memory to ask for).
pub const MAX_SCRIPT_FRAMES: usize = 10 * 60 * 60 * 60;

/// Parse an input script like "f*30,n*5,1,d/f+1,n*40" into one pad per frame (the player on the left).
pub fn parse_script(s: &str) -> Result<Vec<Pad>> {
    let mut out = Vec::new();
    for tok in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let (body, n) = tok.split_once('*').map_or((tok, 1), |(b, n)| (b, n.parse().unwrap_or(1)));
        let starts_with_button = body.chars().next().is_some_and(|c| c.is_ascii_digit());
        let (dir, buttons) = if starts_with_button { ("n", body) } else { body.split_once('+').unwrap_or((body, "")) };
        let mut p = Pad::default();
        for part in dir.split('/') {
            match part {
                "f" => p.right = true,
                "b" => p.left = true,
                "u" => p.up = true,
                "d" => p.down = true,
                "n" | "" => {}
                other => anyhow::bail!("unknown direction {other:?} in {tok:?}"),
            }
        }
        for b in buttons.split('+').filter(|b| !b.is_empty()) {
            let k: u8 = b.parse().with_context(|| format!("bad button in {tok:?}"))?;
            anyhow::ensure!((1..=4).contains(&k), "button {k} out of range in {tok:?}");
            p.buttons |= 1 << (k - 1);
        }
        anyhow::ensure!(n <= MAX_SCRIPT_FRAMES - out.len(), "the script is longer than {MAX_SCRIPT_FRAMES} frames");
        out.extend(std::iter::repeat_n(p, n));
    }
    Ok(out)
}
