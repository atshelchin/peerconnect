//! PeerConnect relay on Cloudflare Workers (SPEC §5).
//!
//! The Worker routes each room to its own Durable Object, which holds the
//! room's WebSockets through the Hibernation API: an idle room costs nothing,
//! `ping`s are answered by the runtime without waking the object, and each
//! socket's seat lives in its attachment, so the object can be evicted from
//! memory at any time and rebuild its view of the room on the next event.
//! Nothing is ever written to storage.
//!
//! Groups (`/g1/<id>`, GROUP.md) get a Durable Object of their own class,
//! [`Group`], which works the same way with up to 32 slots. The runtime
//! sends each frame as it is forwarded and does not report what is still
//! buffered, so this relay cannot drop late voice or put commands first:
//! for voice at scale, prefer the Linux relay.

use std::cell::{Cell, RefCell, RefMut};

use peerconnect::RoomId;
use peerconnect::group::wire as gwire;
use peerconnect::relay::{self, Action, Conn, GroupAction, GroupConn, Incoming};
use peerconnect::wire::{self, Role, close};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use worker::*;

/// Connections that have not joined yet, per room, beyond which the oldest
/// is dropped.
const MAX_PENDING: usize = 64;
/// Connections that have not joined yet, per room and client address; more
/// are refused, so that knowing a room ID is not enough to crowd out its peers.
const MAX_PENDING_PER_ADDRESS: usize = 4;
/// How often forwarding checks whether the partner has stopped pinging (ms).
const STALE_CHECK_INTERVAL: u64 = 5_000;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    if path == "/" || path == "/healthz" {
        return Response::ok("peerconnect relay\n");
    }
    let (binding, name) = match (relay::room_from_path(&path), relay::group_from_path(&path)) {
        (Some(room), _) => ("ROOMS", room.to_string()),
        (_, Some(group)) => ("GROUPS", group.to_string()),
        _ => return Response::error("not found\n", 404),
    };
    let upgrade = req.headers().get("Upgrade")?;
    if !upgrade.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
        return Response::error("expected a WebSocket upgrade\n", 426);
    }
    let objects = env.durable_object(binding)?;
    let stub = objects.id_from_name(&name)?.get_stub()?;
    stub.fetch_with_request(req).await
}

