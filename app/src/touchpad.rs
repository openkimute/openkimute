//! Android: the on-screen pad on a touch screen, a DualShock to the game (gamepad_android.rs: the menus, KEY
//! CONFIGURATION, all), laid out and behaving as the browser's (web/index.html) by default: the face buttons'
//! diamond (□ △ ○ ✕ in the original pad's places, and the 1+2 and 3+4 pairs) in the margin beside the picture on the
//! right, SELECT and START small at the top corners. On the left a fixed 8-way D-pad (the default: a thumb slides
//! across it without lifting, the diagonals' zones wider), or the browser's stick (the left 45% of the screen: where
//! the finger lands is its centre; 8 ways past a dead zone). The player chooses which, and moves and sizes every
//! control ([`Edit`], saved per device: [`Settings`]). Sizes are the page's CSS pixels as Android's dp; places in the
//! window's physical pixels.

use crate::controls::PadButton;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use winit::event::TouchPhase;

/// A control the player can place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    Dpad,
    /// The stick's area (where a finger landing starts it).
    Stick,
    Triangle,
    Square,
    Circle,
    Cross,
    Pair12,
    Pair34,
    Select,
    Start,
}

impl Control {
    /// Drawn last to first, hit first to last: the small ones over the large ones.
    const ALL: [Control; 10] = [Control::Select, Control::Start, Control::Triangle, Control::Square, Control::Circle, Control::Cross, Control::Pair12, Control::Pair34, Control::Dpad, Control::Stick];

    /// The buttons it holds (the D-pad's and the stick's: by where the finger is).
    pub fn buttons(self) -> &'static [PadButton] {
        use PadButton::*;
        match self {
            Control::Triangle => &[North],
            Control::Square => &[West],
            Control::Circle => &[East],
            Control::Cross => &[South],
            Control::Pair12 => &[West, North],
            Control::Pair34 => &[South, East],
            Control::Select => &[Select],
            Control::Start => &[Start],
            Control::Dpad | Control::Stick => &[],
        }
    }

    fn round(self) -> bool {
        !matches!(self, Control::Select | Control::Start | Control::Stick)
    }

    pub fn name(self) -> &'static str {
        match self {
            Control::Dpad => "D-pad",
            Control::Stick => "stick area",
            // (words: egui's font has not the glyphs)
            Control::Triangle => "triangle",
            Control::Square => "square",
            Control::Circle => "circle",
            Control::Cross => "cross",
            Control::Pair12 => "1+2",
            Control::Pair34 => "3+4",
            Control::Select => "SELECT",
            Control::Start => "START",
        }
    }
}

/// The player's touch controls, kept per device (`touch_controls.json` in the save folder; not in replays).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The stick instead of the D-pad.
    pub stick: bool,
    /// Every control's size, times the default.
    pub size: f32,
    /// The pad's opacity, times the default (0.2..1).
    pub opacity: f32,
    /// The controls the player moved or sized.
    pub placed: Vec<Placed>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { stick: false, size: 1.0, opacity: 1.0, placed: Vec::new() }
    }
}

/// A control the player placed: its centre as a fraction of the screen's width and height, its size times the
/// default (and the global size).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placed {
    pub control: Control,
    pub x: f32,
    pub y: f32,
    pub size: f32,
}

