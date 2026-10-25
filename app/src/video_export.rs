//! A replay made a video from inside the game (native): the options asked (size, HUD, camera), where to save it, then
//! `--export-video` in a process of its own (video.rs: the game goes on meanwhile), its progress shown, and Cancel.

use crate::debug::FreeCam;
use crate::replay::Entry;
use egui::{Align2, Color32, RichText};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use web_time::Instant;

/// The options, asked before.
pub struct Dialog {
    pub entry: Box<Entry>,
    pub size: usize,
    pub hud: bool,
    /// The viewer's free camera when the video was asked for there (None: the game's camera only).
    pub free_cam: Option<FreeCam>,
    pub use_free_cam: bool,
}

pub enum DialogAction {
    Export,
    Cancel,
}

impl Dialog {
    pub fn new(entry: Entry, hud: bool, free_cam: Option<FreeCam>) -> Self {
        Self { entry: Box::new(entry), size: 1, hud, use_free_cam: free_cam.is_some(), free_cam }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<DialogAction> {
        let mut action = None;
        egui::Area::new(egui::Id::new("video_dialog")).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::new().fill(Color32::from_rgba_unmultiplied(16, 16, 22, 245)).corner_radius(14).inner_margin(18).stroke(egui::Stroke::new(1.0, Color32::from_white_alpha(35))).show(ui, |ui| {
                ui.set_width(420.0);
                ui.label(RichText::new("Make a video").size(22.0).strong().color(Color32::WHITE));
                ui.label(RichText::new(&self.entry.file.title).color(Color32::GRAY));
                ui.add_space(10.0);
                ui.label(RichText::new("Size").strong().color(Color32::WHITE));
                ui.horizontal(|ui| {
                    for (k, (name, _, _)) in crate::video::SIZES.iter().enumerate() {
                        ui.radio_value(&mut self.size, k, *name);
                    }
                });
                ui.label(RichText::new("Larger sizes take longer: 4K several times as long as 1080p.").small().color(Color32::GRAY));
                ui.add_space(8.0);
                ui.checkbox(&mut self.hud, "Show the HUD (health, timer, names)");
                ui.add_space(4.0);
                ui.label(RichText::new("Camera").strong().color(Color32::WHITE));
                ui.radio_value(&mut self.use_free_cam, false, "The game's camera");
                ui.add_enabled_ui(self.free_cam.is_some(), |ui| {
                    ui.radio_value(&mut self.use_free_cam, true, "The free camera, where it is now").on_disabled_hover_text("Turn on the free camera in the replay first");
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Save video…").strong()).clicked() {
                        action = Some(DialogAction::Export);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(DialogAction::Cancel);
                    }
                });
            });
        });
        action
    }
}

/// How the export goes: frames done of all, then its end.
#[derive(Default)]
struct Progress {
    done: u64,
    all: u64,
    finished: Option<Result<(), String>>,
}

/// The export running in its own process.
pub struct Export {
    child: Option<Child>,
    progress: Arc<Mutex<Progress>>,
    pub out: PathBuf,
    title: String,
    started: Instant,
}

pub enum StatusAction {
    Cancel,
    Show,
    Close,
}

