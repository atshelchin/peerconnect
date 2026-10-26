//! Groups (GROUP.md §3): up to 4 slots per group, frames fanned out by
//! class.
//!
//! Each connection has an outbox of four queues, one per class, which its
//! writer drains most urgent first, so that chat or a state transfer never
//! holds up a command, and voice never waits behind chat. A voice frame for
//! a recipient that is behind is dropped, never queued. Reliable frames are
//! never dropped: when a recipient falls behind on commands and chat, the
//! relay stops reading from the sender until it catches up, but for a few
//! seconds at most; a recipient that does not catch up by then is
//! disconnected, so one stuck member cannot hold the others up for long.
//! A frame is shared, not copied, between its recipients.
//!
//! Groups travel over QUIC only (GROUP.md §3.6): raw QUIC or WebTransport,
//! both in [`crate::quic`]. A connection is a [`Link`] to read from and a
//! writer that drains its [`Outbox`].

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use peerconnect::RoomId;
use peerconnect::group::{Class, wire as gwire};
use peerconnect::relay::{GroupAction, GroupConn, Incoming, VideoRouter};
use peerconnect::wire::close;
use tokio::sync::{Notify, OwnedSemaphorePermit, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout, timeout_at};

use crate::Relay;

/// Bytes a writer takes from an outbox at a time, at most: small, so that
/// an urgent frame queued meanwhile goes next.
const BATCH: usize = 16 << 10;
/// Command and chat bytes queued towards a recipient beyond which its
/// senders wait for it.
const BACKLOG: usize = 256 << 10;
/// How long senders wait for a recipient to work off its backlog before
/// it is disconnected.
const BACKLOG_WAIT: Duration = Duration::from_secs(5);
/// Video queued towards a member beyond which its link is not keeping up,
/// if that lasts (GROUP.md §3.3): what takes this long to send at the pace
/// it is being sent, and at least [`VIDEO_CONGESTED`] bytes, so that a
/// keyframe passing through is no sign of trouble.
const VIDEO_CONGESTED_FOR: Duration = Duration::from_millis(250);
const VIDEO_CONGESTED: usize = 32 << 10;

/// The slots of one group, and which layer of whose video goes to whom.
#[derive(Default)]
pub(crate) struct GroupRoom {
    slots: Mutex<[Option<Seat>; gwire::MAX_SLOTS]>,
    /// Locked after `slots`, when both are.
    router: Mutex<VideoRouter>,
}

/// Milliseconds on the relay's clock, for the video router.
fn now_ms() -> u64 {
    static START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    START.elapsed().as_millis() as u64
}

/// Sends DEMAND to every sender whose wanted layers changed, and LIMIT to
/// every receiver whose link limit did.
fn tell_demands(slots: &[Option<Seat>; gwire::MAX_SLOTS], router: &mut VideoRouter) {
    for (slot, track, layers) in router.demands() {
        if let Some(seat) = &slots[slot as usize] {
            seat.outbox.push(Class::Command, Bytes::copy_from_slice(&gwire::demand(track, layers)));
        }
    }
    for (slot, layer) in router.limits() {
        if let Some(seat) = &slots[slot as usize] {
            seat.outbox.push(Class::Command, Bytes::copy_from_slice(&gwire::limit(layer)));
        }
    }
}

struct Seat {
    id: u64,
    token: [u8; gwire::TOKEN_LEN],
    outbox: Arc<Outbox>,
}

