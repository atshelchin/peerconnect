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
    /// other connection when `None`, queued by class. Video sent to everyone
    /// goes to each connection as its [`VideoRouter`] says.
    Forward { to: Option<u8>, class: group::Class },
    /// The connection wants `layer` of `slot`'s video (`None`: none); tell
    /// the [`VideoRouter`].
    Subscribe { slot: u8, layer: Option<u8> },
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
            (GroupConn::Seated(slot), Incoming::Binary(&[group::wire::SUBSCRIBE, other, layer])) => {
                let layer = (layer != group::wire::NO_VIDEO).then_some(layer);
                let valid = (other as usize) < group::wire::MAX_SLOTS
                    && other != *slot
                    && layer.is_none_or(|l| l < group::wire::VIDEO_LAYERS);
                if valid {
                    GroupAction::Subscribe { slot: other, layer }
                } else {
                    GroupAction::Close(close::PROTOCOL_ERROR)
                }
            }
            (GroupConn::Seated(slot), Incoming::Binary(frame)) => match group::wire::route(*slot, frame) {
                Some((to, class)) => GroupAction::Forward { to, class },
                None => GroupAction::Close(close::PROTOCOL_ERROR),
            },
            _ => GroupAction::Close(close::PROTOCOL_ERROR),
        }
    }
}

/// Which layer of each member's video a relay forwards each other member
/// (GROUP.md §3.3): the layer it subscribed to (layer 0 until it says
/// otherwise), or, if the sender does not send that one, the nearest below
/// it that the sender does, or else the lowest; never above what the
/// receiver's link takes now. A move up takes effect at the new layer's
/// next keyframe, the old layer going on until then, so that the picture
/// never stops; a move down for a link that cannot keep up stops the larger
/// layer at once. Each sender is told which of its layers are wanted
/// (DEMAND), each receiver how far its link limits it (LIMIT). Free of I/O,
/// for relays to share.
#[derive(Debug, Clone)]
pub struct VideoRouter {
    /// By receiver, then sender: the layer subscribed to (`None`: no video).
    wanted: [[Option<u8>; SLOTS]; SLOTS],
    /// By receiver, then sender: the layer being forwarded.
    forwarding: [[Option<u8>; SLOTS]; SLOTS],
    /// By sender, then layer: when it was last seen.
    seen: [[Option<u64>; LAYERS]; SLOTS],
    /// By sender: the DEMAND it was last sent, if seated.
    told: [Option<Option<u8>>; SLOTS],
    /// By receiver: what its link takes.
    links: [Link; SLOTS],
}

/// How many layers a receiver's link takes now, and when to try one more.
#[derive(Debug, Clone, Copy)]
struct Link {
    /// Layers 0 to `layers - 1` (none: audio only); [`LAYERS`] when unlimited.
    layers: u8,
    /// The LIMIT the receiver was last told.
    told: u8,
    changed_at: u64,
    /// The last sign that it could not keep up, and since when that lasts.
    congested_at: Option<u64>,
    congested_since: Option<u64>,
    /// When it was last given one more layer, while that is on trial.
    raised_at: Option<u64>,
    /// How long it must keep up before it is given one more.
    wait: u64,
}

impl Default for Link {
    fn default() -> Self {
        Link {
            layers: LAYERS as u8,
            told: LAYERS as u8,
            changed_at: 0,
            congested_at: None,
            congested_since: None,
            raised_at: None,
            wait: PROBE_WAIT,
        }
    }
}

const SLOTS: usize = group::wire::MAX_SLOTS;
const LAYERS: usize = group::wire::VIDEO_LAYERS as usize;
/// A layer not seen for this long (ms) is taken to be no longer sent.
const LAYER_FRESH: u64 = 1000;
/// A link whose video stays queued this long (ms) does not keep up.
const CONGESTED_FOR: u64 = 300;
/// After a change, a link's signals are ignored this long (ms): the move
/// takes a keyframe, and the queue a moment to drain.
const LINK_SETTLE: u64 = 2000;
/// A link that keeps up this long (ms) is given one more layer to try; the
/// wait doubles, up to [`PROBE_MAX`], each time a try fails within
/// [`PROBE_TRIAL`].
const PROBE_WAIT: u64 = 8000;
const PROBE_MAX: u64 = 60_000;
const PROBE_TRIAL: u64 = 5000;

