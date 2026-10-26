//! The AI styles on the converted assets (skipped without them): Classic is the game's CPU, every style plays the
//! same match again from the same start, and each shows what it is about more than Classic does; a CPU Force
//! player walks on to the soldiers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests: a failure stops them

use kumite_assets::Assets;
use kumite_game::bout::{Bout, Phase};
use kumite_game::Game;
use kumite_sim::ai::{Ai, Style};
use kumite_sim::input::Pad;
use std::path::PathBuf;

fn assets() -> Option<Assets> {
    let root = std::env::var_os("OPENKUMITE_ASSETS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets"));
    Assets::new(&root).map_err(|e| eprintln!("skipping: {e:#}")).ok()
}

/// What the styled seat did in a match, and the match's hashes folded.
#[derive(Default, Debug)]
struct Run {
    hash: u64,
    fight_frames: u32,
    attacks: u32,
    guarded: u32,
    counters: u32,
    throws: u32,
    launches: u32,
    sidesteps: u32,
}

/// A hard (level 9) CPU match of `pair`, seat `seat` playing `style` (None: as `set_cpu` makes it).
fn play(a: &Assets, pair: [usize; 2], seat: usize, style: Option<Style>) -> Run {
    let mut b = Bout::new(Game::load(a, pair).unwrap(), kumite_game::ai_data(a).unwrap(), 2, false);
    b.set_cpu(0, Some((2, 9)));
    b.set_cpu(1, Some((2, 9)));
    if let Some(s) = style {
        b.cpu[seat] = Some(Ai::new(2, 9, seat).with_style(s));
    }
    let (o, mut r) = (1 - seat, Run::default());
    let (mut was_move, mut was_chain, mut was_air) = (None, false, false);
    for _ in 0..60 * 60 * 8 {
        if b.finished() {
            break;
        }
        b.step([Pad::default(); 2]);
        r.hash = r.hash.rotate_left(5) ^ b.game.state_hash();
        r.fight_frames += (b.phase == Phase::Fight) as u32;
        let f = &b.game.fight;
        let air = f.fighters[o].current_move(&f.data).posture.raw & 0x400 != 0;
        for h in &f.hits {
            r.guarded += (h.attacker == o && h.blocked) as u32;
            if h.attacker == seat && !h.blocked {
                r.counters += h.counter as u32;
                r.launches += (!was_air && air) as u32;
            }
        }
        was_air = air;
        let me = &f.fighters[seat];
        r.attacks += (was_move != Some(me.current) && me.current_move(&f.data).active[0] != 0) as u32;
        r.throws += (me.chain > 0 && !was_chain) as u32;
        (was_move, was_chain) = (Some(me.current), me.chain > 0);
    }
    assert!(b.finished(), "{pair:?} {style:?}: the match did not end");
    r.sidesteps = b.cpu[seat].as_ref().map_or(0, |ai| ai.sidesteps);
    r
}

#[test]
fn classic_is_the_games_cpu_and_every_style_plays_its_match_again() {
    let Some(a) = assets() else { return };
    let classic = play(&a, [71, 79], 1, Some(Style::Classic));
    assert_eq!(classic.hash, play(&a, [71, 79], 1, None).hash, "Classic is not the game's CPU");
    for s in Style::ALL.into_iter().skip(1) {
        let r = play(&a, [71, 79], 1, Some(s));
        assert_eq!(r.hash, play(&a, [71, 79], 1, Some(s)).hash, "{s:?} played another match");
        assert_ne!(r.hash, classic.hash, "{s:?} plays as Classic");
    }
}

#[test]
fn each_style_does_its_thing_more_than_classic() {
    let Some(a) = assets() else { return };
    // both seats of six pairs (one bout can be short), totals per style
    let pairs = [[71, 79], [87, 95], [103, 111], [119, 127], [135, 143], [151, 159]];
    let total = |s: Style| {
        let mut t = Run::default();
        for pair in pairs {
            for seat in 0..2 {
                let r = play(&a, pair, seat, Some(s));
                (t.fight_frames, t.attacks, t.guarded, t.counters) = (t.fight_frames + r.fight_frames, t.attacks + r.attacks, t.guarded + r.guarded, t.counters + r.counters);
                (t.throws, t.launches, t.sidesteps) = (t.throws + r.throws, t.launches + r.launches, t.sidesteps + r.sidesteps);
            }
        }
        eprintln!("{:10} {t:?}", s.name());
        t
    };
    let c = total(Style::Classic);
    let rate = |r: &Run| r.attacks as f32 / r.fight_frames.max(1) as f32;
    let more = |s: Style, what: &str, v: f32, base: f32, by: f32| assert!(v >= base * by, "{s:?}: {what} {v} against Classic's {base} (want x{by})");
    more(Style::Rushdown, "attacks a frame", rate(&total(Style::Rushdown)), rate(&c), 1.1);
    more(Style::Defensive, "guarded", total(Style::Defensive).guarded as f32, c.guarded as f32, 1.6);
    more(Style::Counter, "counter hits", total(Style::Counter).counters as f32, c.counters as f32, 1.15);
    more(Style::Thrower, "throws", total(Style::Thrower).throws as f32, c.throws as f32, 2.0);
    more(Style::Evasive, "sidesteps", total(Style::Evasive).sidesteps as f32, c.sidesteps as f32, 1.6);
    more(Style::Juggler, "launches", total(Style::Juggler).launches as f32, c.launches as f32, 1.1);
}

#[test]
fn a_cpu_force_player_walks_on_to_the_soldiers() {
    use kumite_game::force::{ForceData, ForceEvent, ForceStage};
    let Some(a) = assets() else { return };
    let data: std::sync::Arc<ForceData> = std::sync::Arc::new(kumite_sim::data::load(&a.root.join("gamedata/force.json")).unwrap());
    let costumes = data.soldier_costumes[0];
    let soldiers = [0, 1].map(|k| kumite_game::Actor::load(&a, 259 + 4 * costumes[k].min(3)).unwrap());
    let mut game = Game::load(&a, [131, 259 + 4 * costumes[0].min(3)]).unwrap();
    game.round_frames = kumite_game::force::CLOCK;
    (game.ko_collapse, game.fight.ko_collapse) = (false, false);
    let mut bout = Bout::new(game, kumite_game::ai_data(&a).unwrap(), 1, false);
    bout.force = true;
    bout.set_cpu(0, Some((2, 9)));
    bout.set_cpu(1, Some((1, 0)));
    bout.extra_cpu = Some(Ai::new(1, 0, 1));
    let mut st = ForceStage::new(data, 0, soldiers, 7 * 4, 7, 0, 0, 0, 0);
    st.setup(&mut bout.game);
    let x0 = bout.game.fight.fighters[0].position.x;
    let mut hits = 0;
    for _ in 0..60 * 40 {
        bout.step([Pad::default(); 2]);
        let won = matches!(bout.phase, Phase::RoundOver { winner: Some(0) } | Phase::MatchOver { winner: Some(0) });
        st.step(&mut bout.game, bout.phase == Phase::Fight, won);
        if st.events.contains(&ForceEvent::Swapped) {
            bout.swap_cpus();
        }
        hits += bout.game.fight.hits.iter().filter(|h| h.attacker == 0).count();
        if bout.finished() {
            break;
        }
    }
    let x = bout.game.fight.fighters[0].position.x;
    eprintln!("the CPU player went {x0:.0} -> {x:.0}, {hits} hits, score {}", st.score);
    assert!(x > x0 + 2000.0 && hits > 0, "the CPU player went {x0:.0} -> {x:.0} and landed {hits} hits");
}