impl Settings {
    /// Read from a file's text: what does not read is the default.
    pub fn from_json(text: &str) -> Self {
        let s: Settings = serde_json::from_str(text).unwrap_or_default();
        let finite = |v: f32, d: f32| if v.is_finite() { v } else { d };
        Settings {
            size: finite(s.size, 1.0).clamp(0.5, 2.0),
            opacity: finite(s.opacity, 1.0).clamp(0.2, 1.0),
            placed: s.placed.into_iter().filter(|p| p.x.is_finite() && p.y.is_finite() && p.size.is_finite()).map(|p| Placed { size: p.size.clamp(0.5, 2.0), ..p }).collect(),
            ..s
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    fn placed(&self, c: Control) -> Option<&Placed> {
        self.placed.iter().find(|p| p.control == c)
    }

    /// Control `c` placed with its centre at (`x`, `y`) of a `w` x `h` screen (its size kept).
    fn place(&mut self, c: Control, x: f32, y: f32, w: f32, h: f32) {
        let size = self.placed(c).map_or(1.0, |p| p.size);
        self.placed.retain(|p| p.control != c);
        self.placed.push(Placed { control: c, x: x / w, y: y / h, size });
    }
}

/// A control as laid out: its centre and half width and height (a round one: its radius), physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item {
    pub control: Control,
    pub x: f32,
    pub y: f32,
    pub hw: f32,
    pub hh: f32,
}

impl Item {
    /// Whether a finger at (`x`, `y`) is on it (a round button a little beyond its edge, as the browser's).
    fn takes(&self, x: f32, y: f32) -> bool {
        let reach = match self.control {
            Control::Dpad => 1.15,
            c if c.round() => 1.24,
            _ => 1.0,
        };
        if self.control.round() {
            (x - self.x).hypot(y - self.y) < self.hw * reach
        } else {
            (x - self.x).abs() <= self.hw && (y - self.y).abs() <= self.hh
        }
    }
}

/// The pad on a screen of `w` x `h` physical pixels, `dp` of them to a dp.
pub struct Layout {
    pub items: Vec<Item>,
}

/// A control's default centre and half width and height in dp on a `cw` x `ch` dp screen (web/index.html's diamond,
/// the D-pad its mirror).
fn default_place(c: Control, cw: f32, ch: f32) -> (f32, f32, f32, f32) {
    // the diamond in the margin beside the 4:3 picture when there is one wide enough, else as the screen allows
    let margin = (cw - ch * 4.0 / 3.0) / 2.0;
    let fit = if margin > 140.0 { (margin - 16.0) / 2.7 } else { ch * 0.17 };
    let gap = fit.min(88.0).min(ch * 0.2).max(50.0);
    let r = gap * 0.46;
    let (cx, cy) = (cw - gap * 1.6 - 12.0, ch - gap * 1.6 - 12.0);
    let face = |dx: f32, dy: f32, r: f32| (cx + dx * gap, cy + dy * gap, r, r);
    match c {
        Control::Triangle => face(0.0, -1.0, r),
        Control::Square => face(-1.0, 0.0, r),
        Control::Circle => face(1.0, 0.0, r),
        Control::Cross => face(0.0, 1.0, r),
        Control::Pair12 => face(-1.15, -1.15, r * 0.72),
        Control::Pair34 => face(1.15, 1.15, r * 0.72),
        Control::Select => (46.0, 22.0, 36.0, 14.0),
        Control::Start => (cw - 46.0, 22.0, 36.0, 14.0),
        Control::Dpad => (12.0 + gap * 1.6, cy, gap * 1.4, gap * 1.4),
        Control::Stick => (cw * 0.225, ch / 2.0, cw * 0.225, ch / 2.0),
    }
}

/// A centre (`x`, `y`) moved so that what spans `hw` x `hh` around it stays on a `w` x `h` screen (centred when it
/// cannot).
pub fn clamp(x: f32, y: f32, hw: f32, hh: f32, w: f32, h: f32) -> (f32, f32) {
    let fit = |v: f32, half: f32, side: f32| if side >= half * 2.0 { v.clamp(half, side - half) } else { side / 2.0 };
    (fit(x, hw, w), fit(y, hh, h))
}

/// The pad for `settings` on a screen of `w` x `h` physical pixels, `dp` of them to a dp. (The window is the safe
/// area: the activity is not laid out into a display cutout.)
pub fn layout(w: f32, h: f32, dp: f32, settings: &Settings) -> Layout {
    let (cw, ch) = (w / dp, h / dp);
    let items = Control::ALL
        .iter()
        .filter(|&&c| c != if settings.stick { Control::Dpad } else { Control::Stick })
        .map(|&c| {
            let (x, y, hw, hh) = default_place(c, cw, ch);
            // (the global size: the buttons and the D-pad)
            let global = if matches!(c, Control::Stick | Control::Select | Control::Start) { 1.0 } else { settings.size };
            let p = settings.placed(c);
            let k = global * p.map_or(1.0, |p| p.size);
            let (hw, hh) = (hw * dp * k, hh * dp * k);
            let (x, y) = p.map_or((x * dp, y * dp), |p| (p.x * w, p.y * h));
            let (x, y) = clamp(x, y, hw, hh, w, h);
            Item { control: c, x, y, hw, hh }
        })
        .collect();
    Layout { items }
}

impl Layout {
    fn item(&self, c: Control) -> Option<&Item> {
        self.items.iter().find(|i| i.control == c)
    }