impl Default for VideoRouter {
    fn default() -> Self {
        VideoRouter {
            wanted: [[Some(0); SLOTS]; SLOTS],
            forwarding: [[None; SLOTS]; SLOTS],
            seen: [[None; LAYERS]; SLOTS],
            told: [None; SLOTS],
            links: [Link::default(); SLOTS],
        }
    }
}

impl VideoRouter {
    /// A new connection in `slot`: it wants layer 0 of everyone, nobody has
    /// subscribed to its video yet, and its link takes everything.
    pub fn seat(&mut self, slot: u8) {
        self.reset(slot);
        self.told[slot as usize] = Some(None);
    }

    /// The connection in `slot` ended.
    pub fn leave(&mut self, slot: u8) {
        self.reset(slot);
        self.told[slot as usize] = None;
    }

    fn reset(&mut self, slot: u8) {
        let s = slot as usize;
        for other in 0..SLOTS {
            self.wanted[s][other] = Some(0);
            self.wanted[other][s] = Some(0);
            self.forwarding[s][other] = None;
            self.forwarding[other][s] = None;
        }
        self.seen[s] = [None; LAYERS];
        self.links[s] = Link::default();
    }

    /// `receiver` subscribed to `layer` of `sender`'s video.
    pub fn subscribe(&mut self, receiver: u8, sender: u8, layer: Option<u8>) {
        if receiver != sender && (sender as usize) < SLOTS && layer.is_none_or(|l| (l as usize) < LAYERS) {
            self.wanted[receiver as usize][sender as usize] = layer;
            if layer.is_none() {
                self.forwarding[receiver as usize][sender as usize] = None;
            }
        }
    }

    /// What the relay sees of `receiver`'s link now: whether its video is
    /// waiting longer than it should (`congested`), and whether video for it
    /// was dropped since it last said (`dropped`). Call it before forwarding
    /// it a video frame. A link that does not keep up is moved below the
    /// largest layer it gets (to none of them, from the smallest); one that
    /// keeps up long enough is given one more layer to try.
    pub fn link(&mut self, now: u64, receiver: u8, congested: bool, dropped: bool) {
        let r = receiver as usize;
        let top = self.forwarding[r].iter().flatten().max().copied();
        let l = &mut self.links[r];
        if !congested && !dropped {
            l.congested_since = None;
            if l.raised_at.is_some_and(|at| now.saturating_sub(at) >= PROBE_TRIAL) {
                l.raised_at = None;
                l.wait = PROBE_WAIT;
            }
            let calm_since = l.congested_at.map_or(l.changed_at, |at| at.max(l.changed_at));
            if (l.layers as usize) < LAYERS && now.saturating_sub(calm_since) >= l.wait {
                l.layers += 1;
                l.changed_at = now;
                l.raised_at = Some(now);
            }
            return;
        }
        l.congested_at = Some(now);
        if now.saturating_sub(l.changed_at) < LINK_SETTLE && l.changed_at != 0 {
            return;
        }
        let since = *l.congested_since.get_or_insert(now);
        if dropped || now.saturating_sub(since) >= CONGESTED_FOR {
            l.layers = top.map_or(l.layers.saturating_sub(1), |top| top.min(l.layers.saturating_sub(1)));
            if l.raised_at.take().is_some_and(|at| now.saturating_sub(at) < PROBE_TRIAL) {
                l.wait = (l.wait * 2).min(PROBE_MAX);
            }
            l.changed_at = now;
            l.congested_since = None;
        }
    }