impl Export {
    /// Start it: this game's program again, `--export-video`, on the same game data and save folder.
    pub fn start(d: &Dialog, out: PathBuf, assets: &std::path::Path) -> anyhow::Result<Self> {
        let (_, w, h) = crate::video::SIZES[d.size.min(crate::video::SIZES.len() - 1)];
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args(["--assets"]).arg(assets).args(["--export-video", &d.entry.id, "--out"]).arg(&out).args(["--size", &format!("{w}x{h}")]);
        if !d.hud {
            cmd.arg("--no-hud");
        }
        if let (true, Some(c)) = (d.use_free_cam, d.free_cam) {
            cmd.args(["--free-cam", &format!("{},{},{},{},{},{}", c.yaw, c.pitch, c.distance, c.pan.x, c.pan.y, c.pan.z)]);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn()?;
        let progress = Arc::new(Mutex::new(Progress::default()));
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let p = progress.clone();
        std::thread::spawn(move || {
            // (its log: the last error line, if it fails)
            let last_error = Arc::new(Mutex::new(String::new()));
            if let Some(err) = stderr {
                let last = last_error.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(err).lines().map_while(Result::ok) {
                        if line.contains("ERROR") || line.starts_with("Error") {
                            *last.lock().unwrap_or_else(|e| e.into_inner()) = line.split("] ").last().unwrap_or(&line).to_string();
                        }
                    }
                });
            }
            let mut done = false;
            if let Some(out) = stdout {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    let mut g = p.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(rest) = line.strip_prefix("PROGRESS ") {
                        let mut n = rest.split(' ').filter_map(|x| x.parse().ok());
                        (g.done, g.all) = (n.next().unwrap_or(0), n.next().unwrap_or(0));
                    } else if line == "DONE" {
                        done = true;
                    }
                }
            }
            // (give its last log line a moment)
            std::thread::sleep(std::time::Duration::from_millis(200));
            let why = last_error.lock().unwrap_or_else(|e| e.into_inner()).clone();
            p.lock().unwrap_or_else(|e| e.into_inner()).finished = Some(if done {
                Ok(())
            } else if why.is_empty() {
                Err("the export stopped".into())
            } else {
                Err(why)
            });
        });
        Ok(Self { child: Some(child), progress, out, title: d.entry.file.title.clone(), started: Instant::now() })
    }

    pub fn running(&self) -> bool {
        self.progress.lock().is_ok_and(|p| p.finished.is_none())
    }

    pub fn cancel(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_file(&self.out);
    }

    /// The panel at the top left (clear of the replay's controls): how far, how long left, Cancel; when done, where it is.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<StatusAction> {
        let (done, all, finished) = {
            let p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            (p.done, p.all, p.finished.clone())
        };
        let mut action = None;
        egui::Area::new(egui::Id::new("video_export")).anchor(Align2::LEFT_TOP, [14.0, 14.0]).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::new().fill(Color32::from_black_alpha(215)).corner_radius(10).inner_margin(12).show(ui, |ui| {
                ui.set_width(330.0);
                match &finished {
                    None => {
                        ui.label(RichText::new("Making the video").strong().color(Color32::WHITE));
                        ui.label(RichText::new(&self.title).small().color(Color32::GRAY));
                        let frac = if all > 0 { done as f32 / all as f32 } else { 0.0 };
                        let secs = self.started.elapsed().as_secs_f32();
                        let left = if done > 30 && frac > 0.0 { format!(" · about {} left", crate::replay::duration((secs / frac - secs) as u64 * 60)) } else { String::new() };
                        ui.add(egui::ProgressBar::new(frac).text(format!("{:.0}%{left}", frac * 100.0)));
                        if ui.button("Cancel").clicked() {
                            action = Some(StatusAction::Cancel);
                        }
                    }
                    Some(Ok(())) => {
                        ui.label(RichText::new("Video saved").strong().color(Color32::from_rgb(120, 200, 120)));
                        ui.label(RichText::new(self.out.display().to_string()).small().color(Color32::LIGHT_GRAY));
                        ui.horizontal(|ui| {
                            if ui.button("Show in folder").clicked() {
                                action = Some(StatusAction::Show);
                            }
                            if ui.button("Close").clicked() {
                                action = Some(StatusAction::Close);
                            }
                        });
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new("The video was not made").strong().color(Color32::from_rgb(255, 150, 120)));
                        ui.label(RichText::new(e).small().color(Color32::LIGHT_GRAY));
                        if ui.button("Close").clicked() {
                            action = Some(StatusAction::Close);
                        }
                    }
                }
            });
        });
        action
    }

    /// The video in the system's file browser.
    pub fn show(&self) {
        let path = &self.out;
        #[cfg(target_os = "macos")]
        let r = Command::new("open").arg("-R").arg(path).spawn();
        #[cfg(windows)]
        let r = Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
        #[cfg(not(any(target_os = "macos", windows)))]
        let r = Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
        if let Err(e) = r {
            log::warn!("showing {}: {e}", path.display());
        }
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        // (the game closing: an unfinished video is not left half made; a finished one's process is reaped)
        if self.running() {
            self.cancel();
        } else if let Some(mut c) = self.child.take() {
            let _ = c.wait();
        }
    }
}