/// A connection's queued frames, one queue per class. An empty frame is
/// `pong`.
pub(crate) struct Outbox {
    queues: Mutex<Queues>,
    /// Signalled when any queue gets a frame (for a writer that drains all).
    ready: Notify,
    /// Signalled when that class's queue gets one (for a writer per class).
    class_ready: [Notify; Class::COUNT],
    /// Signalled when the command and chat backlog drops below [`BACKLOG`],
    /// while any sender waits for that.
    drained: Notify,
    waiting: AtomicUsize,
    pub(crate) close: watch::Sender<Option<u16>>,
    /// Reliable bytes queued beyond which the connection is not keeping up.
    limit: usize,
    /// Command and voice bytes queued beyond which voice is dropped.
    voice_limit: usize,
    /// Video bytes queued beyond which the oldest video is dropped.
    video_limit: usize,
    /// Video was dropped since the video router last asked.
    video_dropped: std::sync::atomic::AtomicBool,
    /// The pace video leaves at while some waits, bytes per second
    /// (smoothed): what the member's link takes.
    video_pace: Mutex<Pace>,
    traffic: Arc<crate::traffic::Traffic>,
}

/// How fast video leaves, while some waits.
struct Pace {
    since: Instant,
    bytes: usize,
    rate: f64,
    busy: bool,
}

#[derive(Default)]
struct Queues {
    classes: [VecDeque<Bytes>; Class::COUNT],
    bytes: [usize; Class::COUNT],
}

impl Queues {
    /// What senders may have to wait for: commands and chat. (Voice is
    /// dropped instead, and state transfer has its own window.)
    fn backlog(&self) -> usize {
        self.bytes[Class::Command.index()] + self.bytes[Class::Chat.index()]
    }
}

impl Outbox {
    pub(crate) fn new(
        limit: usize,
        voice_limit: usize,
        video_limit: usize,
        traffic: Arc<crate::traffic::Traffic>,
    ) -> Arc<Outbox> {
        Arc::new(Outbox {
            queues: Mutex::default(),
            ready: Notify::new(),
            class_ready: Default::default(),
            drained: Notify::new(),
            waiting: AtomicUsize::new(0),
            close: watch::Sender::new(None),
            limit,
            voice_limit,
            video_limit,
            video_dropped: std::sync::atomic::AtomicBool::new(false),
            video_pace: Mutex::new(Pace { since: Instant::now(), bytes: 0, rate: 0.0, busy: false }),
            traffic,
        })
    }

    /// Queues a frame. A voice frame that would wait too long is dropped,
    /// and so is the oldest video beyond its budget (an old frame is worth
    /// less than a new one); a reliable one that overflows the outbox
    /// closes the connection. Returns whether the recipient is behind on
    /// commands and chat.
    fn push(&self, class: Class, msg: Bytes) -> bool {
        let mut q = self.queues.lock().unwrap();
        let len = msg.len();
        if class == Class::Voice {
            if q.bytes[Class::Command.index()] + q.bytes[Class::Voice.index()] + len > self.voice_limit {
                return false;
            }
        } else if class == Class::Video {
            let video = Class::Video.index();
            while q.bytes[video] + len > self.video_limit {
                self.video_dropped.store(true, Ordering::Relaxed);
                let Some(old) = q.classes[video].pop_front() else { return false };
                q.bytes[video] -= old.len();
            }
        } else if q.bytes.iter().sum::<usize>() - q.bytes[Class::Video.index()] + len > self.limit {
            drop(q);
            self.close(close::TIMEOUT);
            return false;
        }
        q.bytes[class.index()] += len;
        q.classes[class.index()].push_back(msg);
        let behind = q.backlog() > BACKLOG;
        drop(q);
        self.ready.notify_one();
        self.class_ready[class.index()].notify_one();
        behind
    }

    /// What the video router sees of this member's link: whether video is
    /// waiting, and whether some was dropped since it last asked.
    fn video_link(&self) -> (bool, bool) {
        let queued = self.queues.lock().unwrap().bytes[Class::Video.index()];
        let pace = self.video_pace.lock().unwrap().rate;
        let long = queued as f64 > pace * VIDEO_CONGESTED_FOR.as_secs_f64();
        (queued > VIDEO_CONGESTED && long, self.video_dropped.swap(false, Ordering::Relaxed))
    }

