//! Watching replays (`replay.rs`): the browser over the main menu, and the viewer over the match played again —
//! play / pause, speed, a step at a time, seeking on a timeline with the rounds marked, the free camera, the HUD off,
//! each player's inputs.
//!
//! Seeking: before it plays, the match is run through once, quickly, from its inputs (by the app: a Force stage's
//! logic and its players joining with it); a copy of the match is kept every [`SNAPSHOT_EVERY`] steps, the rounds'
//! starts noted, and the end checked against the recorded state. A seek takes the copy before the step and runs on
//! to it.

use crate::hud::{tag::TagHud, HudState};
use crate::replay::{self, Entry, ReplayFile};
use egui::{Align2, Color32, RichText};
use kumite_game::bout::Bout;

/// Steps between the copies a seek starts from (5 seconds).
pub const SNAPSHOT_EVERY: usize = 300;
/// The viewer's speeds.
pub const SPEEDS: [f32; 6] = [0.1, 0.25, 0.5, 1.0, 2.0, 4.0];

/// What the viewer asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Request {
    Seek(usize),
    TogglePause,
    Step,
    Speed(f32),
    FreeCam,
    Keep(bool),
    /// Make it a video (native).
    Video,
    Exit,
}

/// The match at a step, of whichever kind, with its HUD (what a seek starts from).
#[derive(Clone)]
pub enum Snapshot {
    /// A fight (a Force stage's: with the run and its stage), its HUD and what its sounds go by.
    Fight {
        bout: Box<Bout>,
        force: Option<Box<crate::forcerun::ForceRun>>,
        hud: HudState,
        heard: Box<crate::Heard>,
    },
    Tag(Box<kumite_game::tag::TagGame>, Box<TagHud>),
}

pub struct View {
    pub id: String,
    pub file: ReplayFile,
    /// Steps in the match.
    pub total: usize,
    /// Copies of the match every [`SNAPSHOT_EVERY`] steps (the app's run-through).
    pub snapshots: Vec<(usize, Snapshot)>,
    /// The step each round's call began at, and its number.
    pub rounds: Vec<(usize, u32)>,
    /// The run-through ended in the recorded state.
    pub verified: bool,
    pub hud: bool,
    pub inputs: bool,
    /// The controls shown (not in a video).
    pub bar: bool,
    /// The camera counters before the replay (put back after it).
    pub counters_before: [usize; 3],
    /// Played to its last step (since the last seek back), and whether it ended there as recorded.
    pub ended: bool,
    pub watched_ok: Option<bool>,
    pub requests: Vec<Request>,
}

impl View {
    /// The replay of `entry`, its run-through still to come (the app's).
    pub fn new(entry: Entry, counters_before: [usize; 3]) -> Self {
        let total = entry.file.steps as usize;
        Self {
            id: entry.id,
            file: entry.file,
            total,
            snapshots: Vec::new(),
            rounds: Vec::new(),
            verified: false,
            hud: true,
            inputs: false,
            bar: true,
            counters_before,
            ended: false,
            watched_ok: None,
            requests: Vec::new(),
        }
    }

    /// The copy a seek to `step` starts from.
    pub fn snapshot_before(&self, step: usize) -> Option<&(usize, Snapshot)> {
        self.snapshots.iter().rev().find(|(s, _)| *s <= step)
    }