    /// The control a finger at (`x`, `y`) lands on, the small ones first.
    pub fn at(&self, x: f32, y: f32) -> Option<&Item> {
        self.items.iter().find(|i| i.takes(x, y))
    }

    /// The face buttons under a finger at (`x`, `y`): a pair, or two faces when between them.
    pub fn under(&self, x: f32, y: f32) -> Vec<PadButton> {
        let mut out = Vec::new();
        for i in self.items.iter().filter(|i| i.control.round() && i.control != Control::Dpad && i.takes(x, y)) {
            add(&mut out, i.control.buttons());
        }
        out
    }
}

/// `more` added to `out`, each once.
fn add(out: &mut Vec<PadButton>, more: &[PadButton]) {
    for &b in more {
        if !out.contains(&b) {
            out.push(b);
        }
    }
}

/// The stick's travel and dead zone (dp).
const STICK_MAX: f32 = 50.0;
const DEAD: f32 = 18.0;

/// The stick pushed by (`dx`, `dy`) physical pixels: 8 ways past the dead zone (y down).
pub fn stick(dx: f32, dy: f32, dp: f32) -> &'static [PadButton] {
    if dx.hypot(dy) <= DEAD * dp {
        return &[];
    }
    way((dy.atan2(dx) / std::f32::consts::FRAC_PI_4).round() as i32)
}

/// The D-pad (radius `r`) pressed (`dx`, `dy`) from its centre: nothing in its dead centre (a fifth of it), else 8 ways,
/// the diagonals' zones wider than the straight ones' (55° and 35°: down-forward, a fighting game's, is easy to
/// hold). A finger is the D-pad's wherever it slides to.
pub fn dpad(dx: f32, dy: f32, r: f32) -> &'static [PadButton] {
    if dx.hypot(dy) < r * 0.2 {
        return &[];
    }
    let deg = dy.atan2(dx).to_degrees().rem_euclid(360.0);
    // the nearest straight way, unless more than 17.5° off it: then the diagonal between
    let straight = (deg / 90.0).round() * 90.0;
    let off = deg - straight;
    let octant = if off.abs() <= 17.5 { straight / 45.0 } else { (straight + 45.0 * off.signum()) / 45.0 };
    way((octant.round() as i32).rem_euclid(8))
}

/// The 8 ways by octant from the right, clockwise (y down).
fn way(octant: i32) -> &'static [PadButton] {
    use PadButton::{Down, Left, Right, Up};
    match octant.rem_euclid(8) {
        0 => &[Right],
        1 => &[Right, Down],
        2 => &[Down],
        3 => &[Down, Left],
        4 => &[Left],
        5 => &[Left, Up],
        6 => &[Up],
        _ => &[Up, Right],
    }
}

/// A finger on the pad.
#[derive(Clone, Debug)]
enum Finger {
    /// The stick: where it landed, where it is.
    Stick {
        x0: f32,
        y0: f32,
        x: f32,
        y: f32,
    },
    /// The D-pad: where the finger is.
    Dpad {
        x: f32,
        y: f32,
    },
    Buttons(Vec<PadButton>),
    Sys(PadButton),
}

/// The layout editor: each finger's control and its offset from it; the control chosen (its size is the one set).
#[derive(Clone, Debug, Default)]
pub struct Edit {
    grabs: BTreeMap<u64, (Control, f32, f32)>,
    pub chosen: Option<Control>,
}

/// The fingers on the pad, the player's settings, whether it is shown (a gamepad in use hides it; the next touch
/// brings it back), and the editor when open.
#[derive(Clone, Debug)]
pub struct TouchPad {
    fingers: BTreeMap<u64, Finger>,
    pub settings: Settings,
    pub shown: bool,
    pub edit: Option<Edit>,
}

