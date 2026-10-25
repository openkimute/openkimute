//! An online room (OpenKumite's own): up to four computers, each with one or two players, four players at most in all.
//! The computer that created the room (or hosts by IP) hosts it:
//! - it seats the others as they come (in the order they came, its own players first: P1, P2, ...) and tells
//!   everyone who is in the room;
//! - every player's key on the room's screens goes to it, and it hands them out numbered: every computer applies
//!   the same keys in the same order, so the room screen and the setup screens look alike everywhere;
//! - its own screens decide: the mode (the room screen: its 1P's keys), and the match (the setup's end: START), the
//!   setup it sends to all with a seed; then each computer starts the match on a rollback session of its own.
//!
//! A guest that leaves is let go (a match it plays in ends); the host leaving ends the room.

use crate::art::{Art, Screen as View};
use crate::menu::{sfx, Key};
use crate::netlink::{key_code, key_of, Link, LobbyMsg, Member, Token};
use egui::Color32;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use web_time::Instant;

/// Most players in a room.
pub const MAX_PLAYERS: usize = 4;
/// How long the players may take on a setup screen (seconds); then the host finishes it for them.
pub const SETUP_SECONDS: u32 = 30;
/// The room's event of the time being up (in place of a player).
const TIME_UP: u8 = 255;
/// A nickname's longest (in characters).
pub const MAX_NAME: usize = 16;
/// How many matches ahead of the host's a guest's Ready is noted (a guest goes on alone after a run ends here, a
/// few matches at most); a new match's number jumps past them all.
const READY_AHEAD: u32 = 16;
/// A guest's keys the host takes in one poll (a frame) at most, and a guest's keys that may wait their turn.
const MAX_KEYS: usize = 16;
const MAX_WAITING: usize = 256;
/// How far ahead of the next one a guest keeps the host's numbered keys.
const EARLY_WINDOW: u32 = 4096;

/// The host's list of the room: at most four computers, one or two players each, four players in all, no token twice.
fn members_ok(members: &[Member]) -> bool {
    let players: usize = members.iter().map(|m| m.locals as usize).sum();
    members.len() <= MAX_PLAYERS
        && players <= MAX_PLAYERS
        && members.iter().all(|m| (1..=2).contains(&m.locals) && m.name.chars().count() <= MAX_NAME && m.token % 2 == 1)
        && members.iter().enumerate().all(|(i, m)| members[..i].iter().all(|o| o.token != m.token))
}

/// What the room plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnlineMode {
    Versus,
    Ball,
    Ball2,
    TagFree,
    TagTeams,
    TagArcade,
    Force,
}

impl OnlineMode {
    pub const ALL: [OnlineMode; 7] = [OnlineMode::Versus, OnlineMode::Ball, OnlineMode::Ball2, OnlineMode::TagFree, OnlineMode::TagTeams, OnlineMode::TagArcade, OnlineMode::Force];

    /// Its title (the Ball's and the Force's names: screens.json `menu`).
    pub fn name(self, screens: &serde_json::Value) -> String {
        let ball = screens["menu"]["ball"].as_str().unwrap_or_default();
        match self {
            OnlineMode::Versus => "VS MODE".into(),
            OnlineMode::Ball => format!("{ball} 1 ON 1"),
            OnlineMode::Ball2 => format!("{ball} 2 ON 2"),
            OnlineMode::TagFree => "TAG FREE FOR ALL".into(),
            OnlineMode::TagTeams => "TAG TWO ON TWO".into(),
            OnlineMode::TagArcade => "TAG ARCADE".into(),
            OnlineMode::Force => screens["menu"]["force"].as_str().unwrap_or_default().into(),
        }
    }

    /// Its setup screen (the same on every computer of the room).
    pub fn screen(self, roster: &crate::menu::Roster, seed: u32, record: crate::grid::VsRecord) -> crate::menu::Screen {
        use crate::menu::Screen;
        use crate::tagrun::{TagMode, TagSelect};
        let tag = |mode| Screen::Tag(Box::new(TagSelect::new(roster, 1, seed, mode)));
        match self {
            OnlineMode::Versus => Screen::Grid(Box::new(crate::grid::GridSelect::new(roster.grid.clone(), 1, record))),
            OnlineMode::Ball => Screen::Grid(Box::new(crate::grid::GridSelect::ball(roster, 1, 0))),
            OnlineMode::Ball2 => tag(TagMode::Ball),
            OnlineMode::TagFree => tag(TagMode::Free),
            OnlineMode::TagTeams => tag(TagMode::Teams),
            OnlineMode::TagArcade => tag(TagMode::Arcade),
            OnlineMode::Force => tag(TagMode::Force),
        }
    }

    /// The players it takes: the fights of two exactly two; the arcade for two at most.
    pub fn fits(self, players: usize) -> bool {
        match self {
            OnlineMode::Versus | OnlineMode::Ball => players == 2,
            OnlineMode::TagArcade => (1..=2).contains(&players),
            _ => (1..=MAX_PLAYERS).contains(&players),
        }
    }
}

/// What starts a match, as the host sends it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MatchSetup {
    Fight(crate::menu::FightSetup),
    Tag(crate::tagrun::TagSetup),
}

impl MatchSetup {
    /// Whether a host's setup holds only what this computer's menus could have made: it comes from another
    /// computer, and its numbers index the roster, the stages, the ladders.
    pub fn valid(&self, roster: &crate::menu::Roster) -> bool {
        use crate::menu::Mode;
        match self {
            MatchSetup::Fight(f) => matches!(f.mode, Mode::Versus | Mode::Ball) && roster.is_stage(f.stage) && self.numbers_valid(roster),
            MatchSetup::Tag(_) => self.numbers_valid(roster),
        }
    }

    /// Whether its numbers index only what there is (the roster, the ladders, the CPU's levels), whatever its mode
    /// and stage: a replay's setup, from a file someone shared, too (a stage not there loads as a plain floor).
    pub fn numbers_valid(&self, roster: &crate::menu::Roster) -> bool {
        use crate::menu::Mode;
        let pick = |(c, costume): (usize, usize)| roster.characters.get(c).is_some_and(|ch| costume < ch.costumes.len());
        let player = |p: Option<usize>| p.is_none_or(|p| p < MAX_PLAYERS);
        match self {
            MatchSetup::Fight(f) => {
                let ball = f.mode == Mode::Ball;
                f.players.iter().all(|&p| pick(p) || ball && p.0 == crate::menu::CPU_SIDE)
                    && f.difficulty < crate::menu::DIFFICULTIES.len()
                    && f.level < crate::menu::CPU_LEVELS
                    && f.handicap.iter().all(|&h| roster.grid.allows(h))
                    && f.ball.is_none_or(|(kind, side)| kind < roster.ball_types() && side < 2)
                    && f.partners.is_none_or(|d| d.partners.iter().all(|&p| pick(p)) && d.seats.iter().all(|&s| player(s)))
            }
            MatchSetup::Tag(t) => {
                t.difficulty < crate::menu::DIFFICULTIES.len()
                    && t.slots.iter().all(|s| pick((s.character, s.costume)) && player(s.player))
                    && t.arcade.as_ref().is_none_or(|a| a.level < a.ladder.len() && a.ladder.len() <= crate::tagrun::arcade_ladder_len(roster) && a.ladder.iter().flatten().all(|&p| pick(p)))
            }
        }
    }
}

