//! The fight HUD, drawn like the original (EXE HUD driver 0x8003D6B0) on its 368x480 screen from
//! screens.json `hud`: the health bars with their draining trail (0x8004E274), the timer (0x8004E128), the round
//! gems (0x8004E594), the name plates (0x8004E8B4), and the round texts in the big font (ROUND n / FINAL ROUND /
//! READY? 0x8003DCC8, K.O. texts 0x8003DF68, result 0x8003E170).

use crate::art::{int as u, num as n, pair as xy, Art, Screen as View, Tex};
use egui::Color32;
use kumite_game::bout::{Announcement, Bout, Phase};
use serde_json::Value;

pub mod tag;

/// Who plays and how the fight is shown.
#[derive(Clone, Debug, Default)]
pub struct HudInfo {
    /// Model record of each fighter (its name picture).
    pub models: [usize; 2],
    /// The game's costume number (0..3) of each fighter: the name colours.
    pub costumes: [usize; 2],
    /// Display names for "<NAME> WINS!".
    pub names: [String; 2],
    pub humans: [bool; 2],
    /// The title's demo: GAME OVER over the whole fight.
    pub demo: bool,
    /// Practice: no bars, timer or names (the HUD flag 0x800AE200 is off).
    pub hidden: bool,
    /// Arcade: the stage (1..10), shown with the run's time over P1's bar.
    pub stage: Option<u32>,
    /// What each side shows over its bar when it is not the arcade's (0x8004EB54's kinds per battle type).
    pub under: [Under; 2],
    /// Time Attack: the frames already counted before this match (cleared stages and lost attempts).
    pub time_base: Option<u32>,
    /// Team Battle / Survival: the text shown for the whole intro instead of the round.
    pub intro: Option<(String, usize, u32, f32, f32)>,
    /// Team Battle: per side the team's size, the members defeated, and whether the last one lost just now (its
    /// icon blinks).
    pub team: Option<[(usize, usize, bool); 2]>,
    /// Force: no result text after the K.O. (STAGE CLEAR!! and the tally instead).
    pub no_result: bool,
}

/// An under-bar text kind (0x8004EB54).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Under {
    #[default]
    None,
    /// Blinking PUSH Pn START.
    Push,
    /// GAME OVER (colour, x offset).
    GameOver(String, u32, f32),
    /// A mode label (TEAM BATTLE / TIME ATTACK / SURVIVAL), colour 6.
    Label(String, u32),
    /// Time Attack: STAGE n and the run's time.
    Stage(u32),
    /// Survival: the rank and the wins (rank text, colour, wins text, colour, x offset).
    Survival(String, u32, String, u32, f32),
}

/// A big-font text: string, font, colour, x, y.
#[derive(Clone, Debug, Default)]
struct Text {
    text: String,
    font: usize,
    colour: u32,
    x: f32,
    y: f32,
}

impl Text {
    fn from(v: &Value) -> Self {
        Self {
            text: v["text"].as_str().unwrap_or_default().to_string(),
            font: v["font"].as_u64().unwrap_or(0) as usize,
            colour: v["colour"].as_u64().unwrap_or(0) as u32,
            x: v["x"].as_f64().unwrap_or(0.0) as f32,
            y: v["y"].as_f64().unwrap_or(0.0) as f32,
        }
    }
    fn draw(&self, painter: &egui::Painter, v: &View, art: &Art) {
        art.text(painter, v, self.font, self.colour, self.x, self.y, &self.text, Color32::WHITE);
    }
}

/// The HUD tables (screens.json `hud`).
#[derive(Clone, Debug, Default)]
pub struct HudData {
    bar_xy: [[f32; 2]; 2],
    bar_page: [u32; 2],
    cap_uv: [[u32; 2]; 2],
    strip_uv: [u32; 2],
    bar_cluts: [[u32; 2]; 3],
    timer_page: [u32; 2],
    timer_uv: Vec<[u32; 2]>,
    timer_clut: [u32; 2],
    timer_x: [f32; 2],
    timer_y: f32,
    gem_uv: [u32; 2],
    gem_cluts: [[u32; 2]; 2],
    gem_x: [f32; 2],
    gem_step: [f32; 2],
    gem_y: f32,
    name_y: f32,
    name_x: [f32; 2],
    name_cluts: Vec<[u32; 2]>,
    plate_cluts: Vec<[u32; 2]>,
    plate_uv: [u32; 2],
    round: Text,
    final_round: Text,
    ready: Text,
    round_from: [u32; 2],
    ready_from: [u32; 2],
    time_up: Text,
    perfect: Text,
    double_ko: Text,
    ko: Text,
    draw: Text,
    wins: Text,
    you_win: Text,
    you_lose: Text,
    demo: Text,
    replay: Text,
    pub pause_items: Vec<String>,
    /// Ball's (battle type 7): one more, RULES.
    pub pause_ball_items: Vec<String>,
    under_y: f32,
    under_x: [f32; 2],
    /// Force's SCORE and HI-SCORE colours (at under_x).
    score_colours: [u32; 2],
    under_centre: [f32; 2],
    pub push: Vec<String>,
    push_half: [f32; 2],
    push_colour: u32,
    stage_colour: u32,
    final_from: u32,
    /// "STAGE%2d" and " FINAL" (record 5).
    stage_format: String,
    stage_final: String,
    time_colour: u32,
    time_dx: [f32; 3],
    marks_dx: [f32; 2],
    marks_dy: f32,
    /// Names of the extra fighters by model record (they have no name picture).
    pub extra_names: std::collections::HashMap<usize, String>,
    /// The bars' width and height, the caps' width and places (x + dx), where P1 / P2 fill from (x + dx).
    bar_w: i32,
    bar_h: f32,
    cap_w: u32,
    cap_dx: [f32; 2],
    fill_dx: [f32; 2],
    /// The timer's glyphs for the clock off (infinity), a digit's size.
    timer_infinity: [usize; 2],
    timer_size: [u32; 2],
    gem_size: [u32; 2],
    /// The name plate: height, its y under the name, the margin either side.
    plate: [f32; 3],
    /// "<NAME> WINS!" centred: x = centre - (per_char * len + offset).
    wins_x: [f32; 3],
    /// Team Battle's members (screens.json `hud.team`).
    team: TeamIcons,
    /// Ball's charge gauges and floating texts (ball.json).
    ball: BallLayout,
}