impl Default for TouchPad {
    fn default() -> Self {
        Self { fingers: BTreeMap::new(), settings: Settings::default(), shown: true, edit: None }
    }
}

impl TouchPad {
    /// Finger `id` landed, moved or lifted at (`x`, `y`) on a `w` x `h` screen: true when it is the pad's (the
    /// editor's: every finger).
    #[allow(clippy::too_many_arguments)] // one touch, and the screen it is on
    pub fn touch(&mut self, id: u64, phase: TouchPhase, x: f32, y: f32, w: f32, h: f32, dp: f32) -> bool {
        let l = layout(w, h, dp, &self.settings);
        // (the editor: the fingers that grab a control; the others are the panel's)
        if let Some(e) = self.edit.as_mut() {
            return match phase {
                TouchPhase::Started => match l.at(x, y) {
                    Some(i) => {
                        e.grabs.insert(id, (i.control, i.x - x, i.y - y));
                        e.chosen = Some(i.control);
                        true
                    }
                    None => false,
                },
                TouchPhase::Moved => match (e.grabs.get(&id).copied(), e.grabs.get(&id).and_then(|g| l.item(g.0))) {
                    (Some((c, ox, oy)), Some(i)) => {
                        let (nx, ny) = clamp(x + ox, y + oy, i.hw, i.hh, w, h);
                        self.settings.place(c, nx, ny, w, h);
                        true
                    }
                    _ => false,
                },
                TouchPhase::Ended | TouchPhase::Cancelled => e.grabs.remove(&id).is_some(),
            };
        }
        match phase {
            TouchPhase::Started => {
                self.shown = true;
                let f = match l.at(x, y) {
                    Some(i) if matches!(i.control, Control::Select | Control::Start) => Finger::Sys(i.control.buttons()[0]),
                    Some(i) if i.control == Control::Dpad => Finger::Dpad { x, y },
                    Some(i) if i.control == Control::Stick => Finger::Stick { x0: x, y0: y, x, y },
                    _ => Finger::Buttons(l.under(x, y)),
                };
                self.fingers.insert(id, f);
                true
            }
            TouchPhase::Moved => match self.fingers.get_mut(&id) {
                Some(Finger::Stick { x: fx, y: fy, .. } | Finger::Dpad { x: fx, y: fy }) => {
                    (*fx, *fy) = (x, y);
                    true
                }
                Some(Finger::Buttons(b)) => {
                    *b = l.under(x, y);
                    true
                }
                Some(Finger::Sys(_)) => true,
                None => false,
            },
            TouchPhase::Ended | TouchPhase::Cancelled => self.fingers.remove(&id).is_some(),
        }
    }

    /// Every finger off.
    pub fn lift(&mut self) {
        self.fingers.clear();
        if let Some(e) = self.edit.as_mut() {
            e.grabs.clear();
        }
    }

    /// The buttons the fingers hold on a `w` x `h` screen.
    pub fn held(&self, w: f32, h: f32, dp: f32) -> Vec<PadButton> {
        let mut out = Vec::new();
        let pad = layout(w, h, dp, &self.settings).item(Control::Dpad).copied();
        for f in self.fingers.values() {
            match f {
                Finger::Stick { x0, y0, x, y } => add(&mut out, stick(x - x0, y - y0, dp)),
                Finger::Dpad { x, y } => add(&mut out, pad.map_or(&[][..], |p| dpad(x - p.x, y - p.y, p.hw))),
                Finger::Buttons(b) => add(&mut out, b),
                Finger::Sys(b) => add(&mut out, std::slice::from_ref(b)),
            }
        }
        out
    }

    /// The control chosen in the editor resized to `size` times its default.
    pub fn size_chosen(&mut self, size: f32, w: f32, h: f32, dp: f32) {
        let Some(c) = self.edit.as_ref().and_then(|e| e.chosen) else { return };
        let Some(i) = layout(w, h, dp, &self.settings).item(c).copied() else { return };
        self.settings.place(c, i.x, i.y, w, h);
        if let Some(p) = self.settings.placed.iter_mut().find(|p| p.control == c) {
            p.size = size.clamp(0.5, 2.0);
        }
    }