    /// The bar at the bottom: what plays, the controls, the timeline; each player's inputs at the sides.
    /// `at`: the steps played; `inputs`: each player's name and input history.
    pub fn ui(&mut self, ctx: &egui::Context, at: usize, inputs: &[(String, &kumite_sim::input::InputState)], paused: bool, speed: f32, free_cam: bool) {
        if !self.bar {
            return;
        }
        let at = at.min(self.total);
        let mut req = Vec::new();
        egui::Area::new(egui::Id::new("replay_bar")).anchor(Align2::CENTER_BOTTOM, [0.0, -14.0]).show(ctx, |ui| {
            egui::Frame::new().fill(Color32::from_black_alpha(200)).corner_radius(10).inner_margin(12).stroke(egui::Stroke::new(1.0, Color32::from_white_alpha(30))).show(ui, |ui| {
                ui.set_width(620.0f32.min(ctx.content_rect().width() - 40.0));
                ui.horizontal(|ui| {
                    ui.label(RichText::new("REPLAY").strong().color(Color32::from_rgb(255, 106, 61)));
                    ui.label(RichText::new(&self.file.title).color(Color32::WHITE));
                    ui.label(RichText::new(format!("· {}", self.file.result)).color(Color32::GRAY));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.watched_ok == Some(false) {
                            ui.label(RichText::new("⚠ ended differently").small().color(Color32::from_rgb(255, 190, 80))).on_hover_text("Watched to its end, the match was not in the recorded state.");
                        } else if self.verified {
                            ui.label(RichText::new("✔ verified").small().color(Color32::from_rgb(120, 200, 120))).on_hover_text("Played through once: it ends exactly as recorded.");
                        } else {
                            ui.label(RichText::new("⚠ differs").small().color(Color32::from_rgb(255, 190, 80)))
                                .on_hover_text("Played through once, it did not end as recorded: what shows may differ from the match.");
                        }
                    });
                });
                ui.add_space(4.0);
                // the timeline: a slider over the steps, the rounds marked under it
                let mut pos = at as f32;
                let w = ui.available_width();
                ui.spacing_mut().slider_width = w - 110.0;
                let resp = ui.horizontal(|ui| {
                    let r = ui.add(egui::Slider::new(&mut pos, 0.0..=self.total.max(1) as f32).show_value(false));
                    ui.label(RichText::new(format!("{} / {}", replay::duration(at as u64), replay::duration(self.total as u64))).monospace().color(Color32::WHITE));
                    r
                });
                let slider = resp.inner.rect;
                for (step, n) in &self.rounds {
                    let x = slider.left() + slider.width() * (*step as f32 / self.total.max(1) as f32);
                    let p = ui.painter();
                    p.line_segment([egui::pos2(x, slider.bottom() - 2.0), egui::pos2(x, slider.bottom() + 4.0)], egui::Stroke::new(2.0, Color32::from_rgb(255, 209, 102)));
                    p.text(egui::pos2(x + 2.0, slider.bottom() + 4.0), Align2::LEFT_TOP, format!("R{n}"), egui::FontId::proportional(10.0), Color32::from_rgb(255, 209, 102));
                }
                if resp.inner.changed() {
                    req.push(Request::Seek(pos as usize));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let button = |ui: &mut egui::Ui, text: &str, tip: &str| ui.add(egui::Button::new(RichText::new(text).size(16.0))).on_hover_text(tip).clicked();
                    if button(ui, "⏮", "Start (Home)") {
                        req.push(Request::Seek(0));
                    }
                    let prev_round = self.rounds.iter().rev().map(|r| r.0).find(|&s| s + 30 < at).unwrap_or(0);
                    if button(ui, "⏪", "Previous round (PgUp)") {
                        req.push(Request::Seek(prev_round));
                    }
                    if button(ui, "-5s", "Back 5 seconds (←)") {
                        req.push(Request::Seek(at.saturating_sub(300)));
                    }
                    if button(ui, if paused { "▶" } else { "⏸" }, "Play / pause (Space, START)") {
                        req.push(Request::TogglePause);
                    }
                    if button(ui, "+5s", "Forward 5 seconds (→)") {
                        req.push(Request::Seek((at + 300).min(self.total)));
                    }
                    if let Some(next) = self.rounds.iter().map(|r| r.0).find(|&s| s > at) {
                        if button(ui, "⏩", "Next round (PgDn)") {
                            req.push(Request::Seek(next));
                        }
                    }
                    if button(ui, "⏭", "One step (.) — pauses") {
                        req.push(Request::Step);
                    }
                    ui.separator();
                    egui::ComboBox::from_id_salt("replay_speed")
                        .width(64.0)
                        .selected_text(format!("{speed}×"))
                        .show_ui(ui, |ui| {
                            for s in SPEEDS {
                                if ui.selectable_label((s - speed).abs() < 1e-3, format!("{s}×")).clicked() {
                                    req.push(Request::Speed(s));
                                }
                            }
                        })
                        .response
                        .on_hover_text("Speed (↑ ↓)");
                    ui.separator();
                    let toggle = |ui: &mut egui::Ui, on: bool, text: &str, tip: &str| ui.add(egui::Button::new(text).selected(on)).on_hover_text(tip).clicked();
                    if toggle(ui, free_cam, "Camera", "Free camera: drag to orbit, right drag to pan, wheel to zoom (C)") {
                        req.push(Request::FreeCam);
                    }
                    if toggle(ui, self.hud, "HUD", "Show the HUD (H)") {
                        self.hud = !self.hud;
                    }
                    if toggle(ui, self.inputs, "Inputs", "Each player's inputs, in numpad notation (I)") {
                        self.inputs = !self.inputs;
                    }
                    if toggle(ui, self.file.kept, "★ Keep", "Keep this replay: it is never deleted to make room (K)") {
                        req.push(Request::Keep(!self.file.kept));
                    }
                    if cfg!(not(target_arch = "wasm32")) && ui.button("Video…").on_hover_text("Make it an MP4 video (V)").clicked() {
                        req.push(Request::Video);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Exit").on_hover_text("Back to the replays (Esc)").clicked() {
                            req.push(Request::Exit);
                        }
                    });
                });
            });
        });
        if self.inputs {
            // (the first half on the left, the rest on the right: two players, or a tag brawl's teams)
            let half = inputs.len().div_ceil(2);
            for (i, (name, input)) in inputs.iter().enumerate() {
                let (anchor, x, k) = if i < half { (Align2::LEFT_CENTER, 14.0, i) } else { (Align2::RIGHT_CENTER, -14.0, i - half) };
                let y = (k as f32 - (half as f32 - 1.0) / 2.0) * 330.0 / half as f32;
                egui::Area::new(egui::Id::new(("replay_inputs", i))).anchor(anchor, [x, y]).show(ctx, |ui| {
                    egui::Frame::new().fill(Color32::from_black_alpha(170)).corner_radius(8).inner_margin(8).show(ui, |ui| {
                        ui.label(RichText::new(name).strong().color(Color32::WHITE));
                        let text = crate::debug::input_history(input, 60);
                        for line in text.split(' ').rev().take(if half > 1 { 6 } else { 14 }) {
                            ui.label(RichText::new(line).monospace().color(Color32::from_gray(225)));
                        }
                    });
                });
            }
        }
        self.requests.extend(req);
    }
}

