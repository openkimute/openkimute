//! Force's run between stages (force research §1.7) and its HUD (§6): the three health bars (the player's,
//! the two soldiers' sized by their class), the name plates, SCORE / HI-SCORE, the progress map, the keys, the
//! popups (+N SEC, +LIFE!), NOW LOADING..., the boss's bonus time, STAGE CLEAR!! and the tally; and the results
//! screen (record 302 mode 15).

use crate::art::{int, num, pair, printf, Art, Screen as View, Tex};
use egui::Color32;
use glam::Vec3;
use kumite_game::force::{ForceData, ForceStage};
use std::sync::Arc;

/// The run: the player, the stage reached, what carries over.
#[derive(Clone)]
pub struct ForceRun {
    pub data: Arc<ForceData>,
    /// Roster index and costume; the game's selection (char * 4 + costume) and character id.
    pub player: (usize, usize),
    pub selection: usize,
    pub player_char: usize,
    pub difficulty: usize,
    /// The player's costume number (0..3, the name plate's colours).
    pub player_costume: usize,
    /// Stage index: the stages in order, then the final one (`ForceData::final_index`).
    pub index: usize,
    /// OpenKumite's extra stage played after the original's last before the final (force.json `extra_stages`), and
    /// whether the final stage was earned there.
    pub extra: Option<usize>,
    pending_final: bool,
    pub score: u32,
    pub hiscore: u32,
    pub new_hiscore: bool,
    pub keys: u32,
    /// Keys earned this run (results: copper 1, silver 2, gold 4), the final character saved (8), unlocked before (16).
    pub flags: u32,
    pub kills: u32,
    pub clock: u32,
    /// The bosses met (character, costume) per stage before the final.
    pub bosses: Vec<Option<(usize, usize)>>,
    /// The stage being played.
    pub stage: Option<ForceStage>,
    /// Frames since the boss went down (STAGE CLEAR!!).
    pub clear_frames: u32,
    /// Bar states: per bar (last health, target, shown, trail): the player, the two soldiers, the co-op partner;
    /// then (OpenKumite's own) 3P, 4P and the soldier slots 2...
    bars: [(i32, i32, i32, i32); 12],
    /// The co-op partner (OpenKumite's own): roster index and costume, and its costume number (name plate).
    pub partner: Option<(usize, usize)>,
    pub partner_costume: usize,
    /// 3P and 4P (OpenKumite's own): roster index, costume and costume number.
    pub more_players: [Option<(usize, usize, usize)>; 2],
}

/// A run as a stage of it begins (a replay of the stage starts from it): what the stage is made from.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForceStart {
    pub player: (usize, usize),
    pub selection: usize,
    pub player_char: usize,
    pub difficulty: usize,
    pub player_costume: usize,
    pub index: usize,
    pub extra: Option<usize>,
    pub pending_final: bool,
    pub score: u32,
    pub hiscore: u32,
    pub keys: u32,
    pub flags: u32,
    pub kills: u32,
    pub clock: u32,
    pub partner: Option<(usize, usize)>,
    pub partner_costume: usize,
    pub more_players: [Option<(usize, usize, usize)>; 2],
}

impl ForceRun {
    /// The run as the stage about to load begins.
    pub fn start(&self) -> ForceStart {
        ForceStart {
            player: self.player,
            selection: self.selection,
            player_char: self.player_char,
            difficulty: self.difficulty,
            player_costume: self.player_costume,
            index: self.index,
            extra: self.extra,
            pending_final: self.pending_final,
            score: self.score,
            hiscore: self.hiscore,
            keys: self.keys,
            flags: self.flags,
            kills: self.kills,
            clock: self.clock,
            partner: self.partner,
            partner_costume: self.partner_costume,
            more_players: self.more_players,
        }
    }

