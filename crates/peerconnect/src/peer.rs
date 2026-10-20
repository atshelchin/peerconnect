//! The peer state machine (SPEC §6, §7), free of I/O.
//!
//! A driver owns at most one WebSocket at a time and feeds the machine
//! everything that happens to it; the machine answers through
//! [`Peer::poll_output`] and tells the driver when to call back through
//! [`Peer::poll_timeout`]. Time is milliseconds on any monotonic clock.
//!
//! The machine connects, authenticates to the relay, runs the Noise
//! handshake whenever a partner connection appears, and carries a reliable,
//! ordered stream of messages across any number of connections: messages
//! are numbered, acknowledged, kept until acknowledged and resent after a
//! reconnection, so none is lost or duplicated while both peers keep their
//! state. It also moves a new room from its invite ID to its pair ID, keeps
//! the connection alive, and reconnects with backoff when anything fails.

use std::collections::VecDeque;
use std::time::Duration;

use x25519_dalek::{PublicKey, StaticSecret};

use crate::noise::{self, Initiator, Transport};
use crate::room::{Rendezvous, Room};
use crate::wire::{self, Role, close};

/// Transport plaintext kinds (the first plaintext byte, SPEC §5.3).
mod kind {
    /// The last (or only) chunk of a message.
    pub const FINAL: u8 = 0x00;
    /// A chunk of a message, more to follow.
    pub const MORE: u8 = 0x01;
    /// The sender ends the room. Numbered like a message.
    pub const CLOSE: u8 = 0x02;
    /// `next[8]`: the id of the next message expected from the receiver.
    pub const ACK: u8 = 0x03;
}

const CHUNK_LEN: usize = noise::MAX_PLAINTEXT_LEN - 1;
/// The handshake payload: `stream base seen_stream seen_next`, each a u64.
const RESUME_LEN: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A connection that stayed joined this long counts as a success for backoff.
const STABLE: Duration = Duration::from_secs(30);
/// How long to wait before reclaiming a seat another connection took.
const REPLACED_DELAY: Duration = Duration::from_secs(60);
/// An unpaired joiner alone at a rendezvous for this long tries the other one.
const SEEK_INTERVAL: Duration = Duration::from_secs(20);
/// Bytes of messages a peer holds until they are acknowledged.
pub const OUTBOX_LIMIT: usize = 64 << 20;
/// A receiver acknowledges after this many messages, this many bytes, or
/// this long after an unacknowledged delivery, whichever comes first.
const ACK_MESSAGES: u32 = 32;
const ACK_BYTES: usize = 1 << 20;
const ACK_DELAY: Duration = Duration::from_millis(50);

/// What the machine asks of its driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Open a WebSocket to this URL (any previous one is already closed).
    Connect(String),
    /// Send a binary message.
    Send(Vec<u8>),
    /// Send the text message `ping`, ahead of anything queued if possible.
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
    /// A message from the peer, authenticated, decrypted and in order.
    Message(Vec<u8>),
    /// The peer has received every message with an id below this one.
    Delivered(u64),
    /// Pairing is complete: the room now meets at its pair ID, which the
    /// invite does not reach. Persist the room again.
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
    /// [`Peer::close`] was called and the peer has been told. Delete the room.
    Closed,
    /// The peer ended the room. Delete it.
    ClosedByPeer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The message exceeds [`wire::MAX_MESSAGE_LEN`].
    TooLarge,
    /// [`OUTBOX_LIMIT`] bytes await acknowledgement. Try again after
    /// [`Event::Delivered`].
    Full,
    /// The room is closed or being closed.
    Closed,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SendError::TooLarge => "message too large",
            SendError::Full => "too much unacknowledged data",
            SendError::Closed => "the room is closed",
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
    /// Where an unpaired joiner looks next; it alternates.
    seek: Rendezvous,
    /// Inviter, until pairing completes: the joiner it is pairing with.
    tentative: Option<PublicKey>,
    out: Outbox,
    inbound: Inbound,
}

/// Messages we have sent, kept until the peer acknowledges them.
struct Outbox {
    /// Our stream's id, fresh for every `Peer`.
    stream: u64,
    /// The id of `messages[0]`; every lower id has been acknowledged.
    base: u64,
    messages: VecDeque<(u8, Vec<u8>)>,
    bytes: usize,
    /// The id of our CLOSE, once queued.
    close: Option<u64>,
}

impl Outbox {
    fn next_id(&self) -> u64 {
        self.base + self.messages.len() as u64
    }
}

