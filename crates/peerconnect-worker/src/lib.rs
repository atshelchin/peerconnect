//! PeerConnect relay on Cloudflare Workers (SPEC §5).
//!
//! The Worker routes each room to its own Durable Object, which holds the
//! room's WebSockets through the Hibernation API: an idle room costs nothing,
//! `ping`s are answered by the runtime without waking the object, and each
//! socket's seat lives in its attachment, so the object can be evicted from
//! memory at any time and rebuild its view of the room on the next event.
//! Rooms write nothing to storage but their alarm.
//!
//! With `PEERCONNECT_TOKEN_SECRET` set, connections may carry a token (SPEC
//! §4.1), and with `REQUIRE_TOKEN=1` they must (`docs/self-serve.md`). The
//! Worker checks its signature and expiry, and the token's [`Client`] object
//! whether it is still current and within its limits. Rooms then count each
//! connection's traffic and report it to that object, which can also have
//! them close its connections.

mod client;
mod internal;

use std::cell::{Cell, RefCell, RefMut};

use client::{CLIENT_HEADER, Report};
use peerconnect::RoomId;
use peerconnect::relay::{self, Action, Conn, Incoming};
use peerconnect::token::Keys;
use peerconnect::usage::Traffic;
use peerconnect::wire::{self, Role, close};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use worker::*;

pub use client::Client;

/// The header carrying the generation of the connection's token, set by this
/// Worker for the room.
const GENERATION_HEADER: &str = "x-peerconnect-gen";
/// How soon after its first uncounted frame a room reports its traffic. A
/// room is evicted from memory only after about 10 s without events, so
/// nothing counted is lost.
const REPORT_DELAY: u64 = 5_000;

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
    let keys = setting(&env, "PEERCONNECT_TOKEN_SECRET");
    let keys = (!keys.is_empty()).then(|| Keys::parse(&keys)).transpose().map_err(Error::RustError)?;
    if path.starts_with("/internal/") {
        return internal::handle(req, &env, keys.as_ref()).await;
    }
    let Some(room) = relay::room_from_path(&path) else {
        return Response::error("not found\n", 404);
    };
    let upgrade = req.headers().get("Upgrade")?;
    if !upgrade.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
        return Response::error("expected a WebSocket upgrade\n", 426);
    }
    let url = req.url()?;
    let token = url.query().and_then(peerconnect::token::from_query);
    let required = setting(&env, "REQUIRE_TOKEN") == "1";
    // Only this Worker says who a connection is.
    let headers = req.headers().clone();
    headers.delete(CLIENT_HEADER)?;
    headers.delete(GENERATION_HEADER)?;
    match (token, &keys) {
        (Some(token), Some(keys)) => {
            let Ok(claims) = keys.verify(token, Date::now().as_millis() / 1000) else {
                return refuse(close::TOKEN, "invalid token");
            };
            let generation = claims.generation.unwrap_or(0);
            let admit = serde_json::json!({ "gen": generation, "app": claims.app, "room": room.to_string() });
            let ask = Headers::new();
            ask.set(CLIENT_HEADER, &claims.sub)?;
            let init = RequestInit {
                method: Method::Post,
                headers: ask,
                body: Some(admit.to_string().into()),
                ..RequestInit::default()
            };
            let answer = env
                .durable_object("CLIENTS")?
                .get_by_name(&claims.sub)?
                .fetch_with_request(Request::new_with_init("https://client/admit", &init)?)
                .await?;
            match answer.status_code() {
                200 => {}
                429 => return refuse(close::RATE_LIMITED, "too many connections"),
                _ => return refuse(close::TOKEN, "token revoked or replaced"),
            }
            headers.set(CLIENT_HEADER, &claims.sub)?;
            headers.set(GENERATION_HEADER, &generation.to_string())?;
        }
        (None, _) if required => return refuse(close::TOKEN, "this relay requires a token"),
        (Some(_), None) if required => return Response::error("tokens are not configured\n", 503),
        // Not required: a token this relay cannot check is ignored.
        _ => {}
    }
    let init = RequestInit { method: Method::Get, headers, ..RequestInit::default() };
    let rooms = env.durable_object("ROOMS")?;
    let stub = rooms.id_from_name(&room.to_string())?.get_stub()?;
    stub.fetch_with_request(Request::new_with_init(url.as_str(), &init)?).await
}