    /// Draw it over the game (its opacity as set), on a `w` x `h` screen; in the editor, each control's outline.
    pub fn draw(&self, ctx: &egui::Context, w: f32, h: f32) {
        use egui::{Color32, Stroke};
        let dp = ctx.pixels_per_point();
        let l = layout(w, h, dp, &self.settings);
        let held = self.held(w, h, dp);
        let a = self.settings.opacity;
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("touch_pad")));
        let at = |x: f32, y: f32| egui::pos2(x / dp, y / dp);
        let rgb = |c: u32| Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8).gamma_multiply(a);
        let fill = |on: bool| (if on { Color32::from_white_alpha(115) } else { Color32::from_black_alpha(90) }).gamma_multiply(a);
        let white = |alpha: u8| Color32::from_white_alpha(alpha).gamma_multiply(a);
        for i in l.items.iter().rev() {
            let c = at(i.x, i.y);
            let (r, line) = (i.hw / dp, |col: Color32| Stroke::new(3.0, col));
            match i.control {
                Control::Stick => {}
                Control::Dpad => {
                    // a cross, the arms held lit; its circle the diagonals' reach
                    let on = |b: PadButton| held.contains(&b);
                    p.circle(c, r, Color32::from_black_alpha(40).gamma_multiply(a), Stroke::new(1.5, white(70)));
                    let (arm, len) = (r * 0.34, r * 0.92);
                    let rect = |dx: f32, dy: f32, ww: f32, hh: f32| egui::Rect::from_center_size(c + egui::vec2(dx, dy), egui::vec2(ww, hh));
                    p.rect(rect(0.0, 0.0, arm * 2.0, arm * 2.0), 0.0, fill(false), Stroke::NONE, egui::StrokeKind::Inside);
                    for (b, dx, dy, ww, hh) in
                        [(PadButton::Up, 0.0, -1.0, 1.0, 0.0), (PadButton::Down, 0.0, 1.0, 1.0, 0.0), (PadButton::Left, -1.0, 0.0, 0.0, 1.0), (PadButton::Right, 1.0, 0.0, 0.0, 1.0)]
                    {
                        let size = (arm * 2.0 * ww + (len - arm) * (1.0 - ww), arm * 2.0 * hh + (len - arm) * (1.0 - hh));
                        let off = (arm + len) / 2.0;
                        p.rect(rect(dx * off, dy * off, size.0, size.1), 4.0, fill(on(b)), Stroke::new(2.0, white(160)), egui::StrokeKind::Inside);
                        let tip = c + egui::vec2(dx, dy) * (len - arm * 0.45);
                        let side = egui::vec2(dy, dx) * arm * 0.45;
                        let back = c + egui::vec2(dx, dy) * (len - arm * 1.25);
                        p.add(egui::Shape::convex_polygon(vec![tip, back + side, back - side], white(200), Stroke::NONE));
                    }
                }
                Control::Select | Control::Start => {
                    let rect = egui::Rect::from_center_size(c, egui::vec2(i.hw * 2.0 / dp, i.hh * 2.0 / dp));
                    let b = i.control.buttons()[0];
                    p.rect(rect, 14.0, if held.contains(&b) { white(115) } else { Color32::from_black_alpha(115).gamma_multiply(a) }, Stroke::new(1.0, white(102)), egui::StrokeKind::Inside);
                    p.text(rect.center(), egui::Align2::CENTER_CENTER, i.control.name(), egui::FontId::proportional(12.0), white(221));
                }
                face => {
                    let colour = match face {
                        Control::Triangle => rgb(0x5fdc8b),
                        Control::Square => rgb(0xf08cd8),
                        Control::Circle => rgb(0xff7a6b),
                        Control::Cross => rgb(0x86a8ff),
                        _ => white(217),
                    };
                    p.circle(c, r, fill(face.buttons().iter().all(|b| held.contains(b))), Stroke::new(2.0, colour));
                    let g = r * 0.42;
                    match face {
                        Control::Triangle => {
                            p.add(egui::Shape::closed_line(vec![c + egui::vec2(0.0, -g), c + egui::vec2(g * 0.87, g * 0.5), c + egui::vec2(-g * 0.87, g * 0.5)], line(colour)));
                        }
                        Control::Square => {
                            p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(g * 1.5, g * 1.5)), 0.0, line(colour), egui::StrokeKind::Middle);
                        }
                        Control::Circle => {
                            p.circle_stroke(c, g * 0.85, line(colour));
                        }
                        Control::Cross => {
                            p.line_segment([c + egui::vec2(-g * 0.75, -g * 0.75), c + egui::vec2(g * 0.75, g * 0.75)], line(colour));
                            p.line_segment([c + egui::vec2(g * 0.75, -g * 0.75), c + egui::vec2(-g * 0.75, g * 0.75)], line(colour));
                        }
                        _ => {
                            p.text(c, egui::Align2::CENTER_CENTER, face.name(), egui::FontId::proportional(13.0), colour);
                        }
                    }
                }
            }
            // the editor: each control's box, the chosen one's yellow
            if let Some(e) = &self.edit {
                let chosen = e.chosen == Some(i.control);
                let rect = egui::Rect::from_center_size(c, egui::vec2(i.hw * 2.0 / dp, i.hh * 2.0 / dp));
                let colour = if chosen { Color32::from_rgb(255, 210, 60) } else { Color32::from_white_alpha(150) };
                p.rect_stroke(rect, 2.0, Stroke::new(if chosen { 3.0 } else { 1.5 }, colour), egui::StrokeKind::Outside);
                if i.control == Control::Stick {
                    p.text(c, egui::Align2::CENTER_CENTER, "STICK AREA", egui::FontId::proportional(16.0), colour);
                }
            }
        }
        // the stick under its finger: the ring where it landed, the knob as far as it goes
        for f in self.fingers.values() {
            if let Finger::Stick { x0, y0, x, y } = *f {
                let (dx, dy) = (x - x0, y - y0);
                let k = (STICK_MAX * dp / dx.hypot(dy).max(1.0)).min(1.0);
                p.circle(at(x0, y0), 65.0, white(20), Stroke::new(2.0, white(90)));
                p.circle_filled(at(x0 + dx * k, y0 + dy * k), 28.0, white(90));
            }
        }
    }
}

