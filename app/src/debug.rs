//! Debug tools: an egui overlay (performance, fighter/camera state, hit log), testing controls (pause,
//! frame step, slow motion, reset, infinite health, P2 dummy modes, input recording/replay, move browser,
//! character/stage reload, free camera) and debug line drawing (hit volumes, attack paths, skeleton,
//! camera samples).
//!
//! The tools are on with `--debug` (F12 screenshot and M mute work without it). Hotkeys:
//! F1 panel, F2 pause, F3 step one frame, F4 hit volumes, F5 reset round, F6 record P1, F7 P2 mode,
//! F8 pause + free camera (left drag orbits, right drag pans, wheel zooms), F9 god mode (P1 / both / off),
//! F11 save the session demo, F12 screenshot, - / = speed; hold R to rewind (tap while paused: one frame back),
//! 1 K.O. P2 (win the round), 2 K.O. P1, 3 win the match, [ / ] arcade stage back / forward, 5 save state,
//! 6 load state.

use glam::{Mat4, Vec3};
use kumite_game::{Game, UNITS};
use kumite_render::{wgpu, LineVertex, Overlay, RenderStats};
use kumite_sim::data::Bank;
use kumite_sim::input::Pad;
use kumite_sim::Current;
use std::collections::VecDeque;
use web_time::{Duration, Instant};

/// What player 2 does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum P2Mode {
    /// Stands still.
    Idle,
    /// Copies P1's pad, mirrored, so it does the same moves.
    Mirror,
    /// Plays the last P1 recording in a loop (practice-mode dummy).
    Replay,
    /// Holds back: standing guard against highs and mids.
    GuardStand,
    /// Holds down-back: crouching guard against lows (highs whiff over it).
    GuardCrouch,
}

impl P2Mode {
    fn next(self) -> Self {
        match self {
            P2Mode::Idle => P2Mode::Mirror,
            P2Mode::Mirror => P2Mode::Replay,
            P2Mode::Replay => P2Mode::GuardStand,
            P2Mode::GuardStand => P2Mode::GuardCrouch,
            P2Mode::GuardCrouch => P2Mode::Idle,
        }
    }
}

/// Orbit camera around the fighters' midpoint (metres, radians).
#[derive(Clone, Copy, Debug)]
pub struct FreeCam {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    /// Pan of the orbit centre from the fighters' midpoint (metres).
    pub pan: Vec3,
}

/// Something the panel asked for; the app applies it after the UI pass.
#[derive(Clone, Debug)]
pub enum Action {
    ResetRound,
    StepFrames(u32),
    PlayMove {
        player: usize,
        to: Current,
    },
    /// Human (neutral guard) or CPU-style guard for a player (+0xCA).
    SetGuardInput {
        player: usize,
        on: bool,
    },
    Reload {
        models: [usize; 2],
        stage: usize,
    },
    Screenshot,
    SaveRecording,
    Cheat(crate::demo::Cheat),
    SaveState,
    LoadState,
    SaveDemo,
}

/// Debug settings and measurements.
pub struct Debug {
    /// `--debug`: the hotkeys and the panel are on.
    pub enabled: bool,
    pub show_ui: bool,
    pub paused: bool,
    /// Frames still to run while paused (frame step).
    pub step_frames: u32,
    /// Simulation speed (1 = 60 steps per second).
    pub speed: f32,
    pub infinite_health: bool,
    /// P1 cannot lose health.
    pub god_mode: bool,
    /// R is held: the fight runs backwards through the rewind buffer.
    pub rewinding: bool,
    /// Shown in the panel: rewind frames kept, the demo state, the arcade stage to jump to (1..10).
    pub history_len: usize,
    pub demo_status: String,
    pick_level: usize,
    pub draw_hurt: bool,
    pub draw_push: bool,
    pub draw_attack: bool,
    pub draw_skeleton: bool,
    pub draw_samples: bool,
    pub p2_mode: P2Mode,
    pub free_cam: Option<FreeCam>,
    /// P1 pads being recorded.
    pub recording: Option<Vec<Pad>>,
    /// Last finished recording (P2 replays it).
    pub record: Vec<Pad>,
    replay_pos: usize,
    /// Move browser selection per player: (common bank?, index).
    browse: [(bool, usize); 2],
    /// Character/stage picker selection.
    pick_models: [usize; 2],
    pick_stage: usize,
    pub actions: Vec<Action>,
    // ---- measurements
    frame_times: VecDeque<f32>,
    last_frame: Option<Instant>,
    pub step_ms: f32,
    pub render_ms: f32,
    pub ui_ms: f32,
    pub stats: RenderStats,
    pub steps_per_second: f32,
    steps_this_second: u32,
    second_start: Option<Instant>,
    pub hit_log: VecDeque<(u64, kumite_sim::HitEvent)>,
    /// The status message and when it was first drawn (its seconds count from there, not from a slow start).
    pub message: Option<(String, std::cell::Cell<Option<Instant>>)>,
}

/// What the video statistics show besides the measurements.
pub struct VideoInfo {
    /// The frame's size (physical pixels) and the 3D scene's.
    pub frame: [u32; 2],
    pub scene: [u32; 2],
    pub scale: f32,
    pub display: &'static str,
    /// The monitor's refresh rate (Hz), when known.
    pub refresh: Option<f32>,
    pub vsync: &'static str,
    pub textures: &'static str,
    pub adapter: String,
    pub gpu: String,
    pub max_texture: u32,
}

