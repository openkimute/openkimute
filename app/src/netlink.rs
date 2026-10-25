//! The links of a room (up to four computers): from the meeting until one leaves, through any number of matches.
//! Every computer is a peer known by a token it picked at random and said in its hello. They carry:
//! - the room's messages, reliable and in order: on WebRTC its reliable channel (each to each); on UDP, between each
//!   guest and the host only, numbered and acknowledged here, sent again until acknowledged (the room's protocol
//!   only ever needs guest → host and host → all: `room.rs`);
//! - each match's rollback session (a new `ggrs` session per match) through [`LinkSocket`]: WebRTC's unreliable
//!   channel (each to each), or UDP packets; a guest's packets for another guest go through the host, which passes
//!   them on. Each packet carries its match's number: a session never takes the last match's packets still on
//!   their way.

use crate::netplay::NetAddr;
use std::cell::RefCell;
use std::rc::Rc;
use web_time::{Duration, Instant};

pub use kumite_net::{LobbyMsg, Member, Token};
#[cfg(not(target_arch = "wasm32"))]
use kumite_net::{TAG_GGRS, TAG_HELLO, TAG_LOBBY, TAG_RELAY};

/// The menu keys as one byte.
pub fn key_code(k: crate::menu::Key) -> u8 {
    use crate::menu::Key::*;
    match k {
        Up => 1,
        Down => 2,
        Left => 3,
        Right => 4,
        Start => 5,
        Back => 6,
        Button(b) => 16 + b.min(3) as u8,
    }
}

pub fn key_of(c: u8) -> Option<crate::menu::Key> {
    use crate::menu::Key::*;
    Some(match c {
        1 => Up,
        2 => Down,
        3 => Left,
        4 => Right,
        5 => Start,
        6 => Back,
        16..=19 => Button((c - 16) as usize),
        _ => return None,
    })
}

/// This game build's fingerprint (build.rs: the sources and dependencies of everything a match runs): two computers
/// of different builds would desync, so they refuse each other at the hello.
pub const BUILD: &str = env!("OPENKUMITE_BUILD");

/// One UDP peer's reliable channel ([`kumite_net::reliable`]), when its lobby packet was last sent, when it was last heard.
#[cfg(not(target_arch = "wasm32"))]
struct Reliable {
    addr: std::net::SocketAddr,
    channel: kumite_net::reliable::Reliable,
    sent: Option<Instant>,
    heard: Instant,
}

/// The most UDP peers a host keeps a channel for (the room's guests, and a few more saying hello meanwhile).
#[cfg(not(target_arch = "wasm32"))]
const MAX_UDP_PEERS: usize = 8;

enum Transport {
    Rtc {
        socket: matchbox_socket::WebRtcSocket,
    },
    #[cfg(not(target_arch = "wasm32"))]
    Udp {
        socket: std::net::UdpSocket,
        /// The host (a guest's only peer), when this one is a guest.
        host: Option<std::net::SocketAddr>,
    },
}

/// How to reach a peer.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Route {
    Rtc(matchbox_socket::PeerId),
    #[cfg(not(target_arch = "wasm32"))]
    Udp(std::net::SocketAddr),
    /// A guest reaching another guest (UDP): through the host.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    ViaHost,
}

