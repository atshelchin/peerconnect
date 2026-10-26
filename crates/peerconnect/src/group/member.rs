//! The member state machine (GROUP.md §4–§7), free of I/O.
//!
//! Like [`crate::Peer`], a driver owns at most one connection at a time, a
//! QUIC connection to the relay (GROUP.md §3.6), feeds the machine what
//! happens to it, and carries out its outputs. Each [`Output::Send`] says
//! the frame's [`Class`]: a driver sends it on that class's stream, and may
//! drop [`Class::Voice`] and [`Class::Video`] frames when the connection is
//! backed up (GROUP.md §7.2).
//!
//! The machine joins the group, admits the other members by their
//! identities, and carries:
//!
//! - two reliable broadcast streams, commands and chat, each numbered,
//!   kept for resending, and delivered in order to everyone, the sender
//!   included;
//! - direct messages, voice frames and datagrams;
//! - state transfer: a snapshot and the exact commands it contains.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use super::seal::{self, Announce, CHALLENGE_LEN, Challenge, Key};
use super::video;
use super::wire::{self, BROADCAST, Class, MAX_SLOTS};
use super::{Group, Identity, MemberId};
use crate::Error;
use crate::wire::close;

/// CONTROL kinds.
mod control {
    pub const CONFIRM: u8 = 0x00;
    pub const RESEND: u8 = 0x01;
    /// `command_next[8] chat_next[8]`: where our streams are.
    pub const HEADS: u8 = 0x02;
    pub const SYNC_REQUEST: u8 = 0x10;
    pub const SYNC_BEGIN: u8 = 0x11;
    pub const SYNC_ACK: u8 = 0x12;
    pub const SYNC_ABORT: u8 = 0x13;
    /// `track[1] layer[1] frame[4] index...`: video fragments to send again (`0xFF`: all).
    pub const VIDEO_NACK: u8 = 0x20;
    /// `track[1] layer[1]`: a keyframe of that layer, please.
    pub const KEYFRAME_REQUEST: u8 = 0x21;
}

/// COMMAND and CHAT kinds.
mod message {
    /// `id[8] payload`: a message of the sender's stream.
    pub const MESSAGE: u8 = 0x00;
    /// `payload`: to one member, outside any stream.
    pub const DIRECT: u8 = 0x01;
    /// `next[8]`: a resend went up to `next` (exclusive); ask again for more.
    pub const RESENT: u8 = 0x02;
    /// `next[8]`: messages below `next` are no longer available.
    pub const SKIP: u8 = 0x03;
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
/// A connection that stayed this long counts as a success for backoff.
const STABLE: Duration = Duration::from_secs(30);
/// How long to wait after another connection of the same identity took our slot.
const REPLACED_DELAY: Duration = Duration::from_secs(30);
/// How long to wait before trying a full group again.
const FULL_DELAY: Duration = Duration::from_secs(10);
/// Sends `KEY` again to a slot not admitted after this long.
const KEY_RETRY: Duration = Duration::from_secs(5);
/// Announces our stream heads this long after our last message, so that
/// members who lost the end of a stream notice.
const HEADS_DELAY: Duration = Duration::from_secs(1);
/// Asks again for a gap that is not closing.
const RESEND_RETRY: Duration = Duration::from_secs(3);
/// Gives up on a gap that has not closed in this long.
const GAP_TIMEOUT: Duration = Duration::from_secs(15);
/// Gives up on a state transfer that makes no progress in this long.
const SYNC_STALL: Duration = Duration::from_secs(15);
/// Snapshot bytes sent and not yet acknowledged: the window starts here,
/// and adapts between the bounds (see [`Window`]).
const SYNC_WINDOW: u64 = 32 << 10;
const SYNC_WINDOW_MIN: u64 = 16 << 10;
const SYNC_WINDOW_MAX: u64 = 1 << 20;
/// The receiver of a snapshot acknowledges this often: every chunk, so
/// that the sender's delay measurements are fine-grained enough to keep the
/// queue it builds to tens of milliseconds even on a slow link.
const SYNC_ACK_EVERY: u64 = 16 << 10;
/// Bytes of messages resent in answer to one `RESEND`, at most.
const RESEND_BURST: usize = 128 << 10;
/// Streams whose position a member remembers.
const REMEMBERED_STREAMS: usize = 128;
/// Messages held per stream, waiting for a gap to close.
const HOLD_MESSAGES: usize = 1024;
const HOLD_BYTES: usize = 4 << 20;
/// Frontier entries kept from the last snapshot, for streams not met yet.
const REMEMBERED_STARTS: usize = 256;
/// Snapshot chunk: small, so that a voice frame or a command queued behind
/// one on a slow link waits little (4 KiB at 2 Mbit/s: 16 ms).
const BULK_CHUNK: usize = 4 << 10;
/// Snapshot bytes held that arrived before their SYNC_BEGIN, at most: more
/// than a provider's window lets it send unacknowledged.
const MAX_EARLY_BULK: usize = 2 << 20;
/// Sealed frame types, each with its own counter: video's are a layer and
/// whether a keyframe (GROUP.md §7.4).
const SEALED: [u8; 14] = [
    wire::CONTROL,
    wire::COMMAND,
    wire::VOICE,
    wire::DATAGRAM,
    wire::CHAT,
    wire::BULK,
    wire::VIDEO,
    wire::VIDEO + 1,
    wire::VIDEO + 2,
    wire::VIDEO + 3,
    wire::VIDEO + 4,
    wire::VIDEO + 5,
    wire::VIDEO + 6,
    wire::VIDEO + 7,
];

#[derive(Debug, Clone)]
pub struct Config {
    /// Describes this member to the others (a display name, say); at most
    /// [`wire::MAX_PROFILE_LEN`] bytes.
    pub profile: Vec<u8>,
    /// Messages of each stream kept for resending, at most.
    pub keep_messages: usize,
    /// Bytes of each stream kept for resending, at most.
    pub keep_bytes: usize,
    /// Largest snapshot accepted.
    pub max_snapshot: usize,
    /// How much queueing delay a state transfer we send may add to the
    /// path, roughly what voice and commands sharing it suffer meanwhile.
    /// Larger is faster on long, jittery paths, at their expense.
    pub transfer_delay: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            profile: Vec::new(),
            keep_messages: 8192,
            keep_bytes: 8 << 20,
            max_snapshot: 64 << 20,
            transfer_delay: Duration::from_millis(50),
        }
    }
}

/// The two broadcast streams of every member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Urgent and reliable; included in state transfer.
    Command = 0,
    /// Reliable, below commands and voice.
    Chat = 1,
}

impl Channel {
    fn frame_type(self) -> u8 {
        match self {
            Channel::Command => wire::COMMAND,
            Channel::Chat => wire::CHAT,
        }
    }

    fn from_byte(b: u8) -> Option<Channel> {
        match b {
            0 => Some(Channel::Command),
            1 => Some(Channel::Chat),
            _ => None,
        }
    }
}

/// What the machine asks of its driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Open a QUIC connection to this `https://` or `quic://` URL (see
    /// [`super::transport::target`]); any previous one is already closed.
    Connect(String),
    /// Send a message on the stream of its class (voice and video as a
    /// datagram where it fits); a [`Class::Voice`] frame may be dropped if
    /// the connection is backed up.
    Send(Vec<u8>, Class),
    /// Send the empty message (`ping`) on the command stream.
    Ping,
    /// Close the connection (code 1000) and forget it.
    Disconnect,
    /// Tell the application.
    Event(Event),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    State(State),
    /// A member was admitted: its identity is proven and its frames are
    /// accepted from now on.
    Joined {
        member: MemberId,
        profile: Vec<u8>,
    },
    Left {
        member: MemberId,
    },
    /// A message, in its stream's order (`direct: false`, including our
    /// own), or sent to us alone (`direct: true`).
    Message {
        from: MemberId,
        channel: Channel,
        data: Vec<u8>,
        direct: bool,
    },
    /// A voice frame; `seq` counts the sender's audio frames.
    Voice {
        from: MemberId,
        seq: u16,
        data: Vec<u8>,
    },
    Datagram {
        from: MemberId,
        data: Vec<u8>,
    },
    /// A whole video frame from `from`'s `track`, in order and decodable: a
    /// keyframe, or a frame that follows one with nothing lost in between.
    /// `layer` is the size it was sent in: frames of one track come from one
    /// layer at a time, a new one starting with a keyframe. `timestamp` is
    /// the sender's, in its units (milliseconds, say).
    Video {
        from: MemberId,
        track: u8,
        layer: u8,
        keyframe: bool,
        timestamp: u32,
        data: Vec<u8>,
    },
    /// Members need a keyframe of `layer` of our `track` (they lost some, or
    /// are moving to that layer): send one soon. At most every 500 ms per
    /// track and layer, however many ask.
    KeyframeRequest {
        from: MemberId,
        track: u8,
        layer: u8,
    },
    /// The relay says which of our video layers someone wants (bit n: layer
    /// n). Encode those, and no others; for a wanted layer we do not send,
    /// send the nearest below it, or else our lowest (GROUP.md §7.4).
    VideoDemand {
        layers: u8,
    },
    /// The relay says how much video our link takes now: layers up to
    /// `layer` (`None`: none, audio only, the link is too slow). It lowers
    /// this while we do not keep up and raises it again when we do.
    VideoLimit {
        layer: Option<u8>,
    },
    /// `count` messages of `from`'s stream were lost: it no longer had them,
    /// or a gap did not close in time.
    Missed {
        from: MemberId,
        channel: Channel,
        count: u64,
    },
    /// `from` asks for a snapshot. Take it *now*, before handling the next
    /// event: it must contain exactly the commands delivered so far. Then
    /// answer with [`Member::provide_sync`] or [`Member::decline_sync`].
    SyncRequest {
        from: MemberId,
        transfer: u32,
    },
    /// The snapshot asked for with [`Member::request_sync`]: replace the
    /// application state with it. The command messages that follow are
    /// exactly those it does not contain.
    Snapshot {
        from: MemberId,
        data: Vec<u8>,
    },
    /// The state transfer from `from` failed; held commands follow. Ask
    /// another member, or the same one again.
    SyncFailed {
        from: MemberId,
    },
    /// Another connection with our identity took our slot: most likely the
    /// same member running elsewhere. The machine retries in 30 to 60 s;
    /// call [`Member::leave`] to let the other one stay.
    Replaced,
    /// The group is full on this relay; the machine retries in a while.
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not in the group on the relay: connecting, or waiting to retry.
    Offline,
    /// In the group on the relay.
    Online,
    /// Left; no further output.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// Over [`wire::MAX_PAYLOAD_LEN`] (or [`wire::MAX_DATAGRAM_LEN`]).
    TooLarge,
    /// No such member is in the group right now.
    NotPresent,
    /// A state transfer is already under way, or there is none to answer.
    Busy,
    /// The machine has stopped.
    Closed,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SendError::TooLarge => "message too large",
            SendError::NotPresent => "no such member in the group",
            SendError::Busy => "a state transfer is already under way",
            SendError::Closed => "left the group",
        })
    }
}

impl std::error::Error for SendError {}

pub struct Member {
    group: Group,
    identity: Identity,
    me: MemberId,
    token: [u8; wire::TOKEN_LEN],
    config: Config,
    /// Names this run's streams.
    incarnation: u64,
    clock: u64,
    state: State,
    output: VecDeque<Output>,
    /// Connection attempts since the last stable connection.
    failures: u32,
    link: Link,
    /// Our command and chat streams.
    streams: [Outgoing; 2],
    /// Where we are in the streams of others.
    positions: Vec<Position>,
    /// A counter for least-recently-used eviction of positions.
    uses: u64,
    /// Positions to start unknown command streams at, from the last snapshot.
    starts: VecDeque<(u64, u64)>,
    /// Our state transfer, from request to snapshot.
    sync: Option<Sync>,
    /// Snapshots we are sending.
    transfers: Vec<Transfer>,
    next_transfer: u32,
    rtt: Option<u64>,
    /// When to announce our stream heads.
    heads_at: Option<u64>,
    /// Our recent video frames, for resending.
    video_out: video::Outgoing,
    /// Others' video being put back together, by slot.
    video_in: Vec<(u8, video::Incoming)>,
    /// The layer of each member's video we asked the relay for, by member:
    /// asked again whenever the member is (re)admitted.
    subscriptions: Vec<(MemberId, Option<u8>)>,
}