impl Debug {
    pub fn new(enabled: bool, models: [usize; 2], stage: usize) -> Self {
        Self {
            enabled,
            show_ui: enabled,
            paused: false,
            step_frames: 0,
            speed: 1.0,
            infinite_health: false,
            god_mode: false,
            rewinding: false,
            history_len: 0,
            demo_status: String::new(),
            pick_level: 1,
            draw_hurt: false,
            draw_push: false,
            draw_attack: false,
            draw_skeleton: false,
            draw_samples: false,
            p2_mode: P2Mode::Idle,
            free_cam: None,
            recording: None,
            record: Vec::new(),
            replay_pos: 0,
            browse: [(false, 0); 2],
            pick_models: models,
            pick_stage: stage,
            actions: Vec::new(),
            frame_times: VecDeque::new(),
            last_frame: None,
            step_ms: 0.0,
            render_ms: 0.0,
            ui_ms: 0.0,
            stats: RenderStats::default(),
            steps_per_second: 0.0,
            steps_this_second: 0,
            second_start: None,
            hit_log: VecDeque::new(),
            message: None,
        }
    }

    pub fn say(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::info!("{text}");
        self.message = Some((text, Default::default()));
    }

    /// Call once per presented frame.
    pub fn frame_tick(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_frame {
            self.frame_times.push_back((now - last).as_secs_f32() * 1000.0);
            while self.frame_times.len() > 240 {
                self.frame_times.pop_front();
            }
        }
        self.last_frame = Some(now);
        let start = *self.second_start.get_or_insert(now);
        if now - start >= Duration::from_secs(1) {
            self.steps_per_second = self.steps_this_second as f32 / (now - start).as_secs_f32();
            self.steps_this_second = 0;
            self.second_start = Some(now);
        }
    }

    /// Frames per second over the last second of frames.
    pub fn fps(&self) -> f32 {
        let (mut t, mut n) = (0.0, 0);
        for ms in self.frame_times.iter().rev() {
            if t >= 1000.0 {
                break;
            }
            t += ms;
            n += 1;
        }
        if t > 0.0 {
            n as f32 * 1000.0 / t
        } else {
            0.0
        }
    }

    /// Simulation steps to run for `elapsed` wall time, given pause/step/speed (returns the steps; keeps the
    /// fractional remainder in `lag`).
    pub fn steps_for(&mut self, lag: &mut Duration, elapsed: Duration, step: Duration) -> u32 {
        if self.paused {
            *lag = Duration::ZERO;
            let n = self.step_frames;
            self.step_frames = 0;
            return n;
        }
        *lag = (*lag + elapsed.mul_f32(self.speed)).min(step * 10); // don't spiral after a stall
        let mut n = 0;
        while *lag >= step {
            *lag -= step;
            n += 1;
        }
        n
    }

    /// Both pads for this step: P1 from the keyboard/script, P2 from the dummy mode; records P1.
    pub fn pads(&mut self, p1: Pad, p2_script: Option<Pad>) -> [Pad; 2] {
        if let Some(rec) = &mut self.recording {
            rec.push(p1);
        }
        let mirrored = |p: Pad| Pad { left: p.right, right: p.left, ..p };
        let p2 = match (p2_script, self.p2_mode) {
            (Some(p), _) => p,
            (None, P2Mode::Idle) => Pad::default(),
            (None, P2Mode::Mirror) => mirrored(p1),
            (None, P2Mode::Replay) if self.record.is_empty() => Pad::default(),
            (None, P2Mode::Replay) => {
                let p = self.record[self.replay_pos % self.record.len()];
                self.replay_pos = (self.replay_pos + 1) % self.record.len();
                mirrored(p)
            }
            // P2 starts on the right: back is right on the pad
            (None, P2Mode::GuardStand) => Pad { right: true, ..Pad::default() },
            (None, P2Mode::GuardCrouch) => Pad { right: true, down: true, ..Pad::default() },
        };
        self.steps_this_second += 1;
        [p1, p2]
    }

    /// After a step: infinite health, hit log.
    pub fn after_step(&mut self, game: &mut Game) {
        let f = &mut game.fight;
        for h in &f.hits {
            self.hit_log.push_front((f.frame, h.clone()));
        }
        self.hit_log.truncate(14);
        for (i, fighter) in f.fighters.iter_mut().enumerate() {
            if self.infinite_health || (i == 0 && self.god_mode) {
                fighter.health = fighter.max_health * kumite_sim::ONE;
            }
        }
    }

    pub fn toggle_recording(&mut self) {
        match self.recording.take() {
            Some(rec) => {
                let n = rec.len();
                self.record = rec;
                self.replay_pos = 0;
                self.say(format!("recorded {n} frames: {}", short(&kumite_game::format_script(&self.record), 120)));
            }
            None => {
                self.recording = Some(Vec::new());
                self.say("recording P1 (F6 to stop)");
            }
        }
    }

    pub fn toggle_hit_volumes(&mut self) {
        let on = !(self.draw_hurt || self.draw_attack);
        self.draw_hurt = on;
        self.draw_push = on;
        self.draw_attack = on;
        self.draw_samples = on;
    }

    pub fn cycle_p2(&mut self) {
        self.p2_mode = self.p2_mode.next();
        self.replay_pos = 0;
        self.say(format!("P2: {:?}", self.p2_mode));
    }

    /// `from`: direction from the fighters towards the current camera, so the orbit starts where the fight
    /// camera is.
    pub fn toggle_free_cam(&mut self, from: Option<Vec3>) {
        self.free_cam = match self.free_cam {
            Some(_) => None,
            None => {
                let yaw = from.map_or(0.0, |d| d.x.atan2(d.z));
                Some(FreeCam { yaw, pitch: 0.25, distance: 6.0, pan: Vec3::ZERO })
            }
        };
        self.say(if self.free_cam.is_some() { "free camera: left drag orbits, right drag pans, wheel zooms (F8 back)" } else { "fight camera" });
    }

