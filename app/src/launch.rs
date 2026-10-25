//! Debugging from the command line: `--mode NAME` starts a mode straight away, past the menus, with the fighters
//! and costumes, the stage, the difficulty and how far into the mode (an arcade or Time Attack stage, the Survival
//! wins, a Tag Arcade pair); for Force its stage (the original's four, the final one, OpenKumite's extra
//! ones), straight to the boss, the keys held and a co-op partner.
//!
//! ```text
//! --mode force --player NAME --force-stage 5 --boss
//! --mode arcade --player 0 --costume 2 --opponent 14 --level 10 --difficulty hard
//! --mode tag-free --fighters NAME,ID,PREFIX:NAME,ID:2 --stage ID
//! ```
//!
//! `--ai SEATS` (1,2; 1,2,3,4 in the four's modes) has the AI play those seats, `--ai-style STYLE` or
//! `--ai-style 1:STYLE,2:STYLE` how (`random`: one a seat from the game's seed), `--ai-difficulty easy|medium|hard`
//! and `--ai-level 0..9` how well. Offline only.

use crate::menu::Roster;
use anyhow::{bail, Context, Result};
use kumite_sim::ai::Style;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchMode {
    Arcade,
    Versus,
    Practice,
    TimeAttack,
    Survival,
    Ball,
    Force,
    TagFree,
    TagTeams,
    TagArcade,
    ExtraLevels,
}

impl LaunchMode {
    /// (flag value, mode)
    pub const NAMES: [(&'static str, LaunchMode); 11] = [
        ("arcade", LaunchMode::Arcade),
        ("vs", LaunchMode::Versus),
        ("practice", LaunchMode::Practice),
        ("time-attack", LaunchMode::TimeAttack),
        ("survival", LaunchMode::Survival),
        ("ball", LaunchMode::Ball),
        ("force", LaunchMode::Force),
        ("tag-free", LaunchMode::TagFree),
        ("tag-teams", LaunchMode::TagTeams),
        ("tag-arcade", LaunchMode::TagArcade),
        ("levels", LaunchMode::ExtraLevels),
    ];
}

/// What to start.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    pub mode: LaunchMode,
    /// Roster index and costume: P1, P2 (the opponent; Practice: the dummy; Force: the co-op partner), the tag
    /// brawl's 3rd and 4th (Ball 2 ON 2: P1's and P2's partners).
    pub players: [(usize, usize); 4],
    /// Which of `players` were given (the others are the mode's own choice).
    pub given: [bool; 4],
    /// A stage id (stages.json), or None: the mode's own.
    pub stage: Option<usize>,
    pub difficulty: usize,
    /// Arcade / Time Attack stage, Tag Arcade pair (0-based); Survival: the wins so far.
    pub level: usize,
    /// P2 played by the CPU (VS, Ball; the tag brawl: every slot past the first, the arcade's 2P).
    pub p2_cpu: bool,
    /// Force: the stage's number less one (past the original's stages, the extra stages); None: the final stage.
    pub force_stage: Option<usize>,
    pub force_boss: bool,
    pub force_coop: bool,
    pub force_keys: u32,
    /// `--ai`: the seats the AI plays (1P..4P), each its style (None: `random`, from the game's seed).
    pub ai: [bool; 4],
    pub ai_style: [Option<Style>; 4],
    pub ai_difficulty: usize,
    pub ai_level: usize,
}

impl LaunchMode {
    /// The seats an AI can play (`--ai`): 1P and 2P; the four of the tag brawl and Ball 2 ON 2 (3P beside 1P, 4P
    /// beside 2P); Force's player and its co-op partner (shortcut: not 3P and 4P, who join with START on the way;
    /// if a demo needs them, join them at the stage's start). Practice: none (its dummy is the practice's own).
    fn ai_seats(self, doubles: bool, coop: bool) -> usize {
        match self {
            LaunchMode::Practice => 0,
            LaunchMode::Force => 1 + usize::from(coop),
            LaunchMode::TagFree | LaunchMode::TagTeams | LaunchMode::TagArcade => 4,
            LaunchMode::Ball if doubles => 4,
            _ => 2,
        }
    }
}