/// Messages we broadcast, kept for resending.
#[derive(Default)]
struct Outgoing {
    /// The id the next message gets.
    next: u64,
    /// The id of `messages[0]`.
    base: u64,
    messages: VecDeque<Vec<u8>>,
    bytes: usize,
}

/// Where we are in another member's stream.
struct Position {
    incarnation: u64,
    channel: Channel,
    owner: MemberId,
    /// The id of the next message to deliver.
    next: u64,
    /// One past the highest id known to exist.
    want: u64,
    /// Messages beyond `next`, waiting for the gap to close.
    held: BTreeMap<u64, Vec<u8>>,
    held_bytes: usize,
    gap: Option<Gap>,
    used: u64,
}

#[derive(Clone, Copy)]
struct Gap {
    /// When it opened, or last made progress.
    since: u64,
    /// When we last sent `RESEND` for it.
    asked_at: Option<u64>,
}

struct Sync {
    provider: MemberId,
    slot: u8,
    transfer: u32,
    progress_at: u64,
    /// From `SYNC_BEGIN`.
    length: Option<u64>,
    frontier: Vec<(u64, u64)>,
    data: Vec<u8>,
    acked: u64,
    /// Command messages delivered while the transfer runs: (incarnation,
    /// owner, id, data), in order.
    held: Vec<(u64, MemberId, u64, Vec<u8>)>,
}

struct Transfer {
    slot: u8,
    to: MemberId,
    transfer: u32,
    frontier: Vec<(u64, u64)>,
    /// The snapshot, once the application provides it.
    data: Option<Vec<u8>>,
    sent: u64,
    acked: u64,
    progress_at: u64,
    window: Window,
}

/// A delay-based window for state transfer (after LEDBAT, RFC 6817): the
/// transfer may add about [`Config::transfer_delay`] of queueing to the path,
/// and no more, since voice and commands wait in the same queues.
///
/// It grows by an eighth per round trip while the queue it causes is under
/// half the target, by a chunk per round trip below the target, and shrinks
/// by a quarter, at most once per round trip, above it. Growth by a fraction
/// of the window per round trip overshoots by at most that fraction of a
/// round trip of queueing before the delay shows; doubling would fill the
/// queue first and notice after. Chunks are paced over the round trip, not
/// sent in bursts. Voice and commands come first: on long, jittery paths
/// this makes state transfer slow rather than them late.
struct Window {
    bytes: u64,
    /// The queueing delay to stay under, in ms.
    target: u64,
    /// The shortest round trip seen, from sending a chunk to its acknowledgement.
    base: Option<u64>,
    /// The latest round trips: the smallest of them is the current delay,
    /// so that one late acknowledgement (jitter) does not count as a queue.
    recent: VecDeque<u64>,
    /// End offsets of chunks in flight and when they were sent.
    sent: VecDeque<(u64, u64)>,
    acked: u64,
    /// When the window last shrank.
    cut_at: Option<u64>,
    /// When the next chunk may go.
    next_at: f64,
}

impl Window {
    fn new(target: Duration) -> Window {
        Window {
            bytes: SYNC_WINDOW,
            target: ms(target).max(1),
            base: None,
            recent: VecDeque::new(),
            sent: VecDeque::new(),
            acked: 0,
            cut_at: None,
            next_at: 0.0,
        }
    }

    /// Whether a chunk may go at `now`, given what is in flight.
    fn open(&self, in_flight: u64, now: u64) -> bool {
        // Times are whole milliseconds: whatever is due within this one goes.
        in_flight < self.bytes && self.next_at < (now + 1) as f64
    }

    /// When pacing next allows a chunk, if the window does.
    fn due(&self, in_flight: u64) -> Option<u64> {
        (in_flight < self.bytes).then(|| self.next_at.floor() as u64)
    }

    fn sent(&mut self, end: u64, len: u64, now: u64) {
        self.sent.push_back((end, now));
        // At the window's rate: a window per (shortest) round trip.
        let rtt = self.base.unwrap_or(100).max(1) as f64;
        // Credit for a millisecond at most, so that a pause is not made up
        // for with a burst.
        self.next_at = self.next_at.max(now as f64 - 1.0) + len as f64 * rtt / self.bytes as f64;
    }

    /// An acknowledgement of everything below `received`, at `now`.
    fn acked(&mut self, received: u64, now: u64) {
        let newly = received.saturating_sub(self.acked);
        self.acked = self.acked.max(received);
        let mut rtt = None;
        while let Some(&(end, at)) = self.sent.front()
            && end <= received
        {
            rtt = Some(now.saturating_sub(at));
            self.sent.pop_front();
        }
        let Some(rtt) = rtt else { return };
        let base = self.base.map_or(rtt, |b| b.min(rtt));
        self.base = Some(base);
        self.recent.push_back(rtt);
        if self.recent.len() > 3 {
            self.recent.pop_front();
        }
        let queued = self.recent.iter().min().unwrap() - base;
        if queued > self.target {
            if self.cut_at.is_none_or(|at| now >= at + base) {
                self.cut_at = Some(now);
                self.bytes = self.bytes * 3 / 4;
            }
        } else if queued < self.target / 2 {
            self.bytes += newly / 8;
        } else {
            self.bytes += newly * BULK_CHUNK as u64 / self.bytes;
        }
        self.bytes = self.bytes.clamp(SYNC_WINDOW_MIN, SYNC_WINDOW_MAX);
    }
}

enum Link {
    Idle { retry_at: u64 },
    Connecting { deadline: u64 },
    Open(Open),
    Done,
}

struct Open {
    phase: Phase,
    joined_at: Option<u64>,
    receive_deadline: u64,
    next_ping: u64,
    ping_at: Option<u64>,
}

enum Phase {
    /// Waiting for `HELLO` (`sent_join: false`), then `WELCOME`.
    Joining {
        deadline: u64,
        sent_join: bool,
    },
    Joined(Box<Joined>),
}

struct Joined {
    slot: u8,
    key: Key,
    challenge: Challenge,
    counters: [u32; SEALED.len()],
    remotes: [Option<Remote>; MAX_SLOTS],
}

struct Remote {
    /// When the slot's connection appeared.
    since: u64,
    /// When we last sent it our `KEY`.
    key_sent_at: u64,
    state: Admission,
    /// The counters accepted per sealed type.
    counters: [Replay; SEALED.len()],
}

/// The counters of one sealed type accepted from one connection.
#[derive(Debug, Clone, Copy, Default)]
struct Replay {
    /// The highest.
    last: Option<u32>,
    /// Bit `i`: whether `last - i` was accepted.
    seen: u64,
}

impl Replay {
    /// Accepts `counter` if it is above every one accepted so far, or, for
    /// a type that may arrive out of order (`window`), if it is one of the
    /// 63 below the highest and not accepted yet.
    fn accept(&mut self, counter: u32, window: bool) -> bool {
        let Some(last) = self.last else {
            *self = Replay { last: Some(counter), seen: 1 };
            return true;
        };
        if counter > last {
            let shift = counter - last;
            self.seen = if shift >= 64 { 0 } else { self.seen << shift } | 1;
            self.last = Some(counter);
            return true;
        }
        let behind = last - counter;
        if !window || behind >= 64 || self.seen & (1 << behind) != 0 {
            return false;
        }
        self.seen |= 1 << behind;
        true
    }
}

#[allow(clippy::large_enum_variant)]
enum Admission {
    /// Nothing heard from it yet.
    Unknown,
    /// Its `KEY` arrived; waiting for its `CONFIRM`.
    Pending(Announce),
    Admitted(Announce),
}

impl Member {
    /// Starts the machine; the first output asks the driver to connect.
    pub fn new(group: Group, identity: Identity, config: Config, now: u64) -> Result<Member, Error> {
        if config.profile.len() > wire::MAX_PROFILE_LEN {
            return Err(Error::InvalidProfile);
        }
        let mut incarnation = 0;
        while incarnation == 0 {
            incarnation = u64::from_le_bytes(crate::random());
        }
        let mut member = Member {
            me: identity.id(),
            token: identity.token(&group.id()),
            group,
            identity,
            config,
            incarnation,
            clock: now,
            state: State::Offline,
            output: VecDeque::new(),
            failures: 0,
            link: Link::Idle { retry_at: now },
            streams: Default::default(),
            positions: Vec::new(),
            uses: 0,
            starts: VecDeque::new(),
            sync: None,
            transfers: Vec::new(),
            next_transfer: u32::from_le_bytes(crate::random()),
            rtt: None,
            heads_at: None,
            video_out: video::Outgoing::default(),
            video_in: Vec::new(),
            subscriptions: Vec::new(),
        };
        member.connect(now);
        Ok(member)
    }

    pub fn group(&self) -> &Group {
        &self.group
    }