/// Team Battle's member icons (record 5 0x800B1764): per side x, step, the icon (place, size, CLUTs fighting /
/// down), the slash over one down (place, size, y).
#[derive(Clone, Debug, Default)]
struct TeamIcons {
    x: [f32; 2],
    step: [f32; 2],
    y: f32,
    icon: Tex,
    size: [u32; 2],
    cluts: [[u32; 2]; 2],
    slash: Tex,
    slash_size: [u32; 2],
    slash_y: f32,
}

/// Ball's gauges (record 8 0x800B49A8) and floating texts (0x800B3F98).
#[derive(Clone, Debug, Default)]
struct BallLayout {
    x: [f32; 2],
    y: f32,
    w: i32,
    full: i32,
    grow: i32,
    caps: [Tex; 2],
    cap_size: [u32; 2],
    right_dx: f32,
    strip: Tex,
    strip_h: f32,
    from_dx: [f32; 2],
    /// Fill (P1, P2), trail, empty.
    cluts: [[u32; 2]; 4],
    text_font: u32,
    text_frames: u32,
    text_dy: f32,
    text_dx: f32,
    text_max: usize,
}

impl HudData {
    /// The under-bar texts' places (x per side, y) and Force's SCORE / HI-SCORE colours there.
    pub fn under_place(&self) -> ([f32; 2], f32, [u32; 2]) {
        (self.under_x, self.under_y, self.score_colours)
    }

    /// The health bars' width (px).
    pub fn bar_w(&self) -> i32 {
        self.bar_w
    }