    /// Drops the video queued: its link was moved to smaller layers, and the
    /// larger frames ahead of them would only delay them.
    fn drop_video(&self) {
        let mut q = self.queues.lock().unwrap();
        let video = Class::Video.index();
        q.classes[video].clear();
        q.bytes[video] = 0;
    }

    /// Waits until the recipient's command and chat backlog is below
    /// [`BACKLOG`], or closes it if that takes too long.
    async fn catch_up(&self) {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let caught_up = async {
            loop {
                let drained = self.drained.notified();
                if self.queues.lock().unwrap().backlog() <= BACKLOG || self.close.borrow().is_some() {
                    return;
                }
                drained.await;
            }
        };
        let in_time = timeout(BACKLOG_WAIT, caught_up).await.is_ok();
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        if !in_time {
            self.close(close::TIMEOUT);
        }
    }

    /// Discards queued frames from `slot`, which has a new occupant or none.
    fn purge(&self, slot: u8) {
        let mut q = self.queues.lock().unwrap();
        let Queues { classes, bytes } = &mut *q;
        for (queue, bytes) in classes.iter_mut().zip(bytes) {
            queue.retain(|m| {
                let stale = m.len() >= 2 && m[0] < 0x80 && m[1] == slot;
                if stale {
                    *bytes -= m.len();
                }
                !stale
            });
        }
    }

    /// Takes up to [`BATCH`] bytes of frames, most urgent class first.
    #[cfg(test)]
    fn take(&self) -> Vec<Bytes> {
        self.take_from(&Class::PRIORITY.map(Class::index))
    }

    /// Takes up to [`BATCH`] bytes of frames of one class.
    pub(crate) fn take_class(&self, class: Class) -> Vec<Bytes> {
        self.take_from(&[class.index()])
    }

    fn take_from(&self, classes: &[usize]) -> Vec<Bytes> {
        let mut q = self.queues.lock().unwrap();
        let mut batch = Vec::new();
        let mut taken = 0;
        while taken < BATCH {
            let Some(&class) = classes.iter().find(|&&c| !q.classes[c].is_empty()) else { break };
            let msg = q.classes[class].pop_front().unwrap();
            q.bytes[class] -= msg.len();
            taken += msg.len().max(1);
            if class == Class::Video.index() {
                self.video_left(msg.len(), q.bytes[class]);
            }
            self.traffic.sent(msg.len());
            batch.push(msg);
        }
        if q.backlog() <= BACKLOG && self.waiting.load(Ordering::SeqCst) > 0 {
            self.drained.notify_waiters();
        }
        batch
    }

    /// Video bytes on their way out, `waiting` still queued: the pace video
    /// leaves at while some waits, measured every tenth of a second or so.
    /// Time with nothing waiting is not counted, or a keyframe after a quiet
    /// spell would look slow to leave.
    fn video_left(&self, bytes: usize, waiting: usize) {
        let mut p = self.video_pace.lock().unwrap();
        if !p.busy {
            (p.busy, p.since, p.bytes) = (true, Instant::now(), 0);
        }
        p.bytes += bytes;
        let elapsed = p.since.elapsed();
        if elapsed >= Duration::from_millis(100) {
            let rate = p.bytes as f64 / elapsed.as_secs_f64();
            p.rate = if p.rate > 0.0 { 0.7 * p.rate + 0.3 * rate } else { rate };
            (p.since, p.bytes) = (Instant::now(), 0);
        }
        p.busy = waiting > 0;
    }

    /// Takes frames of `class` from the front while they add up to at most
    /// `room` bytes (none if the first does not fit).
    pub(crate) fn take_within(&self, class: Class, room: usize) -> Vec<Bytes> {
        let mut q = self.queues.lock().unwrap();
        let mut batch = Vec::new();
        let mut taken = 0;
        let i = class.index();
        while let Some(front) = q.classes[i].front() {
            if taken + front.len() > room {
                break;
            }
            let msg = q.classes[i].pop_front().unwrap();
            q.bytes[i] -= msg.len();
            taken += msg.len();
            if class == Class::Video {
                self.video_left(msg.len(), q.bytes[i]);
            }
            self.traffic.sent(msg.len());
            batch.push(msg);
        }
        batch
    }

