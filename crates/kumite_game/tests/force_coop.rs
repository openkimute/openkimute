//! Force's co-op (OpenKumite's own): the second player beside the player on the real converted assets.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests and tools: a failure stops them

use kumite_assets::Assets;
use kumite_game::bout::{Bout, Phase};
use kumite_game::force::{ForceData, ForceEvent, ForceStage};
use kumite_game::{Actor, Game};
use kumite_sim::input::Pad;
use std::path::PathBuf;
use std::sync::Arc;

fn assets() -> Option<Assets> {
    let root = std::env::var_os("OPENKUMITE_ASSETS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets"));
    Assets::new(&root).map_err(|e| eprintln!("skipping: {e:#}")).ok()
}

const MODEL_131: usize = 131;
const MODEL_71: usize = 71;

#[test]
fn the_partner_fights_the_soldiers_beside_the_player() {
    let Some(a) = assets() else { return };
    let data: Arc<ForceData> = Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let costumes = data.soldier_costumes[0];
    let soldiers = [0, 1].map(|k| Actor::load(&a, 259 + 4 * costumes[k].min(3)).unwrap());
    let mut game = Game::load(&a, [MODEL_131, 259 + 4 * costumes[0].min(3)]).unwrap();
    game.round_frames = kumite_game::force::CLOCK;
    (game.ko_collapse, game.fight.ko_collapse) = (false, false);
    let mut bout = Bout::new(game, kumite_game::ai_data(&a).unwrap(), 1, false);
    bout.force = true;
    bout.set_cpu(1, Some((1, 0)));
    bout.extra_cpu = Some(kumite_sim::ai::Ai::new(1, 0, 1));
    let mut st = ForceStage::new(data, 0, soldiers, 7 * 4, 7, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    bout.game.join_partner(&Actor::load(&a, MODEL_71).unwrap());
    let (mut partner_hits, mut partner_struck, mut player_hits) = (0, 0, 0);
    for frame in 0..60 * 50 {
        // both walk on and punch
        let pad = |k: u32| {
            if (frame + k) % 24 < 3 {
                Pad { buttons: 1, ..Pad::default() }
            } else if frame % 120 < 60 {
                Pad { right: true, ..Pad::default() }
            } else {
                Pad::default()
            }
        };
        bout.partner_pad = pad(7);
        bout.step([pad(0), Pad::default()]);
        let fighting = bout.phase == Phase::Fight;
        let won = matches!(bout.phase, Phase::RoundOver { winner: Some(0) } | Phase::MatchOver { winner: Some(0) });
        st.step(&mut bout.game, fighting, won);
        if st.events.contains(&ForceEvent::Swapped) {
            bout.swap_cpus();
        }
        for h in &bout.game.fight.hits {
            let player = |i: usize| i == 0 || i == 3;
            assert_ne!(player(h.attacker), player(h.victim), "no hits between friends: {h:?}");
            partner_hits += usize::from(h.attacker == 3);
            partner_struck += usize::from(h.victim == 3);
            player_hits += usize::from(h.attacker == 0);
        }
        let p = bout.game.fight.partner.as_ref().unwrap();
        assert!(p.position.is_finite());
        if bout.finished() {
            break;
        }
    }
    println!("partner hits {partner_hits}, struck {partner_struck}; player hits {player_hits}; score {}", st.score);
    assert!(partner_hits > 0, "the partner lands hits on the soldiers");
    assert!(partner_struck > 0, "the soldiers take the partner on too");
    assert!(player_hits > 0);
}

#[test]
fn the_round_goes_on_while_one_player_stands() {
    let Some(a) = assets() else { return };
    let data: Arc<ForceData> = Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let costumes = data.soldier_costumes[0];
    let soldiers = [0, 1].map(|k| Actor::load(&a, 259 + 4 * costumes[k].min(3)).unwrap());
    let game = Game::load(&a, [MODEL_131, 259 + 4 * costumes[0].min(3)]).unwrap();
    let mut bout = Bout::new(game, kumite_game::ai_data(&a).unwrap(), 1, false);
    bout.force = true;
    let mut st = ForceStage::new(data, 0, soldiers, 7 * 4, 7, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    bout.game.join_partner(&Actor::load(&a, MODEL_71).unwrap());
    for _ in 0..200 {
        bout.step([Pad::default(); 2]);
        st.step(&mut bout.game, bout.phase == Phase::Fight, false);
    }
    assert_eq!(bout.phase, Phase::Fight);
    bout.game.fight.fighters[0].health = 0;
    for _ in 0..30 {
        bout.step([Pad::default(); 2]);
    }
    assert_eq!(bout.phase, Phase::Fight, "the partner still stands");
    bout.game.fight.partner.as_mut().unwrap().health = 0;
    for _ in 0..30 {
        bout.step([Pad::default(); 2]);
    }
    assert!(!matches!(bout.phase, Phase::Fight | Phase::Call), "both down: the run is lost ({:?})", bout.phase);
    // a down partner comes back at full health
    bout.game.join_partner(&Actor::load(&a, MODEL_71).unwrap());
    let p = bout.game.fight.partner.as_ref().unwrap();
    assert_eq!(p.health, p.max_health * kumite_sim::ONE);
}

#[test]
fn the_sounds_and_effects_skip_opponents_past_the_four_bodies() {
    let Some(a) = assets() else { return };
    let data: Arc<ForceData> = Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let costumes = data.soldier_costumes[0];
    let soldiers = [0, 1].map(|k| Actor::load(&a, 259 + 4 * costumes[k].min(3)).unwrap());
    let game = Game::load(&a, [MODEL_131, 259 + 4 * costumes[0].min(3)]).unwrap();
    let mut bout = Bout::new(game, kumite_game::ai_data(&a).unwrap(), 1, false);
    bout.force = true;
    let mut st = ForceStage::new(data, 0, soldiers, 7 * 4, 7, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    st.widen(&mut bout.game);
    bout.game.join_partner(&Actor::load(&a, MODEL_71).unwrap());
    bout.game.join_player(2, &Actor::load(&a, MODEL_71).unwrap());
    let sound = kumite_game::sound::SoundData::load(&a).unwrap();
    let mut vfx = kumite_game::vfx::Vfx::new(&a);
    let mut state = kumite_game::sound::SoundState::default();
    // a timed sound script with an entry for the opponent (cmd 2)
    let scripts: serde_json::Value = kumite_sim::data::load(&a.root.join("gamedata/sound.json")).unwrap();
    let scripts = scripts["scripts"]["entries"].as_array().unwrap();
    let (k, frame) = scripts.iter().enumerate().find_map(|(k, s)| s.as_array()?.iter().find(|e| e[0] == 2).map(|e| (k, e[1].as_u64().unwrap() as u32))).unwrap();
    for _ in 0..120 {
        bout.step([Pad::default(); 2]);
        st.step(&mut bout.game, bout.phase == Phase::Fight, false);
        // everybody faces a soldier of the widened melee, the player at a spark ring for its opponent (op 7)
        let m = bout.game.fight.coop.as_mut().unwrap();
        assert!(m.actors.len() > 6);
        m.links.clear();
        m.target.iter_mut().for_each(|t| *t = 6);
        bout.game.fight.fighters[0].effects.push(kumite_sim::MoveEffect::AtBone { op: 7, slot: 0 });
        state.script = vec![Some((k, frame - 1)); 4];
        state.last.clear();
        sound.step_with_partner(&bout, &mut state, ["m131", "m259"], [0, 1], Some(("m071", 2)), None);
        vfx.step(&bout.game);
    }
}

/// A soldier that went down held in a throw (still in its thrower's victim move, bank Partner) comes back as a fresh
/// one when its slot spawns the same kind again: before, it kept that move with no thrower to end it and stood
/// frozen out of reach, so the stage waited for it until the clock ran out.
#[test]
fn a_soldier_that_went_down_in_a_throw_comes_back_fresh() {
    let Some(a) = assets() else { return };
    let data: Arc<ForceData> = Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let costumes = data.soldier_costumes[0];
    let soldiers = [0, 1].map(|k| Actor::load(&a, 259 + 4 * costumes[k].min(3)).unwrap());
    let game = Game::load(&a, [MODEL_131, 259 + 4 * costumes[0].min(3)]).unwrap();
    let mut bout = Bout::new(game, kumite_game::ai_data(&a).unwrap(), 1, false);
    bout.force = true;
    let mut st = ForceStage::new(data, 0, soldiers, 7 * 4, 7, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    // slot 0's soldier in a throw victim's move, then the slot spawns the same kind again (state 4)
    let actor = st.slot_actor(0);
    bout.game.fight.actor_mut(actor).unwrap().current = kumite_sim::Current { bank: kumite_sim::data::Bank::Partner, index: 0 };
    st.slots[0].state = 4;
    st.step(&mut bout.game, true, false);
    let f = bout.game.fight.actor(actor).unwrap();
    assert!(!f.inactive && f.health > 0, "the soldier is in play");
    assert_ne!(f.current.bank, kumite_sim::data::Bank::Partner, "a fresh soldier, not one still held in the throw");
}

/// Tekken 2's movesets (Bruce here) do not turn to a target out of play: with no soldier about, Force turns such
/// a player to the way on, as a Tekken 3 stance does (before, one turned away walked "forward" into the camera's
/// side for good).
#[test]
fn a_tekken_2_player_with_no_one_to_fight_turns_to_the_way_on() {
    let Some(a) = assets() else { return };
    const T2_BRUCE: usize = 940;
    let data: Arc<ForceData> = Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let costumes = data.soldier_costumes[0];
    let soldiers = [0, 1].map(|k| Actor::load(&a, 259 + 4 * costumes[k].min(3)).unwrap());
    let game = Game::load(&a, [T2_BRUCE, 259 + 4 * costumes[0].min(3)]).unwrap();
    let mut bout = Bout::new(game, kumite_game::ai_data(&a).unwrap(), 1, false);
    bout.force = true;
    let mut st = ForceStage::new(data, 0, soldiers, 34 * 4, 34, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    let away = -std::f32::consts::FRAC_PI_2;
    let f = &mut bout.game.fight.fighters[0];
    (f.yaw, f.root_yaw) = (away, away);
    for _ in 0..60 {
        bout.step([Pad::default(); 2]);
        st.step(&mut bout.game, false, false);
    }
    let f = &bout.game.fight.fighters[0];
    let facing = glam::Vec3::new(f.yaw.sin(), 0.0, f.yaw.cos());
    assert!(facing.dot(bout.game.fight.screen_right) > 0.9, "faces the way on: yaw {}", f.yaw);
}

/// Force co-op's balance: one soldier a player in play at once (two a player, four at once, overran a pair of CPU
/// players on every stage past the first), with the stage's soldiers coming on as before.
#[test]
fn co_op_has_a_soldier_a_player_in_play() {
    let Some(a) = assets() else { return };
    let data: Arc<ForceData> = Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let costumes = data.soldier_costumes[0];
    let soldiers = [0, 1].map(|k| Actor::load(&a, 259 + 4 * costumes[k].min(3)).unwrap());
    let mut game = Game::load(&a, [MODEL_131, 259 + 4 * costumes[0].min(3)]).unwrap();
    game.round_frames = kumite_game::force::CLOCK;
    let mut bout = Bout::new(game, kumite_game::ai_data(&a).unwrap(), 1, false);
    bout.force = true;
    let mut st = ForceStage::new(data, 1, soldiers, 7 * 4, 7, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    st.widen(&mut bout.game);
    bout.game.join_partner(&Actor::load(&a, MODEL_71).unwrap());
    let (mut most, mut spawned) = (0, false);
    for frame in 0..60 * 30 {
        // both walk on, so the stage's soldiers come
        let pad = if frame % 120 < 90 { Pad { right: true, ..Pad::default() } } else { Pad::default() };
        bout.partner_pad = pad;
        bout.step([pad, Pad::default()]);
        st.step(&mut bout.game, bout.phase == Phase::Fight, false);
        let f = &bout.game.fight;
        let in_play = (0..f.actor_count()).filter(|&i| !kumite_sim::Fight::is_player(i) && f.actor(i).is_some_and(|s| !s.inactive)).count();
        most = most.max(in_play);
        spawned |= in_play > 0;
    }
    assert!(spawned, "soldiers came on");
    assert!(most <= 2, "a pair has at most two soldiers at once: {most}");
}
