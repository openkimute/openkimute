//! The online wire formats (everything here comes from other computers, so nothing here may panic on any bytes):
//! - the room's messages ([`LobbyMsg`], JSON) and a UDP link's lobby packet ([`LobbyPacket`]);
//! - UDP packets' first byte ([`TAG_HELLO`] ...), a relayed packet's header ([`relay_header`]);
//! - a rollback packet: the match's number then the `ggrs` message in bincode ([`ggrs_message`]);
//! - one frame's pad ([`NetInput`]);
//! - a UDP link's reliable channel ([`reliable::Reliable`]).

pub mod peers;
pub mod reliable;

use kumite_sim::input::Pad;
use serde::{Deserialize, Serialize};

/// The room's protocol: computers of another refuse each other. Bump it whenever a message's form changes (the
/// golden tests below, and the match setup's in the app's room tests, fail until then); a change of behaviour is
/// told apart by the hello's build fingerprint (the app's build.rs).
pub const PROTOCOL: u32 = 8;

/// A computer in the room: its token (said in its hello).
pub type Token = u64;

/// A computer of the room as the host lists it: its token, nickname and how many play at it (1 or 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub token: Token,
    pub name: String,
    pub locals: u8,
}

/// What the room says.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LobbyMsg {
    /// Meeting: the version, the nickname, how many play here, the token, whether this one hosts, the converted
    /// assets' fingerprint (manifest.json: the fights stay alike only on the same files), the game build's (the app's
    /// build.rs: the same sources and dependencies).
    Hello { version: u32, name: String, locals: u8, token: Token, host: bool, assets: String, build: String },
    /// The host: who is in the room, in seat order (the host first), the room's seed, and the number its next key
    /// will have (a guest seated now expects that one first).
    Room { members: Vec<Member>, seed: u32, next: u32 },
    /// The host: no room for that one (full, or a match on).
    Refused(String),
    /// A guest's player's key (to the host): the player's number in the room and the key.
    Key { player: u8, key: u8 },
    /// The host: the room's keys in order (every computer applies them alike).
    Event { n: u32, player: u8, key: u8 },
    /// The host starts a match: its setup (serialised), the seed, and the match's number (every computer's
    /// rollback packets carry it: a guest seated late counts from the host's).
    Start { setup: String, seed: u32, session: u32 },
    /// A guest (to the host): its match `session` has loaded (a browser may download its models meanwhile).
    Ready { session: u32 },
    /// The host: every computer's match `session` has loaded (the match's clocks start).
    AllReady { session: u32 },
    /// The host is back on the room's screen: a guest still in a match (one it went on to alone) comes back too.
    Back,
    /// Leaving.
    Bye,
    /// Nothing (keeps a UDP link known to be alive).
    Ping,
}

/// A UDP link's lobby packet: what it has received in order (`ack`), and the messages not yet acknowledged.
#[derive(Serialize, Deserialize)]
pub struct LobbyPacket {
    pub ack: u32,
    pub msgs: Vec<(u32, LobbyMsg)>,
}

/// UDP packets' first byte.
pub const TAG_HELLO: u8 = 0;
pub const TAG_GGRS: u8 = 1;
pub const TAG_LOBBY: u8 = 2;
/// A rollback packet for another guest, through the host: the destination's and the source's tokens, then it.
pub const TAG_RELAY: u8 = 3;

/// A room message from its bytes (None: not one).
pub fn lobby_msg(bytes: &[u8]) -> Option<LobbyMsg> {
    serde_json::from_slice(bytes).ok()
}

/// The version a hello says, and whether it says it hosts, read alone: a hello of another version may not parse as
/// this one's [`LobbyMsg`], and must still be told apart from noise (to say why the room cannot be joined).
pub fn hello_version(bytes: &[u8]) -> Option<(u32, bool)> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let h = v.get("Hello")?;
    Some((h.get("version")?.as_u64()?.try_into().ok()?, h.get("host").and_then(|b| b.as_bool()).unwrap_or(false)))
}

/// A UDP lobby packet from its bytes.
pub fn lobby_packet(bytes: &[u8]) -> Option<LobbyPacket> {
    serde_json::from_slice(bytes).ok()
}

/// A relayed packet's destination, source and payload (after its tag).
pub fn relay_header(rest: &[u8]) -> Option<(Token, Token, &[u8])> {
    let (to, rest) = rest.split_at_checked(8)?;
    let (from, payload) = rest.split_at_checked(8)?;
    Some((u64::from_le_bytes(to.try_into().ok()?), u64::from_le_bytes(from.try_into().ok()?), payload))
}

/// A rollback packet: the match's number, then the message.
pub fn ggrs_packet(session: u32, msg: &ggrs::Message) -> Option<Vec<u8>> {
    let m = bincode::serialize(msg).ok()?;
    let mut b = session.to_le_bytes().to_vec();
    b.extend_from_slice(&m);
    Some(b)
}

/// The message of a rollback packet of match `session` (None: another match's, or not one).
pub fn ggrs_message(bytes: &[u8], session: u32) -> Option<ggrs::Message> {
    let (id, rest) = bytes.split_at_checked(4)?;
    if u32::from_le_bytes(id.try_into().ok()?) != session {
        return None;
    }
    // (bincode with a limit: a length field in a bad packet must not ask for gigabytes)
    use bincode::Options;
    let msg = bincode::DefaultOptions::new().with_fixint_encoding().allow_trailing_bytes().with_limit(64 * 1024).deserialize(rest).ok()?;
    ggrs_fields_ok(rest).then_some(msg)
}

/// The largest frame number a peer may name (77 hours of a match): a bigger one is not a real match's.
pub const MAX_FRAME: i32 = 1 << 24;
/// The most bytes a packet's inputs may decode to (a match's pads are a byte a player a frame).
pub const MAX_INPUT_BYTES: u64 = 16 * 1024;