    /// Whether anything of `class` is queued.
    pub(crate) fn has(&self, class: Class) -> bool {
        !self.queues.lock().unwrap().classes[class.index()].is_empty()
    }

    /// Waits until `class` may have frames to take.
    pub(crate) async fn ready(&self, class: Class) {
        self.class_ready[class.index()].notified().await
    }

    /// Tells the connection to close with `code`; the first code wins.
    pub(crate) fn close(&self, code: u16) {
        self.close.send_if_modified(|current| current.is_none() && current.replace(code).is_none());
    }
}

/// What a connection's reader got.
pub(crate) enum Received {
    Binary(Bytes),
    Text(String),
    /// A message over the frame size limit.
    TooBig,
    /// Something the transport does not allow.
    Invalid,
    /// The connection ended or failed.
    Closed,
}

/// Where a group connection's messages come from.
pub(crate) trait Link: Send {
    fn recv(&mut self) -> impl Future<Output = Received> + Send;
}

impl GroupRoom {
    /// Seats a connection (GROUP.md §3.2) and returns its slot, or `None`
    /// if the group is full.
    fn seat(&self, id: u64, token: [u8; gwire::TOKEN_LEN], outbox: Arc<Outbox>, max: usize) -> Option<u8> {
        let mut slots = self.slots.lock().unwrap();
        let slot = match slots.iter().position(|s| s.as_ref().is_some_and(|s| s.token == token)) {
            Some(slot) => {
                let old = slots[slot].take().unwrap();
                old.outbox.close(close::REPLACED);
                for seat in slots.iter().flatten() {
                    seat.outbox.purge(slot as u8);
                }
                slot
            }
            None => {
                if slots.iter().flatten().count() >= max.min(gwire::MAX_SLOTS) {
                    return None;
                }
                slots.iter().position(Option::is_none)?
            }
        };
        let present = slots.iter().enumerate().filter(|(_, s)| s.is_some()).fold(0u32, |m, (i, _)| m | 1 << i);
        outbox.push(Class::Command, Bytes::copy_from_slice(&gwire::welcome(slot as u8, present)));
        for seat in slots.iter().flatten() {
            seat.outbox.push(Class::Command, member(slot as u8, true));
        }
        slots[slot] = Some(Seat { id, token, outbox });
        let mut router = self.router.lock().unwrap();
        router.seat(slot as u8);
        tell_demands(&slots, &mut router);
        Some(slot as u8)
    }

    /// The connection in `slot` wants `layer` of `other`'s video `track`.
    fn subscribe(&self, id: u64, slot: u8, other: u8, track: u8, layer: Option<u8>) {
        let slots = self.slots.lock().unwrap();
        if holds(&slots, slot, id) {
            let mut router = self.router.lock().unwrap();
            router.subscribe(slot, other, track, layer);
            tell_demands(&slots, &mut router);
        }
    }

    /// Fans a frame out. Returns the recipients that are behind (to wait
    /// for), or `None` once this connection no longer holds its slot.
    fn forward(
        &self,
        id: u64,
        slot: u8,
        to: Option<u8>,
        class: Class,
        msg: Bytes,
        ceiling: u8,
    ) -> Option<Vec<Arc<Outbox>>> {
        let slots = self.slots.lock().unwrap();
        if !holds(&slots, slot, id) {
            return None;
        }
        // Video to everyone goes to each in the layer it wants (GROUP.md §3.3).
        let mut router = (class == Class::Video && to.is_none()).then(|| self.router.lock().unwrap());
        if let Some(router) = &mut router {
            router.set_ceiling(ceiling);
        }
        let now = now_ms();
        let mut behind = Vec::new();
        for (i, seat) in slots.iter().enumerate() {
            if let Some(seat) = seat
                && to.map_or(i != slot as usize, |to| i == to as usize)
                && router.as_mut().is_none_or(|r| {
                    let (congested, dropped) = seat.outbox.video_link();
                    if r.link(now, i as u8, congested, dropped) {
                        seat.outbox.drop_video();
                    }
                    r.forward(now, slot, i as u8, msg[0])
                })
                && seat.outbox.push(class, msg.clone())
            {
                behind.push(seat.outbox.clone());
            }
        }
        if let Some(router) = &mut router {
            tell_demands(&slots, router);
        }
        Some(behind)
    }