    // ------------------------------------------------------------------ debug lines

    /// Lines for the enabled overlays (metres, y up).
    pub fn lines(&self, game: &Game) -> Vec<LineVertex> {
        let mut out = Vec::new();
        let mut line = |a: Vec3, b: Vec3, c: [f32; 4]| {
            out.push(LineVertex { pos: a.to_array(), color: c });
            out.push(LineVertex { pos: b.to_array(), color: c });
        };
        let ring = |line: &mut dyn FnMut(Vec3, Vec3, [f32; 4]), centre: Vec3, r: f32, c: [f32; 4]| {
            const N: usize = 16;
            // a horizontal and a vertical ring: enough to read the volume without hiding the model
            for axis in [0, 2] {
                for k in 0..N {
                    let p = |k: usize| {
                        let t = k as f32 / N as f32 * std::f32::consts::TAU;
                        let (s, co) = (t.sin() * r, t.cos() * r);
                        centre
                            + match axis {
                                0 => Vec3::new(co, s, 0.0),
                                1 => Vec3::new(0.0, co, s),
                                _ => Vec3::new(co, 0.0, s),
                            }
                    };
                    line(p(k), p(k + 1), c);
                }
            }
        };
        let volumes = game.spheres();
        for (i, v) in volumes.iter().enumerate() {
            let flash = game.fight.fighters[i].hit_flash > 0;
            let guarding = game.fight.fighters[i].posture(&game.fight.data) & kumite_sim::posture::GUARDS != 0;
            if self.draw_hurt {
                // white: just hit; green: guarding; red: open
                let c = if flash {
                    [1.0, 1.0, 1.0, 1.0]
                } else if guarding {
                    [0.3, 1.0, 0.4, 0.9]
                } else {
                    [1.0, 0.25, 0.25, 0.9]
                };
                for s in &v.hurt {
                    ring(&mut line, s.centre / UNITS, s.radius / UNITS, c);
                }
            }
            if self.draw_push {
                for s in &v.push {
                    ring(&mut line, s.centre / UNITS, s.radius / UNITS, [0.3, 0.6, 1.0, 0.8]);
                }
            }
        }
        let gd = game.fight.data.clone();
        for (i, f) in game.fight.fighters.iter().enumerate() {
            if self.draw_attack {
                for (a, b) in f.attack_segments(&gd) {
                    let (a, b) = (a / UNITS, b / UNITS);
                    line(a, b, [1.0, 0.9, 0.1, 1.0]);
                    // thicken a little
                    for d in [Vec3::X, Vec3::Y, Vec3::Z] {
                        line(a + d * 0.01, b + d * 0.01, [1.0, 0.9, 0.1, 1.0]);
                    }
                }
            }
            if self.draw_samples || self.draw_attack {
                // logical position, facing arrow
                let p = f.position / UNITS;
                let dir = Vec3::new(f.yaw.sin(), 0.0, f.yaw.cos());
                let c = if i == 0 { [0.2, 1.0, 1.0, 1.0] } else { [1.0, 0.4, 1.0, 1.0] };
                line(p + Vec3::Y * 0.01, p + dir * 0.5 + Vec3::Y * 0.01, c);
                line(p - Vec3::X * 0.1, p + Vec3::X * 0.1, c);
                line(p - Vec3::Z * 0.1, p + Vec3::Z * 0.1, c);
            }
            if self.draw_skeleton {
                let (pose, place) = game.pose(i);
                let nodes = &game.bodies[i].character.nodes;
                let at = |m: &Mat4| place.transform_point3(m.w_axis.truncate());
                for (n, node) in nodes.iter().enumerate() {
                    if let Some(p) = node.parent {
                        line(at(&pose[p]), at(&pose[n]), [1.0, 1.0, 1.0, 1.0]);
                    }
                }
            }
        }
        if self.draw_samples {
            for s in game.camera_samples() {
                // game space (y down, z away) -> metres
                let p = Vec3::new(s.x as f32, -s.y as f32, -s.z as f32) / UNITS;
                for d in [Vec3::X, Vec3::Y, Vec3::Z] {
                    line(p - d * 0.12, p + d * 0.12, [0.2, 1.0, 0.3, 1.0]);
                }
            }
        }
        out
    }

    // ------------------------------------------------------------------ the panel