/// What `ggrs` itself trusts in a message (its fields are its own; read here from the bytes, as bincode laid them
/// out: little-endian, fixed-width ints, u64 lengths): the frame numbers in range, and an input's run-length code
/// whole and small (`bitfield-rle` indexes and allocates by it: a lying length is a panic, or terabytes asked).
fn ggrs_fields_ok(b: &[u8]) -> bool {
    let mut r = Reader { b, at: 0 };
    let (Some(_magic), Some(variant)) = (r.take::<2>(), r.u32()) else { return false };
    match variant {
        // (a disconnection said by a peer: ggrs then loads frames it may not have, a panic; the room ends a match
        // anyway when a computer leaves)
        2 => input_fields(&mut r).is_some_and(|i| !i.disconnect),
        // InputAck: the frame
        3 => frame_ok(r.i32(), -1),
        // ChecksumReport: the checksum (u128), the frame
        6 => r.take::<16>().is_some() && frame_ok(r.i32(), -1),
        _ => true,
    }
}

fn frame_ok(f: Option<i32>, lowest: i32) -> bool {
    f.is_some_and(|f| (lowest..=MAX_FRAME).contains(&f))
}

/// An Input message's fields (after its header and variant).
struct InputFields<'a> {
    /// A peer's status says it disconnected, or the message asks to.
    disconnect: bool,
    start: i32,
    code: &'a [u8],
}

fn input_fields<'a>(r: &mut Reader<'a>) -> Option<InputFields<'a>> {
    // the peers' statuses (disconnected, the last frame each), disconnect requested, the start and the acknowledged
    // frames, the run-length coded inputs
    let n = r.u64().filter(|&n| n <= 16)?;
    let mut disconnect = false;
    for _ in 0..n {
        let [d] = r.take::<1>()?;
        disconnect |= d != 0;
        frame_ok(r.i32(), -1).then_some(())?;
    }
    let [d] = r.take::<1>()?;
    disconnect |= d != 0;
    let start = r.i32().filter(|&s| frame_ok(Some(s), 0))?;
    frame_ok(r.i32(), -1).then_some(())?;
    let len = usize::try_from(r.u64()?).ok()?;
    let code = r.b.get(r.at..r.at.checked_add(len)?)?;
    rle_decode(code).is_some().then_some(InputFields { disconnect, start, code })
}

/// The frames an Input rollback packet carries (its match's number first): the first and the last (None: not an
/// Input, or not one kumite_net lets through).
pub fn input_span(packet: &[u8]) -> Option<(i32, i32)> {
    let mut r = Reader { b: packet.get(4..)?, at: 0 };
    let (Some(_magic), Some(2)) = (r.take::<2>(), r.u32()) else { return None };
    let i = input_fields(&mut r)?;
    // the code decoded: one record (a u16 length, then the bytes) a frame
    let data = rle_decode(i.code)?;
    let (mut at, mut frames) = (0usize, 0i32);
    while let Some(len) = data.get(at..at + 2).map(|l| u16::from_le_bytes([l[0], l[1]]) as usize) {
        at += 2 + len;
        if at > data.len() {
            break;
        }
        frames += 1;
    }
    Some((i.start, i.start + frames - 1))
}

/// A rollback packet's ChecksumReport frame, if it is one.
fn checksum_frame(packet: &[u8]) -> Option<i32> {
    let mut r = Reader { b: packet.get(4..)?, at: 0 };
    let (Some(_magic), Some(6), Some(_checksum)) = (r.take::<2>(), r.u32(), r.take::<16>()) else { return None };
    r.i32()
}

/// A run-length code decoded as `bitfield-rle` does, checked: every varint ends within 9 bytes (no overflow),
/// every literal run lies inside, the whole is at most [`MAX_INPUT_BYTES`] (checked before it is allocated).
fn rle_decode(buf: &[u8]) -> Option<Vec<u8>> {
    let (mut at, mut out) = (0usize, Vec::new());
    while at < buf.len() {
        let (mut next, mut k) = (0u64, 0);
        loop {
            let byte = *buf.get(at + k)?;
            next |= u64::from(byte & 127) << (7 * k);
            k += 1;
            if byte & 128 == 0 {
                break;
            }
            if k == 9 {
                return None;
            }
        }
        at += k;
        let run = if next & 1 == 1 { next >> 2 } else { next >> 1 };
        if out.len() as u64 + run > MAX_INPUT_BYTES {
            return None;
        }
        // (run <= MAX_INPUT_BYTES: no overflow)
        let run = run as usize;
        if next & 1 == 1 {
            out.resize(out.len() + run, if next & 2 != 0 { 255 } else { 0 });
        } else {
            out.extend_from_slice(buf.get(at..at + run)?);
            at += run;
        }
    }
    Some(out)
}

/// Frames of input delay: a pad is used this many frames after it was pressed (fewer rollbacks).
pub const INPUT_DELAY: usize = 2;
/// Frames the prediction may run ahead of the other side before the fight waits.
pub const MAX_PREDICTION: usize = 8;

/// When ggrs says a peer has gone quiet (`NetworkInterrupted`), after half a second.
pub const QUIET_NOTICE: std::time::Duration = std::time::Duration::from_millis(500);
/// ggrs's own disconnection timeout: never reached (an hour). ggrs would disconnect a peer from inside
/// `advance_frame` and run that frame, which with two peers gone at once can load a frame it does not have (a
/// panic, found in review); the app counts the quiet itself ([`Quiet`]) and ends the match between frames.
pub const GGRS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3600);
/// Frames a peer may stay quiet (after [`QUIET_NOTICE`]) before the match ends: 1.5 s more.
pub const QUIET_FRAMES: u32 = 90;

/// The peers gone quiet, and for how many frames: the match's own disconnection timeout ([`GGRS_TIMEOUT`]).
struct Quiet<A> {
    quiet: Vec<(A, u32)>,
}

impl<A> Default for Quiet<A> {
    fn default() -> Self {
        Self { quiet: Vec::new() }
    }
}

