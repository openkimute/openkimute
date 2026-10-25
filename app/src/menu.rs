//! Title, main menu, options, character select and the arcade ladder, drawn like the original on its 368x480
//! screen: the layouts, strings, slot order and colours come from the converted screens.json (records 9 and 10,
//! see kumite_converter's reference engine.md), the art from the converted pictures. The roster comes from roster.json,
//! stages.json and ai.json (arcade opponent rules).

use crate::art::{additive, int, modulate, num, pair, Art, Screen as View, Tex};
use egui::Rect;
use egui::{Color32, Pos2};
use serde_json::Value;

/// The menus' system sounds (SePlay codes, bank 1; all through 0x800753BC(0, code, 0)), by their role (screens.json
/// `sounds`, set as the roster loads).
pub mod sfx {
    use std::cell::RefCell;

    fn code(role: &str) -> u16 {
        CODES.with(|c| c.borrow()[role].as_u64().unwrap_or(0) as u16)
    }
    /// Title START (0x800DB540), continue chosen (0x8004FAC4).
    pub fn start() -> u16 {
        code("start")
    }
    /// Main menu, options, pause and practice cursor (0x800DB95C, 0x800781E4).
    pub fn cursor() -> u16 {
        code("cursor")
    }
    /// Main menu confirm (0x800DB974).
    pub fn confirm() -> u16 {
        code("confirm")
    }
    /// Options page / exit, practice confirm (0x800DCA68).
    pub fn back() -> u16 {
        code("back")
    }
    /// Select screen cursor (0x8010DF98).
    pub fn select_cursor() -> u16 {
        code("select_cursor")
    }
    /// Select pick (0x8010DC00); pause open / confirm (0x8007805C).
    pub fn pick() -> u16 {
        code("pick")
    }
    /// A player joins the select screen (0x8010E23C).
    pub fn join() -> u16 {
        code("join")
    }
    /// The key configuration's list opens (0x800DEA4C).
    pub fn list_open() -> u16 {
        code("list_open")
    }

    /// The roles' codes (screens.json `sounds`).
    pub fn set(sounds: &serde_json::Value) {
        CODES.with(|c| *c.borrow_mut() = sounds.clone());
    }

    thread_local! {
        static QUEUE: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
        static CODES: RefCell<serde_json::Value> = const { RefCell::new(serde_json::Value::Null) };
    }

    /// Queue a menu sound (the app plays the queue each step).
    pub fn play(code: u16) {
        QUEUE.with(|q| q.borrow_mut().push(code));
    }

    /// The sounds queued since the last call.
    pub fn take() -> Vec<u16> {
        QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
    }
}

/// A menu key, from either player's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    /// Buttons 1..4 (0..3): confirm, and the costume when picking a character.
    Button(usize),
    Start,
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    /// 1P against a ladder of 10 CPU opponents.
    Arcade,
    /// Two players.
    Versus,
    /// CPU against CPU: the title screen's demo.
    Watch,
    /// P1 against a standing dummy, no clock, refilling health.
    Practice,
    /// Teams of 1..8 fighting one-round matches, the winner staying in (battle type 2).
    TeamBattle,
    /// The arcade ladder against the clock (type 3).
    TimeAttack,
    /// One-round fights until the first loss (type 4).
    Survival,
    /// Volleyball with fighting moves (type 7).
    Ball,
    /// Side-scrolling stages against soldiers, a boss each (type 8).
    Force,
    /// OpenKumite's extra levels: one match against a CPU on an extra level.
    ExtraLevels,
}

pub const DIFFICULTIES: [&str; 3] = ["EASY", "MEDIUM", "HARD"];
/// The CPU's levels (0..9: the AI's profile rows; an arcade ladder's stage).
pub const CPU_LEVELS: usize = 10;
/// Ball: a side nobody plays, as its roster index in a setup (the app picks the CPU's fighter). A number the
/// same on every platform (a setup goes between a desktop and a browser: usize::MAX is not).
pub const CPU_SIDE: usize = 0xFFFF;

/// OPTION's DIFFICULTY as last set (kept in the save; the title and the main menu start from it).
static DIFFICULTY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

pub fn difficulty() -> usize {
    DIFFICULTY.load(std::sync::atomic::Ordering::Relaxed).min(2)
}

pub fn set_difficulty(d: usize) {
    DIFFICULTY.store(d.min(2), std::sync::atomic::Ordering::Relaxed);
}
/// OPTION's rows: DIFFICULTY, GRAPHICS, KEY CONFIGURATION, KEYBOARD, EXIT.
const OPTION_ROWS: usize = 5;

/// A selectable character: id, name, model record per costume.
#[derive(Clone, Debug)]
pub struct Character {
    pub id: usize,
    pub name: String,
    pub costumes: Vec<usize>,
    /// Its extra costume (2) can be picked (0x80097EF4: roster.json's `extra_costume`; with the whole
    /// cast unlocked, all of theirs; roster.json `extra_costume`: always).
    pub extra: bool,
    /// The command line's prefix for it (roster.json `prefix`: `PREFIX:NAME` tells it from another of that name).
    pub prefix: String,
    /// Its select face, a picture of its own (roster.json `face`, in pictures/).
    pub face: Option<String>,
}

impl Character {
    /// The costume a select key picks (0x8010DAB8, raw pad buttons): square 0, cross / circle 1, triangle the
    /// extra costume when there is one (else 0), START the extra costume only.
    pub fn costume_for(&self, key: Key) -> Option<usize> {
        let extra = self.extra && self.costumes.len() > 2;
        match key {
            Key::Button(0) => Some(0),
            Key::Button(1) => Some(if extra { 2 } else { 0 }),
            Key::Button(_) => Some(1),
            Key::Start => extra.then_some(2),
            _ => None,
        }
    }
}

/// The select screen's portrait descriptor of a selection (character*4 + costume).
#[derive(Clone, Copy, Debug, Default)]
pub struct PortraitInfo {
    pub portrait: usize,
    /// The picture faces the other way: mirrored on the P1 side instead of the P2 side.
    pub flipped: bool,
    pub fade_type: usize,
    /// The additive glow behind the portrait.
    pub glow: [u8; 3],
    /// Cell and width of the name in the names sheet.
    pub name_cell: u32,
    pub name_w: u32,
}

/// A main menu entry (screens.json `menu.items`).
#[derive(Clone, Debug)]
pub struct MenuItem {
    pub name: String,
    pub mode: u32,
    pub colour: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub portrait_y: f32,
    pub name_y: f32,
    pub split_y: f32,
    pub grid_y: f32,
    pub row_dy: f32,
    pub cursor_h: u32,
    pub cursor_dy: f32,
    pub tag_dy: f32,
    pub timer_y: f32,
    pub timer_clut: [u32; 2],
    pub push_colour: u32,
    /// Panel pieces: screen (x, y, w, h), VRAM place, CLUT.
    pub decorations: Vec<([f32; 4], [u32; 2], [u32; 2])>,
}

/// The screens' tables (screens.json).
#[derive(Clone, Debug, Default)]
pub struct Ui {
    pub push_start: String,
    /// "CONTINUE? %d" (0x8004FFF0, printf 0x80050168) and GAME OVER (0x80050BDC, of 0x80050428), in the big font.
    pub continue_text: String,
    pub game_over: String,
    pub copyright: Vec<(String, f32, f32, u32)>,
    pub items: Vec<MenuItem>,
    /// Select slots (two rows of `per_row`) -> character id; the hidden slots are reached by moving off the edge.
    pub slots: Vec<usize>,
    pub per_row: usize,
    pub hidden: Vec<usize>,
    /// Slots past the original's 22 (the extra fighters) and the row each is at the end of.
    pub slot_rows: std::collections::HashMap<usize, usize>,
    pub layouts: Vec<Layout>,
    /// Per side: portrait x, name centre x, default slot, tag x offset.
    pub sides: [(f32, f32, usize, f32); 2],
    pub wall_page: [u32; 2],
    /// The wall's tile height and its scroll (px a frame).
    pub wall_tile_h: u32,
    pub wall_speed: u32,
    pub wall_tiles: Vec<(u32, u32, [u32; 2])>,
    pub wall_rows: Vec<Vec<usize>>,
    pub fade_heights: Vec<[f32; 3]>,
    pub fade_keys: [f32; 4],
    /// By selection (character*4 + costume).
    pub portraits: Vec<PortraitInfo>,
    /// Faces by character id: page, depth, u, v, CLUT.
    pub faces: Vec<Tex>,
    /// The face id of the gold "?" (a member not picked yet); the next is the blue one.
    pub question: usize,
    /// One row of faces: record 9's own (w, h; by character id).
    pub one_row_faces: (u32, u32, Vec<Tex>),
    /// The faces drawn in slices (screens.json `select.faces.slices`, by character id): [u offset, y offset, height].
    pub face_slices: std::collections::HashMap<usize, Vec<[u32; 3]>>,
    /// The "?" face in slices (screens.json `grid.face`): place (no CLUT), height, y offset; its CLUTs, gold and blue.
    pub question_face: Vec<(Tex, u32, f32)>,
    pub question_cluts: [[u32; 2]; 2],
    /// A greyed face's CLUT, and the red slash over a face: its sprites (place, w, h, y offset).
    pub grey_clut: [u32; 2],
    pub slash: Vec<(Tex, u32, u32, f32)>,
    /// The half-size face's slash (screens.json `modes.results.half_face`): the same sprites as quads this size, at
    /// these y offsets.
    pub half_slash: (f32, Vec<f32>),
    pub practice_label: String,
    /// The select screen's text on the empty side: TIME ATTACK! / SURVIVAL!
    pub time_attack_label: String,
    pub survival_label: String,
    pub force_label: String,
    /// The game's name of every selection (roster.json: a costume can be another character's).
    pub selection_names: Vec<String>,
    pub vs: VsData,
    pub select: SelectPieces,
    /// A face's size (screens.json `select.faces`).
    pub face_size: [u32; 2],
    /// The timer's digits (screens.json `hud.timer`): their places (no CLUT), size, the next digit's x.
    pub timer_digits: Vec<Tex>,
    pub timer_size: [u32; 2],
    pub timer_step: f32,
}

/// The select screen's pieces (screens.json `select.pieces`, record 9).
#[derive(Clone, Debug, Default)]
pub struct SelectPieces {
    /// A row's faces: the first x, the step.
    pub face_x: [f32; 2],
    /// The cursor: its page, x from the face's, a half's width; the CPU's CLUT; the tags' page and size.
    pub cursor_page: [u32; 2],
    pub cursor_dx: f32,
    pub half_w: u32,
    pub cpu_clut: [u32; 2],
    pub tag_page: [u32; 2],
    pub tag_size: [u32; 2],
    /// Per player: the cursor's uv in the one-row and the two-row layout, its CLUT, the tag's uv, the CPU tag's.
    pub players: [([[u32; 2]; 2], [u32; 2], [u32; 2], [u32; 2]); 2],
    /// The glow behind a portrait: width, its top / middle / bottom from portrait_y; the grey it ends in.
    pub glow: [f32; 4],
    pub glow_grey: [u8; 3],
    /// The lower panel: from split_y + dy to `bottom`, its width; the two-row layout's red.
    pub panel: [f32; 3],
    pub panel_red: [u8; 3],
    /// PUSH START's y; the names sheet (page, CLUT, a name's height, a cell's u and v step); the mode's label (font,
    /// colour, y from name_y, x kept within).
    pub push_y: f32,
    pub names_page: [u32; 2],
    pub names_clut: [u32; 2],
    pub names_h: u32,
    pub names_cell: [u32; 2],
    pub label: (usize, u32, f32, [f32; 2]),
    pub timer_x: f32,
    /// The edge arrows (left, right), their size, x and y from split_y.
    pub arrows: [Tex; 2],
    pub arrow_size: [u32; 2],
    pub arrow_x: [f32; 2],
    pub arrow_dy: f32,
    /// Light streaks: every, max, start brightness, dimming (+ rand), length, width, rise a frame, x bands and width.
    pub streaks: [u32; 10],
    /// The wall: the first column's width and u offset, the others' width; the draw area's top.
    pub wall: [u32; 3],
    pub top: f32,
    /// A portrait's width and its fade-in steps.
    pub portrait_w: f32,
    pub portrait_fade: u32,
}

/// The select screen's edge arrows (screens.json `select.pieces.arrows`): left and right, their size. The menus
/// point at a value with them too.
pub fn arrows(select: &Value) -> ([Tex; 2], [u32; 2]) {
    let a = &select["pieces"]["arrows"];
    let arrow = |k: usize| Tex { page: pair(&a["page"]), bpp: 4, u: int(&a["uv"][k][0]), v: int(&a["uv"][k][1]), clut: Some(pair(&a["clut"])) };
    ([arrow(0), arrow(1)], [int(&a["w"]), int(&a["h"])])
}

impl SelectPieces {
    fn load(s: &Value) -> Self {
        let p = &s["pieces"];
        let rgb = |v: &Value| [int(&v[0]) as u8, int(&v[1]) as u8, int(&v[2]) as u8];
        let (c, a, st) = (&p["cursor"], &p["arrows"], &p["streaks"]);
        let player = |k: usize| {
            let q = &s["players"][k];
            ([pair(&q["cursor_uv"][0]), pair(&q["cursor_uv"][1])], pair(&q["cursor_clut"]), pair(&q["tag_uv"]), pair(&q["cpu_tag_uv"]))
        };
        Self {
            face_x: [num(&p["faces"]["x"]), num(&p["faces"]["dx"])],
            cursor_page: pair(&c["page"]),
            cursor_dx: num(&c["dx"]),
            half_w: int(&c["half_w"]),
            cpu_clut: pair(&c["cpu_clut"]),
            tag_page: pair(&c["tag_page"]),
            tag_size: [int(&c["tag_w"]), int(&c["tag_h"])],
            players: [player(0), player(1)],
            glow: ["w", "top", "mid", "bottom"].map(|k| num(&p["glow"][k])),
            glow_grey: rgb(&p["glow"]["grey"]),
            panel: ["dy", "bottom", "w"].map(|k| num(&p["panel"][k])),
            panel_red: rgb(&p["panel"]["red"]),
            push_y: num(&p["names"]["push_y"]),
            names_page: pair(&p["names"]["page"]),
            names_clut: pair(&p["names"]["clut"]),
            names_h: int(&p["names"]["h"]),
            names_cell: pair(&p["names"]["cell"]),
            label: {
                let l = &p["names"]["label"];
                (int(&l["font"]) as usize, int(&l["colour"]), num(&l["dy"]), [num(&l["x"][0]), num(&l["x"][1])])
            },
            timer_x: num(&p["timer_x"]),
            arrows: arrows(s).0,
            arrow_size: arrows(s).1,
            arrow_x: [num(&a["x"][0]), num(&a["x"][1])],
            arrow_dy: num(&a["dy"]),
            streaks: ["every", "max", "start", "dim", "dim_rand", "len", "w", "rise", "bands", "band_w"].map(|k| int(&st[k])),
            wall: ["first_w", "first_du", "w"].map(|k| int(&p["wall"][k])),
            top: num(&p["wall"]["top"]),
            portrait_w: num(&p["portrait"]["w"]),
            portrait_fade: int(&p["portrait"]["fade"]),
        }
    }
}

/// One mode's VS screen: text, lightning panel and bar kind, portrait and name slides {x, y, vx, vy}.
#[derive(Clone, Debug, Default)]
pub struct VsMode {
    pub text: String,
    /// Lightning panel and bar kind; None: from the number of human players, "humans - 1" (arcade: 1 human = red
    /// panel, purple bar).
    pub lightning: Option<usize>,
    pub bar: Option<usize>,
    pub portraits: [[f32; 4]; 2],
    pub names: [[f32; 4]; 2],
    /// Force's card: one portrait, no VS; its second line and the stage name (raw screens.json entry).
    pub one_player: bool,
    pub raw: Value,
}

/// The pre-fight VS screen (screens.json `vs`).
#[derive(Clone, Debug, Default)]
pub struct VsData {
    pub hold: u32,
    pub lightning: Vec<String>,
    pub columns: Vec<f32>,
    pub rows: Vec<f32>,
    pub bar_y: f32,
    pub bar_h: f32,
    pub bar_fade: f32,
    pub bar_colours: Vec<[u8; 3]>,
    pub bar_widths: Vec<f32>,
    pub frame_rgb: [u8; 3],
    pub frame: Vec<[f32; 4]>,
    pub source_rows: [f32; 2],
    /// The portrait's place in its frame [x, y, w, h], the glow's split from the frame's top and its bottom grey; the
    /// lightning's tiles' size.
    pub inner: [f32; 4],
    pub glow_split: f32,
    pub glow_bottom: [u8; 3],
    pub tile: f32,
    pub name_font: usize,
    pub name_colour: u32,
    pub clamp: [f32; 2],
    pub vs_text: (String, usize, u32, f32, f32),
    pub mode_text: (usize, u32, f32, f32),
    pub modes: std::collections::HashMap<String, VsMode>,
}

