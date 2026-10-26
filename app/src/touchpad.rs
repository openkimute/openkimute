//! Android: the on-screen pad on a touch screen, a DualShock to the game (gamepad_android.rs: the menus, KEY
//! CONFIGURATION, all), laid out and behaving as the browser's (web/index.html): the left 45% of the screen is the
//! stick (where the finger lands is its centre; 8 ways past a dead zone), the right 55% holds the face buttons'
//! diamond (□ △ ○ ✕ in the original pad's places, and the 1+2 and 3+4 pairs) in the margin beside the picture,
//! SELECT and START small at the top corners. Sizes are the page's CSS pixels as Android's dp; places in the
//! window's physical pixels.

use crate::controls::PadButton;
use std::collections::BTreeMap;
use winit::event::TouchPhase;

/// How a button is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Face {
    Triangle,
    Square,
    Circle,
    Cross,
    Pair(&'static str),
}

/// A round button: its centre and drawn diameter (physical pixels), the radius that takes a finger, what it holds.
#[derive(Clone, Debug)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub reach: f32,
    pub buttons: &'static [PadButton],
    pub face: Face,
}

/// SELECT or START: a small box at a top corner (left, top, right, bottom in physical pixels).
#[derive(Clone, Debug)]
pub struct Sys {
    pub rect: [f32; 4],
    pub button: PadButton,
    pub label: &'static str,
}

/// The pad on a screen of `w` x `h` physical pixels.
pub struct Layout {
    pub spots: Vec<Spot>,
    pub sys: [Sys; 2],
    /// Where the buttons' half starts (the stick's is left of it).
    pub split: f32,
}

/// The face buttons and pairs: their place in the diamond (in gaps from its centre), as web/index.html's.
const SPOTS: [(Face, &[PadButton], f32, f32); 6] = [
    (Face::Triangle, &[PadButton::North], 0.0, -1.0),
    (Face::Square, &[PadButton::West], -1.0, 0.0),
    (Face::Circle, &[PadButton::East], 1.0, 0.0),
    (Face::Cross, &[PadButton::South], 0.0, 1.0),
    (Face::Pair("1+2"), &[PadButton::West, PadButton::North], -1.15, -1.15),
    (Face::Pair("3+4"), &[PadButton::South, PadButton::East], 1.15, 1.15),
];

/// The stick's travel and dead zone (dp).
const STICK_MAX: f32 = 50.0;
const DEAD: f32 = 18.0;

/// The pad for a screen of `w` x `h` physical pixels, `dp` of them to a dp.
pub fn layout(w: f32, h: f32, dp: f32) -> Layout {
    let (cw, ch) = (w / dp, h / dp);
    // the diamond in the margin beside the 4:3 picture when there is one wide enough, else as the screen allows
    let margin = (cw - ch * 4.0 / 3.0) / 2.0;
    let fit = if margin > 140.0 { (margin - 16.0) / 2.7 } else { ch * 0.17 };
    let gap = fit.min(88.0).min(ch * 0.2).max(50.0);
    let size = gap * 0.92;
    let (cx, cy) = (cw - gap * 1.6 - 12.0, ch - gap * 1.6 - 12.0);
    let spots = SPOTS
        .iter()
        .map(|&(face, buttons, dx, dy)| {
            let s = if matches!(face, Face::Pair(_)) { size * 0.72 } else { size };
            Spot { x: (cx + dx * gap) * dp, y: (cy + dy * gap) * dp, size: s * dp, reach: s * 0.62 * dp, buttons, face }
        })
        .collect();
    let sys = |left: f32, button, label| Sys { rect: [left * dp, 8.0 * dp, (left + 72.0) * dp, 36.0 * dp], button, label };
    Layout { spots, sys: [sys(10.0, PadButton::Select, "SELECT"), sys(cw - 82.0, PadButton::Start, "START")], split: w * 0.45 }
}

impl Layout {
    /// The buttons under a finger at (`x`, `y`): a pair, or two faces when between them.
    pub fn under(&self, x: f32, y: f32) -> Vec<PadButton> {
        let mut out: Vec<PadButton> = Vec::new();
        for s in self.spots.iter().filter(|s| (x - s.x).hypot(y - s.y) < s.reach) {
            add(&mut out, s.buttons);
        }
        out
    }

