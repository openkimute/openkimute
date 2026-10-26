//! The AI playing Force (OpenKumite's own: `--ai` seats): stage 1 as the app plays it (the soldiers at the default
//! difficulty, the AI seats hard, two soldiers a player in co-op, the bosses when the soldiers are done, a down AI
//! 2P getting up again) on the real converted assets (skipped without them).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests: a failure stops them

use kumite_assets::Assets;
use kumite_game::bout::{Bout, Phase};
use kumite_game::force::{ForceData, ForceEvent, ForceStage};
use kumite_game::{Actor, Game};
use kumite_sim::ai::{Ai, Style};
use kumite_sim::input::Pad;
use std::path::PathBuf;
use std::sync::Arc;

fn assets() -> Option<Assets> {
    let root = std::env::var_os("OPENKUMITE_ASSETS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets"));
    let a = Assets::new(&root).map_err(|e| eprintln!("skipping: {e:#}")).ok()?;
    // (an older conversion, without the chickens' sounds: skipped)
    let d: serde_json::Value = kumite_sim::data::load(&a.root.join("gamedata/force.json")).ok()?;
    if !d["constants"]["chicken"].is_object() {
        eprintln!("skipping: an older conversion");
        return None;
    }
    Some(a)
}

/// Character `c`'s model in costume `k` (the first sixteen characters' records lie 8 apart).
fn model(c: usize, k: usize) -> usize {
    71 + 8 * c.min(15) + 4 * k.min(1)
}

/// How a run of stage 1 went.
#[derive(Debug, Default, PartialEq)]
struct Run {
    cleared: bool,
    steps: u32,
    pickups: u32,
    kills: u32,
    score: u32,
    /// The camera's way along the stage (its clear at 30720).
    progress: i32,
    hash: u64,
}