impl VsData {
    fn load(v: &Value) -> Self {
        let arr = |v: &Value| v.as_array().cloned().unwrap_or_default();
        let fs = |v: &Value| arr(v).iter().map(num).collect::<Vec<f32>>();
        let four = |v: &Value| [num(&v[0]), num(&v[1]), num(&v[2]), num(&v[3])];
        let modes = v["modes"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, m)| {
                let fixed = |v: &Value| v.as_u64().map(|x| x as usize);
                let mode = VsMode {
                    text: m["text"].as_str().unwrap_or_default().to_string(),
                    lightning: fixed(&m["lightning"]),
                    bar: fixed(&m["bar"]),
                    portraits: [four(&m["portraits"][0]), four(&m["portraits"][1])],
                    names: [four(&m["names"][0]), four(&m["names"][1])],
                    one_player: m["one_player"].as_bool().unwrap_or(false),
                    raw: m.clone(),
                };
                (k.clone(), mode)
            })
            .collect();
        let t = &v["vs_text"];
        let m = &v["mode_text"];
        Self {
            hold: v["hold_frames"].as_u64().unwrap_or(0) as u32,
            lightning: arr(&v["lightning"]["pictures"]).iter().filter_map(|s| s.as_str().map(String::from)).collect(),
            columns: fs(&v["lightning"]["columns"]),
            rows: fs(&v["lightning"]["rows"]),
            bar_y: num(&v["bar"]["y"]),
            bar_h: num(&v["bar"]["h"]),
            bar_fade: num(&v["bar"]["fade"]),
            bar_colours: arr(&v["bar"]["colours"]).iter().map(|c| [int(&c[0]) as u8, int(&c[1]) as u8, int(&c[2]) as u8]).collect(),
            bar_widths: fs(&v["bar"]["widths"]),
            frame_rgb: [int(&v["portrait"]["frame_rgb"][0]) as u8, int(&v["portrait"]["frame_rgb"][1]) as u8, int(&v["portrait"]["frame_rgb"][2]) as u8],
            frame: arr(&v["portrait"]["frame"]).iter().map(four).collect(),
            source_rows: [num(&v["portrait"]["source_rows"][0]), num(&v["portrait"]["source_rows"][1])],
            inner: four(&v["portrait"]["inner"]),
            glow_split: num(&v["portrait"]["glow_split"]),
            glow_bottom: [0, 1, 2].map(|k| int(&v["portrait"]["glow_bottom"][k]) as u8),
            tile: num(&v["lightning"]["tile"]),
            name_font: int(&v["names"]["font"]) as usize,
            name_colour: int(&v["names"]["colour"]),
            clamp: [num(&v["names"]["clamp"][0]), num(&v["names"]["clamp"][1])],
            vs_text: (t["text"].as_str().unwrap_or("VS").to_string(), int(&t["font"]) as usize, int(&t["colour"]), num(&t["x"]), num(&t["y"])),
            mode_text: (int(&m["font"]) as usize, int(&m["colour"]), num(&m["x"]), num(&m["y"])),
            modes,
        }
    }
}

impl Ui {
    fn load(screens: &Value, roster: &Value) -> Self {
        let s = &screens["select"];
        let arr = |v: &Value| v.as_array().cloned().unwrap_or_default();
        let mut portraits: Vec<PortraitInfo> = arr(&s["portraits"])
            .iter()
            .map(|p| PortraitInfo {
                portrait: int(&p["portrait"]) as usize,
                flipped: p["flipped"].as_bool().unwrap_or(false),
                fade_type: int(&p["fade_type"]) as usize,
                glow: [int(&p["glow"][0]) as u8, int(&p["glow"][1]) as u8, int(&p["glow"][2]) as u8],
                ..Default::default()
            })
            .collect();
        for e in arr(&roster["selections"]["entries"]) {
            // an extra fighter: its own portrait, glow and fade (roster.json "portrait")
            let sel = int(&e["selection"]) as usize;
            // (a selection past any roster's is a bad file: skipped, not memory asked for)
            if e["portrait"].is_object() && sel < 1024 {
                if portraits.len() <= sel {
                    portraits.resize(sel + 1, PortraitInfo::default());
                }
                let q = &e["portrait"];
                portraits[sel] = PortraitInfo {
                    portrait: int(&q["portrait"]) as usize,
                    flipped: q["flipped"].as_bool().unwrap_or(false),
                    fade_type: int(&q["fade_type"]) as usize,
                    glow: [int(&q["glow"][0]) as u8, int(&q["glow"][1]) as u8, int(&q["glow"][2]) as u8],
                    ..Default::default()
                };
            }
            if let Some(p) = portraits.get_mut(int(&e["selection"]) as usize) {
                p.name_cell = int(&e["descriptor"]["char_or_announcer_id"]);
                p.name_w = int(&e["descriptor"]["byte5"]);
            }
        }
        let side = |p: &Value| (num(&p["portrait_x"]), num(&p["name_x"]), int(&p["default_slot"]) as usize, num(&p["tag_dx"]));
        let timer = &screens["hud"]["timer"];
        Self {
            push_start: screens["title"]["push_start"]["strings"][1]["text"].as_str().unwrap_or_default().to_string(),
            continue_text: screens["hud"]["continue"]["text"].as_str().unwrap_or_default().to_string(),
            game_over: screens["hud"]["continue"]["game_over"]["text"].as_str().unwrap_or_default().to_string(),
            copyright: arr(&screens["title"]["copyright"]).iter().map(|c| (c["text"].as_str().unwrap_or_default().to_string(), num(&c["x"]), num(&c["y"]), int(&c["colour"]))).collect(),
            items: arr(&screens["menu"]["items"]).iter().map(|i| MenuItem { name: i["name"].as_str().unwrap_or_default().to_string(), mode: int(&i["mode"]), colour: int(&i["colour"]) }).collect(),
            slots: arr(&s["slots"]["characters"]).iter().map(|c| int(c) as usize).collect(),
            per_row: (int(&s["slots"]["per_row"]) as usize).max(1),
            hidden: arr(&s["slots"]["hidden"]).iter().map(|c| int(c) as usize).collect(),
            slot_rows: Default::default(),
            layouts: arr(&s["layouts"])
                .iter()
                .map(|l| Layout {
                    portrait_y: num(&l["portrait_y"]),
                    name_y: num(&l["name_y"]),
                    split_y: num(&l["split_y"]),
                    grid_y: num(&l["grid_y"]),
                    row_dy: num(&l["row_dy"]),
                    cursor_h: int(&l["cursor_h"]),
                    cursor_dy: num(&l["cursor_dy"]),
                    tag_dy: num(&l["tag_dy"]),
                    timer_y: num(&l["timer_y"]),
                    timer_clut: pair(&l["timer_clut"]),
                    push_colour: int(&l["push_colour"]),
                    decorations: arr(&l["decorations"]).iter().map(|d| ([num(&d["x"]), num(&d["y"]), num(&d["w"]), num(&d["h"])], pair(&d["vram"]), pair(&d["clut"]))).collect(),
                })
                .collect(),
            sides: [side(&s["players"][0]), side(&s["players"][1])],
            wall_page: pair(&s["wall"]["page"]),
            wall_tile_h: int(&s["wall"]["tile_h"]),
            wall_speed: int(&s["wall"]["speed"]),
            wall_tiles: arr(&s["wall"]["tiles"]).iter().map(|t| (int(&t["u"]), int(&t["v"]), pair(&t["clut"]))).collect(),
            wall_rows: arr(&s["wall"]["rows"]).iter().map(|r| arr(r).iter().map(|t| int(t) as usize).collect()).collect(),
            fade_heights: arr(&s["portrait_fades"]["heights"]).iter().map(|h| [num(&h[0]), num(&h[1]), num(&h[2])]).collect(),
            fade_keys: [0, 1, 2, 3].map(|k| num(&s["portrait_fades"]["keys"][k])),
            portraits,
            faces: arr(&s["faces"]["faces"]).iter().map(|f| Tex { page: pair(&f["page"]), bpp: int(&f["bpp"]), u: int(&f["u"]), v: int(&f["v"]), clut: Some(pair(&f["clut"])) }).collect(),
            question: s["faces"]["question"][0].as_u64().map_or(usize::MAX, |q| q as usize),
            question_face: {
                let f = &screens["grid"]["face"];
                arr(&f["question_slices"]).iter().map(|q| (Tex { page: pair(&q[3]), bpp: 4, u: int(&q[0]), v: 0, clut: None }, int(&q[2]), num(&q[1]))).collect()
            },
            question_cluts: [pair(&screens["grid"]["face"]["question_cluts"]["gold"]), pair(&screens["grid"]["face"]["question_cluts"]["blue"])],
            grey_clut: pair(&s["faces"]["grey_clut"]),
            slash: {
                let sl = &s["faces"]["slash"];
                let (page, bpp, clut) = (pair(&sl["page"]), int(&sl["bpp"]), Some(pair(&sl["clut"])));
                arr(&sl["sprites"]).iter().map(|c| (Tex { page, bpp, u: int(&c["u"]), v: int(&c["v"]), clut }, int(&c["w"]), int(&c["h"]), num(&c["dy"]))).collect()
            },
            half_slash: {
                let h = &screens["modes"]["results"]["half_face"]["slash"];
                (num(&h["size"]), arr(&h["dy"]).iter().map(num).collect())
            },
            one_row_faces: {
                let o = &s["one_row_faces"];
                (int(&o["w"]), int(&o["h"]), arr(&o["faces"]).iter().map(|f| Tex::at(pair(&f["vram"]), int(&f["bpp"]), Some(pair(&f["clut"])))).collect())
            },
            face_slices: s["faces"]["slices"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| Some((k.parse().ok()?, arr(v).iter().map(|c| [int(&c[0]), int(&c[1]), int(&c[2])]).collect())))
                .collect(),
            practice_label: s["subtitles"]["practice"].as_str().unwrap_or_default().to_string(),
            time_attack_label: s["subtitles"]["time_attack"].as_str().unwrap_or_default().to_string(),
            survival_label: s["subtitles"]["survival"].as_str().unwrap_or_default().to_string(),
            force_label: s["subtitles"]["force"].as_str().unwrap_or_default().to_string(),
            selection_names: {
                // (a selection past any roster's is a bad file: not memory asked for)
                let n = arr(&roster["selections"]["entries"]).iter().map(|e| int(&e["selection"]) as usize + 1).filter(|&n| n <= 1024).max().unwrap_or(0);
                let mut names = vec![String::new(); n];
                for e in arr(&roster["selections"]["entries"]) {
                    if let Some(n) = names.get_mut(int(&e["selection"]) as usize) {
                        *n = e["name"].as_str().unwrap_or_default().to_string();
                    }
                }
                names
            },
            vs: VsData::load(&screens["vs"]),
            select: SelectPieces::load(s),
            face_size: [int(&s["faces"]["w"]), int(&s["faces"]["h"])],
            timer_digits: arr(&timer["uv"]).iter().map(|uv| Tex { page: pair(&timer["page"]), bpp: int(&timer["bpp"]), u: int(&uv[0]), v: int(&uv[1]), clut: None }).collect(),
            timer_size: [int(&timer["w"]), int(&timer["h"])],
            timer_step: num(&timer["step"]),
        }
    }
}

pub struct Roster {
    /// Home stage by (character id, costume) (roster.json selections `home_stage`).
    pub homes: std::collections::HashMap<(usize, usize), usize>,
    /// theater.json: the Theater, the endings, the staff roll.
    pub theater: Option<std::sync::Arc<crate::theater::TheaterData>>,
    pub characters: Vec<Character>,
    /// Stage names (the 15 arenas) and their home characters.
    pub stages: Vec<(String, Vec<usize>)>,
    pub ui: Ui,
    /// The game's costume number (0..3) of each model record.
    pub costume_of: std::collections::HashMap<usize, usize>,
    /// Selection (character * 4 + costume) -> character id and model record (roster.json selections).
    pub by_selection: std::collections::HashMap<usize, (usize, usize)>,
    /// Model record -> its voice row (the actor's +0x1A, 0x8004F2C0: byte 8 of the selection's descriptor, roster.json
    /// `player_1A`; an extra fighter's own, `voice_row`).
    pub voice_of: std::collections::HashMap<usize, usize>,
    /// Model record -> its fight music (the selection descriptor's BGM id: a costume that is another character plays
    /// that one's, an alternate its own).
    pub bgm_of: std::collections::HashMap<usize, usize>,
    /// screens.json, for the HUD.
    pub screens: Value,
    /// The grid select (VS MODE).
    pub grid: crate::grid::GridData,
    /// Team Battle, Time Attack and Survival tables.
    pub modes: crate::modes::ModeData,
    /// Ball's data (ball.json), and its select screen's texts (`constants.select`).
    pub ball: Option<std::sync::Arc<kumite_sim::ball::BallData>>,
    pub ball_select: Value,
    /// Its HUD: the charge gauges, the floating texts and the court lines (ball.json `constants`).
    pub ball_hud: Value,
    /// Its stage (ball.json `constants.stage`), the CPU's opponents and the first one ever (`constants.cpu`).
    pub ball_stage: usize,
    pub ball_pool: Vec<usize>,
    pub ball_first: Option<usize>,
    /// The CPU's opponent is drawn from those picked at most this many times more than the least picked.
    pub ball_spread: u32,
    /// The extra levels (stages.json entries with a `menu`): stage id and name.
    pub extra_levels: Vec<(usize, String)>,
    /// Their menu's title (their `menu`).
    pub extra_levels_title: String,
    /// The arcade ladder and the bosses (ai.json `arcade`).
    pub arcade: ArcadeData,
}

impl Roster {
    pub fn load(gamedata: &std::path::Path) -> Self {
        let read = |name: &str| -> Value { kumite_fs::read(&gamedata.join(name)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default() };
        let roster = read("roster.json");
        let index = read("movesets/index.json");
        let playable: Vec<u64> = index["models"].as_array().into_iter().flatten().filter_map(|m| m["model_record"].as_u64()).collect();
        let selections = roster["selections"]["entries"].as_array().cloned().unwrap_or_default();
        // (the select grid's, 0x80022768, and the extra fighters with a select row of their own)
        let on_grid: Vec<u64> = roster["select_grid"]["entries"].as_array().into_iter().flatten().filter_map(|e| e["character"].as_u64()).collect();
        let mut characters = Vec::new();
        for e in roster["characters"].as_array().into_iter().flatten() {
            let id = e["id"].as_u64().unwrap_or(0) as usize;
            let costumes: Vec<usize> =
                selections.iter().filter(|s| s["character"].as_u64() == Some(id as u64)).filter_map(|s| s["model_record"].as_u64()).filter(|r| playable.contains(r)).map(|r| r as usize).collect();
            if costumes.is_empty() || (!on_grid.contains(&(id as u64)) && e["select_row"].is_null()) {
                continue;
            }
            // the extra costume: the characters the costume mask allows (roster.json unlocks), or all of a character's
            let allowed = roster["unlocks"]["costume_mask_allowed_bits"].as_u64().unwrap_or(0);
            let extra = e["extra_costume"].as_bool().unwrap_or(id < 64 && allowed >> id & 1 != 0);
            let (prefix, face) = (e["prefix"].as_str().unwrap_or_default().to_string(), e["face"].as_str().map(String::from));
            characters.push(Character { id, name: e["name"].as_str().unwrap_or("?").to_string(), costumes, extra, prefix, face });
        }
        let stages = read("stages.json")["stages"]
            .as_array()
            .into_iter()
            .flatten()
            .take(15)
            .map(|s| {
                let homes = s["home_characters"].as_array().into_iter().flatten().filter_map(|c| c.as_u64()).map(|c| c as usize).collect();
                (s["name"].as_str().unwrap_or("?").to_string(), homes)
            })
            .collect();
        let stage_list = read("stages.json")["stages"].as_array().cloned().unwrap_or_default();
        let listed = || stage_list.iter().filter(|s| s["menu"].is_string());
        let extra_levels = listed().filter_map(|s| Some((s["id"].as_u64()? as usize, s["name"].as_str()?.to_string()))).collect();
        let extra_levels_title = listed().find_map(|s| s["menu"].as_str()).unwrap_or_default().to_string();
        let ball = read("ball.json");
        let costume_of = selections.iter().filter_map(|s| Some((s["model_record"].as_u64()? as usize, s["costume"].as_u64()? as usize))).collect();
        let by_selection = selections.iter().filter_map(|s| Some((s["selection"].as_u64()? as usize, (s["character"].as_u64()? as usize, s["model_record"].as_u64()? as usize)))).collect();
        let bgm_of = selections.iter().filter_map(|s| Some((s["model_record"].as_u64()? as usize, s["bgm_id"].as_u64()? as usize))).collect();
        let voice_of = selections
            .iter()
            .filter_map(|s| {
                let character = s["character"].as_u64()? as usize;
                // (an extra fighter's: its own, `voice_row`)
                let row = match s["voice_row"].as_u64() {
                    Some(r) => r as usize,
                    None => s["descriptor"]["player_1A"].as_u64().map_or(character, |r| r as usize),
                };
                Some((s["model_record"].as_u64()? as usize, row))
            })
            .collect();
        let screens = read("screens.json");
        sfx::set(&screens["sounds"]);
        let theater = crate::theater::TheaterData::load(gamedata).map(std::sync::Arc::new);
        let homes = selections.iter().filter_map(|s| Some(((s["character"].as_u64()? as usize, s["costume"].as_u64()? as usize), s["home_stage"].as_u64()? as usize))).collect();
        let mut ui = Ui::load(&screens, &roster);
        // the extra fighters: a slot each at the end of their row
        for e in roster["characters"].as_array().into_iter().flatten() {
            let (Some(row), Some(id)) = (e["select_row"].as_u64(), e["id"].as_u64()) else { continue };
            if characters.iter().any(|c| c.id == id as usize) {
                ui.slot_rows.insert(ui.slots.len(), row as usize);
                ui.slots.push(id as usize);
            }
        }
        let mut grid = crate::grid::GridData::load(&screens);
        for e in roster["characters"].as_array().into_iter().flatten() {
            let (Some(c), Some(id)) = (e["grid_slot"].as_array(), e["id"].as_u64()) else { continue };
            if characters.iter().any(|ch| ch.id == id as usize) && c.len() == 2 {
                grid.add_page_cell(c[0].as_u64().unwrap_or(0) as usize, c[1].as_u64().unwrap_or(0) as usize, id as usize);
            }
        }
        Self {
            homes,
            theater,
            characters,
            stages,
            ui,
            costume_of,
            by_selection,
            voice_of,
            bgm_of,
            grid,
            modes: crate::modes::ModeData::load(&screens),
            ball_select: ball["constants"]["select"].clone(),
            ball_hud: serde_json::json!({"gauges": ball["constants"]["gauges"], "texts": ball["constants"]["texts"], "court_lines": ball["constants"]["court_lines"]}),
            ball_stage: ball["constants"]["stage"].as_u64().unwrap_or(0) as usize,
            ball_pool: ball["constants"]["cpu"]["pool"].as_array().into_iter().flatten().filter_map(|c| c.as_u64()).map(|c| c as usize).collect(),
            ball_first: ball["constants"]["cpu"]["first_ever"].as_u64().map(|c| c as usize),
            ball_spread: ball["constants"]["cpu"]["spread"].as_u64().unwrap_or(0) as u32,
            ball: serde_json::from_value::<kumite_sim::ball::BallData>(ball).ok().filter(|b| !b.types.is_empty()).map(std::sync::Arc::new),
            screens,
            extra_levels,
            extra_levels_title,
            arcade: serde_json::from_value(read("ai.json")["arcade"].clone()).unwrap_or_else(|e| {
                log::error!("ai.json arcade: {e}");
                ArcadeData::default()
            }),
        }
    }

    /// The fight music (BGM id) of a character's costume.
    pub fn music_of(&self, id: usize, costume: usize) -> Option<usize> {
        let c = self.by_id(id)?;
        self.bgm_of.get(c.costumes.get(costume).or(c.costumes.first())?).copied()
    }

    pub fn by_id(&self, id: usize) -> Option<&Character> {
        self.characters.iter().find(|c| c.id == id)
    }

    pub fn index_of(&self, id: usize) -> Option<usize> {
        self.characters.iter().position(|c| c.id == id)
    }

    /// The arena a character in a costume calls home (roster.json selections' `home_stage`), else the
    /// first stage listing the character, else stage 0.
    /// Ball's ball types to choose from (ball.json's; the three of the original without it).
    pub fn ball_types(&self) -> usize {
        self.ball.as_ref().map_or(3, |b| b.types.len().max(1)).max(crate::grid::BALL_KINDS)
    }

    /// A stage a fight's setup can name: an arena's, a home stage, or Ball's beach.
    pub fn is_stage(&self, stage: usize) -> bool {
        stage < self.stages.len() || stage == self.ball_stage || self.homes.values().any(|&s| s == stage)
    }

    pub fn home_stage(&self, id: usize, costume: usize) -> usize {
        self.homes.get(&(id, costume)).copied().unwrap_or_else(|| self.stages.iter().position(|(_, homes)| homes.contains(&id)).unwrap_or(0))
    }

    /// The portrait descriptor of a character's costume.
    fn portrait(&self, id: usize, costume: usize) -> PortraitInfo {
        self.ui.portraits.get(id * 4 + costume.min(3)).copied().unwrap_or_default()
    }

    /// The select slots of row `r` that hold a character, in order (the hidden slot excluded).
    fn row_slots(&self, r: usize) -> Vec<usize> {
        // the original's slots (rows 0 and 1; the slots after them are the added fighters'), then the added
        // (no wider than the slots: a huge per_row walks no more than them)
        let n = self.ui.per_row.min(self.ui.slots.len());
        let original = if r < 2 { r * n..r * n + n } else { 0..0 };
        let mut v: Vec<usize> = original.filter(|s| !self.ui.hidden.contains(s) && self.ui.slots.get(*s).is_some_and(|c| self.by_id(*c).is_some())).collect();
        let mut extra: Vec<usize> = self.ui.slot_rows.iter().filter(|(_, row)| **row == r).map(|(s, _)| *s).collect();
        extra.sort_unstable();
        v.extend(extra);
        v
    }

    /// The row of a slot (`per_row` a row; the slots past the original's are at the end of their own row).
    /// Rows of faces: the original's two, then the extra fighters' pages' (`select_row` 2 on).
    fn rows(&self) -> usize {
        self.ui.slot_rows.values().map(|r| r + 1).max().unwrap_or(2).max(2)
    }
    fn row_of(&self, slot: usize) -> usize {
        self.ui.slot_rows.get(&slot).copied().unwrap_or((slot / self.ui.per_row).min(1))
    }

    /// Two rows of faces once any second-row character is there (always, with the whole cast unlocked).
    fn layout(&self) -> usize {
        usize::from(!self.row_slots(1).is_empty())
    }

    /// The hidden slot of row `r`, when its character is there.
    fn hidden_slot(&self, r: usize) -> Option<usize> {
        self.ui.hidden.iter().copied().find(|s| s / self.ui.per_row == r && self.ui.slots.get(*s).is_some_and(|c| self.by_id(*c).is_some()))
    }
}

/// What the fight needs from the menus.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FightSetup {
    pub mode: Mode,
    /// (character index in the roster, costume)
    pub players: [(usize, usize); 2],
    pub stage: usize,
    pub difficulty: usize,
    /// Arcade: stage of the ladder (0..9), the CPU level.
    pub level: usize,
    /// Full health per player in percent (VS MODE's LIFE handicap, 70..140).
    pub handicap: [i32; 2],
    /// Ball: the ball type (0 BEACH, 1 GUM, 2 IRON) and the side that chose it (the first server).
    pub ball: Option<(usize, usize)>,
    /// Ball 2 ON 2 (OpenKumite's own): each side's second fighter and who plays the four.
    pub partners: Option<Doubles>,
}