/// A WebSocket accepted only to be closed with `code` (SPEC §4.1): browsers
/// report a close code, never an HTTP status.
fn refuse(code: u16, reason: &str) -> Result<Response> {
    let pair = WebSocketPair::new()?;
    pair.server.accept()?;
    pair.server.close(Some(code), Some(reason))?;
    Response::from_websocket(pair.client)
}

/// A secret, or else a variable, by `name`; empty if neither is set.
pub(crate) fn setting(env: &Env, name: &str) -> String {
    env.secret(name).map(|s| s.to_string()).or_else(|_| env.var(name).map(|v| v.to_string())).unwrap_or_default()
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
    /// The token's client and generation, if the connection had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client: Option<String>,
    #[serde(default, rename = "gen")]
    generation: u64,
    /// Until when the connection's time has been reported (0: not yet).
    #[serde(default)]
    reported: u64,
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
    env: Env,
    /// Each seat's traffic not yet reported.
    traffic: [Cell<Traffic>; 2],
    /// Reports for client objects, sent at the end of the current event.
    reports: RefCell<Vec<(String, Report)>>,
    /// The alarm as this instance set it (`None`: unknown, or none).
    alarm_at: Cell<Option<u64>>,
}

impl DurableObject for Room {
    fn new(state: State, env: Env) -> Self {
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
            env,
            traffic: Default::default(),
            reports: RefCell::new(Vec::new()),
            alarm_at: Cell::new(None),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        // From a client object whose token changed: close its connections.
        if req.path().starts_with("/internal/kick/") {
            let kick: Kick = req.json().await?;
            self.kick(&kick, Date::now().as_millis());
            self.send_reports().await;
            return Response::empty();
        }
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
        let client = req.headers().get(CLIENT_HEADER)?;
        let generation = req.headers().get(GENERATION_HEADER)?.and_then(|g| g.parse().ok()).unwrap_or(0);
        let attachment = Attachment {
            room: room.to_string(),
            seat: Seat::Pending(nonce),
            since: now,
            address,
            client,
            generation,
            reported: 0,
        };
        pair.server.serialize_attachment(&attachment)?;
        pair.server.send_with_bytes(hello)?;
        // Wake up to enforce the join timeout; a room whose sockets have all
        // joined has no alarm and stays hibernated.
        let deadline = older.unwrap_or(u64::MAX).min(now + join_timeout());
        self.alarm_by(deadline + 100, now).await?;
        Response::from_websocket(pair.client)
    }

    async fn alarm(&self) -> Result<Response> {
        let now = Date::now().as_millis();
        self.alarm_at.set(None);
        let (deadline, _) = self.sweep(now, "");
        self.report_seated(now);
        self.send_reports().await;
        if let Some(deadline) = deadline {
            self.alarm_by(deadline + 100, now).await?;
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
                    let len = frame.len() as u64;
                    let sent = partner.send_with_bytes(frame).is_ok();
                    self.count(role, Traffic { frames_out: 0, bytes_out: 0, frames_in: 1, bytes_in: len });
                    if sent {
                        self.count(role.other(), Traffic { frames_out: 1, bytes_out: len, ..Traffic::default() });
                    }
                } else {
                    self.count(role, Traffic { frames_in: 1, bytes_in: frame.len() as u64, ..Traffic::default() });
                }
                self.alarm_by(now + REPORT_DELAY, now).await?;
            }
            Action::Close(code) => self.drop_socket(&ws, code),
        }
        self.send_reports().await;
        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, _code: usize, _reason: String, _clean: bool) -> Result<()> {
        self.vacate(&ws);
        mark_gone(&ws);
        self.send_reports().await;
        Ok(())
    }

    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        self.vacate(&ws);
        mark_gone(&ws);
        self.send_reports().await;
        Ok(())
    }
}