/// A difficulty flag's value: easy, medium or hard (0..2).
fn difficulty_of(name: &str, d: Option<&str>, default: usize) -> Result<usize> {
    Ok(match d {
        None => default,
        Some("easy") | Some("0") => 0,
        Some("medium") | Some("1") => 1,
        Some("hard") | Some("2") => 2,
        Some(d) => bail!("{name} {d}: easy, medium or hard (0..2)"),
    })
}

/// A character by id, or by name (case, spaces, dots and dashes aside; one without a prefix before one of the
/// same name with a prefix, roster.json `prefix`: `PREFIX:NAME` picks that one), with `:COSTUME` (1..) after it.
pub fn fighter(roster: &Roster, spec: &str) -> Result<(usize, usize)> {
    let (name, costume) = match spec.rsplit_once(':').filter(|(_, c)| c.parse::<usize>().is_ok()) {
        Some((n, c)) => (n, c.parse::<usize>().unwrap_or(1).max(1) - 1),
        None => (spec, 0),
    };
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let (prefix, name) = match name.split_once(':').filter(|(p, _)| roster.characters.iter().any(|c| !c.prefix.is_empty() && c.prefix == *p)) {
        Some((p, n)) => (p, n),
        None => ("", name),
    };
    let want = norm(name);
    let found: Vec<usize> = roster.characters.iter().enumerate().filter(|(_, c)| name.parse::<usize>().ok() == Some(c.id) || norm(&c.name) == want).map(|(i, _)| i).collect();
    // (the asked prefix's fighter first; a name only one has needs no prefix)
    let idx = found.iter().copied().find(|&i| roster.characters[i].prefix == prefix).or_else(|| found.first().copied().filter(|_| prefix.is_empty()));
    let idx = idx.with_context(|| {
        let names: Vec<String> = roster.characters.iter().map(|c| if c.prefix.is_empty() { norm(&c.name) } else { format!("{}:{}", c.prefix, norm(&c.name)) }).collect();
        format!("no character {spec:?} (names: {})", names.join(", "))
    })?;
    Ok((idx, costume.min(roster.characters[idx].costumes.len().saturating_sub(1))))
}