/// What a socket's attachment records: which room, and where the socket stands.
#[derive(Serialize, Deserialize)]
struct Attachment {
    room: String,
    seat: Seat,
    /// When it connected, or joined (milliseconds since the epoch).
    since: u64,
    /// The client's address, as Cloudflare reports it.
    address: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
enum Seat {
    /// Sent `HELLO` with this nonce.
    Pending([u8; wire::NONCE_LEN]),
    Seated(u8),
    /// Closed by the relay; ignore whatever it still says.
    Gone,
}

#[durable_object]
pub struct Room {
    state: State,
    hibernation: Hibernation,
    room: Cell<Option<RoomId>>,
    /// The seated sockets, rebuilt from attachments after hibernation.
    seats: RefCell<Option<[Option<WebSocket>; 2]>>,
    /// When forwarding last checked the partner's liveness.
    checked: Cell<u64>,
    /// When each seat last sent anything (in memory only: a room that
    /// hibernates has nothing in flight anyway).
    seen: [Cell<u64>; 2],
}

impl DurableObject for Room {
    fn new(state: State, _env: Env) -> Self {
        let raw = state._inner();
        let hibernation = raw.unchecked_ref::<Hibernation>().clone();
        let state = State::from(raw);
        if let Ok(pair) = WebSocketRequestResponsePair::new(wire::PING, wire::PONG) {
            state.set_websocket_auto_response(&pair);
        }
        Room {
            state,
            hibernation,
            room: Cell::new(None),
            seats: RefCell::new(None),
            checked: Cell::new(0),
            seen: Default::default(),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let Some(room) = relay::room_from_path(&req.path()) else {
            return Response::error("not found\n", 404);
        };
        self.room.set(Some(room));
        let now = Date::now().as_millis();
        let address = req.headers().get("CF-Connecting-IP")?.unwrap_or_default();
        let (older, from_address) = self.sweep(now, &address);
        if from_address >= MAX_PENDING_PER_ADDRESS {
            return Response::error("too many connections\n", 429);
        }

        let pair = WebSocketPair::new()?;
        self.state.accept_web_socket(&pair.server);
        let (conn, hello) = Conn::new();
        let Conn::Pending(nonce) = conn else { unreachable!() };
        let attachment = Attachment { room: room.to_string(), seat: Seat::Pending(nonce), since: now, address };
        pair.server.serialize_attachment(&attachment)?;
        pair.server.send_with_bytes(hello)?;
        // Wake up to enforce the join timeout; a room whose sockets have all
        // joined has no alarm and stays hibernated.
        let deadline = older.unwrap_or(u64::MAX).min(now + join_timeout());
        self.state.storage().set_alarm((deadline - now) as i64 + 100).await?;
        Response::from_websocket(pair.client)
    }

    async fn alarm(&self) -> Result<Response> {
        let now = Date::now().as_millis();
        if let (Some(deadline), _) = self.sweep(now, "") {
            self.state.storage().set_alarm(deadline.saturating_sub(now) as i64 + 100).await?;
        }
        Response::empty()
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        // Seated sockets are recognised from memory; only a socket that has
        // not joined yet needs its attachment read.
        let now = Date::now().as_millis();
        let (conn, pending) = match self.seat_of(&ws) {
            Some(role) => {
                self.seen[role.index()].set(now);
                (Conn::Seated(role), None)
            }
            None => match attachment(&ws) {
                Some(a @ Attachment { seat: Seat::Pending(nonce), .. }) => (Conn::Pending(nonce), Some(a)),
                _ => return Ok(()), // closed by the relay
            },
        };
        let Some(room) = self.room.get().or_else(|| pending.as_ref().and_then(|a| a.room.parse().ok())) else {
            return Ok(());
        };
        self.room.set(Some(room));
        let incoming = match &message {
            WebSocketIncomingMessage::String(text) => Incoming::Text(text),
            WebSocketIncomingMessage::Binary(frame) => Incoming::Binary(frame),
        };
        match conn.receive(&room, incoming) {
            Action::Pong => {
                let _ = ws.send_with_str(wire::PONG);
            }
            Action::Join(role) => self.join(&ws, pending.expect("only pending sockets join"), role, now)?,
            Action::Forward => {
                let (Conn::Seated(role), WebSocketIncomingMessage::Binary(frame)) = (conn, &message) else {
                    unreachable!()
                };
                let partner = self.seats()[role.other().index()].clone();
                if let Some(partner) = partner {
                    // A partner that stopped pinging is gone; finding out now
                    // tells the sender, instead of dropping into the void.
                    // (Checked every few seconds, not on every frame.)
                    if now.saturating_sub(self.checked.get()) > STALE_CHECK_INTERVAL {
                        self.checked.set(now);
                        if self.stale(&partner, role.other(), now) {
                            self.drop_socket(&partner, close::TIMEOUT);
                            return Ok(());
                        }
                    }
                    let _ = partner.send_with_bytes(frame);
                }
            }
            Action::Close(code) => self.drop_socket(&ws, code),
        }
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, _code: usize, _reason: String, _clean: bool) -> Result<()> {
        self.vacate(&ws);
        mark_gone(&ws);
        Ok(())
    }

    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        self.vacate(&ws);
        mark_gone(&ws);
        Ok(())
    }
}

impl Room {
    fn seat_of(&self, ws: &WebSocket) -> Option<Role> {
        let seats = self.seats();
        seats.iter().position(|s| s.as_ref() == Some(ws)).and_then(|i| Role::from_byte(i as u8))
    }

    fn seats(&self) -> RefMut<'_, [Option<WebSocket>; 2]> {
        let mut cache = self.seats.borrow_mut();
        if cache.is_none() {
            let mut seats = [None, None];
            for ws in self.state.get_websockets() {
                if let Some(Attachment { seat: Seat::Seated(role), room, .. }) = attachment(&ws) {
                    seats[role_of(role).index()] = Some(ws);
                    if self.room.get().is_none() {
                        self.room.set(room.parse().ok());
                    }
                }
            }
            *cache = Some(seats);
        }
        RefMut::map(cache, |cache| cache.as_mut().unwrap())
    }