impl<A: PartialEq + Clone> Quiet<A> {
    /// A ggrs event: a peer gone quiet, or heard again.
    pub fn event<C: ggrs::Config<Address = A>>(&mut self, e: &ggrs::GgrsEvent<C>) {
        match e {
            ggrs::GgrsEvent::NetworkInterrupted { addr, .. } if !self.quiet.iter().any(|q| q.0 == *addr) => self.quiet.push((addr.clone(), 0)),
            ggrs::GgrsEvent::NetworkResumed { addr } => self.quiet.retain(|q| q.0 != *addr),
            _ => {}
        }
    }

    /// A frame passed: whether a peer has been quiet too long (the match ends, before the next frame runs).
    pub fn frame(&mut self) -> bool {
        for q in &mut self.quiet {
            q.1 = q.1.saturating_add(1);
        }
        self.quiet.iter().any(|q| q.1 >= QUIET_FRAMES)
    }
}

/// Frames without a new confirmed frame (every player's input in) before the match ends: 5 s. (A peer that keeps
/// talking but sends no inputs holds the match at the prediction window, and ggrs counts it as heard.)
pub const STALL_FRAMES: u32 = 300;

/// Frames a match may stay connecting once every computer has loaded it (ggrs has no timeout of its own there: a
/// peer that never answers would hold the others on CONNECTING for good): 10 s.
pub const CONNECT_FRAMES: u32 = 600;
/// What [`Watch::over`] says of a match that never connected.
pub const COULD_NOT_CONNECT: &str = "COULD NOT CONNECT";
/// Frames a match may wait for the other computers to load it (a browser downloads its models: tens of MB): 10
/// minutes.
pub const LOAD_FRAMES: u32 = 36_000;

/// When a match is over, one rule for the app and its tests: no frame may run after ggrs disconnected a peer
/// (ggrs would load a frame it may not have), nor once a peer has gone quiet ([`Quiet`]), left
/// [`MAX_UNACKED`] inputs unacknowledged, or let no frame be confirmed for [`STALL_FRAMES`]; and a match that has
/// not connected in [`CONNECT_FRAMES`] once every computer has loaded it (or in [`LOAD_FRAMES`] while one has not)
/// is over too. In the order they come: MAX_UNACKED (100 frames, 1.7 s), quiet ([`QUIET_NOTICE`] 0.5 s +
/// [`QUIET_FRAMES`] 90, 2 s), STALL_FRAMES (300, 5 s), CONNECT_FRAMES (600, 10 s), LOAD_FRAMES (36000, 10 min);
/// ggrs's own [`GGRS_TIMEOUT`] (an hour) never.
pub struct Watch<A> {
    quiet: Quiet<A>,
    disconnected: bool,
    confirmed: i32,
    still: u32,
    connecting: u32,
    loading: u32,
}

impl<A> Default for Watch<A> {
    fn default() -> Self {
        Self { quiet: Quiet::default(), disconnected: false, confirmed: -1, still: 0, connecting: 0, loading: 0 }
    }
}

// ggrs's own disconnection never comes before the app's: the quiet notice and the frames counted after it (at
// 60 a second), with a minute to spare for a slow frame rate, are far inside GGRS_TIMEOUT (in milliseconds: the
// notice is half a second)
const _: () = assert!(QUIET_NOTICE.as_millis() + (QUIET_FRAMES as u128) * 1000 / 60 + 60_000 < GGRS_TIMEOUT.as_millis());

impl<A: PartialEq + Clone> Watch<A> {
    /// Each of the session's events.
    pub fn event<C: ggrs::Config<Address = A>>(&mut self, e: &ggrs::GgrsEvent<C>) {
        self.quiet.event(e);
        self.disconnected |= matches!(e, ggrs::GgrsEvent::Disconnected { .. });
    }

    /// Once a tick, after polling the session and taking its events, before running a frame: why the match is
    /// over (None: it goes on). `remotes`: the players at other computers; `loading`: another computer has not
    /// loaded the match yet (the clocks wait, up to [`LOAD_FRAMES`]).
    pub fn over<C: ggrs::Config<Address = A>>(&mut self, s: &ggrs::P2PSession<C>, remotes: impl IntoIterator<Item = usize>, loading: bool) -> Option<&'static str> {
        if self.disconnected {
            return Some("DISCONNECTED");
        }
        if loading && s.current_state() != ggrs::SessionState::Running {
            self.loading += 1;
            return (self.loading >= LOAD_FRAMES).then_some("A PLAYER COULD NOT LOAD THE MATCH");
        }
        if s.current_state() != ggrs::SessionState::Running {
            self.connecting += 1;
            return (self.connecting >= CONNECT_FRAMES).then_some(COULD_NOT_CONNECT);
        }
        if self.quiet.frame() {
            return Some("CONNECTION LOST");
        }
        if remotes.into_iter().any(|h| s.network_stats(h).is_ok_and(|n| n.send_queue_len >= MAX_UNACKED)) {
            return Some("DISCONNECTED");
        }
        let c = s.confirmed_frame();
        if c > self.confirmed {
            (self.confirmed, self.still) = (c, 0);
        } else {
            self.still += 1;
            if self.still >= STALL_FRAMES {
                return Some("THE MATCH STOPPED");
            }
        }
        None
    }
}

/// How far past the last frame this computer sent a peer's input may reach. An honest peer is at most the
/// prediction window and the input delay ahead (it waits for this one's inputs past that); ggrs keeps 128 frames
/// of inputs a player and asserts there is room (ggrs 0.13's `INPUT_QUEUE_LENGTH`: a peer far ahead was a panic).
pub const INPUTS_AHEAD: i32 = 64;
/// This computer's inputs a peer may leave unacknowledged (ggrs's `send_queue_len`) before the match ends: ggrs
/// gives up on the peer itself at 128, at a frame it may then fail to load (a panic); an honest peer is a dozen
/// behind at most.
pub const MAX_UNACKED: usize = 100;
/// ggrs 0.13's input queue (frames a player).
const GGRS_INPUT_QUEUE: i32 = 128;
const _: () = assert!(2 * (MAX_PREDICTION + INPUT_DELAY + 1) as i32 <= INPUTS_AHEAD, "an honest peer stays well inside");
const _: () = assert!(INPUTS_AHEAD + ((MAX_PREDICTION + INPUT_DELAY) as i32) < GGRS_INPUT_QUEUE, "a lying one never fills the queue");