/// Ball 2 ON 2's seating: the partners (roster index, costume) of P1's side and the other, and who plays
/// each actor (0, 1 the sides' first, 2 the other side's partner, 3 P1's side's; None the CPU).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Doubles {
    pub partners: [(usize, usize); 2],
    pub seats: [Option<usize>; 4],
}

/// No model shows twice on screen: the costume of a character with `costumes` costumes a player gets for `want`,
/// the first from it (round) that `taken` does not hold; None when every costume is taken (the character cannot be
/// played).
pub fn free_costume(costumes: usize, want: usize, taken: impl Fn(usize) -> bool) -> Option<usize> {
    let n = costumes.max(1);
    let want = want.min(n - 1);
    // the original's mirror match (0x8004F04C): 0 <-> 1, 2 -> 0; then any other
    let alt = if want == 0 { 1 } else { 0 };
    [want, alt].into_iter().filter(|&c| c < n).chain(0..n).find(|&c| !taken(c))
}

/// A fight's two models apart: the opponent (side 1) takes a free costume of its character, or when it has none
/// left, the next character in the roster that has one.
pub fn distinct_models(roster: &Roster, setup: &mut FightSetup) {
    let [(a, ca), (b, cb)] = setup.players;
    if a != b || roster.characters.get(b).is_none() {
        return;
    }
    let costumes = roster.characters[b].costumes.len();
    if let Some(c) = free_costume(costumes, cb, |c| c == ca) {
        setup.players[1].1 = c;
        return;
    }
    let n = roster.characters.len();
    if let Some(other) = (1..n).map(|d| (b + d) % n).find(|&i| !roster.characters[i].costumes.is_empty()) {
        setup.players[1] = (other, 0);
    }
}

pub fn lcg(seed: &mut u32, n: usize) -> usize {
    *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
    ((*seed >> 16) as usize) % n.max(1)
}

/// One step of the arcade ladder (ai.json `arcade.ladder`).
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum LadderStep {
    /// `draw` distinct characters from `from`, or from everyone but `except`.
    Draw {
        draw: usize,
        #[serde(default)]
        from: Option<Vec<usize>>,
        #[serde(default)]
        except: Vec<usize>,
    },
    /// A boss; `instead` for a player who is `player`.
    Boss {
        boss: usize,
        #[serde(default)]
        player: Option<usize>,
        #[serde(default)]
        instead: Option<usize>,
    },
}

/// The arcade's ladder and the bosses (ai.json `arcade`).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ArcadeData {
    pub ladder: Vec<LadderStep>,
    /// TAG ARCADE's last pairs.
    pub boss_pairs: Vec<[usize; 2]>,
}

impl ArcadeData {
    /// The characters kept out of the drawn fights (the bosses).
    pub fn bosses(&self) -> Vec<usize> {
        self.ladder.iter().flat_map(|s| if let LadderStep::Draw { except, .. } = s { except.clone() } else { Vec::new() }).collect()
    }
}

/// The arcade ladder (ai.json `arcade.ladder`: its steps drawn from pools or fixed bosses, a boss's stand-in for a
/// player who is that boss). Character ids.
pub fn arcade_ladder(roster: &Roster, player: usize, seed: &mut u32) -> Vec<usize> {
    let mut ladder: Vec<usize> = Vec::new();
    for step in &roster.arcade.ladder {
        match step {
            LadderStep::Draw { draw, from, except } => {
                let pool = from.clone().unwrap_or_else(|| roster.characters.iter().map(|c| c.id).filter(|c| !except.contains(c)).collect());
                let mut pool: Vec<usize> = pool.into_iter().filter(|c| *c != player && !ladder.contains(c) && roster.by_id(*c).is_some()).collect();
                for _ in 0..*draw {
                    if pool.is_empty() {
                        break;
                    }
                    ladder.push(pool.remove(lcg(seed, pool.len())));
                }
            }
            LadderStep::Boss { boss, player: p, instead } => ladder.push(instead.filter(|_| Some(player) == *p).unwrap_or(*boss)),
        }
    }
    ladder
}

/// Arcade stage `level` for a player pick.
pub fn arcade_setup(roster: &Roster, player: (usize, usize), difficulty: usize, ladder: &[usize], level: usize) -> FightSetup {
    let opp_id = ladder.get(level).or(ladder.last()).copied().unwrap_or(0);
    let opp = roster.index_of(opp_id).unwrap_or(0);
    FightSetup { mode: Mode::Arcade, players: [player, (opp, 0)], stage: roster.home_stage(opp_id, 0), difficulty, level, handicap: [100; 2], ball: None, partners: None }
}

// ---------------------------------------------------------------- screens

/// Title sequence (record 10, 0x800DB33C): white fade-in, white hold, the title with the white fading out, then
/// the full title for 300 frames, a fade to black and the demo.
const WHITE_IN: u32 = 26;
/// After a movie or the embu the title starts here: the picture under the white fading out.
pub const WHITE_HOLD: u32 = WHITE_IN + 11;
const WHITE_OUT: u32 = WHITE_HOLD + 86;
const TITLE_HOLD: u32 = WHITE_OUT + 300;
const TITLE_END: u32 = TITLE_HOLD + 32;
/// The main menu goes back to the title after this many idle frames.
const MENU_IDLE: u32 = 480;
/// Select: 20 seconds to choose, then the frames between the last pick and the fight.
const SELECT_SECONDS: u32 = 20;
const SELECT_OUTRO: u32 = 50;
/// The continue count: 808 frames, one number per 90 (9 .. 1), then GAME OVER for 120 frames.
pub const CONTINUE_FRAMES: u32 = 808;
pub const CONTINUE_STEP: u32 = 90;
const GAME_OVER_FRAMES: u32 = 120;

pub enum Screen {
    Title {
        frame: u32,
        difficulty: usize,
    },
    Menu(Menu),
    Options {
        frame: u32,
        cursor: usize,
        difficulty: usize,
    },
    Select(Box<Select>),
    /// Arcade: lost a stage.
    Continue {
        setup: FightSetup,
        ladder: Vec<usize>,
        countdown: u32,
    },
    /// After the continue ran out.
    GameOver {
        frames: u32,
    },
    /// VS MODE's grid select (screen B, mode 10).
    Grid(Box<crate::grid::GridSelect>),
    /// The pre-fight VS screen (mode 11); `number`: the mode text's number (FIGHT n / STAGE n).
    Versus {
        setup: FightSetup,
        ladder: Vec<usize>,
        frame: u32,
        number: u32,
        extra: Box<VsExtra>,
    },
    /// THEATER MODE (record 301 mode 19): the movies, the music player.
    Theater(Box<crate::theater::Theater>),
    /// The staff roll after an ending.
    StaffRoll(Box<crate::theater::StaffRoll>),
    /// Team Battle / Time Attack / Survival results (record 302).
    Results(Box<crate::modes::Results>),
    /// Force's results (record 302 mode 15).
    ForceResults(Box<crate::forcerun::ForceResults>),
    /// The records screens (mode 17), over a stage the camera circles.
    Records(Box<crate::records::RecordsScreen>),
    /// A movie, then `next` (START skips it).
    Movie(Box<crate::movie::Movie>, AfterMovie),
    /// The attract loop's the PRESENTS screen (0x8004FA60), then `next`.
    Presents {
        frame: u32,
        next: AttractNext,
    },
    /// TAG BRAWL's modes (OpenKumite's own): free for all, two on two, tag arcade.
    TagModes {
        frame: u32,
        cursor: usize,
        difficulty: usize,
    },
    /// An online room's screen (who is in, the host chooses the mode): its keys go through the room (the app).
    Room(Box<crate::room::RoomScreen>),
    /// BALL's choice (OpenKumite's own): 1 ON 1 (the original) or 2 ON 2.
    BallModes {
        frame: u32,
        cursor: usize,
        difficulty: usize,
    },
    /// TAG BRAWL's setup (OpenKumite's own mode).
    Tag(Box<crate::tagrun::TagSelect>),
    /// ONLINE (OpenKumite's own): how to meet, the fighter, then waiting for the other player.
    Online(Box<crate::onlinemenu::OnlineMenu>),
    /// The extra levels (OpenKumite's own): the levels to fight on.
    ExtraLevels {
        frame: u32,
        cursor: usize,
        difficulty: usize,
    },
    /// OPTION > GRAPHICS (OpenKumite's own).
    Graphics(Box<GraphicsMenu>),
    /// OPTION > KEY CONFIGURATION (the original's: each player's gamepad).
    KeyConfig(Box<crate::keyconfig::KeyConfig>),
    /// OPTION > KEYBOARD (OpenKumite's: each player's keys).
    Keyboard(Box<crate::keyconfig::KeyboardConfig>),
}

/// What the VS screen shows besides the two fighters: how many humans play (the lightning and bar of some modes),
/// and Team Battle's teams (the strips of faces).
#[derive(Clone, Debug, Default)]
pub struct VsExtra {
    pub humans: usize,
    pub teams: Option<[crate::modes::Team; 2]>,
    /// Force: the stage number (0-based; None: the final stage) and its name's two lines and colour.
    pub force: Option<(Option<usize>, [String; 2], u32)>,
}

/// Where a movie leads.
#[derive(Clone)]
pub enum AfterMovie {
    /// The attract loop's title (the picture under the white fading out).
    Title,
    /// An arcade ending: the staff roll.
    Credits,
    /// Back to the Theater, on the same entry.
    Theater(Box<crate::theater::Theater>),
}

/// What the attract loop shows after the PRESENTS screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttractNext {
    /// A movie (its number: attract.json `cycle`), then the title.
    Movie(usize),
    Embu,
}

/// the PRESENTS screen: 32 frames in, 2 more, held 91 after the fade-in, 32 out, 2 black (0x8004F750 drawer).
const PRESENTS_IN: u32 = 32;
const PRESENTS_HOLD: u32 = PRESENTS_IN + 2 + 91;
const PRESENTS_OUT: u32 = PRESENTS_HOLD + 32;
const PRESENTS_END: u32 = PRESENTS_OUT + 2;

/// What the menu wants the app to do.
pub enum Outcome {
    None,
    Quit,
    /// Start a fight (and, in arcade, the ladder of opponent character ids): the VS screen first.
    Fight(FightSetup, Vec<usize>),
    /// The VS screen is over: load the fight.
    Load(FightSetup, Vec<usize>),
    /// The title ran out: the attract demo fight (battle type 6).
    Demo,
    /// The main menu idled: back to the attract loop (`request_scene(3)`).
    Attract,
    /// START on the title: the main menu, and the attract loop moves on a step.
    TitleStart,
    /// the PRESENTS screen is over: what comes next.
    AttractNext(AttractNext),
    /// A mode's run begins (Team Battle, Time Attack, Survival): its first fight.
    Run(Box<crate::modes::Run>),
    /// Force begins with this player (roster index, costume) at this difficulty.
    StartForce((usize, usize), usize),
    /// THEATER MODE from the main menu.
    OpenTheater,
    /// REPLAYS from the main menu.
    OpenReplays,
    /// EXIT from the main menu: leave the game.
    Exit,
    /// TAG BRAWL's setup is done: the match.
    StartTag(crate::tagrun::TagSetup),
    /// ONLINE's START: meet the other player.
    StartOnline(Box<crate::onlinemenu::OnlineSetup>),
    /// ONLINE's CANCEL while waiting.
    CancelOnline,
    /// What the Theater wants (a movie, music, the exit).
    Theater(crate::theater::Event),
    /// OPTION > GRAPHICS chosen: the app opens it (it knows the display's modes).
    OpenGraphics(usize),
    /// OPTION > KEY CONFIGURATION chosen: the app opens it (it has the controls).
    OpenKeyConfig(usize),
    /// OPTION > KEYBOARD chosen.
    OpenKeyboard(usize),
    /// OPTION's DIFFICULTY changed: kept.
    Difficulty(usize),
    /// The controls changed (KEY CONFIG): applied and kept.
    Controls(crate::controls::Controls),
    /// A graphics setting changed: apply and keep it.
    Graphics(crate::graphics::Graphics),
}

pub struct Menu {
    pub frame: u32,
    pub idle: u32,
    /// Index into the shown items.
    pub cursor: usize,
    /// Scroll offset in pixels (eases to 0).
    pub scroll: i32,
    /// Highlight bar fade-in (0..256).
    pub bar: u32,
    pub difficulty: usize,
}

/// The main menu entries this port plays (the game's own names and colours), and what each starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Mode(Mode),
    Options,
    Theater,
    /// OpenKumite's own last entry: leave the game (not in a browser).
    Exit,
    /// OpenKumite's own mode: two against two on one stage.
    Tag,
    /// OpenKumite's own: the extra levels.
    ExtraLevels,
    /// OpenKumite's own: VS online (a room code or an IP).
    Online,
    /// OpenKumite's own: the matches recorded, to watch again.
    Replays,
}

fn entry_of(item: &MenuItem) -> Option<Entry> {
    match item.mode {
        0 => Some(Entry::Mode(Mode::Arcade)),
        1 => Some(Entry::Mode(Mode::Versus)),
        2 => Some(Entry::Mode(Mode::TeamBattle)),
        3 => Some(Entry::Mode(Mode::TimeAttack)),
        4 => Some(Entry::Mode(Mode::Survival)),
        7 => Some(Entry::Mode(Mode::Ball)),
        8 => Some(Entry::Mode(Mode::Force)),
        5 => Some(Entry::Mode(Mode::Practice)),
        9 => Some(Entry::Options),
        10 => Some(Entry::Theater),
        _ => None,
    }
}