/// What the editor's panel asked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PanelAction {
    Done,
    Reset,
}

/// The editor's panel (top centre): D-pad or stick, the size of all and of the control chosen, the opacity, Reset
/// and Done. `size_chosen` is set when the chosen control's size slider moved.
pub fn panel(ctx: &egui::Context, pad: &mut TouchPad, chosen_size: &mut Option<f32>) -> Option<PanelAction> {
    let mut action = None;
    let chosen = pad.edit.as_ref().and_then(|e| e.chosen);
    let current = chosen.and_then(|c| pad.settings.placed(c)).map_or(1.0, |p| p.size);
    egui::Area::new(egui::Id::new("touch_controls_panel")).anchor(egui::Align2::CENTER_TOP, [0.0, 50.0]).order(egui::Order::Foreground).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_max_width(420.0);
            ui.vertical_centered(|ui| {
                ui.strong("Touch controls");
                ui.label("Drag a control to move it; choose one to size it.");
            });
            ui.horizontal(|ui| {
                ui.selectable_value(&mut pad.settings.stick, false, "D-pad");
                ui.selectable_value(&mut pad.settings.stick, true, "Stick");
            });
            ui.add(egui::Slider::new(&mut pad.settings.size, 0.5..=2.0).text("size of all"));
            if let Some(c) = chosen {
                let mut s = current;
                if ui.add(egui::Slider::new(&mut s, 0.5..=2.0).text(format!("size of {}", c.name()))).changed() {
                    *chosen_size = Some(s);
                }
            }
            ui.add(egui::Slider::new(&mut pad.settings.opacity, 0.2..=1.0).text("opacity"));
            ui.horizontal(|ui| {
                if ui.button("Reset to default").clicked() {
                    action = Some(PanelAction::Reset);
                }
                if ui.button("Done").clicked() {
                    action = Some(PanelAction::Done);
                }
            });
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use PadButton::*;

    #[test]
    fn stick_eight_ways_past_the_dead_zone() {
        assert!(stick(10.0, 10.0, 1.0).is_empty());
        assert_eq!(stick(40.0, 0.0, 1.0), [Right]);
        assert_eq!(stick(-40.0, 0.0, 1.0), [Left]);
        assert_eq!(stick(0.0, -40.0, 1.0), [Up]);
        assert_eq!(stick(30.0, 30.0, 1.0), [Right, Down]);
        assert_eq!(stick(-30.0, -30.0, 1.0), [Left, Up]);
        // (dp: a dead zone of 18 dp is 45 pixels at 2.5)
        assert!(stick(40.0, 0.0, 2.5).is_empty());
    }

    #[test]
    fn dpad_eight_ways_wide_diagonals_and_a_dead_centre() {
        let r = 100.0;
        assert!(dpad(0.0, 0.0, r).is_empty());
        assert!(dpad(10.0, -10.0, r).is_empty(), "the dead centre");
        let at = |deg: f32, d: f32| {
            let a = deg.to_radians();
            dpad(a.cos() * d, a.sin() * d, r)
        };
        for (deg, way) in [(0.0, &[Right][..]), (45.0, &[Right, Down]), (90.0, &[Down]), (135.0, &[Down, Left]), (180.0, &[Left]), (225.0, &[Left, Up]), (270.0, &[Up]), (315.0, &[Up, Right])] {
            assert_eq!(at(deg, 60.0), way, "{deg}°");
        }
        // the diagonals' zones are wider: 20° off straight is already diagonal, 15° is not
        assert_eq!(at(20.0, 60.0), [Right, Down]);
        assert_eq!(at(15.0, 60.0), [Right]);
        assert_eq!(at(-20.0, 60.0), [Up, Right]);
        // beyond its edge (a thumb slid off) still a direction
        assert_eq!(at(180.0, 300.0), [Left]);
    }

    #[test]
    fn controls_kept_on_the_screen() {
        assert_eq!(clamp(-50.0, 30.0, 40.0, 40.0, 1000.0, 500.0), (40.0, 40.0));
        assert_eq!(clamp(990.0, 499.0, 40.0, 20.0, 1000.0, 500.0), (960.0, 480.0));
        assert_eq!(clamp(500.0, 250.0, 40.0, 20.0, 1000.0, 500.0), (500.0, 250.0));
        // larger than the screen: centred
        assert_eq!(clamp(0.0, 0.0, 600.0, 20.0, 1000.0, 500.0), (500.0, 20.0));
        // a control placed off the screen (a smaller screen than it was placed on) comes back in
        let mut s = Settings::default();
        s.placed.push(Placed { control: Control::Cross, x: 1.5, y: -0.2, size: 1.0 });
        let l = layout(2400.0, 1080.0, 2.625, &s);
        for i in &l.items {
            assert!(i.x - i.hw >= 0.0 && i.x + i.hw <= 2400.0 && i.y - i.hh >= 0.0 && i.y + i.hh <= 1080.0, "{i:?}");
        }
    }

    #[test]
    fn settings_saved_and_read_back() {
        let mut s = Settings { stick: true, size: 1.3, opacity: 0.6, placed: Vec::new() };
        s.place(Control::Dpad, 300.0, 700.0, 2400.0, 1080.0);
        s.placed.push(Placed { control: Control::Start, x: 0.5, y: 0.1, size: 1.5 });
        assert_eq!(Settings::from_json(&s.to_json()), s);
        // nothing or junk: the default; out of range: clamped
        assert_eq!(Settings::from_json(""), Settings::default());
        assert_eq!(Settings::from_json("{\"size\": 9, \"opacity\": 0}"), Settings { size: 2.0, opacity: 0.2, ..Settings::default() });
    }

    #[test]
    fn fingers_hold_a_direction_and_buttons_at_once() {
        // a 2400 x 1080 phone at 2.625 pixels to the dp; the D-pad by default
        let (w, h, dp) = (2400.0, 1080.0, 2.625);
        let mut pad = TouchPad::default();
        let l = layout(w, h, dp, &pad.settings);
        let item = |c: Control| *l.item(c).unwrap();
        let (dp_, square, cross, pair) = (item(Control::Dpad), item(Control::Square), item(Control::Cross), item(Control::Pair34));
        assert!(l.item(Control::Stick).is_none(), "the D-pad is the default");
        assert!(dp_.x < w / 2.0 && square.x > w / 2.0);
        assert_eq!(l.under(square.x, square.y), [West]);
        assert_eq!(l.under(pair.x, pair.y), [South, East]);
        // forward held on the D-pad, square pressed, the finger slid to cross; the thumb slides to back, then down-back
        assert!(pad.touch(1, TouchPhase::Started, dp_.x + dp_.hw * 0.7, dp_.y, w, h, dp));
        assert!(pad.touch(2, TouchPhase::Started, square.x, square.y, w, h, dp));
        assert_eq!(pad.held(w, h, dp), [Right, West]);
        pad.touch(2, TouchPhase::Moved, cross.x, cross.y, w, h, dp);
        assert_eq!(pad.held(w, h, dp), [Right, South]);
        pad.touch(1, TouchPhase::Moved, dp_.x - dp_.hw * 0.7, dp_.y, w, h, dp);
        assert_eq!(pad.held(w, h, dp), [Left, South]);
        pad.touch(1, TouchPhase::Moved, dp_.x - dp_.hw * 0.5, dp_.y + dp_.hw * 0.5, w, h, dp);
        assert_eq!(pad.held(w, h, dp), [Down, Left, South]);
        assert!(pad.touch(2, TouchPhase::Ended, cross.x, cross.y, w, h, dp));
        assert!(!pad.touch(9, TouchPhase::Moved, 1.0, 1.0, w, h, dp), "not a finger of the pad");
        // START at the top right
        let start = item(Control::Start);
        pad.touch(3, TouchPhase::Started, start.x, start.y, w, h, dp);
        assert_eq!(pad.held(w, h, dp), [Down, Left, Start]);
        // the stick instead: the left of the screen
        pad.lift();
        pad.settings.stick = true;
        assert!(pad.touch(4, TouchPhase::Started, 300.0, 700.0, w, h, dp));
        pad.touch(4, TouchPhase::Moved, 400.0, 600.0, w, h, dp);
        assert_eq!(pad.held(w, h, dp), [Up, Right]);
    }

    #[test]
    fn the_editor_moves_and_sizes_controls() {
        let (w, h, dp) = (2400.0, 1080.0, 2.625);
        let mut pad = TouchPad { edit: Some(Edit::default()), ..TouchPad::default() };
        let cross = *layout(w, h, dp, &pad.settings).item(Control::Cross).unwrap();
        // a finger on no control (the panel's slider) is not the editor's, nor are its moves
        assert!(!pad.touch(5, TouchPhase::Started, 1200.0, 300.0, w, h, dp));
        assert!(!pad.touch(5, TouchPhase::Moved, 1300.0, 300.0, w, h, dp));
        assert!(!pad.touch(5, TouchPhase::Ended, 1300.0, 300.0, w, h, dp));
        // grabbed off its centre, dragged: it keeps the finger's offset; then off the screen: kept on it
        assert!(pad.touch(1, TouchPhase::Started, cross.x + 10.0, cross.y, w, h, dp));
        pad.touch(1, TouchPhase::Moved, 1210.0, 500.0, w, h, dp);
        let moved = *layout(w, h, dp, &pad.settings).item(Control::Cross).unwrap();
        assert_eq!((moved.x, moved.y), (1200.0, 500.0));
        pad.touch(1, TouchPhase::Moved, 5000.0, 500.0, w, h, dp);
        let edge = *layout(w, h, dp, &pad.settings).item(Control::Cross).unwrap();
        assert_eq!(edge.x, w - edge.hw);
        pad.touch(1, TouchPhase::Ended, 5000.0, 500.0, w, h, dp);
        assert!(pad.fingers.is_empty(), "the editor's fingers press nothing");
        pad.size_chosen(1.5, w, h, dp);
        let big = *layout(w, h, dp, &pad.settings).item(Control::Cross).unwrap();
        assert!((big.hw - cross.hw * 1.5).abs() < 0.01);
        // pressed where it now is (the editor closed)
        pad.edit = None;
        pad.touch(2, TouchPhase::Started, big.x, big.y, w, h, dp);
        assert_eq!(pad.held(w, h, dp), [South]);
    }
}