    /// Build the debug UI for this frame (only when [`Debug::show_ui`]).
    /// The last status message, for a few seconds (drawn on every screen).
    /// The video statistics in the corner (OPTION > GRAPHICS > STATS, `--stats`, F10): frame rate and times, what
    /// the CPU and the GPU draw, the window, the 3D scene's resolution, the settings, the GPU.
    pub fn video_stats(&self, ctx: &egui::Context, v: &VideoInfo) {
        let mut sorted: Vec<f32> = self.frame_times.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let n = sorted.len().max(1);
        let avg = sorted.iter().sum::<f32>() / n as f32;
        let max = sorted.last().copied().unwrap_or(0.0);
        // 1% low: the frame rate of the slowest 1% of frames
        let worst = &sorted[sorted.len() - (sorted.len() / 100).max(1).min(sorted.len())..];
        let low = if worst.is_empty() { 0.0 } else { 1000.0 / (worst.iter().sum::<f32>() / worst.len() as f32) };
        let fps = self.fps();
        let colour = if fps >= 57.0 {
            egui::Color32::LIGHT_GREEN
        } else if fps >= 45.0 {
            egui::Color32::YELLOW
        } else {
            egui::Color32::from_rgb(255, 90, 90)
        };
        let mono = |t: String| egui::RichText::new(t).monospace().size(11.0).color(egui::Color32::from_gray(225));
        egui::Area::new(egui::Id::new("video_stats")).anchor(egui::Align2::LEFT_BOTTOM, [6.0, -6.0]).interactable(false).show(ctx, |ui| {
            egui::Frame::new().fill(egui::Color32::from_black_alpha(170)).corner_radius(3.0).inner_margin(6.0).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(egui::RichText::new(format!("{fps:.0} FPS")).monospace().size(15.0).strong().color(colour));
                ui.label(mono(format!("frame {avg:5.2} ms  max {max:5.2}  1% low {low:.0}")));
                ui.label(mono(format!("cpu   sim {:.2}  draw {:.2}  ui {:.2} ms", self.step_ms, self.render_ms, self.ui_ms)));
                ui.label(mono(format!("game  {:.0} steps/s", self.steps_per_second)));
                ui.label(mono(format!("gpu   {} calls  {} tris  {} lines", self.stats.draw_calls, self.stats.triangles, self.stats.lines)));
                ui.label(mono(format!("frame {}x{}  {}{}", v.frame[0], v.frame[1], v.display, v.refresh.map_or(String::new(), |hz| format!(" @{hz:.0} Hz")))));
                ui.label(mono(format!("3d    {}x{}  scale {}x", v.scene[0], v.scene[1], v.scale)));
                ui.label(mono(format!("vsync {}  textures {}", v.vsync, v.textures)));
                ui.label(mono(format!("gpu   {}", v.adapter)));
                ui.label(mono(format!("      {}  max texture {}", v.gpu, v.max_texture)));
                // frame times: 240 frames, 33 ms full scale, the line at 16.7 ms (60 FPS)
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.min_rect().width().max(200.0), 28.0), egui::Sense::hover());
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 1.0, egui::Color32::from_black_alpha(120));
                let y = |ms: f32| rect.bottom() - (ms / 33.3).min(1.0) * rect.height();
                p.line_segment([egui::pos2(rect.left(), y(16.7)), egui::pos2(rect.right(), y(16.7))], egui::Stroke::new(1.0, egui::Color32::DARK_GREEN));
                let k0 = self.frame_times.len();
                for (k, ms) in self.frame_times.iter().enumerate() {
                    let x = rect.right() - (k0 - k) as f32 * rect.width() / 240.0;
                    let c = if *ms > 20.0 { egui::Color32::RED } else { egui::Color32::LIGHT_GREEN };
                    p.line_segment([egui::pos2(x, rect.bottom()), egui::pos2(x, y(*ms))], egui::Stroke::new(1.0, c));
                }
            });
        });
    }

    /// No message shown (a video's frames).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn hush(&mut self) {
        self.message = None;
    }

    pub fn toasts(&self, ctx: &egui::Context) {
        if let Some((text, shown)) = &self.message {
            let at = shown.get().unwrap_or_else(Instant::now);
            shown.set(Some(at));
            if at.elapsed() < Duration::from_secs(4) {
                egui::Area::new(egui::Id::new("message")).anchor(egui::Align2::CENTER_BOTTOM, [0.0, -12.0]).show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| ui.add(egui::Label::new(text.as_str()).wrap_mode(egui::TextWrapMode::Extend)));
                });
            }
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, game: &Game, catalog: &Catalog, adapter: &str, size: [u32; 2]) {
        if !self.show_ui {
            return;
        }
        let height = (ctx.content_rect().height() - 40.0).max(200.0);
        // see-through, so the fighters stay visible behind the panel
        let frame = egui::Frame::window(&ctx.global_style()).fill(egui::Color32::from_rgba_unmultiplied(16, 16, 20, 170));
        egui::Window::new("Debug (F1)").default_pos([8.0, 8.0]).default_size([300.0, height]).resizable(true).frame(frame).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.perf_ui(ui, game, adapter, size);
                egui::CollapsingHeader::new("Controls").default_open(true).show(ui, |ui| self.controls_ui(ui));
                egui::CollapsingHeader::new("Jumps, states, demo").default_open(true).show(ui, |ui| self.tools_ui(ui));
                egui::CollapsingHeader::new("Draw").default_open(true).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut self.draw_hurt, "hurt spheres");
                        ui.checkbox(&mut self.draw_push, "push spheres");
                        ui.checkbox(&mut self.draw_attack, "attacks");
                        ui.checkbox(&mut self.draw_skeleton, "skeleton");
                        ui.checkbox(&mut self.draw_samples, "camera samples");
                    });
                });
                egui::CollapsingHeader::new("Hits").default_open(true).show(ui, |ui| {
                    if self.hit_log.is_empty() {
                        ui.weak("no hits yet");
                    }
                    for (frame, h) in &self.hit_log {
                        let text = format!("{frame:5} P{} {} zone {:2} -{:<3} -> {}", h.attacker + 1, h.anim, h.zone, h.damage, h.health);
                        if h.counter {
                            ui.colored_label(egui::Color32::from_rgb(255, 160, 60), egui::RichText::new(text + "  COUNTER").monospace());
                        } else if h.blocked {
                            ui.colored_label(egui::Color32::LIGHT_GREEN, egui::RichText::new(text + "  BLOCKED").monospace());
                        } else {
                            ui.monospace(text);
                        }
                    }
                });
                for i in 0..2 {
                    egui::CollapsingHeader::new(format!("P{} {}", i + 1, game.bodies[i].character.name)).default_open(i == 0).show(ui, |ui| self.fighter_ui(ui, game, i));
                }
                egui::CollapsingHeader::new("Camera").show(ui, |ui| camera_ui(ui, game));
                egui::CollapsingHeader::new("Fight setup").show(ui, |ui| self.setup_ui(ui, catalog));
            });
        });
    }

    fn perf_ui(&mut self, ui: &mut egui::Ui, game: &Game, adapter: &str, size: [u32; 2]) {
        let (avg, max) = {
            let n = self.frame_times.len().max(1) as f32;
            (self.frame_times.iter().sum::<f32>() / n, self.frame_times.iter().cloned().fold(0.0, f32::max))
        };
        ui.heading(format!("{:.0} FPS", self.fps()));
        egui::Grid::new("perf").num_columns(2).show(ui, |ui| {
            ui.label("frame");
            ui.monospace(format!("{avg:5.2} ms avg, {max:5.2} max"));
            ui.end_row();
            ui.label("sim step / render / ui");
            ui.monospace(format!("{:.3} / {:.2} / {:.2} ms", self.step_ms, self.render_ms, self.ui_ms));
            ui.end_row();
            ui.label("sim");
            ui.monospace(format!("{:.0} steps/s, frame {}", self.steps_per_second, game.fight.frame));
            ui.end_row();
            ui.label("draws");
            ui.monospace(format!("{} calls, {} tris, {} lines", self.stats.draw_calls, self.stats.triangles, self.stats.lines));
            ui.end_row();
            ui.label("screen");
            ui.monospace(format!("{}x{}", size[0], size[1]));
            ui.end_row();
            ui.label("GPU");
            ui.label(adapter);
            ui.end_row();
        });
        // frame-time graph: 240 frames, 33 ms full scale, line at 16.7 ms
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 2.0, egui::Color32::from_black_alpha(120));
        let y = |ms: f32| rect.bottom() - (ms / 33.3).min(1.0) * rect.height();
        p.line_segment([egui::pos2(rect.left(), y(16.7)), egui::pos2(rect.right(), y(16.7))], egui::Stroke::new(1.0, egui::Color32::DARK_GREEN));
        let n = self.frame_times.len();
        for (k, ms) in self.frame_times.iter().enumerate() {
            let x = rect.right() - (n - k) as f32 * rect.width() / 240.0;
            let c = if *ms > 20.0 { egui::Color32::RED } else { egui::Color32::LIGHT_GREEN };
            p.line_segment([egui::pos2(x, rect.bottom()), egui::pos2(x, y(*ms))], egui::Stroke::new(1.0, c));
        }
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(if self.paused { "▶ Resume (F2)" } else { "⏸ Pause (F2)" }).clicked() {
                self.paused = !self.paused;
            }
            ui.add_enabled_ui(self.paused, |ui| {
                if ui.button("Step (F3)").clicked() {
                    self.actions.push(Action::StepFrames(1));
                }
                if ui.button("+10").clicked() {
                    self.actions.push(Action::StepFrames(10));
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("speed");
            ui.add(egui::Slider::new(&mut self.speed, 0.05..=4.0).logarithmic(true).suffix("x"));
        });
        ui.horizontal(|ui| {
            if ui.button("Reset round (F5)").clicked() {
                self.actions.push(Action::ResetRound);
            }
            ui.checkbox(&mut self.god_mode, "god mode P1 (F9)");
            ui.checkbox(&mut self.infinite_health, "both");
        });
        ui.horizontal(|ui| {
            ui.label("P2 (F7)");
            for m in [P2Mode::Idle, P2Mode::Mirror, P2Mode::Replay] {
                ui.selectable_value(&mut self.p2_mode, m, format!("{m:?}"));
            }
        });
        ui.horizontal(|ui| {
            ui.label("    guard");
            ui.selectable_value(&mut self.p2_mode, P2Mode::GuardStand, "hold back");
            ui.selectable_value(&mut self.p2_mode, P2Mode::GuardCrouch, "hold down-back");
        });
        ui.horizontal(|ui| {
            let rec = self.recording.is_some();
            if ui.button(if rec { "⏹ Stop recording (F6)" } else { "⏺ Record P1 (F6)" }).clicked() {
                self.toggle_recording();
            }
            if rec {
                ui.colored_label(egui::Color32::RED, format!("{} frames", self.recording.as_ref().map_or(0, Vec::len)));
            } else {
                ui.label(format!("{} frames recorded", self.record.len()));
            }
        });
        ui.add_enabled_ui(!self.record.is_empty(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("Copy script").clicked() {
                    ui.ctx().copy_text(kumite_game::format_script(&self.record));
                    self.say("recording copied as a --p1 script");
                }
                if ui.button("Save").clicked() {
                    self.actions.push(Action::SaveRecording);
                }
            });
        });
        ui.horizontal(|ui| {
            let mut free = self.free_cam.is_some();
            if ui.checkbox(&mut free, "free camera (F8)").changed() {
                self.toggle_free_cam(None);
            }
            if ui.button("Screenshot (F12)").clicked() {
                self.actions.push(Action::Screenshot);
            }
        });
    }

    fn tools_ui(&mut self, ui: &mut egui::Ui) {
        use crate::demo::Cheat;
        ui.horizontal(|ui| {
            if ui.button("K.O. P2 (1)").clicked() {
                self.actions.push(Action::Cheat(Cheat::Ko(1)));
            }
            if ui.button("K.O. P1 (2)").clicked() {
                self.actions.push(Action::Cheat(Cheat::Ko(0)));
            }
            if ui.button("Win match (3)").clicked() {
                self.actions.push(Action::Cheat(Cheat::WinMatch));
            }
        });
        ui.horizontal(|ui| {
            ui.label("arcade stage");
            ui.add(egui::DragValue::new(&mut self.pick_level).range(1..=99)); // (the app keeps it within the ladder)
            if ui.button("Go ([ / ])").clicked() {
                self.actions.push(Action::Cheat(Cheat::ArcadeStage(self.pick_level - 1)));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Save state (5)").clicked() {
                self.actions.push(Action::SaveState);
            }
            if ui.button("Load state (6)").clicked() {
                self.actions.push(Action::LoadState);
            }
            ui.label(format!("rewind: {} frames (hold R)", self.history_len));
        });
        ui.horizontal(|ui| {
            if ui.button("Save demo (F11)").clicked() {
                self.actions.push(Action::SaveDemo);
            }
            ui.label(&self.demo_status);
        });
    }

    fn fighter_ui(&mut self, ui: &mut egui::Ui, game: &Game, i: usize) {
        let gd = &game.fight.data;
        let f = &game.fight.fighters[i];
        let mv = f.current_move(gd);
        let bank = match f.current.bank {
            Bank::Common => "common",
            Bank::Partner => "opponent's",
            _ => "own",
        };
        egui::Grid::new(("fighter", i)).num_columns(2).show(ui, |ui| {
            ui.label("health");
            ui.add(egui::ProgressBar::new(f.health as f32 / (gd.max_health * kumite_sim::ONE) as f32).text(format!("{}", f.health / kumite_sim::ONE)));
            ui.end_row();
            ui.label("move");
            ui.monospace(format!("{bank} #{} {}  {}/{}", f.current.index, mv.anim, f.frame, mv.frames));
            ui.end_row();
            ui.label("attack");
            if mv.active == [0, 0] {
                ui.monospace("-");
            } else {
                ui.monospace(format!("{} {} dmg, active {}-{}", mv.attack_mask.level_name, mv.damage, mv.active[0], mv.active[1]));
            }
            ui.end_row();
            ui.label("posture");
            let p = f.posture(gd);
            let guard = match (p & kumite_sim::posture::STAND_GUARD != 0, p & kumite_sim::posture::CROUCH_GUARD != 0) {
                (true, true) => "guard high+low",
                (true, false) => "guard high/mid",
                (false, true) => "guard low",
                _ => "open",
            };
            ui.monospace(format!("{p:#06x} {guard}"));
            ui.end_row();
            ui.label("player");
            let mut cpu = !f.guard_input;
            if ui.checkbox(&mut cpu, "CPU (no neutral guard)").changed() {
                self.actions.push(Action::SetGuardInput { player: i, on: !cpu });
            }
            ui.end_row();
            ui.label("queued");
            match f.queued() {
                Some((to, exec)) => ui.monospace(format!("{:?} #{} at {}", to.bank, to.index, if exec == 255 { "end".into() } else { exec.to_string() })),
                None => ui.monospace("-"),
            };
            ui.end_row();
            ui.label("position");
            ui.monospace(format!("({:.0}, {:.0}) root ({:.0}, {:.0}) yaw {:.1}°", f.position.x, f.position.z, f.root().x, f.root().z, f.yaw.to_degrees()));
            ui.end_row();
            ui.label("input");
            ui.monospace(input_history(&f.input, 40));
            ui.end_row();
        });
        // move browser
        let (common, index) = &mut self.browse[i];
        let set = if *common { &gd.common } else { &*f.moveset };
        ui.horizontal(|ui| {
            ui.label("play move");
            ui.selectable_value(common, false, "own");
            ui.selectable_value(common, true, "common");
            ui.add(egui::DragValue::new(index).range(0..=set.moves.len().saturating_sub(1)));
        });
        if let Some(m) = set.moves.get(*index) {
            ui.horizontal(|ui| {
                ui.monospace(format!("{} {}f dmg {} {}", m.anim, m.frames, m.damage, m.attack_mask.level_name));
                if ui.button("Play").clicked() {
                    let to = Current { bank: if *common { Bank::Common } else { Bank::Own }, index: *index };
                    self.actions.push(Action::PlayMove { player: i, to });
                }
            });
        }
    }

    fn setup_ui(&mut self, ui: &mut egui::Ui, catalog: &Catalog) {
        for p in 0..2 {
            let label = catalog.characters.iter().find(|c| c.0 == self.pick_models[p]).map_or("?".to_string(), |c| c.1.clone());
            egui::ComboBox::from_label(format!("P{}", p + 1)).selected_text(label).height(400.0).show_ui(ui, |ui| {
                for (record, name) in &catalog.characters {
                    ui.selectable_value(&mut self.pick_models[p], *record, name);
                }
            });
        }
        let stage = catalog.stages.get(self.pick_stage).cloned().unwrap_or_default();
        egui::ComboBox::from_label("stage").selected_text(stage).height(400.0).show_ui(ui, |ui| {
            for (k, name) in catalog.stages.iter().enumerate() {
                ui.selectable_value(&mut self.pick_stage, k, name);
            }
        });
        if ui.button("Load fight").clicked() {
            self.actions.push(Action::Reload { models: self.pick_models, stage: self.pick_stage });
        }
    }
}