pub struct Link {
    transport: Transport,
    /// This computer's token, nickname, players here, and whether it hosts.
    pub token: Token,
    pub name: String,
    pub locals: u8,
    pub host: bool,
    /// The converted assets' fingerprint (said in the hello; a peer with others is refused).
    pub assets: String,
    /// The peers met, and how each is reached (a token stays its first route's: [`kumite_net::peers`]).
    peers: kumite_net::peers::Peers<Route>,
    /// UDP: each directly linked peer's reliable channel (a guest: the host's; the host: each guest's).
    #[cfg(not(target_arch = "wasm32"))]
    reliable: Vec<Reliable>,
    /// UDP: where a datagram is received (one big message goes alone: room for any).
    #[cfg(not(target_arch = "wasm32"))]
    udp_buf: Vec<u8>,
    /// WebRTC peers connected that have not said hello yet (ours is sent).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    greeted: Vec<matchbox_socket::PeerId>,
    inbox: Vec<(Token, LobbyMsg)>,
    /// UDP host: when it last answered another version's hello.
    #[cfg(not(target_arch = "wasm32"))]
    answered: Option<Instant>,
    /// Seated in the room (the host met listed this one): from then on a second claim to host is ignored; before, it
    /// ends the join (who hosts cannot be told).
    seated: bool,
    /// The matches' rollback inputs kept in step ([`kumite_net::InputGuard`]): each one's number and guard.
    input_guards: Vec<(u32, kumite_net::InputGuard)>,
    /// The last two matches' sessions: number, seats as they started, the last Input packet sent to each seat.
    sessions: Vec<(u32, Vec<Token>, Vec<(usize, Vec<u8>)>)>,
    /// Rollback packets received: (from, bytes).
    ggrs_in: std::collections::VecDeque<(Token, Vec<u8>)>,
    /// Peers gone since the last [`Link::take_gone`].
    gone: Vec<Token>,
    /// Peers told they cannot join ([`Link::refuse_peer`]), and since when: forgotten once told (UDP: the message
    /// acknowledged, or [`REFUSED_FOR`] past), told nothing else meanwhile.
    refused: Vec<(Token, Instant)>,
    /// The seats of the match (index = seat): their tokens, for the rollback session's addresses.
    pub seats: Vec<Token>,
    /// The room's version (a peer of another is refused).
    version: u32,
    /// The link broke (WebRTC: the signalling server could not be reached, or its loop ended): why.
    pub failed: Option<String>,
}

/// How far past the newest open match another's packets are kept (its peers may start it first: TAG ARCADE's next
/// pair, Force's next stage, a new match 16 on).
const SESSIONS_AHEAD: u32 = 32;

/// UDP datagrams read a frame at most.
#[cfg(not(target_arch = "wasm32"))]
const MAX_READS: usize = 1024;

/// A rollback packet's largest kept (bytes).
const MAX_GGRS_PACKET: usize = 8 * 1024;

/// The rollback packets kept for a session at most (a frame's few a peer; outside a match nobody takes them).
const MAX_GGRS_IN: usize = 512;

/// A UDP peer refused stays this long at most (its refusal unacknowledged) before it is forgotten.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const REFUSED_FOR: Duration = Duration::from_secs(2);

/// A UDP peer with this many room messages unacknowledged counts as gone (an honest one acknowledges each within a
/// second; one that never does would hold them all).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_UNACKED: usize = 4096;

/// How long a UDP peer may stay silent before it counts as gone.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const SILENCE: Duration = Duration::from_secs(8);
/// How often a UDP link sends its lobby packet (again) when it has something unacknowledged, or to keep alive.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const RESEND: Duration = Duration::from_millis(150);
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const KEEPALIVE: Duration = Duration::from_millis(1000);

/// A token nobody else will have.
pub fn new_token() -> Token {
    let t = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
    // (two computers started in the same nanosecond still differ in the address of this stack)
    let here = &t as *const u64 as u64;
    (t ^ here.rotate_left(17)).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1
}

impl Link {
    fn with(transport: Transport, version: u32, name: &str, locals: u8, host: bool) -> Self {
        Self {
            transport,
            token: new_token(),
            assets: String::new(),
            name: name.chars().filter(|c| !c.is_control()).take(crate::room::MAX_NAME).collect(),
            locals: locals.clamp(1, 2),
            host,
            #[cfg(not(target_arch = "wasm32"))]
            udp_buf: vec![0; 65536],
            peers: Default::default(),
            input_guards: Vec::new(),
            sessions: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            answered: None,
            seated: false,
            #[cfg(not(target_arch = "wasm32"))]
            reliable: Vec::new(),
            greeted: Vec::new(),
            inbox: Vec::new(),
            ggrs_in: Default::default(),
            gone: Vec::new(),
            refused: Vec::new(),
            seats: Vec::new(),
            version,
            failed: None,
        }
    }