/// The launch the command line asks for (`--mode`), or None.
pub fn from_args(args: &[String], roster: &Roster) -> Result<Option<LaunchSpec>> {
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let flag = |name: &str| args.iter().any(|a| a == name);
    if flag("--ai") && (flag("--online") || flag("--host") || flag("--join")) {
        bail!("--ai plays offline only: not with --online, --host or --join");
    }
    let Some(m) = arg("--mode") else {
        if flag("--ai") {
            bail!("--ai needs --mode");
        }
        return Ok(None);
    };
    let names: Vec<&str> = LaunchMode::NAMES.iter().map(|n| n.0).collect();
    let mode = LaunchMode::NAMES.iter().find(|n| n.0 == m).map(|n| n.1).with_context(|| format!("--mode {m}: one of {}", names.join(", ")))?;
    let mut players = [(0, 0), (1, 0), (2, 0), (3, 0)];
    let mut given = [false; 4];
    // the fighters: --fighters A,B,C,D, or --player / --opponent with --costume / --costume2
    if let Some(list) = arg("--fighters") {
        for (k, f) in list.split(',').filter(|f| !f.is_empty()).take(4).enumerate() {
            (players[k], given[k]) = (fighter(roster, f)?, true);
        }
    }
    for (k, name, costume) in [(0, "--player", "--costume"), (1, "--opponent", "--costume2")] {
        if let Some(p) = arg(name) {
            players[k] = fighter(roster, &p)?;
            given[k] = true;
        }
        if let Some(c) = arg(costume) {
            let c: usize = c.parse().with_context(|| format!("{costume} 1.."))?;
            let n = roster.characters.get(players[k].0).map_or(1, |c| c.costumes.len().max(1));
            players[k].1 = c.clamp(1, n) - 1;
        }
    }
    let difficulty = difficulty_of("--difficulty", arg("--difficulty").as_deref(), 1)?;
    let level = match arg("--level") {
        Some(l) => {
            let n: usize = l.parse().context("--level N")?;
            if mode == LaunchMode::Survival {
                n
            } else {
                n.max(1) - 1
            }
        }
        None => 0,
    };
    let stage = arg("--stage").map(|s| s.parse::<usize>().context("--stage ID (stages.json)")).transpose()?;
    // Force: --force-stage N (1.., the extra stages after the original's), or final
    let force_stage = match arg("--force-stage").as_deref() {
        None => Some(0),
        Some("final") => None,
        Some(n) => match n.parse::<usize>() {
            Ok(n @ 1..) => Some(n - 1),
            _ => bail!("--force-stage {n}: 1.. or final"),
        },
    };
    // the AI's seats, their styles (one for all, or SEAT:STYLE each), how well they play
    let mut ai = [false; 4];
    let seats = mode.ai_seats(given[2] && given[3], flag("--coop"));
    for s in arg("--ai").iter().flat_map(|l| l.split(',')) {
        match s.trim().parse::<usize>() {
            Ok(k @ 1..=4) if k <= seats => ai[k - 1] = true,
            Ok(k @ 1..=4) if seats == 0 => bail!("--ai {k}: --mode {m} has no seats the AI can play"),
            Ok(k @ 1..=4) => bail!("--ai {k}: --mode {m} has seats 1..{seats}"),
            _ => bail!("--ai {s}: seats 1..4, as a comma list (--ai 1,2)"),
        }
    }
    let style = |s: &str| match s.trim() {
        "random" => Ok(None),
        s => Style::parse(s).map(Some).with_context(|| format!("--ai-style {s}: {}, or random", Style::ALL.map(Style::name).join(", "))),
    };
    let mut ai_style = [Some(Style::Classic); 4];
    match arg("--ai-style") {
        Some(l) if l.contains(':') => {
            for each in l.split(',') {
                let (k, s) = each.split_once(':').with_context(|| format!("--ai-style {each}: SEAT:STYLE"))?;
                let k = k.trim().parse::<usize>().ok().filter(|k| (1..=4).contains(k) && ai[k - 1]).with_context(|| format!("--ai-style {each}: seat {k} is not one of --ai's"))?;
                ai_style[k - 1] = style(s)?;
            }
        }
        Some(s) => ai_style = [style(&s)?; 4],
        None => {}
    }
    let ai_level = match arg("--ai-level") {
        Some(l) => l.parse().ok().filter(|l| *l <= 9).with_context(|| format!("--ai-level {l}: 0..9"))?,
        None => 9,
    };
    let p2_cpu = match mode {
        LaunchMode::Versus => flag("--p2-cpu") && !ai[1],
        LaunchMode::TagArcade => flag("--p2-cpu"),
        _ => !flag("--p2-human"),
    };
    Ok(Some(LaunchSpec {
        mode,
        players,
        given,
        stage,
        difficulty,
        level,
        p2_cpu,
        force_stage,
        force_boss: flag("--boss"),
        force_coop: flag("--coop"),
        force_keys: arg("--force-keys").and_then(|k| k.parse().ok()).unwrap_or(0),
        ai,
        ai_style,
        ai_difficulty: difficulty_of("--ai-difficulty", arg("--ai-difficulty").as_deref(), 2)?,
        ai_level,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Option<Roster> {
        let gd = crate::test_assets().join("gamedata");
        kumite_fs::exists(&gd.join("screens.json")).then(|| Roster::load(&gd))
    }

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    /// `s` with each `{N}` the name of character N (roster.json's, as a player types it).
    fn named(r: &Roster, s: &str) -> Vec<String> {
        args(s).into_iter().map(|a| r.characters.iter().fold(a, |a, c| a.replace(&format!("{{{}}}", c.id), &c.name.to_lowercase()))).collect()
    }

    #[test]
    fn the_command_line_names_a_mode_its_fighters_and_how_far_in() {
        let Some(r) = roster() else { return };
        let id = |p: (usize, usize)| (r.characters[p.0].id, p.1);
        let s = from_args(&named(&r, "--mode force --player {7} --costume 2 --force-stage 5 --boss --force-keys 3"), &r).unwrap().unwrap();
        assert_eq!((s.mode, id(s.players[0]), s.force_stage, s.force_boss, s.force_keys), (LaunchMode::Force, (7, 1), Some(4), true, 3));
        let s = from_args(&named(&r, "--mode arcade --player 0 --opponent {14} --level 10 --difficulty hard"), &r).unwrap().unwrap();
        assert_eq!((id(s.players[0]).0, id(s.players[1]).0, s.level, s.difficulty, s.given[1]), (0, 14, 9, 2, true));
        assert_eq!(from_args(&args("--mode force --force-stage final"), &r).unwrap().unwrap().force_stage, None);
        assert!(from_args(&args("--mode nothing"), &r).is_err());
        assert!(from_args(&args("--fight"), &r).unwrap().is_none());
        // a fighter of the same name by its prefix, a costume after a colon
        let twin = r.characters.iter().find(|c| !c.prefix.is_empty() && r.by_id(0).is_some_and(|c0| c.id != c0.id && c.name.eq_ignore_ascii_case(&c0.name)));
        if let (Some(twin), Some(c33)) = (twin, r.by_id(33)) {
            let line = format!("--mode tag-free --fighters {{0}},{}:{{0}},{{11}}:2,{}", twin.prefix, c33.name.to_lowercase());
            let s = from_args(&named(&r, &line), &r).unwrap().unwrap();
            let ids: Vec<(usize, usize)> = s.players.iter().map(|&p| id(p)).collect();
            assert_eq!(ids[0], (0, 0));
            assert_eq!(r.characters[s.players[1].0].id, twin.id);
            assert_eq!(ids[2].1, 1);
            assert_eq!(ids[3].0, 33, "a prefixed name without the prefix");
            assert_eq!(s.given, [true; 4]);
        }
    }

    #[test]
    fn the_ai_plays_the_seats_asked_as_asked() {
        let Some(r) = roster() else { return };
        let s = from_args(&args("--mode vs --ai 1,2 --ai-style 1:rushdown,2:random --ai-difficulty easy --ai-level 4"), &r).unwrap().unwrap();
        assert_eq!((s.ai, s.ai_style[0], s.ai_style[1], s.ai_difficulty, s.ai_level), ([true, true, false, false], Some(Style::Rushdown), None, 0, 4));
        let s = from_args(&args("--mode tag-teams --ai 1,2,3,4 --ai-style Thrower"), &r).unwrap().unwrap();
        assert_eq!((s.ai, s.ai_style, s.ai_difficulty, s.ai_level), ([true; 4], [Some(Style::Thrower); 4], 2, 9));
        let s = from_args(&args("--mode arcade --ai 1"), &r).unwrap().unwrap();
        assert_eq!((s.ai, s.ai_style[0]), ([true, false, false, false], Some(Style::Classic)));
        assert!(from_args(&args("--mode force --coop --ai 2"), &r).unwrap().is_some());
        for bad in [
            "--mode vs --ai 3",
            "--mode vs --ai 0",
            "--mode vs --ai one",
            "--mode practice --ai 1",
            "--mode force --ai 2",
            "--mode vs --ai 1 --ai-style kicker",
            "--mode vs --ai 1 --ai-style 2:rushdown",
            "--mode vs --ai 1 --ai-level 10",
            "--mode vs --ai 1 --ai-difficulty insane",
            "--mode vs --ai 1 --online ROOM",
            "--ai 1",
        ] {
            assert!(from_args(&args(bad), &r).is_err(), "{bad}");
        }
    }
}
