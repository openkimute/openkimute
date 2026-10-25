//! A UDP link's reliable channel for the room's messages: each numbered, sent again in every lobby packet until the
//! other side acknowledges it, delivered in order and once. No socket and no clock here: the link sends
//! [`Reliable::packet`] when it is due and hands over what arrives ([`Reliable::receive`]).

use crate::{LobbyMsg, LobbyPacket};
use std::collections::{BTreeMap, VecDeque};

/// How far ahead of the next expected message one is kept (further ones are dropped: a peer never runs that far
/// ahead, and memory stays bounded whatever it sends).
pub const WINDOW: u32 = 256;
/// A lobby packet's largest (bytes of JSON): below a datagram that the network would cut in pieces. A single message
/// bigger than this still goes alone.
pub const MAX_PACKET: usize = 1200;

#[derive(Default)]
pub struct Reliable {
    /// Sent, not yet acknowledged, oldest first; the next one's number.
    out: VecDeque<(u32, LobbyMsg)>,
    next_out: u32,
    /// The next number expected, and those that came before it.
    next_in: u32,
    early: BTreeMap<u32, LobbyMsg>,
}

impl Reliable {
    /// Queue a message.
    pub fn push(&mut self, msg: LobbyMsg) {
        self.out.push_back((self.next_out, msg));
        self.next_out = self.next_out.wrapping_add(1);
    }

    /// How many messages wait for the ones before them (at most [`WINDOW`]).
    pub fn waiting(&self) -> usize {
        self.early.len()
    }

    /// How many sent are still unacknowledged.
    pub fn unacked(&self) -> usize {
        self.out.len()
    }

    /// Whether something sent is still unacknowledged.
    pub fn pending(&self) -> bool {
        !self.out.is_empty()
    }

    /// A packet from the other side: what it acknowledges is forgotten; returns its messages now in order.
    pub fn receive(&mut self, p: LobbyPacket) -> Vec<LobbyMsg> {
        // (an acknowledgement names the next number it expects: everything before it arrived)
        let acked = p.ack.wrapping_sub(self.out.front().map_or(p.ack, |m| m.0));
        if acked <= self.out.len() as u32 {
            self.out.drain(..acked as usize);
        }
        for (n, m) in p.msgs {
            if n.wrapping_sub(self.next_in) < WINDOW {
                self.early.entry(n).or_insert(m);
            }
        }
        let mut got = Vec::new();
        while let Some(m) = self.early.remove(&self.next_in) {
            got.push(m);
            self.next_in = self.next_in.wrapping_add(1);
        }
        // (numbers wrapped past: kept ones now behind the window are stale)
        let next = self.next_in;
        self.early.retain(|&n, _| n.wrapping_sub(next) < WINDOW);
        got
    }

    /// The packet to send: the acknowledgement and the oldest unacknowledged messages that fit in [`MAX_PACKET`]
    /// (at least one). None: it could not be written.
    pub fn packet(&self) -> Option<Vec<u8>> {
        let mut msgs = Vec::new();
        let mut size = 32;
        for m in &self.out {
            let n = serde_json::to_vec(m).ok()?.len() + 1;
            if !msgs.is_empty() && size + n > MAX_PACKET {
                break;
            }
            size += n;
            msgs.push(m.clone());
        }
        serde_json::to_vec(&LobbyPacket { ack: self.next_in, msgs }).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deliver(from: &Reliable, to: &mut Reliable) -> Vec<LobbyMsg> {
        let b = from.packet().unwrap();
        to.receive(crate::lobby_packet(&b).unwrap())
    }

    fn keys(m: &[LobbyMsg]) -> Vec<u32> {
        m.iter().map(|m| if let LobbyMsg::Event { n, .. } = m { *n } else { u32::MAX }).collect()
    }

    #[test]
    fn messages_arrive_in_order_once_and_are_forgotten_when_acknowledged() {
        let (mut a, mut b) = (Reliable::default(), Reliable::default());
        for n in 0..5 {
            a.push(LobbyMsg::Event { n, player: 0, key: 5 });
        }
        assert_eq!(keys(&deliver(&a, &mut b)), [0, 1, 2, 3, 4]);
        // sent again (no acknowledgement yet): nothing twice
        assert!(deliver(&a, &mut b).is_empty());
        deliver(&b, &mut a);
        assert!(!a.pending());
    }

    #[test]
    fn a_big_backlog_goes_in_packets_that_fit() {
        let (mut a, mut b) = (Reliable::default(), Reliable::default());
        for n in 0..200 {
            a.push(LobbyMsg::Event { n, player: 1, key: 16 });
        }
        let mut got = Vec::new();
        while a.pending() {
            assert!(a.packet().unwrap().len() <= MAX_PACKET);
            got.extend(keys(&deliver(&a, &mut b)));
            deliver(&b, &mut a);
        }
        assert_eq!(got, (0..200).collect::<Vec<_>>());
    }

    #[test]
    fn a_peer_far_ahead_or_behind_is_ignored() {
        let mut b = Reliable::default();
        let far: Vec<(u32, LobbyMsg)> = (WINDOW..WINDOW + 10_000).chain([u32::MAX]).map(|n| (n, LobbyMsg::Ping)).collect();
        assert!(b.receive(LobbyPacket { ack: u32::MAX, msgs: far }).is_empty());
        assert!(b.early.is_empty());
        // and a lying acknowledgement does not lose what is unacknowledged
        let mut a = Reliable::default();
        a.push(LobbyMsg::Ping);
        a.receive(LobbyPacket { ack: 1000, msgs: Vec::new() });
        assert!(a.pending());
    }
}