    /// A WebRTC room's link (every peer greeted as it connects).
    pub fn rtc(socket: matchbox_socket::WebRtcSocket, version: u32, name: &str, locals: u8, host: bool) -> Self {
        Self::with(Transport::Rtc { socket }, version, name, locals, host)
    }

    /// A UDP link: the host's (guests come to it), or a guest's (to the host at `host`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn udp(socket: std::net::UdpSocket, host: Option<std::net::SocketAddr>, version: u32, name: &str, locals: u8) -> Self {
        let is_host = host.is_none();
        Self::with(Transport::Udp { socket, host }, version, name, locals, is_host)
    }

    fn hello(&self) -> LobbyMsg {
        LobbyMsg::Hello { version: self.version, name: self.name.clone(), locals: self.locals, token: self.token, host: self.host, assets: self.assets.clone(), build: BUILD.to_string() }
    }

    /// Whether a match's packets reach `token` (this one; WebRTC: a link of its own to it; UDP: guests through the
    /// host).
    pub fn reaches(&self, token: Token) -> bool {
        token == self.token || self.peers.route_of(token).is_some()
    }

    /// The host met (a guest), its token.
    pub fn host_token(&self) -> Option<Token> {
        self.peers.host()
    }

    /// Send a room message to a peer (`None`: to every peer met).
    pub fn send(&mut self, to: Option<Token>, msg: LobbyMsg) {
        let refused = |t: Token| self.refused.iter().any(|r| r.0 == t);
        let targets: Vec<Route> = self.peers.iter().filter(|p| to.map_or(!refused(p.token), |t| t == p.token)).map(|p| p.route).collect();
        for route in targets {
            self.send_route(route, &msg);
        }
        self.pump();
    }

    fn send_route(&mut self, route: Route, msg: &LobbyMsg) {
        match (&mut self.transport, route) {
            (Transport::Rtc { socket }, Route::Rtc(peer)) => {
                if let Ok(b) = serde_json::to_vec(msg) {
                    let _ = socket.channel_mut(1).try_send(b.into_boxed_slice(), peer);
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            (Transport::Udp { .. }, Route::Udp(addr)) => {
                if let Some(r) = self.reliable.iter_mut().find(|r| r.addr == addr) {
                    r.channel.push(msg.clone());
                    r.sent = None;
                }
            }
            // (the room's messages never go guest to guest)
            _ => {}
        }
    }

    /// A peer said hello: known from now on, unless it cannot play with this one (another version, game build or
    /// game files: said only of the host, the room this one cannot join; another peer is just not met) or would take
    /// another's place ([`kumite_net::peers`]).
    fn met(&mut self, route: Route, hello: LobbyMsg) {
        let LobbyMsg::Hello { version, ref name, locals, token, host, ref assets, ref build } = hello else { return };
        if token == self.token {
            return;
        }
        let differs = if version != self.version {
            Some("THE OTHER PLAYER HAS ANOTHER VERSION")
        } else if build.as_str() != BUILD {
            Some("ANOTHER GAME BUILD: UPDATE OR RELOAD THE GAME")
        } else if *assets != self.assets {
            Some("THE OTHER PLAYER'S GAME FILES DIFFER")
        } else {
            None
        };
        if let Some(why) = differs {
            log::warn!("room: {name}: {why}");
            // (the room's end only for the host this one is joining: one not met yet, or the one met, again)
            if host && !self.host && self.peers.host().is_none_or(|h| self.peers.route_of(h) == Some(route)) {
                self.refuse(why);
            }
            return;
        }
        // (the host is this one: nobody else may say so)
        if host && self.host {
            log::warn!("room: {name} says it hosts too: not met");
            return;
        }
        // shortcut: a WebRTC room's host is whoever says so first to a newcomer (the signalling server names no
        // creator): a peer there before the host's hello can run the newcomer's room; its setups are still checked
        // (`MatchSetup::valid`), so it can spoil a room, not crash a game
        match self.peers.meet(route, token, name.clone(), locals, host) {
            Ok(new) => {
                if new {
                    log::info!("room: met {name} ({locals} playing){}", if host { ", the host" } else { "" });
                }
                self.inbox.push((token, hello));
            }
            Err(why) => {
                log::warn!("room: a hello from {name} not taken: {why:?}");
                // two say they host this room: before seated, which is the host cannot be told
                if host && !self.host && !self.seated && matches!(why, kumite_net::peers::Rejected::TokenTaken | kumite_net::peers::Rejected::SecondHost) {
                    self.refuse("TWO PLAYERS SAY THEY HOST THE ROOM: JOIN AGAIN");
                }
            }
        }
    }

    /// The room cannot be joined (its host is of another version, build or files): why, to the room.
    fn refuse(&mut self, why: &str) {
        self.inbox.push((0, LobbyMsg::Refused(why.into())));
    }

    /// A UDP guest learns of the other guests from the room (reached through the host).
    pub fn know(&mut self, members: &[Member]) {
        let udp_guest = {
            #[cfg(not(target_arch = "wasm32"))]
            {
                matches!(self.transport, Transport::Udp { host: Some(_), .. })
            }
            #[cfg(target_arch = "wasm32")]
            {
                false
            }
        };
        if !udp_guest {
            return;
        }
        let members: Vec<(Token, String, u8)> = members.iter().map(|m| (m.token, m.name.clone(), m.locals)).collect();
        self.peers.via(Route::ViaHost, &members, self.token);
    }

    /// Drop a peer (it left, or the host let it go).
    /// Tell a peer it cannot join (the room full, a match on), and forget it once told: it hears nothing else of the
    /// room, and its rollback packets are not taken.
    pub fn refuse_peer(&mut self, token: Token, why: &str) {
        self.send(Some(token), LobbyMsg::Refused(why.into()));
        if !self.refused.iter().any(|r| r.0 == token) {
            self.refused.push((token, Instant::now()));
        }
        self.forget_refused();
    }

    /// The refused peers told (WebRTC: at once; UDP: acknowledged, or long enough ago), forgotten.
    fn forget_refused(&mut self) {
        let told = |l: &Self, refused: (Token, Instant)| match l.peers.route_of(refused.0) {
            #[cfg(not(target_arch = "wasm32"))]
            Some(Route::Udp(addr)) => refused.1.elapsed() >= REFUSED_FOR || l.reliable.iter().find(|r| r.addr == addr).is_none_or(|r| !r.channel.pending()),
            _ => true,
        };
        let done: Vec<Token> = self.refused.iter().copied().filter(|&r| told(self, r)).map(|r| r.0).collect();
        for &t in &done {
            self.forget(t);
        }
        self.refused.retain(|r| !done.contains(&r.0));
    }

    pub fn forget(&mut self, token: Token) {
        let route = self.peers.remove_token(token);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(Route::Udp(addr)) = route {
            self.reliable.retain(|r| r.addr != addr);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = route;
    }

    /// Everything received (the room's messages kept for [`Link::take`], the rollback packets for the session);
    /// the UDP lobby packets sent when due.
    pub fn pump(&mut self) {
        let hello = self.hello();
        // (rollback packets only from the matches' seats: a stranger met, or refused, has none to send)
        let seated: Vec<Token> = self.seats.iter().chain(self.sessions.iter().flat_map(|s| s.1.iter())).copied().collect();
        // the room's messages in the order they came (a hello first makes its sender known to what follows)
        let mut received: Vec<(Route, LobbyMsg)> = Vec::new();
        let mut other_version = false;
        match &mut self.transport {
            // (the socket's loop over: nothing more to read or send)
            Transport::Rtc { .. } if self.failed.is_some() => {}
            Transport::Rtc { socket } => {
                let Ok(changes) = socket.try_update_peers() else {
                    self.failed = Some("CAN'T REACH THE ROOM SERVER".into());
                    return;
                };
                for (id, state) in changes {
                    match state {
                        matchbox_socket::PeerState::Connected => {
                            if !self.greeted.contains(&id) {
                                if let Ok(b) = serde_json::to_vec(&hello) {
                                    let _ = socket.channel_mut(1).try_send(b.into_boxed_slice(), id);
                                }
                                self.greeted.push(id);
                            }
                        }
                        matchbox_socket::PeerState::Disconnected => {
                            self.greeted.retain(|p| *p != id);
                            if let Some(token) = self.peers.remove_route(Route::Rtc(id)) {
                                self.gone.push(token);
                            }
                        }
                    }
                }
                for (from, b) in socket.channel_mut(1).receive() {
                    if let Some(m) = kumite_net::lobby_msg(&b) {
                        received.push((Route::Rtc(from), m));
                    } else if kumite_net::hello_version(&b).is_some_and(|(v, host)| v != self.version && host) {
                        other_version = true;
                    }
                }
                for (from, b) in socket.channel_mut(0).receive() {
                    if let Some(t) = self.peers.token_of(Route::Rtc(from)) {
                        Self::ggrs_got(&mut self.ggrs_in, &seated, t, &b);
                    }
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            Transport::Udp { socket, host } => {
                let buf = &mut self.udp_buf;
                let mut relay: Vec<(Token, Token, Vec<u8>)> = Vec::new();
                // at most MAX_READS a frame (a flood cannot hold the frame); a read error other than "nothing more"
                // (Windows reports an ICMP error for an earlier send as one) skipped, not the end of the reading
                for _ in 0..MAX_READS {
                    let (n, from) = match socket.recv_from(buf) {
                        Ok(r) => r,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => continue,
                    };
                    // a guest hears only its host
                    // shortcut: known by its address alone (no secret in its packets): one who can send from the host's
                    // address (a forged source, on a network that lets it) speaks for the host; what it can say is still
                    // checked (setups, members, keys), so it can spoil a room, not crash a game
                    if host.is_some_and(|h| h != from) {
                        continue;
                    }
                    let known = self.reliable.iter().position(|r| r.addr == from);
                    if let Some(k) = known {
                        self.reliable[k].heard = Instant::now();
                    }
                    let sender = self.peers.token_of(Route::Udp(from));
                    match buf[..n].split_first() {
                        Some((&TAG_HELLO, rest)) => {
                            // (only a hello: nothing else makes a peer's slot)
                            let Some(m) = kumite_net::lobby_msg(rest).filter(|m| matches!(m, LobbyMsg::Hello { .. })) else {
                                // another version's hello: answered all the same (its sender reads ours, and is told)
                                if let Some((v, claims_host)) = kumite_net::hello_version(rest) {
                                    other_version |= v != self.version && claims_host;
                                    // (at most twice a second: a reply to anyone is no amplifier)
                                    if host.is_none() && self.answered.is_none_or(|t| t.elapsed() >= Duration::from_millis(500)) {
                                        self.answered = Some(Instant::now());
                                        let mut b = vec![TAG_HELLO];
                                        b.extend(serde_json::to_vec(&hello).unwrap_or_default());
                                        let _ = socket.send_to(&b, from);
                                    }
                                }
                                continue;
                            };
                            // (all taken: a refused peer's slot makes room, else the newcomer waits for one)
                            if known.is_none() && self.reliable.len() >= MAX_UDP_PEERS {
                                let refused = |a| self.peers.token_of(Route::Udp(a)).is_some_and(|t| self.refused.iter().any(|r| r.0 == t));
                                let Some(k) = self.reliable.iter().position(|r| refused(r.addr)) else { continue };
                                self.reliable.remove(k);
                            }
                            // the host answers every hello (a guest still asking: its answer got lost)
                            if host.is_none() {
                                let mut b = vec![TAG_HELLO];
                                b.extend(serde_json::to_vec(&hello).unwrap_or_default());
                                let _ = socket.send_to(&b, from);
                            }
                            if known.is_none() {
                                self.reliable.push(Reliable { addr: from, channel: Default::default(), sent: None, heard: Instant::now() });
                            }
                            received.push((Route::Udp(from), m));
                        }
                        Some((&TAG_GGRS, rest)) => {
                            if let Some(t) = sender {
                                Self::ggrs_got(&mut self.ggrs_in, &seated, t, rest);
                            }
                        }
                        Some((&TAG_RELAY, rest)) => {
                            let Some((to, src, payload)) = kumite_net::relay_header(rest) else { continue };
                            if host.is_some() {
                                // a guest: from its host, which said who sent it
                                if to == self.token {
                                    Self::ggrs_got(&mut self.ggrs_in, &seated, src, payload);
                                }
                            } else if let Some(sender) = sender {
                                // the host passes on a guest's packet, as from that guest whatever it says
                                relay.push((to, sender, payload.to_vec()));
                            }
                        }
                        Some((&TAG_LOBBY, rest)) => {
                            let (Some(p), Some(k)) = (kumite_net::lobby_packet(rest), known) else { continue };
                            let r = &mut self.reliable[k];
                            received.extend(r.channel.receive(p).into_iter().map(|m| (Route::Udp(from), m)));
                            // an acknowledgement owed: answer now
                            r.sent = None;
                        }
                        _ => {}
                    }
                }
                for (to, src, bytes) in relay {
                    if let Some(Route::Udp(addr)) = self.peers.route_of(to) {
                        let mut b = vec![TAG_RELAY];
                        b.extend(to.to_le_bytes());
                        b.extend(src.to_le_bytes());
                        b.extend(bytes);
                        let _ = socket.send_to(&b, addr);
                    }
                }
                for r in &mut self.reliable {
                    let due = r.sent.is_none_or(|t| t.elapsed() >= if r.channel.pending() { RESEND } else { KEEPALIVE });
                    if due {
                        if let Some(j) = r.channel.packet() {
                            let mut b = vec![TAG_LOBBY];
                            b.extend_from_slice(&j);
                            let _ = socket.send_to(&b, r.addr);
                        }
                        r.sent = Some(Instant::now());
                    }
                }
                // silent too long: gone
                // (or never acknowledging: what it is sent would only pile up)
                let silent: Vec<std::net::SocketAddr> = self.reliable.iter().filter(|r| r.heard.elapsed() > SILENCE || r.channel.unacked() > MAX_UNACKED).map(|r| r.addr).collect();
                for addr in silent {
                    self.reliable.retain(|r| r.addr != addr);
                    if let Some(token) = self.peers.remove_route(Route::Udp(addr)) {
                        self.gone.push(token);
                    }
                }
            }
        }
        // (a joiner's room ends: the host never takes a stranger's word for it)
        if other_version && !self.host && self.peers.host().is_none() {
            self.refuse("THE OTHER PLAYER HAS ANOTHER VERSION");
        }
        for (route, m) in received {
            match m {
                LobbyMsg::Hello { .. } => self.met(route, m),
                m => {
                    if let Some(t) = self.peers.token_of(route) {
                        if let LobbyMsg::Room { members, .. } = &m {
                            self.seated |= Some(t) == self.peers.host() && members.iter().any(|x| x.token == self.token);
                        }
                        self.inbox.push((t, m));
                    }
                }
            }
        }
        // (a UDP slot is made for each hello that reads: one not met, another version's or a token taken, goes)
        #[cfg(not(target_arch = "wasm32"))]
        {
            let peers = &self.peers;
            self.reliable.retain(|r| peers.token_of(Route::Udp(r.addr)).is_some());
        }
        self.forget_refused();
    }

    /// A UDP guest asks the host for its hello (twice a second until answered).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn ask_host(&mut self) {
        if let Transport::Udp { socket, host: Some(h) } = &self.transport {
            let mut b = vec![TAG_HELLO];
            b.extend(serde_json::to_vec(&self.hello()).unwrap_or_default());
            let _ = socket.send_to(&b, *h);
        }
    }

    /// The room's messages received since the last call: (from, message).
    pub fn take(&mut self) -> Vec<(Token, LobbyMsg)> {
        self.pump();
        std::mem::take(&mut self.inbox)
    }

    /// The peers gone since the last call.
    pub fn take_gone(&mut self) -> Vec<Token> {
        std::mem::take(&mut self.gone)
    }

    /// A match's rollback session starts: its seats as they are now (a finished match's session, still polled a
    /// while, keeps its own while the next one's are set); the last two kept.
    pub fn open_session(&mut self, session: u32) {
        self.sessions.retain(|s| s.0 != session);
        if self.sessions.len() >= 2 {
            self.sessions.remove(0);
        }
        self.sessions.push((session, self.seats.clone(), Vec::new()));
    }

    /// Match `session`'s seats (as it started; the room's now for one never opened).
    fn seats_of(&self, session: u32) -> Vec<Token> {
        self.sessions.iter().find(|s| s.0 == session).map_or_else(|| self.seats.clone(), |s| s.1.clone())
    }

    /// A finished match's last inputs sent again to each seat (ggrs resends only after 200 ms with nothing heard,
    /// and a peer still short of the end keeps resetting that: a lost last packet would hold it for good).
    pub fn resend_inputs(&mut self, session: u32) {
        let Some(s) = self.sessions.iter().find(|s| s.0 == session) else { return };
        let packets: Vec<(Token, Vec<u8>)> = s.2.iter().filter_map(|(seat, b)| Some((*s.1.get(*seat)?, b.clone()))).collect();
        for (to, b) in packets {
            self.ggrs_out(to, b);
        }
    }

    fn ggrs_send(&mut self, session: u32, to_seat: usize, msg: &ggrs::Message) {
        let Some(&to) = self.seats_of(session).get(to_seat) else { return };
        let Some(b) = kumite_net::ggrs_packet(session, msg) else { return };
        self.guard(session).sent(&b);
        // (an Input: the last one to each seat kept, to send again when the match is over here)
        if kumite_net::input_span(&b).is_some() {
            if let Some(s) = self.sessions.iter_mut().find(|s| s.0 == session) {
                s.2.retain(|(seat, _)| *seat != to_seat);
                s.2.push((to_seat, b.clone()));
            }
        }
        self.ggrs_out(to, b);
    }

    /// A rollback packet to a peer, by its route.
    fn ggrs_out(&mut self, to: Token, b: Vec<u8>) {
        let Some(route) = self.peers.route_of(to) else { return };
        match (&mut self.transport, route) {
            (Transport::Rtc { socket }, Route::Rtc(peer)) => {
                let _ = socket.channel_mut(0).try_send(b.into_boxed_slice(), peer);
            }
            #[cfg(not(target_arch = "wasm32"))]
            (Transport::Udp { socket, .. }, Route::Udp(addr)) => {
                let mut p = Vec::with_capacity(b.len() + 1);
                p.push(TAG_GGRS);
                p.extend_from_slice(&b);
                let _ = socket.send_to(&p, addr);
            }
            #[cfg(not(target_arch = "wasm32"))]
            (Transport::Udp { socket, host: Some(h) }, Route::ViaHost) => {
                let mut p = vec![TAG_RELAY];
                p.extend(to.to_le_bytes());
                p.extend(self.token.to_le_bytes());
                p.extend_from_slice(&b);
                let _ = socket.send_to(&p, *h);
            }
            _ => {}
        }
    }

    fn ggrs_receive(&mut self, session: u32) -> Vec<(NetAddr, ggrs::Message)> {
        self.pump();
        let seats = self.seats_of(session);
        let mut out = Vec::new();
        let mut others = std::collections::VecDeque::new();
        for (from, b) in std::mem::take(&mut self.ggrs_in) {
            // (another match's: kept for its own session, a finished one still polled or one coming next (a peer
            // there first); any other match's dropped: a finished one's resends, or junk numbers)
            if let Some(n) = b.get(..4).and_then(|n| n.try_into().ok()).map(u32::from_le_bytes).filter(|&n| n != session) {
                let newest = self.sessions.last().map_or(session, |s| s.0);
                if self.sessions.iter().any(|s| s.0 == n) || n.wrapping_sub(newest) <= SESSIONS_AHEAD {
                    others.push_back((from, b));
                }
                continue;
            }
            let (Some(seat), Some(m)) = (seats.iter().position(|&t| t == from), kumite_net::ggrs_message(&b, session)) else { continue };
            if self.guard(session).accept(from, &b) {
                out.push((NetAddr::Seat(seat as u8), m));
            }
        }
        self.ggrs_in = others;
        out
    }

    /// A rollback packet received: kept for the session (at most [`MAX_GGRS_IN`]: none takes them outside a match).
    fn ggrs_got(ggrs_in: &mut std::collections::VecDeque<(Token, Vec<u8>)>, seated: &[Token], from: Token, b: &[u8]) {
        // (an honest one is under 2 KB, a hundred frames unacknowledged: a big one is junk, not memory to hold; and
        // one from outside the matches would push the seats' own out)
        if b.len() > MAX_GGRS_PACKET || !seated.contains(&from) {
            return;
        }
        if ggrs_in.len() >= MAX_GGRS_IN {
            ggrs_in.pop_front();
        }
        ggrs_in.push_back((from, b.to_vec()));
    }

    /// Tests: a rollback packet received from `from`; the packets queued; none queued.
    #[cfg(test)]
    pub fn test_got(&mut self, from: Token, b: &[u8]) {
        let seated: Vec<Token> = self.seats.iter().chain(self.sessions.iter().flat_map(|s| s.1.iter())).copied().collect();
        Self::ggrs_got(&mut self.ggrs_in, &seated, from, b);
    }
    /// Tests: the peers met, and the UDP slots held.
    #[cfg(test)]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn test_peers(&self) -> (usize, usize) {
        (self.peers.iter().count(), self.reliable.len())
    }
    #[cfg(test)]
    pub fn test_queued(&self) -> usize {
        self.ggrs_in.len()
    }
    #[cfg(test)]
    pub fn test_clear(&mut self) {
        self.ggrs_in.clear();
    }

    /// The rollback inputs' guard of match `session` (a new one for a new match; the last two kept: a finished
    /// match's session is still polled a while beside the next one's).
    fn guard(&mut self, session: u32) -> &mut kumite_net::InputGuard {
        if !self.input_guards.iter().any(|g| g.0 == session) {
            if self.input_guards.len() >= 2 {
                self.input_guards.remove(0);
            }
            self.input_guards.push((session, Default::default()));
        }
        let i = self.input_guards.iter().position(|g| g.0 == session).unwrap_or(0);
        &mut self.input_guards[i].1
    }
}

/// A match's rollback session's socket: the room's link (shared with the room) and the match's number.
pub struct LinkSocket(pub Rc<RefCell<Link>>, pub u32);

impl LinkSocket {
    /// Match `session`'s socket, its seats taken as they are now.
    pub fn open(link: Rc<RefCell<Link>>, session: u32) -> Self {
        link.borrow_mut().open_session(session);
        Self(link, session)
    }
}

impl ggrs::NonBlockingSocket<NetAddr> for LinkSocket {
    fn send_to(&mut self, msg: &ggrs::Message, addr: &NetAddr) {
        if let NetAddr::Seat(s) = addr {
            self.0.borrow_mut().ggrs_send(self.1, *s as usize, msg);
        }
    }

    fn receive_all_messages(&mut self) -> Vec<(NetAddr, ggrs::Message)> {
        self.0.borrow_mut().ggrs_receive(self.1)
    }
}
