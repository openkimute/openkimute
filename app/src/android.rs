//! Android (packaging/android): the game in its activity (OpenKumiteActivity.java, a NativeActivity), and the Game
//! data screen before it. The converted game data is the player's own, never in the app: they choose the folder
//! kumite_converter wrote (or a .zip of it) with the system's picker, from Downloads, an SD card or a USB drive; the
//! activity copies it into the app's files folder (`/sdcard/Android/data/io.github.openkumite/files/assets`, where
//! `adb push` puts it too), it is checked as the desktop checks it, and the game starts. The save is in the app's
//! internal folder; the log in logcat (tag `openkumite`).

use crate::debug::EguiLayer;
use crate::App;
use jni::objects::{JObject, JString, JValue};
use kumite_render::Renderer;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{NativeKeyCode, PhysicalKey};
use winit::platform::android::activity::{AndroidApp, WindowManagerFlags};
use winit::platform::android::EventLoopBuilderExtAndroid;
use winit::window::{Window, WindowId};

/// Android's back (the gesture or the button).
pub const BACK: PhysicalKey = PhysicalKey::Unidentified(NativeKeyCode::Android(4));

/// The activity's entry.
#[allow(unsafe_code)] // (no_mangle: the name android-activity calls)
#[no_mangle]
fn android_main(android: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_tag("openkumite").with_max_level(log::LevelFilter::Info));
    // (a panic the game does not catch would otherwise say nothing)
    std::panic::set_hook(Box::new(|p| log::error!("{p}")));
    android.set_window_flags(WindowManagerFlags::KEEP_SCREEN_ON | WindowManagerFlags::FULLSCREEN, WindowManagerFlags::empty());
    if let Some(dir) = android.internal_data_path() {
        std::env::set_var("OPENKUMITE_SAVE_DIR", dir);
    }
    let files = android.external_data_path().unwrap_or_default();
    // (a copy cut off when the process ended: nothing copies it now)
    let _ = std::fs::remove_dir_all(files.join("assets.part"));
    let mut root = Root { now: Now::Data(Box::new(Data::new(&files, None, None))), android: android.clone(), files };
    if kumite_assets::Assets::new(root.files.join("assets")).is_ok() {
        match game(&root.files) {
            Ok(app) => root.now = Now::Game(app),
            Err(e) => root.data_message(format!("{e:#}")),
        }
    }
    match EventLoop::builder().with_android_app(android).build() {
        Ok(event_loop) => {
            if let Err(e) = event_loop.run_app(&mut root) {
                log::error!("{e}");
            }
        }
        Err(e) => log::error!("no event loop: {e}"),
    }
    // (Android keeps the process; android-activity cannot run android_main twice in one: launched again, it panics)
    std::process::exit(0);
}

/// The game on the assets kept in the app's files folder.
fn game(files: &Path) -> anyhow::Result<Box<App>> {
    let app = crate::new_app(files.join("assets"), vec!["openkumite".into()])?;
    app.map(Box::new).ok_or_else(|| anyhow::anyhow!("the game did not start"))
}

/// The Game data button (the title, the main menu): pressed.
pub fn data_button(ctx: &egui::Context) -> bool {
    egui::Area::new(egui::Id::new("game_data_button")).anchor(egui::Align2::CENTER_TOP, [0.0, 8.0]).show(ctx, |ui| ui.button("Game data").clicked()).inner
}

struct Root {
    android: AndroidApp,
    /// The app's external files folder: assets/ (and assets.part/ while a copy goes).
    files: PathBuf,
    now: Now,
}

enum Now {
    Data(Box<Data>),
    Game(Box<App>),
}

impl Root {
    fn data_message(&mut self, m: String) {
        if let Now::Data(d) = &mut self.now {
            d.message = Some(m);
        }
    }