    /// A run at a stage's start (a replay of it).
    pub fn from_start(data: Arc<ForceData>, s: &ForceStart) -> Self {
        let mut run = Self::new(data, s.player, s.selection, s.player_char, s.difficulty, s.keys, s.hiscore, false);
        (run.player_costume, run.index, run.extra, run.pending_final, run.score, run.flags, run.kills, run.clock) =
            (s.player_costume, s.index, s.extra, s.pending_final, s.score, s.flags, s.kills, s.clock);
        (run.partner, run.partner_costume, run.more_players) = (s.partner, s.partner_costume, s.more_players);
        run
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(data: Arc<ForceData>, player: (usize, usize), selection: usize, player_char: usize, difficulty: usize, keys: u32, hiscore: u32, drb_unlocked: bool) -> Self {
        let stages = data.final_index();
        Self {
            data,
            player,
            selection,
            player_char,
            difficulty,
            player_costume: selection & 3,
            index: 0,
            extra: None,
            pending_final: false,
            score: 0,
            hiscore,
            new_hiscore: false,
            keys,
            flags: 0,
            kills: 0,
            clock: kumite_game::force::CLOCK,
            bosses: vec![None; stages],
            stage: None,
            clear_frames: 0,
            bars: Default::default(),
            partner: None,
            partner_costume: 0,
            more_players: [None; 2],
        }
        .with_unlock(drb_unlocked)
    }

    /// The final character already unlocked: the results show the bosses and the soldiers beaten (flag 0x10).
    fn with_unlock(mut self, drb: bool) -> Self {
        if drb {
            self.flags |= 0x10;
        }
        self
    }

    /// The stage after a clear (0x800B5CF0): the next one, the final stage when it was earned, or the end; after
    /// the last before the final, OpenKumite's extra stages first.
    pub fn next(&mut self) -> bool {
        let Some(st) = self.stage.take() else { return false };
        let last = self.data.final_index();
        self.score = st.score;
        self.keys = st.keys;
        self.kills = st.kills_total;
        if let (Some(b), None) = (st.boss, self.extra) {
            if let Some(slot) = self.bosses.get_mut(self.index) {
                *slot = Some(b);
            }
        }
        if st.key_earned {
            self.flags |= [1, 3, 7][(self.keys as usize).saturating_sub(1).min(2)];
        }
        self.clear_frames = 0;
        if let Some(e) = self.extra {
            if e + 1 < self.data.extra_stages.len() {
                self.extra = Some(e + 1);
                return true;
            }
            self.extra = None;
            if self.pending_final {
                self.index = last;
            }
            return self.pending_final;
        }
        let challenge = st.final_challenge;
        if self.index + 1 == last && !self.data.extra_stages.is_empty() {
            self.pending_final = challenge;
            self.extra = Some(0);
            return true;
        }
        if self.index == last {
            self.flags |= 8;
        }
        self.index += 1;
        self.clear_frames = 0;
        self.index < last || (self.index == last && challenge)
    }

    /// The stage card's line and name: the stage's number (0-based; None: the final stage), its name and colour.
    pub fn card(&self) -> (Option<usize>, [String; 2], u32) {
        if let Some(x) = self.extra.and_then(|e| self.data.extra_stages.get(e)) {
            return (Some(self.data.final_index() + self.extra.unwrap_or(0)), x.name.clone(), x.name_colour);
        }
        let last = self.data.final_index();
        let st = self.data.stages.get(self.index.min(last));
        ((self.index < last).then_some(self.index), st.map_or_else(Default::default, |s| s.name.clone()), st.map_or(0, |s| s.name_colour))
    }

    /// The stage's id (stages.json): the original's (force.json `stages`), an extra stage's level.
    pub fn stage_id(&self) -> usize {
        match self.extra.and_then(|e| self.data.extra_stages.get(e)) {
            Some(x) => x.stage,
            None => self.data.stages.get(self.index.min(self.data.final_index())).map_or(0, |s| s.stage),
        }
    }

    /// Per frame: the bars (0x800B2C34: grow 3 px a frame, drop at once, the trail follows).
    pub fn step_hud(&mut self, game: &kumite_game::Game) {
        let Some(st) = &self.stage else { return };
        let f = &game.fight;
        let actors: [Option<&kumite_sim::Fighter>; 12] = std::array::from_fn(|k| match k {
            0 => Some(&f.fighters[0]),
            1 | 2 => slot_fighter(st, game, k - 1),
            3 => f.partner.as_deref(),
            4 | 5 => f.actor(k),
            _ => (k - 4 < st.slots.len()).then(|| slot_fighter(st, game, k - 4)).flatten(),
        });
        for (k, a) in actors.iter().enumerate() {
            let w = self.bar_w(k) - 4;
            let b = &mut self.bars[k];
            if let Some(a) = a {
                let max = (a.max_health.max(1) * kumite_sim::ONE) as i64;
                let unit = (max / w.max(1) as i64).max(1);
                let hp = a.health.max(0) as i64;
                if hp as i32 != b.0 {
                    b.0 = hp as i32;
                    b.1 = ((hp + unit - 1) / unit) as i32;
                }
                b.1 = b.1.min(w);
                b.3 = if b.2 < b.3 { b.3 - ((b.3 - b.2 + 15) >> 4) } else { b.2 };
                b.2 = if b.2 < b.1 { (b.2 + 3).min(b.1) } else { b.1 };
                b.3 = b.3.max(b.2);
            }
        }
    }

    /// Bar k's width: the players' the full width, a soldier's its class's.
    fn bar_w(&self, k: usize) -> i32 {
        let full = self.data.draw["bars"]["right"].as_i64().unwrap_or(0) as i32;
        if matches!(k, 0 | 3 | 4 | 5) {
            return full;
        }
        let slot = if k < 3 { k - 1 } else { k - 4 };
        let class = self.stage.as_ref().and_then(|s| s.slots.get(slot)).map_or(0, |s| s.class);
        self.data.classes.get(class).map_or(full, |c| c.bar_w)
    }

    /// The Force HUD over the fight (`project`: world metres to screen).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        painter: &egui::Painter,
        v: &View,
        art: &Art,
        hud: &crate::hud::Hud,
        roster: &crate::menu::Roster,
        game: &kumite_game::Game,
        frame: u64,
        project: &dyn Fn(Vec3) -> Option<(f32, f32)>,
    ) {
        let Some(st) = &self.stage else { return };
        let src: &[&str] = &["exe"];
        let dr = &self.data.draw;
        let b = &dr["bars"];
        let page = pair(&b["page"]);
        let (right, [cap_w, cap_h]) = (num(&b["right"]), [int(&b["caps"]["w"]), int(&b["caps"]["h"])]);
        let cap_clut = Some(pair(&b["caps"]["clut"]));
        let [su, sv] = pair(&b["strip"]["uv"]);
        let strip_cluts = [0, 1, 2].map(|k| pair(&b["strip"]["cluts"][k]));
        // the three bars, right-aligned at x + 156 (OpenKumite's own: up to four players on the left, four soldiers or
        // bosses on the right)
        let slot_in = |k: usize| st.slots.get(k).is_some_and(|s| (4..=7).contains(&s.state));
        for (k, &(_, _, shown, trail)) in self.bars.iter().enumerate().take(8) {
            let show = match k {
                1 | 2 => slot_in(k - 1),
                3 => game.partner_body.is_some() && game.fight.partner.is_some(),
                4 | 5 => game.more_body.get(k - 4).is_some_and(Option::is_some),
                6 | 7 => slot_in(k - 4),
                _ => true,
            };
            if !show {
                continue;
            }
            // (the original's three places; OpenKumite's go on down the two columns, as far apart)
            let &[[lx, y0], [rx, _], [_, y1]] = self.data.hud.bars.as_slice() else { continue };
            let dy = y1 - y0;
            let (x, y) = [(lx, y0), (rx, y0), (rx, y1), (lx, y1), (lx, y1 + dy), (lx, y1 + 2 * dy), (rx, y1 + dy), (rx, y1 + 2 * dy)][k];
            let (x, y) = (x as f32, y as f32);
            let w = self.bar_w(k) as f32;
            // (right-aligned: the caps at either end, the strips filling leftwards from 2 px in)
            let left = x + right - w;
            let cap = |k: usize| Tex { page, bpp: 4, u: int(&b["caps"]["uv"][k][0]), v: int(&b["caps"]["uv"][k][1]), clut: cap_clut };
            art.sprite(painter, v, src, &cap(0), cap_w, cap_h, left, y, Color32::WHITE);
            art.sprite(painter, v, src, &cap(1), cap_w, cap_h, x + right - cap_w as f32, y, Color32::WHITE);
            let inner = w - 4.0;
            let (shown, trail) = (shown as f32, trail as f32);
            let mut at = x + right - 2.0;
            for (len, clut) in [(shown, strip_cluts[0]), (trail - shown, strip_cluts[1]), (inner - trail, strip_cluts[2])] {
                if len <= 0.0 {
                    continue;
                }
                at -= len;
                let strip = Tex { page, bpp: 4, u: su, v: sv, clut: Some(clut) };
                art.quad(painter, v, src, &strip, 1, cap_h, [at, y, len, cap_h as f32], [Color32::WHITE; 4]);
            }
        }
        // the name plates: the player's, the soldiers' right-aligned; OpenKumite's more players and soldiers go on down
        // the two columns as far apart
        let plate = |k: usize| [num(&dr["plates"][k][0]), num(&dr["plates"][k][1])];
        let ([px, py], [sx, sy], [_, sy1]) = (plate(0), plate(1), plate(2));
        let pdy = sy1 - sy;
        hud.name_plate(painter, v, art, game.bodies[0].model_record, self.player_costume, px, py, false);
        // the co-op partner under the player (OpenKumite's own), or the invitation to join; a partner who is down
        // can come back with START
        let blink = game.fight.frame & 0x30 != 0;
        let push_p2 = hud.data.push.get(1).map_or("", String::as_str);
        match (game.partner_body.as_ref(), game.fight.partner.as_deref()) {
            (Some(b), Some(p)) => {
                hud.name_plate(painter, v, art, b.model_record, self.partner_costume, px, py + pdy, false);
                art.text(painter, v, 0, 5, 150.0 - 18.0, 116.0, "2P", Color32::WHITE);
                if p.health <= 0 && blink {
                    art.text(painter, v, 0, 5, 19.0, 140.0, push_p2, Color32::WHITE);
                }
            }
            _ if st.state < 7 && blink => art.text(painter, v, 0, 5, 19.0, 92.0, push_p2, Color32::WHITE),
            _ => {}
        }
        // 3P and 4P under them
        for (j, p) in self.more_players.iter().enumerate() {
            if let (Some((_, _, cos)), Some(Some(b))) = (p, game.more_body.get(j)) {
                let y = py + pdy * (2 + j) as f32;
                hud.name_plate(painter, v, art, b.model_record, *cos, px, y, false);
                art.text(painter, v, 0, 5, 150.0 - 18.0, y + 2.0, &format!("{}P", j + 3), Color32::WHITE);
            }
        }
        for k in 0..st.slots.len().min(4) {
            if st.slots[k].state < 4 || st.slots[k].state > 7 {
                continue;
            }
            if let Some(b) = game.body_of(st.slot_actor(k)) {
                // (the soldiers' costumes: a model every 4 records from the first soldier's)
                let first = roster.by_selection.get(&self.data.soldier_selection_base).map_or(usize::MAX, |s| s.1);
                let cos = if b.model_record >= first { (b.model_record - first) / 4 } else { 0 };
                hud.name_plate(painter, v, art, b.model_record, cos, sx, sy + pdy * k as f32, true);
            }
        }
        // SCORE and HI-SCORE over the bars
        let texts = &self.data.hud;
        let (ux, uy, uc) = hud.data.under_place();
        art.text(painter, v, 0, uc[0], ux[0], uy, &printf(&texts.score, &[st.score]), Color32::WHITE);
        art.text(painter, v, 0, uc[1], ux[1], uy, &printf(&texts.hi_score, &[st.hiscore]), Color32::WHITE);
        // the progress map: a box per stage, the final box, the cursor
        let mp = &dr["map"];
        let (mx, mdx, my, mh, fw) = (num(&mp["x"]), num(&mp["dx"]), num(&mp["y"]), num(&mp["h"]), num(&mp["final_w"]));
        let last = self.data.final_index();
        let colours: Vec<[u8; 3]> = mp["colours"].as_array().into_iter().flatten().map(|c| [0, 1, 2].map(|k| int(&c[k]) as u8)).collect();
        for (i, c) in colours.iter().enumerate() {
            let lit = i <= self.index.min(last.saturating_sub(1)) || self.index == last;
            let mut col = Color32::from_rgba_unmultiplied(c[0], c[1], c[2], if lit { 160 } else { 40 });
            if i == self.index && self.index < last {
                let t = (frame as f32 * 0x40 as f32 / 4096.0 * std::f32::consts::TAU).sin();
                let k = (1.0 + t * 0.5).clamp(0.5, 2.0);
                col = Color32::from_rgb((c[0] as f32 * k).min(255.0) as u8, (c[1] as f32 * k).min(255.0) as u8, (c[2] as f32 * k).min(255.0) as u8);
            }
            Art::shade(painter, v, [mx + mdx * i as f32, my, mdx, mh], [col; 4]);
        }
        let end = mx + mdx * colours.len() as f32;
        let fin = if self.index == last { Color32::from_rgb(160, 160, 160) } else { Color32::from_rgba_unmultiplied(64, 64, 64, 80) };
        Art::shade(painter, v, [end, my, fw, mh], [fin; 4]);
        let (lx, [ly0, ly1]) = (num(&mp["lines_x"]), [num(&mp["lines_y"][0]), num(&mp["lines_y"][1])]);
        for k in 0..=colours.len() {
            Art::shade(painter, v, [lx + mdx * k as f32, ly0, 1.0, ly1 - ly0], [Color32::WHITE, Color32::WHITE, Color32::from_gray(0x80), Color32::from_gray(0x80)]);
        }
        let box_x = mx + mdx * self.index as f32;
        let m = if self.index == last {
            end + fw
        } else if st.boss_phase {
            box_x + mdx
        } else {
            (box_x + ((st.cam_x.max(0) as f32 * mdx) / st.end_x() as f32)).min(box_x + mdx)
        };
        Art::shade(painter, v, [m, my, 1.0, mh], [Color32::WHITE; 4]);
        // (the cursor, a blue triangle 12 px high pointing down at the map: OpenKumite's)
        let w = (12.0 * ((frame as f32 * 0x40 as f32 / 4096.0) * std::f32::consts::TAU).sin()).abs() / 2.0 + 1.0;
        painter.add(egui::Shape::convex_polygon(vec![v.pos(m - w, my - 12.0), v.pos(m + w, my - 12.0), v.pos(m, my)], Color32::from_rgb(0, 128, 255), egui::Stroke::NONE));
        // the keys owned (copper, silver, gold)
        let ky = &dr["keys"];
        let vram: Vec<[u32; 2]> = ky["vram"].as_array().into_iter().flatten().map(pair).collect();
        for (k, &place) in vram.iter().enumerate().take(st.keys as usize) {
            let clut = [int(&ky["clut_x"][k]), int(&ky["clut_y"])];
            let t = Tex::at(place, 4, Some(clut));
            art.sprite(painter, v, &["r007_raw", "exe"], &t, int(&ky["w"]), int(&ky["h"]), num(&ky["dx"]) * k as f32, num(&ky["y"]), Color32::WHITE);
        }
        // +N SEC over a soldier just killed, rising
        let pp = &dr["popup"];
        let ((pf, pc), (sf, sc)) = ((int(&pp["plus"][0]) as usize, int(&pp["plus"][1])), (int(&pp["sec"][0]) as usize, int(&pp["sec"][1])));
        let nf = int(&pp["number_font"]) as usize;
        for (secs, at, left) in st.popups() {
            if let Some((sx, sy)) = project(at + Vec3::new(0.0, 0.3, 0.0)) {
                let y = sy - (60 - left) as f32 / 2.0;
                let x = sx + num(&pp["dx"]);
                art.text(painter, v, pf, pc, x, y + num(&pp["plus_dy"]), "+", Color32::WHITE);
                let n = format!("{secs}");
                // (the number in SEC's colour, each piece after the last)
                let nx = x + art.text_w(pf, "+");
                art.text(painter, v, nf, sc, nx, y, &n, Color32::WHITE);
                art.text(painter, v, sf, sc, nx + art.text_w(nf, &n), y + num(&pp["sec_dy"]), &texts.sec, Color32::WHITE);
            }
        }
        // +LIFE! over the player after a chicken
        for c in &st.chickens {
            if c.state == 2 {
                let left = st.life_text(c);
                if let Some((sx, sy)) = project(c.at / kumite_game::UNITS + Vec3::new(0.0, 0.3, 0.0)) {
                    // (the first colour while frame & 2 is clear, 0x800B4174)
                    let col = int(&dr["life"]["colours"][usize::from(frame & 2 != 0)]);
                    art.text(painter, v, 0, col, sx + num(&dr["life"]["dx"]), sy - (60 - left.min(60)) as f32 / 2.0, &texts.life, Color32::WHITE);
                }
            }
        }
        // NOW LOADING..., the boss's bonus time
        if st.state == 4 {
            let nl = &dr["now_loading"];
            art.text(painter, v, int(&nl["font"]) as usize, int(&nl["colour"]), num(&nl["x"]), num(&nl["y"]), &texts.now_loading, Color32::WHITE);
        }
        if st.state == 6 && st.bonus_time > 0 && game.fight.fighters[1].health > 0 {
            let b = st.bonus_time;
            let at = game.fight.fighters[1].root() / kumite_game::UNITS + Vec3::new(0.0, 1.9, 0.0);
            if let Some((sx, sy)) = project(at) {
                let bt = &dr["bonus_time"];
                art.text(painter, v, 0, int(&bt["colour"]), sx + num(&bt["dx"]), sy, &format!("{:2}\"{:02}", b / 60 % 60, b * 100 / 60 % 100), Color32::WHITE);
            }
        }
        // STAGE CLEAR!! and the tally
        if st.state == 7 || st.state == 10 {
            let sc = crate::modes::Text::from(&dr["stage_clear"]);
            art.text(painter, v, sc.font, sc.colour, sc.x, sc.y, &self.data.hud.stage_clear, Color32::WHITE);
        }
        if st.state == 7 || st.tally_done && self.index < self.data.final_index() {
            let t = st.tally_frame();
            let time = |fmt: &str, f: u32| printf(fmt, &[f / 3600, f / 60 % 60, f * 100 / 60 % 100]);
            let ta = &dr["tally"];
            let (tx, ty, tdy) = (num(&ta["x"]), num(&ta["y"]), num(&ta["dy"]));
            let line = |k: usize| (int(&ta["lines"][k][0]) as usize, int(&ta["lines"][k][1]), ty + tdy * k as f32);
            let font = line(3).0;
            if t >= 30 {
                let (f, c, y) = line(0);
                art.text(painter, v, f, c, tx, y, &printf(&texts.stage_bonus, &[self.data.stage_bonus.get(self.index).copied().unwrap_or(0)]), Color32::WHITE);
            }
            if t >= 60 {
                let (f, c, y) = line(1);
                art.text(painter, v, f, c, tx, y, &printf(&texts.clear_bonus, &[st.clear_bonus]), Color32::WHITE);
            }
            if t >= 90 {
                let (f, c, y) = line(2);
                art.text(painter, v, f, c, tx, y, &time(&texts.clear_time, st.elapsed), Color32::WHITE);
            }
            if t >= 120 {
                let (_, c, y) = line(3);
                if st.key_earned {
                    art.text(painter, v, font, if t & 2 != 0 { int(&ta["blink"]) } else { c }, num(&ta["key_x"]), y, &self.data.hud.key, Color32::WHITE);
                } else if st.final_challenge {
                    art.text(painter, v, font, if t & 4 != 0 { int(&ta["blink"]) } else { c }, num(&ta["final_x"]), y, &self.data.hud.final_challenge, Color32::WHITE);
                } else if self.index + 1 < self.data.final_index() && st.bonus_time > 0 {
                    art.text(painter, v, font, c, tx, y, &time(&texts.bonus_time, st.bonus_time), Color32::WHITE);
                }
            }
        }
    }
}

