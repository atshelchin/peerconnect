//! Group frame layout, classes and timers (GROUP.md §3, §4).
//!
//! Peer frames start `type from to`. The relay reads those three bytes and
//! nothing else: `from` must be the sender's slot, `to` a slot or
//! [`BROADCAST`], and the type's range gives the frame's class, which says
//! how urgent it is and whether it may be dropped.

use std::time::Duration;

pub use crate::wire::{HELLO, NONCE_LEN, PING, PONG};

/// Member → relay: `token[16] signature[64]`.
pub const JOIN: u8 = 0x81;
/// Relay → member: `slot[1] present[4]`, own slot and a bitmap of the others.
pub const WELCOME: u8 = 0x83;
/// Relay → member: `slot[1] present[1]`.
pub const MEMBER: u8 = 0x84;

/// Announces a connection's sender key (encrypted under the group secret).
pub const KEY: u8 = 0x01;
/// Protocol control: admission, resends, state transfer.
pub const CONTROL: u8 = 0x02;
pub const COMMAND: u8 = 0x03;
pub const VOICE: u8 = 0x21;
/// Application data that is dropped rather than delayed (positions, poses).
pub const DATAGRAM: u8 = 0x22;
/// A fragment of a video frame (GROUP.md §7.4).
pub const VIDEO: u8 = 0x31;
pub const CHAT: u8 = 0x41;
/// State transfer data.
pub const BULK: u8 = 0x61;

/// `to` for every other member.
pub const BROADCAST: u8 = 0xFF;
pub const MAX_SLOTS: usize = 32;
/// Largest frame. Small frames keep a less urgent frame from holding up a
/// more urgent one for long, wherever they queue.
pub const MAX_FRAME_LEN: usize = 16 << 10;
pub const HEADER_LEN: usize = 3;
/// `type from to counter[4]`.
pub const SEALED_HEADER_LEN: usize = HEADER_LEN + 4;
pub const TAG_LEN: usize = 16;
/// Largest plaintext of a sealed frame.
pub const MAX_PLAINTEXT_LEN: usize = MAX_FRAME_LEN - SEALED_HEADER_LEN - TAG_LEN;
/// Largest message payload (after a kind byte and an id).
pub const MAX_PAYLOAD_LEN: usize = MAX_PLAINTEXT_LEN - 9;
/// Largest voice frame or datagram.
pub const MAX_DATAGRAM_LEN: usize = MAX_PLAINTEXT_LEN - 2;
/// Video: a fragment's header, `track[1] frame[4] index[1] count[1] flags[1] timestamp[4]`.
pub const VIDEO_HEADER_LEN: usize = 12;
/// Video data per fragment, at most: small enough for a QUIC datagram on any path.
pub const MAX_VIDEO_FRAGMENT: usize = 1000;
/// Largest video frame: 255 fragments.
pub const MAX_VIDEO_FRAME: usize = 255 * MAX_VIDEO_FRAGMENT;
pub const MAX_PROFILE_LEN: usize = 1024;

pub const TOKEN_LEN: usize = 16;
pub const JOIN_LEN: usize = 1 + TOKEN_LEN + 64;
pub const WELCOME_LEN: usize = 1 + 1 + 4;
pub const MEMBER_LEN: usize = 1 + 1 + 1;

pub const JOIN_CONTEXT: &[u8] = b"peerconnect/g1 join";
pub const KEY_CONTEXT: &[u8] = b"peerconnect/g1 key";

pub const PING_INTERVAL: Duration = Duration::from_secs(10);
pub const RECEIVE_TIMEOUT: Duration = Duration::from_secs(25);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(40);
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(10);

/// How a frame is delivered, and how urgent it is: see [`Class::PRIORITY`].
/// The numbers are the classes' QUIC stream headers (GROUP.md §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// Commands and protocol control (and relay frames): reliable.
    Command = 0,
    /// Voice and datagrams: dropped rather than delayed.
    Voice = 1,
    /// Chat: reliable.
    Chat = 2,
    /// State transfer: reliable, flow-controlled end to end.
    Bulk = 3,
    /// Video: dropped rather than delayed, like voice, but less urgent than
    /// chat, which it could otherwise starve.
    Video = 4,
}