    fn leave(&self, id: u64, slot: u8) {
        let mut slots = self.slots.lock().unwrap();
        if !holds(&slots, slot, id) {
            return; // replaced: the others were told then
        }
        slots[slot as usize] = None;
        for seat in slots.iter().flatten() {
            seat.outbox.purge(slot);
            seat.outbox.push(Class::Command, member(slot, false));
        }
        let mut router = self.router.lock().unwrap();
        router.leave(slot);
        tell_demands(&slots, &mut router);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.slots.lock().unwrap().iter().all(Option::is_none)
    }
}

/// Paces a sender: `rate` bytes per second, bursts of `burst`.
struct Bucket {
    tokens: f64,
    at: Instant,
    rate: f64,
    burst: f64,
}

impl Bucket {
    fn new(rate: usize, burst: usize) -> Bucket {
        Bucket { tokens: burst as f64, at: Instant::now(), rate: rate as f64, burst: burst as f64 }
    }

    /// Takes `len` bytes; returns how long to wait first if over the rate,
    /// or `None` (and takes nothing) if `drop_if_over`.
    fn take(&mut self, len: usize, drop_if_over: bool) -> Option<Duration> {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * self.rate).min(self.burst);
        self.at = now;
        let len = len as f64;
        if self.tokens >= len {
            self.tokens -= len;
            return Some(Duration::ZERO);
        }
        if drop_if_over {
            return None;
        }
        let wait = (len - self.tokens) / self.rate;
        self.tokens -= len;
        Some(Duration::from_secs_f64(wait))
    }
}

impl Relay {
    pub(crate) fn group_outbox(&self) -> Arc<Outbox> {
        let config = &self.config;
        Outbox::new(config.group_queue_bytes, config.voice_queue_bytes, config.video_queue_bytes, self.traffic.clone())
    }