/// What happened in the room this frame, in order.
#[derive(Clone, Debug)]
pub enum RoomEvent {
    /// A player's key (every computer applies it alike).
    Key { player: usize, key: Key },
    /// The setup's time is up (every computer finishes it alike).
    TimeUp,
    /// The host started a match.
    Start { setup: MatchSetup, seed: u32, session: u32 },
    /// Who is in the room changed.
    Members,
    /// The host is back on the room's screen (a guest still in a match comes back too).
    Back,
    /// The room is over for this computer (why).
    Left(String),
}

pub struct Room {
    pub link: Rc<RefCell<Link>>,
    /// Who is in the room, in seat order (the host first). Empty for a guest until the host has seated it.
    pub members: Vec<Member>,
    pub seed: u32,
    /// The host: the next key's number; a guest: the next one expected, and those come early.
    next: u32,
    early: BTreeMap<u32, (u8, u8)>,
    /// Matches started (each session's packets carry its number).
    pub matches: u32,
    /// VS MODE's tally between the two.
    pub record: crate::grid::VsRecord,
    /// The host: who has loaded which match (token, match); every computer: the match all have loaded.
    ready: Vec<(Token, u32)>,
    all_ready: Option<u32>,
    /// The host: guests' keys waiting their turn (from, player, key), and how many each guest has waiting.
    waiting_keys: std::collections::VecDeque<(Token, u8, u8)>,
    waiting_count: Vec<(Token, usize)>,
    /// The host lets players in (on the room screen only).
    pub open: bool,
    /// A UDP guest asks the host for its hello until it answers.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    asked: Option<Instant>,
    /// What the room screen says while nothing is settled.
    pub status: String,
}

impl Room {
    pub fn new(link: Link, seed: u32) -> Self {
        let host = link.host;
        let me = Member { token: link.token, name: link.name.clone(), locals: link.locals };
        Self {
            members: if host { vec![me] } else { Vec::new() },
            link: Rc::new(RefCell::new(link)),
            seed,
            next: 0,
            early: BTreeMap::new(),
            matches: 0,
            ready: Vec::new(),
            all_ready: None,
            waiting_keys: Default::default(),
            waiting_count: Vec::new(),
            record: Default::default(),
            open: true,
            asked: None,
            status: if host { "WAITING FOR PLAYERS".into() } else { "JOINING THE ROOM".into() },
        }
    }

    pub fn host(&self) -> bool {
        self.link.borrow().host
    }

    pub fn token(&self) -> Token {
        self.link.borrow().token
    }

    /// This computer's seat, once seated.
    pub fn seat(&self) -> Option<usize> {
        let t = self.token();
        self.members.iter().position(|m| m.token == t)
    }

    /// The players in the room.
    pub fn players(&self) -> usize {
        self.members.iter().map(|m| m.locals as usize).sum()
    }

    /// The members this computer has no link to (WebRTC: two networks that do not let their computers meet, or not
    /// yet): a match with them cannot connect.
    pub fn unreachable(&self) -> Vec<String> {
        let link = self.link.borrow();
        self.members.iter().filter(|m| !link.reaches(m.token)).map(|m| m.name.clone()).collect()
    }

    /// Why a match ended, as the room tells it: the session's reason, or who this computer never reached when that is
    /// why.
    pub fn why_over(&self, status: &str) -> String {
        let cut = self.unreachable();
        if status == kumite_net::COULD_NOT_CONNECT && !cut.is_empty() {
            format!("COULD NOT REACH {}", cut.join(", "))
        } else {
            status.to_string()
        }
    }

    /// The players of seat `s` (their numbers in the room: P1 = 0).
    pub fn players_of(&self, s: usize) -> std::ops::Range<usize> {
        let start: usize = self.members.iter().take(s).map(|m| m.locals as usize).sum();
        start..start + self.members.get(s).map_or(0, |m| m.locals as usize)
    }

    /// This computer's players.
    pub fn mine(&self) -> std::ops::Range<usize> {
        self.seat().map_or(0..0, |s| self.players_of(s))
    }

    /// The seat whose computer plays player `p`.
    pub fn owner(&self, p: usize) -> Option<usize> {
        (0..self.members.len()).find(|&s| self.players_of(s).contains(&p))
    }

    /// Player `p`'s nickname (a computer's second player: its nickname and 2).
    pub fn name_of(&self, p: usize) -> String {
        let Some(s) = self.owner(p) else { return format!("{}P", p + 1) };
        let m = &self.members[s];
        if m.locals > 1 {
            format!("{}{}", m.name, p - self.players_of(s).start + 1)
        } else {
            m.name.clone()
        }
    }

    /// A key of this computer's `local` player (0 its first, 1 its second): the host numbers it, a guest sends it to
    /// the host (it comes back numbered).
    pub fn key(&mut self, local: usize, key: Key, out: &mut Vec<RoomEvent>) {
        let mine = self.mine();
        let player = mine.start + local;
        if !mine.contains(&player) {
            return;
        }
        let host_token = self.link.borrow().host_token();
        if self.host() {
            self.number(player, key, out);
        } else if let Some(h) = host_token {
            self.link.borrow_mut().send(Some(h), LobbyMsg::Key { player: player as u8, key: key_code(key) });
        }
    }

    /// The host: key `n` to everyone, and here.
    fn number(&mut self, player: usize, key: Key, out: &mut Vec<RoomEvent>) {
        let n = self.next;
        self.next = n.wrapping_add(1);
        self.link.borrow_mut().send(None, LobbyMsg::Event { n, player: player as u8, key: key_code(key) });
        out.push(RoomEvent::Key { player, key });
    }

