//! The peer state machine (SPEC §6, §7), free of I/O.
//!
//! A driver owns at most one WebSocket at a time and feeds the machine
//! everything that happens to it; the machine answers through
//! [`Peer::poll_output`] and tells the driver when to call back through
//! [`Peer::poll_timeout`]. Time is milliseconds on any monotonic clock.
//!
//! The machine connects, authenticates to the relay, runs the Noise
//! handshake whenever a partner connection appears, encrypts and splits
//! outgoing messages, reassembles incoming ones, keeps the connection alive,
//! and reconnects with backoff when anything goes wrong.

use std::collections::VecDeque;
use std::time::Duration;

use x25519_dalek::StaticSecret;

use crate::noise::{self, Initiator, Transport};
use crate::room::Room;
use crate::wire::{self, Role, close};

/// Transport plaintext kinds (the first plaintext byte).
mod kind {
    /// The last (or only) chunk of a message.
    pub const FINAL: u8 = 0x00;
    /// A chunk with more to follow.
    pub const MORE: u8 = 0x01;
    /// The sender has ended the room.
    pub const CLOSE: u8 = 0x02;
}

const CHUNK_LEN: usize = noise::MAX_PLAINTEXT_LEN - 1;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A connection that stayed joined this long counts as a success for backoff.
const STABLE: Duration = Duration::from_secs(30);
/// How long to wait before reclaiming a seat another connection took.
const REPLACED_DELAY: Duration = Duration::from_secs(60);

/// What the machine asks of its driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Open a WebSocket to this URL (any previous one is already closed).
    Connect(String),
    /// Send a binary message.
    Send(Vec<u8>),
    /// Send the text message `ping`.
    Ping,
    /// Close the WebSocket (code 1000) after flushing what was sent, then
    /// forget it: report nothing more about that socket.
    Disconnect,
    /// Tell the application.
    Event(Event),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The connection state changed.
    State(State),
    /// A message from the peer, authenticated and decrypted.
    Message(Vec<u8>),
    /// The inviter has learned the joiner's identity. Persist the room
    /// again: the new record is what lets it recognise the joiner later.
    Paired,
    /// Another connection took this peer's seat on the relay: most likely the
    /// same room opened elsewhere (another device, tab or process). The peer
    /// waits a minute before taking the seat back; call [`Peer::leave`] to
    /// let the other instance keep it.
    Replaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not connected to the relay; connecting or waiting to retry.
    Offline,
    /// On the relay, without a secure session with the peer (yet).
    Waiting,
    /// End-to-end session with the peer established.
    Connected,
    /// Stopped for good; no further output.
    Closed(CloseReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// [`Peer::leave`] was called. The room remains valid.
    Left,
    /// [`Peer::close`] was called. The room is ended; delete it.
    Closed,
    /// The peer ended the room. Delete it.
    ClosedByPeer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// No end-to-end session right now. Nothing was sent.
    NotConnected,
    /// The message exceeds [`wire::MAX_MESSAGE_LEN`].
    TooLarge,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SendError::NotConnected => "not connected to the peer",
            SendError::TooLarge => "message too large",
        })
    }
}

impl std::error::Error for SendError {}

pub struct Peer {
    room: Room,
    link: Link,
    state: State,
    output: VecDeque<Output>,
    /// Connection attempts since the last stable connection.
    failures: u32,
}

#[allow(clippy::large_enum_variant)] // one per peer: boxing would buy nothing
enum Link {
    /// No socket; connect at `retry_at`.
    Idle {
        retry_at: u64,
    },
    /// Waiting for the socket to open.
    Connecting {
        deadline: u64,
    },
    Open(Open),
    Done,
}

struct Open {
    phase: Phase,
    /// When the relay accepted our `JOIN`.
    joined_at: Option<u64>,
    /// Reconnect if nothing arrives by then.
    receive_deadline: u64,
    next_ping: u64,
}

enum Phase {
    /// Waiting for `HELLO`, then (after sending `JOIN`) for `PRESENCE`.
    Joining {
        deadline: u64,
        joined: bool,
    },
    Joined(Session),
}