fn shown_items(roster: &Roster) -> Vec<(MenuItem, Entry)> {
    let mut items: Vec<(MenuItem, Entry)> = roster.ui.items.iter().filter_map(|i| entry_of(i).map(|e| (i.clone(), e))).collect();
    // (OpenKumite's EXIT after the game's entries, red like VS MODE: every other colour of the menu font is taken or
    // too dim; a page cannot close itself)
    // (and OpenKumite's own TAG BRAWL before it)
    if !items.is_empty() {
        let at = items.iter().position(|i| i.1 == Entry::Options).unwrap_or(items.len());
        items.insert(at, (MenuItem { name: "TAG BRAWL".into(), mode: u32::MAX - 1, colour: 7 }, Entry::Tag));
        items.insert(at + 1, (MenuItem { name: "ONLINE".into(), mode: u32::MAX - 3, colour: 9 }, Entry::Online));
        if !roster.extra_levels.is_empty() {
            items.insert(at + 2, (MenuItem { name: roster.extra_levels_title.clone(), mode: u32::MAX - 2, colour: 5 }, Entry::ExtraLevels));
        }
        let at = items.iter().position(|i| i.1 == Entry::Options).unwrap_or(items.len());
        items.insert(at, (MenuItem { name: "REPLAYS".into(), mode: u32::MAX - 4, colour: 6 }, Entry::Replays));
    }
    if cfg!(not(target_arch = "wasm32")) && !items.is_empty() {
        items.push((MenuItem { name: "EXIT".into(), mode: u32::MAX, colour: 2 }, Entry::Exit));
    }
    if items.is_empty() {
        // no screens.json: plain entries (OpenKumite's names)
        let plain = |name: &str, colour, e| (MenuItem { name: name.into(), mode: 0, colour }, e);
        return vec![
            plain("ARCADE", 5, Entry::Mode(Mode::Arcade)),
            plain("VERSUS", 2, Entry::Mode(Mode::Versus)),
            plain("PRACTICE", 0, Entry::Mode(Mode::Practice)),
            plain("OPTIONS", 9, Entry::Options),
        ];
    }
    items
}

/// One side of the select screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideState {
    /// Nobody plays this side: "PUSH START" blinks.
    Off,
    /// The empty side of a one-player mode: the mode's name.
    Label,
    Choosing,
    /// Choosing the CPU's character (green cursor, "CPU" tag), with P1's controls.
    CpuChoosing,
    Chosen,
    CpuChosen,
}

#[derive(Clone, Copy, Debug)]
pub struct Side {
    pub state: SideState,
    pub slot: usize,
    pub costume: usize,
    /// Portrait fade-in after a change (28 -> 0).
    pub fade: u32,
}

impl Side {
    fn choosing(&self) -> bool {
        matches!(self.state, SideState::Choosing | SideState::CpuChoosing)
    }
    fn shows(&self) -> bool {
        !matches!(self.state, SideState::Off | SideState::Label)
    }
}

/// A rising light streak behind the names.
#[derive(Clone, Copy, Debug)]
struct Streak {
    side: usize,
    x: f32,
    y: f32,
    b: i32,
}

pub struct Select {
    pub mode: Mode,
    pub difficulty: usize,
    pub sides: [Side; 2],
    pub frame: u32,
    /// Frames left to choose.
    pub timer: u32,
    /// Frames since every side chose.
    pub done: u32,
    /// Frames the edge arrows still show (180 when a hidden slot is there).
    arrows: u32,
    streaks: Vec<Streak>,
    rng: u32,
    /// The stage the fight takes place on instead of the opponent's home (the extra levels).
    pub stage: Option<usize>,
    /// The empty side's label in OpenKumite's own modes (the extra levels: the level's name).
    pub label: String,
    /// The page of faces shown (two rows a page; the extra fighters' is 1): the last moved cursor's.
    pub page: usize,
}

impl Screen {
    pub fn title() -> Self {
        Screen::Title { frame: 0, difficulty: difficulty() }
    }

    /// the extra levels' list with the cursor on a stage.
    pub fn extra_levels(roster: &Roster, stage: usize, difficulty: usize) -> Self {
        Screen::ExtraLevels { frame: 0, cursor: roster.extra_levels.iter().position(|l| l.0 == stage).unwrap_or(0), difficulty }
    }

    /// The main menu.
    pub fn menu(difficulty: usize) -> Self {
        Screen::Menu(Menu { frame: 0, idle: 0, cursor: 0, scroll: 0, bar: 0, difficulty })
    }

    /// Handle a key of `player` (0 = P1, 1 = P2).
    pub fn key(&mut self, player: usize, key: Key, roster: &Roster, seed: &mut u32) -> Outcome {
        match self {
            // START (only) goes to the main menu from any part of the title (0x800DB528)
            Screen::Title { difficulty, .. } => match key {
                Key::Back => return Outcome::Quit,
                Key::Start => {
                    sfx::play(sfx::start());
                    *self = Screen::menu(*difficulty);
                    return Outcome::TitleStart;
                }
                _ => {}
            },
            Screen::Presents { .. } => match key {
                Key::Back => return Outcome::Quit,
                Key::Start => {
                    sfx::play(sfx::start());
                    *self = Screen::menu(difficulty());
                }
                _ => {}
            },
            Screen::Menu(m) => {
                let items = shown_items(roster);
                m.idle = 0;
                match key {
                    Key::Up => {
                        sfx::play(sfx::cursor());
                        m.cursor = (m.cursor + items.len() - 1) % items.len();
                        m.scroll -= 28;
                        m.bar = 0;
                    }
                    Key::Down => {
                        sfx::play(sfx::cursor());
                        m.cursor = (m.cursor + 1) % items.len();
                        m.scroll += 28;
                        m.bar = 0;
                    }
                    Key::Start | Key::Button(_) => {
                        sfx::play(sfx::confirm());
                        if items[m.cursor].1 == Entry::Theater {
                            return Outcome::OpenTheater;
                        }
                        if items[m.cursor].1 == Entry::Replays {
                            return Outcome::OpenReplays;
                        }
                        if items[m.cursor].1 == Entry::Exit {
                            return Outcome::Exit;
                        }
                        *self = match items[m.cursor].1 {
                            // VS MODE has its own grid select (screen B), with a fresh win tally
                            Entry::Mode(Mode::Versus) => Screen::Grid(Box::new(crate::grid::GridSelect::new(roster.grid.clone(), m.difficulty, Default::default()))),
                            // Team Battle picks its teams on the grid too
                            Entry::Mode(Mode::TeamBattle) => Screen::Grid(Box::new(crate::grid::GridSelect::team(roster, m.difficulty))),
                            Entry::Mode(Mode::Ball) => Screen::BallModes { frame: 0, cursor: 0, difficulty: m.difficulty },
                            Entry::Mode(mode) => Screen::Select(Box::new(Select::new(mode, m.difficulty, roster, *seed))),
                            Entry::Tag => Screen::TagModes { frame: 0, cursor: 1, difficulty: m.difficulty },
                            Entry::Online => Screen::Online(Box::new(crate::onlinemenu::OnlineMenu::new(*seed, m.difficulty))),
                            Entry::ExtraLevels => Screen::ExtraLevels { frame: 0, cursor: 0, difficulty: m.difficulty },
                            Entry::Options | Entry::Theater | Entry::Exit | Entry::Replays => Screen::Options { frame: 0, cursor: 0, difficulty: m.difficulty },
                        };
                    }
                    Key::Back => *self = Screen::Title { frame: WHITE_OUT, difficulty: m.difficulty },
                    _ => {}
                }
            }
            Screen::Options { cursor, difficulty, .. } => match key {
                // DIFFICULTY, GRAPHICS (OpenKumite's own), KEY CONFIGURATION, KEYBOARD (OpenKumite's own), EXIT
                Key::Up => {
                    sfx::play(sfx::cursor());
                    *cursor = (*cursor + OPTION_ROWS - 1) % OPTION_ROWS;
                }
                Key::Down => {
                    sfx::play(sfx::cursor());
                    *cursor = (*cursor + 1) % OPTION_ROWS;
                }
                Key::Start | Key::Button(_) if *cursor == 1 => {
                    sfx::play(sfx::confirm());
                    return Outcome::OpenGraphics(*difficulty);
                }
                Key::Start | Key::Button(_) if *cursor == 2 => {
                    sfx::play(sfx::confirm());
                    return Outcome::OpenKeyConfig(*difficulty);
                }
                Key::Start | Key::Button(_) if *cursor == 3 => {
                    sfx::play(sfx::confirm());
                    return Outcome::OpenKeyboard(*difficulty);
                }
                Key::Left if *cursor == 0 => {
                    sfx::play(sfx::cursor());
                    *difficulty = (*difficulty + 2) % 3;
                    return Outcome::Difficulty(*difficulty);
                }
                Key::Right if *cursor == 0 => {
                    sfx::play(sfx::cursor());
                    *difficulty = (*difficulty + 1) % 3;
                    return Outcome::Difficulty(*difficulty);
                }
                Key::Start | Key::Button(_) if *cursor == 0 => {
                    sfx::play(sfx::cursor());
                    *difficulty = (*difficulty + 1) % 3;
                    return Outcome::Difficulty(*difficulty);
                }
                Key::Start | Key::Button(_) | Key::Back => {
                    sfx::play(sfx::back());
                    let d = *difficulty;
                    *self = Screen::menu(d);
                    if let Screen::Menu(m) = self {
                        m.cursor = shown_items(roster).iter().position(|i| i.1 == Entry::Options).unwrap_or(0);
                    }
                }
                _ => {}
            },
            Screen::ExtraLevels { cursor, difficulty, .. } => {
                let n = roster.extra_levels.len().max(1);
                // (OpenKumite's list: square / triangle / START pick, cross / circle go back as SELECT)
                let key = if matches!(key, Key::Button(2 | 3)) { Key::Back } else { key };
                match key {
                    Key::Up => {
                        sfx::play(sfx::cursor());
                        *cursor = (*cursor + n - 1) % n;
                    }
                    Key::Down => {
                        sfx::play(sfx::cursor());
                        *cursor = (*cursor + 1) % n;
                    }
                    Key::Start | Key::Button(_) => {
                        if let Some((stage, name)) = roster.extra_levels.get(*cursor) {
                            sfx::play(sfx::confirm());
                            let mut sel = Select::new(Mode::ExtraLevels, *difficulty, roster, *seed);
                            sel.stage = Some(*stage);
                            sel.label = level_title(name);
                            *self = Screen::Select(Box::new(sel));
                        }
                    }
                    Key::Back => {
                        sfx::play(sfx::back());
                        let d = *difficulty;
                        *self = Screen::menu(d);
                        if let Screen::Menu(m) = self {
                            m.cursor = shown_items(roster).iter().position(|i| i.1 == Entry::ExtraLevels).unwrap_or(0);
                        }
                    }
                    _ => {}
                }
            }
            Screen::Graphics(g) => {
                if let Some(o) = g.key(key) {
                    return o;
                }
                if g.done {
                    let d = g.difficulty;
                    *self = Screen::Options { frame: 0, cursor: 1, difficulty: d };
                }
            }
            Screen::KeyConfig(k) => {
                if let Some(o) = k.key(player, key) {
                    return o;
                }
                if k.done {
                    let d = k.difficulty;
                    *self = Screen::Options { frame: 0, cursor: 2, difficulty: d };
                }
            }
            Screen::Keyboard(k) => {
                if let Some(o) = k.key(key) {
                    return o;
                }
                if k.done {
                    let d = k.difficulty;
                    *self = Screen::Options { frame: 0, cursor: 3, difficulty: d };
                }
            }
            Screen::Tag(t) => match t.key(player, key, roster) {
                Outcome::Quit if t.mode == crate::tagrun::TagMode::Ball => {
                    *self = Screen::BallModes { frame: 0, cursor: 1, difficulty: t.difficulty };
                }
                Outcome::Quit => {
                    let (d, mode) = (t.difficulty, t.mode);
                    let cursor = crate::tagrun::TagMode::ALL.iter().position(|&m| m == mode).unwrap_or(0);
                    *self = Screen::TagModes { frame: 0, cursor, difficulty: d };
                }
                o => return o,
            },
            Screen::Online(o) => match o.key(key) {
                Outcome::Quit => {
                    let d = o.difficulty;
                    *self = Screen::menu(d);
                    if let Screen::Menu(m) = self {
                        m.cursor = shown_items(roster).iter().position(|i| i.1 == Entry::Online).unwrap_or(0);
                    }
                }
                o => return o,
            },
            // (the room's keys go through the app)
            Screen::Room(_) => {}
            Screen::BallModes { cursor, difficulty, .. } => {
                // (as TAG BRAWL's list)
                let key = if matches!(key, Key::Button(2 | 3)) { Key::Back } else { key };
                match key {
                    Key::Up | Key::Down => {
                        sfx::play(sfx::cursor());
                        *cursor = 1 - *cursor;
                    }
                    Key::Start | Key::Button(_) => {
                        sfx::play(sfx::confirm());
                        *self = if *cursor == 0 {
                            Screen::Grid(Box::new(crate::grid::GridSelect::ball(roster, *difficulty, 0)))
                        } else {
                            // 2 ON 2: the four-slot setup (the players join with START, the ball after)
                            Screen::Tag(Box::new(crate::tagrun::TagSelect::new(roster, *difficulty, *seed, crate::tagrun::TagMode::Ball)))
                        };
                    }
                    Key::Back => {
                        sfx::play(sfx::back());
                        let d = *difficulty;
                        *self = Screen::menu(d);
                        if let Screen::Menu(m) = self {
                            m.cursor = shown_items(roster).iter().position(|i| i.1 == Entry::Mode(Mode::Ball)).unwrap_or(0);
                        }
                    }
                    _ => {}
                }
            }
            Screen::TagModes { cursor, difficulty, .. } => {
                let n = crate::tagrun::TagMode::ALL.len();
                // (as the extra levels: square / triangle / START pick, cross / circle go back)
                let key = if matches!(key, Key::Button(2 | 3)) { Key::Back } else { key };
                match key {
                    Key::Up => {
                        sfx::play(sfx::cursor());
                        *cursor = (*cursor + n - 1) % n;
                    }
                    Key::Down => {
                        sfx::play(sfx::cursor());
                        *cursor = (*cursor + 1) % n;
                    }
                    Key::Start | Key::Button(_) => {
                        sfx::play(sfx::confirm());
                        let mode = crate::tagrun::TagMode::ALL[*cursor];
                        *self = Screen::Tag(Box::new(crate::tagrun::TagSelect::new(roster, *difficulty, *seed, mode)));
                    }
                    Key::Back => {
                        sfx::play(sfx::back());
                        let d = *difficulty;
                        *self = Screen::menu(d);
                        if let Screen::Menu(m) = self {
                            m.cursor = shown_items(roster).iter().position(|i| i.1 == Entry::Tag).unwrap_or(0);
                        }
                    }
                    _ => {}
                }
            }
            Screen::Grid(g) => match g.key(player, key, roster) {
                Outcome::Quit => {
                    let d = g.difficulty;
                    *self = Screen::menu(d);
                }
                Outcome::None => {}
                o => return o,
            },
            Screen::Results(r) => {
                if key == Key::Start {
                    r.skip();
                }
            }
            // the app steps the records screen (START, the name entry)
            Screen::Records(r) => r.key(if key == Key::Back { Key::Start } else { key }),
            Screen::ForceResults(r) => {
                if key == Key::Start && r.fade >= 256 {
                    r.leaving = true;
                }
            }
            // START skips a movie
            Screen::Movie(_, next) => {
                if key == Key::Start {
                    *self = after_movie(next.clone(), roster);
                }
            }
            Screen::Select(s) => {
                if s.key(player, key, roster) {
                    let d = s.difficulty;
                    // the extra levels' select backs out to its list
                    *self = match (s.mode, s.stage) {
                        (Mode::ExtraLevels, Some(stage)) => Screen::extra_levels(roster, stage, d),
                        _ => Screen::menu(d),
                    };
                }
            }
            // START continues; a face button hurries the count (0x8005008C)
            Screen::Continue { setup, ladder, countdown } => match key {
                Key::Start => {
                    sfx::play(sfx::start());
                    return Outcome::Fight(setup.clone(), ladder.clone());
                }
                Key::Button(_) => *countdown = countdown.saturating_sub(CONTINUE_STEP).max(1),
                Key::Back => *self = Screen::GameOver { frames: 0 },
                _ => {}
            },
            Screen::GameOver { .. } | Screen::Versus { .. } => {}
            Screen::Theater(t) => return Outcome::Theater(t.key(key)),
            Screen::StaffRoll(r) => r.key(key),
        }
        Outcome::None
    }

    /// Per 60 Hz frame: animations, countdowns, and the select screen's start of the fight.
    pub fn tick(&mut self, roster: &Roster, seed: &mut u32) -> Outcome {
        match self {
            Screen::Title { frame, .. } => {
                *frame += 1;
                if *frame >= TITLE_END {
                    // the demo fight (the app picks it: the attract loop's state)
                    *frame = 0;
                    return Outcome::Demo;
                }
            }
            Screen::Menu(m) => {
                m.frame += 1;
                m.idle += 1;
                m.scroll -= m.scroll.signum() * ((m.scroll.abs() + 3) >> 2);
                if m.scroll == 0 {
                    m.bar = (m.bar + 24).min(256);
                }
                if m.idle >= MENU_IDLE {
                    m.idle = 0;
                    return Outcome::Attract;
                }
            }
            Screen::Presents { frame, next } => {
                *frame += 1;
                if *frame >= PRESENTS_END {
                    return Outcome::AttractNext(*next);
                }
            }
            Screen::Options { frame, .. } | Screen::ExtraLevels { frame, .. } | Screen::TagModes { frame, .. } | Screen::BallModes { frame, .. } => *frame += 1,
            Screen::Room(r) => r.frame += 1,
            Screen::Online(o) => o.tick(),
            Screen::Graphics(g) => g.frame += 1,
            Screen::Keyboard(k) => k.frame += 1,
            Screen::KeyConfig(k) => k.tick(),
            Screen::Select(s) => return s.tick(roster, seed),
            Screen::Grid(g) => return g.tick(roster, seed),
            Screen::Tag(t) => t.tick(),
            Screen::Continue { countdown, .. } => {
                *countdown = countdown.saturating_sub(1);
                if *countdown == 0 {
                    *self = Screen::GameOver { frames: 0 };
                }
            }
            // the VS screen: 8 frames of slide, then it stays until 60 frames have passed (and the fight loads)
            Screen::Versus { setup, ladder, frame, .. } => {
                *frame += 1;
                if *frame >= roster.ui.vs.hold {
                    return Outcome::Load(setup.clone(), ladder.clone());
                }
            }
            Screen::GameOver { frames } => {
                *frames += 1;
                if *frames >= GAME_OVER_FRAMES {
                    *self = Screen::title();
                }
            }
            Screen::Theater(t) => t.tick(),
            // the app steps the staff roll (its sound, music and the records after it)
            Screen::StaffRoll(_) => {}
            // the app steps the result screens (their sounds and the records after them)
            Screen::Results(_) | Screen::ForceResults(_) | Screen::Records(_) => {}
            Screen::Movie(m, next) => {
                if m.tick() {
                    *self = after_movie(next.clone(), roster);
                }
            }
        }
        Outcome::None
    }