    /// Whether to forward a video frame of type `kind`, sent by `sender` to
    /// everyone, to `receiver`. Call it for each receiver in turn.
    pub fn forward(&mut self, now: u64, sender: u8, receiver: u8, kind: u8) -> bool {
        let Some((layer, keyframe)) = group::wire::video_layer(kind) else { return true };
        let (s, r) = (sender as usize, receiver as usize);
        self.seen[s][layer as usize] = Some(now);
        let (Some(want), Some(limit)) = (self.wanted[r][s], self.links[r].layers.checked_sub(1)) else {
            self.forwarding[r][s] = None;
            return false;
        };
        let sent = |l: u8| self.seen[s][l as usize].is_some_and(|at| now.saturating_sub(at) <= LAYER_FRESH);
        let target = (0..=want.min(limit)).rev().find(|&l| sent(l)).or_else(|| (0..=limit).find(|&l| sent(l)));
        match self.forwarding[r][s] {
            Some(current) if current == layer && layer <= limit => true,
            // Too large for the link now: stopped at once.
            Some(current) if current == layer => {
                self.forwarding[r][s] = None;
                false
            }
            // The new layer from its keyframe on, or at once if the old one
            // stopped (or was stopped: the receiver waits for a keyframe).
            Some(current) if target == Some(layer) && (keyframe || !sent(current) || current > limit) => {
                self.forwarding[r][s] = Some(layer);
                true
            }
            Some(_) => false,
            None if target == Some(layer) => {
                self.forwarding[r][s] = Some(layer);
                true
            }
            None => false,
        }
    }

    /// The DEMAND frames due, `(slot, layers)`: for every seated sender whose
    /// wanted layers changed since it was last told. Layers being forwarded
    /// count as wanted until the move away from them is done; a layer above
    /// what a receiver's link takes is not wanted by it.
    pub fn demands(&mut self) -> Vec<(u8, u8)> {
        let mut due = Vec::new();
        for s in 0..SLOTS {
            let Some(told) = self.told[s] else { continue };
            let mut layers = 0u8;
            for r in (0..SLOTS).filter(|&r| r != s && self.told[r].is_some()) {
                let Some(limit) = self.links[r].layers.checked_sub(1) else { continue };
                let forwarding = self.forwarding[r][s].filter(|&l| l <= limit);
                for layer in [self.wanted[r][s].map(|l| l.min(limit)), forwarding].into_iter().flatten() {
                    layers |= 1 << layer;
                }
            }
            if told != Some(layers) {
                self.told[s] = Some(Some(layers));
                due.push((s as u8, layers));
            }
        }
        due
    }

    /// The LIMIT frames due, `(slot, layer)`: the largest layer each seated
    /// receiver's link takes now (`None`: no video at all), for those whose
    /// limit changed since they were last told.
    pub fn limits(&mut self) -> Vec<(u8, Option<u8>)> {
        let mut due = Vec::new();
        for (r, link) in self.links.iter_mut().enumerate() {
            if self.told[r].is_some() && link.told != link.layers {
                link.told = link.layers;
                due.push((r as u8, link.layers.checked_sub(1)));
            }
        }
        due
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
        let group = crate::group::Group::create("https://r.example").unwrap();
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
        let id = crate::group::Group::create("https://r.example").unwrap().id();
        assert_eq!(group_from_path(&format!("/g1/{id}")), Some(id));
        assert_eq!(group_from_path(&format!("/relay/g1/{id}?x=1")), Some(id));
        assert_eq!(group_from_path(&format!("/v1/{id}")), None);
        assert_eq!(room_from_path(&format!("/g1/{id}")), None);
    }