/// The replays over the main menu: the list, newest first; play, keep, delete.
pub struct Browser {
    pub entries: Vec<Entry>,
    pub cursor: usize,
    /// The game data's fingerprint: a replay of other data cannot play.
    data: String,
    /// Delete asked once (the replay's id): a second time deletes.
    confirm_delete: Option<String>,
    pub message: Option<String>,
}

/// What the browser asks the app to do.
pub enum BrowserAction {
    Play(Box<Entry>),
    /// Save it as a file to share.
    Export(Box<Entry>),
    /// Open replay files someone shared.
    Import,
    /// Make it a video (native).
    Video(#[cfg_attr(target_arch = "wasm32", allow(dead_code))] Box<Entry>),
    Close,
}

impl Browser {
    /// The list, `selected` (a replay just watched) under the cursor.
    pub fn new(data: String, selected: Option<&str>) -> Self {
        let entries = replay::list();
        let cursor = selected.and_then(|id| entries.iter().position(|e| e.id == id)).unwrap_or(0);
        Self { entries, cursor, data, confirm_delete: None, message: None }
    }

    fn playable(&self, e: &Entry) -> bool {
        e.file.plays_on(&self.data)
    }

    /// A key from the keyboard or a pad: up / down, START plays, button 1 keeps, button 2 deletes (twice), button 3
    /// exports, button 4 imports, BACK closes.
    pub fn key(&mut self, key: crate::menu::Key) -> Option<BrowserAction> {
        use crate::menu::Key;
        let n = self.entries.len();
        match key {
            Key::Up if n > 0 => self.cursor = (self.cursor + n - 1) % n,
            Key::Down if n > 0 => self.cursor = (self.cursor + 1) % n,
            Key::Start => return self.play(self.cursor),
            Key::Button(0) => self.toggle_keep(self.cursor),
            Key::Button(1) => self.delete(self.cursor),
            Key::Button(2) => return self.entries.get(self.cursor).map(|e| BrowserAction::Export(Box::new(e.clone()))),
            Key::Button(3) => return Some(BrowserAction::Import),
            Key::Back => return Some(BrowserAction::Close),
            _ => {}
        }
        None
    }

    fn play(&mut self, i: usize) -> Option<BrowserAction> {
        let e = self.entries.get(i)?;
        if !self.playable(e) {
            self.message = Some("This replay was recorded on other game data or another version: it cannot play here.".into());
            return None;
        }
        Some(BrowserAction::Play(Box::new(e.clone())))
    }

    fn toggle_keep(&mut self, i: usize) {
        if let Some(e) = self.entries.get_mut(i) {
            let kept = !e.file.kept;
            match replay::set_kept(&e.id, kept) {
                Ok(()) => e.file.kept = kept,
                Err(err) => self.message = Some(format!("Could not change it: {err}")),
            }
        }
    }

    fn delete(&mut self, i: usize) {
        let Some(e) = self.entries.get(i) else { return };
        if self.confirm_delete.as_deref() == Some(&e.id) {
            replay::remove(&e.id);
            self.entries.remove(i);
            self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
            self.confirm_delete = None;
            self.message = Some("Deleted.".into());
        } else {
            self.confirm_delete = Some(e.id.clone());
            self.message = Some("Press Delete again to delete this replay.".into());
        }
    }