    /// Runs a group connection: reads `link`, while `writer` drains `outbox`.
    pub(crate) async fn group_connection(
        self: Arc<Self>,
        group_id: RoomId,
        mut link: impl Link,
        outbox: Arc<Outbox>,
        writer: JoinHandle<()>,
        _slot: OwnedSemaphorePermit,
    ) {
        let config = &self.config;
        let mut closing = outbox.close.subscribe();
        let (pending, hello) = GroupConn::new();
        outbox.push(Class::Command, Bytes::copy_from_slice(&hello));
        let join_deadline = Instant::now() + gwire::JOIN_TIMEOUT;
        let mut seated: Option<(Arc<GroupRoom>, u64, u8)> = None;
        let mut bucket = Bucket::new(config.group_rate, config.group_burst);

        loop {
            let deadline = if seated.is_some() { Instant::now() + gwire::IDLE_TIMEOUT } else { join_deadline };
            let msg = tokio::select! {
                biased;
                _ = closing.changed() => break, // replaced, overflowed, or closed by the relay
                msg = timeout_at(deadline, link.recv()) => msg,
            };
            let msg = match msg {
                Ok(Received::Binary(frame)) => Ok(frame),
                Ok(Received::Text(text)) => Err(text),
                Ok(Received::TooBig) => break outbox.close(close::TOO_BIG),
                Ok(Received::Invalid) => break outbox.close(close::PROTOCOL_ERROR),
                Ok(Received::Closed) => break,
                Err(_) => break outbox.close(close::TIMEOUT),
            };
            let incoming = match &msg {
                Ok(frame) => Incoming::Binary(frame),
                Err(text) => Incoming::Text(text),
            };
            let conn = seated.as_ref().map_or(pending, |&(_, _, slot)| GroupConn::Seated(slot));
            match conn.receive(&group_id, incoming) {
                GroupAction::Pong => {
                    outbox.push(Class::Command, Bytes::new());
                }
                GroupAction::Join(token) => {
                    let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let room = self.groups.lock().unwrap().entry(group_id).or_default().clone();
                    match room.seat(id, token, outbox.clone(), config.max_group_members) {
                        Some(slot) => seated = Some((room, id, slot)),
                        None => {
                            drop(room);
                            self.collect_group(&group_id);
                            break outbox.close(close::FULL);
                        }
                    }
                }
                GroupAction::Forward { to, class } => {
                    let Ok(msg) = msg else { unreachable!("only binary frames are forwarded") };
                    let (room, id, slot) = seated.as_ref().expect("only seated connections forward");
                    match bucket.take(msg.len(), class.is_realtime()) {
                        None => continue, // over the rate: voice is dropped
                        Some(wait) if !wait.is_zero() => sleep(wait).await,
                        Some(_) => {}
                    }
                    let Some(behind) = room.forward(*id, *slot, to, class, msg, self.traffic.ceiling()) else {
                        break; // replaced
                    };
                    // Hold this sender back until slow recipients catch up
                    // (or are dropped), reading nothing more from it.
                    for outbox in behind {
                        tokio::select! {
                            biased;
                            _ = closing.changed() => break,
                            () = outbox.catch_up() => {}
                        }
                    }
                }
                GroupAction::Subscribe { slot: other, track, layer } => {
                    if let Some((room, id, slot)) = &seated {
                        room.subscribe(*id, *slot, other, track, layer);
                    }
                }
                GroupAction::Close(code) => break outbox.close(code),
            }
        }

        if let Some((room, id, slot)) = seated {
            room.leave(id, slot);
            drop(room);
            self.collect_group(&group_id);
        }
        outbox.close(close::NORMAL);
        let _ = writer.await;
    }

    /// Forgets a group once nobody is in it or about to be.
    fn collect_group(&self, group_id: &RoomId) {
        let mut groups = self.groups.lock().unwrap();
        if let Some(room) = groups.get(group_id)
            && room.is_empty()
            && Arc::strong_count(room) == 1
        {
            groups.remove(group_id);
        }
    }
}

fn member(slot: u8, present: bool) -> Bytes {
    Bytes::copy_from_slice(&gwire::member(slot, present))
}