impl ForceRun {
    /// The roast chickens lying on the stage (record 7 0x800B3E90), behind the fighters: (x, y, z) projected
    /// (`place`: game point → screen x, y and the GTE depth SZ / 4), the chicken W = w / depth wide and H = h / depth
    /// high from H to 2H above it (force.json `draw.chicken`), its shadow (half-transparent, a quarter bright) the same
    /// width and W high under it; the stage's own textures.
    pub fn draw_pickups(&self, painter: &egui::Painter, v: &View, art: &Art, place: &dyn Fn(Vec3) -> Option<(f32, f32, f32)>) {
        let Some(st) = &self.stage else { return };
        let k = if self.index < self.data.final_index() { self.index } else { 0 };
        let pic = |name: &str| art.pic(&format!("force/{name}_{k}.png")).map(|(t, _)| t.id());
        let (Some(chicken), Some(shadow)) = (pic("chicken"), pic("shadow")) else { return };
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
        let ch = &self.data.draw["chicken"];
        let (cy, cw, chh) = (num(&ch["y"]), num(&ch["w"]), num(&ch["h"]));
        for c in st.chickens.iter().filter(|c| c.state == 1) {
            // game (x, y down, z) → the simulation's axes (y up, z = −game z)
            let Some((sx, sy, depth)) = place(Vec3::new(c.x as f32, -cy, -c.z as f32)) else { continue };
            let depth = depth.max(1.0);
            let (w, h) = (cw / depth, chh / depth);
            painter.image(shadow, v.rect(sx - w / 2.0, sy - h - w, w, w), uv, Color32::from_black_alpha(128));
            painter.image(chicken, v.rect(sx - w / 2.0, sy - 2.0 * h, w, h), uv, Color32::WHITE);
        }
    }
}