fn camera_ui(ui: &mut egui::Ui, game: &Game) {
    let v = game.camera.view;
    egui::Grid::new("camera").num_columns(2).show(ui, |ui| {
        ui.label("eye");
        ui.monospace(format!("{:?}", v.eye));
        ui.end_row();
        ui.label("pitch / yaw");
        ui.monospace(format!("{} / {}  ({:.1}° / {:.1}°)", v.pitch, v.yaw, v.pitch as f32 * 360.0 / 4096.0, v.yaw as f32 * 360.0 / 4096.0));
        ui.end_row();
        ui.label("H");
        ui.monospace(v.h.to_string());
        ui.end_row();
        ui.label("intro");
        ui.monospace(game.camera.in_intro().to_string());
        ui.end_row();
        for (i, s) in game.camera_samples().iter().enumerate() {
            ui.label(format!("sample P{}", i + 1));
            ui.monospace(format!("({}, {}, {}) side {}", s.x, s.y, s.z, s.side));
            ui.end_row();
        }
    });
}

/// Recent input as numpad notation, newest last, runs merged: "5x12 6x3 6+1".
pub fn input_history(input: &kumite_sim::input::InputState, frames: usize) -> String {
    let mut runs: Vec<(String, usize)> = Vec::new();
    for ago in (0..frames).rev() {
        let (new, dir) = input.past(ago);
        let mut t = dir.to_string();
        for b in 0..4 {
            if new & (1 << b) != 0 {
                t.push('+');
                t.push_str(&(b + 1).to_string());
            }
        }
        match runs.last_mut() {
            Some((last, n)) if *last == t => *n += 1,
            _ => runs.push((t, 1)),
        }
    }
    runs.iter().map(|(t, n)| if *n > 1 { format!("{t}x{n}") } else { t.clone() }).collect::<Vec<_>>().join(" ")
}