/// Where we are in the peer's stream.
struct Inbound {
    /// The peer's stream id, 0 if unknown.
    stream: u64,
    /// The id of the next message expected.
    next: u64,
    /// Delivered since we last acknowledged.
    unacked: u32,
    unacked_bytes: usize,
    ack_at: Option<u64>,
}

/// The state each side announces in its handshake message.
#[derive(Clone, Copy)]
struct Resume {
    stream: u64,
    base: u64,
    seen_stream: u64,
    seen_next: u64,
}

impl Resume {
    fn encode(&self) -> [u8; RESUME_LEN] {
        let mut out = [0; RESUME_LEN];
        for (i, v) in [self.stream, self.base, self.seen_stream, self.seen_next].into_iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// Reads the first 32 bytes; anything after is for future revisions.
    fn decode(payload: &[u8]) -> Option<Resume> {
        let field = |i: usize| Some(u64::from_be_bytes(payload.get(i * 8..i * 8 + 8)?.try_into().unwrap()));
        Some(Resume { stream: field(0)?, base: field(1)?, seen_stream: field(2)?, seen_next: field(3)? })
    }
}

#[allow(clippy::large_enum_variant)] // one per peer: boxing would buy nothing
enum Link {
    /// No socket; connect at `retry_at`.
    Idle {
        retry_at: u64,
    },
    /// Waiting for the socket to open.
    Connecting {
        at: Rendezvous,
        deadline: u64,
    },
    Open(Open),
    Done,
}

struct Open {
    at: Rendezvous,
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

#[allow(clippy::large_enum_variant)] // one per peer: boxing would buy nothing
enum Session {
    /// No partner connection in the other seat, since then.
    Absent {
        since: u64,
    },
    /// Inviter, waiting for the joiner's handshake.
    AwaitingInit,
    /// Joiner, waiting for the inviter's reply.
    AwaitingReply {
        initiator: Initiator,
        deadline: u64,
    },
    Established(Established),
}

struct Established {
    transport: Transport,
    /// A message being reassembled.
    partial: Vec<u8>,
    /// The id of the next message to transmit in this session.
    sent: u64,
    /// Bytes transmitted in this session and not yet acknowledged.
    in_flight: usize,
}

impl Peer {
    /// Starts the machine; the first output asks the driver to connect. A
    /// room that is closing resumes telling the peer.
    pub fn new(room: Room, now: u64) -> Peer {
        let mut stream = 0;
        while stream == 0 {
            stream = u64::from_le_bytes(crate::random());
        }
        let mut peer = Peer {
            room,
            link: Link::Idle { retry_at: now },
            state: State::Offline,
            output: VecDeque::new(),
            failures: 0,
            seek: Rendezvous::Invite,
            tentative: None,
            out: Outbox { stream, base: 0, messages: VecDeque::new(), bytes: 0, close: None },
            inbound: Inbound { stream: 0, next: 0, unacked: 0, unacked_bytes: 0, ack_at: None },
        };
        if peer.room.is_closing() {
            peer.queue_close();
        }
        peer.connect(now);
        peer
    }