    pub fn draw(&self, ctx: &egui::Context, art: &Art, roster: &Roster) {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("menu")));
        let window = ctx.content_rect();
        // CONTINUE? and GAME OVER draw over the fight's loser shot
        if !matches!(self, Screen::Continue { .. } | Screen::GameOver { .. } | Screen::Records(_)) {
            painter.rect_filled(window, 0.0, Color32::BLACK);
        }
        let v = View::fit(window);
        let painter = painter.with_clip_rect(v.rect);
        match self {
            Screen::Title { frame, .. } => draw_title(&painter, &v, art, roster, *frame),
            Screen::Presents { frame, .. } => draw_presents(&painter, &v, art, *frame),
            Screen::Menu(m) => draw_menu(&painter, &v, art, roster, m),
            Screen::Options { frame, cursor, difficulty } => draw_options(&painter, &v, art, *frame, *cursor, *difficulty),
            Screen::Select(s) => s.draw(&painter, &v, art, roster),
            Screen::Grid(g) => g.draw(&painter, &v, art, roster),
            Screen::Tag(t) => t.draw(&painter, &v, art, roster),
            Screen::ExtraLevels { frame, cursor, .. } => {
                let names: Vec<String> = roster.extra_levels.iter().map(|(_, n)| level_title(n)).collect();
                draw_list(&painter, &v, art, &roster.extra_levels_title, &names, *frame, *cursor);
            }
            Screen::Online(o) => o.draw(&painter, &v, art),
            Screen::Room(r) => r.draw(&painter, &v, art),
            Screen::TagModes { frame, cursor, .. } => {
                let names: Vec<String> = crate::tagrun::TagMode::ALL.iter().map(|m| m.name(&art.screens)).collect();
                draw_list(&painter, &v, art, "TAG BRAWL", &names, *frame, *cursor);
            }
            Screen::BallModes { frame, cursor, .. } => {
                let ball = art.screens["menu"]["ball"].as_str().unwrap_or_default();
                draw_list(&painter, &v, art, ball, &["1 ON 1".to_string(), "2 ON 2".to_string()], *frame, *cursor);
            }
            Screen::Graphics(g) => g.draw(&painter, &v, art),
            Screen::KeyConfig(k) => k.draw(&painter, &v, art),
            Screen::Keyboard(k) => k.draw(&painter, &v, art),
            // "CONTINUE? n" (0x8004FFF0, printf 0x80050168) and GAME OVER (0x80050BDC, of 0x80050428) in the big font
            Screen::Continue { countdown, .. } => {
                let text = roster.ui.continue_text.replace("%d", &countdown.div_ceil(CONTINUE_STEP).to_string());
                let t = crate::modes::Text::from(&art.screens["hud"]["continue"]);
                art.text(&painter, &v, t.font, t.colour, t.x, t.y, &text, Color32::WHITE);
            }
            Screen::GameOver { .. } => {
                let t = crate::modes::Text::from(&art.screens["hud"]["continue"]["game_over"]);
                art.text(&painter, &v, t.font, t.colour, t.x, t.y, &roster.ui.game_over, Color32::WHITE)
            }
            Screen::Versus { setup, frame, number, extra, .. } => draw_versus(&painter, &v, art, roster, setup, *frame, *number, extra),
            Screen::Results(r) => r.draw(&painter, &v, art, roster),
            Screen::ForceResults(r) => r.draw(&painter, &v, art, roster),
            Screen::Records(r) => r.draw(&painter, &v, art, roster),
            Screen::Movie(m, _) => m.draw(ctx, &painter, v.rect),
            Screen::Theater(t) => t.draw(&painter, &v, art),
            Screen::StaffRoll(r) => r.draw(&painter, &v, art),
        }
    }
}

/// The screen after a movie.
fn after_movie(next: AfterMovie, roster: &Roster) -> Screen {
    match next {
        AfterMovie::Title => Screen::Title { frame: WHITE_HOLD, difficulty: difficulty() },
        AfterMovie::Credits => match &roster.theater {
            Some(t) => Screen::StaffRoll(Box::new(crate::theater::StaffRoll::new(t.clone()))),
            None => Screen::title(),
        },
        AfterMovie::Theater(t) => Screen::Theater(t),
    }
}

/// The title picture, six 8-bit strips (record 10 VRAM, screens.json `title`): in its own colours, or the options
/// tint.
pub fn title_picture(painter: &egui::Painter, v: &View, art: &Art, options: bool) {
    for s in art.screens["title"][if options { "strips_options" } else { "strips" }].as_array().into_iter().flatten() {
        let (t, w, h, x, y) = Tex::sprite(s);
        art.sprite(painter, v, &["r010"], &t, w, h, x, y, Color32::WHITE);
    }
}

/// The original hardware's triangle wave 0..255..0 of `a` (0x8004DFE0); called with frame * 16, one period is 32
/// frames.
pub fn triangle(a: u32) -> u32 {
    let a = a & 0x1FF;
    if a >= 256 {
        511 - a
    } else {
        a
    }
}

fn blink(frame: u32) -> bool {
    frame & 0x30 != 0
}

/// the PRESENTS screen on black (0x8004FA60; screens.json title.presents: its pieces in their colours, the
/// font and place); it fades in and out under a box over the screen (grey 255 - 8k, then 8k).
fn draw_presents(painter: &egui::Painter, v: &View, art: &Art, frame: u32) {
    if frame >= PRESENTS_OUT {
        return;
    }
    let t = &art.screens["title"]["presents"];
    let (font, mut x, y) = (t["font"].as_u64().unwrap_or(0) as usize, t["x"].as_f64().unwrap_or(0.0) as f32, t["y"].as_f64().unwrap_or(0.0) as f32);
    for (part, colour) in t["parts"].as_array().into_iter().flatten().zip(t["colours"].as_array().into_iter().flatten()) {
        let part = part.as_str().unwrap_or_default();
        art.text(painter, v, font, colour.as_u64().unwrap_or(0) as u32, x, y, part, Color32::WHITE);
        x += art.text_w(font, part);
    }
    let dark = if frame < PRESENTS_IN {
        255 - 8 * frame
    } else if frame >= PRESENTS_HOLD {
        8 * (frame - PRESENTS_HOLD)
    } else {
        0
    };
    if dark > 0 {
        Art::shade(painter, v, crate::art::FULL, [Color32::from_black_alpha(dark.min(255) as u8); 4]);
    }
}

fn draw_title(painter: &egui::Painter, v: &View, art: &Art, roster: &Roster, frame: u32) {
    let full = crate::art::FULL;
    if frame < WHITE_HOLD {
        let level = (frame * 10).min(255) as u8;
        Art::shade(painter, v, full, [Color32::from_gray(level); 4]);
        return;
    }
    title_picture(painter, v, art, false);
    if frame < WHITE_OUT {
        let level = 255 - (frame - WHITE_HOLD) * 3;
        Art::shade(painter, v, full, [additive(Color32::from_gray(level.min(255) as u8)); 4]);
        return;
    }
    // the publisher's logo, copyright, PUSH START (48 frames on, 16 off)
    let (t, w, h, x, y) = Tex::sprite(&art.screens["title"]["logo"]);
    art.sprite(painter, v, &["r000", "r010"], &t, w, h, x, y, Color32::WHITE);
    for (text, x, y, colour) in &roster.ui.copyright {
        art.text(painter, v, 0, *colour, *x, *y, text, Color32::WHITE);
    }
    if blink(frame) {
        let (s, p) = (&roster.ui.push_start, &art.screens["title"]["push_start"]);
        let font = p["font"].as_u64().unwrap_or(0) as usize;
        let (colour, y) = (p["colour"].as_u64().unwrap_or(0) as u32, p["y"].as_f64().unwrap_or(0.0) as f32);
        art.text(painter, v, font, colour, crate::art::CENTRE - art.text_w(font, s) / 2.0, y, s, Color32::WHITE);
    }
    if frame > TITLE_HOLD {
        let a = ((frame - TITLE_HOLD) * 8).min(255) as u8;
        Art::shade(painter, v, full, [Color32::from_black_alpha(a); 4]);
    }
}

/// The main menu's highlight bar (screens.json `menu.bar`, 0x800DBD60) with its top at `y`, at brightness `b` (of
/// 256): two gradients to the middle, a line along its top and one lower.
fn highlight(painter: &egui::Painter, v: &View, art: &Art, y: f32, b: u32) {
    let bar = &art.screens["menu"]["bar"];
    let f = num;
    let grey = |c: &Value| Color32::from_gray((c[0].as_u64().unwrap_or(0) as u32 * b / 256) as u8);
    let (mid, line) = (grey(&bar["rgb"]), grey(&bar["line_rgb"]));
    let ([x0, xm, x1], top, h, right) = ([0, 1, 2].map(|k| f(&bar["x"][k])), f(&bar["y"][0]), f(&bar["y"][1]) - f(&bar["y"][0]), f(&bar["right"]));
    Art::shade(painter, v, [x0, y, xm - x0, h], [Color32::BLACK, mid, Color32::BLACK, mid]);
    Art::shade(painter, v, [xm, y, x1 - xm, h], [mid, Color32::BLACK, mid, Color32::BLACK]);
    for l in bar["lines"].as_array().into_iter().flatten() {
        let (ly, lh) = (y + f(&l[0]) - top, f(&l[1]) - f(&l[0]));
        Art::shade(painter, v, [0.0, ly, xm, lh], [Color32::BLACK, line, Color32::BLACK, line]);
        Art::shade(painter, v, [xm, ly, right - xm, lh], [line, Color32::BLACK, line, Color32::BLACK]);
    }
}

fn draw_menu(painter: &egui::Painter, v: &View, art: &Art, roster: &Roster, m: &Menu) {
    title_picture(painter, v, art, false);
    let items = shown_items(roster);
    let n = items.len() as i32;
    let d = &art.screens["menu"];
    let f = num;
    // highlight bar under the selected item (only while the list stands still)
    if m.scroll == 0 && m.bar > 0 {
        highlight(painter, v, art, f(&d["bar"]["y"][0]), m.bar);
    }
    let c = &d["clip"];
    let clip = painter.with_clip_rect(v.rect(f(&c[0]), f(&c[1]), f(&c[2]), f(&c[3])));
    // the selected row `from` below the first, as many rows after it
    let (from, font) = (d["from"].as_i64().unwrap_or(0) as i32, d["font"].as_u64().unwrap_or(0) as usize);
    // (no more rows than the items)
    for j in 0..(2 * from + 1).min(n) {
        let k = (m.cursor as i32 + j - from).rem_euclid(n) as usize;
        let (item, _) = &items[k];
        let x = f(&d["centre_x"]) - (art.text_w(font, &item.name) as i32 / 2) as f32;
        let y = f(&d["first_y"]) + f(&d["row_height"]) * j as f32 + m.scroll as f32;
        art.text(&clip, v, font, item.colour, x, y, &item.name, Color32::WHITE);
    }
    // the list fades out at the clip edges (subtractive in the original)
    for (k, fade) in d["fades"].as_array().into_iter().flatten().enumerate() {
        let r = &fade["rect"];
        let dark = Color32::from_black_alpha(fade["rgb"][0].as_u64().unwrap_or(0) as u8);
        let cs = if k == 0 { [dark, dark, Color32::TRANSPARENT, Color32::TRANSPARENT] } else { [Color32::TRANSPARENT, Color32::TRANSPARENT, dark, dark] };
        Art::shade(painter, v, [f(&r[0]), f(&r[1]), f(&r[2]) - f(&r[0]), f(&r[3]) - f(&r[1])], cs);
    }
}

/// A level's name in the menu font (capitals, no underscores): "level_name" -> "LEVEL NAME".
pub fn level_title(name: &str) -> String {
    name.replace('_', " ").to_uppercase()
}

/// the extra levels: the title picture, the heading and the levels (five at a time), the chosen one on the menu's
/// highlight bar.
/// OpenKumite's own lists over the title picture (the extra levels, TAG BRAWL's modes): a heading, the entries.
fn draw_list(painter: &egui::Painter, v: &View, art: &Art, head: &str, entries: &[String], frame: u32, cursor: usize) {
    title_picture(painter, v, art, false);
    art.text(painter, v, 1, 5, crate::art::CENTRE - art.text_w(1, head) / 2.0, 300.0, head, Color32::WHITE);
    // five rows show; the list scrolls with the cursor
    const ROWS: usize = 5;
    let first = cursor.saturating_sub(ROWS / 2).min(entries.len().saturating_sub(ROWS));
    for (k, text) in entries.iter().enumerate().skip(first).take(ROWS) {
        let y = 340.0 + 28.0 * (k - first) as f32;
        if k == cursor {
            highlight(painter, v, art, y - 4.0, frame.min(10) * 25);
        }
        art.text(painter, v, 1, if k == cursor { 9 } else { 0 }, crate::art::CENTRE - art.text_w(1, text) / 2.0, y, text, Color32::WHITE);
    }
}

/// OPTION > GRAPHICS: display mode, resolution, render scale, vsync, textures; Left / Right change the row's
/// value (applied at once), EXIT or BACK go back to OPTION.
pub struct GraphicsMenu {
    pub frame: u32,
    pub cursor: usize,
    pub settings: crate::graphics::Graphics,
    /// What the display and the GPU allow: the resolutions and render scales offered.
    pub display: crate::graphics::Display,
    pub difficulty: usize,
    done: bool,
}

const GRAPHICS_ROWS: usize = 9;

impl GraphicsMenu {
    pub fn new(settings: crate::graphics::Graphics, display: crate::graphics::Display, difficulty: usize) -> Self {
        Self { frame: 0, cursor: 0, settings, display, difficulty, done: false }
    }

    fn rows(&self) -> [(&'static str, Option<String>); GRAPHICS_ROWS] {
        use crate::graphics::DisplayMode;
        let g = &self.settings;
        let res = match (g.display, self.display.screen) {
            (DisplayMode::Borderless, Some([w, h])) => format!("{w}X{h}"),
            (DisplayMode::Borderless, None) => "DESKTOP".to_string(),
            _ => format!("{}X{}", g.resolution[0], g.resolution[1]),
        };
        // (in a browser the browser's own)
        let vsync = if cfg!(target_arch = "wasm32") {
            "AUTO"
        } else if g.vsync {
            "ON"
        } else {
            "OFF"
        };
        [
            ("DISPLAY", Some(g.display.label().to_string())),
            ("RESOLUTION", Some(res)),
            ("3D SCALE", Some(format!("{}X", g.scale))),
            ("VSYNC", Some(vsync.to_string())),
            ("TEXTURES", Some(if g.smooth_textures { "SMOOTH" } else { "SHARP" }.to_string())),
            ("STATS", Some(if g.stats { "ON" } else { "OFF" }.to_string())),
            ("RECOMMENDED", None),
            ("DEFAULT", None),
            ("EXIT", None),
        ]
    }

    /// A key: the setting changed (to apply), or None.
    pub fn key(&mut self, key: Key) -> Option<Outcome> {
        use crate::graphics::SCALES;
        let step = |i: usize, n: usize, right: bool| if right { (i + 1) % n } else { (i + n - 1) % n };
        match key {
            Key::Up => {
                sfx::play(sfx::cursor());
                self.cursor = step(self.cursor, GRAPHICS_ROWS, false);
            }
            Key::Down => {
                sfx::play(sfx::cursor());
                self.cursor = step(self.cursor, GRAPHICS_ROWS, true);
            }
            Key::Back => {
                sfx::play(sfx::back());
                self.done = true;
            }
            Key::Start | Key::Button(_) if self.cursor == GRAPHICS_ROWS - 1 => {
                sfx::play(sfx::back());
                self.done = true;
            }
            // RECOMMENDED / DEFAULT: the settings chosen for the screen and the GPU, or the defaults
            Key::Start | Key::Button(_) if matches!(self.cursor, 6 | 7) => {
                let p = if self.cursor == 6 { crate::graphics::Preset::Recommended } else { crate::graphics::Preset::Default };
                self.settings = self.display.preset_over(p, &self.settings);
                sfx::play(sfx::confirm());
                return Some(Outcome::Graphics(self.settings.clone()));
            }
            Key::Left | Key::Right | Key::Start | Key::Button(_) => {
                let right = key != Key::Left;
                let g = &mut self.settings;
                match self.cursor {
                    0 => {
                        let modes = self.display.display_modes();
                        let i = modes.iter().position(|d| *d == g.display).unwrap_or(0);
                        g.display = modes[step(i, modes.len(), right)];
                        // a resolution and a render scale the new mode can show
                        *g = self.display.fit(g.clone());
                    }
                    1 => {
                        let list = self.display.resolutions(g.display);
                        if list.len() < 2 {
                            return None;
                        }
                        let i = list.iter().position(|r| *r == g.resolution).unwrap_or(0);
                        g.resolution = list[step(i, list.len(), right)];
                        *g = self.display.fit(g.clone());
                    }
                    2 => {
                        let scales: Vec<f32> = SCALES.iter().copied().filter(|&s| self.display.scale_fits(g, s)).collect();
                        if scales.len() < 2 {
                            return None;
                        }
                        let i = scales.iter().position(|s| (s - g.scale).abs() < 1e-3).unwrap_or(0);
                        g.scale = scales[step(i, scales.len(), right)];
                    }
                    3 if cfg!(target_arch = "wasm32") => return None,
                    3 => g.vsync = !g.vsync,
                    4 => g.smooth_textures = !g.smooth_textures,
                    5 => g.stats = !g.stats,
                    _ => return None,
                }
                sfx::play(sfx::cursor());
                return Some(Outcome::Graphics(self.settings.clone()));
            }
        }
        None
    }

    pub fn draw(&self, painter: &egui::Painter, v: &View, art: &Art) {
        title_picture(painter, v, art, true);
        art.text(painter, v, 1, 6, crate::art::CENTRE - art.text_w(1, "GRAPHICS") / 2.0, 84.0, "GRAPHICS", Color32::WHITE);
        for (k, (label, value)) in self.rows().iter().enumerate() {
            // (the actions a little apart from the settings)
            let y = 136.0 + 32.0 * k as f32 + if k >= 6 { 10.0 } else { 0.0 };
            let colour = if k == self.cursor { 5 } else { 6 };
            match value {
                Some(val) => {
                    art.text(painter, v, 1, colour, 20.0, y, label, Color32::WHITE);
                    let x = 270.0 - art.text_w(1, val) / 2.0;
                    art.text(painter, v, 1, colour, x, y, val, Color32::WHITE);
                    if k == self.cursor {
                        let s = triangle((self.frame & 31) << 4) as f32 / 32.0;
                        let ([left, right], [aw, ah]) = arrows(&art.screens["select"]);
                        let w = art.text_w(1, val);
                        art.sprite(painter, v, &["exe", "r009"], &left, aw, ah, x - 16.0 - s, y + 4.0, Color32::WHITE);
                        art.sprite(painter, v, &["exe", "r009"], &right, aw, ah, x + w + 4.0 + s, y + 4.0, Color32::WHITE);
                    }
                }
                None => art.text(painter, v, 1, colour, crate::art::CENTRE - art.text_w(1, label) / 2.0, y, label, Color32::WHITE),
            }
        }
    }
}

fn draw_options(painter: &egui::Painter, v: &View, art: &Art, frame: u32, cursor: usize, difficulty: usize) {
    // the options background: the title picture with its dark-blue CLUTs (rows 500-505)
    title_picture(painter, v, art, true);
    art.text(painter, v, 1, 6, crate::art::CENTRE - art.text_w(1, "OPTION") / 2.0, 120.0, "OPTION", Color32::WHITE);
    let rows = [("DIFFICULTY", Some(DIFFICULTIES[difficulty])), ("GRAPHICS", None), ("KEY CONFIGURATION", None), ("KEYBOARD", None), ("EXIT", None)];
    for (k, (label, value)) in rows.iter().enumerate() {
        let y = 220.0 + 48.0 * k as f32;
        let colour = if k == cursor { 5 } else { 6 };
        match value {
            Some(val) => {
                art.text(painter, v, 1, colour, 40.0, y, label, Color32::WHITE);
                let x = 300.0 - art.text_w(1, val) / 2.0;
                art.text(painter, v, 1, colour, x, y, val, Color32::WHITE);
                if k == cursor {
                    // left / right arrows (the select screen's edge arrows), bobbing
                    let s = triangle((frame & 31) << 4) as f32 / 32.0;
                    let ([left, right], [aw, ah]) = arrows(&art.screens["select"]);
                    let w = art.text_w(1, val);
                    art.sprite(painter, v, &["exe", "r009"], &left, aw, ah, x - 18.0 - s, y + 4.0, Color32::WHITE);
                    art.sprite(painter, v, &["exe", "r009"], &right, aw, ah, x + w + 6.0 + s, y + 4.0, Color32::WHITE);
                }
            }
            None => art.text(painter, v, 1, colour, crate::art::CENTRE - art.text_w(1, label) / 2.0, y, label, Color32::WHITE),
        }
    }
}

// ---------------------------------------------------------------- character select (record 9)

impl Select {
    pub fn new(mode: Mode, difficulty: usize, roster: &Roster, seed: u32) -> Self {
        let side = |k: usize, state| {
            let slot = roster.ui.sides[k].2;
            Side { state, slot, costume: 0, fade: 28 }
        };
        let p2 = match mode {
            Mode::Versus => SideState::Choosing,
            Mode::Practice | Mode::TimeAttack | Mode::Survival | Mode::Force | Mode::ExtraLevels => SideState::Label,
            _ => SideState::Off,
        };
        let arrows = if roster.hidden_slot(0).is_some() || roster.hidden_slot(1).is_some() { 180 } else { 0 };
        let mut s = Self {
            mode,
            difficulty,
            sides: [side(0, SideState::Choosing), side(1, p2)],
            frame: 0,
            timer: SELECT_SECONDS * 60,
            done: 0,
            arrows,
            streaks: Vec::new(),
            rng: seed | 1,
            stage: None,
            label: String::new(),
            page: 0,
        };
        for k in 0..2 {
            s.sides[k].slot = s.valid_slot(roster, s.sides[k].slot);
        }
        s
    }