    /// Seats `ws`, replacing whoever held the seat, and tells both sides who
    /// is there. The object handles one event at a time, so nothing reaches
    /// either socket in between.
    fn join(&self, ws: &WebSocket, mut attachment: Attachment, role: Role, now: u64) -> Result<()> {
        let (old, partner) = {
            // Load the seats before this socket's attachment says it is seated.
            let mut seats = self.seats();
            let old = seats[role.index()].replace(ws.clone());
            (old, seats[role.other().index()].clone())
        };
        attachment.seat = Seat::Seated(role as u8);
        attachment.since = now;
        ws.serialize_attachment(&attachment)?;
        self.seen[role.index()].set(now);
        if let Some(old) = old.filter(|old| old != ws) {
            retire(&old, close::REPLACED);
        }
        let _ = ws.send_with_bytes(wire::presence(partner.is_some()));
        if let Some(partner) = partner {
            let _ = partner.send_with_bytes(wire::presence(true));
        }
        Ok(())
    }

    /// The relay closes `ws` and frees its seat.
    fn drop_socket(&self, ws: &WebSocket, code: u16) {
        self.vacate(ws);
        retire(ws, code);
    }

    /// Frees the seat `ws` held, if it still held one, telling the partner.
    fn vacate(&self, ws: &WebSocket) {
        let partner = {
            let mut seats = self.seats();
            let Some(index) = seats.iter().position(|s| s.as_ref() == Some(ws)) else { return };
            seats[index] = None;
            seats[1 - index].clone()
        };
        if let Some(partner) = partner {
            let _ = partner.send_with_bytes(wire::presence(false));
        }
    }

    /// Enforces the join and idle timeouts and bounds pending connections.
    /// Runs when a connection arrives and on the join-timeout alarm. Returns
    /// when the oldest connection still pending must have joined, and how
    /// many pending connections come from `address`.
    fn sweep(&self, now: u64, address: &str) -> (Option<u64>, usize) {
        let mut pending = Vec::new();
        let mut from_address = 0;
        for ws in self.state.get_websockets() {
            match attachment(&ws) {
                Some(Attachment { seat: Seat::Pending(_), since, address: from, .. }) => {
                    if now.saturating_sub(since) >= join_timeout() {
                        retire(&ws, close::TIMEOUT);
                    } else {
                        from_address += usize::from(from == address);
                        pending.push((since, ws));
                    }
                }
                Some(Attachment { seat: Seat::Seated(role), .. }) if self.stale(&ws, role_of(role), now) => {
                    self.drop_socket(&ws, close::TIMEOUT);
                }
                _ => {}
            }
        }
        pending.sort_by_key(|(since, _)| *since);
        let excess = (pending.len() + 1).saturating_sub(MAX_PENDING);
        for (_, ws) in pending.drain(..excess) {
            retire(&ws, close::TIMEOUT);
        }
        (pending.first().map(|(since, _)| since + join_timeout()), from_address)
    }

    /// Whether a seated socket has sent nothing, not even a `ping`, for too long.
    fn stale(&self, ws: &WebSocket, role: Role, now: u64) -> bool {
        let pinged = self.hibernation.auto_response_timestamp(ws.as_ref()).map(|date| date.get_time() as u64);
        let joined = attachment(ws).map(|a| a.since);
        let last = [pinged, joined, Some(self.seen[role.index()].get())].into_iter().flatten().max().unwrap_or(now);
        now.saturating_sub(last) > wire::IDLE_TIMEOUT.as_millis() as u64
    }
}

/// Marks a socket as closed by the relay, then closes it.
fn retire(ws: &WebSocket, code: u16) {
    mark_gone(ws);
    let _ = ws.close(Some(code), Some(""));
}