    /// The host: the setup's time is up, to everyone (in turn with the keys) and here.
    pub fn time_up(&mut self, out: &mut Vec<RoomEvent>) {
        let n = self.next;
        self.next = n.wrapping_add(1);
        self.link.borrow_mut().send(None, LobbyMsg::Event { n, player: TIME_UP, key: 0 });
        out.push(RoomEvent::TimeUp);
    }

    /// The host starts a match: the setup and a seed to everyone, and here. A setup of this computer's own menus
    /// that the guests' check would refuse is a bug: not sent, the room ends saying so.
    pub fn start(&mut self, setup: MatchSetup, roster: &crate::menu::Roster, out: &mut Vec<RoomEvent>) {
        if !setup.valid(roster) {
            log::error!("room: our own setup fails the check (a bug): {setup:?}");
            out.push(RoomEvent::Left("THIS MATCH'S SETUP IS NOT VALID (A BUG)".into()));
            return;
        }
        // a number past every one a guest may still use (a run cut short here goes on at a guest for a few matches: TAG
        // ARCADE's next pair, Force's next stage, its Ready or its rollback packets maybe still on their way): those
        // fall outside the notes' window and the new match's packets; the notes of earlier matches go
        let session = self.matches.wrapping_add(READY_AHEAD);
        self.ready.clear();
        let seed = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(1, |d| d.subsec_nanos() | 1);
        if let Ok(s) = serde_json::to_string(&setup) {
            self.link.borrow_mut().send(None, LobbyMsg::Start { setup: s, seed, session });
        }
        out.push(RoomEvent::Start { setup, seed, session });
    }

    /// This computer has loaded the current match: the host notes it (and, once every computer has, says so to all);
    /// a guest tells the host.
    pub fn ready(&mut self) {
        let (me, session) = (self.token(), self.matches);
        let host_token = self.link.borrow().host_token();
        if self.host() {
            self.note_ready(me, session);
        } else if let Some(h) = host_token {
            self.link.borrow_mut().send(Some(h), LobbyMsg::Ready { session });
        }
    }

    /// Whether every computer has loaded the current match.
    pub fn all_ready(&self) -> bool {
        self.all_ready == Some(self.matches)
    }

    /// The host: a computer has loaded a match (a guest may be a match ahead: TAG ARCADE's next pair). A computer
    /// that loads after the others gave up (LOAD_FRAMES) still hears AllReady from the host, still on that match, and
    /// so ends in CONNECT_FRAMES, not another wait: keep the notes of the match until the next one starts.
    fn note_ready(&mut self, token: Token, session: u32) {
        if !self.ready.contains(&(token, session)) {
            self.ready.push((token, session));
        }
        let now = self.matches;
        self.ready.retain(|r| r.1.wrapping_sub(now) < READY_AHEAD);
        if self.all_ready != Some(now) && self.members.iter().all(|m| self.ready.contains(&(m.token, now))) {
            self.all_ready = Some(now);
            self.link.borrow_mut().send(None, LobbyMsg::AllReady { session: now });
        }
    }

    /// The host, back on the room's screen: the guests too.
    pub fn back(&mut self) {
        self.link.borrow_mut().send(None, LobbyMsg::Back);
    }

    /// Leave (the others are told).
    pub fn leave(&mut self) {
        self.link.borrow_mut().send(None, LobbyMsg::Bye);
    }

    fn tell_members(&mut self) {
        let msg = LobbyMsg::Room { members: self.members.clone(), seed: self.seed, next: self.next };
        self.link.borrow_mut().send(None, msg);
    }