/// A client object's request: close `client`'s connections from before
/// generation `gen`, or all of them if it is deleted.
#[derive(Deserialize)]
struct Kick {
    client: String,
    #[serde(rename = "gen")]
    generation: u64,
    #[serde(default)]
    deleted: bool,
}

impl Room {
    fn count(&self, role: Role, traffic: Traffic) {
        let cell = &self.traffic[role.index()];
        let mut total = cell.get();
        total.add(traffic);
        cell.set(total);
    }

    /// Sets the alarm to `at` unless it is already due by then.
    async fn alarm_by(&self, at: u64, now: u64) -> Result<()> {
        if self.alarm_at.get().is_some_and(|set| set <= at) {
            return Ok(());
        }
        self.state.storage().set_alarm(at.saturating_sub(now) as i64).await?;
        self.alarm_at.set(Some(at));
        Ok(())
    }

    /// Queues a report of what `ws`, in seat `index`, has used since its last
    /// report: its traffic, and its time up to `now`.
    fn settle(&self, ws: &WebSocket, index: usize, now: u64) {
        let traffic = self.traffic[index].take();
        let Some(mut attachment) = attachment(ws) else { return };
        let Some(client) = attachment.client.clone() else { return };
        let from = attachment.reported.max(attachment.since);
        attachment.reported = now;
        let _ = ws.serialize_attachment(&attachment);
        self.reports.borrow_mut().push((client, Report { traffic, from, to: now }));
    }

    /// Reports every seat that has traffic not yet reported.
    fn report_seated(&self, now: u64) {
        let seats = self.seats().clone();
        for (index, ws) in seats.iter().enumerate() {
            if let Some(ws) = ws
                && !self.traffic[index].get().is_empty()
            {
                self.settle(ws, index, now);
            }
        }
    }

    /// Hands the queued reports to their client objects. Best effort: a
    /// report that fails is logged, never retried, and never fails the event.
    async fn send_reports(&self) {
        let reports = std::mem::take(&mut *self.reports.borrow_mut());
        if reports.is_empty() {
            return;
        }
        let Ok(clients) = self.env.durable_object("CLIENTS") else { return };
        for (client, report) in reports {
            let sent = async {
                let headers = Headers::new();
                headers.set(CLIENT_HEADER, &client)?;
                let body = serde_json::to_string(&report).map_err(|e| Error::RustError(e.to_string()))?;
                let init =
                    RequestInit { method: Method::Post, headers, body: Some(body.into()), ..RequestInit::default() };
                clients
                    .get_by_name(&client)?
                    .fetch_with_request(Request::new_with_init("https://client/usage", &init)?)
                    .await
            };
            if let Err(e) = sent.await {
                console_error!("peerconnect: usage of {client} not reported: {e}");
            }
        }
    }

    /// Closes the connections `kick` names, with 4403.
    fn kick(&self, kick: &Kick, now: u64) {
        for ws in self.state.get_websockets() {
            let Some(a) = attachment(&ws) else { continue };
            if a.client.as_deref() != Some(kick.client.as_str()) || !(kick.deleted || a.generation < kick.generation) {
                continue;
            }
            match a.seat {
                Seat::Seated(_) => {
                    self.vacate_at(&ws, now);
                    retire(&ws, close::TOKEN);
                }
                Seat::Pending(_) => retire(&ws, close::TOKEN),
                Seat::Gone => {}
            }
        }
    }

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
            self.settle(&old, role.index(), now);
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
        self.vacate_at(ws, Date::now().as_millis());
    }

    fn vacate_at(&self, ws: &WebSocket, now: u64) {
        let (index, partner) = {
            let mut seats = self.seats();
            let Some(index) = seats.iter().position(|s| s.as_ref() == Some(ws)) else { return };
            seats[index] = None;
            (index, seats[1 - index].clone())
        };
        self.settle(ws, index, now);
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