/// A match's rollback inputs, kept in step: none lies more than [`INPUTS_AHEAD`] frames past this computer's own
/// (ggrs's queue would overflow), and each peer's continue its last (none skipped ahead); its checksum reports rise.
/// What only the sequence tells, where [`ggrs_message`] reads one message alone.
#[derive(Default)]
pub struct InputGuard {
    /// The last frame this computer sent (-1: none yet).
    sent: Option<i32>,
    /// Each peer's next frame expected (once it has sent one).
    next: Vec<(Token, i32)>,
    /// Each peer's last checksum report's frame.
    checksums: Vec<(Token, i32)>,
}

impl InputGuard {
    /// A rollback packet this computer sends.
    pub fn sent(&mut self, packet: &[u8]) {
        if let Some((_, end)) = input_span(packet) {
            self.sent = Some(self.sent.map_or(end, |s| s.max(end)));
        }
    }

    /// Whether a rollback packet from a peer may go to ggrs (anything but an Input may).
    pub fn accept(&mut self, from: Token, packet: &[u8]) -> bool {
        if let Some(frame) = checksum_frame(packet) {
            // ggrs reports strictly rising frames: one at or below the last would grow its `pending_checksums`
            // without bound (it drops only those below the newest's window)
            if frame > self.sent.unwrap_or(-1) + INPUTS_AHEAD {
                return false;
            }
            return match self.checksums.iter_mut().find(|c| c.0 == from) {
                Some(c) if frame <= c.1 => false,
                Some(c) => {
                    c.1 = frame;
                    true
                }
                None => {
                    self.checksums.push((from, frame));
                    true
                }
            };
        }
        let Some((start, end)) = input_span(packet) else { return true };
        if end > self.sent.unwrap_or(-1) + INPUTS_AHEAD {
            return false;
        }
        // (the first may start anywhere below that: after the input delay)
        match self.next.iter_mut().find(|n| n.0 == from) {
            Some(n) if start > n.1 => false,
            Some(n) => {
                n.1 = n.1.max(end + 1);
                true
            }
            None => {
                self.next.push((from, end + 1));
                true
            }
        }
    }
}

/// Little-endian fields from the front of a slice.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let v = self.b.get(self.at..self.at.checked_add(N)?)?.try_into().ok()?;
        self.at += N;
        Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn i32(&mut self) -> Option<i32> {
        self.take().map(i32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }
}

/// One frame's pad on the wire: buttons 1..4 in bits 0..3, directions in bits 4..7, START in bit 8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetInput(pub u16);

impl NetInput {
    pub fn of(p: Pad) -> Self {
        let d = |b: bool, k: u16| if b { 1 << k } else { 0 };
        NetInput(u16::from(p.buttons & 0xF) | d(p.up, 4) | d(p.down, 5) | d(p.left, 6) | d(p.right, 7) | d(p.start, 8))
    }