/// The fighter showing soldier slot k, when it is in play.
fn slot_fighter<'a>(st: &ForceStage, game: &'a kumite_game::Game, k: usize) -> Option<&'a kumite_sim::Fighter> {
    game.fight.actor(st.slot_actor(k))
}

/// The results screen (record 302 mode 15).
pub struct ForceResults {
    pub score: u32,
    pub hiscore: u32,
    pub new_hiscore: bool,
    pub flags: u32,
    pub bosses: Vec<Option<(usize, usize)>>,
    pub kills: u32,
    pub t: u32,
    pub frame: u32,
    pub fade: i32,
    pub leaving: bool,
}

impl ForceResults {
    /// `sfx`: the tick's sound and the saved character's line's.
    pub fn tick(&mut self, sfx: (u16, u16)) -> (Option<u16>, bool) {
        self.frame += 1;
        if self.leaving {
            self.fade -= 4;
            return (None, self.fade <= 0);
        }
        if self.fade < 256 {
            self.fade = (self.fade + 8).min(256);
            return (None, false);
        }
        self.t += 1;
        if self.frame >= 4681 {
            self.leaving = true;
        }
        let sfx = match self.t {
            30 | 60 => Some(sfx.0),
            90 if self.flags & 0x10 == 0 && self.flags & 8 != 0 => Some(sfx.1),
            90 | 120 | 150 if self.flags & 0x18 == 0 => Some(sfx.0),
            _ => None,
        };
        (sfx, false)
    }