fn short(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

/// Character and stage names for the pickers (from the converted roster.json / stages.json).
#[derive(Default)]
pub struct Catalog {
    /// (model record, label)
    pub characters: Vec<(usize, String)>,
    pub stages: Vec<String>,
}

impl Catalog {
    pub fn load(gamedata: &std::path::Path) -> Self {
        let mut c = Catalog::default();
        let read = |name: &str| -> serde_json::Value { kumite_fs::read(&gamedata.join(name)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default() };
        let roster = read("roster.json");
        let index = read("movesets/index.json");
        let playable: Vec<u64> = index["models"].as_array().into_iter().flatten().filter_map(|m| m["model_record"].as_u64()).collect();
        for e in roster["selections"]["entries"].as_array().into_iter().flatten() {
            let (Some(rec), Some(name)) = (e["model_record"].as_u64(), e["name"].as_str()) else { continue };
            if playable.contains(&rec) && !c.characters.iter().any(|x| x.0 == rec as usize) {
                c.characters.push((rec as usize, format!("{name} c{} ({rec})", e["costume"].as_u64().unwrap_or(0))));
            }
        }
        for rec in playable {
            if !c.characters.iter().any(|x| x.0 == rec as usize) {
                c.characters.push((rec as usize, format!("model {rec}")));
            }
        }
        c.stages = read("stages.json")["stages"].as_array().into_iter().flatten().map(|s| s["name"].as_str().unwrap_or("?").to_string()).collect();
        c
    }
}

// ---------------------------------------------------------------------- egui rendering

/// egui state for the window (input + GPU renderers).
pub struct EguiLayer {
    pub ctx: egui::Context,
    /// egui's window input (natively; a browser build feeds it the screen size only, the game reads the keys).
    #[cfg(not(target_arch = "wasm32"))]
    state: Option<egui_winit::State>,
    renderer: egui_wgpu::Renderer,
    /// Draws what comes before the [`UnderMarker`] behind the fighters (own vertex buffers, same textures).
    under_renderer: egui_wgpu::Renderer,
    /// Texture updates not yet applied (a skipped frame must not lose them: later deltas build on them).
    pending: egui::TexturesDelta,
}

/// Marks the end of the 2D drawn behind the fighters: paint everything before it with
/// [`mark_under`] in an `Order::Background` layer.
pub struct UnderMarker;

/// End the "behind the fighters" part of this frame's shapes (call last in that layer).
pub fn mark_under(painter: &egui::Painter) {
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0));
    painter.add(egui::Shape::Callback(egui::epaint::PaintCallback { rect, callback: std::sync::Arc::new(UnderMarker) }));
}

