//! The relay's decisions (SPEC §5), shared by every relay implementation.
//!
//! A relay keeps, per room, at most one connection in each of the two seats.
//! Everything it does with a single message is decided here; seating,
//! eviction and forwarding are left to the runtime, which knows how its
//! connections are stored.

use crate::group;
use crate::room::RoomId;
use crate::wire::{self, Role, close};

/// A WebSocket message as received by the relay.
#[derive(Debug, Clone, Copy)]
pub enum Incoming<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Reply with the text message `pong`.
    Pong,
    /// The connection proved access to the room: seat it in `Role`, closing
    /// any connection already there with [`close::REPLACED`].
    Join(Role),
    /// Forward the message unchanged to the connection in the other seat,
    /// or drop it if the seat is empty.
    Forward,
    /// Close the connection with this code.
    Close(u16),
}

/// A connection's position in the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conn {
    /// Sent `HELLO` with this nonce; waiting for `JOIN`.
    Pending([u8; wire::NONCE_LEN]),
    Seated(Role),
}

impl Conn {
    /// A new connection and the `HELLO` frame to send it first.
    pub fn new() -> (Conn, [u8; wire::HELLO_LEN]) {
        let nonce = crate::random();
        (Conn::Pending(nonce), wire::hello(&nonce))
    }

    /// Decides what to do with a message from this connection.
    pub fn receive(&self, room: &RoomId, message: Incoming<'_>) -> Action {
        match (self, message) {
            (_, Incoming::Text(text)) if text == wire::PING => Action::Pong,
            (_, Incoming::Binary(frame)) if frame.len() > wire::MAX_FRAME_LEN => Action::Close(close::TOO_BIG),
            (Conn::Pending(nonce), Incoming::Binary(frame)) => match room.verify_join(nonce, frame) {
                Ok(role) => Action::Join(role),
                Err(code) => Action::Close(code),
            },
            (Conn::Seated(_), Incoming::Binary(frame)) if wire::is_peer_frame(frame) => Action::Forward,
            _ => Action::Close(close::PROTOCOL_ERROR),
        }
    }
}

/// Extracts the room from a request path ending in `/v1/<room id>`. Any
/// prefix is allowed, so a relay can be mounted below a path.
pub fn room_from_path(path: &str) -> Option<RoomId> {
    let path = path.split('?').next()?;
    let (rest, id) = path.rsplit_once('/')?;
    rest.ends_with("/v1").then(|| id.parse().ok())?
}

/// Extracts the group from a request path ending in `/g1/<group id>`.
pub fn group_from_path(path: &str) -> Option<RoomId> {
    let path = path.split('?').next()?;
    let (rest, id) = path.rsplit_once('/')?;
    rest.ends_with("/g1").then(|| id.parse().ok())?
}

/// What a relay does with a message from a group connection (GROUP.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupAction {
    /// Reply with the text message `pong`.
    Pong,
    /// The connection proved access to the group: seat it, in the slot of the
    /// connection with this join token if there is one (closing that one
    /// with [`close::REPLACED`]), or else in the lowest free slot.
    Join([u8; group::wire::TOKEN_LEN]),
    /// Forward the frame unchanged to slot `to` (if occupied), or to every
    /// other connection when `None`, queued by class.
    Forward { to: Option<u8>, class: group::Class },
    /// Close the connection with this code.
    Close(u16),
}

/// A group connection's position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupConn {
    /// Sent `HELLO` with this nonce; waiting for `JOIN`.
    Pending([u8; wire::NONCE_LEN]),
    Seated(u8),
}

impl GroupConn {
    /// A new connection and the `HELLO` frame to send it first.
    pub fn new() -> (GroupConn, [u8; wire::HELLO_LEN]) {
        let nonce = crate::random();
        (GroupConn::Pending(nonce), wire::hello(&nonce))
    }