/// Stage 1 with the AI as the player (`p`: character + 10 x costume, playing `styles[0]`) and, given `partner`,
/// as the co-op 2P; `late` frames into the fight before the AI seats take over (another run of the same pick).
fn play(a: &Assets, p: usize, partner: Option<usize>, styles: [Style; 2], late: usize) -> Run {
    let data: Arc<ForceData> = Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let soldier = |k: usize| 259 + 4 * data.soldier_costumes[0][k].min(3);
    let soldiers = [0, 1].map(|k| Actor::load(a, soldier(k)).unwrap());
    let mut game = Game::load(a, [model(p % 10, p / 10), soldier(0)]).unwrap();
    game.round_frames = kumite_game::force::CLOCK;
    (game.ko_collapse, game.fight.ko_collapse) = (false, false);
    let mut bout = Bout::new(game, kumite_game::ai_data(a).unwrap(), 1, false);
    bout.force = true;
    let (d, l) = (1, data.ai_level(0));
    bout.set_cpu(1, Some((d, l)));
    bout.extra_cpu = Some(Ai::new(d, l, 1));
    let mut st = ForceStage::new(data.clone(), 0, soldiers, 4 * (p % 10) + p / 10, p % 10, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    let join = |bout: &mut Bout, q: usize| {
        bout.game.join_partner(&Actor::load(a, model(q % 10, q / 10)).unwrap());
        bout.game.fight.partner.as_mut().unwrap().guard_input = false;
    };
    if let Some(q) = partner {
        join(&mut bout, q);
        st.widen(&mut bout.game);
        let n = bout.game.fight.more.len();
        bout.more_cpu = (4..4 + n).map(|k| (!kumite_sim::Fight::is_player(k)).then(|| Ai::new(d, l, 1))).collect();
        bout.more_human = vec![None; n];
    }
    while bout.phase != Phase::Fight {
        bout.step([Pad::default(); 2]);
    }
    for _ in 0..late {
        bout.step([Pad::default(); 2]);
    }
    // (as the app's AI seats: a CPU's guard)
    bout.set_cpu(0, Some((2, 9)));
    bout.cpu[0] = Some(Ai::new(2, 9, 0).with_style(styles[0]));
    bout.partner_cpu = partner.map(|_| Ai::new(2, 9, 0).with_style(styles[1]));
    let mut r = Run::default();
    for _ in 0..60 * 60 * 10 {
        bout.step([Pad::default(); 2]);
        let fighting = bout.phase == Phase::Fight;
        let won = matches!(bout.phase, Phase::RoundOver { winner: Some(0) } | Phase::MatchOver { winner: Some(0) });
        let waiting = st.chickens.map(|c| c.state == 1);
        st.step(&mut bout.game, fighting, won);
        r.pickups += st.chickens.iter().zip(waiting).filter(|(c, w)| *w && c.state == 2).count() as u32;
        for e in std::mem::take(&mut st.events) {
            match e {
                ForceEvent::Swapped => bout.swap_cpus(),
                // (co-op: a boss each, the next stage's beside this one's)
                ForceEvent::LoadBoss(ch, cos) => {
                    let picks: Vec<(usize, usize)> = (0..bout.game.players().len()).map(|j| if j == 0 { (ch, cos) } else { data.boss(4 * (p % 10) + p / 10, p % 10, j) }).collect();
                    let actors: Vec<Actor> = picks.iter().map(|&(c, k)| Actor::load(a, model(c, k)).unwrap()).collect();
                    if picks.len() == 1 {
                        st.set_boss(&mut bout.game, &actors[0], (ch, cos));
                    } else {
                        st.set_bosses(&mut bout.game, &actors, &picks);
                        bout.extra_cpu = Some(Ai::new(d, l, 1));
                    }
                    bout.set_cpu(1, Some((d, l)));
                }
                _ => {}
            }
        }
        // the AI 2P down gets up again (the app's START for it)
        if let (Some(q), Phase::Fight) = (partner, bout.phase) {
            if bout.game.fight.partner.as_ref().is_some_and(|p| p.health <= 0) {
                join(&mut bout, q);
            }
        }
        r.hash = r.hash.rotate_left(5) ^ bout.game.state_hash();
        r.steps += 1;
        // (a time up is lost too)
        if st.tally_done || bout.game.fight.clock == 0 || matches!(bout.phase, Phase::RoundOver { winner: Some(1) | None } | Phase::MatchOver { winner: Some(1) | None }) {
            break;
        }
    }
    (r.cleared, r.kills, r.score, r.progress) = (st.tally_done, st.kills_total, st.score, st.cam_x);
    r
}

/// Run `k` of a survey: every character, style and costume once in 160 runs (the partner another character in
/// the other costume), then again a little later into the fight.
fn pick(k: usize) -> (usize, usize, [Style; 2], usize) {
    let (p, q) = (k % 10 + 10 * (k / 80 % 2), (k + 1 + k / 10 % 9 + k / 160) % 10 + 10 * (1 - k / 80 % 2));
    (p, q, [Style::ALL[k / 10 % 8], Style::ALL[(k / 10 + k % 10 + 3) % 8]], k / 160 * 7)
}

#[test]
fn a_hurt_ai_player_goes_for_the_chicken() {
    let Some(a) = assets() else { return };
    let data: Arc<ForceData> = Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let soldiers = [0, 1].map(|k| Actor::load(&a, 259 + 4 * data.soldier_costumes[0][k].min(3)).unwrap());
    let mut game = Game::load(&a, [model(0, 0), 259]).unwrap();
    game.round_frames = kumite_game::force::CLOCK;
    let mut bout = Bout::new(game, kumite_game::ai_data(&a).unwrap(), 1, false);
    bout.force = true;
    bout.set_cpu(0, Some((2, 9)));
    let mut st = ForceStage::new(data, 0, soldiers, 0, 0, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    // hurt, a chicken ahead and off the line (game z: towards the camera); no soldier yet
    bout.game.fight.fighters[0].health = 40 * kumite_sim::ONE;
    st.drop_chicken(1200, -1400);
    let mut got = None;
    for frame in 0..60 * 8 {
        bout.step([Pad::default(); 2]);
        st.step(&mut bout.game, bout.phase == Phase::Fight, false);
        if st.chickens[0].state == 2 {
            got = Some(frame);
            break;
        }
    }
    let f = &bout.game.fight.fighters[0];
    assert!(got.is_some() && f.health == 90 * kumite_sim::ONE, "the chicken taken ({got:?}), health {} at {:?}", f.health / kumite_sim::ONE, f.root());
}

/// Alone on stage 1: the AI clears it more often and takes the chickens (before the Force layer these 48 runs
/// cleared 5, took 28 chickens and went 23000 on average; with it 16, 108 and 25800).
#[test]
fn the_ai_player_gets_on_through_stage_1() {
    let Some(a) = assets() else { return };
    let runs: Vec<Run> = (0..48).map(|k| pick(k * 10 + 3)).map(|(p, _, s, late)| play(&a, p, None, s, late)).collect();
    let progress = runs.iter().map(|r| r.progress).sum::<i32>() / runs.len() as i32;
    let cleared = runs.iter().filter(|r| r.cleared).count();
    let pickups: u32 = runs.iter().map(|r| r.pickups).sum();
    eprintln!("48 runs: {cleared} cleared, {progress} on average, {pickups} chickens");
    assert!(cleared >= 10 && pickups >= 60, "{cleared} cleared, {pickups} chickens");
}

#[test]
fn a_co_op_run_plays_the_same_again() {
    let Some(a) = assets() else { return };
    let (p, q, s, late) = pick(17);
    let r = play(&a, p, Some(q), s, late);
    assert!(r.steps > 600 && r.kills > 0, "{r:?}");
    assert_eq!(r, play(&a, p, Some(q), s, late));
}

/// The survey (`cargo test --release -p kumite_game --test force_ai -- --ignored --nocapture`, `N` runs a
/// config, default 160): cleared, steps, chickens and score, alone and in co-op.
#[test]
#[ignore]
fn survey() {
    let Some(a) = assets() else { return };
    let n: usize = std::env::var("N").ok().and_then(|s| s.parse().ok()).unwrap_or(160);
    for coop in [false, true] {
        let runs: Vec<Run> = (0..n).map(pick).map(|(p, q, s, late)| play(&a, p, coop.then_some(q), s, late)).collect();
        let sum = |f: fn(&Run) -> u32| runs.iter().map(f).sum::<u32>();
        eprintln!(
            "{}: cleared {}/{n}, steps {} on average, chickens {}, score {} on average",
            if coop { "co-op" } else { "alone" },
            sum(|r| r.cleared as u32),
            sum(|r| r.steps) / n as u32,
            sum(|r| r.pickups),
            sum(|r| r.score) / n as u32
        );
    }
}