/// Split a frame's primitives at the [`UnderMarker`]: (drawn behind the fighters, drawn on top). Without a
/// marker everything is on top.
fn split_under(mut jobs: Vec<egui::ClippedPrimitive>) -> (Vec<egui::ClippedPrimitive>, Vec<egui::ClippedPrimitive>) {
    let is_marker = |p: &egui::ClippedPrimitive| matches!(&p.primitive, egui::epaint::Primitive::Callback(c) if c.callback.is::<UnderMarker>());
    match jobs.iter().position(is_marker) {
        Some(i) => {
            let over = jobs.split_off(i + 1);
            jobs.pop();
            (jobs, over)
        }
        None => (Vec::new(), jobs),
    }
}

/// One tessellated egui frame, drawn as a [`kumite_render::Overlay`]: `under` behind the characters, `jobs` on top.
pub struct EguiFrame<'a> {
    renderer: &'a mut egui_wgpu::Renderer,
    under_renderer: &'a mut egui_wgpu::Renderer,
    under: Vec<egui::ClippedPrimitive>,
    jobs: Vec<egui::ClippedPrimitive>,
    textures: &'a mut egui::TexturesDelta,
    textures_done: bool,
    pixels_per_point: f32,
}

impl EguiLayer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, window: Option<&winit::window::Window>) -> Self {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        #[cfg(not(target_arch = "wasm32"))]
        let state = window.map(|w| egui_winit::State::new(ctx.clone(), egui::ViewportId::ROOT, w, Some(w.scale_factor() as f32), None, None));
        #[cfg(target_arch = "wasm32")]
        let _ = window;
        let renderer = egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default());
        let under_renderer = egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default());
        Self {
            ctx,
            #[cfg(not(target_arch = "wasm32"))]
            state,
            renderer,
            under_renderer,
            pending: Default::default(),
        }
    }

    /// Feed a window event; true when egui used it (the game should ignore it).
    pub fn on_event(&mut self, window: &winit::window::Window, event: &winit::event::WindowEvent) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.state.as_mut().is_some_and(|s| s.on_window_event(window, event).consumed);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (window, event);
            false
        }
    }
    pub fn wants_keyboard(&self) -> bool {
        self.ctx.egui_wants_keyboard_input()
    }

    pub fn wants_pointer(&self) -> bool {
        self.ctx.egui_wants_pointer_input()
    }

    /// Run the UI for one frame. Without a window (headless shots) the screen is `size` at 1 point per pixel.
    pub fn run(&mut self, window: Option<&winit::window::Window>, size: [u32; 2], ui: impl FnMut(&egui::Context)) -> EguiFrame<'_> {
        let plain = || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32))), ..Default::default() };
        #[cfg(not(target_arch = "wasm32"))]
        let raw = match (&mut self.state, window) {
            (Some(s), Some(w)) => s.take_egui_input(w),
            _ => plain(),
        };
        #[cfg(target_arch = "wasm32")]
        let raw = {
            let _ = window;
            plain()
        };
        let mut ui = ui;
        let out = self.ctx.run_ui(raw, |root| ui(&root.ctx().clone()));
        #[cfg(not(target_arch = "wasm32"))]
        if let (Some(s), Some(w)) = (&mut self.state, window) {
            s.handle_platform_output(w, out.platform_output);
        }
        let jobs = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        let (under, jobs) = split_under(jobs);
        self.pending.append(out.textures_delta);
        EguiFrame { renderer: &mut self.renderer, under_renderer: &mut self.under_renderer, under, jobs, textures: &mut self.pending, textures_done: false, pixels_per_point: out.pixels_per_point }
    }
}