    pub fn pad(self) -> Pad {
        let b = |k: u16| self.0 & (1 << k) != 0;
        Pad { up: b(4), down: b(5), left: b(6), right: b(7), buttons: (self.0 & 0xF) as u8, start: b(8) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Input message's bytes (bincode's layout of ggrs 0.13): match 1, two peers' statuses, the frames, the code.
    fn input(start: i32, last: i32, code: &[u8]) -> Vec<u8> {
        let mut b = 1u32.to_le_bytes().to_vec();
        b.extend(0x1234u16.to_le_bytes());
        b.extend(2u32.to_le_bytes());
        b.extend(2u64.to_le_bytes());
        for _ in 0..2 {
            b.push(0);
            b.extend(last.to_le_bytes());
        }
        b.push(0);
        b.extend(start.to_le_bytes());
        b.extend((-1i32).to_le_bytes());
        b.extend((code.len() as u64).to_le_bytes());
        b.extend(code);
        b
    }

    #[test]
    fn a_rollback_input_that_would_crash_ggrs_is_dropped() {
        // a literal run of 3 bytes: one input (length 1, the pad 7)
        assert!(ggrs_message(&input(0, -1, &[6, 1, 0, 7]), 1).is_some());
        // a varint that never ends (bitfield-rle indexes past the end)
        assert!(ggrs_message(&input(0, -1, &[0x80]), 1).is_none());
        // a repeat run of 2^60 bytes (bitfield-rle allocates it)
        let huge = [0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x40];
        assert!(ggrs_message(&input(0, -1, &huge), 1).is_none());
        // a literal run longer than the code
        assert!(ggrs_message(&input(0, -1, &[20, 1]), 1).is_none());
        // frames out of range
        assert!(ggrs_message(&input(-5, -1, &[]), 1).is_none());
        assert!(ggrs_message(&input(0, i32::MAX, &[]), 1).is_none());
    }

    #[test]
    fn a_hello_of_another_version_is_told_apart() {
        let other = br#"{"Hello":{"version":99,"name":"X","locals":1,"token":5,"host":true,"new_field":[1,2]}}"#;
        assert_eq!(hello_version(other), Some((99, true)));
        assert_eq!(hello_version(b"{}"), None);
        let ours = serde_json::to_vec(&LobbyMsg::Hello { version: PROTOCOL, name: "P".into(), locals: 2, token: 9, host: false, assets: "ab".into(), build: "cd".into() }).unwrap();
        assert_eq!(hello_version(&ours), Some((PROTOCOL, false)));
    }

    /// The bytes on the wire stay what they were (a change here needs [`PROTOCOL`] bumped).
    #[test]
    fn the_room_messages_keep_their_wire_form() {
        let j = |m: &LobbyMsg| String::from_utf8(serde_json::to_vec(m).unwrap()).unwrap();
        assert_eq!(
            j(&LobbyMsg::Hello { version: 4, name: "P".into(), locals: 2, token: 9, host: false, assets: "ab".into(), build: "cd".into() }),
            r#"{"Hello":{"version":4,"name":"P","locals":2,"token":9,"host":false,"assets":"ab","build":"cd"}}"#
        );
        assert_eq!(
            j(&LobbyMsg::Room { members: vec![Member { token: 1, name: "A".into(), locals: 1 }], seed: 3, next: 5 }),
            r#"{"Room":{"members":[{"token":1,"name":"A","locals":1}],"seed":3,"next":5}}"#
        );
        assert_eq!(j(&LobbyMsg::Refused("FULL".into())), r#"{"Refused":"FULL"}"#);
        assert_eq!(j(&LobbyMsg::Key { player: 1, key: 5 }), r#"{"Key":{"player":1,"key":5}}"#);
        assert_eq!(j(&LobbyMsg::Event { n: 2, player: 0, key: 16 }), r#"{"Event":{"n":2,"player":0,"key":16}}"#);
        assert_eq!(j(&LobbyMsg::Start { setup: "{}".into(), seed: 7, session: 3 }), r#"{"Start":{"setup":"{}","seed":7,"session":3}}"#);
        assert_eq!(j(&LobbyMsg::Ready { session: 2 }), r#"{"Ready":{"session":2}}"#);
        assert_eq!(j(&LobbyMsg::AllReady { session: 2 }), r#"{"AllReady":{"session":2}}"#);
        assert_eq!(j(&LobbyMsg::Back), r#""Back""#);
        assert_eq!(j(&LobbyMsg::Bye), r#""Bye""#);
        assert_eq!(j(&LobbyMsg::Ping), r#""Ping""#);
        assert_eq!(relay_header(&[1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 9]), Some((1, 2, &[9u8][..])));
    }

    #[test]
    fn a_pad_survives_the_wire() {
        for v in 0..512u16 {
            assert_eq!(NetInput::of(NetInput(v).pad()), NetInput(v));
        }
    }

    #[test]
    fn a_rollback_packet_round_trips_and_other_matches_are_dropped() {
        // a keep-alive: the header's magic (u16), the body's variant 7 (u32)
        let mut b = 3u32.to_le_bytes().to_vec();
        b.extend(0x1234u16.to_le_bytes());
        b.extend(7u32.to_le_bytes());
        let msg = ggrs_message(&b, 3).unwrap();
        assert_eq!(ggrs_packet(3, &msg).unwrap(), b);
        assert!(ggrs_message(&b, 3).is_some());
        assert!(ggrs_message(&b, 4).is_none());
        assert!(ggrs_message(&b[..2], 3).is_none());
    }
}

/// Two real ggrs sessions (one or two players at each computer), their packets through [`ggrs_message`] and
/// [`InputGuard`] as a link sends them: an honest match is never held back, and the guard does read its inputs (a
/// ggrs whose layout changed would pass unchecked: this fails instead).
#[cfg(test)]
mod session_tests {
    use super::*;
    use ggrs::{GgrsRequest, Message, NonBlockingSocket, PlayerType, SessionBuilder};
    use std::sync::{Arc, Mutex};

    struct Cfg;
    impl ggrs::Config for Cfg {
        type Input = NetInput;
        type InputPredictor = ggrs::PredictRepeatLast;
        type State = u64;
        type Address = u8;
    }

    /// Both directions' packets in flight; each side's guard; the packets a guard refused, the Inputs it read.
    #[derive(Default)]
    struct Wire {
        queue: [Vec<Vec<u8>>; 2],
        guard: [InputGuard; 2],
        refused: usize,
        inputs: usize,
    }

    struct Sock(usize, Arc<Mutex<Wire>>);

    impl NonBlockingSocket<u8> for Sock {
        fn send_to(&mut self, msg: &Message, _: &u8) {
            let mut w = self.1.lock().unwrap();
            if let Some(b) = ggrs_packet(1, msg) {
                w.guard[self.0].sent(&b);
                w.queue[1 - self.0].push(b);
            }
        }
        fn receive_all_messages(&mut self) -> Vec<(u8, Message)> {
            let mut w = self.1.lock().unwrap();
            let w = &mut *w;
            let mut out = Vec::new();
            for b in std::mem::take(&mut w.queue[self.0]) {
                w.inputs += usize::from(input_span(&b).is_some());
                if !w.guard[self.0].accept(1, &b) {
                    w.refused += 1;
                    continue;
                }
                if let Some(m) = ggrs_message(&b, 1) {
                    out.push((1 - self.0 as u8, m));
                }
            }
            out
        }
    }

    /// `locals` players at each computer; 600 frames.
    fn honest_match(locals: usize) {
        let wire = Arc::new(Mutex::new(Wire::default()));
        let players = 2 * locals;
        let mut sessions: Vec<_> = (0..2)
            .map(|me| {
                let mut b = SessionBuilder::<Cfg>::new()
                    .with_num_players(players)
                    .unwrap()
                    .with_input_delay(INPUT_DELAY)
                    .with_max_prediction_window(MAX_PREDICTION)
                    .with_disconnect_notify_delay(QUIET_NOTICE)
                    .with_disconnect_timeout(GGRS_TIMEOUT)
                    .with_desync_detection_mode(ggrs::DesyncDetection::On { interval: 10 });
                for p in 0..players {
                    let at = p / locals;
                    b = b.add_player(if at == me { PlayerType::Local } else { PlayerType::Remote(at as u8) }, p).unwrap();
                }
                b.start_p2p_session(Sock(me, wire.clone())).unwrap()
            })
            .collect();
        let mut state = [0u64; 2];
        let mut frames = [0usize; 2];
        let mut watch: [Watch<u8>; 2] = Default::default();
        let start = std::time::Instant::now();
        while frames.iter().any(|&f| f < 600) && start.elapsed().as_secs() < 20 {
            for (me, s) in sessions.iter_mut().enumerate() {
                s.poll_remote_clients();
                for e in s.events() {
                    watch[me].event(&e);
                }
                let remotes = (0..players).filter(|p| p / locals != me);
                assert_eq!(watch[me].over(s, remotes, false), None, "an honest match goes on");
                if s.current_state() != ggrs::SessionState::Running {
                    continue;
                }
                if (0..locals).any(|k| s.add_local_input(me * locals + k, NetInput(((frames[me] * 7 + me + k) & 0xFF) as u16)).is_err()) {
                    continue;
                }
                let Ok(requests) = s.advance_frame() else { continue };
                for r in requests {
                    match r {
                        GgrsRequest::SaveGameState { cell, frame } => cell.save(frame, Some(state[me]), Some(u128::from(state[me]))),
                        GgrsRequest::LoadGameState { cell, .. } => state[me] = cell.load().unwrap_or_default(),
                        GgrsRequest::AdvanceFrame { inputs } => {
                            state[me] = inputs.iter().fold(state[me].wrapping_mul(31), |h, i| h.wrapping_mul(131) ^ u64::from(i.0 .0));
                            frames[me] += 1;
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let w = wire.lock().unwrap();
        assert!(frames.iter().all(|&f| f >= 600), "both ran: {frames:?}, refused {}", w.refused);
        assert_eq!(w.refused, 0, "nothing honest refused");
        assert!(w.inputs >= 600, "the guard read the inputs ({})", w.inputs);
    }

    #[test]
    fn an_honest_match_is_never_held_back() {
        honest_match(1);
    }

    #[test]
    fn an_honest_match_of_four_at_two_computers_is_never_held_back() {
        honest_match(2);
    }
    /// A peer that plays along but never acknowledges this one's inputs: ggrs disconnects it, at a frame it would
    /// fail to load on the next frame run (a panic, found in review); with the app's rules no frame runs after the
    /// disconnection, whatever lag the peer keeps.
    #[test]
    fn a_peer_that_never_acknowledges_ends_the_match_before_ggrs_panics() {
        // `frames` of input from `start`, acknowledging nothing
        let packet = |start: i32, frames: &[u8]| {
            let mut delta = Vec::new();
            // (a frame's input: NetInput, two bytes)
            for &f in frames {
                delta.extend(2u16.to_le_bytes());
                delta.extend([f, 0]);
            }
            let mut code = Vec::new();
            let mut v = (delta.len() as u64) << 1;
            loop {
                let b = (v & 127) as u8;
                v >>= 7;
                if v == 0 {
                    code.push(b);
                    break;
                }
                code.push(b | 128);
            }
            code.extend(delta);
            let mut b = 1u32.to_le_bytes().to_vec();
            b.extend([0x34, 0x12]);
            b.extend(2u32.to_le_bytes());
            b.extend(2u64.to_le_bytes());
            for _ in 0..2 {
                b.push(0);
                b.extend((-1i32).to_le_bytes());
            }
            b.push(0);
            b.extend(start.to_le_bytes());
            b.extend((-1i32).to_le_bytes());
            b.extend((code.len() as u64).to_le_bytes());
            b.extend(code);
            b
        };
        // unpaced: ggrs's own disconnection comes first (its stats wait a second); at 60 frames a second, as the
        // app runs: the app's own rule (MAX_UNACKED) does
        for (lag, paced) in [(-3, false), (-2, false), (-1, false), (0, false), (1, false), (2, false), (-2, true), (0, true), (2, true)] {
            let wire = Arc::new(Mutex::new(Wire::default()));
            let mut watch: Watch<u8> = Watch::default();
            let mut by_stats = false;
            let mut s = SessionBuilder::<Cfg>::new()
                .with_num_players(2)
                .unwrap()
                .with_input_delay(INPUT_DELAY)
                .with_max_prediction_window(MAX_PREDICTION)
                .with_disconnect_notify_delay(QUIET_NOTICE)
                .with_disconnect_timeout(GGRS_TIMEOUT)
                .add_player(PlayerType::Local, 0)
                .unwrap()
                .add_player(PlayerType::Remote(1), 1)
                .unwrap()
                .start_p2p_session(Sock(0, wire.clone()))
                .unwrap();
            // the peer answers the sync at once
            let mut state = 0u64;
            let mut next = 0;
            let mut ended = false;
            for t in 0..2000 {
                {
                    let mut w = wire.lock().unwrap();
                    let w = &mut *w;
                    // (sync requests out: answered)
                    let requests: Vec<Vec<u8>> = std::mem::take(&mut w.queue[1]);
                    for b in requests {
                        if b.get(6..10) == Some(&[0, 0, 0, 0]) {
                            let mut reply = 1u32.to_le_bytes().to_vec();
                            reply.extend([0x34, 0x12]);
                            reply.extend(1u32.to_le_bytes());
                            reply.extend(&b[10..14]);
                            w.queue[0].push(reply);
                        }
                    }
                    let end = s.current_frame() - lag;
                    if s.current_state() == ggrs::SessionState::Running && end >= next {
                        let frames: Vec<u8> = (next..=end).map(|f| f as u8).collect();
                        w.queue[0].push(packet(next, &frames));
                        next = end + 1;
                    }
                }
                s.poll_remote_clients();
                // the app's rule (Watch, as NetPlay::tick): no frame runs once the match is over
                for e in s.events() {
                    watch.event(&e);
                }
                by_stats = s.network_stats(1).is_ok_and(|n| n.send_queue_len >= MAX_UNACKED);
                if watch.over(&s, [1], false).is_some() {
                    ended = true;
                    break;
                }
                if s.current_state() != ggrs::SessionState::Running {
                    continue;
                }
                if s.add_local_input(0, NetInput((t & 0xFF) as u16)).is_err() {
                    continue;
                }
                if paced {
                    std::thread::sleep(std::time::Duration::from_millis(16));
                }
                let Ok(requests) = s.advance_frame() else { continue };
                for r in requests {
                    match r {
                        GgrsRequest::SaveGameState { cell, frame } => cell.save(frame, Some(state), None),
                        GgrsRequest::LoadGameState { cell, .. } => state = cell.load().unwrap_or_default(),
                        GgrsRequest::AdvanceFrame { inputs } => state = inputs.iter().fold(state, |h, i| h.wrapping_mul(131) ^ u64::from(i.0 .0)),
                    }
                }
            }
            assert!(ended, "lag {lag}: the peer counted as gone");
            assert!(by_stats || !paced, "lag {lag}: at the app's pace, its own rule ends the match before ggrs's");
            assert_eq!(wire.lock().unwrap().refused, 0);
        }
    }

    /// Three computers; the two others go silent at once (this one's network dropped): the match ends. (ggrs's own
    /// timeout, crossed inside a frame, could load one it does not have with two peers gone: a panic found in review
    /// with a lagging peer. That cannot happen now by construction: ggrs's timeout is never reached, a compile-time
    /// check above. This test checks the app's own count ends the match.)
    #[test]
    fn two_peers_going_quiet_at_once_end_the_match_without_a_panic() {
        /// Packets in flight to each computer: (from, bytes).
        type Mesh = Arc<Mutex<Vec<Vec<(u8, Vec<u8>)>>>>;
        struct MeshSock(u8, Mesh);
        impl NonBlockingSocket<u8> for MeshSock {
            fn send_to(&mut self, msg: &Message, to: &u8) {
                if let Some(b) = ggrs_packet(1, msg) {
                    self.1.lock().unwrap()[*to as usize].push((self.0, b));
                }
            }
            fn receive_all_messages(&mut self) -> Vec<(u8, Message)> {
                std::mem::take(&mut self.1.lock().unwrap()[self.0 as usize]).into_iter().filter_map(|(from, b)| Some((from, ggrs_message(&b, 1)?))).collect()
            }
        }
        for run in 0..40u64 {
            let mesh: Mesh = Arc::new(Mutex::new(vec![Vec::new(); 3]));
            let mut sessions: Vec<_> = (0..3u8)
                .map(|me| {
                    let mut b = SessionBuilder::<Cfg>::new()
                        .with_num_players(3)
                        .unwrap()
                        .with_input_delay(INPUT_DELAY)
                        .with_max_prediction_window(MAX_PREDICTION)
                        .with_disconnect_notify_delay(std::time::Duration::from_millis(20))
                        .with_disconnect_timeout(GGRS_TIMEOUT);
                    for p in 0..3u8 {
                        b = b.add_player(if p == me { PlayerType::Local } else { PlayerType::Remote(p) }, p as usize).unwrap();
                    }
                    b.start_p2p_session(MeshSock(me, mesh.clone())).unwrap()
                })
                .collect();
            let mut state = [0u64; 3];
            let mut frames = [0usize; 3];
            let mut watch: Watch<u8> = Watch::default();
            let stop = 60 + (run as usize * 7) % 40;
            let mut ended = false;
            let start = std::time::Instant::now();
            while !ended && start.elapsed().as_secs() < 20 {
                for (me, s) in sessions.iter_mut().enumerate() {
                    // the two others stop at `stop`: nothing more from them, nothing reaches them
                    if me > 0 && frames[0] >= stop {
                        mesh.lock().unwrap()[me].clear();
                        continue;
                    }
                    s.poll_remote_clients();
                    if me == 0 {
                        for e in s.events() {
                            watch.event(&e);
                        }
                        if watch.over(s, [1, 2], false).is_some() {
                            ended = true;
                            break;
                        }
                    } else {
                        let _ = s.events().count();
                    }
                    if s.current_state() != ggrs::SessionState::Running || s.add_local_input(me, NetInput((frames[me] & 0xFF) as u16)).is_err() {
                        continue;
                    }
                    let Ok(requests) = s.advance_frame() else { continue };
                    for r in requests {
                        match r {
                            GgrsRequest::SaveGameState { cell, frame } => cell.save(frame, Some(state[me]), None),
                            GgrsRequest::LoadGameState { cell, .. } => state[me] = cell.load().unwrap_or_default(),
                            GgrsRequest::AdvanceFrame { inputs } => {
                                state[me] = inputs.iter().fold(state[me], |h, i| h.wrapping_mul(131) ^ u64::from(i.0 .0));
                                frames[me] += 1;
                            }
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_micros(500 + (run * 131 % 1500)));
            }
            assert!(ended, "run {run}: the match ended (frames {frames:?})");
        }
    }

    /// A peer that keeps talking (acknowledging, reporting) but sends no inputs: ggrs counts it as heard, and the
    /// match would sit at the prediction window for good; it ends once nothing is confirmed for STALL_FRAMES.
    #[test]
    fn a_peer_that_talks_but_sends_no_inputs_ends_the_match() {
        let wire = Arc::new(Mutex::new(Wire::default()));
        let mut sessions: Vec<_> = (0..2)
            .map(|me| {
                let b = SessionBuilder::<Cfg>::new()
                    .with_num_players(2)
                    .unwrap()
                    .with_input_delay(INPUT_DELAY)
                    .with_max_prediction_window(MAX_PREDICTION)
                    .with_disconnect_notify_delay(QUIET_NOTICE)
                    .with_disconnect_timeout(GGRS_TIMEOUT);
                let b = if me == 0 {
                    b.add_player(PlayerType::Local, 0).unwrap().add_player(PlayerType::Remote(1), 1)
                } else {
                    b.add_player(PlayerType::Remote(0), 0).unwrap().add_player(PlayerType::Local, 1)
                };
                b.unwrap().start_p2p_session(Sock(me, wire.clone())).unwrap()
            })
            .collect();
        let mut watch: Watch<u8> = Watch::default();
        let mut frames = [0usize; 2];
        let mut why = None;
        for _ in 0..5000 {
            for (me, s) in sessions.iter_mut().enumerate() {
                // from frame 60 on, the second one's inputs never reach the first (all else does)
                if me == 0 && frames[1] >= 60 {
                    wire.lock().unwrap().queue[0].retain(|b| input_span(b).is_none());
                }
                s.poll_remote_clients();
                if me == 0 {
                    for e in s.events() {
                        watch.event(&e);
                    }
                    why = watch.over(s, [1], false);
                    if why.is_some() {
                        break;
                    }
                } else {
                    let _ = s.events().count();
                }
                if s.current_state() != ggrs::SessionState::Running || s.add_local_input(me, NetInput(1)).is_err() {
                    continue;
                }
                let Ok(requests) = s.advance_frame() else { continue };
                for r in requests {
                    match r {
                        GgrsRequest::SaveGameState { cell, frame } => cell.save(frame, Some(0), None),
                        GgrsRequest::AdvanceFrame { .. } => frames[me] += 1,
                        GgrsRequest::LoadGameState { .. } => {}
                    }
                }
            }
            if why.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        assert_eq!(why, Some("THE MATCH STOPPED"), "frames {frames:?}");
    }

    /// A peer that never answers the sync: the match is over after CONNECT_FRAMES (ggrs would wait for good).
    #[test]
    fn a_match_that_never_connects_ends() {
        let wire = Arc::new(Mutex::new(Wire::default()));
        let s =
            SessionBuilder::<Cfg>::new().with_num_players(2).unwrap().add_player(PlayerType::Local, 0).unwrap().add_player(PlayerType::Remote(1), 1).unwrap().start_p2p_session(Sock(0, wire)).unwrap();
        let mut watch: Watch<u8> = Watch::default();
        let ended = (0..CONNECT_FRAMES + 1).position(|_| watch.over(&s, [1], false).is_some());
        assert_eq!(ended, Some(CONNECT_FRAMES as usize - 1));
    }

    /// While another computer loads, the match waits up to LOAD_FRAMES; once all have loaded, the connect clock
    /// starts from zero.
    #[test]
    fn a_match_waits_for_loading_then_its_connect_clock_starts() {
        let session = || {
            SessionBuilder::<Cfg>::new()
                .with_num_players(2)
                .unwrap()
                .add_player(PlayerType::Local, 0)
                .unwrap()
                .add_player(PlayerType::Remote(1), 1)
                .unwrap()
                .start_p2p_session(Sock(0, Arc::new(Mutex::new(Wire::default()))))
                .unwrap()
        };
        let s = session();
        let mut watch: Watch<u8> = Watch::default();
        let ended = (0..LOAD_FRAMES + 1).position(|_| watch.over(&s, [1], true).is_some());
        assert_eq!(ended, Some(LOAD_FRAMES as usize - 1));
        let s = session();
        let mut watch: Watch<u8> = Watch::default();
        assert!((0..LOAD_FRAMES - 1).all(|_| watch.over(&s, [1], true).is_none()), "a long load is waited for");
        let ended = (0..CONNECT_FRAMES + 1).position(|_| watch.over(&s, [1], false).is_some());
        assert_eq!(ended, Some(CONNECT_FRAMES as usize - 1), "then the connect clock, from zero");
    }

    #[test]
    fn a_peer_far_ahead_or_skipping_is_refused() {
        // one frame of input from `start`, as an Input packet of match 1
        let input = |start: i32| {
            let mut b = 1u32.to_le_bytes().to_vec();
            b.extend(0u16.to_le_bytes());
            b.extend(2u32.to_le_bytes());
            b.extend(1u64.to_le_bytes());
            b.push(0);
            b.extend((-1i32).to_le_bytes());
            b.push(0);
            b.extend(start.to_le_bytes());
            b.extend((-1i32).to_le_bytes());
            b.extend(4u64.to_le_bytes());
            b.extend([6, 1, 0, 7]);
            b
        };
        assert_eq!(input_span(&input(5)), Some((5, 5)));
        let mut g = InputGuard::default();
        assert!(!g.accept(9, &input(500)), "a first far past what this one sent");
        assert!(g.accept(9, &input(0)));
        assert!(!g.accept(9, &input(2)), "frame 1 skipped");
        assert!(g.accept(9, &input(1)));
        assert!(g.accept(9, &input(1)), "sent again (not acknowledged yet)");
        assert!(!g.accept(9, &input(INPUTS_AHEAD + 5)), "far past what this one sent");
        g.sent(&input(100));
        for f in 2..=100 + INPUTS_AHEAD {
            assert!(g.accept(9, &input(f)));
        }
        assert!(!g.accept(9, &input(101 + INPUTS_AHEAD)));
        // checksum reports: each peer's rising, none far past what this one sent
        let report = |frame: i32| {
            let mut b = 1u32.to_le_bytes().to_vec();
            b.extend(0u16.to_le_bytes());
            b.extend(6u32.to_le_bytes());
            b.extend([0; 16]);
            b.extend(frame.to_le_bytes());
            b
        };
        assert!(g.accept(9, &report(60)));
        assert!(!g.accept(9, &report(60)), "the same frame again");
        assert!(!g.accept(9, &report(50)), "a falling frame");
        assert!(g.accept(7, &report(50)), "another peer's own");
        assert!(g.accept(9, &report(70)));
        assert!(!g.accept(9, &report(101 + INPUTS_AHEAD)));
        // a disconnection said: refused alone
        let mut d = input(0);
        d[4 + 2 + 4 + 8] = 1;
        assert!(ggrs_message(&d, 1).is_none());
    }
}