    /// Decides what to do with a message from this connection.
    pub fn receive(&self, group: &RoomId, message: Incoming<'_>) -> GroupAction {
        match (self, message) {
            (_, Incoming::Text(text)) if text == wire::PING => GroupAction::Pong,
            (_, Incoming::Binary(frame)) if frame.len() > group::wire::MAX_FRAME_LEN => {
                GroupAction::Close(close::TOO_BIG)
            }
            (GroupConn::Pending(nonce), Incoming::Binary(frame)) => match group.verify_group_join(nonce, frame) {
                Ok(token) => GroupAction::Join(token),
                Err(code) => GroupAction::Close(code),
            },
            (GroupConn::Seated(slot), Incoming::Binary(frame)) => match group::wire::route(*slot, frame) {
                Some((to, class)) => GroupAction::Forward { to, class },
                None => GroupAction::Close(close::PROTOCOL_ERROR),
            },
            _ => GroupAction::Close(close::PROTOCOL_ERROR),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Room;

    #[test]
    fn decides_like_the_spec() {
        let room = Room::create("wss://r.example").unwrap();
        let id = room.id();
        let (pending, hello) = Conn::new();
        assert_eq!(hello[0], wire::HELLO);
        let Conn::Pending(nonce) = pending else { unreachable!() };

        let join = room.join_frame(crate::room::Rendezvous::Invite, &nonce);
        assert_eq!(pending.receive(&id, Incoming::Binary(&join)), Action::Join(Role::Inviter));
        assert_eq!(pending.receive(&id, Incoming::Text("ping")), Action::Pong);
        assert_eq!(pending.receive(&id, Incoming::Binary(&[wire::MESSAGE, 1])), Action::Close(close::PROTOCOL_ERROR));
        assert_eq!(
            pending.receive(&id, Incoming::Binary(&room.join_frame(crate::room::Rendezvous::Invite, &[0; 32]))),
            Action::Close(close::UNAUTHORIZED)
        );

        let seated = Conn::Seated(Role::Joiner);
        assert_eq!(seated.receive(&id, Incoming::Binary(&[wire::MESSAGE, 1, 2])), Action::Forward);
        assert_eq!(seated.receive(&id, Incoming::Binary(&[0x7f])), Action::Forward);
        assert_eq!(seated.receive(&id, Incoming::Binary(&join)), Action::Close(close::PROTOCOL_ERROR));
        assert_eq!(seated.receive(&id, Incoming::Binary(&[])), Action::Close(close::PROTOCOL_ERROR));
        assert_eq!(seated.receive(&id, Incoming::Text("hi")), Action::Close(close::PROTOCOL_ERROR));
        let big = vec![wire::MESSAGE; wire::MAX_FRAME_LEN + 1];
        assert_eq!(seated.receive(&id, Incoming::Binary(&big)), Action::Close(close::TOO_BIG));
    }

    #[test]
    fn parses_paths() {
        let id = Room::create("wss://r.example").unwrap().id();
        assert_eq!(room_from_path(&format!("/v1/{id}")), Some(id));
        assert_eq!(room_from_path(&format!("/relay/v1/{id}?x=1")), Some(id));
        assert_eq!(room_from_path(&format!("/v2/{id}")), None);
        assert_eq!(room_from_path(&format!("/v1/{id}/")), None);
        assert_eq!(room_from_path("/v1/abc"), None);
        assert_eq!(room_from_path("/"), None);
    }

    #[test]
    fn group_decisions() {
        let group = crate::group::Group::create("wss://r.example").unwrap();
        let id = group.id();
        let (pending, hello) = GroupConn::new();
        let GroupConn::Pending(nonce) = pending else { unreachable!() };
        assert_eq!(hello[0], wire::HELLO);
        let join = group.join_frame(&nonce, &[3; 16]);
        assert_eq!(pending.receive(&id, Incoming::Binary(&join)), GroupAction::Join([3; 16]));
        assert_eq!(pending.receive(&id, Incoming::Text("ping")), GroupAction::Pong);
        assert_eq!(pending.receive(&id, Incoming::Binary(&[0x03, 0, 0xFF])), GroupAction::Close(close::PROTOCOL_ERROR));
        assert_eq!(
            pending.receive(&id, Incoming::Binary(&group.join_frame(&[0; 32], &[3; 16]))),
            GroupAction::Close(close::UNAUTHORIZED)
        );
        // A two-peer JOIN is not a group JOIN.
        let room = crate::Room::create("wss://r.example").unwrap();
        let pair_join = room.join_frame(crate::room::Rendezvous::Invite, &nonce);
        assert_eq!(pending.receive(&id, Incoming::Binary(&pair_join)), GroupAction::Close(close::PROTOCOL_ERROR));

        let seated = GroupConn::Seated(2);
        assert_eq!(
            seated.receive(&id, Incoming::Binary(&[0x21, 2, 0xFF, 1])),
            GroupAction::Forward { to: None, class: group::Class::Voice }
        );
        assert_eq!(
            seated.receive(&id, Incoming::Binary(&[0x03, 2, 3])),
            GroupAction::Forward { to: Some(3), class: group::Class::Command }
        );
        assert_eq!(seated.receive(&id, Incoming::Binary(&[0x03, 1, 3])), GroupAction::Close(close::PROTOCOL_ERROR));
        // Slots 0–3 only (GROUP.md §3.2).
        assert_eq!(seated.receive(&id, Incoming::Binary(&[0x03, 2, 4])), GroupAction::Close(close::PROTOCOL_ERROR));
        assert_eq!(seated.receive(&id, Incoming::Binary(&join)), GroupAction::Close(close::PROTOCOL_ERROR));
        assert_eq!(seated.receive(&id, Incoming::Text("hi")), GroupAction::Close(close::PROTOCOL_ERROR));
        let big = vec![0x03; group::wire::MAX_FRAME_LEN + 1];
        assert_eq!(seated.receive(&id, Incoming::Binary(&big)), GroupAction::Close(close::TOO_BIG));
    }

    #[test]
    fn group_paths() {
        let id = crate::group::Group::create("wss://r.example").unwrap().id();
        assert_eq!(group_from_path(&format!("/g1/{id}")), Some(id));
        assert_eq!(group_from_path(&format!("/relay/g1/{id}?x=1")), Some(id));
        assert_eq!(group_from_path(&format!("/v1/{id}")), None);
        assert_eq!(room_from_path(&format!("/g1/{id}")), None);
    }
}