    #[test]
    fn subscriptions_are_read_and_checked() {
        let id = crate::group::Group::create("https://r.example").unwrap().id();
        let seated = GroupConn::Seated(1);
        let sub = |f: &[u8]| seated.receive(&id, Incoming::Binary(f));
        assert_eq!(sub(&[0x85, 2, 1]), GroupAction::Subscribe { slot: 2, layer: Some(1) });
        assert_eq!(sub(&[0x85, 0, 0xFF]), GroupAction::Subscribe { slot: 0, layer: None });
        for bad in [&[0x85, 1, 0][..], &[0x85, 4, 0], &[0x85, 2, 4], &[0x85, 2], &[0x86, 3]] {
            assert_eq!(sub(bad), GroupAction::Close(close::PROTOCOL_ERROR), "{bad:?}");
        }
        assert_eq!(
            GroupConn::Pending([0; 32]).receive(&id, Incoming::Binary(&[0x85, 2, 1])),
            GroupAction::Close(close::PROTOCOL_ERROR)
        );
    }

    /// Layer `l`, a keyframe or not.
    fn v(l: u8, key: bool) -> u8 {
        group::wire::video_type(l, key)
    }

    #[test]
    fn each_receiver_gets_the_layer_it_wants_switching_at_a_keyframe() {
        let mut r = VideoRouter::default();
        for slot in 0..3 {
            r.seat(slot);
        }
        // Slot 0 sends layers 1 and 2; nobody has subscribed: the lowest it sends.
        assert!(r.forward(0, 0, 2, v(1, true)));
        assert!(!r.forward(1, 0, 2, v(2, true)), "layer 2 is not the lowest sent");
        assert!(r.forward(1, 0, 1, v(1, true)));
        assert!(!r.forward(2, 0, 1, v(2, false)));
        assert!(r.forward(3, 0, 1, v(1, false)));
        // Slot 1 asks for layer 2: layer 1 goes on until layer 2's keyframe.
        r.subscribe(1, 0, Some(2));
        assert!(r.forward(10, 0, 1, v(1, false)));
        assert!(!r.forward(11, 0, 1, v(2, false)));
        assert!(r.forward(12, 0, 1, v(2, true)), "switched at the keyframe");
        assert!(!r.forward(13, 0, 1, v(1, false)));
        assert!(r.forward(14, 0, 1, v(2, false)));
        // Slot 2 still gets layer 1, whose first frame it saw.
        assert!(r.forward(15, 0, 2, v(1, false)));
        assert!(!r.forward(16, 0, 2, v(2, false)));
        // No video at all.
        r.subscribe(2, 0, None);
        assert!(!r.forward(17, 0, 2, v(1, false)));
        // Frames that are not video are not the router's business.
        assert!(r.forward(18, 0, 2, group::wire::VOICE));
    }

    #[test]
    fn a_layer_the_sender_stops_is_replaced_at_once() {
        let mut r = VideoRouter::default();
        r.seat(0);
        r.seat(1);
        r.subscribe(1, 0, Some(2));
        assert!(r.forward(0, 0, 1, v(2, true)));
        assert!(!r.forward(10, 0, 1, v(1, false)), "layer 2 is still sent");
        // Layer 2 stopped (the sender's uplink, say): layer 1, without waiting for a keyframe.
        assert!(r.forward(10 + LAYER_FRESH + 1, 0, 1, v(1, false)));
        // Asking for a layer above anything sent gets the best below it.
        r.subscribe(1, 0, Some(3));
        assert!(r.forward(2000, 0, 1, v(1, false)));
    }

    #[test]
    fn senders_are_told_which_layers_are_wanted() {
        let mut r = VideoRouter::default();
        r.seat(0);
        assert_eq!(r.demands(), [(0, 0)], "alone: nothing");
        assert_eq!(r.demands(), [], "only changes");
        r.seat(1);
        assert_eq!(r.demands(), [(0, 0b1), (1, 0b1)], "layer 0 by default");
        r.subscribe(1, 0, Some(2));
        assert_eq!(r.demands(), [(0, 0b100)]);
        // Forwarding layer 1 (the only one sent), while layer 2 is wanted: both.
        assert!(r.forward(0, 0, 1, v(1, true)));
        assert_eq!(r.demands(), [(0, 0b110)]);
        assert!(!r.forward(1, 0, 1, v(2, false)));
        assert!(r.forward(2, 0, 1, v(2, true)));
        assert_eq!(r.demands(), [(0, 0b100)], "layer 1 no longer, once switched");
        r.subscribe(1, 0, None);
        assert_eq!(r.demands(), [(0, 0)], "no video wanted, none forwarded");
        r.subscribe(1, 0, Some(1));
        assert_eq!(r.demands(), [(0, 0b10)]);
        r.leave(1);
        assert_eq!(r.demands(), [(0, 0)], "the subscriber left");
    }