    pub fn draw(&self, painter: &egui::Painter, v: &View, art: &Art, roster: &crate::menu::Roster) {
        Art::shade(painter, v, crate::art::FULL, [Color32::BLACK; 4]);
        let texts = &roster.screens["modes"]["results"]["force"];
        crate::modes::Text::from(&texts["title"]).draw(painter, v, art);
        fn s(v: &serde_json::Value) -> &str {
            v.as_str().unwrap_or_default()
        }
        // (each text's place, font and colour: `layout`, as the original prints them)
        let l = &texts["layout"];
        let place = |p: &serde_json::Value, text: &str, colour: Option<u32>| {
            let t = crate::modes::Text::from(p);
            art.text(painter, v, t.font, colour.unwrap_or(t.colour), t.x, t.y, text, Color32::WHITE);
        };
        // the blinking texts' second colour on frame & 2 (traced with 0x800FA840 0 and 2)
        let blink = |p: &serde_json::Value| p["colours"][usize::from(self.frame & 2 != 0)].as_u64().map(|c| c as u32);
        let t = self.t;
        if t >= 30 {
            place(&l["your_score"], s(&texts["your_score"]), None);
            place(&l["score"], &crate::art::printf(s(&l["score"]["text"]), &[self.score]), None);
        }
        if t >= 60 {
            place(&l["high_score"], s(&texts["high_score"]), None);
            place(&l["hiscore"], &crate::art::printf(s(&l["hiscore"]["text"]), &[self.hiscore]), None);
            if self.new_hiscore {
                place(&l["new_record"], s(&texts["new_record"]), blink(&l["new_record"]));
            }
        }
        if self.flags & 0x10 != 0 {
            if t >= 90 {
                place(&l["boss"], s(&texts["boss"]), None);
            }
            let late = t.saturating_sub(90);
            for (k, (b, at)) in self.bosses.iter().zip(l["boss_faces"].as_array().into_iter().flatten()).enumerate() {
                if t >= 90 && late >= 20 * k as u32 {
                    let id = b.map_or(roster.ui.question, |(c, _)| c);
                    let xy = |i: usize| at[i].as_f64().unwrap_or(0.0) as f32;
                    crate::modes::face(painter, v, art, roster, id, xy(0), xy(1), 0);
                }
            }
            if late >= 80 {
                place(&l["force"], s(&texts["force"]), None);
                let n = ((late - 80) / 2).min(self.kills);
                place(&l["count"], &crate::art::printf(s(&l["count"]["text"]), &[n]), None);
            }
        } else if self.flags & 8 != 0 {
            if t >= 90 {
                let (saved, name) = (s(&texts["saved"][0]), s(&texts["saved"][1]));
                let p = &l["saved"];
                place(&p[0], saved, None);
                let t = crate::modes::Text::from(&p[1]);
                art.text(painter, v, t.font, blink(&p[1]).unwrap_or(t.colour), t.x + art.text_w(t.font, saved), t.y, name, Color32::WHITE);
            }
        } else {
            if t >= 90 {
                place(&l["keys"], s(&texts["keys"]), None);
            }
            for (k, at) in [90, 120, 150].into_iter().enumerate() {
                if t >= at && self.flags & (1 << k) != 0 {
                    place(&l["metals"][k], s(&texts["metals"][k]), None);
                }
            }
            if t >= 180 && t & 0x30 != 0 {
                place(&l["to_be_continued"], s(&texts["to_be_continued"]), None);
            }
        }
        if self.fade < 256 {
            let a = (255 - self.fade.clamp(0, 255)) as u8;
            Art::shade(painter, v, crate::art::FULL, [Color32::from_black_alpha(a); 4]);
        }
    }
}