impl EguiFrame<'_> {
    /// Apply the pending texture uploads to both renderers (once a frame, before either draws).
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.textures_done {
            return;
        }
        self.textures_done = true;
        for (id, deltas) in &self.textures.set {
            for delta in deltas {
                self.renderer.update_texture(device, queue, *id, delta);
                self.under_renderer.update_texture(device, queue, *id, delta);
            }
        }
        self.textures.set.clear();
    }
}

/// Record one egui render pass of `jobs` onto `target`.
fn paint(
    renderer: &mut egui_wgpu::Renderer,
    jobs: &[egui::ClippedPrimitive],
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    screen: &egui_wgpu::ScreenDescriptor,
) {
    if jobs.is_empty() {
        return;
    }
    let extra = renderer.update_buffers(device, queue, encoder, jobs, screen);
    if !extra.is_empty() {
        queue.submit(extra);
    }
    let mut pass = encoder
        .begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
        .forget_lifetime();
    renderer.render(&mut pass, jobs, screen);
}

impl Overlay for EguiFrame<'_> {
    fn draw_under(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, size: [u32; 2], scale: f32) {
        self.upload(device, queue);
        // into the 3D scene's target: its pixels are the window's times the render scale
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: size, pixels_per_point: self.pixels_per_point * scale };
        paint(self.under_renderer, &self.under, device, queue, encoder, target, &screen);
    }

    fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, size: [u32; 2]) {
        self.upload(device, queue);
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: size, pixels_per_point: self.pixels_per_point };
        paint(self.renderer, &self.jobs, device, queue, encoder, target, &screen);
        for id in std::mem::take(&mut self.textures.free) {
            self.renderer.free_texture(&id);
            self.under_renderer.free_texture(&id);
        }
    }
}

#[cfg(test)]
mod under_tests {
    use super::*;

    #[test]
    fn the_hud_layer_splits_off_behind_the_fighters() {
        let ctx = egui::Context::default();
        let mut shapes = 0;
        let mut out = ctx.run_ui(egui::RawInput::default(), |root| {
            let ctx = root.ctx().clone();
            let back = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("hud")));
            back.rect_filled(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)), 0.0, egui::Color32::RED);
            back.rect_filled(egui::Rect::from_min_size(egui::pos2(20.0, 0.0), egui::vec2(10.0, 10.0)), 0.0, egui::Color32::RED);
            mark_under(&back);
            let front = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("front")));
            front.rect_filled(egui::Rect::from_min_size(egui::pos2(0.0, 20.0), egui::vec2(10.0, 10.0)), 0.0, egui::Color32::BLUE);
            shapes += 1;
        });
        // (egui checks in debug builds that the texture changes were taken, as the app does)
        out.textures_delta.clear();
        let jobs = ctx.tessellate(out.shapes, out.pixels_per_point);
        let (under, over) = split_under(jobs);
        let colours = |j: &[egui::ClippedPrimitive]| -> Vec<egui::Color32> {
            j.iter()
                .filter_map(|p| match &p.primitive {
                    egui::epaint::Primitive::Mesh(m) => m.vertices.first().map(|v| v.color),
                    _ => None,
                })
                .collect()
        };
        assert!(colours(&under).iter().all(|c| *c == egui::Color32::RED) && !under.is_empty(), "the HUD goes behind");
        assert_eq!(colours(&over), [egui::Color32::BLUE], "the front layer stays on top, the marker is dropped");
        assert_eq!(shapes, 1);
        let (none, all) = split_under(ctx.tessellate(Vec::new(), 1.0));
        assert!(none.is_empty() && all.is_empty());
    }
}
