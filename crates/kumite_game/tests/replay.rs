//! Replays on the converted assets (skipped without them): a match played from the inputs its tape recorded is the
//! same match, step by step, to its end.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests: a failure stops them

use kumite_assets::Assets;
use kumite_game::bout::{self, Bout, Phase};
use kumite_game::{parse_script, Game};
use kumite_sim::input::Pad;
use std::path::PathBuf;
use std::sync::Arc;

fn assets() -> Option<Assets> {
    let root = std::env::var_os("OPENKUMITE_ASSETS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets"));
    Assets::new(&root).map_err(|e| eprintln!("skipping: {e:#}")).ok()
}

/// A match of `models`, P2 the CPU at `cpu` (difficulty, level), the camera counters at `counters`.
fn bout(a: &Assets, models: [usize; 2], cpu: (usize, usize), counters: [usize; 3]) -> Bout {
    bout::set_counters(counters);
    let mut b = Bout::new(Game::load(a, models).unwrap(), kumite_game::ai_data(a).unwrap(), 2, true);
    b.set_cpu(1, Some(cpu));
    b
}

#[test]
fn a_match_replayed_from_its_tape_is_the_same_match() {
    let Some(a) = assets() else { return };
    let models = [71, 79];
    let (cpu, counters) = ((1, 5), [3, 1, 0]);
    // P1 walks in and attacks over and over, the CPU fights back: the match goes to its end
    let script: Vec<Pad> = parse_script(&["f*20,1,n*6,2,n*6,3,n*8,4,n*10,d+3,n*12,b*10"; 40].join(",")).unwrap();
    let mut live = bout(&a, models, cpu, counters);
    live.tape = Some(Default::default());
    let mut hashes = Vec::new();
    for step in 0..20_000 {
        live.step([script.get(step % script.len()).copied().unwrap_or_default(), Pad::default()]);
        hashes.push(live.game.state_hash());
        if live.finished() {
            break;
        }
    }
    assert!(live.finished(), "the match did not end in 20000 steps");
    assert!(matches!(live.phase, Phase::MatchOver { .. }));
    eprintln!("the match: {} steps ({:.0} s), rounds won {:?}, {:?}", hashes.len(), hashes.len() as f32 / 60.0, live.wins, live.phase);
    let tape = live.tape.take().unwrap();
    assert_eq!(tape.len(), hashes.len());

    // the replay: same setup, the tape's inputs (whatever the pads passed), the same state every step
    let mut replay = bout(&a, models, cpu, counters);
    replay.play(Arc::from(tape.to_vec()));
    for (step, want) in hashes.iter().enumerate() {
        replay.step([Pad { buttons: 15, ..Pad::default() }; 2]);
        assert_eq!(replay.game.state_hash(), *want, "the replay went apart at step {step}");
    }
    assert!(replay.finished());
    assert_eq!(replay.wins, live.wins);
    assert_eq!(replay.played(), hashes.len());
}

#[test]
fn a_rollback_takes_the_tape_back_with_the_match() {
    let Some(a) = assets() else { return };
    let script: Vec<Pad> = parse_script(&["f*15,1,n*5,2,n*9,b*6,3,n*10"; 30].join(",")).unwrap();
    let pad = |s: usize| [script[s % script.len()], Pad::default()];
    let mut b = bout(&a, [71, 79], (1, 3), [0; 3]);
    b.tape = Some(Default::default());
    let mut want = Vec::new();
    for s in 0..1200 {
        // every 7 steps, as online: a copy saved, 4 steps on a wrong guess (P1 idle), the copy back, the 4 again
        if s % 7 == 0 {
            let saved = b.clone();
            for _ in 0..4 {
                b.step([Pad { buttons: 8, ..Pad::default() }, Pad::default()]);
            }
            b = saved;
        }
        b.step(pad(s));
        want.push(pad(s)[0]);
    }
    let tape = b.tape.take().unwrap().to_vec();
    assert_eq!(tape.len(), 1200);
    assert!(tape.iter().zip(&want).all(|(t, w)| t.pads[0] == *w), "the tape kept a mispredicted step");
}

#[test]
fn a_tag_brawl_replayed_from_its_tape_is_the_same_brawl() {
    use kumite_game::tag::{Slot, TagGame};
    let Some(a) = assets() else { return };
    // two people (scripted) and two CPUs, two teams
    let slots = [Slot { record: 71, cpu: None }, Slot { record: 79, cpu: None }, Slot { record: 71, cpu: Some((1, 4)) }, Slot { record: 79, cpu: Some((1, 4)) }];
    let load = || TagGame::load(&a, slots, vec![0, 0, 1, 1], kumite_game::ai_data(&a).unwrap(), 2).unwrap();
    let p1: Vec<Pad> = parse_script(&["f*20,1,n*6,2,n*8,3,n*10,b*8"; 60].join(",")).unwrap();
    let p2: Vec<Pad> = parse_script(&["f*12,4,n*9,d+1,n*14,b*5"; 60].join(",")).unwrap();
    let mut live = load();
    live.tape = Some(Default::default());
    let mut hashes = Vec::new();
    for step in 0..30_000 {
        live.step([p1[step % p1.len()], p2[step % p2.len()], Pad::default(), Pad::default()]);
        hashes.push(live.state_hash());
        if live.finished() {
            break;
        }
    }
    assert!(live.finished(), "the brawl did not end in 30000 steps");
    eprintln!("the brawl: {} steps ({:.0} s), wins {:?}", hashes.len(), hashes.len() as f32 / 60.0, live.wins);
    let tape = live.tape.take().unwrap();
    let mut replay = load();
    replay.play(Arc::from(tape.to_vec()));
    for (step, want) in hashes.iter().enumerate() {
        replay.step([Pad { buttons: 15, ..Pad::default() }; 4]);
        assert_eq!(replay.state_hash(), *want, "the replay went apart at step {step}");
    }
    assert!(replay.finished());
    assert_eq!(replay.wins, live.wins);
}
