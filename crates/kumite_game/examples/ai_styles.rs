//! Each AI style against the game's CPU (Classic), headless, on both sides of several character pairs:
//! `cargo run --release --example ai_styles -- [style ...] [hard level ...]` (levels 7 8 9 by default). Prints its
//! rounds and matches won and what it did.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests and tools: a failure stops them
use kumite_game::bout::{Bout, Phase};
use kumite_game::Game;
use kumite_sim::ai::{Ai, Style};
use kumite_sim::input::Pad;

/// A model of each of the first 14 move sets: each meets the next and the fifth after it.
const MODELS: [usize; 14] = [71, 79, 87, 95, 103, 111, 119, 127, 135, 143, 151, 159, 167, 175];

/// What one seat did in a match.
#[derive(Default, Debug, Clone, Copy)]
struct Counts {
    frames: u32,
    attacks: u32,
    hits: u32,
    taken: u32,
    guarded: u32,
    counters: u32,
    punishes: u32,
    launches: u32,
    juggles: u32,
    throws: u32,
    sidesteps: u32,
}

/// One match of `style` in seat `seat` against Classic; returns (rounds won, rounds lost, match won, the style's counts).
fn play(assets: &kumite_assets::Assets, pair: [usize; 2], seat: usize, style: Style, level: usize) -> anyhow::Result<(u32, u32, bool, Counts)> {
    let mut b = Bout::new(Game::load(assets, pair)?, kumite_game::ai_data(assets)?, 2, false);
    for i in 0..2 {
        b.set_cpu(i, Some((2, level)));
    }
    b.cpu[seat] = Some(Ai::new(2, level, seat).with_style(style));
    let (o, mut c) = (1 - seat, Counts::default());
    let (mut was_move, mut was_chain, mut whiff_at, mut was_air) = (None, false, 0u32, false);
    for step in 0..60 * 60 * 8 {
        if b.finished() {
            break;
        }
        b.step([Pad::default(); 2]);
        c.frames += (b.phase == Phase::Fight) as u32;
        let f = &b.game.fight;
        for h in &f.hits {
            if h.attacker == seat && !h.blocked {
                c.hits += 1;
                c.counters += h.counter as u32;
                c.punishes += (step - whiff_at < 30) as u32;
                // (in the air: the airborne posture)
                let air = f.fighters[o].current_move(&f.data).posture.raw & 0x400 != 0;
                c.juggles += was_air as u32;
                c.launches += (!was_air && air) as u32;
            }
            c.guarded += (h.attacker == o && h.blocked) as u32;
            c.taken += (h.attacker == o && !h.blocked) as u32;
        }
        if f.fighters[o].whiffed {
            whiff_at = step;
        }
        was_air = f.fighters[o].current_move(&f.data).posture.raw & 0x400 != 0;
        let me = &f.fighters[seat];
        // (a new move with active frames: string parts count one by one)
        c.attacks += (was_move != Some(me.current) && me.current_move(&f.data).active[0] != 0) as u32;
        c.throws += (me.chain > 0 && !was_chain) as u32;
        (was_move, was_chain) = (Some(me.current), me.chain > 0);
    }
    c.sidesteps = b.cpu[seat].as_ref().map_or(0, |a| a.sidesteps);
    Ok((b.wins[seat], b.wins[o], b.wins[seat] > b.wins[o], c))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut levels: Vec<usize> = args.iter().filter_map(|a| a.parse().ok()).collect();
    if levels.is_empty() {
        levels = vec![7, 8, 9];
    }
    let mut styles: Vec<Style> = args.iter().filter_map(|a| Style::parse(a)).collect();
    if styles.is_empty() {
        styles = Style::ALL.to_vec();
    }
    let assets = kumite_assets::Assets::new(std::env::var_os("OPENKUMITE_ASSETS").map_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets"), std::path::PathBuf::from))?;
    let pairs: Vec<[usize; 2]> = (0..MODELS.len()).flat_map(|i| [1, 5].map(|k| [MODELS[i], MODELS[(i + k) % MODELS.len()]])).collect();
    println!("hard, levels {levels:?}: rounds / matches won against classic over {} matches; the style's counts per match", pairs.len() * 2 * levels.len());
    for style in styles {
        let (mut rw, mut rl, mut mw, mut t) = (0, 0, 0, Counts::default());
        for (&pair, &level) in pairs.iter().flat_map(|p| levels.iter().map(move |l| (p, l))) {
            for seat in 0..2 {
                let (w, l, won, c) = play(&assets, pair, seat, style, level)?;
                (rw, rl, mw) = (rw + w, rl + l, mw + won as u32);
                t.frames += c.frames;
                t.attacks += c.attacks;
                t.hits += c.hits;
                t.taken += c.taken;
                t.guarded += c.guarded;
                t.counters += c.counters;
                t.punishes += c.punishes;
                t.juggles += c.juggles;
                t.launches += c.launches;
                t.throws += c.throws;
                t.sidesteps += c.sidesteps;
            }
        }
        let n = (pairs.len() * 2 * levels.len()) as f32;
        let per = |x: u32| x as f32 / n;
        println!(
            "{:10} rounds {rw:3}-{rl:<3} ({:3.0} %) matches {mw:2}/{n} | attacks/s {:.2} hits {:.1} taken {:.1} guarded {:.1} counter-hits {:.1} punishes {:.1} launches {:.1} juggles {:.1} throws {:.1} sidesteps {:.1}",
            style.name(),
            100.0 * rw as f32 / (rw + rl).max(1) as f32,
            t.attacks as f32 * 60.0 / t.frames.max(1) as f32,
            per(t.hits),
            per(t.taken),
            per(t.guarded),
            per(t.counters),
            per(t.punishes),
            per(t.launches),
            per(t.juggles),
            per(t.throws),
            per(t.sidesteps)
        );
    }
    Ok(())
}