    /// From one screen to the other: the game asked for the Game data screen; the data copied, or the player back.
    fn switch(&mut self, event_loop: &ActiveEventLoop) {
        match &mut self.now {
            Now::Game(app) if app.game_data => {
                let suspended = app.suspended;
                let mut d = Data::new(&self.files, app.window.take(), app.renderer.take());
                d.suspended = suspended;
                self.now = Now::Data(Box::new(d));
            }
            Now::Data(d) if d.play && !d.suspended => {
                d.play = false;
                match game(&self.files) {
                    Ok(mut app) => {
                        if let (Some(w), Some(r)) = (d.window.take(), d.renderer.take()) {
                            app.start(event_loop, w, r);
                        }
                        self.now = Now::Game(app);
                    }
                    Err(e) => {
                        log::error!("{e:#}");
                        d.message = Some(format!("{e:#}"));
                    }
                }
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for Root {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        match &mut self.now {
            Now::Game(app) => app.resumed(event_loop),
            Now::Data(d) => d.resumed(event_loop),
        }
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        match &mut self.now {
            Now::Game(app) => app.suspended(event_loop),
            Now::Data(d) => {
                d.suspended = true;
                if let Some(r) = d.renderer.as_mut() {
                    let _ = r.set_surface(None, 0, 0);
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match &mut self.now {
            Now::Game(app) => app.window_event(event_loop, id, event),
            Now::Data(d) => d.window_event(event_loop, &self.android, &self.files, event),
        }
        self.switch(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match &mut self.now {
            Now::Game(app) => app.about_to_wait(event_loop),
            Now::Data(d) if d.suspended => event_loop.set_control_flow(ControlFlow::Wait),
            Now::Data(d) => {
                event_loop.set_control_flow(ControlFlow::Poll);
                if let Some(w) = &d.window {
                    w.request_redraw();
                }
            }
        }
        self.switch(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        if let Now::Game(app) = &mut self.now {
            app.exiting(event_loop);
        }
    }
}

/// The Game data screen: what is needed, the pickers, the copy's progress; with game data kept, its size, Remove and
/// back to the game.
struct Data {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    egui: Option<EguiLayer>,
    suspended: bool,
    /// What went wrong last (a folder without a conversion, no room, ...).
    message: Option<String>,
    /// The game data kept now, its bytes (None: none).
    kept: Option<u64>,
    /// Remove asked: waiting for its confirmation.
    confirm: bool,
    /// Start the game (the data copied and checked, or the player back to it).
    play: bool,
}

/// The copy as the activity tells it.
struct Import {
    /// 0 none, 1 copying, 2 done, 3 failed.
    state: i32,
    done: i64,
    total: i64,
    error: Option<String>,
}

impl Data {
    fn new(files: &Path, window: Option<Arc<Window>>, renderer: Option<Renderer>) -> Self {
        let assets = files.join("assets");
        let kept = kumite_assets::Assets::new(&assets).is_ok().then(|| size_of(&assets));
        let egui = renderer.as_ref().map(|r| EguiLayer::new(r.device(), r.overlay_format(), window.as_deref()));
        Self { window, renderer, egui, suspended: false, message: None, kept, confirm: false, play: false }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let (Some(w), Some(r)) = (self.window.clone(), self.renderer.as_mut()) {
            if std::mem::take(&mut self.suspended) {
                let size = w.inner_size();
                if let Err(e) = r.set_surface(Some(w.into()), size.width, size.height) {
                    log::error!("the window's surface could not be made again: {e:#}");
                    event_loop.exit();
                }
            }
            return;
        }
        let window = match event_loop.create_window(Window::default_attributes().with_title("OpenKumite")) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("no window: {e}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        match Renderer::new(window.clone(), size.width, size.height) {
            Ok(r) => {
                self.egui = Some(EguiLayer::new(r.device(), r.overlay_format(), Some(&window)));
                self.renderer = Some(r);
                self.window = Some(window);
            }
            Err(e) => {
                log::error!("no renderer: {e:#}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, android: &AndroidApp, files: &Path, event: WindowEvent) {
        if let (Some(e), Some(w)) = (&mut self.egui, &self.window) {
            e.on_event(w, &event);
        }
        match event {
            WindowEvent::Resized(s) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(s.width, s.height);
                }
            }
            WindowEvent::CloseRequested => event_loop.exit(),
            // back: to the game kept, else out (not while copying)
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: BACK, state: ElementState::Pressed, repeat: false, .. }, .. } => {
                if import(android).is_none_or(|i| i.state != 1) {
                    if self.kept.is_some() {
                        self.play = true;
                    } else {
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::RedrawRequested if !self.suspended => self.redraw(android, files),
            _ => {}
        }
    }

    /// The copy followed (done: checked and kept, the game next), and the screen drawn.
    fn redraw(&mut self, android: &AndroidApp, files: &Path) {
        let now = import(android);
        match &now {
            Some(i) if i.state == 2 => {
                java(android, |env, a| env.call_method(a, "importSeen", "()V", &[]).map(|_| ()));
                match keep(files) {
                    Ok(()) => {
                        log::info!("game data copied and checked");
                        self.play = true;
                    }
                    Err(e) => self.message = Some(format!("{e:#}")),
                }
            }
            Some(i) if i.state == 3 => {
                java(android, |env, a| env.call_method(a, "importSeen", "()V", &[]).map(|_| ()));
                self.message = i.error.clone();
            }
            _ => {}
        }
        let (Some(egui), Some(r)) = (self.egui.as_mut(), self.renderer.as_mut()) else { return };
        let copying = now.filter(|i| i.state == 1);
        let mut pick = None;
        let mut remove = false;
        let mut back = false;
        let mut frame = egui.run(self.window.as_deref(), r.size(), |ctx| {
            let screen = ctx.content_rect();
            ctx.layer_painter(egui::LayerId::background()).rect_filled(screen, 0.0, egui::Color32::from_rgb(12, 13, 17));
            egui::Area::new(egui::Id::new("game_data")).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.set_width((screen.width() - 48.0).min(620.0));
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.heading(egui::RichText::new("OpenKumite").size(30.0).strong());
                    ui.add_space(10.0);
                    ui.label(
                        "Play the game you own. Choose the folder kumite_converter made from your disc (the one with \
                         manifest.json): the game copies it once and keeps it.",
                    );
                    ui.add_space(6.0);
                    ui.label("1. Convert your disc with kumite_converter.\n2. Put the folder it wrote on this device: Downloads, an SD card or a USB drive (or a .zip of it).\n3. Choose it here, then play.");
                    ui.add_space(14.0);
                    let wide = |ui: &mut egui::Ui, text: &str| ui.add_sized([280.0, 44.0], egui::Button::new(text)).clicked();
                    if let Some(i) = &copying {
                        let gb = |b: i64| b as f64 / 1e9;
                        let part = if i.total > 0 { i.done as f32 / i.total as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(part).desired_width(420.0).show_percentage());
                        ui.label(format!("{:.1} GB of {:.1} GB copied", gb(i.done), gb(i.total)));
                    } else if self.confirm {
                        ui.label("Remove the game data kept in this app? You can choose the folder again later.");
                        if wide(ui, "Remove game data") {
                            remove = true;
                        }
                        if wide(ui, "Cancel") {
                            self.confirm = false;
                        }
                    } else {
                        let again = if self.kept.is_some() { "Replace with another " } else { "Choose " };
                        if wide(ui, &format!("{again}folder")) {
                            pick = Some(false);
                        }
                        if wide(ui, &format!("{again}.zip")) {
                            pick = Some(true);
                        }
                        if let Some(b) = self.kept {
                            ui.add_space(10.0);
                            ui.label(format!("{:.1} GB of game data kept in this app.", b as f64 / 1e9));
                            if wide(ui, "Back to the game") {
                                back = true;
                            }
                            if wide(ui, "Remove") {
                                self.confirm = true;
                            }
                        }
                    }
                    if let Some(m) = &self.message {
                        ui.add_space(12.0);
                        ui.colored_label(egui::Color32::from_rgb(255, 140, 120), m);
                    }
                });
            });
        });
        let cam = kumite_render::Camera::new(glam::Vec3::Z, glam::Vec3::ZERO, 1.0);
        if let Err(e) = r.render_with(&cam, &[], &[], &[], None, &[], Some(&mut frame)) {
            log::error!("render: {e:#}");
        }
        if let Some(zip) = pick {
            self.message = None;
            java(android, |env, a| env.call_method(a, "pick", "(Z)V", &[JValue::Bool(zip.into())]).map(|_| ()));
        }
        if remove {
            self.confirm = false;
            match std::fs::remove_dir_all(files.join("assets")) {
                Ok(()) => self.kept = None,
                Err(e) => self.message = Some(format!("The game data could not be removed: {e}")),
            }
        }
        self.play |= back;
    }
}

/// The copy in assets.part checked (its manifest.json and format, as the desktop's) and made the assets kept, over
/// the ones before; else removed, and why.
fn keep(files: &Path) -> anyhow::Result<()> {
    let part = files.join("assets.part");
    let kept = (|| {
        let found = crate::assetdir::conversion_in(&part)?;
        let assets = files.join("assets");
        if assets.exists() {
            std::fs::remove_dir_all(&assets)?;
        }
        std::fs::rename(&found, &assets)?;
        anyhow::Ok(())
    })();
    let _ = std::fs::remove_dir_all(&part);
    kept
}

/// The bytes of the files under `dir`.
fn size_of(dir: &Path) -> u64 {
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| if e.path().is_dir() { size_of(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) }).sum()
}

/// The copy as the activity tells it (None: it could not be asked).
fn import(android: &AndroidApp) -> Option<Import> {
    java(android, |env, a| {
        let error = env.get_field(a, "importError", "Ljava/lang/String;")?.l()?;
        let error = if error.is_null() { None } else { Some(env.get_string(&JString::from(error))?.into()) };
        Ok(Import { state: env.get_field(a, "importState", "I")?.i()?, done: env.get_field(a, "importDone", "J")?.j()?, total: env.get_field(a, "importTotal", "J")?.j()?, error })
    })
}

/// Run `f` on the activity (OpenKumiteActivity.java) from this thread; a JNI failure logged, None.
#[allow(unsafe_code)] // JNI: the VM and the activity android-activity holds
fn java<T>(android: &AndroidApp, f: impl FnOnce(&mut jni::JNIEnv, &JObject) -> jni::errors::Result<T>) -> Option<T> {
    // SAFETY: android-activity's pointers to the process's Java VM and to its global reference of the activity, both
    // valid while the activity runs (this event loop)
    let vm = unsafe { jni::JavaVM::from_raw(android.vm_as_ptr().cast()) }.map_err(|e| log::error!("JNI: {e}")).ok()?;
    let mut env = vm.attach_current_thread().map_err(|e| log::error!("JNI: {e}")).ok()?;
    let activity = unsafe { JObject::from_raw(android.activity_as_ptr().cast()) };
    let out = f(&mut env, &activity);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    out.map_err(|e| log::error!("JNI: {e}")).ok()
}