fn holds(slots: &[Option<Seat>; gwire::MAX_SLOTS], slot: u8, id: u64) -> bool {
    slots[slot as usize].as_ref().is_some_and(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_sends_urgent_first_and_drops_late_voice() {
        let outbox = Outbox::new(1 << 20, 100, 1000, crate::traffic::Traffic::new());
        outbox.push(Class::Bulk, Bytes::from(vec![0x61, 0, 0xFF]));
        outbox.push(Class::Chat, Bytes::from(vec![0x41, 0, 0xFF]));
        outbox.push(Class::Voice, Bytes::from(vec![0x21, 0, 0xFF]));
        outbox.push(Class::Command, Bytes::from(vec![0x03, 0, 0xFF]));
        outbox.push(Class::Video, Bytes::from(vec![0x31, 0, 0xFF]));
        let order: Vec<u8> = outbox.take().iter().map(|m| m[0]).collect();
        assert_eq!(order, [0x03, 0x21, 0x41, 0x31, 0x61], "commands, voice, chat, video, bulk");

        for _ in 0..10 {
            assert!(!outbox.push(Class::Voice, Bytes::from(vec![0x21; 30])));
        }
        assert_eq!(outbox.take().len(), 3, "voice beyond 100 queued bytes is dropped");
        // Bulk does not count against voice: it waits behind it anyway.
        outbox.push(Class::Bulk, Bytes::from(vec![0x61; 1000]));
        outbox.push(Class::Voice, Bytes::from(vec![0x21; 30]));
        assert_eq!(outbox.take().len(), 2);
    }

    #[test]
    fn video_beyond_its_budget_loses_its_oldest() {
        let outbox = Outbox::new(1 << 20, 100, 1000, crate::traffic::Traffic::new());
        for i in 0..5u8 {
            outbox
                .push(Class::Video, Bytes::from(vec![0x31, i, 0xFF].into_iter().chain([0; 297]).collect::<Vec<u8>>()));
        }
        let kept: Vec<u8> = outbox.take().iter().map(|m| m[1]).collect();
        assert_eq!(kept, [2, 3, 4], "1000 bytes of the newest");
        // Video does not count towards the reliable limit, nor towards voice's.
        for _ in 0..3 {
            outbox.push(Class::Video, Bytes::from(vec![0x31; 300]));
        }
        outbox.push(Class::Voice, Bytes::from(vec![0x21; 90]));
        assert_eq!(outbox.take().len(), 4, "the voice frame too");
        assert_eq!(*outbox.close.borrow(), None);
    }

    #[test]
    fn outbox_overflow_closes_and_purge_forgets_a_slot() {
        let outbox = Outbox::new(100, 100, 1000, crate::traffic::Traffic::new());
        outbox.push(Class::Chat, Bytes::from(vec![0x41, 3, 0xFF, 0]));
        outbox.push(Class::Chat, Bytes::from(vec![0x41, 4, 0xFF, 0]));
        outbox.push(Class::Command, Bytes::copy_from_slice(&gwire::member(3, true)));
        outbox.purge(3);
        let left: Vec<Vec<u8>> = outbox.take().into_iter().map(|m| m.to_vec()).collect();
        assert_eq!(left, [gwire::member(3, true).to_vec(), vec![0x41, 4, 0xFF, 0]]);
        assert_eq!(*outbox.close.borrow(), None);
        outbox.push(Class::Bulk, Bytes::from(vec![0x61; 101]));
        assert_eq!(*outbox.close.borrow(), Some(close::TIMEOUT));
    }

    #[tokio::test]
    async fn a_backlog_holds_senders_back_until_it_drains() {
        let outbox = Outbox::new(4 << 20, 100, 1000, crate::traffic::Traffic::new());
        let mut behind = false;
        for _ in 0..20 {
            behind = outbox.push(Class::Chat, Bytes::from(vec![0x41; 16 << 10]));
        }
        assert!(behind);
        // Bulk does not count: it has its own window.
        let bulk = Outbox::new(4 << 20, 100, 1000, crate::traffic::Traffic::new());
        assert!(!bulk.push(Class::Bulk, Bytes::from(vec![0x61; 1 << 20])));

        let reader = outbox.clone();
        let drain = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            while !reader.take().is_empty() {}
        });
        timeout(Duration::from_secs(1), outbox.catch_up()).await.unwrap();
        drain.await.unwrap();
        assert_eq!(*outbox.close.borrow(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_recipient_that_does_not_catch_up_is_dropped() {
        let outbox = Outbox::new(4 << 20, 100, 1000, crate::traffic::Traffic::new());
        for _ in 0..20 {
            outbox.push(Class::Command, Bytes::from(vec![0x03; 16 << 10]));
        }
        outbox.catch_up().await;
        assert_eq!(*outbox.close.borrow(), Some(close::TIMEOUT));
    }

    #[test]
    fn bucket_paces_and_drops() {
        let mut bucket = Bucket::new(1000, 1000);
        assert_eq!(bucket.take(1000, false), Some(Duration::ZERO));
        assert_eq!(bucket.take(10, true), None);
        let wait = bucket.take(500, false).unwrap();
        assert!(wait > Duration::from_millis(400) && wait <= Duration::from_millis(500), "{wait:?}");
    }
}