/// Records that a socket is closed, so that it is never mistaken for a
/// seated one while the runtime still lists it.
fn mark_gone(ws: &WebSocket) {
    if let Some(mut attachment) = attachment(ws) {
        attachment.seat = Seat::Gone;
        let _ = ws.serialize_attachment(&attachment);
    }
}

fn attachment(ws: &WebSocket) -> Option<Attachment> {
    ws.deserialize_attachment().ok().flatten()
}

fn join_timeout() -> u64 {
    wire::JOIN_TIMEOUT.as_millis() as u64
}

fn role_of(byte: u8) -> Role {
    Role::from_byte(byte).unwrap_or(Role::Joiner)
}

#[wasm_bindgen]
extern "C" {
    /// The part of `DurableObjectState` that workers-rs does not bind yet.
    #[derive(Clone)]
    type Hibernation;

    /// When the runtime last answered this socket's `ping`, if ever.
    #[wasm_bindgen(method, js_name = getWebSocketAutoResponseTimestamp)]
    fn auto_response_timestamp(this: &Hibernation, ws: &web_sys::WebSocket) -> Option<js_sys::Date>;
}

/// Members per group.
const MAX_GROUP_MEMBERS: usize = 10;

/// A group socket's attachment.
#[derive(Serialize, Deserialize)]
struct GroupAttachment {
    group: String,
    seat: GroupSeat,
    since: u64,
    address: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
enum GroupSeat {
    Pending([u8; wire::NONCE_LEN]),
    Seated { slot: u8, token: [u8; gwire::TOKEN_LEN] },
    Gone,
}

type Slots = [Option<(WebSocket, [u8; gwire::TOKEN_LEN])>; gwire::MAX_SLOTS];

#[durable_object]
pub struct Group {
    state: State,
    hibernation: Hibernation,
    group: Cell<Option<RoomId>>,
    /// The seated sockets and their join tokens, rebuilt from attachments.
    slots: RefCell<Option<Slots>>,
    checked: Cell<u64>,
    seen: [Cell<u64>; gwire::MAX_SLOTS],
}

impl DurableObject for Group {
    fn new(state: State, _env: Env) -> Self {
        let raw = state._inner();
        let hibernation = raw.unchecked_ref::<Hibernation>().clone();
        let state = State::from(raw);
        if let Ok(pair) = WebSocketRequestResponsePair::new(wire::PING, wire::PONG) {
            state.set_websocket_auto_response(&pair);
        }
        Group {
            state,
            hibernation,
            group: Cell::new(None),
            slots: RefCell::new(None),
            checked: Cell::new(0),
            seen: std::array::from_fn(|_| Cell::new(0)),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let Some(group) = relay::group_from_path(&req.path()) else {
            return Response::error("not found\n", 404);
        };
        self.group.set(Some(group));
        let now = Date::now().as_millis();
        let address = req.headers().get("CF-Connecting-IP")?.unwrap_or_default();
        let (older, from_address) = self.sweep(now, &address);
        if from_address >= MAX_PENDING_PER_ADDRESS {
            return Response::error("too many connections\n", 429);
        }
        let pair = WebSocketPair::new()?;
        self.state.accept_web_socket(&pair.server);
        let (conn, hello) = GroupConn::new();
        let GroupConn::Pending(nonce) = conn else { unreachable!() };
        let attachment =
            GroupAttachment { group: group.to_string(), seat: GroupSeat::Pending(nonce), since: now, address };
        pair.server.serialize_attachment(&attachment)?;
        pair.server.send_with_bytes(hello)?;
        let deadline = older.unwrap_or(u64::MAX).min(now + join_timeout());
        self.state.storage().set_alarm((deadline - now) as i64 + 100).await?;
        Response::from_websocket(pair.client)
    }

    async fn alarm(&self) -> Result<Response> {
        let now = Date::now().as_millis();
        if let (Some(deadline), _) = self.sweep(now, "") {
            self.state.storage().set_alarm(deadline.saturating_sub(now) as i64 + 100).await?;
        }
        Response::empty()
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        let now = Date::now().as_millis();
        let (conn, pending) = match self.slot_of(&ws) {
            Some(slot) => {
                self.seen[slot as usize].set(now);
                (GroupConn::Seated(slot), None)
            }
            None => match group_attachment(&ws) {
                Some(a @ GroupAttachment { seat: GroupSeat::Pending(nonce), .. }) => {
                    (GroupConn::Pending(nonce), Some(a))
                }
                _ => return Ok(()), // closed by the relay
            },
        };
        let Some(group) = self.group.get().or_else(|| pending.as_ref().and_then(|a| a.group.parse().ok())) else {
            return Ok(());
        };
        self.group.set(Some(group));
        let incoming = match &message {
            WebSocketIncomingMessage::String(text) => Incoming::Text(text),
            WebSocketIncomingMessage::Binary(frame) => Incoming::Binary(frame),
        };
        match conn.receive(&group, incoming) {
            GroupAction::Pong => {
                let _ = ws.send_with_str(wire::PONG);
            }
            GroupAction::Join(token) => self.join(&ws, pending.expect("only pending sockets join"), token, now)?,
            GroupAction::Forward { to, .. } => {
                let (GroupConn::Seated(slot), WebSocketIncomingMessage::Binary(frame)) = (conn, &message) else {
                    unreachable!()
                };
                // Members that stopped pinging are gone; checked every few seconds.
                if now.saturating_sub(self.checked.get()) > STALE_CHECK_INTERVAL {
                    self.checked.set(now);
                    let stale: Vec<WebSocket> = self
                        .slots()
                        .iter()
                        .enumerate()
                        .filter_map(|(i, s)| s.as_ref().map(|(ws, _)| (i, ws.clone())))
                        .filter(|(i, ws)| *i != slot as usize && self.stale(ws, *i, now))
                        .map(|(_, ws)| ws)
                        .collect();
                    for ws in stale {
                        self.drop_socket(&ws, close::TIMEOUT);
                    }
                }
                let targets: Vec<WebSocket> = self
                    .slots()
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| to.map_or(*i != slot as usize, |to| *i == to as usize))
                    .filter_map(|(_, s)| s.as_ref().map(|(ws, _)| ws.clone()))
                    .collect();
                for target in targets {
                    let _ = target.send_with_bytes(frame);
                }
            }
            GroupAction::Close(code) => self.drop_socket(&ws, code),
        }
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, _code: usize, _reason: String, _clean: bool) -> Result<()> {
        self.vacate(&ws);
        mark_group_gone(&ws);
        Ok(())
    }

    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        self.vacate(&ws);
        mark_group_gone(&ws);
        Ok(())
    }
}