    #[test]
    fn a_link_that_does_not_keep_up_gets_smaller_layers_then_none() {
        let mut r = VideoRouter::default();
        for slot in 0..2 {
            r.seat(slot);
        }
        r.subscribe(1, 0, Some(2));
        assert!(r.forward(0, 0, 1, v(1, true)));
        assert!(r.forward(1, 0, 1, v(2, true)));
        assert_eq!(r.limits(), []);
        r.demands();
        // Video waits, but not for long enough to count.
        r.link(100, 1, true, false);
        r.link(300, 1, false, false);
        r.link(500, 1, true, false);
        assert_eq!(r.limits(), []);
        // It goes on waiting: below layer 2, at once.
        r.link(500 + CONGESTED_FOR, 1, true, false);
        assert_eq!(r.limits(), [(1, Some(1))]);
        assert!(!r.forward(900, 0, 1, v(2, false)), "the larger layer stops at once");
        assert!(r.forward(901, 0, 1, v(1, false)), "the smaller one goes on");
        assert_eq!(r.demands(), [(0, 0b10)], "layer 2 is no longer wanted");
        // Signals are ignored while the move settles; then a drop moves it at once.
        r.link(1000, 1, true, true);
        assert_eq!(r.limits(), []);
        r.link(900 + LINK_SETTLE, 1, true, true);
        assert_eq!(r.limits(), [(1, Some(0))], "below layer 1");
        assert!(!r.forward(3000, 0, 1, v(1, true)), "a sends nothing that small");
        assert_eq!(r.demands(), [(0, 0b1)], "so a is asked for layer 0");
        r.link(3000 + LINK_SETTLE, 1, true, true);
        assert_eq!(r.limits(), [(1, None)], "and below that: audio only");
        assert_eq!(r.demands(), [(0, 0)], "nothing to encode for it");
    }

    #[test]
    fn a_link_that_keeps_up_is_given_more_and_tries_less_often_after_failing() {
        let mut r = VideoRouter::default();
        r.seat(0);
        r.seat(1);
        r.subscribe(1, 0, Some(2));
        assert!(r.forward(0, 0, 1, v(2, true)));
        r.link(10_000, 1, true, true);
        assert_eq!(r.limits(), [(1, Some(1))]);
        // Calm for PROBE_WAIT: one more layer to try.
        r.link(10_000 + PROBE_WAIT - 1, 1, false, false);
        assert_eq!(r.limits(), []);
        r.link(10_000 + PROBE_WAIT, 1, false, false);
        assert_eq!(r.limits(), [(1, Some(2))]);
        // The try fails at once: back down, and the next try waits twice as long.
        let failed = 10_000 + PROBE_WAIT + LINK_SETTLE;
        assert!(r.forward(failed - 1, 0, 1, v(2, true)));
        r.link(failed, 1, true, true);
        assert_eq!(r.limits(), [(1, Some(1))]);
        r.link(failed + PROBE_WAIT, 1, false, false);
        assert_eq!(r.limits(), [], "not yet");
        r.link(failed + 2 * PROBE_WAIT, 1, false, false);
        assert_eq!(r.limits(), [(1, Some(2))]);
        // A try that holds brings the wait back down.
        r.link(failed + 2 * PROBE_WAIT + PROBE_TRIAL, 1, false, false);
        assert_eq!(r.links[1].wait, PROBE_WAIT);
    }
}