    /// A slot with a character on it (screens.json's default slots hold one each).
    fn valid_slot(&self, roster: &Roster, slot: usize) -> usize {
        if roster.ui.slots.get(slot).is_some_and(|c| roster.by_id(*c).is_some()) {
            return slot;
        }
        roster.row_slots(0).first().or(roster.row_slots(1).first()).copied().unwrap_or(0)
    }

    /// The character (roster index) on `slot`.
    fn character(&self, roster: &Roster, slot: usize) -> Option<usize> {
        roster.ui.slots.get(slot).and_then(|c| roster.index_of(*c))
    }

    /// Which side a player's keys move: in practice P1 also picks the dummy.
    fn side_of(&self, player: usize) -> usize {
        if matches!(self.mode, Mode::Practice | Mode::ExtraLevels) && self.sides[1].state == SideState::CpuChoosing {
            1
        } else {
            player.min(1)
        }
    }

    fn move_cursor(&mut self, k: usize, key: Key, roster: &Roster) {
        let s = &mut self.sides[k];
        let row = roster.row_of(s.slot);
        let slots = roster.row_slots(row);
        let at = slots.iter().position(|&x| x == s.slot);
        let hidden = roster.hidden_slot(row);
        let before = s.slot;
        match key {
            Key::Left | Key::Right => {
                // off either edge onto the hidden slot (when it is there), then round to the other edge
                let right = key == Key::Right;
                let mut ring: Vec<usize> = slots.clone();
                if let Some(h) = hidden {
                    ring.push(h);
                }
                let i = at.unwrap_or(ring.len() - 1);
                let n = ring.len();
                s.slot = ring[if right { (i + 1) % n } else { (i + n - 1) % n }];
            }
            Key::Up | Key::Down => {
                // the next row with faces, round through both pages (two rows a page)
                let n = roster.rows();
                let step = if key == Key::Down { 1 } else { n - 1 };
                let other = (1..n).map(|d| (row + step * d) % n).find(|&r| !roster.row_slots(r).is_empty()).unwrap_or(row);
                let target = roster.row_slots(other);
                if !target.is_empty() && other != row {
                    let i = at.unwrap_or(slots.len()).min(target.len() - 1);
                    s.slot = if at.is_none() { roster.hidden_slot(other).unwrap_or(target[i]) } else { target[i] };
                }
            }
            _ => {}
        }
        if s.slot != before {
            sfx::play(sfx::select_cursor());
            s.fade = 28;
            s.costume = 0;
            let slot = s.slot;
            self.page = roster.row_of(slot) / 2;
        }
    }

    /// Pick the character under side `k`'s cursor with costume `costume`.
    /// The model the other side has chosen (roster index, costume), if it has.
    fn chosen_by(&self, k: usize, roster: &Roster) -> Option<(usize, usize)> {
        let o = &self.sides[1 - k];
        matches!(o.state, SideState::Chosen | SideState::CpuChosen).then(|| self.character(roster, o.slot).map(|i| (i, o.costume))).flatten()
    }

    /// Pick the character under side `k`'s cursor with costume `costume`, or the next costume the other side has
    /// not taken; false when it has taken them all (no model shows twice).
    fn pick(&mut self, k: usize, costume: usize, roster: &Roster) -> bool {
        let Some(idx) = self.character(roster, self.sides[k].slot) else { return false };
        let other = self.chosen_by(k, roster);
        let Some(costume) = free_costume(roster.characters[idx].costumes.len(), costume, |c| other == Some((idx, c))) else {
            sfx::play(sfx::back());
            return false;
        };
        sfx::play(sfx::pick());
        let s = &mut self.sides[k];
        s.costume = costume;
        s.state = if s.state == SideState::CpuChoosing { SideState::CpuChosen } else { SideState::Chosen };
        if matches!(self.mode, Mode::Practice | Mode::ExtraLevels) && k == 0 {
            // then P1 picks the dummy (the extra levels: the CPU opponent)
            self.sides[1].state = SideState::CpuChoosing;
            self.page = roster.row_of(self.sides[1].slot) / 2;
            self.sides[1].fade = 28;
            self.timer = SELECT_SECONDS * 60;
        }
        true
    }

    /// Handle a key; true = back out to the main menu.
    fn key(&mut self, player: usize, key: Key, roster: &Roster) -> bool {
        let k = self.side_of(player);
        if self.done > 0 {
            return false;
        }
        match (self.sides[k].state, key) {
            (SideState::Choosing | SideState::CpuChoosing, Key::Left | Key::Right | Key::Up | Key::Down) => self.move_cursor(k, key, roster),
            (SideState::Choosing | SideState::CpuChoosing, Key::Button(_) | Key::Start) => {
                if let Some(c) = self.character(roster, self.sides[k].slot).and_then(|i| roster.characters[i].costume_for(key)) {
                    self.pick(k, c, roster);
                }
            }
            // SELECT on the pad choosing for the other side takes it back (0x8010E19C); the screen itself has no
            // way back (only the soft reset, START + SELECT)
            (SideState::CpuChoosing, Key::Back) => {
                self.sides[1].state = SideState::Label;
                self.sides[0].state = SideState::Choosing;
            }
            _ => {}
        }
        false
    }

    /// Everyone who plays has chosen.
    fn all_chosen(&self) -> bool {
        self.sides.iter().all(|s| !s.choosing())
    }

    fn tick(&mut self, roster: &Roster, seed: &mut u32) -> Outcome {
        self.frame += 1;
        self.arrows = self.arrows.saturating_sub(1);
        for s in &mut self.sides {
            s.fade = s.fade.saturating_sub(1);
        }
        // time out: whoever is still choosing takes the character under the cursor
        if self.sides.iter().any(Side::choosing) {
            self.timer = self.timer.saturating_sub(1);
            if self.timer == 0 {
                for k in 0..2 {
                    if self.sides[k].choosing() && !self.pick(k, 0, roster) {
                        // every costume under the cursor is the other side's: the first slot with one left
                        let slots: Vec<usize> = (0..roster.rows()).flat_map(|r| roster.row_slots(r)).collect();
                        for slot in slots {
                            self.sides[k].slot = slot;
                            if self.pick(k, 0, roster) {
                                break;
                            }
                        }
                    }
                }
                self.timer = SELECT_SECONDS * 60;
            }
        }
        self.streak_step(roster);
        if !self.all_chosen() {
            return Outcome::None;
        }
        self.done += 1;
        if self.done < SELECT_OUTRO {
            return Outcome::None;
        }
        let pick = |k: usize| (self.character(roster, self.sides[k].slot).unwrap_or(0), self.sides[k].costume);
        let p1 = pick(0);
        if self.mode == Mode::Arcade {
            let ladder = arcade_ladder(roster, roster.characters[p1.0].id, seed);
            return Outcome::Fight(arcade_setup(roster, p1, self.difficulty, &ladder, 0), ladder);
        }
        match self.mode {
            Mode::TimeAttack => return Outcome::Run(Box::new(crate::modes::Run::time_attack(roster, p1, seed))),
            Mode::Survival => return Outcome::Run(Box::new(crate::modes::Run::survival(p1))),
            Mode::Force => return Outcome::StartForce(p1, self.difficulty),
            _ => {}
        }
        let p2 = pick(1);
        let stage = self.stage.unwrap_or_else(|| roster.home_stage(roster.characters[p2.0].id, p2.1));
        Outcome::Fight(FightSetup { mode: self.mode, players: [p1, p2], stage, difficulty: self.difficulty, level: 5, handicap: [100; 2], ball: None, partners: None }, Vec::new())
    }

    /// Light streaks (0x8010EB94): one every few frames on alternating sides, rising, dimming.
    fn streak_step(&mut self, roster: &Roster) {
        let rand = |rng: &mut u32, n: u32| {
            *rng = rng.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (*rng >> 16) % n.max(1)
        };
        let [every, max, start, dim, dim_rand, len, _, rise, bands, band_w] = roster.ui.select.streaks;
        for s in &mut self.streaks {
            s.y -= rise as f32;
            s.b -= (dim + rand(&mut self.rng, dim_rand)) as i32;
        }
        self.streaks.retain(|s| s.b > 0 && s.y + len as f32 > 0.0);
        if self.frame.is_multiple_of(every.max(1)) && self.streaks.len() < max as usize {
            let k = ((self.frame / every.max(1)) & 1) as usize;
            if self.sides[k].shows() {
                let band = ((self.frame / (2 * every.max(1))) % bands.max(1)) as f32 * band_w as f32;
                let x = (band + rand(&mut self.rng, band_w) as f32).min(roster.ui.select.portrait_w - 1.0);
                self.streaks.push(Streak { side: k, x, y: 0.0, b: start as i32 });
            }
        }
    }

    /// Screen x, y of a slot's face (None for a hidden slot).
    fn face_pos(&self, roster: &Roster, slot: usize) -> Option<(f32, f32)> {
        let row = roster.row_of(slot);
        let slots = roster.row_slots(row);
        let k = slots.iter().position(|&s| s == slot)?;
        let l = &roster.ui.layouts[roster.layout().min(roster.ui.layouts.len() - 1)];
        // the original's rows hold up to a row's faces, a step apart; a longer row (the extra fighters) packs its
        // faces closer so it still fits the screen
        // (a row's slots but its hidden ones)
        let shown = roster.ui.per_row.saturating_sub(roster.ui.hidden.iter().filter(|&&h| h / roster.ui.per_row == row % 2).count());
        let ([x0, dx], n, fw) = (roster.ui.select.face_x, shown.max(2), roster.ui.face_size[0] as f32);
        let x = if slots.len() > n {
            x0 + (crate::art::SCREEN_W - 2.0 * x0 - fw) / (slots.len() - 1) as f32 * k as f32
        } else {
            x0 + dx * k as f32 + ((dx as usize * (n - slots.len().min(n))) / 2) as f32
        };
        Some((x, l.grid_y + (row % 2) as f32 * l.row_dy))
    }