enum Session {
    /// No partner connection in the other seat.
    Absent,
    /// Inviter, waiting for the joiner's handshake.
    AwaitingInit,
    /// Joiner, waiting for the inviter's reply.
    AwaitingReply {
        initiator: Initiator,
        deadline: u64,
    },
    Established {
        transport: Transport,
        partial: Vec<u8>,
    },
}

impl Peer {
    /// Starts the machine; the first output asks the driver to connect.
    pub fn new(room: Room, now: u64) -> Peer {
        let mut peer = Peer {
            room,
            link: Link::Idle { retry_at: now },
            state: State::Offline,
            output: VecDeque::new(),
            failures: 0,
        };
        peer.connect(now);
        peer
    }

    pub fn room(&self) -> &Room {
        &self.room
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn poll_output(&mut self) -> Option<Output> {
        self.output.pop_front()
    }

    /// When the driver must next call [`Peer::handle_timeout`].
    pub fn poll_timeout(&self) -> Option<u64> {
        match &self.link {
            Link::Idle { retry_at } => Some(*retry_at),
            Link::Connecting { deadline } => Some(*deadline),
            Link::Open(open) => {
                let phase = match &open.phase {
                    Phase::Joining { deadline, .. } => Some(*deadline),
                    Phase::Joined(Session::AwaitingReply { deadline, .. }) => Some(*deadline),
                    Phase::Joined(_) => None,
                };
                [Some(open.receive_deadline), Some(open.next_ping), phase].into_iter().flatten().min()
            }
            Link::Done => None,
        }
    }

    pub fn handle_timeout(&mut self, now: u64) {
        match &mut self.link {
            Link::Idle { retry_at } if now >= *retry_at => self.connect(now),
            Link::Connecting { deadline } if now >= *deadline => self.reconnect(now),
            Link::Open(open) => {
                let expired = match &open.phase {
                    Phase::Joining { deadline, .. } => now >= *deadline,
                    Phase::Joined(Session::AwaitingReply { deadline, .. }) => now >= *deadline,
                    Phase::Joined(_) => false,
                };
                if expired || now >= open.receive_deadline {
                    self.reconnect(now);
                } else if now >= open.next_ping {
                    open.next_ping = now + ms(wire::PING_INTERVAL);
                    self.output.push_back(Output::Ping);
                }
            }
            _ => {}
        }
    }

    /// The socket requested by [`Output::Connect`] is open.
    pub fn handle_open(&mut self, now: u64) {
        if let Link::Connecting { .. } = self.link {
            self.link = Link::Open(Open {
                phase: Phase::Joining { deadline: now + ms(wire::JOIN_TIMEOUT), joined: false },
                joined_at: None,
                receive_deadline: now + ms(wire::RECEIVE_TIMEOUT),
                next_ping: now + ms(wire::PING_INTERVAL),
            });
        }
    }

    /// The socket closed or failed, with the close code if one was received.
    pub fn handle_close(&mut self, now: u64, code: Option<u16>) {
        if !matches!(self.link, Link::Connecting { .. } | Link::Open(_)) {
            return;
        }
        self.drop_socket(now, false);
        if code == Some(close::REPLACED) {
            self.output.push_back(Output::Event(Event::Replaced));
            let delay = ms(REPLACED_DELAY);
            self.retry_at(now + delay + random_below(delay / 6));
        } else {
            let delay = self.backoff();
            self.retry_at(now + delay);
        }
    }

    pub fn handle_text(&mut self, now: u64, _text: &str) {
        // `pong` is the only text a relay sends; all that matters is that it arrived.
        if let Link::Open(open) = &mut self.link {
            open.receive_deadline = now + ms(wire::RECEIVE_TIMEOUT);
        }
    }

    pub fn handle_binary(&mut self, now: u64, frame: &[u8]) {
        let Link::Open(open) = &mut self.link else { return };
        open.receive_deadline = now + ms(wire::RECEIVE_TIMEOUT);
        let Some((&kind, body)) = frame.split_first() else { return };

        match &mut open.phase {
            Phase::Joining { joined: false, .. } if kind == wire::HELLO => {
                let Ok(nonce) = body.try_into() else { return self.reconnect(now) };
                let join = self.room.join_frame(nonce);
                self.output.push_back(Output::Send(join.to_vec()));
                open.phase = Phase::Joining { deadline: now + ms(wire::JOIN_TIMEOUT), joined: true };
            }
            Phase::Joining { joined: true, .. } if kind == wire::PRESENCE => {
                open.phase = Phase::Joined(Session::Absent);
                open.joined_at = Some(now);
                self.set_state(State::Waiting);
                self.presence(now, body == [1]);
            }
            Phase::Joining { .. } => self.reconnect(now),
            Phase::Joined(_) => match kind {
                wire::PRESENCE => self.presence(now, body == [1]),
                wire::INIT => self.handle_init(body),
                wire::REPLY => self.handle_reply(body),
                wire::MESSAGE => self.handle_message(now, body),
                // Anything else is from a newer protocol revision or stale: ignore.
                _ => {}
            },
        }
    }

    /// Sends a message to the peer, split into as many frames as needed.
    pub fn send(&mut self, data: &[u8]) -> Result<(), SendError> {
        if data.len() > wire::MAX_MESSAGE_LEN {
            return Err(SendError::TooLarge);
        }
        let Some(transport) = self.transport() else { return Err(SendError::NotConnected) };
        let mut chunks = data.chunks(CHUNK_LEN).peekable();
        let mut frames = Vec::new();
        loop {
            let chunk = chunks.next().unwrap_or_default();
            let more = chunks.peek().is_some();
            frames.push(seal(transport, if more { kind::MORE } else { kind::FINAL }, chunk));
            if !more {
                break;
            }
        }
        self.output.extend(frames.into_iter().map(Output::Send));
        Ok(())
    }

    /// Disconnects and stops, keeping the room valid for later.
    pub fn leave(&mut self) {
        self.finish(CloseReason::Left);
    }

    /// Ends the room: tells the peer if it is connected, then stops. Both
    /// sides should delete the room afterwards. A peer that is offline at
    /// this moment is not told (the relay keeps no state to tell it later).
    pub fn close(&mut self) {
        if let Some(transport) = self.transport() {
            let frame = seal(transport, kind::CLOSE, &[]);
            self.output.push_back(Output::Send(frame));
        }
        self.finish(CloseReason::Closed);
    }

    fn connect(&mut self, now: u64) {
        self.link = Link::Connecting { deadline: now + ms(CONNECT_TIMEOUT) };
        self.output.push_back(Output::Connect(self.room.endpoint()));
    }

    /// Abandons the current socket and schedules another attempt.
    fn reconnect(&mut self, now: u64) {
        self.drop_socket(now, true);
        let delay = self.backoff();
        self.retry_at(now + delay);
    }

    /// Forgets the current socket, asking the driver to close it if it is
    /// still open. A connection that stayed joined long enough proves the
    /// path works, so the backoff starts over.
    fn drop_socket(&mut self, now: u64, close: bool) {
        let link = std::mem::replace(&mut self.link, Link::Idle { retry_at: now });
        if let Link::Open(Open { joined_at: Some(at), .. }) = link
            && now.saturating_sub(at) >= ms(STABLE)
        {
            self.failures = 0;
        }
        if close && matches!(link, Link::Connecting { .. } | Link::Open(_)) {
            self.output.push_back(Output::Disconnect);
        }
    }

    /// The delay before the next attempt: exponential with jitter, between
    /// half and all of `min(30 s, 0.5 s × 2^failures)`.
    fn backoff(&mut self) -> u64 {
        let ceiling = ms(BACKOFF_BASE.saturating_mul(1 << self.failures.min(16)).min(BACKOFF_MAX));
        self.failures += 1;
        ceiling / 2 + random_below(ceiling / 2 + 1)
    }

    fn retry_at(&mut self, at: u64) {
        self.link = Link::Idle { retry_at: at };
        self.set_state(State::Offline);
    }

    fn finish(&mut self, reason: CloseReason) {
        if matches!(self.link, Link::Done) {
            return;
        }
        if matches!(self.link, Link::Connecting { .. } | Link::Open(_)) {
            self.output.push_back(Output::Disconnect);
        }
        self.link = Link::Done;
        self.set_state(State::Closed(reason));
    }

    fn set_state(&mut self, state: State) {
        if self.state != state {
            self.state = state;
            self.output.push_back(Output::Event(Event::State(state)));
        }
    }

    fn session(&mut self) -> Option<&mut Session> {
        match &mut self.link {
            Link::Open(Open { phase: Phase::Joined(session), .. }) => Some(session),
            _ => None,
        }
    }

    fn transport(&mut self) -> Option<&mut Transport> {
        match self.session()? {
            Session::Established { transport, .. } => Some(transport),
            _ => None,
        }
    }

    /// A partner connection arrived (`present`) or left. Either way any
    /// session with the previous one is over; a new arrival gets a fresh
    /// handshake, which the joiner starts.
    fn presence(&mut self, now: u64, present: bool) {
        let next = match (present, self.room.role()) {
            (false, _) => Session::Absent,
            (true, Role::Inviter) => Session::AwaitingInit,
            (true, Role::Joiner) => {
                let peer = self.room.peer().expect("a joiner always knows the inviter");
                let started = Initiator::start(self.room.identity(), peer, self.room.psk(), ephemeral());
                let Ok((initiator, init)) = started else { return self.reconnect(now) };
                self.output.push_back(Output::Send(frame(wire::INIT, &init)));
                Session::AwaitingReply { initiator, deadline: now + ms(wire::HANDSHAKE_TIMEOUT) }
            }
        };
        if let Some(session) = self.session() {
            *session = next;
        }
        self.set_state(State::Waiting);
    }

    fn handle_init(&mut self, body: &[u8]) {
        let accepting = self.room.role() == Role::Inviter
            && matches!(self.session(), Some(Session::AwaitingInit | Session::Established { .. }));
        if !accepting {
            return;
        }
        let Ok(response) = noise::respond(self.room.identity(), self.room.psk(), body, ephemeral()) else {
            return;
        };
        match self.room.peer() {
            // Only the joiner this inviter paired with may connect.
            Some(pinned) if pinned != &response.peer => return,
            Some(_) => {}
            None => {
                self.room.pin(response.peer);
                self.output.push_back(Output::Event(Event::Paired));
            }
        }
        self.output.push_back(Output::Send(frame(wire::REPLY, &response.reply)));
        self.establish(response.transport);
    }

    fn handle_reply(&mut self, body: &[u8]) {
        let Some(Session::AwaitingReply { initiator, .. }) = self.session() else { return };
        // A reply that does not verify is stale or forged; keep waiting.
        if let Ok(transport) = initiator.finish(body) {
            self.establish(transport);
        }
    }

    fn establish(&mut self, transport: Transport) {
        if let Some(session) = self.session() {
            *session = Session::Established { transport, partial: Vec::new() };
            self.set_state(State::Connected);
        }
    }

    fn handle_message(&mut self, now: u64, body: &[u8]) {
        // Messages outside a session are left over from an earlier one.
        let Link::Open(Open { phase: Phase::Joined(Session::Established { transport, partial }), .. }) = &mut self.link
        else {
            return;
        };
        let Ok(plaintext) = transport.decrypt(body) else {
            // Within a session the relay must deliver every message, in order,
            // untouched. Anything else breaks the session: start over.
            return self.reconnect(now);
        };
        let Some((&kind, data)) = plaintext.split_first() else { return };
        match kind {
            kind::MORE | kind::FINAL => {
                if partial.len() + data.len() > wire::MAX_MESSAGE_LEN {
                    return self.reconnect(now);
                }
                partial.extend_from_slice(data);
                if kind == kind::FINAL {
                    let message = std::mem::take(partial);
                    self.output.push_back(Output::Event(Event::Message(message)));
                }
            }
            kind::CLOSE => self.finish(CloseReason::ClosedByPeer),
            _ => {}
        }
    }
}

fn seal(transport: &mut Transport, kind: u8, data: &[u8]) -> Vec<u8> {
    let mut plaintext = Vec::with_capacity(1 + data.len());
    plaintext.push(kind);
    plaintext.extend_from_slice(data);
    let mut out = Vec::with_capacity(1 + plaintext.len() + noise::TAG_LEN);
    out.push(wire::MESSAGE);
    transport.encrypt(&plaintext, &mut out).expect("chunks fit and nonces never run out");
    out
}

fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    [&[kind], body].concat()
}

fn ephemeral() -> StaticSecret {
    StaticSecret::from(crate::random::<32>())
}

fn random_below(bound: u64) -> u64 {
    u64::from_le_bytes(crate::random()) % bound.max(1)
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}