    pub fn load(screens: &Value, ball: &Value) -> Self {
        let h = &screens["hud"];
        let tm = &h["team"];
        let (bg, bt) = (&ball["gauges"], &ball["texts"]);
        let b = &h["bars"];
        let bpage = xy(&b["page"]);
        let t = &h["timer"];
        let m = &h["markers"];
        let nm = &h["names"];
        let uv = |v: &Value| [v["u"].as_u64().unwrap_or(0) as u32, v["v"].as_u64().unwrap_or(0) as u32];
        let pl = |k: usize| [n(&b["players"][k]["x"]), n(&b["players"][k]["y"])];
        let both = |v: &Value| [n(&v[0]), n(&v[1])];
        Self {
            bar_xy: [pl(0), pl(1)],
            bar_page: xy(&b["page"]),
            cap_uv: [uv(&b["left_cap"]), uv(&b["right_cap"])],
            strip_uv: uv(&b["strip"]),
            bar_cluts: [xy(&b["cluts"]["fill"]), xy(&b["cluts"]["trail"]), xy(&b["cluts"]["empty"])],
            timer_page: xy(&t["page"]),
            timer_uv: t["uv"].as_array().into_iter().flatten().map(xy).collect(),
            timer_clut: xy(&t["clut"]),
            timer_x: both(&t["digits_x"]),
            timer_y: n(&t["y"]),
            gem_uv: uv(m),
            gem_cluts: [xy(&m["won_clut"]), xy(&m["not_won_clut"])],
            gem_x: both(&m["x"]),
            gem_step: both(&m["step"]),
            gem_y: n(&m["y"]),
            name_y: n(&nm["y"]),
            name_x: both(&nm["anchor_x"]),
            name_cluts: nm["cluts"].as_array().into_iter().flatten().map(xy).collect(),
            plate_cluts: nm["plate_cluts"].as_array().into_iter().flatten().map(xy).collect(),
            plate_uv: uv(&nm["plate"]),
            round: Text::from(&h["intro"]["round"]),
            final_round: Text::from(&h["intro"]["final_round"]),
            ready: Text::from(&h["intro"]["ready"]),
            round_from: [h["intro"]["round_from"]["first"].as_u64().unwrap_or(0) as u32, h["intro"]["round_from"]["other"].as_u64().unwrap_or(0) as u32],
            ready_from: [h["intro"]["ready_from"]["first"].as_u64().unwrap_or(0) as u32, h["intro"]["ready_from"]["other"].as_u64().unwrap_or(0) as u32],
            time_up: Text::from(&h["ko"]["time_up"]),
            perfect: Text::from(&h["ko"]["perfect"]),
            double_ko: Text::from(&h["ko"]["double_ko"]),
            ko: Text::from(&h["ko"]["ko"]),
            draw: Text::from(&h["result"]["draw"]),
            wins: Text::from(&h["result"]["wins"]),
            you_win: Text::from(&h["result"]["you_win"]),
            you_lose: Text::from(&h["result"]["you_lose"]),
            demo: Text::from(&h["demo"]),
            replay: Text::from(&h["replay"]),
            under_y: n(&h["under_bar"]["y"]),
            under_x: both(&h["under_bar"]["x"]),
            score_colours: [0, 1].map(|k| h["under_bar"]["score_colours"][k].as_u64().unwrap_or(0) as u32),
            under_centre: both(&h["under_bar"]["centre_x"]),
            push: h["under_bar"]["push"]["texts"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect(),
            push_half: match &h["under_bar"]["push"]["half_width"] {
                v @ Value::Array(_) => both(v),
                v => [n(v); 2], // (one for both players before)
            },
            push_colour: h["under_bar"]["push"]["colour"].as_u64().unwrap_or(0) as u32,
            stage_colour: h["under_bar"]["stage"]["colour"].as_u64().unwrap_or(0) as u32,
            final_from: h["under_bar"]["stage"]["final_from"].as_u64().unwrap_or(u64::MAX) as u32,
            stage_format: h["under_bar"]["stage"]["format"].as_str().unwrap_or_default().to_string(),
            stage_final: h["under_bar"]["stage"]["final"].as_str().unwrap_or_default().to_string(),
            time_colour: h["under_bar"]["time"]["colour"].as_u64().unwrap_or(0) as u32,
            time_dx: [0, 1, 2].map(|k| n(&h["under_bar"]["time"]["dx"][k])),
            marks_dx: both(&h["under_bar"]["time"]["marks_dx"]),
            marks_dy: n(&h["under_bar"]["time"]["marks_dy"]),
            extra_names: Default::default(),
            bar_w: b["w"].as_i64().unwrap_or(0) as i32,
            bar_h: n(&b["h"]),
            cap_w: u(&b["left_cap"]["w"]),
            cap_dx: [n(&b["left_cap"]["dx"]), n(&b["right_cap"]["dx"])],
            fill_dx: both(&b["fill_dx"]),
            timer_infinity: [0, 1].map(|k| t["infinity"][k].as_u64().unwrap_or(0) as usize),
            timer_size: [u(&t["w"]), u(&t["h"])],
            gem_size: [u(&m["w"]), u(&m["h"])],
            plate: [n(&nm["plate"]["h"]), n(&nm["plate"]["dy"]), n(&nm["plate"]["pad"])],
            wins_x: {
                let r = &h["result"]["wins"]["x_rule"];
                [n(&r["centre"]), n(&r["per_char"]), n(&r["offset"])]
            },
            team: TeamIcons {
                x: both(&tm["x"]),
                step: both(&tm["step"]),
                y: n(&tm["y"]),
                icon: Tex { page: xy(&tm["page"]), bpp: u(&tm["bpp"]), u: u(&tm["u"]), v: u(&tm["v"]), clut: None },
                size: [u(&tm["w"]), u(&tm["h"])],
                cluts: [xy(&tm["cluts"]["fighting"]), xy(&tm["cluts"]["down"])],
                slash: Tex { page: xy(&tm["page"]), bpp: u(&tm["bpp"]), u: u(&tm["slash"]["u"]), v: u(&tm["slash"]["v"]), clut: Some(xy(&tm["slash"]["clut"])) },
                slash_size: [u(&tm["slash"]["w"]), u(&tm["slash"]["h"])],
                slash_y: n(&tm["slash"]["y"]),
            },
            ball: BallLayout {
                x: both(&bg["x"]),
                y: n(&bg["y"]),
                w: bg["w"].as_i64().unwrap_or(0) as i32,
                full: bg["full"].as_i64().unwrap_or(1).max(1) as i32,
                grow: bg["grow"].as_i64().unwrap_or(0) as i32,
                caps: [0, 1].map(|k| Tex { page: bpage, bpp: 4, u: u(&bg["caps"]["u"][k]), v: u(&bg["caps"]["v"]), clut: Some(xy(&bg["caps"]["clut"])) }),
                cap_size: [u(&bg["caps"]["w"]), u(&bg["caps"]["h"])],
                right_dx: n(&bg["caps"]["right_dx"]),
                strip: Tex { page: bpage, bpp: 4, u: u(&bg["strip"]["u"]), v: u(&bg["strip"]["v"]), clut: None },
                strip_h: n(&bg["strip"]["h"]),
                from_dx: both(&bg["strip"]["from_dx"]),
                cluts: [xy(&bg["cluts"]["p1"]), xy(&bg["cluts"]["p2"]), xy(&bg["cluts"]["trail"]), xy(&bg["cluts"]["empty"])],
                text_font: u(&bt["font"]),
                text_frames: u(&bt["frames"]),
                text_dy: n(&bt["dy"]),
                text_dx: n(&bt["dx_per_char"]),
                text_max: bt["max"].as_u64().unwrap_or(0) as usize,
            },
            pause_items: h["pause"]["items"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect(),
            pause_ball_items: h["pause"]["ball_items"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect(),
        }
    }
}

pub struct Hud {
    pub data: HudData,
    pub state: HudState,
    /// The camera's projection (metres to clip space), for texts over the 3D scene; set by the app each frame.
    pub view_proj: Option<glam::Mat4>,
}

/// What the HUD counts step by step (a replay's seek copies keep it: the bars and the run's time as they were).
#[derive(Clone, Debug, Default)]
pub struct HudState {
    /// Per player: shown width (grows 3 px a frame at round start, drops at once) and the trail edge.
    shown: [i32; 2],
    trail: [i32; 2],
    /// The K.O. text of this round.
    ko: Option<Announcement>,
    /// Wins at the start of the round (the newest gem blinks until the next round).
    wins_at_start: [u32; 2],
    round: u32,
    frame: u32,
    /// Arcade: fighting time of the whole run, in frames.
    run_frames: u32,
    /// Ball: the charge gauges and the floating texts.
    ball: BallHud,
}

/// Ball's HUD state (record 8 0x800B49A8 gauges, 0x800B3F98 floating texts).
#[derive(Clone, Debug, Default)]
struct BallHud {
    /// Per side: last value, target, shown and trail widths (px of the gauge's).
    gauges: [(i32, i32, i32, i32); 2],
    /// The owner side whose gauge flashes (the ball has power).
    flash: Option<usize>,
    /// Texts: string, colour, world position (game units, simulation axes), frames left.
    texts: Vec<(String, u32, glam::Vec3, u32)>,
}

impl Hud {
    pub fn new(data: HudData) -> Self {
        Self { data, state: HudState::default(), view_proj: None }
    }

    /// A new fight; the arcade run's time starts again at stage 1 (Time Attack: from its run's time).
    pub fn reset(&mut self, info: &HudInfo) {
        if let Some(base) = info.time_base {
            self.state.run_frames = base;
        } else if info.stage.is_none_or(|s| s <= 1) {
            self.state.run_frames = 0;
        }
        self.state.shown = [0; 2];
        self.state.trail = [0; 2];
        self.state.ko = None;
        self.state.round = 0;
        self.state.ball = BallHud::default();
    }

    /// Ball's gauges (two bars filling towards the middle, the owner's flashing while the ball has power) and the
    /// floating texts at the ball, rising a pixel a frame.
    fn draw_ball(&self, painter: &egui::Painter, v: &View, art: &Art) {
        let src: &[&str] = &["exe"];
        let g = &self.data.ball;
        for (i, &(_, _, shown, trail)) in self.state.ball.gauges.iter().enumerate() {
            let (x, y) = (g.x[i], g.y);
            let tint = if self.state.ball.flash == Some(i) {
                let b = crate::menu::triangle(self.state.frame << 4);
                Color32::from_gray(((0x80 + b / 2).min(255)) as u8)
            } else {
                Color32::WHITE
            };
            let [cw, ch] = g.cap_size;
            art.sprite(painter, v, src, &g.caps[0], cw, ch, x, y, tint);
            art.sprite(painter, v, src, &g.caps[1], cw, ch, x + g.right_dx, y, tint);
            let parts = [(shown, g.cluts[i.min(1)]), (trail - shown, g.cluts[2]), (g.w - trail, g.cluts[3])];
            let mut at = 0.0;
            for (w, clut) in parts {
                if w <= 0 {
                    continue;
                }
                let w = w as f32;
                let x0 = if i == 0 { x + g.from_dx[0] + at } else { x + g.from_dx[1] - at - w };
                let strip = Tex { clut: Some(clut), ..g.strip };
                art.quad(painter, v, src, &strip, 1, g.strip_h as u32, [x0, y, w, g.strip_h], [tint; 4]);
                at += w;
            }
        }
        let Some(vp) = self.view_proj else { return };
        for (s, c, world, life) in &self.state.ball.texts {
            let clip = vp * (*world / kumite_game::UNITS).extend(1.0);
            if clip.w <= 0.0 {
                continue;
            }
            let (sx, sy) = ((clip.x / clip.w + 1.0) * crate::art::CENTRE, (1.0 - clip.y / clip.w) * crate::art::SCREEN_H / 2.0);
            let rise = g.text_frames.saturating_sub(*life) as f32;
            art.text(painter, v, g.text_font as usize, *c, sx - g.text_dx * s.len() as f32, sy + g.text_dy - rise, s, Color32::WHITE);
        }
    }

    /// A name plate (0x8004E8B4 / Force 0x800B32AC): the model's name picture on a shaded plate in the costume's
    /// colours, its left edge at `x` (or its right edge, when `right`), at `y`. A sheet of names (Force's
    /// soldiers, one 16-row name per costume) shows its costume's row.
    #[allow(clippy::too_many_arguments)]
    pub fn name_plate(&self, painter: &egui::Painter, v: &View, art: &Art, model: usize, costume: usize, x: f32, y: f32, right: bool) {
        name_plate(&self.data, painter, v, art, model, costume, x, y, right);
    }

    /// STAGE n (from the final stage STAGE while frame & 0x20, else FINAL: 0x800B1A54-0x800B1A74) and the run's time
    /// mm'ss"cc (record 5 0x800B19B0).
    fn stage_time(&self, painter: &egui::Painter, v: &View, art: &Art, stage: u32, x: f32, y: f32) {
        let d = &self.data;
        let label = if stage >= d.final_from && self.state.frame & 0x20 == 0 { d.stage_final.clone() } else { crate::art::printf(&d.stage_format, &[stage]) };
        art.text(painter, v, 0, d.stage_colour, x, y, &label, Color32::WHITE);
        let f = self.state.run_frames.min(359_999);
        let parts = [f / 3600, f / 60 % 60, f % 60 * 100 / 60];
        for (k, p) in parts.iter().enumerate() {
            art.text(painter, v, 0, d.time_colour, x + d.time_dx[k], y, &format!("{p:02}"), Color32::WHITE);
        }
        for (k, m) in ["'", "\""].iter().enumerate() {
            art.text(painter, v, 0, d.time_colour, x + d.marks_dx[k], y + d.marks_dy, m, Color32::WHITE);
        }
    }

    /// Per simulation step (0x8004E274's width rules, round-start latches).
    pub fn step(&mut self, bout: &Bout) {
        self.state.frame += 1;
        if let Some(b) = &bout.game.fight.ball {
            let h = &mut self.state.ball;
            for (i, g) in h.gauges.iter_mut().enumerate() {
                let v = b.charge[i];
                let w = self.data.ball.w;
                if v != g.0 {
                    g.0 = v;
                    g.1 = (v * w / self.data.ball.full).clamp(0, w.max(0));
                }
                g.3 = if g.2 < g.3 { g.3 - ((g.3 - g.2 + 15) >> 4) } else { g.2 };
                g.2 = if g.2 >= g.1 { g.1 } else { (g.2 + self.data.ball.grow).min(g.1) };
                g.3 = g.3.max(g.2);
            }
            h.flash = (b.power != 0).then_some(b.owner);
            for t in &mut h.texts {
                t.3 = t.3.saturating_sub(1);
            }
            h.texts.retain(|t| t.3 > 0);
            for e in &b.events {
                if let kumite_sim::ball::BallEvent::Text(s, c) = e {
                    if h.texts.len() < self.data.ball.text_max {
                        h.texts.push((s.clone(), *c, b.position(), self.data.ball.text_frames));
                    }
                }
            }
        }
        if bout.phase == Phase::Fight {
            self.state.run_frames = (self.state.run_frames + 1).min(100 * 60 * 60 - 1);
        }
        if bout.round != self.state.round {
            self.state.round = bout.round;
            self.state.shown = [0; 2];
            self.state.trail = [0; 2];
            self.state.ko = None;
            self.state.wins_at_start = bout.wins;
        }
        for e in &bout.events {
            if matches!(e, Announcement::TimeUp | Announcement::Perfect | Announcement::DoubleKo | Announcement::Great | Announcement::Ko) {
                self.state.ko = Some(*e);
            }
        }
        let fight = &bout.game.fight;
        for i in 0..2 {
            // the bar is its width in px of the fighter's own full health (the VS handicap changes it)
            let bar_w = self.data.bar_w as i64;
            let max = (fight.fighters[i].max_health * kumite_sim::ONE) as i64;
            let unit = (max / bar_w.max(1)).max(1);
            let hp = fight.fighters[i].health.max(0) as i64;
            let target = ((hp + unit - 1) / unit).min(bar_w) as i32;
            (self.state.shown[i], self.state.trail[i]) = bar_step(self.state.shown[i], self.state.trail[i], target);
        }
    }

    /// The HUD; `under` paints more 2D behind the fighters (Force's pickups).
    pub fn draw_with(&self, ctx: &egui::Context, art: &Art, bout: &Bout, info: &HudInfo, under: &dyn Fn(&egui::Painter, &View)) {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("hud")));
        let v = View::fit(ctx.content_rect());
        let painter = painter.with_clip_rect(v.rect);
        let d = &self.data;
        let src: &[&str] = &["exe"];
        // no HUD during the replay, only its blinking REPLAY
        let replay = matches!(bout.phase, Phase::Replay { .. });
        if !info.hidden && !replay {
            // health bars: fill, trail and empty strips (a 1-texel column stretched), then the end caps
            for i in 0..2 {
                let [x, y] = d.bar_xy[i];
                draw_bar(d, &painter, &v, art, [x, y], i, self.state.shown[i], self.state.trail[i]);
            }
            // Ball 2 ON 2 (OpenKumite's own): each partner's health, a thin bar under its side's
            if bout.game.fight.ball_doubles() {
                for i in 0..2 {
                    let Some(f) = bout.game.fight.actor([3, 2][i]) else { continue };
                    let [x, y] = d.bar_xy[i];
                    let full = (f.max_health * kumite_sim::ONE).max(1) as f32;
                    let w = 150.0 * (f.health.max(0) as f32 / full).clamp(0.0, 1.0);
                    let x0 = if i == 0 { x + 154.0 - w } else { x + 2.0 };
                    Art::shade(&painter, &v, [x + 2.0, y + 25.0, 152.0, 5.0], [Color32::from_rgb(30, 30, 40); 4]);
                    Art::shade(&painter, &v, [x0, y + 26.0, w, 3.0], [Color32::from_rgb(250, 210, 60); 4]);
                }
            }
            // timer: two 16x46 digits, infinity when the clock is off
            let secs = bout.game.fight.clock.div_ceil(60).min(99) as usize;
            // the clock off (Ball): the infinity sign (glyphs 10 and 11)
            let digits = if bout.game.fight.clock > 100 * 60 { d.timer_infinity } else { [secs / 10, secs % 10] };
            for (k, dg) in digits.into_iter().enumerate() {
                if let Some(&[u, vv]) = d.timer_uv.get(dg) {
                    let t = Tex { page: d.timer_page, bpp: 4, u, v: vv, clut: Some(d.timer_clut) };
                    art.sprite(&painter, &v, src, &t, d.timer_size[0], d.timer_size[1], d.timer_x[k], d.timer_y, Color32::WHITE);
                }
            }
            // Team Battle: a person icon per member instead of the gems (record 5 0x800B1764): green still to
            // fight, purple with a red slash defeated, the one who just lost blinking
            if let Some(team) = &info.team {
                let tm = &d.team;
                let ([w, h], [sw, sh]) = (tm.size, tm.slash_size);
                for (i, &(size, down, just)) in team.iter().enumerate() {
                    for k in 0..size {
                        let x = tm.x[i] + tm.step[i] * k as f32;
                        let lost = k < down;
                        let blink = just && k + 1 == down && self.state.frame & 0x10 == 0;
                        let t = Tex { clut: Some(tm.cluts[usize::from(lost && !blink)]), ..tm.icon };
                        // P2's are mirrored
                        let (x0, qw) = if i == 0 { (x, w as f32) } else { (x + w as f32, -(w as f32)) };
                        art.quad(&painter, &v, src, &t, w, h, [x0, tm.y, qw, h as f32], [Color32::WHITE; 4]);
                        if lost && !blink {
                            art.sprite(&painter, &v, src, &tm.slash, sw, sh, x, tm.slash_y, Color32::WHITE);
                        }
                    }
                }
            }
            // round gems from the centre outwards; the newest win blinks until the next round
            for i in (0..2).filter(|_| info.team.is_none()) {
                let wins = bout.wins[i];
                for k in 0..bout.rounds_to_win {
                    let newest = k + 1 == wins && self.state.wins_at_start[i] != wins;
                    let won = k < wins && (!newest || self.state.frame & 0x10 != 0);
                    let t = Tex { page: d.bar_page, bpp: 4, u: d.gem_uv[0], v: d.gem_uv[1], clut: Some(d.gem_cluts[usize::from(!won)]) };
                    art.sprite(&painter, &v, src, &t, d.gem_size[0], d.gem_size[1], d.gem_x[i] + d.gem_step[i] * k as f32, d.gem_y, Color32::WHITE);
                }
            }
            // names (each character's own name picture) on a shaded plate in the costume's colours
            for i in 0..2 {
                name_plate(d, &painter, &v, art, info.models[i], info.costumes[i], d.name_x[i], d.name_y, i == 1);
            }
        }
        // over the bars, the other modes' kinds
        if !replay {
            for (i, under) in info.under.iter().enumerate() {
                let (y, x) = (d.under_y, d.under_x[i]);
                match under {
                    Under::None => {}
                    Under::Push => {
                        if self.state.frame & 0x30 != 0 {
                            if let Some(t) = d.push.get(i) {
                                art.text(&painter, &v, 0, d.push_colour, d.under_centre[i] - d.push_half[i], y, t, Color32::WHITE);
                            }
                        }
                    }
                    Under::GameOver(t, c, dx) => art.text(&painter, &v, 0, *c, x + dx, y, t, Color32::WHITE),
                    Under::Label(t, c) => art.text(&painter, &v, 0, *c, x, y, t, Color32::WHITE),
                    Under::Stage(stage) => self.stage_time(&painter, &v, art, *stage, x, y),
                    Under::Survival(rank, rc, wins, wc, dx) => {
                        art.text(&painter, &v, 0, *rc, x, y, rank, Color32::WHITE);
                        art.text(&painter, &v, 0, *wc, x + dx, y, wins, Color32::WHITE);
                    }
                }
            }
        }
        // over the bars: arcade's stage and run time on P1's side, PUSH START blinking on the CPU's
        if let (Some(stage), false) = (info.stage, replay) {
            let (y, x) = (d.under_y, d.under_x[0]);
            let label = if stage >= d.final_from && self.state.frame & 0x20 == 0 { d.stage_final.clone() } else { crate::art::printf(&d.stage_format, &[stage]) };
            art.text(&painter, &v, 0, d.stage_colour, x, y, &label, Color32::WHITE);
            let f = self.state.run_frames;
            let parts = [f / 3600, f / 60 % 60, f % 60 * 100 / 60];
            for (k, p) in parts.iter().enumerate() {
                art.text(&painter, &v, 0, d.time_colour, x + d.time_dx[k], y, &format!("{p:02}"), Color32::WHITE);
            }
            for (k, m) in ["'", "\""].iter().enumerate() {
                art.text(&painter, &v, 0, d.time_colour, x + d.marks_dx[k], y + d.marks_dy, m, Color32::WHITE);
            }
            if self.state.frame & 0x30 != 0 {
                if let Some(t) = d.push.get(1) {
                    art.text(&painter, &v, 0, d.push_colour, d.under_centre[1] - d.push_half[1], y, t, Color32::WHITE);
                }
            }
        }
        if bout.game.fight.ball.is_some() && !replay {
            self.draw_ball(&painter, &v, art);
        }
        under(&painter, &v);
        // everything above is drawn behind the fighters (the game's 2D list B); the round texts are in front
        crate::debug::mark_under(&painter);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hud_front"))).with_clip_rect(v.rect);
        if info.demo {
            d.demo.draw(&painter, &v, art);
        }
        let first = usize::from(bout.round != 1);
        match bout.phase {
            Phase::Call if info.intro.is_some() => {
                if let Some((t, f, c, x, y)) = &info.intro {
                    art.text(&painter, &v, *f, *c, *x, *y, t, Color32::WHITE);
                }
                if bout.phase_frame >= d.ready_from[first] {
                    d.ready.draw(&painter, &v, art);
                }
            }
            Phase::Call if bout.is_final_round() => d.final_round.draw(&painter, &v, art),
            Phase::Call => {
                let f = bout.phase_frame;
                if f >= d.round_from[first] {
                    let mut t = d.round.clone();
                    t.text = t.text.replace("%1x", &format!("{:x}", bout.round));
                    t.draw(&painter, &v, art);
                }
                if f >= d.ready_from[first] {
                    d.ready.draw(&painter, &v, art);
                }
            }
            Phase::Replay { .. } => {
                if bout.replay_text() {
                    d.replay.draw(&painter, &v, art);
                }
            }
            Phase::RoundOver { winner } | Phase::Result { winner } | Phase::MatchOver { winner } => {
                if matches!(bout.phase, Phase::RoundOver { .. }) {
                    let t = match self.state.ko {
                        Some(Announcement::TimeUp) => &d.time_up,
                        Some(Announcement::Perfect) => &d.perfect,
                        Some(Announcement::DoubleKo) => &d.double_ko,
                        _ => &d.ko,
                    };
                    t.draw(&painter, &v, art);
                } else if !info.no_result {
                    self.result(&painter, &v, art, winner, info);
                }
            }
            _ => {}
        }
    }

    /// DRAW, YOU WIN! / YOU LOSE (one human player), or "<NAME> WINS!".
    fn result(&self, painter: &egui::Painter, v: &View, art: &Art, winner: Option<usize>, info: &HudInfo) {
        let d = &self.data;
        let Some(w) = winner else {
            d.draw.draw(painter, v, art);
            return;
        };
        if info.humans[0] != info.humans[1] {
            let human_won = info.humans[w];
            (if human_won { &d.you_win } else { &d.you_lose }).draw(painter, v, art);
            return;
        }
        let name = info.names[w].to_uppercase();
        let mut t = d.wins.clone();
        t.text = t.text.replace("%s", &name);
        let [centre, per_char, offset] = d.wins_x;
        t.x = centre - (per_char * name.len() as f32 + offset);
        t.draw(painter, v, art);
    }
}

/// A health bar at `xy` (0x8004E274): fill, trail and empty strips (a 1-texel column stretched; side 0 fills
/// leftwards, 1 rightwards), then the end caps.
#[allow(clippy::too_many_arguments)]
pub fn draw_bar(d: &HudData, painter: &egui::Painter, v: &View, art: &Art, [x, y]: [f32; 2], side: usize, shown: i32, trail: i32) {
    let src: &[&str] = &["exe"];
    let (a, t) = (shown as f32, trail as f32);
    for (from, to, clut) in [(0.0, a, d.bar_cluts[0]), (a, t, d.bar_cluts[1]), (t, d.bar_w as f32, d.bar_cluts[2])] {
        if to <= from {
            continue;
        }
        let x0 = if side == 0 { x + d.fill_dx[0] - to } else { x + d.fill_dx[1] + from };
        let strip = Tex { page: d.bar_page, bpp: 4, u: d.strip_uv[0], v: d.strip_uv[1], clut: Some(clut) };
        art.quad(painter, v, src, &strip, 1, d.bar_h as u32, [x0, y, to - from, d.bar_h], [Color32::WHITE; 4]);
    }
    for (k, dx) in d.cap_dx.into_iter().enumerate() {
        let cap = Tex { page: d.bar_page, bpp: 4, u: d.cap_uv[k][0], v: d.cap_uv[k][1], clut: Some(d.bar_cluts[2]) };
        art.sprite(painter, v, src, &cap, d.cap_w, d.bar_h as u32, x + dx, y, Color32::WHITE);
    }
}

/// One frame of a bar (0x8004E300): the shown width drops at once or grows 3 px towards `target`; the trail
/// closes 1/16 of its gap to last frame's width, and never sits inside the bar.
fn bar_step(shown: i32, trail: i32, target: i32) -> (i32, i32) {
    let next = if shown >= target { target } else { (shown + 3).min(target) };
    let trail = if shown < trail { trail - (trail - shown + 15) / 16 } else { shown };
    (next, trail.max(next))
}

/// A name plate (0x8004E8B4 / Force 0x800B32AC): the model's name picture on a shaded plate in the costume's
/// colours, its left edge at `x` (or its right edge, when `right`), at `y`. A sheet of names (Force's
/// soldiers, one 16-row name per costume) shows its costume's row. Returns the name's width (None: nothing drawn).
#[allow(clippy::too_many_arguments)]
pub fn name_plate(d: &HudData, painter: &egui::Painter, v: &View, art: &Art, model: usize, costume: usize, x: f32, y: f32, right: bool) -> Option<f32> {
    let file = format!("names/model_{model:03}_index.png");
    let cos = costume.min(3);
    let (Some(&clut), Some(&plate)) = (d.name_cluts.get(cos), d.plate_cluts.get(cos)) else { return None };
    let strip = Tex { page: d.bar_page, bpp: 4, u: d.plate_uv[0], v: d.plate_uv[1], clut: Some(plate) };
    let [ph, pdy, pad] = d.plate;
    let Some([w, h]) = art.indexed_size(&file) else {
        // an extra fighter has no name picture: its name in the HUD font, on the plate strip
        let name = d.extra_names.get(&model)?;
        let w = art.text_w(0, name);
        let x = if right { x - w } else { x };
        art.quad(painter, v, &["exe"], &strip, 1, ph as u32, [x - pad, y + pdy, w + 2.0 * pad, ph], [Color32::WHITE; 4]);
        art.text(painter, v, 0, 0, x, y + 4.0, name, Color32::WHITE);
        return Some(w);
    };
    let (w, sheet) = (w as f32, h > 16);
    let x = if right { x - w } else { x };
    art.quad(painter, v, &["exe"], &strip, 1, ph as u32, [x - pad, y + pdy, w + 2.0 * pad, ph], [Color32::WHITE; 4]);
    if sheet {
        art.indexed_rows(painter, v, &["exe"], &file, clut, x, y, 16 * cos, 16);
    } else {
        art.indexed(painter, v, &["exe"], &file, clut, x, y);
    }
    Some(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_refill_then_drain_with_a_trail() {
        // round start: 0 -> 152 at 3 px a frame
        let w = 152;
        let (mut s, mut t) = (0, 0);
        let mut frames = 0;
        while s < w {
            (s, t) = bar_step(s, t, w);
            frames += 1;
        }
        assert_eq!((frames, t), (51, w));
        // a hit: the bar drops at once, the trail follows 1/16 of the gap a frame
        (s, t) = bar_step(s, t, 100);
        assert_eq!((s, t), (100, 152));
        (s, t) = bar_step(s, t, 100);
        assert_eq!((s, t), (100, 152 - 4));
        for _ in 0..200 {
            (s, t) = bar_step(s, t, 100);
        }
        assert_eq!(t, 100);
    }

    #[test]
    fn the_hud_tables_load() {
        let p = crate::test_assets().join("gamedata/screens.json");
        let Ok(b) = kumite_fs::read(&p) else { return };
        let d = HudData::load(&serde_json::from_slice(&b).unwrap(), &Value::Null);
        assert!(d.bar_xy[1][0] > d.bar_xy[0][0] && d.bar_w > 0 && d.timer_y > 0.0 && d.gem_y > 0.0 && d.name_y > 0.0);
        assert!(d.round.text.ends_with("%1x"), "the round's number");
        assert!(!d.ko.text.is_empty() && d.ko.x > 0.0 && d.ko.y > 0.0);
        assert!(!d.pause_items.is_empty() && d.pause_ball_items.len() > d.pause_items.len());
    }
}