impl Group {
    fn slot_of(&self, ws: &WebSocket) -> Option<u8> {
        self.slots().iter().position(|s| s.as_ref().is_some_and(|(w, _)| w == ws)).map(|i| i as u8)
    }

    fn slots(&self) -> RefMut<'_, Slots> {
        let mut cache = self.slots.borrow_mut();
        if cache.is_none() {
            let mut slots: Slots = Default::default();
            for ws in self.state.get_websockets() {
                if let Some(GroupAttachment { seat: GroupSeat::Seated { slot, token }, group, .. }) =
                    group_attachment(&ws)
                    && (slot as usize) < gwire::MAX_SLOTS
                {
                    slots[slot as usize] = Some((ws, token));
                    if self.group.get().is_none() {
                        self.group.set(group.parse().ok());
                    }
                }
            }
            *cache = Some(slots);
        }
        RefMut::map(cache, |cache| cache.as_mut().unwrap())
    }

    /// Seats `ws` (GROUP.md §3.2) and tells everyone. The object handles one
    /// event at a time, so nothing reaches anyone in between.
    fn join(&self, ws: &WebSocket, mut attachment: GroupAttachment, token: [u8; 16], now: u64) -> Result<()> {
        let (slot, old, present, others) = {
            let mut slots = self.slots();
            let (slot, old) = match slots.iter().position(|s| s.as_ref().is_some_and(|(_, t)| *t == token)) {
                Some(slot) => (slot, slots[slot].take().map(|(ws, _)| ws)),
                None => {
                    let free = slots.iter().position(Option::is_none);
                    match free.filter(|_| slots.iter().flatten().count() < MAX_GROUP_MEMBERS) {
                        Some(slot) => (slot, None),
                        None => {
                            drop(slots);
                            retire_group(ws, close::FULL);
                            return Ok(());
                        }
                    }
                }
            };
            let present = slots.iter().enumerate().filter(|(_, s)| s.is_some()).fold(0u32, |m, (i, _)| m | 1 << i);
            let others: Vec<WebSocket> = slots.iter().flatten().map(|(ws, _)| ws.clone()).collect();
            slots[slot] = Some((ws.clone(), token));
            (slot as u8, old, present, others)
        };
        attachment.seat = GroupSeat::Seated { slot, token };
        attachment.since = now;
        ws.serialize_attachment(&attachment)?;
        self.seen[slot as usize].set(now);
        if let Some(old) = old.filter(|old| old != ws) {
            retire_group(&old, close::REPLACED);
        }
        let _ = ws.send_with_bytes(gwire::welcome(slot, present));
        for other in others {
            let _ = other.send_with_bytes(gwire::member(slot, true));
        }
        Ok(())
    }

    fn drop_socket(&self, ws: &WebSocket, code: u16) {
        self.vacate(ws);
        retire_group(ws, code);
    }

    /// Frees the slot `ws` held, if it still held one, telling the others.
    fn vacate(&self, ws: &WebSocket) {
        let (slot, others) = {
            let mut slots = self.slots();
            let Some(slot) = slots.iter().position(|s| s.as_ref().is_some_and(|(w, _)| w == ws)) else { return };
            slots[slot] = None;
            (slot as u8, slots.iter().flatten().map(|(ws, _)| ws.clone()).collect::<Vec<_>>())
        };
        for other in others {
            let _ = other.send_with_bytes(gwire::member(slot, false));
        }
    }

    /// Enforces the join and idle timeouts and bounds pending connections,
    /// as [`Room::sweep`] does.
    fn sweep(&self, now: u64, address: &str) -> (Option<u64>, usize) {
        let mut pending = Vec::new();
        let mut from_address = 0;
        for ws in self.state.get_websockets() {
            match group_attachment(&ws) {
                Some(GroupAttachment { seat: GroupSeat::Pending(_), since, address: from, .. }) => {
                    if now.saturating_sub(since) >= join_timeout() {
                        retire_group(&ws, close::TIMEOUT);
                    } else {
                        from_address += usize::from(from == address);
                        pending.push((since, ws));
                    }
                }
                Some(GroupAttachment { seat: GroupSeat::Seated { slot, .. }, .. })
                    if self.stale(&ws, slot as usize, now) =>
                {
                    self.drop_socket(&ws, close::TIMEOUT);
                }
                _ => {}
            }
        }
        pending.sort_by_key(|(since, _)| *since);
        let excess = (pending.len() + 1).saturating_sub(MAX_PENDING);
        for (_, ws) in pending.drain(..excess) {
            retire_group(&ws, close::TIMEOUT);
        }
        (pending.first().map(|(since, _)| since + join_timeout()), from_address)
    }

    fn stale(&self, ws: &WebSocket, slot: usize, now: u64) -> bool {
        let pinged = self.hibernation.auto_response_timestamp(ws.as_ref()).map(|date| date.get_time() as u64);
        let joined = group_attachment(ws).map(|a| a.since);
        let seen = self.seen.get(slot).map(Cell::get);
        let last = [pinged, joined, seen].into_iter().flatten().max().unwrap_or(now);
        now.saturating_sub(last) > gwire::IDLE_TIMEOUT.as_millis() as u64
    }
}

fn retire_group(ws: &WebSocket, code: u16) {
    mark_group_gone(ws);
    let _ = ws.close(Some(code), Some(""));
}

fn mark_group_gone(ws: &WebSocket) {
    if let Some(mut attachment) = group_attachment(ws) {
        attachment.seat = GroupSeat::Gone;
        let _ = ws.serialize_attachment(&attachment);
    }
}

fn group_attachment(ws: &WebSocket) -> Option<GroupAttachment> {
    ws.deserialize_attachment().ok().flatten()
}