    fn draw(&self, painter: &egui::Painter, v: &View, art: &Art, roster: &Roster) {
        let Some(l) = roster.ui.layouts.get(roster.layout()) else { return };
        let src: &[&str] = &["exe", "r009"];
        let ui = &roster.ui;
        let sp = &ui.select;
        let sw = crate::art::SCREEN_W;
        let split = l.split_y;
        let top = painter.with_clip_rect(v.rect(0.0, sp.top, sw, split - sp.top));
        // 1. black backdrop, the title pieces, the scrolling machinery wall (looping over its rows)
        Art::shade(painter, v, [0.0, 0.0, sw, split], [Color32::BLACK; 4]);
        // (the title pieces go into the ordering table in this order: the last is drawn first)
        for s in art.screens["select"]["title"].as_array().into_iter().flatten().rev() {
            let (t, w, h, x, y) = Tex::sprite(s);
            art.sprite(&top, v, src, &t, w, h, x, y, Color32::WHITE);
        }
        let [first_w, first_du, tile_w] = sp.wall;
        let period = (ui.wall_rows.len() as u32 * ui.wall_tile_h).max(1);
        let scroll = -(((self.frame * ui.wall_speed) % period) as f32);
        let mut y = scroll;
        let mut r = 0;
        while y < split && ui.wall_tile_h > 0 {
            if let Some(row) = ui.wall_rows.get(r % ui.wall_rows.len().max(1)) {
                let mut x = 0.0;
                for (c, &tile) in row.iter().enumerate() {
                    let Some(&(u, tv, clut)) = ui.wall_tiles.get(tile) else { continue };
                    let (w, du) = if c == 0 { (first_w, first_du) } else { (tile_w, 0) };
                    let t = Tex { page: ui.wall_page, bpp: 4, u: u + du, v: tv, clut: Some(clut) };
                    art.sprite(&top, v, src, &t, w, ui.wall_tile_h, x, y, Color32::WHITE);
                    x += w as f32;
                }
            }
            y += ui.wall_tile_h as f32;
            r += 1;
        }
        // 2. additive glow behind each shown portrait, in the character's colour
        for (k, s) in self.sides.iter().enumerate() {
            let Some(c) = self.shown(roster, k) else { continue };
            let p = roster.portrait(roster.characters[c].id, s.costume);
            let px = ui.sides[k].0;
            let glow = additive(Color32::from_rgb(p.glow[0], p.glow[1], p.glow[2]));
            let none = additive(Color32::BLACK);
            let [gw, gt, gm, gb] = sp.glow;
            Art::shade(&top, v, [px, l.portrait_y + gt, gw, gm - gt], [none, none, glow, glow]);
            let [r, g, b] = sp.glow_grey;
            let grey = additive(Color32::from_rgb(r, g, b));
            Art::shade(&top, v, [px, l.portrait_y + gm, gw, gb - gm], [glow, glow, grey, grey]);
        }
        // 3. the lower panel: a gradient (dark red at the bottom in the two-row layout) and its metal pieces
        let [r, g, b] = sp.panel_red;
        let bottom = if roster.layout() == 1 { Color32::from_rgb(r, g, b) } else { Color32::BLACK };
        let [pdy, pbottom, pw] = sp.panel;
        Art::shade(painter, v, [0.0, split, pw, pdy], [Color32::BLACK; 4]);
        Art::shade(painter, v, [0.0, split + pdy, pw, pbottom - split - pdy], [Color32::BLACK, Color32::BLACK, bottom, bottom]);
        for (d, vram, clut) in l.decorations.iter().rev() {
            art.sprite(painter, v, src, &Tex::at(*vram, 4, Some(*clut)), d[2] as u32, d[3] as u32, d[0], d[1], Color32::WHITE);
        }
        // 4. faces
        for row in self.page * 2..self.page * 2 + 2 {
            for slot in roster.row_slots(row) {
                let (Some((x, y)), Some(&id)) = (self.face_pos(roster, slot), ui.slots.get(slot)) else { continue };
                self.face(painter, v, art, roster, id, x, y);
            }
        }
        // 5. names, or what an empty side shows
        for (k, s) in self.sides.iter().enumerate() {
            let cx = ui.sides[k].1;
            match s.state {
                SideState::Off => {
                    if blink(self.frame) {
                        let text = art.screens["hud"]["under_bar"]["push"]["texts"][k].as_str().unwrap_or_default();
                        art.text(painter, v, 0, l.push_colour, cx - art.text_w(0, text) / 2.0, sp.push_y, text, Color32::WHITE);
                    }
                }
                SideState::Label => {
                    let text = match self.mode {
                        Mode::Practice => ui.practice_label.as_str(),
                        Mode::TimeAttack => ui.time_attack_label.as_str(),
                        Mode::Survival => ui.survival_label.as_str(),
                        Mode::Force => ui.force_label.as_str(),
                        Mode::ExtraLevels => self.label.as_str(),
                        _ => "",
                    };
                    let (font, colour, dy, [x0, x1]) = sp.label;
                    let w = art.text_w(font, text);
                    let x = (cx - w / 2.0).max(x0).min(x1 - w);
                    art.text(painter, v, font, colour, x, l.name_y + dy, text, Color32::WHITE);
                }
                _ => {
                    if let Some(c) = self.shown(roster, k) {
                        let p = roster.portrait(roster.characters[c].id, s.costume);
                        if p.name_w == 0 {
                            // an extra fighter: no cell in the names sheet, the name in the menu font
                            let text = roster.characters[c].name.to_uppercase();
                            let w = art.text_w(1, &text);
                            art.text(painter, v, 1, 5, cx - w / 2.0, l.name_y, &text, Color32::WHITE);
                            continue;
                        }
                        let n = p.name_cell;
                        let [cu, cv] = sp.names_cell;
                        let t = Tex { page: sp.names_page, bpp: 4, u: cu * ((n >> 4) & 1), v: cv * (n & 15), clut: Some(sp.names_clut) };
                        art.sprite(painter, v, src, &t, p.name_w, sp.names_h, cx - (p.name_w / 2) as f32, l.name_y, Color32::WHITE);
                    }
                }
            }
        }
        // 6. the timer (the HUD's digits)
        if self.sides.iter().any(Side::choosing) {
            let secs = self.timer.div_ceil(60).min(99) as usize;
            let digit = |d: usize, x: f32| {
                if let Some(t) = ui.timer_digits.get(d) {
                    art.sprite(painter, v, src, &Tex { clut: Some(l.timer_clut), ..*t }, ui.timer_size[0], ui.timer_size[1], x, l.timer_y, Color32::WHITE);
                }
            };
            digit(secs % 10, sp.timer_x);
            digit(secs / 10, sp.timer_x + ui.timer_step);
        }
        // 7. edge arrows while a hidden slot is there (bobbing 0..8 px, pulsing)
        if self.arrows > 0 && self.sides.iter().any(|s| s.choosing()) {
            let s = triangle((self.frame & 31) << 4) as f32 / 32.0;
            let g = triangle(self.frame << 4);
            let tint = modulate(g, g, g);
            let [w, h] = sp.arrow_size;
            art.sprite(painter, v, src, &sp.arrows[0], w, h, sp.arrow_x[0] - s, split + sp.arrow_dy, tint);
            art.sprite(painter, v, src, &sp.arrows[1], w, h, sp.arrow_x[1] + s, split + sp.arrow_dy, tint);
        }
        // 8. the big portraits, fading in over 28 frames after a change, darker at the top and bottom
        for (k, s) in self.sides.iter().enumerate() {
            let Some(c) = self.shown(roster, k) else { continue };
            let p = roster.portrait(roster.characters[c].id, s.costume);
            self.portrait(&top, v, art, roster, &p, k, s.fade, l.portrait_y);
        }
        // 9. light streaks, clipped above the names
        let streaks = painter.with_clip_rect(v.rect(0.0, sp.top, sw, l.name_y - sp.top));
        let (len, w) = (sp.streaks[5] as f32, sp.streaks[6] as f32);
        for st in &self.streaks {
            let x = ui.sides[st.side].0 + st.x;
            let b = st.b.clamp(0, 255) as u8;
            let y = l.name_y + st.y;
            Art::shade(&streaks, v, [x, y, w, len], [additive(Color32::from_gray(b)), additive(Color32::from_gray(b)), additive(Color32::from_rgb(0, 0, b)), additive(Color32::from_rgb(0, 0, b))]);
        }
        // 10. cursors: two halves each; a player's own half is on top, so a shared face is half red, half yellow
        let mut halves = Vec::new();
        for (k, s) in self.sides.iter().enumerate() {
            if !s.shows() {
                continue;
            }
            let Some((x, y)) = self.face_pos(roster, s.slot).filter(|_| roster.row_of(s.slot) / 2 == self.page) else { continue };
            let cpu = matches!(s.state, SideState::CpuChoosing | SideState::CpuChosen);
            let b = if s.choosing() { triangle(self.frame << 4) } else { 0x80 };
            let tint = modulate(b, b, b);
            let (uv, own_clut, tag_uv, cpu_tag_uv) = sp.players[k];
            let [u, cv] = uv[usize::from(roster.layout() == 1)];
            let clut = if cpu { sp.cpu_clut } else { own_clut };
            let frame = Tex { page: sp.cursor_page, bpp: 4, u, v: cv, clut: Some(clut) };
            let size = [sp.half_w, l.cursor_h];
            for half in 0..2u32 {
                let own = half as usize == k;
                halves.push((own, frame.offset(sp.half_w * half, 0), size, x + sp.cursor_dx + (sp.half_w * half) as f32, y + l.cursor_dy, tint));
            }
            let [tu, tv] = if cpu { cpu_tag_uv } else { tag_uv };
            halves.push((true, Tex { page: sp.tag_page, bpp: 4, u: tu, v: tv, clut: None }, sp.tag_size, x + sp.cursor_dx + ui.sides[k].3, y + l.tag_dy, tint));
        }
        for own in [false, true] {
            for (o, t, [w, h], x, y, tint) in &halves {
                if *o == own {
                    art.sprite(painter, v, src, t, *w, *h, *x, *y, *tint);
                }
            }
        }
    }

    /// The character a side shows (under its cursor or chosen).
    fn shown(&self, roster: &Roster, k: usize) -> Option<usize> {
        self.sides[k].shows().then(|| self.character(roster, self.sides[k].slot)).flatten()
    }

    #[allow(clippy::too_many_arguments)]
    fn face(&self, painter: &egui::Painter, v: &View, art: &Art, roster: &Roster, id: usize, x: f32, y: f32) {
        let src: &[&str] = &["exe", "r009"];
        let [fw, fh] = roster.ui.face_size;
        if art.extra_face(painter, v, roster, id, x, y, fh as f32) {
            return;
        }
        if roster.layout() == 0 {
            // one row: record 9's own faces
            let (w, h, faces) = &roster.ui.one_row_faces;
            if let Some(t) = faces.get(id) {
                art.sprite(painter, v, src, t, *w, *h, x, y, Color32::WHITE);
            }
            return;
        }
        let Some(t) = roster.ui.faces.get(id) else { return };
        if let Some(slices) = roster.ui.face_slices.get(&id) {
            // (a face in slices of a wider picture)
            for &[du, dy, h] in slices {
                art.sprite(painter, v, src, &t.offset(du, 0), fw, h, x, y + dy as f32, Color32::WHITE);
            }
        } else {
            art.sprite(painter, v, src, t, fw, fh, x, y, Color32::WHITE);
        }
    }

