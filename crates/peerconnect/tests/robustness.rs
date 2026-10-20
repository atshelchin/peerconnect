//! Arbitrary input must never panic: every parser and the peer state
//! machine are fed pseudo-random bytes (deterministically seeded, so a
//! failure reproduces).

use peerconnect::relay::{Conn, Incoming};
use peerconnect::wire;
use peerconnect::{Invite, Peer, Room, RoomId};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = self.below(max as u64 + 1) as usize;
        (0..len).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn parsers_accept_or_reject_but_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_#:/.=+ ";
    for _ in 0..20_000 {
        let raw = rng.bytes(200);
        let _ = Room::from_bytes(&raw);
        let text: String = raw.iter().map(|b| alphabet[*b as usize % alphabet.len()] as char).collect();
        let _ = text.parse::<Invite>();
        let _ = text.parse::<Room>();
        let _ = text.parse::<RoomId>();
        let _ = format!("wss://r.example#{text}").parse::<Invite>();
        let _ = peerconnect::relay::room_from_path(&format!("/v1/{text}"));
    }
}

#[test]
fn relay_decisions_never_panic() {
    let mut rng = Rng(42);
    let room = Room::create("wss://r.example").unwrap();
    let id = room.id();
    let (pending, _) = Conn::new();
    for _ in 0..20_000 {
        let mut frame = rng.bytes(200);
        if let Some(first) = frame.first_mut() {
            *first = [wire::JOIN, wire::MESSAGE, wire::INIT, 0x7f, 0xff][rng.below(5) as usize];
        }
        for conn in [pending, Conn::Seated(peerconnect::Role::Inviter)] {
            let _ = conn.receive(&id, Incoming::Binary(&frame));
            let _ = conn.receive(&id, Incoming::Text(&String::from_utf8_lossy(&frame)));
        }
    }
}

/// Drives a peer with random events, frames and timings.
fn shake(peer: &mut Peer, rng: &mut Rng, now: &mut u64, steps: usize) {
    let kinds = [wire::HELLO, wire::PRESENCE, wire::INIT, wire::REPLY, wire::MESSAGE, wire::JOIN, 0x00, 0x7f];
    for _ in 0..steps {
        *now += rng.below(30_000);
        match rng.below(10) {
            0 => peer.handle_open(*now),
            1 => peer.handle_close(*now, [None, Some(1000), Some(4409), Some(4401)][rng.below(4) as usize]),
            2 => peer.handle_timeout(*now),
            3 => peer.handle_text(*now, "pong"),
            4 => {
                let _ = peer.send(&rng.bytes(100));
            }
            5 if rng.below(50) == 0 => peer.close(),
            _ => {
                let mut frame = rng.bytes(300);
                match frame.first_mut() {
                    Some(first) => *first = kinds[rng.below(kinds.len() as u64) as usize],
                    None => frame.push(wire::PRESENCE),
                }
                if frame[0] == wire::PRESENCE {
                    frame.truncate(2);
                }
                peer.handle_binary(*now, &frame);
            }
        }
        while peer.poll_output().is_some() {}
        let _ = peer.poll_timeout();
    }
}

#[test]
fn peers_survive_arbitrary_input() {
    let mut rng = Rng(7);
    for round in 0..200 {
        let inviter = Room::create("wss://r.example").unwrap();
        let joiner = Room::join(&inviter.invite().unwrap());
        for room in [inviter, joiner] {
            let mut now = round * 1000;
            let mut peer = Peer::new(room, now);
            shake(&mut peer, &mut rng, &mut now, 300);
        }
    }
}