impl Class {
    pub const ALL: [Class; 5] = [Class::Command, Class::Voice, Class::Chat, Class::Bulk, Class::Video];
    /// Most urgent first.
    pub const PRIORITY: [Class; 5] = [Class::Command, Class::Voice, Class::Chat, Class::Video, Class::Bulk];
    pub const COUNT: usize = 5;

    /// The class of a peer frame type (`0x00`–`0x7F`).
    pub fn of(frame_type: u8) -> Class {
        match frame_type >> 4 {
            0 | 1 => Class::Command,
            2 => Class::Voice,
            3 => Class::Video,
            4 | 5 => Class::Chat,
            _ => Class::Bulk,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// Whether frames of this class may be dropped instead of queued.
    pub fn is_realtime(self) -> bool {
        matches!(self, Class::Voice | Class::Video)
    }

    /// Higher is more urgent: 4 for commands down to 0 for bulk.
    pub fn urgency(self) -> u8 {
        (Class::COUNT - 1 - Class::PRIORITY.iter().position(|&c| c == self).unwrap()) as u8
    }
}

/// The message a member signs to answer a `HELLO`.
pub fn join_message(nonce: &[u8; NONCE_LEN], token: &[u8; TOKEN_LEN]) -> Vec<u8> {
    [JOIN_CONTEXT, nonce, token].concat()
}

pub fn welcome(slot: u8, present: u32) -> [u8; WELCOME_LEN] {
    let mut f = [WELCOME, slot, 0, 0, 0, 0];
    f[2..].copy_from_slice(&present.to_be_bytes());
    f
}

pub fn member(slot: u8, present: bool) -> [u8; MEMBER_LEN] {
    [MEMBER, slot, present as u8]
}

/// Checks a peer frame's header against the sender's slot. Returns the
/// recipient (`None` for everyone) and the class, or `None` if the frame
/// is not a valid peer frame from `slot`.
pub fn route(slot: u8, frame: &[u8]) -> Option<(Option<u8>, Class)> {
    let [kind, from, to, ..] = *frame else { return None };
    if kind >= 0x80 || from != slot {
        return None;
    }
    let to = match to {
        BROADCAST => None,
        to if (to as usize) < MAX_SLOTS && to != slot => Some(to),
        _ => return None,
    };
    Some((to, Class::of(kind)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_and_routes() {
        assert_eq!(Class::of(KEY), Class::Command);
        assert_eq!(Class::of(COMMAND), Class::Command);
        assert_eq!(Class::of(VOICE), Class::Voice);
        assert_eq!(Class::of(DATAGRAM), Class::Voice);
        assert_eq!(Class::of(0x2F), Class::Voice);
        assert_eq!(Class::of(VIDEO), Class::Video);
        assert_eq!(Class::of(0x3F), Class::Video);
        assert_eq!(Class::of(0x40), Class::Chat);
        assert_eq!(Class::of(0x1F), Class::Command);
        assert_eq!(Class::PRIORITY.map(Class::urgency), [4, 3, 2, 1, 0], "commands, voice, chat, video, bulk");
        assert!(Class::Video.is_realtime() && Class::Voice.is_realtime() && !Class::Chat.is_realtime());
        assert_eq!(Class::of(CHAT), Class::Chat);
        assert_eq!(Class::of(BULK), Class::Bulk);
        assert_eq!(Class::of(0x7F), Class::Bulk);
        assert_eq!(route(3, &[COMMAND, 3, BROADCAST]), Some((None, Class::Command)));
        assert_eq!(route(3, &[VOICE, 3, 4, 9]), Some((Some(4), Class::Voice)));
        assert_eq!(route(3, &[VOICE, 2, 4]), None); // not its slot
        assert_eq!(route(3, &[VOICE, 3, 3]), None); // to itself
        assert_eq!(route(3, &[VOICE, 3, 32]), None);
        assert_eq!(route(3, &[0x80, 3, 4]), None);
        assert_eq!(route(3, &[VOICE, 3]), None);
        assert_eq!(MAX_PAYLOAD_LEN, 16_352);
    }
}