    pub fn room(&self) -> &Room {
        &self.room
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Bytes of messages waiting to be acknowledged by the peer.
    pub fn queued(&self) -> usize {
        self.out.bytes
    }

    pub fn poll_output(&mut self) -> Option<Output> {
        self.output.pop_front()
    }

    /// When the driver must next call [`Peer::handle_timeout`].
    pub fn poll_timeout(&self) -> Option<u64> {
        match &self.link {
            Link::Idle { retry_at } => Some(*retry_at),
            Link::Connecting { deadline, .. } => Some(*deadline),
            Link::Open(open) => {
                let phase = match &open.phase {
                    Phase::Joining { deadline, .. } => Some(*deadline),
                    Phase::Joined(Session::AwaitingReply { deadline, .. }) => Some(*deadline),
                    Phase::Joined(Session::Absent { since }) if self.seeking() => Some(since + ms(SEEK_INTERVAL)),
                    Phase::Joined(Session::Established(_)) => self.inbound.ack_at,
                    Phase::Joined(_) => None,
                };
                [Some(open.receive_deadline), Some(open.next_ping), phase].into_iter().flatten().min()
            }
            Link::Done => None,
        }
    }

    pub fn handle_timeout(&mut self, now: u64) {
        let seeking = self.seeking();
        match &mut self.link {
            Link::Idle { retry_at } if now >= *retry_at => self.connect(now),
            Link::Connecting { deadline, .. } if now >= *deadline => self.reconnect(now),
            Link::Open(open) => {
                let expired = match &open.phase {
                    Phase::Joining { deadline, .. } => now >= *deadline,
                    Phase::Joined(Session::AwaitingReply { deadline, .. }) => now >= *deadline,
                    Phase::Joined(_) => false,
                };
                let seek = seeking
                    && matches!(open.phase, Phase::Joined(Session::Absent { since }) if now >= since + ms(SEEK_INTERVAL));
                if expired || now >= open.receive_deadline {
                    self.reconnect(now);
                } else if seek {
                    // Nobody here: try the other rendezvous straight away.
                    self.drop_socket(now, true);
                    self.connect(now);
                } else {
                    if now >= open.next_ping {
                        open.next_ping = now + ms(wire::PING_INTERVAL);
                        self.output.push_back(Output::Ping);
                    }
                    if self.inbound.ack_at.is_some_and(|at| now >= at) {
                        self.send_ack();
                    }
                }
            }
            _ => {}
        }
    }

    /// The socket requested by [`Output::Connect`] is open.
    pub fn handle_open(&mut self, now: u64) {
        if let Link::Connecting { at, .. } = self.link {
            self.link = Link::Open(Open {
                at,
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
                let join = self.room.join_frame(open.at, nonce);
                self.output.push_back(Output::Send(join.to_vec()));
                open.phase = Phase::Joining { deadline: now + ms(wire::JOIN_TIMEOUT), joined: true };
            }
            Phase::Joining { joined: true, .. } if kind == wire::PRESENCE => {
                open.phase = Phase::Joined(Session::Absent { since: now });
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

    /// Queues a message for the peer and returns its id. It is sent as soon
    /// as there is a session, kept until the peer acknowledges it, and resent
    /// after a reconnection if need be; [`Event::Delivered`] reports progress.
    pub fn send(&mut self, data: &[u8]) -> Result<u64, SendError> {
        if self.room.is_closing() || matches!(self.state, State::Closed(_)) {
            return Err(SendError::Closed);
        }
        if data.len() > wire::MAX_MESSAGE_LEN {
            return Err(SendError::TooLarge);
        }
        if self.out.bytes + data.len() > OUTBOX_LIMIT {
            return Err(SendError::Full);
        }
        let id = self.push(kind::FINAL, data.to_vec());
        self.pump();
        Ok(id)
    }

    /// Disconnects and stops, keeping the room valid for later. Messages not
    /// yet acknowledged are dropped.
    pub fn leave(&mut self) {
        self.finish(CloseReason::Left);
    }

    /// Ends the room: queued messages go first, then the peer is told, and
    /// once it has acknowledged that the state becomes
    /// [`CloseReason::Closed`]. The machine keeps reconnecting until then;
    /// the room record now says it is closing (persist it), so that a peer
    /// restarted from it carries on telling.
    pub fn close(&mut self) {
        if !self.room.is_closing() && !matches!(self.state, State::Closed(_)) {
            self.room.set_closing();
            self.queue_close();
        }
    }

    fn queue_close(&mut self) {
        self.out.close = Some(self.push(kind::CLOSE, Vec::new()));
        self.pump();
    }

    fn push(&mut self, kind: u8, data: Vec<u8>) -> u64 {
        let id = self.out.next_id();
        self.out.bytes += data.len();
        self.out.messages.push_back((kind, data));
        id
    }

    /// Whether this is a joiner that has not found the pair ID yet.
    fn seeking(&self) -> bool {
        self.room.role() == Role::Joiner && !self.room.is_paired()
    }

    fn connect(&mut self, now: u64) {
        let at = if self.room.is_paired() {
            Rendezvous::Pair
        } else if self.room.role() == Role::Inviter {
            Rendezvous::Invite
        } else {
            let at = self.seek;
            self.seek = match at {
                Rendezvous::Invite => Rendezvous::Pair,
                Rendezvous::Pair => Rendezvous::Invite,
            };
            at
        };
        self.link = Link::Connecting { at, deadline: now + ms(CONNECT_TIMEOUT) };
        self.output.push_back(Output::Connect(self.room.endpoint_at(at)));
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
        self.inbound.ack_at = None;
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

    fn resume(&self) -> Resume {
        Resume {
            stream: self.out.stream,
            base: self.out.base,
            seen_stream: self.inbound.stream,
            seen_next: self.inbound.next,
        }
    }

    /// A partner connection arrived (`present`) or left. Either way any
    /// session with the previous one is over; a new arrival gets a fresh
    /// handshake, which the joiner starts.
    fn presence(&mut self, now: u64, present: bool) {
        let was_established = matches!(self.session(), Some(Session::Established(_)));
        if !present && was_established && self.seeking() {
            // An inviter that leaves a working session before pairing is
            // complete has most likely just completed it and moved to the
            // pair ID: follow it there.
            self.drop_socket(now, true);
            return self.connect(now);
        }
        let next = match (present, self.room.role()) {
            (false, _) => Session::Absent { since: now },
            (true, Role::Inviter) => Session::AwaitingInit,
            (true, Role::Joiner) => {
                let inviter = self.room.peer().expect("a joiner always knows the inviter");
                let payload = self.resume().encode();
                let started = Initiator::start(self.room.identity(), inviter, self.room.psk(), ephemeral(), &payload);
                let Ok((initiator, init)) = started else { return self.reconnect(now) };
                self.output.push_back(Output::Send(frame(wire::INIT, &init)));
                Session::AwaitingReply { initiator, deadline: now + ms(wire::HANDSHAKE_TIMEOUT) }
            }
        };
        if let Some(session) = self.session() {
            *session = next;
        }
        self.inbound.ack_at = None;
        self.set_state(State::Waiting);
    }

    fn handle_init(&mut self, body: &[u8]) {
        let accepting = self.room.role() == Role::Inviter
            && matches!(self.session(), Some(Session::AwaitingInit | Session::Established(_)));
        if !accepting {
            return;
        }
        let payload = self.resume().encode();
        let Ok(response) = noise::respond(self.room.identity(), self.room.psk(), body, ephemeral(), &payload) else {
            return;
        };
        let Some(theirs) = Resume::decode(&response.payload) else { return };
        // Only the joiner this inviter is pairing, or has paired, with may connect.
        let known = if self.room.is_paired() { self.room.peer() } else { self.tentative.as_ref() };
        match known {
            Some(joiner) if joiner != &response.peer => return,
            Some(_) => {}
            None => self.tentative = Some(response.peer),
        }
        self.output.push_back(Output::Send(frame(wire::REPLY, &response.reply)));
        self.establish(response.transport, theirs);
    }

    fn handle_reply(&mut self, body: &[u8]) {
        let Some(Session::AwaitingReply { initiator, .. }) = self.session() else { return };
        // A reply that does not verify is stale or forged; keep waiting.
        let Ok((transport, payload)) = initiator.finish(body) else { return };
        let Some(theirs) = Resume::decode(&payload) else { return };
        self.establish(transport, theirs);
    }

    /// Starts a session and picks the stream up where the peer left it.
    fn establish(&mut self, transport: Transport, theirs: Resume) {
        let Link::Open(Open { at, .. }) = self.link else { return };
        // Outgoing: resend from the first message the peer has not seen;
        // everything before it is acknowledged.
        let from = if theirs.seen_stream == self.out.stream {
            theirs.seen_next.clamp(self.out.base, self.out.next_id())
        } else {
            self.out.base
        };
        // Incoming: the peer resends by the same rule.
        if theirs.stream == self.inbound.stream {
            self.inbound.next = self.inbound.next.max(theirs.base);
        } else {
            self.inbound.stream = theirs.stream;
            self.inbound.next = theirs.base;
        }
        self.inbound.unacked = 0;
        self.inbound.unacked_bytes = 0;
        self.inbound.ack_at = None;

        self.acknowledged(from);
        if matches!(self.link, Link::Done) {
            return; // that acknowledged our CLOSE: the room is over
        }
        if let Some(session) = self.session() {
            *session = Session::Established(Established { transport, partial: Vec::new(), sent: from, in_flight: 0 });
        }
        self.set_state(State::Connected);
        if self.room.role() == Role::Joiner {
            if at == Rendezvous::Pair && !self.room.is_paired() {
                let inviter = *self.room.peer().expect("a joiner always knows the inviter");
                self.room.set_paired(inviter);
                self.output.push_back(Output::Event(Event::Paired));
            }
            // Tells the inviter at once that the session works, and so that
            // this joiner holds the identity it is pinning.
            self.send_ack();
        }
        self.pump();
    }

    /// The peer has everything below `next`.
    fn acknowledged(&mut self, next: u64) {
        let next = next.min(self.out.next_id());
        if next <= self.out.base {
            return;
        }
        let sent = match self.session() {
            Some(Session::Established(est)) => Some(est.sent),
            _ => None,
        };
        let mut freed_in_flight = 0;
        while self.out.base < next {
            let (_, data) = self.out.messages.pop_front().expect("ids below next_id are queued");
            self.out.bytes -= data.len();
            if sent.is_some_and(|sent| self.out.base < sent) {
                freed_in_flight += data.len();
            }
            self.out.base += 1;
        }
        if let Some(Session::Established(est)) = self.session() {
            est.in_flight = est.in_flight.saturating_sub(freed_in_flight);
        }
        self.output.push_back(Output::Event(Event::Delivered(next)));
        if self.out.close.is_some_and(|id| id < next) {
            self.finish(CloseReason::Closed);
        }
    }

    /// Transmits queued messages while the window allows.
    fn pump(&mut self) {
        let Link::Open(Open { phase: Phase::Joined(Session::Established(est)), .. }) = &mut self.link else {
            return;
        };
        while est.in_flight < wire::WINDOW {
            let Some((kind, data)) = self.out.messages.get((est.sent - self.out.base) as usize) else { break };
            if *kind == kind::CLOSE {
                self.output.push_back(Output::Send(seal(&mut est.transport, kind::CLOSE, &[])));
            } else {
                let mut chunks = data.chunks(CHUNK_LEN).peekable();
                loop {
                    let chunk = chunks.next().unwrap_or_default();
                    let more = chunks.peek().is_some();
                    let kind = if more { kind::MORE } else { kind::FINAL };
                    self.output.push_back(Output::Send(seal(&mut est.transport, kind, chunk)));
                    if !more {
                        break;
                    }
                }
            }
            est.in_flight += data.len();
            est.sent += 1;
        }
    }

    fn send_ack(&mut self) {
        let next = self.inbound.next;
        let Link::Open(Open { phase: Phase::Joined(Session::Established(est)), .. }) = &mut self.link else {
            return;
        };
        self.output.push_back(Output::Send(seal(&mut est.transport, kind::ACK, &next.to_be_bytes())));
        self.inbound.unacked = 0;
        self.inbound.unacked_bytes = 0;
        self.inbound.ack_at = None;
    }

    fn handle_message(&mut self, now: u64, body: &[u8]) {
        // Messages outside a session are left over from an earlier one.
        let Link::Open(Open { phase: Phase::Joined(Session::Established(est)), .. }) = &mut self.link else {
            return;
        };
        let Ok(mut plaintext) = est.transport.decrypt(body) else {
            // Within a session the relay must deliver every message, in order,
            // untouched. Anything else breaks the session: start over.
            return self.reconnect(now);
        };
        let Some(&kind) = plaintext.first() else { return };
        match kind {
            kind::MORE | kind::FINAL => {
                if est.partial.len() + plaintext.len() - 1 > wire::MAX_MESSAGE_LEN {
                    return self.reconnect(now);
                }
                if kind == kind::MORE {
                    est.partial.extend_from_slice(&plaintext[1..]);
                } else {
                    let message = if est.partial.is_empty() {
                        plaintext.remove(0);
                        plaintext
                    } else {
                        est.partial.extend_from_slice(&plaintext[1..]);
                        std::mem::take(&mut est.partial)
                    };
                    self.deliver(now, message);
                }
            }
            kind::CLOSE => {
                self.inbound.next += 1;
                self.send_ack();
                return self.finish(CloseReason::ClosedByPeer);
            }
            kind::ACK => {
                if let Some(next) = plaintext.get(1..9) {
                    self.acknowledged(u64::from_be_bytes(next.try_into().unwrap()));
                    self.pump();
                }
            }
            _ => {}
        }
        self.confirm(now);
    }

    fn deliver(&mut self, now: u64, message: Vec<u8>) {
        self.inbound.next += 1;
        self.inbound.unacked += 1;
        self.inbound.unacked_bytes += message.len();
        self.output.push_back(Output::Event(Event::Message(message)));
        if self.inbound.unacked >= ACK_MESSAGES || self.inbound.unacked_bytes >= ACK_BYTES {
            self.send_ack();
        } else if self.inbound.ack_at.is_none() {
            self.inbound.ack_at = Some(now + ms(ACK_DELAY));
        }
    }

    /// Inviter: an authenticated message from the joiner proves it holds the
    /// session, and so that it knows it was pinned. Pairing is complete:
    /// move to the pair ID at once, where the joiner follows, so the room
    /// stops using the address the invite reaches.
    fn confirm(&mut self, now: u64) {
        if self.room.role() == Role::Inviter
            && !self.room.is_paired()
            && let Some(joiner) = self.tentative
        {
            self.room.set_paired(joiner);
            self.output.push_back(Output::Event(Event::Paired));
            self.drop_socket(now, true);
            self.set_state(State::Offline);
            self.connect(now);
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