    /// Per frame: what the peers said and did.
    pub fn poll(&mut self) -> Vec<RoomEvent> {
        let mut out = Vec::new();
        let host = self.host();
        // a UDP guest's hello, twice a second until the host answers
        #[cfg(not(target_arch = "wasm32"))]
        if !host && self.link.borrow().host_token().is_none() && self.asked.is_none_or(|t| t.elapsed().as_millis() >= 500) {
            self.link.borrow_mut().ask_host();
            self.asked = Some(Instant::now());
        }
        let (msgs, gone, failed) = {
            let mut l = self.link.borrow_mut();
            (l.take(), l.take_gone(), l.failed.clone())
        };
        if let Some(why) = failed {
            out.push(RoomEvent::Left(why));
            return out;
        }
        let from_host = |from: Token, l: &Link| !host && Some(from) == l.host_token();
        for (from, m) in msgs {
            // (what only the host says is taken from the host only: on WebRTC every guest reaches every other)
            let by_host = from_host(from, &self.link.borrow());
            match m {
                // the host seats a newcomer when there is room (and the room screen is up)
                LobbyMsg::Hello { name, locals, token, .. } if host => {
                    if self.members.iter().any(|m| m.token == token) || !(1..=2).contains(&locals) {
                        continue;
                    }
                    let name: String = name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
                    if !self.open || self.players() + locals as usize > MAX_PLAYERS {
                        let why = if self.open { "THE ROOM IS FULL" } else { "A MATCH IS ON" };
                        self.link.borrow_mut().refuse_peer(token, why);
                        continue;
                    }
                    self.members.push(Member { token, name, locals });
                    self.tell_members();
                    out.push(RoomEvent::Members);
                    sfx::play(sfx::join());
                }
                LobbyMsg::Room { members, seed, next } if by_host => {
                    if !members_ok(&members) {
                        out.push(RoomEvent::Left("THE HOST'S ROOM IS NOT VALID".into()));
                        continue;
                    }
                    let was = self.seat();
                    self.members = members;
                    self.seed = seed;
                    // seated now: the host's keys from here on (those before were the room's alone)
                    if was.is_none() && self.seat().is_some() {
                        self.next = next;
                        self.early.retain(|&n, _| n.wrapping_sub(next) < EARLY_WINDOW);
                    }
                    self.link.borrow_mut().know(&self.members);
                    if was.is_some() && self.seat().is_none() {
                        out.push(RoomEvent::Left("THE HOST LET YOU GO".into()));
                    }
                    out.push(RoomEvent::Members);
                }
                // (from the host, or this computer's link about the host (token 0: tokens are odd): the room's end;
                // another guest's is ignored)
                LobbyMsg::Refused(why) if by_host || from == 0 && !host => out.push(RoomEvent::Left(why)),
                // the host numbers a guest's player's key (only the guest's own players)
                // (a guest's keys, at most a few a frame: a flood would grow every queue)
                // (a guest's keys, at most MAX_KEYS a poll; more wait their turn, up to MAX_WAITING: a burst after a
                // hiccup comes in a few frames, a flood is dropped)
                LobbyMsg::Key { player, key } if host => {
                    // (each guest's own share of the wait: one flooding cannot crowd out the others)
                    let n = self.waiting_count.iter_mut().find(|c| c.0 == from);
                    match n {
                        Some(c) if c.1 >= MAX_WAITING => {}
                        Some(c) => {
                            c.1 += 1;
                            self.waiting_keys.push_back((from, player, key));
                        }
                        None => {
                            self.waiting_count.push((from, 1));
                            self.waiting_keys.push_back((from, player, key));
                        }
                    }
                }
                LobbyMsg::Event { n, player, key } if by_host => {
                    // (only a window ahead is kept: the host never runs that far ahead)
                    if n.wrapping_sub(self.next) >= EARLY_WINDOW || (player != TIME_UP && player as usize >= MAX_PLAYERS) {
                        continue;
                    }
                    self.early.insert(n, (player, key));
                    while let Some((p, k)) = self.early.remove(&self.next) {
                        self.next = self.next.wrapping_add(1);
                        if p == TIME_UP {
                            out.push(RoomEvent::TimeUp);
                        } else if let Some(key) = key_of(k) {
                            out.push(RoomEvent::Key { player: p as usize, key });
                        }
                    }
                }
                LobbyMsg::Start { setup, seed, session } if by_host => match serde_json::from_str::<MatchSetup>(&setup) {
                    Ok(setup) => out.push(RoomEvent::Start { setup, seed, session }),
                    Err(e) => {
                        log::error!("room: the host's match: {e}");
                        out.push(RoomEvent::Left("THE HOST'S MATCH IS NOT VALID".into()));
                    }
                },
                LobbyMsg::Ready { session } if host && self.members.iter().any(|m| m.token == from) => self.note_ready(from, session),
                LobbyMsg::AllReady { session } if by_host => self.all_ready = Some(session),
                LobbyMsg::Back if by_host => out.push(RoomEvent::Back),
                LobbyMsg::Bye => {
                    if by_host {
                        out.push(RoomEvent::Left("THE HOST LEFT".into()));
                    } else {
                        self.drop_peer(from, &mut out);
                    }
                }
                _ => {}
            }
        }
        for t in gone {
            if !host && self.members.first().is_some_and(|m| m.token == t) {
                out.push(RoomEvent::Left("THE HOST LEFT".into()));
            } else {
                self.drop_peer(t, &mut out);
            }
        }
        // the host numbers the guests' keys waiting, MAX_KEYS a guest this poll, in the order they came
        let mut taken: Vec<Token> = Vec::new();
        let mut later = std::collections::VecDeque::new();
        while let Some((from, player, key)) = self.waiting_keys.pop_front() {
            if taken.iter().filter(|&&t| t == from).count() >= MAX_KEYS {
                later.push_back((from, player, key));
                continue;
            }
            taken.push(from);
            if let Some(c) = self.waiting_count.iter_mut().find(|c| c.0 == from) {
                c.1 = c.1.saturating_sub(1);
            }
            let owner = self.members.iter().position(|m| m.token == from);
            if let (Some(s), Some(k)) = (owner, key_of(key)) {
                if self.players_of(s).contains(&(player as usize)) {
                    self.number(player as usize, k, &mut out);
                }
            }
        }
        self.waiting_keys = later;
        self.waiting_count.retain(|c| c.1 > 0);
        if self.seat().is_some() {
            self.status.clear();
        } else if !host && self.link.borrow().host_token().is_some() {
            self.status = "WAITING FOR A SEAT".into();
        }
        out
    }

    /// A peer is gone: the host lets it go and tells the others.
    fn drop_peer(&mut self, t: Token, out: &mut Vec<RoomEvent>) {
        self.link.borrow_mut().forget(t);
        if self.host() && self.members.iter().any(|m| m.token == t) {
            self.members.retain(|m| m.token != t);
            self.tell_members();
            out.push(RoomEvent::Members);
        }
    }
}

/// The room's screen: who is in, and the modes (the host's 1P moves the cursor and starts; every computer shows it
/// alike).
#[derive(Clone, Debug, Default)]
pub struct RoomScreen {
    pub cursor: usize,
    pub frame: u32,
    /// Why the last mode would not start (too many or too few players), or why the last match ended.
    pub note: Option<String>,
    /// What it shows of the room (the app keeps it up to date): each computer's nickname, its players' numbers and
    /// whether it is this one; the room's line; whether this one hosts; the players.
    pub lines: Vec<(String, String, bool)>,
    pub status: String,
    pub host: bool,
    pub players: usize,
}

impl RoomScreen {
    /// A key (the host's 1P only): the mode chosen, when START picks one that fits.
    pub fn key(&mut self, player: usize, key: Key, players: usize) -> Option<OnlineMode> {
        if player != 0 {
            return None;
        }
        let n = OnlineMode::ALL.len();
        match key {
            Key::Up => self.cursor = (self.cursor + n - 1) % n,
            Key::Down => self.cursor = (self.cursor + 1) % n,
            Key::Start | Key::Button(_) => {
                let m = OnlineMode::ALL[self.cursor];
                if !m.fits(players) {
                    self.note = Some(match m {
                        OnlineMode::Versus | OnlineMode::Ball => "FOR TWO PLAYERS".into(),
                        _ => "FOR TWO PLAYERS AT MOST".into(),
                    });
                } else {
                    self.note = None;
                    return Some(m);
                }
                return None;
            }
            _ => return None,
        }
        self.note = None;
        None
    }

    /// Take in the room as it is now.
    pub fn update(&mut self, room: &Room) {
        let cut = room.unreachable();
        let name = |m: &Member| if cut.contains(&m.name) { format!("{} - NO LINK YET", m.name) } else { m.name.clone() };
        self.lines = (0..room.members.len()).map(|s| (name(&room.members[s]), room.players_of(s).map(|p| format!("{}P", p + 1)).collect::<Vec<_>>().join(" "), Some(s) == room.seat())).collect();
        self.status = room.status.clone();
        self.host = room.host();
        self.players = room.players();
    }