    fn sys_at(&self, x: f32, y: f32) -> Option<PadButton> {
        self.sys.iter().find(|s| (s.rect[0]..s.rect[2]).contains(&x) && (s.rect[1]..s.rect[3]).contains(&y)).map(|s| s.button)
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

/// The stick pushed by (`dx`, `dy`) physical pixels: 8 ways past the dead zone (y down).
pub fn stick(dx: f32, dy: f32, dp: f32) -> &'static [PadButton] {
    use PadButton::{Down, Left, Right, Up};
    if dx.hypot(dy) <= DEAD * dp {
        return &[];
    }
    match (dy.atan2(dx) / std::f32::consts::FRAC_PI_4).round() as i32 {
        0 => &[Right],
        1 => &[Right, Down],
        2 => &[Down],
        3 => &[Down, Left],
        -3 => &[Left, Up],
        -2 => &[Up],
        -1 => &[Up, Right],
        _ => &[Left],
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
    Buttons(Vec<PadButton>),
    Sys(PadButton),
}

/// The fingers on the pad, and whether it is shown (a gamepad in use hides it; the next touch brings it back).
#[derive(Clone, Debug)]
pub struct TouchPad {
    fingers: BTreeMap<u64, Finger>,
    pub shown: bool,
}

impl Default for TouchPad {
    fn default() -> Self {
        Self { fingers: BTreeMap::new(), shown: true }
    }
}

impl TouchPad {
    /// Finger `id` landed, moved or lifted at (`x`, `y`) on a `w` x `h` screen: true when it is the pad's.
    #[allow(clippy::too_many_arguments)] // one touch, and the screen it is on
    pub fn touch(&mut self, id: u64, phase: TouchPhase, x: f32, y: f32, w: f32, h: f32, dp: f32) -> bool {
        let l = layout(w, h, dp);
        match phase {
            TouchPhase::Started => {
                self.shown = true;
                let f = match l.sys_at(x, y) {
                    Some(b) => Finger::Sys(b),
                    None if x < l.split => Finger::Stick { x0: x, y0: y, x, y },
                    None => Finger::Buttons(l.under(x, y)),
                };
                self.fingers.insert(id, f);
                true
            }
            TouchPhase::Moved => match self.fingers.get_mut(&id) {
                Some(Finger::Stick { x: fx, y: fy, .. }) => {
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
    }

    /// The buttons the fingers hold.
    pub fn held(&self, dp: f32) -> Vec<PadButton> {
        let mut out: Vec<PadButton> = Vec::new();
        for f in self.fingers.values() {
            let held: &[PadButton] = match f {
                Finger::Stick { x0, y0, x, y } => stick(x - x0, y - y0, dp),
                Finger::Buttons(b) => b,
                Finger::Sys(b) => std::slice::from_ref(b),
            };
            add(&mut out, held);
        }
        out
    }

    /// Draw it over the game (semi-transparent), on a `w` x `h` screen.
    pub fn draw(&self, ctx: &egui::Context, w: f32, h: f32) {
        use egui::{Color32, Stroke};
        let dp = ctx.pixels_per_point();
        let l = layout(w, h, dp);
        let held = self.held(dp);
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("touch_pad")));
        let at = |x: f32, y: f32| egui::pos2(x / dp, y / dp);
        let rgb = |c: u32| Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
        let fill = |on: bool| if on { Color32::from_white_alpha(115) } else { Color32::from_black_alpha(90) };
        for s in &l.spots {
            let (c, r) = (at(s.x, s.y), s.size / 2.0 / dp);
            let on = s.buttons.iter().all(|b| held.contains(b));
            let colour = match s.face {
                Face::Triangle => rgb(0x5fdc8b),
                Face::Square => rgb(0xf08cd8),
                Face::Circle => rgb(0xff7a6b),
                Face::Cross => rgb(0x86a8ff),
                Face::Pair(_) => Color32::from_white_alpha(217),
            };
            p.circle(c, r, fill(on), Stroke::new(2.0, colour));
            let (g, line) = (r * 0.42, Stroke::new(3.0, colour));
            match s.face {
                Face::Triangle => {
                    p.add(egui::Shape::closed_line(vec![c + egui::vec2(0.0, -g), c + egui::vec2(g * 0.87, g * 0.5), c + egui::vec2(-g * 0.87, g * 0.5)], line));
                }
                Face::Square => {
                    p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(g * 1.5, g * 1.5)), 0.0, line, egui::StrokeKind::Middle);
                }
                Face::Circle => {
                    p.circle_stroke(c, g * 0.85, line);
                }
                Face::Cross => {
                    p.line_segment([c + egui::vec2(-g * 0.75, -g * 0.75), c + egui::vec2(g * 0.75, g * 0.75)], line);
                    p.line_segment([c + egui::vec2(g * 0.75, -g * 0.75), c + egui::vec2(-g * 0.75, g * 0.75)], line);
                }
                Face::Pair(t) => {
                    p.text(c, egui::Align2::CENTER_CENTER, t, egui::FontId::proportional(13.0), colour);
                }
            }
        }
        for s in &l.sys {
            let rect = egui::Rect::from_min_max(at(s.rect[0], s.rect[1]), at(s.rect[2], s.rect[3]));
            let on = held.contains(&s.button);
            p.rect(rect, 14.0, if on { Color32::from_white_alpha(115) } else { Color32::from_black_alpha(115) }, Stroke::new(1.0, Color32::from_white_alpha(102)), egui::StrokeKind::Inside);
            p.text(rect.center(), egui::Align2::CENTER_CENTER, s.label, egui::FontId::proportional(12.0), Color32::from_gray(221));
        }
        // the stick under its finger: the ring where it landed, the knob as far as it goes
        for f in self.fingers.values() {
            if let Finger::Stick { x0, y0, x, y } = *f {
                let (dx, dy) = (x - x0, y - y0);
                let k = (STICK_MAX * dp / dx.hypot(dy).max(1.0)).min(1.0);
                p.circle(at(x0, y0), 65.0, Color32::from_white_alpha(20), Stroke::new(2.0, Color32::from_white_alpha(90)));
                p.circle_filled(at(x0 + dx * k, y0 + dy * k), 28.0, Color32::from_white_alpha(90));
            }
        }
    }
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
    fn fingers_hold_a_direction_and_buttons_at_once() {
        // a 2400 x 1080 phone at 2.625 pixels to the dp
        let (w, h, dp) = (2400.0, 1080.0, 2.625);
        let l = layout(w, h, dp);
        let spot = |f: Face| l.spots.iter().find(|s| s.face == f).unwrap().clone();
        let (square, cross, pair) = (spot(Face::Square), spot(Face::Cross), spot(Face::Pair("3+4")));
        // all on the screen's right half, clear of its edges
        for s in &l.spots {
            assert!(s.x > l.split && s.x + s.size / 2.0 < w && s.y + s.size / 2.0 < h && s.y > h / 2.0, "{s:?}");
        }
        assert_eq!(l.under(square.x, square.y), [West]);
        assert_eq!(l.under(pair.x, pair.y), [South, East]);
        assert!(l.under(w - 2.0, 2.0 * dp + 100.0).is_empty());
        let mut pad = TouchPad::default();
        // the stick held forward (right), then square, then the finger slides onto cross
        assert!(pad.touch(1, TouchPhase::Started, 300.0, 700.0, w, h, dp));
        assert!(pad.touch(1, TouchPhase::Moved, 400.0, 700.0, w, h, dp));
        assert!(pad.touch(2, TouchPhase::Started, square.x, square.y, w, h, dp));
        assert_eq!(pad.held(dp), [Right, West]);
        pad.touch(2, TouchPhase::Moved, cross.x, cross.y, w, h, dp);
        assert_eq!(pad.held(dp), [Right, South]);
        // diagonal: up-right
        pad.touch(1, TouchPhase::Moved, 400.0, 600.0, w, h, dp);
        assert_eq!(pad.held(dp), [Up, Right, South]);
        assert!(pad.touch(2, TouchPhase::Ended, cross.x, cross.y, w, h, dp));
        assert!(!pad.touch(9, TouchPhase::Moved, 1.0, 1.0, w, h, dp), "not a finger of the pad");
        // START at the top right
        let start = &l.sys[1];
        pad.touch(3, TouchPhase::Started, start.rect[0] + 5.0, start.rect[1] + 5.0, w, h, dp);
        assert_eq!(pad.held(dp), [Up, Right, Start]);
    }
}