    /// Our member ID.
    pub fn id(&self) -> MemberId {
        self.me
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The admitted members (not us) and their profiles.
    pub fn members(&self) -> Vec<(MemberId, Vec<u8>)> {
        let Some(joined) = self.joined() else { return Vec::new() };
        joined
            .remotes
            .iter()
            .flatten()
            .filter_map(|r| match &r.state {
                Admission::Admitted(a) => Some((a.identity, a.profile.clone())),
                _ => None,
            })
            .collect()
    }

    /// The latest round trip to the relay (`ping` to `pong`), in milliseconds.
    pub fn rtt(&self) -> Option<u64> {
        self.rtt
    }

    pub fn poll_output(&mut self) -> Option<Output> {
        self.output.pop_front()
    }

    /// When the driver must next call [`Member::handle_timeout`].
    pub fn poll_timeout(&self) -> Option<u64> {
        match &self.link {
            Link::Idle { retry_at } => Some(*retry_at),
            Link::Connecting { deadline } => Some(*deadline),
            Link::Open(open) => {
                let mut at = open.receive_deadline.min(open.next_ping);
                match &open.phase {
                    Phase::Joining { deadline, .. } => at = at.min(*deadline),
                    Phase::Joined(joined) => {
                        for remote in joined.remotes.iter().flatten() {
                            if !matches!(remote.state, Admission::Admitted(_)) {
                                at = at.min(remote.key_sent_at.max(remote.since) + ms(KEY_RETRY));
                            }
                        }
                        for pos in &self.positions {
                            if let Some(gap) = pos.gap
                                && self.slot_of(pos.incarnation).is_some()
                            {
                                let ask = gap.asked_at.map_or(self.clock, |t| t + ms(RESEND_RETRY));
                                at = at.min(ask).min(gap.since + ms(GAP_TIMEOUT));
                            }
                        }
                        if let Some(sync) = &self.sync {
                            at = at.min(sync.progress_at + ms(SYNC_STALL));
                        }
                        if let Some(heads) = self.heads_at {
                            at = at.min(heads);
                        }
                        let rtt = self.member_rtt();
                        for (_, v) in &self.video_in {
                            if let Some(due) = v.deadline(rtt) {
                                at = at.min(due);
                            }
                        }
                        for t in &self.transfers {
                            at = at.min(t.progress_at + ms(SYNC_STALL));
                            if let Some(data) = &t.data
                                && t.sent < data.len() as u64
                                && let Some(due) = t.window.due(t.sent - t.acked)
                            {
                                at = at.min(due);
                            }
                        }
                    }
                }
                Some(at)
            }
            Link::Done => None,
        }
    }

    pub fn handle_timeout(&mut self, now: u64) {
        self.clock = now;
        match &mut self.link {
            Link::Idle { retry_at } if now >= *retry_at => self.connect(now),
            Link::Connecting { deadline } if now >= *deadline => self.reconnect(now),
            Link::Open(open) => {
                let expired = match &open.phase {
                    Phase::Joining { deadline, .. } => now >= *deadline,
                    Phase::Joined(_) => false,
                };
                if expired || now >= open.receive_deadline {
                    return self.reconnect(now);
                }
                if now >= open.next_ping {
                    open.next_ping = now + ms(wire::PING_INTERVAL);
                    open.ping_at = Some(now);
                    self.output.push_back(Output::Ping);
                }
                if self.joined().is_some() {
                    if self.heads_at.is_some_and(|t| now >= t) {
                        self.heads_at = None;
                        let mut data = vec![control::HEADS];
                        data.extend_from_slice(&self.streams[0].next.to_be_bytes());
                        data.extend_from_slice(&self.streams[1].next.to_be_bytes());
                        self.seal_to(wire::CONTROL, BROADCAST, &data);
                    }
                    self.retry_keys(now);
                    self.check_gaps(now);
                    self.check_transfers(now);
                    self.pump_transfers();
                    self.tick_video(now);
                }
            }
            _ => {}
        }
    }

    /// The socket requested by [`Output::Connect`] is open.
    pub fn handle_open(&mut self, now: u64) {
        self.clock = now;
        if let Link::Connecting { .. } = self.link {
            self.link = Link::Open(Open {
                phase: Phase::Joining { deadline: now + ms(wire::JOIN_TIMEOUT), sent_join: false },
                joined_at: None,
                receive_deadline: now + ms(wire::RECEIVE_TIMEOUT),
                next_ping: now + ms(wire::PING_INTERVAL),
                ping_at: None,
            });
        }
    }

    /// The socket closed or failed, with the close code if one was received.
    pub fn handle_close(&mut self, now: u64, code: Option<u16>) {
        self.clock = now;
        if !matches!(self.link, Link::Connecting { .. } | Link::Open(_)) {
            return;
        }
        self.drop_socket(now, false);
        let delay = match code {
            Some(close::REPLACED) => {
                self.output.push_back(Output::Event(Event::Replaced));
                ms(REPLACED_DELAY) + random_below(ms(REPLACED_DELAY))
            }
            Some(close::FULL) => {
                self.output.push_back(Output::Event(Event::Full));
                self.backoff().max(ms(FULL_DELAY))
            }
            _ => self.backoff(),
        };
        self.link = Link::Idle { retry_at: now + delay };
    }

    pub fn handle_text(&mut self, now: u64, text: &str) {
        self.clock = now;
        if let Link::Open(open) = &mut self.link {
            open.receive_deadline = now + ms(wire::RECEIVE_TIMEOUT);
            if text == wire::PONG
                && let Some(sent) = open.ping_at.take()
            {
                self.rtt = Some(now - sent);
            }
        }
    }

    pub fn handle_binary(&mut self, now: u64, frame: &[u8]) {
        self.clock = now;
        let Link::Open(open) = &mut self.link else { return };
        open.receive_deadline = now + ms(wire::RECEIVE_TIMEOUT);
        let Some(&kind) = frame.first() else { return };
        match &mut open.phase {
            Phase::Joining { sent_join: false, .. } if kind == wire::HELLO => {
                let Ok(nonce) = frame[1..].try_into() else { return self.reconnect(now) };
                let join = self.group.join_frame(nonce, &self.token);
                self.output.push_back(Output::Send(join.to_vec(), Class::Command));
                open.phase = Phase::Joining { deadline: now + ms(wire::JOIN_TIMEOUT), sent_join: true };
            }
            Phase::Joining { sent_join: true, .. } if kind == wire::WELCOME => {
                let Some((slot, present)) = parse_welcome(frame) else { return self.reconnect(now) };
                let mut remotes: [Option<Remote>; MAX_SLOTS] = Default::default();
                for (i, remote) in remotes.iter_mut().enumerate() {
                    if present & (1 << i) != 0 && i != slot as usize {
                        *remote = Some(Remote::new(now));
                    }
                }
                let joined = Joined {
                    slot,
                    key: zeroize::Zeroizing::new(crate::random()),
                    challenge: crate::random(),
                    counters: [0; SEALED.len()],
                    remotes,
                };
                open.phase = Phase::Joined(Box::new(joined));
                open.joined_at = Some(now);
                self.set_state(State::Online);
                self.send_key(BROADCAST, [0; CHALLENGE_LEN]);
            }
            // Peer frames can overtake WELCOME on a transport with a
            // stream per class (GROUP.md §3.6); they are not for us yet.
            Phase::Joining { sent_join: true, .. } if kind < 0x80 => {}
            Phase::Joining { .. } => self.reconnect(now),
            Phase::Joined(_) => match kind {
                wire::MEMBER => self.handle_member(now, frame),
                wire::DEMAND => {
                    if let &[_, layers, ..] = frame {
                        self.emit(Event::VideoDemand { layers });
                    }
                }
                wire::LIMIT => {
                    if let &[_, layer, ..] = frame {
                        self.handle_limit((layer != wire::NO_VIDEO).then_some(layer));
                    }
                }
                wire::KEY => self.handle_key(frame),
                kind if kind < 0x80 => self.handle_sealed(frame),
                // Anything else is from a newer protocol revision: ignore.
                _ => {}
            },
        }
    }

    /// Broadcasts a message on `channel` and returns its id. It is delivered
    /// to us at once (as an [`Event::Message`] from ourselves) and to every
    /// member present, now or, if we are offline, when we are back.
    pub fn send(&mut self, channel: Channel, data: &[u8]) -> Result<u64, SendError> {
        if self.state == State::Closed {
            return Err(SendError::Closed);
        }
        if data.len() > wire::MAX_PAYLOAD_LEN {
            return Err(SendError::TooLarge);
        }
        let (keep_messages, keep_bytes) = (self.config.keep_messages.max(1), self.config.keep_bytes);
        let stream = &mut self.streams[channel as usize];
        let id = stream.next;
        stream.next += 1;
        stream.messages.push_back(data.to_vec());
        stream.bytes += data.len();
        while stream.messages.len() > keep_messages || (stream.bytes > keep_bytes && stream.messages.len() > 1) {
            let old = stream.messages.pop_front().unwrap();
            stream.bytes -= old.len();
            stream.base += 1;
        }
        self.deliver(channel, self.me, self.incarnation, id, data.to_vec());
        if self.joined().is_some() {
            self.seal_to(channel.frame_type(), BROADCAST, &stream_message(id, data));
            self.heads_at = Some(self.clock + ms(HEADS_DELAY));
        }
        Ok(id)
    }

    /// Sends a message to one member, outside the streams: in order with
    /// `channel`'s messages, but not resent if the connection drops.
    pub fn send_to(&mut self, member: MemberId, channel: Channel, data: &[u8]) -> Result<(), SendError> {
        if data.len() > wire::MAX_PAYLOAD_LEN {
            return Err(SendError::TooLarge);
        }
        let slot = self.slot_of_member(member).ok_or(SendError::NotPresent)?;
        self.seal_to(channel.frame_type(), slot, &[&[message::DIRECT], data].concat());
        Ok(())
    }

    /// Sends a voice frame to everyone. `seq` counts audio frames of fixed
    /// duration, silent ones included. Returns false if it was not sent.
    pub fn send_voice(&mut self, seq: u16, data: &[u8]) -> bool {
        if data.len() > wire::MAX_DATAGRAM_LEN || self.joined().is_none() {
            return false;
        }
        self.seal_to(wire::VOICE, BROADCAST, &[&seq.to_be_bytes()[..], data].concat())
    }

    /// Sends data that is dropped rather than delayed, to one member or to
    /// everyone. Returns false if it was not sent.
    pub fn send_datagram(&mut self, to: Option<MemberId>, data: &[u8]) -> bool {
        if data.len() > wire::MAX_DATAGRAM_LEN {
            return false;
        }
        let slot = match to {
            Some(member) => match self.slot_of_member(member) {
                Some(slot) => slot,
                None => return false,
            },
            None if self.joined().is_some() => BROADCAST,
            None => return false,
        };
        self.seal_to(wire::DATAGRAM, slot, data)
    }

    /// Sends a video frame (at most [`wire::MAX_VIDEO_FRAME`] bytes) of
    /// `layer` (0 to 3, from the smallest picture up) of `track` to everyone,
    /// cut into fragments that each fit a datagram (GROUP.md §7.4). Frames are
    /// numbered per track and layer; receivers deliver them whole and in
    /// order, ask for lost fragments once, and drop what follows a frame lost
    /// for good until a keyframe ([`Event::KeyframeRequest`] asks for one).
    /// The relay forwards each member the layer it subscribed to
    /// ([`Member::subscribe`]); [`Event::VideoDemand`] says which are wanted.
    /// Returns false if it was not sent.
    pub fn send_video(&mut self, track: u8, layer: u8, keyframe: bool, timestamp: u32, data: &[u8]) -> bool {
        if self.joined().is_none() || layer >= wire::VIDEO_LAYERS {
            return false;
        }
        let Some(fragments) = self.video_out.send(self.clock, track, layer, keyframe, timestamp, data) else {
            return false;
        };
        for fragment in fragments {
            if !self.seal_to(wire::video_type(layer, keyframe), BROADCAST, &fragment) {
                return false;
            }
        }
        true
    }

    /// Asks the relay for `layer` of `member`'s video (`None`: none of it),
    /// from now on and whenever `member` is back after a reconnection. The
    /// relay forwards the nearest layer the member sends, moving at a
    /// keyframe; until a member says, the relay forwards layer 0 or the
    /// nearest. Asks `member` for a keyframe of that layer, to move soon.
    pub fn subscribe(&mut self, member: MemberId, layer: Option<u8>) -> Result<(), SendError> {
        if self.state == State::Closed {
            return Err(SendError::Closed);
        }
        if layer.is_some_and(|l| l >= wire::VIDEO_LAYERS) {
            return Err(SendError::TooLarge);
        }
        match self.subscriptions.iter_mut().find(|(m, _)| *m == member) {
            Some((_, l)) => *l = layer,
            None => self.subscriptions.push((member, layer)),
        }
        let Some(slot) = self.slot_of_member(member) else { return Ok(()) };
        self.send_subscription(slot, layer);
        if let Some(layer) = layer {
            let tracks: Vec<u8> =
                self.video_in.iter().filter(|(s, _)| *s == slot).flat_map(|(_, v)| v.tracks()).collect();
            for track in tracks {
                self.seal_to(wire::CONTROL, slot, &[control::KEYFRAME_REQUEST, track, layer]);
            }
        }
        Ok(())
    }

    /// The relay moved what our link takes: ask each member whose video we
    /// get for a keyframe of the layer we will get now, to move without
    /// waiting for its next one.
    fn handle_limit(&mut self, limit: Option<u8>) {
        self.emit(Event::VideoLimit { layer: limit });
        let Some(limit) = limit else { return };
        let mut asks = Vec::new();
        for (slot, v) in &self.video_in {
            let Some(member) = self.admitted(*slot).map(|a| a.identity) else { continue };
            let wanted = self.subscriptions.iter().find(|(m, _)| *m == member).map_or(Some(0), |&(_, l)| l);
            if let Some(wanted) = wanted {
                asks.extend(v.tracks().into_iter().map(|track| (*slot, track, wanted.min(limit))));
            }
        }
        for (slot, track, layer) in asks {
            self.seal_to(wire::CONTROL, slot, &[control::KEYFRAME_REQUEST, track, layer]);
        }
    }

    fn send_subscription(&mut self, slot: u8, layer: Option<u8>) {
        if self.joined().is_some() {
            self.output.push_back(Output::Send(wire::subscribe(slot, layer).to_vec(), Class::Command));
        }
    }

    /// Asks `from` for a snapshot. Command messages are held from now until
    /// [`Event::Snapshot`] (or [`Event::SyncFailed`]).
    pub fn request_sync(&mut self, from: MemberId) -> Result<(), SendError> {
        if self.sync.is_some() {
            return Err(SendError::Busy);
        }
        let slot = self.slot_of_member(from).ok_or(SendError::NotPresent)?;
        let transfer = self.next_transfer;
        self.next_transfer = self.next_transfer.wrapping_add(1);
        self.sync = Some(Sync {
            provider: from,
            slot,
            transfer,
            progress_at: self.clock,
            length: None,
            frontier: Vec::new(),
            data: Vec::new(),
            acked: 0,
            held: Vec::new(),
        });
        self.seal_to(wire::CONTROL, slot, &[&[control::SYNC_REQUEST][..], &transfer.to_be_bytes()].concat());
        Ok(())
    }

    /// Answers [`Event::SyncRequest`] with the snapshot taken when it arrived.
    pub fn provide_sync(&mut self, to: MemberId, transfer: u32, snapshot: Vec<u8>) -> Result<(), SendError> {
        let Some(t) = self.transfers.iter_mut().find(|t| t.to == to && t.transfer == transfer && t.data.is_none())
        else {
            return Err(SendError::Busy);
        };
        let mut begin = vec![control::SYNC_BEGIN];
        begin.extend_from_slice(&transfer.to_be_bytes());
        begin.extend_from_slice(&(snapshot.len() as u64).to_be_bytes());
        begin.push(t.frontier.len() as u8);
        for (incarnation, next) in &t.frontier {
            begin.extend_from_slice(&incarnation.to_be_bytes());
            begin.extend_from_slice(&next.to_be_bytes());
        }
        t.data = Some(snapshot);
        t.progress_at = self.clock;
        let slot = t.slot;
        self.seal_to(wire::CONTROL, slot, &begin);
        self.pump_transfers();
        Ok(())
    }

    /// Refuses [`Event::SyncRequest`].
    pub fn decline_sync(&mut self, to: MemberId, transfer: u32) {
        if let Some(i) = self.transfers.iter().position(|t| t.to == to && t.transfer == transfer) {
            let t = self.transfers.remove(i);
            self.seal_to(wire::CONTROL, t.slot, &abort(transfer));
        }
    }

    /// Leaves the group and stops.
    pub fn leave(&mut self) {
        if matches!(self.link, Link::Done) {
            return;
        }
        if matches!(self.link, Link::Connecting { .. } | Link::Open(_)) {
            self.output.push_back(Output::Disconnect);
        }
        self.link = Link::Done;
        self.set_state(State::Closed);
    }

    // Connection lifecycle.

    fn connect(&mut self, now: u64) {
        self.output.push_back(Output::Connect(self.group.endpoint()));
        self.link = Link::Connecting { deadline: now + ms(CONNECT_TIMEOUT) };
    }

    fn reconnect(&mut self, now: u64) {
        self.drop_socket(now, true);
        let delay = self.backoff();
        self.link = Link::Idle { retry_at: now + delay };
    }

    /// Forgets the socket and everything tied to it: slots, keys, the
    /// members we had admitted, transfers under way.
    fn drop_socket(&mut self, now: u64, close: bool) {
        let link = std::mem::replace(&mut self.link, Link::Idle { retry_at: now });
        if close && matches!(link, Link::Connecting { .. } | Link::Open(_)) {
            self.output.push_back(Output::Disconnect);
        }
        if let Link::Open(open) = link {
            if open.joined_at.is_some_and(|joined| now.saturating_sub(joined) >= ms(STABLE)) {
                self.failures = 0;
            }
            if let Phase::Joined(joined) = open.phase {
                for remote in joined.remotes.into_iter().flatten() {
                    if let Admission::Admitted(a) = remote.state {
                        self.emit(Event::Left { member: a.identity });
                    }
                }
            }
        }
        self.fail_sync();
        self.transfers.clear();
        self.video_in.clear();
        self.heads_at = None; // the next KEY carries them
        for pos in &mut self.positions {
            if let Some(gap) = &mut pos.gap {
                gap.asked_at = None;
            }
        }
        self.set_state(State::Offline);
    }

    /// The delay before the next attempt: between half and all of
    /// `min(10 s, 250 ms × 2^failures)`.
    fn backoff(&mut self) -> u64 {
        let ceiling = ms(BACKOFF_BASE.saturating_mul(1 << self.failures.min(16)).min(BACKOFF_MAX));
        self.failures += 1;
        ceiling / 2 + random_below(ceiling / 2 + 1)
    }

    fn set_state(&mut self, state: State) {
        if self.state != state {
            self.state = state;
            self.emit(Event::State(state));
        }
    }

    fn emit(&mut self, event: Event) {
        self.output.push_back(Output::Event(event));
    }

    fn joined(&self) -> Option<&Joined> {
        match &self.link {
            Link::Open(Open { phase: Phase::Joined(joined), .. }) => Some(joined),
            _ => None,
        }
    }

    fn joined_mut(&mut self) -> Option<&mut Joined> {
        match &mut self.link {
            Link::Open(Open { phase: Phase::Joined(joined), .. }) => Some(joined),
            _ => None,
        }
    }

    fn admitted(&self, slot: u8) -> Option<&Announce> {
        match &self.joined()?.remotes.get(slot as usize)?.as_ref()?.state {
            Admission::Admitted(a) => Some(a),
            _ => None,
        }
    }

    fn slot_of_member(&self, member: MemberId) -> Option<u8> {
        (0..MAX_SLOTS as u8).find(|&s| self.admitted(s).is_some_and(|a| a.identity == member))
    }

    /// The slot of the admitted member whose stream this is.
    fn slot_of(&self, incarnation: u64) -> Option<u8> {
        (0..MAX_SLOTS as u8).find(|&s| self.admitted(s).is_some_and(|a| a.incarnation == incarnation))
    }

    // Admission (GROUP.md §4.2).

    fn send_key(&mut self, to: u8, echo: Challenge) {
        let now = self.clock;
        let next = [self.streams[0].next, self.streams[1].next];
        // Borrow the link alone, leaving the other fields free.
        let Link::Open(Open { phase: Phase::Joined(joined), .. }) = &mut self.link else { return };
        let announce = Announce {
            identity: self.identity.id(),
            key: joined.key.clone(),
            challenge: joined.challenge,
            echo,
            incarnation: self.incarnation,
            next,
            profile: self.config.profile.clone(),
        };
        let frame = seal::seal_key(&self.group, &self.identity, joined.slot, to, &announce);
        for (slot, remote) in joined.remotes.iter_mut().enumerate() {
            if let Some(remote) = remote
                && (to == BROADCAST || to as usize == slot)
            {
                remote.key_sent_at = now;
            }
        }
        self.output.push_back(Output::Send(frame, Class::Command));
    }

    fn handle_member(&mut self, now: u64, frame: &[u8]) {
        let &[_, slot, present] = frame else { return };
        let Some(joined) = self.joined_mut() else { return };
        if slot as usize >= MAX_SLOTS || slot == joined.slot {
            return;
        }
        let old = std::mem::replace(&mut joined.remotes[slot as usize], (present == 1).then(|| Remote::new(now)));
        self.video_in.retain(|(s, _)| *s != slot);
        if let Some(Remote { state: Admission::Admitted(a), .. }) = old {
            self.left(slot, a.identity);
        }
    }

    /// A member we had admitted is gone from `slot`.
    fn left(&mut self, slot: u8, member: MemberId) {
        self.emit(Event::Left { member });
        if self.sync.as_ref().is_some_and(|s| s.slot == slot) {
            self.fail_sync();
        }
        self.transfers.retain(|t| t.slot != slot);
    }

    fn handle_key(&mut self, frame: &[u8]) {
        let Some(announce) = seal::open_key(&self.group, frame) else { return };
        let from = frame[1];
        if announce.identity == self.me {
            return;
        }
        let Some(joined) = self.joined_mut() else { return };
        let mine = joined.challenge;
        let Some(remote) = joined.remotes.get_mut(from as usize).and_then(Option::as_mut) else { return };
        let admitted = match &remote.state {
            Admission::Admitted(a) => Some(a.challenge),
            _ => None,
        };
        if announce.echo == [0; CHALLENGE_LEN] {
            // Their first word to us, or a retry: answer it so they can
            // admit us. Unless already admitted, wait for their CONFIRM.
            if admitted.is_none() {
                remote.state = Admission::Pending(announce.clone());
                remote.counters = Default::default();
            }
            self.send_key(from, announce.challenge);
        } else if announce.echo == mine {
            match admitted {
                // They have not seen our CONFIRM yet.
                Some(echo) => self.send_confirm(from, echo),
                // Proves the sender saw our fresh challenge: admit it.
                None => {
                    remote.counters = Default::default();
                    let (echo, next) = (announce.challenge, announce.next);
                    self.admit(from, announce, next);
                    self.send_confirm(from, echo);
                }
            }
        }
    }

    fn send_confirm(&mut self, to: u8, echo: Challenge) {
        let mut data = vec![control::CONFIRM];
        data.extend_from_slice(&echo);
        data.extend_from_slice(&self.streams[0].next.to_be_bytes());
        data.extend_from_slice(&self.streams[1].next.to_be_bytes());
        self.seal_to(wire::CONTROL, to, &data);
    }

    /// Admits the member in `slot`, whose streams are at `next` now.
    fn admit(&mut self, slot: u8, announce: Announce, next: [u64; 2]) {
        // One identity, one slot: an older one is stale.
        if let Some(other) = self.slot_of_member(announce.identity).filter(|&s| s != slot) {
            if let Some(joined) = self.joined_mut() {
                joined.remotes[other as usize] = None;
            }
            self.left(other, announce.identity);
        }
        let (identity, incarnation, first) = (announce.identity, announce.incarnation, announce.next);
        let profile = announce.profile.clone();
        let Some(remote) = self.joined_mut().and_then(|j| j.remotes[slot as usize].as_mut()) else { return };
        remote.state = Admission::Admitted(announce);
        self.emit(Event::Joined { member: identity, profile });
        if let Some(&(_, layer)) = self.subscriptions.iter().find(|(m, _)| *m == identity) {
            self.send_subscription(slot, layer);
        }
        for channel in [Channel::Command, Channel::Chat] {
            let start = match channel {
                Channel::Command => self.starts.iter().find(|(i, _)| *i == incarnation).map(|(_, f)| *f),
                Channel::Chat => None,
            };
            let i = self.position(incarnation, channel, identity, start.unwrap_or(first[channel as usize]));
            self.announced(i, next[channel as usize]);
        }
    }

    fn retry_keys(&mut self, now: u64) {
        let Some(joined) = self.joined() else { return };
        let due: Vec<(u8, Challenge)> = joined
            .remotes
            .iter()
            .enumerate()
            .filter_map(|(slot, r)| {
                let r = r.as_ref()?;
                let echo = match &r.state {
                    Admission::Unknown => [0; CHALLENGE_LEN],
                    Admission::Pending(a) => a.challenge,
                    Admission::Admitted(_) => return None,
                };
                (now >= r.key_sent_at.max(r.since) + ms(KEY_RETRY)).then_some((slot as u8, echo))
            })
            .collect();
        for (slot, echo) in due {
            self.send_key(slot, echo);
        }
    }

    // Sealed frames (GROUP.md §4.3).

    /// Seals and queues a frame; false if there is no connection to send it on.
    fn seal_to(&mut self, kind: u8, to: u8, plaintext: &[u8]) -> bool {
        let index = SEALED.iter().position(|&k| k == kind).expect("a sealed frame type");
        let now = self.clock;
        let Some(joined) = self.joined_mut() else { return false };
        let counter = joined.counters[index];
        if counter == u32::MAX {
            // Never reuse a nonce: a new connection brings a new key.
            self.reconnect(now);
            return false;
        }
        joined.counters[index] += 1;
        let frame = seal::seal(&joined.key, kind, joined.slot, to, counter, plaintext);
        self.output.push_back(Output::Send(frame, Class::of(kind)));
        true
    }

    fn handle_sealed(&mut self, frame: &[u8]) {
        let [kind, from, ..] = *frame else { return };
        let Some(index) = SEALED.iter().position(|&k| k == kind) else { return };
        let Some(joined) = self.joined_mut() else { return };
        let Some(remote) = joined.remotes.get_mut(from as usize).and_then(Option::as_mut) else { return };
        let (key, admitted) = match &remote.state {
            Admission::Unknown => return,
            Admission::Pending(a) => (&a.key, false),
            Admission::Admitted(a) => (&a.key, true),
        };
        let Some((counter, plaintext)) = seal::open(key, frame) else { return };
        // Real-time frames may overtake each other (GROUP.md §4.3).
        if !remote.counters[index].accept(counter, Class::of(kind).is_realtime()) {
            return; // replayed
        }
        if !admitted {
            // Until admitted, only its CONFIRM counts.
            if kind == wire::CONTROL && plaintext.first() == Some(&control::CONFIRM) {
                self.handle_confirm(from, &plaintext[1..]);
            }
            return;
        }
        let sender = self.admitted(from).map(|a| a.identity).expect("admitted");
        match kind {
            wire::CONTROL | wire::COMMAND | wire::CHAT => {
                let Some((&head, data)) = plaintext.split_first() else { return };
                match kind {
                    wire::CONTROL => self.handle_control(from, head, data),
                    wire::COMMAND => self.handle_channel(from, Channel::Command, head, data),
                    _ => self.handle_channel(from, Channel::Chat, head, data),
                }
            }
            wire::VOICE => {
                if let Some(seq) = plaintext.get(..2) {
                    let seq = u16::from_be_bytes(seq.try_into().unwrap());
                    self.emit(Event::Voice { from: sender, seq, data: plaintext[2..].to_vec() });
                }
            }
            wire::DATAGRAM => self.emit(Event::Datagram { from: sender, data: plaintext }),
            kind if let Some((layer, keyframe)) = wire::video_layer(kind) => {
                // The relay switches layers by the type's keyframe bit: it must not lie.
                if video::parse(&plaintext).is_none_or(|f| f.keyframe != keyframe) {
                    return;
                }
                let now = self.clock;
                let mut actions = Vec::new();
                match self.video_in.iter_mut().find(|(s, _)| *s == from) {
                    Some((_, v)) => v.push(now, layer, &plaintext, &mut actions),
                    None => {
                        let mut v = video::Incoming::default();
                        v.push(now, layer, &plaintext, &mut actions);
                        self.video_in.push((from, v));
                    }
                }
                self.video_actions(from, sender, actions);
            }
            wire::BULK => self.handle_bulk(from, &plaintext),
            _ => {}
        }
    }

    fn handle_confirm(&mut self, from: u8, data: &[u8]) {
        let Some(joined) = self.joined() else { return };
        if data.len() < CHALLENGE_LEN + 16 || data[..CHALLENGE_LEN] != joined.challenge {
            return;
        }
        let next = [u64_at(data, CHALLENGE_LEN), u64_at(data, CHALLENGE_LEN + 8)];
        match &joined.remotes[from as usize].as_ref().map(|r| &r.state) {
            Some(Admission::Pending(announce)) => {
                let announce = announce.clone();
                self.admit(from, announce, next);
            }
            Some(Admission::Admitted(a)) => {
                let incarnation = a.incarnation;
                for channel in [Channel::Command, Channel::Chat] {
                    if let Some(i) = self.find_position(incarnation, channel) {
                        self.announced(i, next[channel as usize]);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_control(&mut self, from: u8, kind: u8, data: &[u8]) {
        match kind {
            control::CONFIRM => self.handle_confirm(from, data),
            control::HEADS if data.len() >= 16 => {
                let Some(incarnation) = self.admitted(from).map(|a| a.incarnation) else { return };
                for channel in [Channel::Command, Channel::Chat] {
                    if let Some(i) = self.find_position(incarnation, channel) {
                        self.announced(i, u64_at(data, channel as usize * 8));
                    }
                }
            }
            control::RESEND if data.len() >= 17 => {
                if let Some(channel) = Channel::from_byte(data[0])
                    && u64_at(data, 1) == self.incarnation
                {
                    self.resend(from, channel, u64_at(data, 9));
                }
            }
            control::SYNC_REQUEST if data.len() >= 4 => self.handle_sync_request(from, u32_at(data, 0)),
            control::SYNC_BEGIN if data.len() >= 13 => self.handle_sync_begin(from, data),
            control::SYNC_ACK if data.len() >= 12 => {
                let (transfer, received) = (u32_at(data, 0), u64_at(data, 4));
                if let Some(t) = self.transfers.iter_mut().find(|t| t.slot == from && t.transfer == transfer) {
                    t.acked = t.acked.max(received.min(t.sent));
                    t.progress_at = self.clock;
                    t.window.acked(received, self.clock);
                    self.pump_transfers();
                }
            }
            control::VIDEO_NACK if data.len() >= 7 && data[1] < wire::VIDEO_LAYERS => {
                let (track, layer, frame) = (data[0], data[1], u32_at(data, 2));
                for fragment in self.video_out.resend(self.clock, track, layer, frame, &data[6..]) {
                    let keyframe = video::parse(&fragment).is_some_and(|f| f.keyframe);
                    self.seal_to(wire::video_type(layer, keyframe), from, &fragment);
                }
            }
            control::KEYFRAME_REQUEST if data.len() >= 2 && data[1] < wire::VIDEO_LAYERS => {
                let (track, layer) = (data[0], data[1]);
                if let Some(sender) = self.admitted(from).map(|a| a.identity)
                    && self.video_out.keyframe_wanted(self.clock, track, layer)
                {
                    self.emit(Event::KeyframeRequest { from: sender, track, layer });
                }
            }
            control::SYNC_ABORT if data.len() >= 4 => {
                let transfer = u32_at(data, 0);
                self.transfers.retain(|t| !(t.slot == from && t.transfer == transfer));
                if self.sync.as_ref().is_some_and(|s| s.slot == from && s.transfer == transfer) {
                    self.fail_sync();
                }
            }
            _ => {}
        }
    }

    // Video (GROUP.md §7.4).

    /// The round trip to other members: twice ours to the relay.
    fn member_rtt(&self) -> u64 {
        self.rtt.map_or(100, |r| 2 * r)
    }

    fn tick_video(&mut self, now: u64) {
        let rtt = self.member_rtt();
        let mut all = Vec::new();
        for (slot, v) in &mut self.video_in {
            let mut actions = Vec::new();
            v.tick(now, rtt, &mut actions);
            if !actions.is_empty() {
                all.push((*slot, actions));
            }
        }
        for (slot, actions) in all {
            if let Some(sender) = self.admitted(slot).map(|a| a.identity) {
                self.video_actions(slot, sender, actions);
            }
        }
    }

    fn video_actions(&mut self, slot: u8, sender: MemberId, actions: Vec<video::Action>) {
        for action in actions {
            match action {
                video::Action::Deliver { track, layer, keyframe, timestamp, data } => {
                    self.emit(Event::Video { from: sender, track, layer, keyframe, timestamp, data });
                }
                video::Action::Nack { track, layer, frame, indices } => {
                    let nack = [&[control::VIDEO_NACK, track, layer][..], &frame.to_be_bytes(), &indices].concat();
                    self.seal_to(wire::CONTROL, slot, &nack);
                }
                video::Action::RequestKeyframe { track, layer } => {
                    self.seal_to(wire::CONTROL, slot, &[control::KEYFRAME_REQUEST, track, layer]);
                }
            }
        }
    }

    // Streams (GROUP.md §5).

    fn handle_channel(&mut self, from: u8, channel: Channel, kind: u8, data: &[u8]) {
        let Some(sender) = self.admitted(from) else { return };
        let (identity, incarnation) = (sender.identity, sender.incarnation);
        match kind {
            message::DIRECT => {
                self.emit(Event::Message { from: identity, channel, data: data.to_vec(), direct: true });
            }
            message::MESSAGE if data.len() >= 8 => {
                let id = u64_at(data, 0);
                let i = self.position(incarnation, channel, identity, id);
                self.arrived(i, id, data[8..].to_vec());
            }
            message::RESENT if data.len() >= 8 => {
                if let Some(i) = self.find_position(incarnation, channel) {
                    // That burst is over: ask for the rest, if any.
                    if let Some(gap) = &mut self.positions[i].gap {
                        gap.asked_at = None;
                    }
                    self.announced(i, u64_at(data, 0));
                }
            }
            message::SKIP if data.len() >= 8 => {
                if let Some(i) = self.find_position(incarnation, channel) {
                    self.skip_to(i, u64_at(data, 0));
                }
            }
            _ => {}
        }
    }

    /// The index of our position in a stream, created at `start` if new.
    fn position(&mut self, incarnation: u64, channel: Channel, owner: MemberId, start: u64) -> usize {
        self.uses += 1;
        if let Some(i) = self.find_position(incarnation, channel) {
            self.positions[i].used = self.uses;
            return i;
        }
        if self.positions.len() >= REMEMBERED_STREAMS {
            let (oldest, _) = self.positions.iter().enumerate().min_by_key(|(_, p)| p.used).unwrap();
            self.positions.swap_remove(oldest);
        }
        self.positions.push(Position {
            incarnation,
            channel,
            owner,
            next: start,
            want: start,
            held: BTreeMap::new(),
            held_bytes: 0,
            gap: None,
            used: self.uses,
        });
        self.positions.len() - 1
    }

    fn find_position(&self, incarnation: u64, channel: Channel) -> Option<usize> {
        self.positions.iter().position(|p| p.incarnation == incarnation && p.channel == channel)
    }

    /// The stream's owner says its next message will be `next`.
    fn announced(&mut self, i: usize, next: u64) {
        let pos = &mut self.positions[i];
        pos.want = pos.want.max(next);
        self.check_gap(i);
    }

    fn arrived(&mut self, i: usize, id: u64, data: Vec<u8>) {
        let pos = &mut self.positions[i];
        pos.want = pos.want.max(id + 1);
        if id < pos.next || pos.held.contains_key(&id) {
            return; // a duplicate
        }
        if id > pos.next {
            pos.held_bytes += data.len();
            pos.held.insert(id, data);
            if pos.held.len() > HOLD_MESSAGES || pos.held_bytes > HOLD_BYTES {
                let first = *pos.held.keys().next().unwrap();
                return self.skip_to(i, first);
            }
            return self.check_gap(i);
        }
        pos.next += 1;
        let (channel, owner, incarnation) = (pos.channel, pos.owner, pos.incarnation);
        self.deliver(channel, owner, incarnation, id, data);
        self.drain(i);
    }

    /// Delivers held messages that are now in order.
    fn drain(&mut self, i: usize) {
        let before = self.positions[i].next;
        loop {
            let pos = &mut self.positions[i];
            let Some(data) = pos.held.remove(&pos.next) else { break };
            pos.held_bytes -= data.len();
            let (id, channel, owner, incarnation) = (pos.next, pos.channel, pos.owner, pos.incarnation);
            pos.next += 1;
            self.deliver(channel, owner, incarnation, id, data);
        }
        let pos = &mut self.positions[i];
        if pos.next > before
            && let Some(gap) = &mut pos.gap
        {
            gap.since = self.clock;
        }
        self.check_gap(i);
    }

    /// Moves past messages that will not come, reporting them as missed.
    fn skip_to(&mut self, i: usize, next: u64) {
        let pos = &mut self.positions[i];
        if next <= pos.next {
            return;
        }
        let kept = pos.held.split_off(&next);
        let missed = next - pos.next - pos.held.len() as u64;
        let early = std::mem::replace(&mut pos.held, kept);
        pos.held_bytes = pos.held.values().map(Vec::len).sum();
        let (channel, owner, incarnation) = (pos.channel, pos.owner, pos.incarnation);
        // Held messages below `next` still arrived: deliver them in order.
        for (id, data) in early {
            self.deliver(channel, owner, incarnation, id, data);
        }
        self.positions[i].next = next;
        if missed > 0 {
            self.emit(Event::Missed { from: owner, channel, count: missed });
        }
        self.drain(i);
    }

    /// Opens or closes the gap, asking the owner for what is missing.
    fn check_gap(&mut self, i: usize) {
        let now = self.clock;
        let pos = &mut self.positions[i];
        if pos.next >= pos.want {
            pos.gap = None;
            return;
        }
        let gap = pos.gap.get_or_insert(Gap { since: now, asked_at: None });
        if gap.asked_at.is_none() {
            self.ask(i);
        }
    }

    fn ask(&mut self, i: usize) {
        let pos = &self.positions[i];
        let Some(slot) = self.slot_of(pos.incarnation) else { return };
        let mut data = vec![control::RESEND, pos.channel as u8];
        data.extend_from_slice(&pos.incarnation.to_be_bytes());
        data.extend_from_slice(&pos.next.to_be_bytes());
        if self.seal_to(wire::CONTROL, slot, &data)
            && let Some(gap) = &mut self.positions[i].gap
        {
            gap.asked_at = Some(self.clock);
        }
    }

    fn check_gaps(&mut self, now: u64) {
        for i in 0..self.positions.len() {
            let pos = &self.positions[i];
            let Some(gap) = pos.gap else { continue };
            if self.slot_of(pos.incarnation).is_none() {
                continue; // its owner is away; it will announce itself when back
            }
            if now >= gap.since + ms(GAP_TIMEOUT) {
                let to = pos.held.keys().next().copied().unwrap_or(pos.want);
                self.skip_to(i, to);
            } else if gap.asked_at.is_none_or(|at| now >= at + ms(RESEND_RETRY)) {
                self.ask(i);
            }
        }
    }

    /// Answers `RESEND`: a burst of our messages from `from`, then `RESENT`.
    fn resend(&mut self, to: u8, channel: Channel, from: u64) {
        let stream = &self.streams[channel as usize];
        let mut frames = Vec::new();
        let mut id = from.min(stream.next);
        if id < stream.base {
            frames.push([&[message::SKIP][..], &stream.base.to_be_bytes()].concat());
            id = stream.base;
        }
        let mut bytes = 0;
        while id < stream.next && bytes < RESEND_BURST {
            let data = &stream.messages[(id - stream.base) as usize];
            bytes += data.len() + 16;
            frames.push(stream_message(id, data));
            id += 1;
        }
        frames.push([&[message::RESENT][..], &id.to_be_bytes()].concat());
        for frame in frames {
            self.seal_to(channel.frame_type(), to, &frame);
        }
    }

    /// Hands a stream message to the application, or holds a command while
    /// a state transfer runs.
    fn deliver(&mut self, channel: Channel, from: MemberId, incarnation: u64, id: u64, data: Vec<u8>) {
        if channel == Channel::Command
            && let Some(sync) = &mut self.sync
        {
            sync.held.push((incarnation, from, id, data));
            return;
        }
        self.emit(Event::Message { from, channel, data, direct: false });
    }

    // State transfer (GROUP.md §6).

    fn handle_sync_request(&mut self, from: u8, transfer: u32) {
        let Some(to) = self.admitted(from).map(|a| a.identity) else { return };
        if self.sync.is_some() {
            // We are not up to date ourselves.
            self.seal_to(wire::CONTROL, from, &abort(transfer));
            return;
        }
        // Our position in every command stream, our own included: exactly
        // what the application has been given so far.
        let mut known: Vec<&Position> = self.positions.iter().filter(|p| p.channel == Channel::Command).collect();
        known.sort_by_key(|p| std::cmp::Reverse(p.used));
        let mut frontier: Vec<(u64, u64)> = vec![(self.incarnation, self.streams[0].next)];
        frontier.extend(known.iter().take(254).map(|p| (p.incarnation, p.next)));
        // One transfer per requester: a newer request replaces an older one,
        // so that a member cannot pile them up.
        self.transfers.retain(|t| t.slot != from);
        self.transfers.push(Transfer {
            slot: from,
            to,
            transfer,
            frontier,
            data: None,
            sent: 0,
            acked: 0,
            progress_at: self.clock,
            window: Window::new(self.config.transfer_delay),
        });
        self.emit(Event::SyncRequest { from: to, transfer });
    }

    fn pump_transfers(&mut self) {
        let now = self.clock;
        let mut frames = Vec::new();
        for t in &mut self.transfers {
            let Some(data) = &t.data else { continue };
            while t.sent < data.len() as u64 && t.window.open(t.sent - t.acked, now) {
                let start = t.sent as usize;
                let end = (start + BULK_CHUNK).min(data.len());
                frames.push((t.slot, [&t.transfer.to_be_bytes()[..], &data[start..end]].concat()));
                t.sent = end as u64;
                t.window.sent(t.sent, (end - start) as u64, now);
            }
        }
        // Done once the receiver has it all (an empty one, once begun).
        self.transfers.retain(|t| t.data.as_ref().is_none_or(|d| t.acked < d.len() as u64));
        for (slot, frame) in frames {
            self.seal_to(wire::BULK, slot, &frame);
        }
    }

    fn handle_sync_begin(&mut self, from: u8, data: &[u8]) {
        let (transfer, length, count) = (u32_at(data, 0), u64_at(data, 4), data[12] as usize);
        let Some(entries) = data.get(13..13 + 16 * count) else { return };
        let max = self.config.max_snapshot as u64;
        let Some(sync) = self.sync.as_mut().filter(|s| s.slot == from && s.transfer == transfer && s.length.is_none())
        else {
            return;
        };
        if length > max {
            self.seal_to(wire::CONTROL, from, &abort(transfer));
            return self.fail_sync();
        }
        sync.length = Some(length);
        sync.frontier = entries.chunks(16).map(|e| (u64_at(e, 0), u64_at(e, 8))).collect();
        sync.progress_at = self.clock;
        // Data that overtook SYNC_BEGIN (on another stream) counts now.
        self.received_bulk(from, transfer);
    }

    fn handle_bulk(&mut self, from: u8, plaintext: &[u8]) {
        let Some(transfer) = plaintext.get(..4).map(|t| u32_at(t, 0)) else { return };
        let now = self.clock;
        let Some(sync) = self.sync.as_mut().filter(|s| s.slot == from && s.transfer == transfer) else { return };
        sync.data.extend_from_slice(&plaintext[4..]);
        sync.progress_at = now;
        if sync.length.is_none() {
            // Ahead of SYNC_BEGIN, which a transport with a stream per
            // class may deliver later; no more than the provider's window.
            if sync.data.len() > MAX_EARLY_BULK {
                self.seal_to(wire::CONTROL, from, &abort(transfer));
                self.fail_sync();
            }
            return;
        }
        self.received_bulk(from, transfer);
    }

    /// Acknowledges and completes a transfer with what it has received.
    fn received_bulk(&mut self, from: u8, transfer: u32) {
        let Some(sync) = self.sync.as_mut() else { return };
        let Some(length) = sync.length else { return };
        let received = sync.data.len() as u64;
        if received > length {
            self.seal_to(wire::CONTROL, from, &abort(transfer));
            return self.fail_sync();
        }
        if received - sync.acked >= SYNC_ACK_EVERY || (received == length && received > sync.acked) {
            sync.acked = received;
            let ack = [&[control::SYNC_ACK][..], &transfer.to_be_bytes(), &received.to_be_bytes()].concat();
            self.seal_to(wire::CONTROL, from, &ack);
        }
        if received == length {
            self.finish_sync();
        }
    }

    /// The snapshot is complete: deliver it, then exactly the commands it
    /// does not contain.
    fn finish_sync(&mut self) {
        let Some(sync) = self.sync.take() else { return };
        self.emit(Event::Snapshot { from: sync.provider, data: sync.data });
        let in_frontier = |incarnation: u64| sync.frontier.iter().find(|(i, _)| *i == incarnation).map(|(_, f)| *f);

        // Our own stream: everything from the frontier on, again.
        if let Some(f) = in_frontier(self.incarnation) {
            let stream = &self.streams[0];
            let (base, next) = (stream.base, stream.next);
            let again: Vec<(u64, Vec<u8>)> = (f.max(base)..next.max(f))
                .filter_map(|id| stream.messages.get((id - base) as usize).map(|m| (id, m.clone())))
                .collect();
            if f < base {
                self.emit(Event::Missed { from: self.me, channel: Channel::Command, count: base - f });
            }
            for (id, data) in again {
                self.deliver(Channel::Command, self.me, self.incarnation, id, data);
            }
        }
        // Other streams in the frontier restart at it: what they delivered
        // beyond it comes again (held, or asked for anew), what is below it
        // is in the snapshot.
        let mut touched = Vec::new();
        for &(incarnation, f) in &sync.frontier {
            if incarnation == self.incarnation {
                continue;
            }
            match self.find_position(incarnation, Channel::Command) {
                Some(i) => {
                    let pos = &mut self.positions[i];
                    pos.want = pos.want.max(pos.next).max(f);
                    pos.held = pos.held.split_off(&f);
                    pos.next = f;
                    touched.push(i);
                }
                None => {
                    // A stream we have not met: start there when we do.
                    self.starts.retain(|(i, _)| *i != incarnation);
                    self.starts.push_back((incarnation, f));
                    if self.starts.len() > REMEMBERED_STARTS {
                        self.starts.pop_front();
                    }
                }
            }
        }
        for (incarnation, from, id, data) in sync.held {
            if incarnation == self.incarnation && in_frontier(incarnation).is_some() {
                continue; // delivered again above
            }
            match in_frontier(incarnation).and_then(|f| Some((f, self.find_position(incarnation, Channel::Command)?))) {
                Some((f, i)) if id >= f => {
                    let pos = &mut self.positions[i];
                    pos.held_bytes += data.len();
                    pos.held.insert(id, data);
                }
                Some(_) => {} // in the snapshot
                None => self.deliver(Channel::Command, from, incarnation, id, data),
            }
        }
        for i in touched {
            let pos = &mut self.positions[i];
            pos.held_bytes = pos.held.values().map(Vec::len).sum();
            self.drain(i);
        }
    }

    /// The transfer failed: deliver what was held, as if none had been asked for.
    fn fail_sync(&mut self) {
        let Some(sync) = self.sync.take() else { return };
        self.emit(Event::SyncFailed { from: sync.provider });
        for (incarnation, from, id, data) in sync.held {
            self.deliver(Channel::Command, from, incarnation, id, data);
        }
    }

    fn check_transfers(&mut self, now: u64) {
        if let Some(sync) = &self.sync
            && now >= sync.progress_at + ms(SYNC_STALL)
        {
            let (slot, transfer) = (sync.slot, sync.transfer);
            self.seal_to(wire::CONTROL, slot, &abort(transfer));
            self.fail_sync();
        }
        let stalled: Vec<(u8, u32)> = self
            .transfers
            .iter()
            .filter(|t| now >= t.progress_at + ms(SYNC_STALL))
            .map(|t| (t.slot, t.transfer))
            .collect();
        for (slot, transfer) in stalled {
            self.transfers.retain(|t| !(t.slot == slot && t.transfer == transfer));
            self.seal_to(wire::CONTROL, slot, &abort(transfer));
        }
    }
}

impl Remote {
    fn new(now: u64) -> Remote {
        Remote { since: now, key_sent_at: now, state: Admission::Unknown, counters: Default::default() }
    }
}

fn parse_welcome(frame: &[u8]) -> Option<(u8, u32)> {
    let &[_, slot, a, b, c, d] = frame else { return None };
    ((slot as usize) < MAX_SLOTS).then_some((slot, u32::from_be_bytes([a, b, c, d])))
}

fn stream_message(id: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + data.len());
    out.push(message::MESSAGE);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(data);
    out
}

fn abort(transfer: u32) -> Vec<u8> {
    [&[control::SYNC_ABORT][..], &transfer.to_be_bytes()].concat()
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(data[at..at + 8].try_into().unwrap())
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
}

fn random_below(bound: u64) -> u64 {
    u64::from_le_bytes(crate::random()) % bound.max(1)
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

#[cfg(test)]
#[path = "vectors.rs"]
mod vectors;

#[cfg(test)]
mod tests {
    use super::*;

    /// A member that the relay has just welcomed into slot 0, alone.
    fn welcomed() -> Member {
        let group = Group::create("https://relay.test").unwrap();
        let mut m = Member::new(group, Identity::generate(), Config::default(), 0).unwrap();
        m.handle_open(0);
        m.handle_binary(0, &crate::wire::hello(&[0; 32]));
        m.handle_binary(0, &wire::welcome(0, 0));
        while m.poll_output().is_some() {}
        m
    }

    #[test]
    fn a_counter_about_to_wrap_brings_a_new_connection_not_a_reused_nonce() {
        let mut m = welcomed();
        let voice = SEALED.iter().position(|&k| k == wire::VOICE).unwrap();
        m.joined_mut().unwrap().counters[voice] = u32::MAX - 1;
        assert!(m.send_voice(0, b"last"));
        assert!(!m.send_voice(1, b"one too many"));
        let outputs: Vec<Output> = std::iter::from_fn(|| m.poll_output()).collect();
        assert!(outputs.contains(&Output::Disconnect));
        assert_eq!(m.state(), State::Offline);
        let sends = outputs.iter().filter(|o| matches!(o, Output::Send(f, _) if f[0] == wire::VOICE)).count();
        assert_eq!(sends, 1);
    }

    #[test]
    fn what_cannot_be_sent_is_refused() {
        let mut m = welcomed();
        let big = vec![0; wire::MAX_PAYLOAD_LEN + 1];
        let stranger = MemberId([9; 32]);
        assert_eq!(m.send(Channel::Command, &big), Err(SendError::TooLarge));
        assert!(m.send(Channel::Command, &big[1..]).is_ok(), "the largest fits");
        assert_eq!(m.send_to(stranger, Channel::Chat, &big), Err(SendError::TooLarge));
        assert_eq!(m.send_to(stranger, Channel::Chat, b"hi"), Err(SendError::NotPresent));
        assert!(!m.send_voice(0, &vec![0; wire::MAX_DATAGRAM_LEN + 1]));
        assert!(m.send_voice(0, &vec![0; wire::MAX_DATAGRAM_LEN]));
        assert!(!m.send_datagram(None, &vec![0; wire::MAX_DATAGRAM_LEN + 1]));
        assert!(!m.send_datagram(Some(stranger), b"x"));
        assert_eq!(m.request_sync(stranger), Err(SendError::NotPresent));
        assert_eq!(m.provide_sync(stranger, 1, Vec::new()), Err(SendError::Busy));
        m.decline_sync(stranger, 1); // nothing to decline: harmless
        // Every frame fits the relay's limit.
        for out in std::iter::from_fn(|| m.poll_output()) {
            if let Output::Send(frame, _) = out {
                assert!(frame.len() <= wire::MAX_FRAME_LEN, "{}", frame.len());
            }
        }
        m.leave();
        m.leave();
        assert_eq!(m.send(Channel::Chat, b"x"), Err(SendError::Closed));
        assert!(!m.send_voice(0, b"x"));
        assert!(!m.send_datagram(None, b"x"));
        assert_eq!(m.group().relay(), "https://relay.test");
        assert_eq!(SendError::Busy.to_string(), "a state transfer is already under way");
    }

    #[test]
    fn offline_a_member_queues_stream_messages_and_drops_the_rest() {
        let group = Group::create("https://relay.test").unwrap();
        let mut m = Member::new(group, Identity::generate(), Config::default(), 0).unwrap();
        assert_eq!(m.send(Channel::Chat, b"later"), Ok(0));
        assert!(!m.send_voice(0, b"x"));
        assert!(!m.send_datagram(None, b"x"));
        let echo = std::iter::from_fn(|| m.poll_output())
            .any(|o| matches!(o, Output::Event(Event::Message { channel: Channel::Chat, direct: false, .. })));
        assert!(echo, "our own message is delivered to us at once");
    }

    #[test]
    fn profiles_over_1_kib_are_refused() {
        let group = Group::create("https://relay.test").unwrap();
        let config = Config { profile: vec![0; wire::MAX_PROFILE_LEN + 1], ..Config::default() };
        assert_eq!(Member::new(group.clone(), Identity::generate(), config, 0).err(), Some(Error::InvalidProfile));
        let config = Config { profile: vec![0; wire::MAX_PROFILE_LEN], ..Config::default() };
        let mut m = Member::new(group, Identity::generate(), config, 0).unwrap();
        m.handle_open(0);
        m.handle_binary(0, &crate::wire::hello(&[0; 32]));
        m.handle_binary(0, &wire::welcome(0, 0));
        let key = std::iter::from_fn(|| m.poll_output()).find_map(|o| match o {
            Output::Send(f, _) if f[0] == wire::KEY => Some(f),
            _ => None,
        });
        assert!(key.unwrap().len() <= wire::MAX_FRAME_LEN);
    }

    #[test]
    fn a_stable_connection_resets_the_backoff() {
        let mut m = welcomed();
        m.failures = 6;
        m.handle_timeout(STABLE.as_millis() as u64 + 1);
        m.handle_close(STABLE.as_millis() as u64 + 1, None);
        assert_eq!(m.failures, 1, "reset, then this failure counted");
    }

    /// Two members in slots 0 and 1, with every frame shuttled between them
    /// until both have admitted each other. Returns them and the events
    /// each produced on the way.
    fn pair(config: Config) -> (Member, Member) {
        let group = Group::create("https://relay.test").unwrap();
        let mut a = Member::new(group.clone(), Identity::generate(), config, 0).unwrap();
        let mut b = Member::new(group, Identity::generate(), Config::default(), 0).unwrap();
        for (m, slot, present) in [(&mut a, 0, 0b10), (&mut b, 1, 0b01)] {
            m.handle_open(0);
            m.handle_binary(0, &crate::wire::hello(&[0; 32]));
            m.handle_binary(0, &wire::welcome(slot, present));
        }
        shuttle(&mut a, &mut b);
        assert_eq!((a.members().len(), b.members().len()), (1, 1));
        (a, b)
    }

    /// Delivers frames both ways until nothing is left; drops both members' events.
    fn shuttle(a: &mut Member, b: &mut Member) {
        while carry(a, b) | carry(b, a) {}
    }

    /// Delivers `from`'s pending peer frames to `to`; whether there were any.
    fn carry(from: &mut Member, to: &mut Member) -> bool {
        let mut moved = false;
        while let Some(out) = from.poll_output() {
            if let Output::Send(frame, _) = out
                && frame[0] < 0x80
            {
                to.handle_binary(0, &frame);
                moved = true;
            }
        }
        moved
    }

    fn events(m: &mut Member) -> Vec<Event> {
        std::iter::from_fn(|| m.poll_output())
            .filter_map(|o| if let Output::Event(e) = o { Some(e) } else { None })
            .collect()
    }

    /// Seals a frame as `b` would, with its key and its next counter.
    fn forge(b: &mut Member, kind: u8, to: u8, plaintext: &[u8]) -> Vec<u8> {
        let index = SEALED.iter().position(|&k| k == kind);
        let joined = b.joined_mut().unwrap();
        let counter = index.map_or(0, |i| {
            joined.counters[i] += 1;
            joined.counters[i] - 1
        });
        seal::seal(&joined.key, kind, joined.slot, to, counter, plaintext)
    }

    #[test]
    fn the_transfer_window_grows_on_a_clear_path_and_shrinks_when_it_queues() {
        let mut w = Window::new(Duration::from_millis(50));
        let mut now = 0;
        let mut offset = 0;
        let mut ack = |w: &mut Window, now: &mut u64, rtt: u64| {
            offset += SYNC_ACK_EVERY;
            w.sent(offset, SYNC_ACK_EVERY, *now);
            *now += rtt;
            w.acked(offset, *now);
        };
        // Acknowledgements 40 ms after sending, every time: no queue.
        for _ in 0..1000 {
            ack(&mut w, &mut now, 40);
        }
        assert_eq!(w.bytes, SYNC_WINDOW_MAX);
        // Then they take 200 ms: the transfer is filling a queue.
        for _ in 0..100 {
            ack(&mut w, &mut now, 200);
        }
        assert_eq!(w.bytes, SYNC_WINDOW_MIN);
        // Back to 40 ms: it grows again, but slowly now.
        for _ in 0..10 {
            ack(&mut w, &mut now, 40);
        }
        assert!(w.bytes > SYNC_WINDOW_MIN && w.bytes < 3 * SYNC_WINDOW_MIN, "{}", w.bytes);
        // Paced: a chunk every rtt × chunk / window.
        let expected = SYNC_ACK_EVERY as f64 * 40.0 / w.bytes as f64;
        w.sent(offset + SYNC_ACK_EVERY, SYNC_ACK_EVERY, now);
        assert!((w.next_at - (now as f64 - 1.0) - expected).abs() < 0.01, "{}", w.next_at - now as f64);
        // An acknowledgement of nothing new changes nothing.
        let before = w.bytes;
        w.acked(offset, now + 1000);
        assert_eq!(w.bytes, before);
    }

    /// Runs two members for `ms` of simulated time, dropping the frames
    /// from a to b that `lose` picks; returns a's events and b's.
    fn video_between(
        a: &mut Member,
        b: &mut Member,
        from: u64,
        to: u64,
        mut lose: impl FnMut(&[u8]) -> bool,
    ) -> (Vec<Event>, Vec<Event>) {
        let (mut ea, mut eb) = (Vec::new(), Vec::new());
        for now in from..to {
            a.handle_timeout(now);
            b.handle_timeout(now);
            while let Some(out) = a.poll_output() {
                match out {
                    Output::Send(f, _) if f[0] < 0x80 && !lose(&f) => b.handle_binary(now, &f),
                    Output::Event(e) => ea.push(e),
                    _ => {}
                }
            }
            while let Some(out) = b.poll_output() {
                match out {
                    Output::Send(f, _) if f[0] < 0x80 => a.handle_binary(now, &f),
                    Output::Event(e) => eb.push(e),
                    _ => {}
                }
            }
        }
        (ea, eb)
    }

    fn frames_from(events: &[Event], member: MemberId) -> Vec<(bool, u32, Vec<u8>)> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Video { from, track: 0, keyframe, timestamp, data, .. } if *from == member => {
                    Some((*keyframe, *timestamp, data.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn video_frames_arrive_whole_with_a_lost_fragment_resent() {
        let (mut a, mut b) = pair(Config::default());
        let ia = a.id();
        let key: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        assert!(a.send_video(0, 0, true, 1000, &key));
        assert!(a.send_video(0, 0, false, 1033, b"delta"));
        // The keyframe's second fragment is lost, the first time only.
        let mut broadcast = 0;
        let (_, got) = video_between(&mut a, &mut b, 1, 200, |f| {
            if wire::video_layer(f[0]).is_none() || f[2] != BROADCAST {
                return false;
            }
            broadcast += 1;
            broadcast == 2
        });
        assert_eq!(frames_from(&got, ia), [(true, 1000, key), (false, 1033, b"delta".to_vec())]);
        assert!(!a.send_video(0, 0, false, 0, &vec![0; wire::MAX_VIDEO_FRAME + 1]));
    }

    #[test]
    fn a_video_frame_lost_for_good_brings_a_keyframe_request() {
        let (mut a, mut b) = pair(Config::default());
        let (ia, ib) = (a.id(), b.id());
        assert!(a.send_video(0, 0, true, 0, b"key"));
        assert!(a.send_video(0, 0, false, 1, b"lost"));
        assert!(a.send_video(0, 0, false, 2, b"after"));
        // Frame 1 never arrives, nor do its resends.
        let lost = |f: &[u8]| f[0] == wire::video_type(0, false) && f.len() == 7 + 12 + 4 + 16;
        let (asked, got) = video_between(&mut a, &mut b, 1, 600, lost);
        assert_eq!(frames_from(&got, ia), [(true, 0, b"key".to_vec())], "frame 2 cannot be decoded without 1");
        assert!(
            asked.iter().any(|e| matches!(e, Event::KeyframeRequest { from, track: 0, layer: 0 } if *from == ib)),
            "{asked:?}"
        );
        // The keyframe that answers it gets things going again.
        assert!(a.send_video(0, 0, true, 3, b"again"));
        assert!(a.send_video(0, 0, false, 4, b"fine"));
        let (_, got) = video_between(&mut a, &mut b, 600, 700, |_| false);
        assert_eq!(frames_from(&got, ia), [(true, 3, b"again".to_vec()), (false, 4, b"fine".to_vec())]);
    }

    #[test]
    fn layers_travel_apart_and_subscriptions_reach_the_relay() {
        let (mut a, mut b) = pair(Config::default());
        let (ia, ib) = (a.id(), b.id());
        assert!(!a.send_video(0, wire::VIDEO_LAYERS, true, 0, b"x"), "no such layer");
        assert!(a.send_video(0, 1, true, 0, b"small"));
        assert!(a.send_video(0, 2, true, 0, b"large"));
        let types: Vec<u8> = std::iter::from_fn(|| a.poll_output())
            .filter_map(|o| if let Output::Send(f, _) = o { Some(f[0]) } else { None })
            .collect();
        assert_eq!(types, [wire::video_type(1, true), wire::video_type(2, true)]);

        // b asks the relay for layer 2 of a's video, and asks a for a keyframe of it once it has seen a's video.
        assert!(a.send_video(0, 1, false, 33, b"small delta"));
        let (_, got) = video_between(&mut a, &mut b, 1, 10, |_| false);
        assert!(got.is_empty() || frames_from(&got, ia).len() <= 1);
        b.subscribe(ia, Some(2)).unwrap();
        let mut relay = Vec::new();
        let mut peer = Vec::new();
        while let Some(out) = b.poll_output() {
            if let Output::Send(f, _) = out {
                if f[0] >= 0x80 { relay.push(f) } else { peer.push(f) }
            }
        }
        assert_eq!(relay, [wire::subscribe(0, Some(2)).to_vec()]);
        for f in &peer {
            a.handle_binary(20, f);
        }
        let asked: Vec<Event> = std::iter::from_fn(|| a.poll_output())
            .filter_map(|o| if let Output::Event(e) = o { Some(e) } else { None })
            .collect();
        assert!(asked.contains(&Event::KeyframeRequest { from: ib, track: 0, layer: 2 }), "{asked:?}");
        assert_eq!(b.subscribe(ia, Some(wire::VIDEO_LAYERS)), Err(SendError::TooLarge));

        // The relay's DEMAND reaches the application.
        a.handle_binary(30, &wire::demand(0b110));
        let demand: Vec<Event> = std::iter::from_fn(|| a.poll_output())
            .filter_map(|o| if let Output::Event(e) = o { Some(e) } else { None })
            .collect();
        assert_eq!(demand, [Event::VideoDemand { layers: 0b110 }]);
    }

    #[test]
    fn replay_windows() {
        let mut strict = Replay::default();
        assert!(strict.accept(5, false));
        assert!(!strict.accept(5, false));
        assert!(strict.accept(6, false));
        assert!(!strict.accept(4, false), "no reordering outside the real-time classes");

        let mut window = Replay::default();
        for c in [10, 12, 11, 9, 13] {
            assert!(window.accept(c, true), "{c}");
        }
        for c in [10, 11, 12, 13, 9] {
            assert!(!window.accept(c, true), "{c} again");
        }
        assert!(window.accept(100, true));
        assert!(window.accept(37, true), "63 below the highest");
        assert!(!window.accept(36, true), "64 below is too old");
        assert!(!window.accept(37, true));
        assert!(window.accept(u32::MAX, true));
        assert!(!window.accept(100, true), "far behind after a jump");
    }

    #[test]
    fn voice_that_arrives_out_of_order_is_played_but_never_twice() {
        let (mut a, mut b) = pair(Config::default());
        for seq in 0..3u16 {
            b.send_voice(seq, &[seq as u8]);
        }
        let mut frames: Vec<Vec<u8>> = std::iter::from_fn(|| b.poll_output())
            .filter_map(|o| if let Output::Send(f, _) = o { Some(f) } else { None })
            .collect();
        frames.swap(0, 2);
        for frame in frames.iter().chain(&frames) {
            a.handle_binary(0, frame);
        }
        let seqs: Vec<u16> = events(&mut a)
            .iter()
            .filter_map(|e| if let Event::Voice { seq, .. } = e { Some(*seq) } else { None })
            .collect();
        assert_eq!(seqs, [2, 1, 0]);
    }

    #[test]
    fn peer_frames_that_overtake_welcome_are_ignored() {
        let group = Group::create("quic://relay.test:4433").unwrap();
        let (_, mut b) = pair(Config::default());
        b.send(Channel::Chat, b"early").unwrap();
        b.send_voice(0, b"early");
        let early: Vec<Vec<u8>> = std::iter::from_fn(|| b.poll_output())
            .filter_map(|o| if let Output::Send(f, _) = o { Some(f) } else { None })
            .collect();
        let mut c = Member::new(group, Identity::generate(), Config::default(), 0).unwrap();
        c.handle_timeout(0);
        c.handle_open(0);
        c.handle_binary(0, &crate::wire::hello(&[0; 32]));
        for frame in &early {
            c.handle_binary(0, frame);
        }
        c.handle_binary(0, &wire::welcome(2, 0b11));
        let outs: Vec<Output> = std::iter::from_fn(|| c.poll_output()).collect();
        assert!(!outs.contains(&Output::Disconnect), "{outs:?}");
        assert_eq!(c.state(), State::Online);
    }

    #[test]
    fn unknown_frames_kinds_and_short_frames_are_ignored() {
        let (mut a, mut b) = pair(Config::default());
        events(&mut a);
        let ignored = [
            forge(&mut b, 0x04, 0, b"a future command type"),
            forge(&mut b, wire::CONTROL, 0, &[0x7F, 1, 2, 3]),
            forge(&mut b, wire::CONTROL, 0, &[control::RESEND, 0]),
            forge(&mut b, wire::CONTROL, 0, &[control::SYNC_BEGIN, 0, 0]),
            forge(&mut b, wire::CONTROL, 0, &[]),
            forge(&mut b, wire::COMMAND, 0, &[0x09, 1]),
            forge(&mut b, wire::COMMAND, 0, &[message::MESSAGE, 1, 2]),
            forge(&mut b, wire::CHAT, 0, &[message::SKIP]),
            forge(&mut b, wire::VOICE, 0, &[1]),
            forge(&mut b, wire::BULK, 0, &[0, 0, 0, 1, 9]),
            forge(&mut b, wire::BULK, 0, &[1]),
            vec![wire::COMMAND, 7, 0, 0, 0, 0, 0], // from an empty slot
            vec![wire::COMMAND, 1],                // too short to be sealed
            vec![0x90, 1, 2],                      // a future relay frame
            vec![wire::MEMBER, 0, 1],              // about our own slot
            vec![wire::MEMBER, 40, 1],             // no such slot
        ];
        for frame in &ignored {
            a.handle_binary(0, frame);
        }
        let outs: Vec<Output> = std::iter::from_fn(|| a.poll_output()).collect();
        assert!(outs.iter().all(|o| !matches!(o, Output::Event(_))), "nothing came of them: {outs:?}");
        // And the stream still works afterwards.
        b.send(Channel::Command, b"still fine").unwrap();
        carry(&mut b, &mut a); // (carrying drops the sender's own events, not the receiver's)
        let got = events(&mut a);
        assert!(got.iter().any(|e| matches!(e, Event::Message { data, .. } if data == b"still fine")), "{got:?}");
    }

    #[test]
    fn snapshot_data_that_overtakes_its_begin_is_kept() {
        let (mut a, mut b) = pair(Config::default());
        let (ia, ib) = (a.id(), b.id());
        a.request_sync(ib).unwrap();
        let request = std::iter::from_fn(|| a.poll_output())
            .find_map(|o| if let Output::Send(f, _) = o { Some(f) } else { None })
            .unwrap();
        b.handle_binary(0, &request);
        let Some(Event::SyncRequest { transfer, .. }) =
            events(&mut b).into_iter().find(|e| matches!(e, Event::SyncRequest { .. }))
        else {
            panic!()
        };
        let world: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
        b.provide_sync(ia, transfer, world.clone()).unwrap();
        let frames: Vec<Vec<u8>> = std::iter::from_fn(|| b.poll_output())
            .filter_map(|o| if let Output::Send(f, _) = o { Some(f) } else { None })
            .collect();
        let (bulk, rest): (Vec<_>, Vec<_>) = frames.into_iter().partition(|f| f[0] == wire::BULK);
        assert!(!bulk.is_empty() && rest.iter().any(|f| f[0] == wire::CONTROL));
        for frame in bulk.iter().chain(&rest) {
            a.handle_binary(0, frame);
        }
        // The rest of it, paced, in order.
        let mut got = Vec::new();
        for now in 1..5000 {
            b.handle_timeout(now);
            while let Some(out) = b.poll_output() {
                if let Output::Send(f, _) = out {
                    a.handle_binary(now, &f);
                }
            }
            while let Some(out) = a.poll_output() {
                match out {
                    Output::Send(f, _) if f[0] < 0x80 => b.handle_binary(now, &f),
                    Output::Event(e) => got.push(e),
                    _ => {}
                }
            }
            if !got.is_empty() {
                break;
            }
        }
        assert_eq!(got, [Event::Snapshot { from: ib, data: world }]);
    }

    #[test]
    fn a_provider_that_sends_more_than_it_announced_is_refused() {
        let (mut a, mut b) = pair(Config::default());
        let ib = b.id();
        a.request_sync(ib).unwrap();
        let request = std::iter::from_fn(|| a.poll_output())
            .find_map(|o| if let Output::Send(f, _) = o { Some(f) } else { None })
            .unwrap();
        b.handle_binary(0, &request);
        let Some(Event::SyncRequest { transfer, .. }) =
            events(&mut b).into_iter().find(|e| matches!(e, Event::SyncRequest { .. }))
        else {
            panic!()
        };
        let begin = [&[control::SYNC_BEGIN][..], &transfer.to_be_bytes(), &3u64.to_be_bytes(), &[0]].concat();
        let frame = forge(&mut b, wire::CONTROL, 0, &begin);
        a.handle_binary(0, &frame);
        let frame = forge(&mut b, wire::BULK, 0, &[&transfer.to_be_bytes()[..], b"far more than 3"].concat());
        a.handle_binary(0, &frame);
        assert_eq!(events(&mut a), [Event::SyncFailed { from: ib }]);
    }

    #[test]
    fn a_frontier_below_what_we_still_keep_reports_our_own_missed_commands() {
        let config = Config { keep_messages: 2, ..Config::default() };
        let (mut a, mut b) = pair(config);
        for i in 0..5u8 {
            a.send(Channel::Command, &[i]).unwrap();
        }
        events(&mut a);
        while a.poll_output().is_some() {} // b never gets them
        let ib = b.id();
        a.request_sync(ib).unwrap();
        while a.poll_output().is_some() {}
        // A snapshot whose frontier says b has none of a's commands.
        let transfer = a.sync.as_ref().unwrap().transfer;
        let begin = [
            &[control::SYNC_BEGIN][..],
            &transfer.to_be_bytes(),
            &0u64.to_be_bytes(),
            &[1],
            &a.incarnation.to_be_bytes(),
            &0u64.to_be_bytes(),
        ]
        .concat();
        let frame = forge(&mut b, wire::CONTROL, 0, &begin);
        a.handle_binary(0, &frame);
        let got = events(&mut a);
        assert_eq!(got[0], Event::Snapshot { from: ib, data: Vec::new() });
        assert_eq!(got[1], Event::Missed { from: a.id(), channel: Channel::Command, count: 3 });
        let again: Vec<u8> = got[2..]
            .iter()
            .filter_map(|e| if let Event::Message { data, .. } = e { Some(data[0]) } else { None })
            .collect();
        assert_eq!(again, [3, 4], "what we still keep is applied again after the snapshot");
    }
}