    pub fn draw(&self, painter: &egui::Painter, v: &View, art: &Art) {
        Art::shade(painter, v, crate::art::FULL, [Color32::from_rgb(0x06, 0x10, 0x28), Color32::from_rgb(0x06, 0x10, 0x28), Color32::BLACK, Color32::BLACK]);
        let centred = |font: usize, colour: u32, y: f32, t: &str| art.text(painter, v, font, colour, crate::art::CENTRE - art.text_w(font, t) / 2.0, y, t, Color32::WHITE);
        centred(1, 6, 18.0, "ONLINE ROOM");
        let blink = self.frame & 0x10 != 0;
        // who is in: each computer, its players' numbers
        for (s, (name, tag, mine)) in self.lines.iter().enumerate() {
            let y = 56.0 + 22.0 * s as f32;
            art.text(painter, v, 0, if *mine { 5 } else { 6 }, 40.0, y, name, Color32::WHITE);
            art.text(painter, v, 0, 7, 200.0, y, tag, Color32::WHITE);
            if s == 0 {
                art.text(painter, v, 0, 2, 280.0, y, "HOST", Color32::WHITE);
            }
        }
        if !self.status.is_empty() {
            centred(0, if blink { 5 } else { 6 }, 150.0, &self.status);
        }
        // the modes
        let players = self.players;
        for (k, m) in OnlineMode::ALL.iter().enumerate() {
            let y = 176.0 + 28.0 * k as f32;
            let on = k == self.cursor;
            if on {
                Art::shade(painter, v, [30.0, y - 5.0, 308.0, 26.0], [Color32::from_rgba_unmultiplied(255, 255, 255, 40); 4]);
            }
            let usable = m.fits(players);
            centred(
                1,
                if !usable {
                    7
                } else if on {
                    9
                } else {
                    0
                },
                y,
                &m.name(&art.screens),
            );
        }
        if let Some(n) = &self.note {
            centred(0, 2, 382.0, n);
        }
        let hint = if self.host { "START: PLAY   BACK: LEAVE" } else { "THE HOST CHOOSES   BACK: LEAVE" };
        centred(0, 6, 420.0, hint);
        centred(0, 7, 444.0, &format!("{} PLAYERS (UP TO {MAX_PLAYERS})", players));
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use crate::menu::{FightSetup, Mode, Roster};

    #[test]
    fn a_hosts_setup_out_of_range_is_refused() {
        let gd = crate::test_assets().join("gamedata");
        if !gd.exists() {
            return;
        }
        let r = Roster::load(&gd);
        let ok = FightSetup { mode: Mode::Versus, players: [(0, 0), (1, 0)], stage: 0, difficulty: 1, level: 5, handicap: [100; 2], ball: None, partners: None };
        assert!(MatchSetup::Fight(ok.clone()).valid(&r));
        let bad = |f: &dyn Fn(&mut FightSetup)| {
            let mut s = ok.clone();
            f(&mut s);
            !MatchSetup::Fight(s).valid(&r)
        };
        assert!(bad(&|s| s.players[1].0 = 10_000));
        assert!(bad(&|s| s.players[0].1 = 9));
        assert!(bad(&|s| s.players[0].0 = crate::menu::CPU_SIDE), "a CPU side: Ball's only");
        assert!(bad(&|s| s.stage = 99));
        assert!(bad(&|s| s.handicap[0] = i32::MAX));
        assert!(bad(&|s| s.mode = Mode::Arcade));
        assert!(bad(&|s| s.ball = Some((7, 0))));
    }

    /// Every online mode's setup screen, run out of time (the host's clock), gives a setup the guests take.
    #[test]
    fn every_online_modes_own_setup_passes_the_guests_check() {
        let gd = crate::test_assets().join("gamedata");
        if !gd.exists() {
            return;
        }
        let r = Roster::load(&gd);
        for mode in OnlineMode::ALL {
            for seed in [1, 77, 12345] {
                let outcome = match mode.screen(&r, seed, Default::default()) {
                    crate::menu::Screen::Grid(mut g) => {
                        g.finish(&r);
                        let mut s = seed;
                        (0..2000).map(|_| g.tick(&r, &mut s)).find(|o| !matches!(o, crate::menu::Outcome::None))
                    }
                    crate::menu::Screen::Tag(mut t) => Some(t.finish(&r)),
                    _ => None,
                };
                let setup = match outcome {
                    Some(crate::menu::Outcome::Fight(f, _)) => MatchSetup::Fight(f),
                    Some(crate::menu::Outcome::StartTag(t)) => MatchSetup::Tag(t),
                    o => panic!("{mode:?}: no match from its setup ({:?})", o.is_some()),
                };
                assert!(setup.valid(&r), "{mode:?}: {setup:?}");
            }
        }
    }

    /// Every online mode's setup screen, played by 1..4 players pressing keys at random (a seeded run each), then
    /// the clock: every setup it gives passes the guests' check, and has no number a 32-bit browser cannot read.
    #[test]
    fn random_play_on_every_online_setup_gives_setups_the_guests_take() {
        use crate::menu::{Key, Outcome, Screen};
        let gd = crate::test_assets().join("gamedata");
        if !gd.exists() {
            return;
        }
        let r = Roster::load(&gd);
        let keys = [Key::Up, Key::Down, Key::Left, Key::Right, Key::Start, Key::Button(0), Key::Button(1), Key::Button(2), Key::Button(3)];
        let fits_u32 = |v: &serde_json::Value| {
            fn walk(v: &serde_json::Value) -> bool {
                match v {
                    serde_json::Value::Number(n) => n.as_u64().is_none_or(|n| n <= u64::from(u32::MAX)),
                    serde_json::Value::Array(a) => a.iter().all(walk),
                    serde_json::Value::Object(o) => o.values().all(walk),
                    _ => true,
                }
            }
            walk(v)
        };
        let mut checked = 0;
        for mode in OnlineMode::ALL {
            for players in (1..=MAX_PLAYERS).filter(|&p| mode.fits(p)) {
                for run in 0..12u32 {
                    let mut rng = run.wrapping_mul(2_654_435_761) ^ players as u32;
                    let mut next = || {
                        rng ^= rng << 13;
                        rng ^= rng >> 17;
                        rng ^= rng << 5;
                        rng
                    };
                    let mut screen = mode.screen(&r, next(), Default::default());
                    let mut outcomes = Vec::new();
                    for _ in 0..200 {
                        let (p, k) = (next() as usize % players, keys[next() as usize % keys.len()]);
                        let o = match &mut screen {
                            Screen::Grid(g) if p < 2 => g.key(p, k, &r),
                            Screen::Tag(t) => t.key(p, k, &r),
                            _ => Outcome::None,
                        };
                        outcomes.push(o);
                    }
                    outcomes.push(match &mut screen {
                        Screen::Grid(g) => {
                            g.finish(&r);
                            let mut s = run;
                            (0..2000).map(|_| g.tick(&r, &mut s)).find(|o| !matches!(o, Outcome::None)).unwrap_or(Outcome::None)
                        }
                        Screen::Tag(t) => t.finish(&r),
                        _ => Outcome::None,
                    });
                    for o in outcomes {
                        let setup = match o {
                            Outcome::Fight(f, _) => MatchSetup::Fight(f),
                            Outcome::StartTag(t) => MatchSetup::Tag(t),
                            _ => continue,
                        };
                        assert!(setup.valid(&r), "{mode:?}, {players} players, run {run}: {setup:?}");
                        assert!(fits_u32(&serde_json::to_value(&setup).unwrap()), "{mode:?}: a number past 32 bits: {setup:?}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked >= OnlineMode::ALL.len() * 12, "setups reached: {checked}");
    }

    /// A match's setup keeps its wire form (a change needs kumite_net::PROTOCOL bumped).
    #[test]
    fn a_match_setup_keeps_its_wire_form() {
        use crate::tagrun::{SlotPick, TagMode, TagSetup};
        let fight = FightSetup { mode: Mode::Ball, players: [(1, 0), (crate::menu::CPU_SIDE, 0)], stage: 19, difficulty: 1, level: 5, handicap: [100; 2], ball: Some((2, 1)), partners: None };
        assert_eq!(
            serde_json::to_string(&MatchSetup::Fight(fight)).unwrap(),
            r#"{"Fight":{"mode":"Ball","players":[[1,0],[65535,0]],"stage":19,"difficulty":1,"level":5,"handicap":[100,100],"ball":[2,1],"partners":null}}"#
        );
        let slot = |character, player| SlotPick { character, costume: 0, player };
        let tag = TagSetup { slots: [slot(0, Some(0)), slot(1, None), slot(2, Some(1)), slot(3, None)], difficulty: 2, mode: TagMode::Arcade, arcade: None };
        assert_eq!(
            serde_json::to_string(&MatchSetup::Tag(tag)).unwrap(),
            r#"{"Tag":{"slots":[{"character":0,"costume":0,"player":0},{"character":1,"costume":0,"player":null},{"character":2,"costume":0,"player":1},{"character":3,"costume":0,"player":null}],"difficulty":2,"mode":"Arcade","arcade":null}}"#
        );
    }

    #[test]
    fn a_hosts_room_list_out_of_range_is_refused() {
        let m = |token, locals| Member { token, name: "P".into(), locals };
        assert!(members_ok(&[m(1, 2), m(3, 2)]));
        assert!(!members_ok(&[m(1, 2), m(3, 2), m(5, 1)]), "five players");
        assert!(!members_ok(&[m(1, 1), m(1, 1)]), "a token twice");
        assert!(!members_ok(&[m(1, 1), m(0, 1)]), "a token nobody draws");
        assert!(!members_ok(&[m(1, 0)]));
        assert!(!members_ok(&[Member { token: 1, name: "X".repeat(100), locals: 1 }]));
    }
}

/// Rooms over UDP on this computer (host and guests in one process, real sockets).
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod udp_tests {
    use super::*;
    use crate::netplay::NetAddr;
    use ggrs::NonBlockingSocket;

    fn free_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// Poll every room until `done` (or 5 s), collecting each one's events.
    fn run(rooms: &mut [Room], events: &mut [Vec<RoomEvent>], done: impl Fn(&[Room], &[Vec<RoomEvent>]) -> bool) {
        let start = std::time::Instant::now();
        while !done(rooms, events) {
            assert!(start.elapsed().as_secs() < 5, "the room never settled");
            for (r, e) in rooms.iter_mut().zip(events.iter_mut()) {
                e.extend(r.poll());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn keys(e: &[RoomEvent]) -> Vec<(usize, Key)> {
        e.iter().filter_map(|e| if let RoomEvent::Key { player, key } = e { Some((*player, *key)) } else { None }).collect()
    }

    #[test]
    fn a_udp_room_seats_its_guests_orders_their_keys_and_relays_their_packets() {
        let port = free_port();
        let mut rooms = vec![Room::new(crate::direct::host(port, "HOST", 1).unwrap(), 7)];
        for name in ["ONE", "TWO"] {
            rooms.push(Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), name, 1).unwrap(), 0));
        }
        let mut events = vec![Vec::new(); 3];
        run(&mut rooms, &mut events, |r, _| r[0].members.len() == 3 && r.iter().all(|r| r.seat().is_some() && r.members.len() == 3));
        assert_eq!(rooms[2].seed, 7);
        // (guests reach each other through the host: nobody shows as out of reach)
        assert!(rooms.iter().all(|r| r.unreachable().is_empty()));
        assert_eq!(rooms[1].why_over(kumite_net::COULD_NOT_CONNECT), kumite_net::COULD_NOT_CONNECT);
        // a burst of keys from the second guest: every computer gets them all, in the same order
        let mut out = Vec::new();
        for k in 0..200 {
            rooms[2].key(0, if k % 2 == 0 { Key::Left } else { Key::Right }, &mut out);
        }
        run(&mut rooms, &mut events, |_, e| e.iter().all(|e| keys(e).len() == 200));
        assert!(keys(&events[0]).iter().all(|&(p, _)| p == 2));
        assert_eq!(keys(&events[0]), keys(&events[1]));
        assert_eq!(keys(&events[0]), keys(&events[2]));
        // a rollback packet from guest ONE (seat 1) to guest TWO (seat 2): through the host, from ONE
        let tokens: Vec<Token> = rooms[0].members.iter().map(|m| m.token).collect();
        for r in &rooms {
            r.link.borrow_mut().seats = tokens.clone();
        }
        let keepalive = [9, 0, 0, 0, 0x34, 0x12, 7, 0, 0, 0];
        let msg = kumite_net::ggrs_message(&keepalive, 9).unwrap();
        let mut one = crate::netlink::LinkSocket::open(rooms[1].link.clone(), 9);
        let mut two = crate::netlink::LinkSocket::open(rooms[2].link.clone(), 9);
        one.send_to(&msg, &NetAddr::Seat(2));
        let start = std::time::Instant::now();
        let got = loop {
            rooms[0].link.borrow_mut().pump();
            let got = two.receive_all_messages();
            if !got.is_empty() || start.elapsed().as_secs() >= 5 {
                break got;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(got.len(), 1, "relayed once");
        assert!(matches!(got[0].0, NetAddr::Seat(1)), "from guest ONE's seat");
        // another match's packets are not this one's, and it leaves them for their own (a finished match's session
        // is still polled a while beside the next one's)
        one.send_to(&msg, &NetAddr::Seat(2));
        std::thread::sleep(std::time::Duration::from_millis(50));
        rooms[0].link.borrow_mut().pump();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(crate::netlink::LinkSocket::open(rooms[2].link.clone(), 10).receive_all_messages().is_empty());
        assert_eq!(two.receive_all_messages().len(), 1, "still there for match 9");
        // a closed match's, one too far ahead, one too big: dropped (sessions 10 and 11 open below, 9 closed then)
        let packet = |session: u32, len: usize| {
            let mut b = session.to_le_bytes().to_vec();
            b.resize(len, 0);
            b
        };
        let link = rooms[2].link.clone();
        crate::netlink::LinkSocket::open(link.clone(), 11);
        for (b, kept) in [(packet(9, 10), false), (packet(11 + 33, 10), false), (packet(12, 9000), false), (packet(12, 10), true)] {
            link.borrow_mut().test_got(rooms[1].token(), &b);
            crate::netlink::LinkSocket(link.clone(), 10).receive_all_messages();
            assert_eq!(link.borrow().test_queued(), kept as usize, "{} bytes of match {}", b.len(), u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            link.borrow_mut().test_clear();
        }
        // from a peer met but in no match (a stranger, one refused): not kept, it would push the seats' own out
        link.borrow_mut().test_got(0x55, &packet(12, 10));
        assert_eq!(link.borrow().test_queued(), 0, "a stranger's rollback packet");
        // a finished match's last Input, lost, is sent again (the session opened with its seats)
        let mut input = 11u32.to_le_bytes().to_vec();
        input.extend([0x34, 0x12, 2, 0, 0, 0]);
        input.extend(3u64.to_le_bytes());
        for _ in 0..3 {
            input.push(0);
            input.extend((-1i32).to_le_bytes());
        }
        input.push(0);
        input.extend(0i32.to_le_bytes());
        input.extend((-1i32).to_le_bytes());
        input.extend(5u64.to_le_bytes());
        input.extend([8, 2, 0, 7, 0]);
        let input = kumite_net::ggrs_message(&input, 11).expect("an Input");
        let mut one = crate::netlink::LinkSocket::open(rooms[1].link.clone(), 11);
        one.send_to(&input, &NetAddr::Seat(2));
        let mut two = crate::netlink::LinkSocket::open(rooms[2].link.clone(), 11);
        let mut got = 0;
        for _ in 0..40 {
            rooms[0].link.borrow_mut().pump();
            got += two.receive_all_messages().len();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got, 1, "sent once");
        rooms[1].link.borrow_mut().resend_inputs(11);
        let mut again = 0;
        for _ in 0..40 {
            rooms[0].link.borrow_mut().pump();
            again += two.receive_all_messages().len();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(again, 1, "sent again");
    }

    /// The host pressed keys alone in the room: a guest seated later gets the keys from then on.
    #[test]
    fn a_guest_seated_late_gets_the_keys_after() {
        let port = free_port();
        let mut rooms = vec![Room::new(crate::direct::host(port, "HOST", 1).unwrap(), 7)];
        let mut out = Vec::new();
        for _ in 0..3 {
            rooms[0].key(0, Key::Down, &mut out);
        }
        rooms.push(Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), "LATE", 1).unwrap(), 0));
        let mut events = vec![Vec::new(); 2];
        run(&mut rooms, &mut events, |r, _| r[1].seat().is_some());
        rooms[0].key(0, Key::Up, &mut out);
        run(&mut rooms, &mut events, |_, e| keys(&e[1]).len() == 1);
        assert_eq!(keys(&events[1]), [(0, Key::Up)]);
    }

    /// A full room tells a newcomer so and forgets it (no room messages, no slot); hellos that are not met keep no
    /// slot either.
    #[test]
    fn a_full_room_refuses_a_newcomer_and_forgets_it() {
        let port = free_port();
        let mut rooms = vec![Room::new(crate::direct::host(port, "HOST", 2).unwrap(), 7), Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), "TWO", 2).unwrap(), 0)];
        let mut events = vec![Vec::new(); 3];
        run(&mut rooms, &mut events, |r, _| r[1].seat().is_some());
        rooms.push(Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), "LATE", 1).unwrap(), 0));
        run(&mut rooms, &mut events, |_, e| e[2].iter().any(|e| matches!(e, RoomEvent::Left(w) if w == "THE ROOM IS FULL")));
        // (the refusal acknowledged: forgotten; LATE has given up asking by then)
        rooms.truncate(2);
        run(&mut rooms, &mut events, |r, _| r[0].link.borrow().test_peers() == (1, 1));
        assert_eq!(rooms[0].members.len(), 2);
        // hellos that read but are not met (another version; another build, an even token) take no slot
        let stranger = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        for (version, token) in [(0, 3), (kumite_net::PROTOCOL, 2)] {
            let hello = format!("\0{{\"Hello\":{{\"version\":{version},\"name\":\"X\",\"locals\":1,\"token\":{token},\"host\":false,\"assets\":\"\",\"build\":\"\"}}}}");
            stranger.send_to(hello.as_bytes(), ("127.0.0.1", port)).unwrap();
        }
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < 200 {
            for (r, e) in rooms.iter_mut().zip(events.iter_mut()) {
                e.extend(r.poll());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(rooms[0].link.borrow().test_peers(), (1, 1));
    }

    /// Anyone's datagram saying it hosts another version does not end a host's room.
    #[test]
    fn a_stray_hello_does_not_end_the_hosts_room() {
        let port = free_port();
        let mut host = Room::new(crate::direct::host(port, "HOST", 1).unwrap(), 7);
        let stranger = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        for _ in 0..3 {
            stranger.send_to(b"\0{\"Hello\":{\"version\":0,\"host\":true}}", ("127.0.0.1", port)).unwrap();
        }
        let mut events = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < 300 {
            events.extend(host.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!events.iter().any(|e| matches!(e, RoomEvent::Left(_))));
    }

    /// A guest's word that the room is over is not taken by another guest (only the host's is).
    #[test]
    fn a_guest_cannot_end_another_guests_room() {
        let port = free_port();
        let mut rooms = vec![Room::new(crate::direct::host(port, "HOST", 1).unwrap(), 7)];
        for name in ["ONE", "TWO"] {
            rooms.push(Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), name, 1).unwrap(), 0));
        }
        let mut events = vec![Vec::new(); 3];
        run(&mut rooms, &mut events, |r, _| r.iter().all(|r| r.seat().is_some() && r.members.len() == 3));
        let two = rooms[2].token();
        rooms[1].link.borrow_mut().send(Some(two), LobbyMsg::Refused("GO".into()));
        let start = std::time::Instant::now();
        run(&mut rooms, &mut events, |_, _| start.elapsed().as_millis() >= 300);
        rooms[0].link.borrow_mut().send(Some(two), LobbyMsg::Refused("GONE".into()));
        run(&mut rooms, &mut events, |_, e| e[2].iter().any(|e| matches!(e, RoomEvent::Left(_))));
        let left: Vec<_> = events[2].iter().filter_map(|e| if let RoomEvent::Left(why) = e { Some(why.as_str()) } else { None }).collect();
        assert_eq!(left, ["GONE"]);
    }

    /// A guest seated after the host's first match plays the next one under the host's match number.
    #[test]
    fn a_guest_seated_late_counts_matches_from_the_host() {
        let gd = crate::test_assets().join("gamedata");
        if !gd.exists() {
            return;
        }
        let r = crate::menu::Roster::load(&gd);
        let setup = || {
            MatchSetup::Fight(crate::menu::FightSetup { mode: crate::menu::Mode::Versus, players: [(0, 0), (1, 0)], stage: 0, difficulty: 1, level: 5, handicap: [100; 2], ball: None, partners: None })
        };
        let port = free_port();
        let mut rooms = vec![Room::new(crate::direct::host(port, "HOST", 1).unwrap(), 7)];
        let mut out = Vec::new();
        rooms[0].start(setup(), &r, &mut out);
        rooms[0].matches = 1;
        rooms.push(Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), "LATE", 1).unwrap(), 0));
        let mut events = vec![Vec::new(); 2];
        run(&mut rooms, &mut events, |r, _| r[1].seat().is_some());
        rooms[0].start(setup(), &r, &mut out);
        run(&mut rooms, &mut events, |_, e| e[1].iter().any(|e| matches!(e, RoomEvent::Start { .. })));
        assert!(events[1].iter().any(|e| matches!(e, RoomEvent::Start { session, .. } if *session == 1 + READY_AHEAD)));
    }

    /// Every computer says it has loaded the match; all learn when all have (a guest a match ahead counts too).
    #[test]
    fn the_match_starts_its_clocks_once_every_computer_has_loaded_it() {
        let port = free_port();
        let mut rooms = vec![Room::new(crate::direct::host(port, "HOST", 1).unwrap(), 7)];
        for name in ["ONE", "TWO"] {
            rooms.push(Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), name, 1).unwrap(), 0));
        }
        let mut events = vec![Vec::new(); 3];
        run(&mut rooms, &mut events, |r, _| r.iter().all(|r| r.seat().is_some() && r.members.len() == 3));
        for r in &mut rooms {
            r.matches = 4;
        }
        // the second guest is a match ahead (its next pair loaded first): it says so before the others
        rooms[2].matches = 5;
        rooms[2].ready();
        rooms[0].ready();
        rooms[1].ready();
        rooms[2].matches = 4;
        rooms[2].ready();
        run(&mut rooms, &mut events, |r, _| r.iter().all(Room::all_ready));
        // the next match: not until all have loaded it (the second guest already has)
        for r in &mut rooms {
            r.matches = 5;
        }
        assert!(!rooms[0].all_ready());
        rooms[0].ready();
        rooms[1].ready();
        run(&mut rooms, &mut events, |r, _| r.iter().all(Room::all_ready));
    }

    /// A guest said Ready for the next match of a run that was then cut short: the host's next match takes another
    /// number, so that note cannot count for it (a guest still downloading was cut off).
    #[test]
    fn a_stale_ready_does_not_count_for_the_next_match() {
        let gd = crate::test_assets().join("gamedata");
        if !gd.exists() {
            return;
        }
        let r = crate::menu::Roster::load(&gd);
        let port = free_port();
        let mut rooms = vec![Room::new(crate::direct::host(port, "HOST", 1).unwrap(), 7)];
        rooms.push(Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), "AHEAD", 1).unwrap(), 0));
        let mut events = vec![Vec::new(); 2];
        run(&mut rooms, &mut events, |r, _| r[1].seat().is_some());
        (rooms[0].matches, rooms[1].matches) = (4, 5);
        rooms[1].ready();
        let start = std::time::Instant::now();
        while rooms[0].ready.is_empty() && start.elapsed().as_secs() < 5 {
            rooms[0].poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(rooms[0].ready.len(), 1, "the host noted it");
        let setup = MatchSetup::Fight(crate::menu::FightSetup {
            mode: crate::menu::Mode::Versus,
            players: [(0, 0), (1, 0)],
            stage: 0,
            difficulty: 1,
            level: 5,
            handicap: [100; 2],
            ball: None,
            partners: None,
        });
        let mut out = Vec::new();
        rooms[0].start(setup, &r, &mut out);
        assert!(matches!(out[..], [RoomEvent::Start { session, .. }] if session == 4 + READY_AHEAD), "past every number a guest may use");
        assert!(rooms[0].ready.is_empty());
    }

    /// The host back on the room's screen: a guest hears it; a guest's key flood is cut to MAX_KEYS a poll.
    #[test]
    fn the_host_going_back_reaches_the_guests_and_floods_are_cut() {
        let port = free_port();
        let mut rooms = vec![Room::new(crate::direct::host(port, "HOST", 1).unwrap(), 7)];
        rooms.push(Room::new(crate::direct::join(&format!("127.0.0.1:{port}"), "GUEST", 1).unwrap(), 0));
        let mut events = vec![Vec::new(); 2];
        run(&mut rooms, &mut events, |r, _| r[1].seat().is_some());
        rooms[0].back();
        run(&mut rooms, &mut events, |_, e| e[1].iter().any(|e| matches!(e, RoomEvent::Back)));
        // 100 keys at once from the guest: the host takes MAX_KEYS a poll, the rest in the polls after
        let mut out = Vec::new();
        for _ in 0..100 {
            rooms[1].key(0, Key::Left, &mut out);
        }
        let mut taken = Vec::new();
        for _ in 0..60 {
            taken.push(keys(&rooms[0].poll()).len());
            rooms[1].poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(taken.iter().all(|&n| n <= MAX_KEYS) && taken.iter().sum::<usize>() == 100, "{taken:?}");
    }

    #[test]
    fn a_guest_with_other_game_files_is_told_so() {
        let port = free_port();
        let mut host = crate::direct::host(port, "HOST", 1).unwrap();
        host.assets = "aaaa".into();
        let mut guest = crate::direct::join(&format!("127.0.0.1:{port}"), "GUEST", 1).unwrap();
        guest.assets = "bbbb".into();
        let mut rooms = vec![Room::new(host, 1), Room::new(guest, 0)];
        let mut events = vec![Vec::new(); 2];
        run(&mut rooms, &mut events, |_, e| e[1].iter().any(|e| matches!(e, RoomEvent::Left(_))));
        assert!(events[1].iter().any(|e| matches!(e, RoomEvent::Left(why) if why.contains("GAME FILES"))));
        assert_eq!(rooms[0].members.len(), 1, "the host did not seat it");
    }
}