    /// A portrait in three bands with brightness keys at their borders (0x8010F140).
    #[allow(clippy::too_many_arguments)]
    fn portrait(&self, painter: &egui::Painter, v: &View, art: &Art, roster: &Roster, p: &PortraitInfo, k: usize, fade: u32, top: f32) {
        let Some(tex) = art.portrait(p.portrait) else { return };
        let ui = &roster.ui;
        let heights = ui.fade_heights.get(p.fade_type).or(ui.fade_heights.first()).copied().unwrap_or_default();
        let total = heights.iter().sum::<f32>().max(1.0);
        let keys = ui.fade_keys;
        let steps = ui.select.portrait_fade.max(1);
        let scale = (steps - fade.min(steps)) as f32 / steps as f32;
        let mirror = (k == 1) != p.flipped;
        let x = ui.sides[k].0;
        let borders = [0.0, heights[0], heights[0] + heights[1], total];
        // a band with no height leaves its upper key unused (types 2 and 3 start bright)
        let key = |i: usize| -> f32 {
            let raw = if i == 0 && heights[0] == 0.0 { keys[1] } else { keys[i] };
            (raw / 256.0 * scale).clamp(0.0, 1.0)
        };
        let mut mesh = egui::Mesh::with_texture(tex.id());
        for (i, by) in borders.iter().enumerate() {
            let b = (key(i) * 255.0) as u8;
            let col = Color32::from_rgba_premultiplied(b, b, b, b);
            let vv = by / total;
            let (u0, u1) = if mirror { (1.0, 0.0) } else { (0.0, 1.0) };
            mesh.vertices.push(egui::epaint::Vertex { pos: v.pos(x, top + by), uv: Pos2::new(u0, vv), color: col });
            mesh.vertices.push(egui::epaint::Vertex { pos: v.pos(x + ui.select.portrait_w, top + by), uv: Pos2::new(u1, vv), color: col });
        }
        for i in 0..3u32 {
            let a = 2 * i;
            mesh.indices.extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
        painter.add(egui::Shape::mesh(mesh));
    }
}

// ---------------------------------------------------------------- the VS screen (mode 11)

/// The game's selection (character * 4 + costume number) of a fight player.
fn selection(roster: &Roster, (idx, costume): (usize, usize)) -> (usize, usize) {
    let c = &roster.characters[idx.min(roster.characters.len() - 1)];
    let model = c.costumes[costume.min(c.costumes.len() - 1)];
    (c.id, roster.costume_of.get(&model).copied().unwrap_or(0))
}

/// The VS screen at `frame` (0x80052520): black while the portraits and names slide in (8 frames), then the
/// lightning panel, the mode colour bar, the mode text and VS pop in.
#[allow(clippy::too_many_arguments)]
fn draw_versus(painter: &egui::Painter, v: &View, art: &Art, roster: &Roster, setup: &FightSetup, frame: u32, number: u32, extra: &VsExtra) {
    let d = &roster.ui.vs;
    if frame < 2 {
        return;
    }
    let cnt = 9u32.saturating_sub(frame).min(7) as f32;
    let key = match setup.mode {
        Mode::Arcade => "arcade",
        Mode::Versus => "vs",
        Mode::Practice => "practice",
        Mode::Watch => "demo",
        Mode::TeamBattle => "team",
        Mode::TimeAttack => "time_attack",
        Mode::Survival => "survival",
        Mode::Ball => "ball",
        Mode::Force => "force",
        Mode::ExtraLevels => "arcade",
    };
    let Some(m) = d.modes.get(key) else { return };
    let humans = extra.humans;
    let by_humans = usize::from(humans >= 2);
    let (lightning, bar) = (m.lightning.unwrap_or(by_humans), m.bar.unwrap_or(by_humans));
    let slide = |t: [f32; 4]| (t[0] + ((cnt as i32 * t[2] as i32) >> 3) as f32, t[1] + ((cnt as i32 * t[3] as i32) >> 3) as f32);
    if cnt == 0.0 {
        // the lightning panel: a tile per cell, each stretched over its cell (all but the tile's last texel shown)
        if let Some((tex, [pw, ph])) = d.lightning.get(lightning).and_then(|f| art.pic(f)) {
            let (tile, shown) = (d.tile, d.tile - 1.0);
            for (r, ys) in d.rows.windows(2).enumerate() {
                for (k, xs) in d.columns.windows(2).enumerate() {
                    let (u0, v0) = (tile * k as f32, tile * r as f32);
                    let u1 = (u0 + shown).min(pw as f32);
                    let x1 = xs[0] + (xs[1] - xs[0]) * (u1 - u0) / shown;
                    let uv = Rect::from_min_max(Pos2::new(u0 / pw as f32, v0 / ph as f32), Pos2::new(u1 / pw as f32, (v0 + shown) / ph as f32));
                    painter.image(tex.id(), v.rect(xs[0], ys[0], x1 - xs[0], ys[1] - ys[0]), uv, Color32::WHITE);
                }
            }
        }
        // the mode colour bar: solid, then fading out to the right over 110 px ("lighten" in the original)
        if let (Some(&[r, g, b]), Some(&w)) = (d.bar_colours.get(bar), d.bar_widths.get(bar)) {
            let c = Color32::from_rgb(r, g, b);
            Art::shade(painter, v, [0.0, d.bar_y, w - d.bar_fade, d.bar_h], [c; 4]);
            Art::shade(painter, v, [w - d.bar_fade, d.bar_y, d.bar_fade, d.bar_h], [c, Color32::TRANSPARENT, c, Color32::TRANSPARENT]);
        }
    }
    // portraits (P2 under P1) in their frames, with the character's glow behind (Force: P1's only)
    let shown: &[usize] = if m.one_player { &[0] } else { &[1, 0] };
    for &k in shown {
        let (id, cos) = selection(roster, setup.players[k]);
        let p = roster.portrait(id, cos);
        let (x, y) = slide(m.portraits[k]);
        let glow = additive(Color32::from_rgb(p.glow[0], p.glow[1], p.glow[2]));
        let ([ix, iy, iw, ih], split, [r, g, b]) = (d.inner, d.glow_split, d.glow_bottom);
        let bottom = additive(Color32::from_rgb(r, g, b));
        Art::shade(painter, v, [x + ix, y + iy, iw, split - iy], [additive(Color32::BLACK), additive(Color32::BLACK), glow, glow]);
        Art::shade(painter, v, [x + ix, y + split, iw, iy + ih - split], [glow, glow, bottom, bottom]);
        let [fr, fg, fb] = d.frame_rgb;
        for t in &d.frame {
            Art::shade(painter, v, [x + t[0], y + t[1], t[2], t[3]], [Color32::from_rgb(fr, fg, fb); 4]);
        }
        if let Some(tex) = art.portrait(p.portrait) {
            let [s0, s1] = d.source_rows;
            let mirror = (k == 1) != p.flipped;
            let (ua, ub) = if mirror { (1.0, 0.0) } else { (0.0, 1.0) };
            // (the portrait picture's height: the select's fade bands')
            let full = roster.ui.fade_heights.first().map_or(1.0, |h| h.iter().sum::<f32>().max(1.0));
            let uv = Rect::from_min_max(Pos2::new(ua, s0 / full), Pos2::new(ub, s1 / full));
            painter.image(tex.id(), v.rect(x + ix, y + iy, iw, s1 - s0), uv, Color32::WHITE);
        }
    }
    if let Some(teams) = &extra.teams {
        crate::modes::draw_team_vs(painter, v, art, roster, teams, cnt);
    }
    // texts: the mode text and VS (once in place), the names (sliding in from the screen edges)
    if cnt == 0.0 {
        let (f, c, x, y) = d.mode_text;
        let text = m.text.replace(
            "%d",
            &match setup.mode {
                Mode::Arcade => setup.level + 1,
                Mode::Versus | Mode::TeamBattle | Mode::TimeAttack | Mode::Survival => number as usize,
                _ => 1,
            }
            .to_string(),
        );
        art.text(painter, v, f, c, x, y, &text, Color32::WHITE);
        if !m.one_player {
            let (t, f, c, x, y) = &d.vs_text;
            art.text(painter, v, *f, *c, *x, *y, t, Color32::WHITE);
        }
        // Force: STAGE n / FINAL STAGE, and the stage's name right-aligned in font 3
        if let Some((i, name, colour)) = &extra.force {
            let l = &m.raw["line2"];
            let line = match i {
                None => l["final"].as_str().unwrap_or_default().to_string(),
                Some(i) => crate::art::printf(l["stage"].as_str().unwrap_or_default(), &[*i as u32 + 1]),
            };
            art.text(painter, v, num(&l["font"]) as usize, num(&l["colour"]) as u32, num(&l["x"]), num(&l["y"]), &line, Color32::WHITE);
            let sn = &m.raw["stage_name"];
            for (row, t) in name.iter().enumerate().filter(|(_, t)| !t.is_empty()) {
                let x = num(&sn["x"]) - num(&sn["advance"]) * t.chars().count() as f32;
                art.text(painter, v, num(&sn["font"]) as usize, *colour, x, num(&sn["y"]) + num(&sn["dy"]) * row as f32, t, Color32::WHITE);
            }
        }
    }
    for &k in shown {
        let (id, cos) = selection(roster, setup.players[k]);
        let name = roster.ui.selection_names.get(id * 4 + cos).cloned().unwrap_or_default();
        let w = art.text_w(d.name_font, &name);
        let [bx, by, vx, _] = m.names[k];
        // x >= 0: left-aligned; x < 0: right-aligned at -x; kept inside 12..356
        let mut x = if bx >= 0.0 { bx } else { -bx - w };
        let right = x + w;
        x = x.max(d.clamp[0]);
        if right >= d.clamp[1] + 1.0 {
            x += d.clamp[1] - right;
        }
        // a non-zero slide enters from the screen edge on its side
        let vx = if vx > 0.0 {
            crate::art::SCREEN_W - x
        } else if vx < 0.0 {
            -(x + w)
        } else {
            0.0
        };
        let sx = x + ((cnt as i32 * vx as i32) >> 3) as f32;
        art.text(painter, v, d.name_font, d.name_colour, sx, by, &name, Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Option<Roster> {
        let gd = crate::test_assets().join("gamedata");
        kumite_fs::exists(&gd.join("screens.json")).then(|| Roster::load(&gd))
    }

    // (this game's own data: its cast, stages, slot order, costumes and portraits)
    #[test]
    fn the_select_screen_has_every_playable_character() {
        let Some(r) = roster() else { return };
        let original = r.characters.iter().filter(|c| c.id <= 20).count();
        assert_eq!(original, 21, "{:?}", r.characters.iter().map(|c| &c.name).collect::<Vec<_>>());
        let brought = r.characters.len() - original;
        assert_eq!(r.stages.len(), 15);
        let c0 = r.by_id(0).unwrap();
        assert_eq!(c0.costumes, vec![71, 75]);
        assert_eq!((r.home_stage(9, 0), r.home_stage(9, 2)), (8, 7), "Character 9's dojo; his school uniform's school");
        // the original slot order: character 7 first, character 9 last of the first row; character 17 hidden off the
        // edges
        assert_eq!((r.ui.slots[0], r.ui.slots[9]), (7, 9));
        assert_eq!(r.row_slots(0).len() + r.row_slots(1).len(), 20, "everyone of the original but character 17 has a face");
        assert_eq!((2..r.rows()).map(|k| r.row_slots(k).len()).sum::<usize>(), brought, "and the extra fighters' page the rest");
        assert_eq!(r.hidden_slot(0), Some(10));
        assert_eq!(r.layout(), 1, "two rows of faces");
        let l = &r.ui.layouts[1];
        assert_eq!((l.portrait_y, l.name_y, l.grid_y, l.row_dy), (14.0, 288.0, 336.0, 62.0));
        assert_eq!(l.decorations.len(), 16);
        assert!(r.portrait(1, 0).flipped, "Character 1's picture faces the other way");
        assert_eq!(r.portrait(5, 2).portrait, 18, "Character 5's third costume shows character 18's portrait");
        assert_eq!(r.portrait(0, 0).glow, [0xD9, 0, 0x1C], "Character 0's red glow");
        assert!(!r.ui.items[0].name.is_empty());
    }

    #[test]
    fn the_arcade_ladder_follows_the_game_rules() {
        let Some(r) = roster() else { return };
        // the fixed bosses at the end (ai.json arcade.ladder), a boss's stand-in for a player who is that boss
        let steps = &r.arcade.ladder;
        let bosses: Vec<usize> = steps.iter().filter_map(|s| if let LadderStep::Boss { boss, .. } = s { Some(*boss) } else { None }).collect();
        // (the first four from the original cast's first row, the next four anyone but the bosses: this game's pools)
        for seed in 0..50u32 {
            let mut s = seed;
            let ladder = arcade_ladder(&r, 0, &mut s);
            assert_eq!(ladder.len(), 10);
            assert!(ladder[..4].iter().all(|&c| c < 10 && c != 0), "{ladder:?}");
            assert!(ladder[4..8].iter().all(|&c| c != 0 && !r.arcade.bosses().contains(&c)), "{ladder:?}");
            assert_eq!(ladder[ladder.len() - bosses.len()..], bosses[..]);
            let mut uniq = ladder.clone();
            uniq.sort();
            uniq.dedup();
            assert_eq!(uniq.len(), 10, "no repeats: {ladder:?}");
        }
        for (k, step) in steps.iter().enumerate() {
            if let LadderStep::Boss { player: Some(p), instead: Some(i), .. } = step {
                let mut s = 1;
                let at = ladder_len_before(steps, k);
                assert_eq!(arcade_ladder(&r, *p, &mut s)[at], *i, "character {p} meets its stand-in {i}");
            }
        }
        // the extra fighters turn up in stages 5-8
        if r.characters.iter().any(|c| !c.prefix.is_empty()) {
            let mut s = 3;
            let extra = (0..50).flat_map(|_| arcade_ladder(&r, 0, &mut s)[4..8].to_vec()).filter(|&c| r.by_id(c).is_some_and(|c| !c.prefix.is_empty())).count();
            assert!(extra > 50, "{extra} extra fighters in 200 fights");
        }
    }

    /// How many characters the ladder's steps before step `k` give.
    fn ladder_len_before(steps: &[LadderStep], k: usize) -> usize {
        steps[..k].iter().map(|s| if let LadderStep::Draw { draw, .. } = s { *draw } else { 1 }).sum()
    }

    /// Tick until the screen starts a fight (or give up).
    fn run(s: &mut Screen, r: &Roster, seed: &mut u32) -> Option<(FightSetup, Vec<usize>)> {
        for _ in 0..200 {
            if let Outcome::Fight(setup, ladder) = s.tick(r, seed) {
                return Some((setup, ladder));
            }
        }
        None
    }

    #[test]
    fn no_model_shows_twice() {
        assert_eq!(free_costume(2, 0, |c| c == 0), Some(1));
        assert_eq!(free_costume(3, 2, |c| c == 2), Some(0));
        assert_eq!(free_costume(1, 0, |c| c == 0), None);
        let Some(r) = roster() else { return };
        // practice: P1 takes a one-costume character; the dummy cannot be the same one, the other costume of a
        // two-costume one comes instead
        let one = r.characters.iter().position(|c| c.costumes.len() == 1);
        let two = r.characters.iter().position(|c| c.costumes.len() == 2).unwrap();
        let slot_of = |i: usize| r.ui.slots.iter().position(|&c| c == r.characters[i].id).unwrap();
        for (i, expect) in [(two, Some(1))].into_iter().chain(one.map(|i| (i, None))) {
            let mut s = Select::new(Mode::Practice, 1, &r, 1);
            s.sides[0].slot = slot_of(i);
            assert!(s.pick(0, 0, &r));
            assert_eq!(s.sides[1].state, SideState::CpuChoosing);
            s.sides[1].slot = slot_of(i);
            assert_eq!(s.pick(1, 0, &r), expect.is_some());
            if let Some(c) = expect {
                assert_eq!(s.sides[1].costume, c);
            }
        }
        // a CPU-chosen clash (Survival, Ball, the demo) is moved apart
        let mut setup = FightSetup { mode: Mode::Survival, players: [(two, 0), (two, 0)], stage: 0, difficulty: 1, level: 1, handicap: [100; 2], ball: None, partners: None };
        distinct_models(&r, &mut setup);
        assert_eq!(setup.players[1], (two, 1));
        if let Some(i) = one {
            let mut setup = FightSetup { players: [(i, 0), (i, 0)], ..setup };
            distinct_models(&r, &mut setup);
            assert_ne!(setup.players[1].0, i);
        }
    }

    #[test]
    fn extra_levels_fight_a_cpu_on_the_level() {
        let Some(r) = roster() else { return };
        let Some(&(stage, _)) = r.extra_levels.first() else { return };
        let mut seed = 7;
        let mut s = Screen::menu(1);
        let at = shown_items(&r).iter().position(|i| i.1 == Entry::ExtraLevels).expect("the extra levels in the menu");
        for _ in 0..at {
            s.key(0, Key::Down, &r, &mut seed);
        }
        s.key(0, Key::Start, &r, &mut seed);
        assert!(matches!(s, Screen::ExtraLevels { cursor: 0, .. }));
        s.key(0, Key::Start, &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed);
        assert!(matches!(&s, Screen::Select(sel) if sel.mode == Mode::ExtraLevels && sel.sides[1].state == SideState::CpuChoosing));
        s.key(0, Key::Right, &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed);
        let (setup, _) = run(&mut s, &r, &mut seed).expect("the fight");
        assert_eq!((setup.mode, setup.stage), (Mode::ExtraLevels, stage));
        // the select has no way back (as the original's: only START + SELECT, the soft reset); cross / circle on the
        // list go back to the main menu
        let mut s = Screen::extra_levels(&r, stage, 1);
        s.key(0, Key::Start, &r, &mut seed);
        s.key(0, Key::Back, &r, &mut seed);
        assert!(matches!(s, Screen::Select(_)));
        let mut s = Screen::extra_levels(&r, stage, 1);
        s.key(0, Key::Button(2), &r, &mut seed);
        assert!(matches!(s, Screen::Menu(_)));
    }

    // (this game's characters: their costumes per button)
    #[test]
    fn select_buttons_pick_the_originals_costumes() {
        let Some(r) = roster() else { return };
        let by = |id: usize| r.by_id(id).unwrap();
        let c9 = by(9);
        // square the first, cross / circle the second, triangle and START the school uniform (0x8010DAB8)
        assert_eq!(c9.costume_for(Key::Button(0)), Some(0));
        assert_eq!(c9.costume_for(Key::Button(2)), Some(1));
        assert_eq!(c9.costume_for(Key::Button(3)), Some(1));
        assert_eq!(c9.costume_for(Key::Button(1)), Some(2));
        assert_eq!(c9.costume_for(Key::Start), Some(2));
        // no extra costume: triangle is the first, START picks nothing
        let c0 = by(0);
        assert_eq!(c0.costume_for(Key::Button(1)), Some(0));
        assert_eq!(c0.costume_for(Key::Start), None);
        // Character 5's slot 2 (character 18's model) is never reachable
        assert_eq!(by(5).costume_for(Key::Start), None);
        // the mirror match: 0 <-> 1, 2 -> 0
        assert_eq!(free_costume(3, 1, |c| c == 1), Some(0));
        assert_eq!(free_costume(3, 0, |c| c == 0), Some(1));
        assert_eq!(free_costume(3, 2, |c| c == 2), Some(0));
    }

    #[test]
    fn menus_lead_to_a_fight() {
        let Some(r) = roster() else { return };
        let mut seed = 7;
        let start = Select::new(Mode::Arcade, 1, &r, 1).sides[0].slot;
        // arcade: title -> menu -> select; cross picks the second costume of the slot right of P1's first
        // START goes straight to the menu, also during the white flash (0x800DB528)
        let mut s = Screen::title();
        s.key(0, Key::Start, &r, &mut seed);
        assert!(matches!(s, Screen::Menu(_)));
        s.key(0, Key::Start, &r, &mut seed);
        assert!(matches!(&s, Screen::Select(sel) if sel.mode == Mode::Arcade && sel.sides[1].state == SideState::Off));
        s.key(0, Key::Right, &r, &mut seed);
        s.key(0, Key::Button(2), &r, &mut seed);
        let (setup, ladder) = run(&mut s, &r, &mut seed).expect("arcade should start a fight");
        assert_eq!(setup.mode, Mode::Arcade);
        assert_eq!(r.characters[setup.players[0].0].id, r.ui.slots[start + 1], "one right of P1's first slot");
        assert_eq!(setup.players[0].1, 1, "cross picks costume 2");
        assert_eq!(ladder.len(), 10);
        assert_eq!(r.characters[setup.players[1].0].id, ladder[0]);
        // vs: the grid select; both players pick at once with their own keys, then confirm their LIFE; the stage is
        // P2's home (the cells this game's grid has)
        let mut s = Screen::title();
        s.key(0, Key::Start, &r, &mut seed);
        s.key(0, Key::Down, &r, &mut seed);
        s.key(0, Key::Start, &r, &mut seed);
        assert!(matches!(s, Screen::Grid(_)), "VS MODE has its own select screen");
        s.key(1, Key::Left, &r, &mut seed); // P2: character 9 -> character 2
        s.key(0, Key::Down, &r, &mut seed); // P1: character 7 -> character 6
        s.key(1, Key::Button(0), &r, &mut seed);
        s.key(1, Key::Button(0), &r, &mut seed);
        assert!(run(&mut s, &r, &mut seed).is_none(), "P1 has not chosen yet");
        s.key(0, Key::Button(0), &r, &mut seed);
        s.key(0, Key::Start, &r, &mut seed);
        let (setup, _) = run(&mut s, &r, &mut seed).expect("vs should start a fight");
        assert_eq!(setup.mode, Mode::Versus);
        assert_eq!((r.characters[setup.players[0].0].id, r.characters[setup.players[1].0].id), (6, 2));
        assert_eq!((setup.stage, setup.handicap), (r.home_stage(2, 0), [100, 100]));
        // time attack: one player, TIME ATTACK! on the other side, then the run with its ladder
        let mut s = Screen::menu(1);
        for _ in 0..3 {
            s.key(0, Key::Down, &r, &mut seed);
        }
        s.key(0, Key::Start, &r, &mut seed);
        assert!(matches!(&s, Screen::Select(sel) if sel.mode == Mode::TimeAttack && sel.sides[1].state == SideState::Label));
        s.key(0, Key::Button(0), &r, &mut seed);
        let started = (0..200).find_map(|_| match s.tick(&r, &mut seed) {
            Outcome::Run(run) => Some(run),
            _ => None,
        });
        let ta = started.expect("time attack should start its run");
        assert!(matches!(&ta.kind, crate::modes::Kind::TimeAttack(t) if t.ladder.len() == 10 && t.stage == 0));
        assert_eq!(ta.rules(&r.modes), (2, Some(2400), 130), "this game's Time Attack");
        // team battle: the grid; P1 sets 5 members, picks two, START fills the rest; the CPU's team as big
        let mut s = Screen::menu(1);
        for _ in 0..2 {
            s.key(0, Key::Down, &r, &mut seed);
        }
        s.key(0, Key::Start, &r, &mut seed);
        assert!(matches!(s, Screen::Grid(_)), "Team Battle picks on the grid");
        s.key(0, Key::Right, &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed); // the same character again: ignored
        s.key(0, Key::Right, &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed);
        s.key(0, Key::Start, &r, &mut seed);
        match s.tick(&r, &mut seed) {
            Outcome::Run(team) => match &team.kind {
                crate::modes::Kind::Team(t) => {
                    assert_eq!((t.teams[0].members.len(), t.teams[1].members.len()), (5, 5));
                    assert_eq!((t.teams[0].picked, t.teams[0].human, t.teams[1].human), (2, true, false));
                    let mut ids: Vec<usize> = t.teams[0].members.iter().map(|m| m.0).collect();
                    ids.dedup();
                    assert_eq!(ids.len(), 5, "no character twice in a team");
                }
                _ => panic!("a team run"),
            },
            _ => panic!("the team battle should start"),
        }
        // practice: P1 picks, then picks the dummy
        let mut s = Screen::menu(1);
        for _ in 0..7 {
            s.key(0, Key::Down, &r, &mut seed);
        }
        s.key(0, Key::Start, &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed);
        assert!(matches!(&s, Screen::Select(sel) if sel.mode == Mode::Practice && sel.sides[1].state == SideState::CpuChoosing));
        s.key(0, Key::Right, &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed);
        let (setup, _) = run(&mut s, &r, &mut seed).expect("practice should start");
        // off the edge right of the first row: the hidden slot (the extra fighters are on their own page)
        assert_eq!(Some(r.characters[setup.players[1].0].id), r.hidden_slot(0).map(|h| r.ui.slots[h]));
        // the select has no way back (the soft reset is the app's); quit from the title
        let mut s = Screen::menu(1);
        s.key(0, Key::Start, &r, &mut seed);
        s.key(0, Key::Back, &r, &mut seed);
        assert!(matches!(s, Screen::Select(_)));
        assert!(matches!(Screen::title().key(0, Key::Back, &r, &mut seed), Outcome::Quit));
    }

    #[test]
    fn extra_fighters_have_a_slot_and_a_cell() {
        let Some(r) = roster() else { return };
        // the extra fighters' page: its own rows of the character select (two a page), pages of the VS grid
        for c in r.characters.iter().filter(|c| !c.prefix.is_empty()) {
            let slot = r.ui.slots.iter().position(|&x| x == c.id).expect("a select slot");
            assert!((2..r.rows()).contains(&r.row_of(slot)), "{} on the extra fighters' pages", c.name);
            assert!(r.grid.cell_ids().contains(&c.id), "{}: a VS grid cell", c.name);
            assert_ne!(r.portrait(c.id, 0).portrait, 0, "{}: its own portrait", c.name);
        }
        // Down from the original's second row goes on to the extra fighters' page and comes back round
        if r.characters.iter().any(|c| !c.prefix.is_empty()) {
            let mut s = Select::new(Mode::Arcade, 1, &r, 1);
            s.move_cursor(0, Key::Down, &r);
            assert_eq!((r.row_of(s.sides[0].slot), s.page), (1, 0));
            s.move_cursor(0, Key::Down, &r);
            assert_eq!((r.row_of(s.sides[0].slot), s.page), (2, 1));
            for _ in 0..3 {
                s.move_cursor(0, Key::Down, &r);
            }
            assert_eq!(s.page, 0);
        }
    }

    #[test]
    fn the_select_timer_picks_for_you() {
        let Some(r) = roster() else { return };
        let mut seed = 3;
        let sel = Select::new(Mode::Arcade, 1, &r, 1);
        let first = r.ui.slots[sel.sides[0].slot];
        let mut s = Screen::Select(Box::new(sel));
        let mut frames = 0;
        loop {
            frames += 1;
            if let Outcome::Fight(setup, _) = s.tick(&r, &mut seed) {
                assert_eq!(r.characters[setup.players[0].0].id, first, "the slot it starts on");
                break;
            }
            assert!(frames < 20 * 60 + 100);
        }
        assert!(frames >= 20 * 60);
    }

    #[test]
    fn the_title_runs_the_demo_and_the_menu_times_out() {
        let Some(r) = roster() else { return };
        let mut seed = 5;
        let mut s = Screen::title();
        // the title hands over to the attract demo fight; the menu idles back to the attract loop
        let demo = (0..TITLE_END + 1).any(|_| matches!(s.tick(&r, &mut seed), Outcome::Demo));
        assert!(demo);
        let mut s = Screen::menu(0);
        let back = (0..MENU_IDLE).any(|_| matches!(s.tick(&r, &mut seed), Outcome::Attract));
        assert!(back);
        // the PRESENTS screen hands over to what follows it
        let mut s = Screen::Presents { frame: 0, next: AttractNext::Embu };
        let next = (0..PRESENTS_END).find_map(|_| match s.tick(&r, &mut seed) {
            Outcome::AttractNext(n) => Some(n),
            _ => None,
        });
        assert_eq!(next, Some(AttractNext::Embu));
    }

    #[test]
    fn menus_play_the_games_sounds() {
        let Some(r) = roster() else { return };
        let mut seed = 1;
        sfx::take();
        let mut s = Screen::Title { frame: WHITE_OUT, difficulty: 1 };
        s.key(0, Key::Start, &r, &mut seed);
        s.key(0, Key::Down, &r, &mut seed);
        s.key(0, Key::Up, &r, &mut seed);
        s.key(0, Key::Start, &r, &mut seed);
        s.key(0, Key::Right, &r, &mut seed);
        s.key(0, Key::Button(0), &r, &mut seed);
        assert_eq!(sfx::take(), [sfx::start(), sfx::cursor(), sfx::cursor(), sfx::confirm(), sfx::select_cursor(), sfx::pick()]);
        assert!([sfx::start(), sfx::cursor(), sfx::confirm(), sfx::select_cursor(), sfx::pick()].iter().all(|&c| c >> 14 == 1), "system sounds (bank 1)");
    }

    /// OpenKumite's EXIT, last in the main menu (up from ARCADE MODE), leaves the game.
    #[test]
    fn exit_leaves_the_game() {
        let Some(r) = roster() else { return };
        let mut seed = 1;
        let mut s = Screen::menu(1);
        sfx::take();
        s.key(0, Key::Up, &r, &mut seed);
        assert!(matches!(s.key(0, Key::Start, &r, &mut seed), Outcome::Exit));
        assert_eq!(sfx::take(), [sfx::cursor(), sfx::confirm()], "the cursor and the confirm sound");
    }
}