    /// The list again (replays imported), `select` under the cursor.
    pub fn reload(&mut self, select: Option<&str>, message: String) {
        self.entries = replay::list();
        self.cursor = select.and_then(|id| self.entries.iter().position(|e| e.id == id)).unwrap_or(self.cursor.min(self.entries.len().saturating_sub(1)));
        self.message = Some(message);
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<BrowserAction> {
        let mut action = None;
        let now = replay::now();
        egui::Area::new(egui::Id::new("replays")).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            egui::Frame::new().fill(Color32::from_rgba_unmultiplied(16, 16, 22, 240)).corner_radius(14).inner_margin(18).stroke(egui::Stroke::new(1.0, Color32::from_white_alpha(35))).show(ui, |ui| {
                let width = 640.0f32.min(ctx.content_rect().width() - 40.0);
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Replays").size(24.0).strong().color(Color32::WHITE));
                    ui.label(RichText::new(format!("every match is recorded · the newest {} are kept, and any you keep", replay::KEEP)).small().color(Color32::GRAY));
                });
                ui.add_space(8.0);
                if self.entries.is_empty() {
                    ui.label(RichText::new("No replays yet. Play a match: it is recorded on its own. Or import one someone shared.").color(Color32::LIGHT_GRAY));
                }
                egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 220.0).max(160.0)).show(ui, |ui| {
                    for i in 0..self.entries.len() {
                        let e = &self.entries[i];
                        let selected = i == self.cursor;
                        let playable = self.playable(e);
                        let when = ago(now.saturating_sub(e.file.recorded));
                        let row = egui::Frame::new()
                            .fill(if selected { Color32::from_rgb(70, 28, 22) } else { Color32::TRANSPARENT })
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.set_width(width - 20.0);
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(if e.file.kept { "★" } else { " " }).color(Color32::from_rgb(255, 209, 102)));
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new(&e.file.title).color(if playable { Color32::WHITE } else { Color32::GRAY }).strong());
                                        ui.label(
                                            RichText::new(format!("{} · {} · {}{}", e.file.result, replay::duration(e.file.steps), when, if playable { "" } else { " · other game data" }))
                                                .small()
                                                .color(Color32::GRAY),
                                        );
                                    });
                                });
                            });
                        let resp = row.response.interact(egui::Sense::click());
                        if resp.clicked() {
                            self.cursor = i;
                        }
                        if resp.double_clicked() {
                            action = self.play(i);
                        }
                        if selected {
                            resp.scroll_to_me(None);
                        }
                    }
                });
                ui.add_space(10.0);
                if let Some(m) = &self.message {
                    ui.label(RichText::new(m).color(Color32::from_rgb(255, 190, 80)));
                }
                ui.horizontal(|ui| {
                    let has = !self.entries.is_empty();
                    if ui.add_enabled(has, egui::Button::new(RichText::new("▶ Play").strong())).on_hover_text("START / Enter, or double-click").clicked() {
                        action = self.play(self.cursor);
                    }
                    let kept = self.entries.get(self.cursor).is_some_and(|e| e.file.kept);
                    if ui.add_enabled(has, egui::Button::new(if kept { "★ Kept" } else { "☆ Keep" }).selected(kept)).on_hover_text("Never deleted to make room (button 1)").clicked() {
                        self.toggle_keep(self.cursor);
                    }
                    if ui.add_enabled(has, egui::Button::new("Export…")).on_hover_text("Save it as a file to share (button 3)").clicked() {
                        action = self.entries.get(self.cursor).map(|e| BrowserAction::Export(Box::new(e.clone())));
                    }
                    if ui.button("Import…").on_hover_text("Open replay files someone shared, or drop them on the window (button 4)").clicked() {
                        action = Some(BrowserAction::Import);
                    }
                    let playable = self.entries.get(self.cursor).is_some_and(|e| self.playable(e));
                    if cfg!(not(target_arch = "wasm32")) && ui.add_enabled(playable, egui::Button::new("Video…")).on_hover_text("Make it an MP4 video").clicked() {
                        action = self.entries.get(self.cursor).map(|e| BrowserAction::Video(Box::new(e.clone())));
                    }
                    if ui.add_enabled(has, egui::Button::new("Delete")).on_hover_text("Button 2, twice").clicked() {
                        self.delete(self.cursor);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").on_hover_text("BACK / Esc").clicked() {
                            action = Some(BrowserAction::Close);
                        }
                    });
                });
            });
        });
        action
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}
