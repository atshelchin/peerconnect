//! Video (GROUP.md §7.4): frames cut into fragments that each fit a QUIC
//! datagram, put back together by receivers, a fragment resent once when a
//! receiver says it is missing, and a keyframe asked for when a frame is
//! lost for good. A track may be sent in several layers (sizes), each
//! numbered and put back together on its own; a receiver plays one layer
//! of each track at a time, and moves to another at one of its keyframes.
//! Free of I/O and crypto: the member seals what this says.

use std::collections::{BTreeMap, VecDeque};

use super::wire::{MAX_VIDEO_FRAGMENT, MAX_VIDEO_FRAME, VIDEO_HEADER_LEN};

/// Fragment flags.
pub(crate) const KEYFRAME: u8 = 0x01;
/// In a NACK, "every fragment of the frame" (none of it arrived).
pub(crate) const WHOLE_FRAME: u8 = 0xFF;

/// A frame missing a fragment waits this long before it is asked for
/// (fragments of a frame travel together; this covers mild reordering).
const NACK_DELAY: u64 = 5;
/// And is given up after this long without a fragment of it arriving:
/// frames behind it wait meanwhile.
const HOLD: u64 = 300;
/// The end of a frame is asked for only once its fragments stopped coming
/// this long: a large keyframe arrives over a while, and asking for what is
/// still on its way would only send it twice.
const TAIL_STALL: u64 = 30;
/// How often to ask for a keyframe, at most, per sender and track.
const KEYFRAME_RETRY: u64 = 500;
/// Frames kept by a sender for resending: this long, and this many bytes.
const KEEP_FOR: u64 = 500;
const KEEP_BYTES: usize = 2 << 20;
/// What a sender resends, at most: bytes per second, in bursts of at most a
/// quarter of that.
const RESEND_RATE: f64 = 512.0 * 1024.0;
const RESEND_BURST: f64 = RESEND_RATE / 4.0;
/// Frames a receiver holds per sender, track and layer, waiting for missing parts.
const HOLD_FRAMES: usize = 64;
/// Frames of a layer a receiver just moved away from are ignored this long:
/// they were already on their way, and are not the start of a move back.
const LEFT_GRACE: u64 = 1000;

/// A fragment's plaintext: `track frame[4] index count flags timestamp[4] ‖ data`.
pub(crate) fn fragments(track: u8, frame: u32, keyframe: bool, timestamp: u32, data: &[u8]) -> Option<Vec<Vec<u8>>> {
    if data.is_empty() || data.len() > MAX_VIDEO_FRAME {
        return None;
    }
    let chunks: Vec<&[u8]> = data.chunks(MAX_VIDEO_FRAGMENT).collect();
    let count = chunks.len() as u8;
    let flags = if keyframe { KEYFRAME } else { 0 };
    Some(
        chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                let mut p = Vec::with_capacity(VIDEO_HEADER_LEN + chunk.len());
                p.push(track);
                p.extend_from_slice(&frame.to_be_bytes());
                p.extend_from_slice(&[index as u8, count, flags]);
                p.extend_from_slice(&timestamp.to_be_bytes());
                p.extend_from_slice(chunk);
                p
            })
            .collect(),
    )
}

/// A parsed fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fragment<'a> {
    pub track: u8,
    pub frame: u32,
    pub index: u8,
    pub count: u8,
    pub keyframe: bool,
    pub timestamp: u32,
    pub data: &'a [u8],
}

pub(crate) fn parse(plaintext: &[u8]) -> Option<Fragment<'_>> {
    let header = plaintext.get(..VIDEO_HEADER_LEN)?;
    let (index, count) = (header[5], header[6]);
    if count == 0 || index >= count {
        return None;
    }
    Some(Fragment {
        track: header[0],
        frame: u32::from_be_bytes(header[1..5].try_into().unwrap()),
        index,
        count,
        keyframe: header[7] & KEYFRAME != 0,
        timestamp: u32::from_be_bytes(header[8..12].try_into().unwrap()),
        data: &plaintext[VIDEO_HEADER_LEN..],
    })
}

/// `a` comes before `b`, frame ids wrapping.
fn before(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

// The sending side.

/// Our recent frames, per track and layer, for resending.
pub(crate) struct Outgoing {
    tracks: Vec<TrackOut>,
    tokens: f64,
    tokens_at: u64,
    /// When we last told the application a layer's keyframe is wanted, by track and layer.
    keyframe_told: Vec<((u8, u8), u64)>,
}

impl Default for Outgoing {
    /// With a full resend allowance: the first loss is answered at once.
    fn default() -> Self {
        Outgoing { tracks: Vec::new(), tokens: RESEND_BURST, tokens_at: 0, keyframe_told: Vec::new() }
    }
}

struct TrackOut {
    track: u8,
    layer: u8,
    next: u32,
    recent: VecDeque<Sent>,
    bytes: usize,
}

struct Sent {
    frame: u32,
    at: u64,
    fragments: Vec<Vec<u8>>,
}

impl Outgoing {
    /// Cuts a frame and keeps it; returns its fragments' plaintexts.
    pub(crate) fn send(
        &mut self,
        now: u64,
        track: u8,
        layer: u8,
        keyframe: bool,
        timestamp: u32,
        data: &[u8],
    ) -> Option<Vec<Vec<u8>>> {
        let t = match self.tracks.iter().position(|t| (t.track, t.layer) == (track, layer)) {
            Some(i) => &mut self.tracks[i],
            None => {
                self.tracks.push(TrackOut { track, layer, next: 0, recent: VecDeque::new(), bytes: 0 });
                self.tracks.last_mut().unwrap()
            }
        };
        let frame = t.next;
        let fragments = fragments(track, frame, keyframe, timestamp, data)?;
        t.next = t.next.wrapping_add(1);
        t.bytes += data.len();
        t.recent.push_back(Sent { frame, at: now, fragments: fragments.clone() });
        while t.recent.front().is_some_and(|s| now.saturating_sub(s.at) > KEEP_FOR) || t.bytes > KEEP_BYTES {
            let old = t.recent.pop_front().unwrap();
            t.bytes -= old.fragments.iter().map(|f| f.len() - VIDEO_HEADER_LEN).sum::<usize>();
        }
        Some(fragments)
    }

    /// The fragments a receiver asked for again (`NACK`), those still kept
    /// and within the resend rate.
    pub(crate) fn resend(&mut self, now: u64, track: u8, layer: u8, frame: u32, indices: &[u8]) -> Vec<Vec<u8>> {
        let elapsed = now.saturating_sub(self.tokens_at) as f64 / 1000.0;
        self.tokens = (self.tokens + elapsed * RESEND_RATE).min(RESEND_BURST);
        self.tokens_at = now;
        let Some(t) = self.tracks.iter().find(|t| (t.track, t.layer) == (track, layer)) else { return Vec::new() };
        let Some(sent) = t.recent.iter().find(|s| s.frame == frame && now.saturating_sub(s.at) <= KEEP_FOR) else {
            return Vec::new();
        };
        let wanted: Vec<usize> = if indices == [WHOLE_FRAME] {
            (0..sent.fragments.len()).collect()
        } else {
            indices.iter().map(|&i| i as usize).filter(|&i| i < sent.fragments.len()).collect()
        };
        let mut out = Vec::new();
        for i in wanted {
            let fragment = &sent.fragments[i];
            if self.tokens < fragment.len() as f64 {
                break;
            }
            self.tokens -= fragment.len() as f64;
            out.push(fragment.clone());
        }
        out
    }

    /// Whether to tell the application that `layer` of `track` needs a
    /// keyframe: once per [`KEYFRAME_RETRY`], however many receivers ask.
    pub(crate) fn keyframe_wanted(&mut self, now: u64, track: u8, layer: u8) -> bool {
        let track = (track, layer);
        match self.keyframe_told.iter_mut().find(|(t, _)| *t == track) {
            Some((_, at)) if now.saturating_sub(*at) < KEYFRAME_RETRY => false,
            Some((_, at)) => {
                *at = now;
                true
            }
            None => {
                self.keyframe_told.push((track, now));
                true
            }
        }
    }
}

// The receiving side.

/// What the member does about a sender's video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// A whole frame, in order, decodable (it follows a keyframe without loss).
    Deliver { track: u8, layer: u8, keyframe: bool, timestamp: u32, data: Vec<u8> },
    /// Ask the sender for these fragments of `frame` again.
    Nack { track: u8, layer: u8, frame: u32, indices: Vec<u8> },
    /// Ask the sender for a keyframe.
    RequestKeyframe { track: u8, layer: u8 },
}

/// One sender's video, per track and layer.
#[derive(Default)]
pub(crate) struct Incoming {
    tracks: Vec<TrackIn>,
    /// Per track: the layer delivered, and the layers moved away from, when.
    playing: Vec<Playing>,
}

struct Playing {
    track: u8,
    layer: u8,
    left: Vec<(u8, u64)>,
}

#[derive(Default)]
struct TrackIn {
    track: u8,
    layer: u8,
    /// The next frame to deliver, once one has arrived.
    next: Option<u32>,
    /// Frames from `next` on, as far as they have arrived.
    frames: BTreeMap<u64, Partial>,
    /// Lost something: deltas are dropped until a keyframe.
    broken: bool,
    keyframe_asked: Option<u64>,
    /// When `next` started holding up a later frame.
    blocked_since: Option<u64>,
    nacks: u8,
}

struct Partial {
    frame: u32,
    count: u8,
    keyframe: bool,
    timestamp: u32,
    parts: Vec<Option<Vec<u8>>>,
    have: u8,
    /// When its latest fragment arrived, and the highest index arrived.
    last_at: u64,
    highest: u8,
}

impl Partial {
    /// The fragments to ask for now: those missing before the highest one
    /// arrived (lost, or overtaken), and those after it once they stopped coming.
    fn missing(&self, now: u64) -> Vec<u8> {
        let stalled = now.saturating_sub(self.last_at) >= TAIL_STALL;
        (0..self.count).filter(|&i| self.parts[i as usize].is_none() && (i < self.highest || stalled)).collect()
    }

    /// When something of it can first be asked for: now if a fragment is
    /// missing before the highest arrived, else once its end stopped coming.
    fn askable_at(&self) -> Option<u64> {
        let missing = |i: u8| self.parts[i as usize].is_none();
        if (0..self.highest).any(missing) {
            Some(0)
        } else {
            (self.highest + 1..self.count).any(missing).then_some(self.last_at + TAIL_STALL)
        }
    }
}

impl TrackIn {
    /// The key of `frame` in `frames`: its distance from `next`.
    fn key(&self, frame: u32) -> u64 {
        frame.wrapping_sub(self.next.unwrap_or(frame)) as u64
    }
}

impl Incoming {
    /// A fragment of `layer`, the layer its type said.
    pub(crate) fn push(&mut self, now: u64, layer: u8, plaintext: &[u8], out: &mut Vec<Action>) {
        let Some(f) = parse(plaintext) else { return };
        if let Some(p) = self.playing.iter_mut().find(|p| p.track == f.track) {
            p.left.retain(|&(_, at)| now.saturating_sub(at) < LEFT_GRACE);
            if p.layer != layer && p.left.iter().any(|&(l, _)| l == layer) {
                return; // still arriving from before the move
            }
        }
        let i = match self.tracks.iter().position(|t| (t.track, t.layer) == (f.track, layer)) {
            Some(i) => i,
            None => {
                self.tracks.push(TrackIn { track: f.track, layer, broken: true, ..TrackIn::default() });
                self.tracks.len() - 1
            }
        };
        let mut actions = Vec::new();
        Self::assemble(&mut self.tracks[i], now, f, &mut actions);
        self.play(now, actions, out);
    }

    /// Passes on what the layers' assemblers decided, delivering one layer
    /// per track: another layer takes over with its first keyframe.
    fn play(&mut self, now: u64, actions: Vec<Action>, out: &mut Vec<Action>) {
        for action in actions {
            let Action::Deliver { track, layer, keyframe, .. } = action else {
                out.push(action);
                continue;
            };
            match self.playing.iter_mut().find(|p| p.track == track) {
                Some(p) if p.layer == layer => {}
                Some(p) if keyframe => {
                    let old = std::mem::replace(&mut p.layer, layer);
                    p.left.push((old, now));
                    self.tracks.retain(|t| !(t.track == track && t.layer == old));
                }
                Some(_) => continue,
                None => self.playing.push(Playing { track, layer, left: Vec::new() }),
            }
            out.push(action);
        }
    }

    fn assemble(t: &mut TrackIn, now: u64, f: Fragment<'_>, out: &mut Vec<Action>) {
        let next = *t.next.get_or_insert(f.frame);
        if before(f.frame, next) {
            return; // delivered or given up already
        }
        let key = t.key(f.frame);
        if key >= HOLD_FRAMES as u64 {
            // Far ahead: give up on everything before it.
            t.frames.retain(|&k, _| k >= key);
            t.next = Some(f.frame);
            t.frames = std::mem::take(&mut t.frames).into_iter().map(|(k, p)| (k - key, p)).collect();
            t.broken = true;
            t.blocked_since = None;
        }
        let key = t.key(f.frame);
        let partial = t.frames.entry(key).or_insert_with(|| Partial {
            frame: f.frame,
            count: f.count,
            keyframe: f.keyframe,
            timestamp: f.timestamp,
            parts: vec![None; f.count as usize],
            have: 0,
            last_at: now,
            highest: 0,
        });
        if partial.count != f.count || partial.parts[f.index as usize].is_some() {
            return; // inconsistent, or a duplicate
        }
        partial.parts[f.index as usize] = Some(f.data.to_vec());
        partial.have += 1;
        partial.last_at = now;
        partial.highest = partial.highest.max(f.index);
        Self::advance(t, now, out);
    }

    /// Delivers what is complete from `next` on, and keeps track of what holds it up.
    fn advance(t: &mut TrackIn, now: u64, out: &mut Vec<Action>) {
        loop {
            let complete = t.frames.get(&0).is_some_and(|p| p.have == p.count);
            if !complete {
                break;
            }
            let p = t.frames.remove(&0).unwrap();
            t.next = Some(p.frame.wrapping_add(1));
            t.frames = std::mem::take(&mut t.frames).into_iter().map(|(k, p)| (k - 1, p)).collect();
            t.blocked_since = None;
            t.nacks = 0;
            if p.keyframe {
                t.broken = false;
            }
            if t.broken {
                Self::want_keyframe(t, now, out);
                continue;
            }
            let data = p.parts.into_iter().flatten().flatten().collect();
            out.push(Action::Deliver {
                track: t.track,
                layer: t.layer,
                keyframe: p.keyframe,
                timestamp: p.timestamp,
                data,
            });
        }
        if !t.frames.is_empty() {
            t.blocked_since.get_or_insert(now);
        }
    }

    fn want_keyframe(t: &mut TrackIn, now: u64, out: &mut Vec<Action>) {
        if t.keyframe_asked.is_none_or(|at| now.saturating_sub(at) >= KEYFRAME_RETRY) {
            t.keyframe_asked = Some(now);
            out.push(Action::RequestKeyframe { track: t.track, layer: t.layer });
        }
    }

    /// Asks for what is missing, and gives up on what is too late.
    pub(crate) fn tick(&mut self, now: u64, rtt: u64, out: &mut Vec<Action>) {
        let mut actions = Vec::new();
        self.tick_layers(now, rtt, &mut actions);
        self.play(now, actions, out);
    }

    fn tick_layers(&mut self, now: u64, rtt: u64, out: &mut Vec<Action>) {
        for t in &mut self.tracks {
            let Some(since) = t.blocked_since else { continue };
            if now.saturating_sub(Self::progress(t, since)) >= HOLD {
                // Give up on the frame at `next`, missing or incomplete.
                let next = t.next.unwrap();
                t.frames.remove(&0);
                t.next = Some(next.wrapping_add(1));
                t.frames = std::mem::take(&mut t.frames).into_iter().map(|(k, p)| (k - 1, p)).collect();
                t.broken = true;
                t.blocked_since = None;
                t.nacks = 0;
                Self::want_keyframe(t, now, out);
                Self::advance(t, now, out);
                continue;
            }
            let due = NACK_DELAY + u64::from(t.nacks) * (rtt.max(20) + NACK_DELAY);
            if t.nacks < 2 && now.saturating_sub(since) >= due {
                let next = t.next.unwrap();
                let highest = t.frames.keys().next_back().copied().unwrap_or(0);
                let nacks: Vec<Action> = (0..=highest)
                    .filter_map(|key| {
                        let indices = match t.frames.get(&key) {
                            Some(p) => p.missing(now),
                            None => vec![WHOLE_FRAME],
                        };
                        let frame = next.wrapping_add(key as u32);
                        (!indices.is_empty()).then_some(Action::Nack { track: t.track, layer: t.layer, frame, indices })
                    })
                    .collect();
                // Nothing to ask for yet (a frame still arriving) uses up no try.
                if !nacks.is_empty() {
                    t.nacks += 1;
                    out.extend(nacks);
                }
            }
        }
    }

    /// When the frame holding `t` up last made progress: its latest fragment,
    /// or when it started holding things up.
    fn progress(t: &TrackIn, since: u64) -> u64 {
        t.frames.get(&0).map_or(since, |p| p.last_at.max(since))
    }

    /// The tracks seen from this sender.
    pub(crate) fn tracks(&self) -> Vec<u8> {
        let mut tracks: Vec<u8> = self.tracks.iter().map(|t| t.track).collect();
        tracks.sort_unstable();
        tracks.dedup();
        tracks
    }

    /// When [`Incoming::tick`] has something to do.
    pub(crate) fn deadline(&self, rtt: u64) -> Option<u64> {
        self.tracks
            .iter()
            .filter_map(|t| {
                let since = t.blocked_since?;
                let hold = Self::progress(t, since) + HOLD;
                let due = since + NACK_DELAY + u64::from(t.nacks) * (rtt.max(20) + NACK_DELAY);
                // The first time anything can be asked for: a frame missing whole
                // or a hole at once, the end of a frame once it stopped coming.
                let highest = t.frames.keys().next_back().copied().unwrap_or(0);
                let askable =
                    (0..=highest).filter_map(|key| t.frames.get(&key).map_or(Some(0), Partial::askable_at)).min();
                let nack = askable.filter(|_| t.nacks < 2).map(|at| at.max(due));
                Some(nack.map_or(hold, |n| n.min(hold)))
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(data_len: usize, seed: u8) -> Vec<u8> {
        (0..data_len).map(|i| seed.wrapping_add(i as u8)).collect()
    }

    fn delivered(out: &[Action]) -> Vec<(bool, u32, usize)> {
        out.iter()
            .filter_map(|a| match a {
                Action::Deliver { keyframe, timestamp, data, .. } => Some((*keyframe, *timestamp, data.len())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fragments_round_trip() {
        let data = frame(2500, 1);
        let parts = fragments(3, 7, true, 99, &data).unwrap();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.len() <= VIDEO_HEADER_LEN + MAX_VIDEO_FRAGMENT));
        let f = parse(&parts[2]).unwrap();
        assert_eq!((f.track, f.frame, f.index, f.count, f.keyframe, f.timestamp), (3, 7, 2, 3, true, 99));
        assert!(fragments(0, 0, false, 0, &[]).is_none());
        assert!(fragments(0, 0, false, 0, &vec![0; MAX_VIDEO_FRAME + 1]).is_none());
        assert_eq!(fragments(0, 0, false, 0, &vec![0; MAX_VIDEO_FRAME]).unwrap().len(), 255);
        assert!(parse(&[0; 5]).is_none());
        assert!(parse(&[0, 0, 0, 0, 0, 3, 3, 0, 0, 0, 0, 0]).is_none(), "index beyond count");
    }

    #[test]
    fn frames_arrive_whole_and_in_order_even_shuffled() {
        let mut tx = Outgoing::default();
        let mut rx = Incoming::default();
        let mut out = Vec::new();
        let mut all = Vec::new();
        for i in 0..5u32 {
            all.extend(tx.send(0, 0, 0, i == 0, i * 33, &frame(1500 + i as usize, i as u8)).unwrap());
        }
        // Out of order, but the first to arrive is the first frame's.
        all.swap(1, 4);
        all.swap(2, 7);
        for p in &all {
            rx.push(1, 0, p, &mut out);
        }
        assert_eq!(
            delivered(&out),
            [(true, 0, 1500), (false, 33, 1501), (false, 66, 1502), (false, 99, 1503), (false, 132, 1504)]
        );
        assert!(out.iter().all(|a| matches!(a, Action::Deliver { .. })));
    }

    #[test]
    fn a_lost_fragment_is_asked_for_and_resent() {
        let mut tx = Outgoing::default();
        let mut rx = Incoming::default();
        let mut out = Vec::new();
        let key = tx.send(0, 0, 0, true, 0, &frame(3000, 0)).unwrap();
        let next = tx.send(0, 0, 0, false, 33, &frame(800, 1)).unwrap();
        rx.push(1, 0, &key[0], &mut out);
        rx.push(1, 0, &key[2], &mut out); // key[1] is lost
        rx.push(2, 0, &next[0], &mut out);
        assert!(delivered(&out).is_empty(), "held until the keyframe is whole");
        assert_eq!(rx.deadline(50), Some(1 + NACK_DELAY));
        rx.tick(1 + NACK_DELAY, 50, &mut out);
        let nack = out.iter().find_map(|a| {
            if let Action::Nack { frame, indices, .. } = a { Some((*frame, indices.clone())) } else { None }
        });
        assert_eq!(nack, Some((0, vec![1])));
        let again = tx.resend(30, 0, 0, 0, &[1]);
        assert_eq!(again, vec![key[1].clone()]);
        rx.push(40, 0, &again[0], &mut out);
        assert_eq!(delivered(&out), [(true, 0, 3000), (false, 33, 800)]);
    }

    #[test]
    fn a_frame_missing_only_its_end_is_asked_for() {
        let mut rx = Incoming::default();
        let mut out = Vec::new();
        let parts = fragments(0, 0, true, 0, &frame(2500, 0)).unwrap();
        rx.push(0, 0, &parts[0], &mut out);
        rx.push(0, 0, &parts[1], &mut out);
        rx.tick(NACK_DELAY, 20, &mut out);
        assert!(out.is_empty(), "the end may still be on its way");
        assert_eq!(rx.deadline(20), Some(TAIL_STALL));
        rx.tick(TAIL_STALL, 20, &mut out);
        assert_eq!(out, [Action::Nack { track: 0, layer: 0, frame: 0, indices: vec![2] }]);
    }

    #[test]
    fn a_large_keyframe_arriving_slowly_is_waited_for_not_asked_for() {
        let mut rx = Incoming::default();
        let mut out = Vec::new();
        // 200 fragments, one every 3 ms over a slow link: 600 ms, twice HOLD.
        let parts = fragments(0, 0, true, 0, &frame(200_000, 0)).unwrap();
        for (i, part) in parts.iter().enumerate() {
            let now = 3 * i as u64;
            rx.tick(now, 20, &mut out);
            rx.push(now, 0, part, &mut out);
        }
        assert_eq!(delivered(&out), [(true, 0, 200_000)]);
        assert!(
            !out.iter().any(|a| matches!(a, Action::Nack { .. } | Action::RequestKeyframe { .. })),
            "{:?}",
            out.len()
        );
    }

    #[test]
    fn a_frame_lost_for_good_drops_deltas_until_a_keyframe() {
        let mut tx = Outgoing::default();
        let mut rx = Incoming::default();
        let mut out = Vec::new();
        let f0 = tx.send(0, 0, 0, true, 0, &frame(100, 0)).unwrap();
        let _f1 = tx.send(0, 0, 0, false, 1, &frame(100, 1)).unwrap(); // never arrives
        let f2 = tx.send(0, 0, 0, false, 2, &frame(100, 2)).unwrap();
        rx.push(0, 0, &f0[0], &mut out);
        rx.push(5, 0, &f2[0], &mut out);
        rx.tick(5 + NACK_DELAY, 20, &mut out);
        assert!(out.iter().any(|a| matches!(a, Action::Nack { frame: 1, indices, .. } if indices == &[WHOLE_FRAME])));
        rx.tick(5 + HOLD, 20, &mut out);
        assert!(out.iter().any(|a| matches!(a, Action::RequestKeyframe { track: 0, layer: 0 })));
        // Frame 2 depended on frame 1: not delivered.
        assert_eq!(delivered(&out), [(true, 0, 100)]);
        let f3 = tx.send(400, 0, 0, false, 3, &frame(100, 3)).unwrap();
        let f4 = tx.send(400, 0, 0, true, 4, &frame(100, 4)).unwrap();
        rx.push(400, 0, &f3[0], &mut out);
        rx.push(401, 0, &f4[0], &mut out);
        assert_eq!(delivered(&out), [(true, 0, 100), (true, 4, 100)], "back with the keyframe");
    }

    #[test]
    fn a_receiver_that_starts_mid_stream_waits_for_a_keyframe() {
        let mut tx = Outgoing::default();
        let mut rx = Incoming::default();
        let mut out = Vec::new();
        for i in 0..3 {
            let _ = tx.send(0, 0, 0, i == 0, i, &frame(10, 0));
        }
        let delta = tx.send(0, 0, 0, false, 3, &frame(10, 0)).unwrap();
        rx.push(0, 0, &delta[0], &mut out);
        assert_eq!(out, [Action::RequestKeyframe { track: 0, layer: 0 }]);
        let key = tx.send(10, 0, 0, true, 4, &frame(10, 0)).unwrap();
        rx.push(10, 0, &key[0], &mut out);
        assert_eq!(delivered(&out), [(true, 4, 10)]);
    }

    #[test]
    fn the_first_loss_is_answered_at_once() {
        let mut tx = Outgoing::default();
        let f = tx.send(0, 0, 0, true, 0, &frame(1500, 0)).unwrap();
        assert_eq!(tx.resend(0, 0, 0, 0, &[1]), vec![f[1].clone()]);
    }

    #[test]
    fn resends_are_bounded_by_age_and_rate() {
        let mut tx = Outgoing::default();
        let f = tx.send(0, 0, 0, true, 0, &frame(MAX_VIDEO_FRAME, 0)).unwrap();
        assert!(tx.resend(KEEP_FOR + 1, 0, 0, 0, &[0]).is_empty(), "too old");
        let mut tx = Outgoing::default();
        let _ = tx.send(0, 0, 0, true, 0, &frame(MAX_VIDEO_FRAME, 0));
        let burst = tx.resend(400, 0, 0, 0, &[WHOLE_FRAME]);
        assert!(burst.len() < f.len() && !burst.is_empty(), "{} of {}", burst.len(), f.len());
        assert!(tx.resend(400, 0, 0, 0, &[200]).is_empty(), "the rate is spent");
        assert!(tx.resend(450, 0, 0, 9, &[0]).is_empty(), "unknown frame");
        assert!(tx.resend(450, 5, 0, 0, &[0]).is_empty(), "unknown track");
    }

    #[test]
    fn keyframe_requests_are_coalesced() {
        let mut tx = Outgoing::default();
        assert!(tx.keyframe_wanted(0, 0, 0));
        assert!(!tx.keyframe_wanted(100, 0, 0));
        assert!(tx.keyframe_wanted(100, 1, 0), "per track");
        assert!(tx.keyframe_wanted(KEYFRAME_RETRY, 0, 0));
    }

    #[test]
    fn frame_ids_wrap_and_far_jumps_resynchronize() {
        let mut rx = Incoming::default();
        let mut out = Vec::new();
        for (i, id) in [u32::MAX - 1, u32::MAX, 0, 1].into_iter().enumerate() {
            rx.push(i as u64, 0, &fragments(0, id, i == 0, id, &[1]).unwrap()[0], &mut out);
        }
        assert_eq!(delivered(&out).len(), 4);
        out.clear();
        rx.push(10, 0, &fragments(0, 1000, true, 1000, &[1]).unwrap()[0], &mut out);
        assert_eq!(delivered(&out), [(true, 1000, 1)]);
        rx.push(11, 0, &fragments(0, 999, false, 999, &[1]).unwrap()[0], &mut out);
        assert_eq!(delivered(&out).len(), 1, "older than what was delivered: ignored");
    }

    #[test]
    fn layers_are_numbered_and_resent_apart() {
        let mut tx = Outgoing::default();
        let low = tx.send(0, 0, 1, true, 0, &frame(1500, 1)).unwrap();
        let high = tx.send(0, 0, 2, true, 0, &frame(1500, 2)).unwrap();
        assert_eq!(parse(&low[0]).unwrap().frame, 0);
        assert_eq!(parse(&high[0]).unwrap().frame, 0, "each layer counts its own frames");
        assert_eq!(tx.resend(1, 0, 2, 0, &[1]), vec![high[1].clone()]);
        assert!(tx.keyframe_wanted(1, 0, 1));
        assert!(tx.keyframe_wanted(1, 0, 2), "per layer");
    }

    #[test]
    fn one_layer_plays_and_another_takes_over_at_its_keyframe() {
        let mut tx = Outgoing::default();
        let mut rx = Incoming::default();
        let mut out = Vec::new();
        let layers = |out: &[Action]| -> Vec<(u8, bool)> {
            out.iter()
                .filter_map(|a| match a {
                    Action::Deliver { layer, keyframe, .. } => Some((*layer, *keyframe)),
                    _ => None,
                })
                .collect()
        };
        // Layer 1 plays.
        for i in 0..3u32 {
            rx.push(i as u64, 1, &tx.send(0, 0, 1, i == 0, i, &frame(10, 0)).unwrap()[0], &mut out);
        }
        // Layer 2 starts mid-stream: its deltas are not played, and its keyframe is asked for.
        let _ = tx.send(0, 0, 2, true, 0, &frame(10, 0));
        rx.push(5, 2, &tx.send(5, 0, 2, false, 1, &frame(10, 0)).unwrap()[0], &mut out);
        assert!(out.contains(&Action::RequestKeyframe { track: 0, layer: 2 }));
        rx.push(6, 1, &tx.send(6, 0, 1, false, 3, &frame(10, 0)).unwrap()[0], &mut out);
        // Its keyframe: layer 2 plays from then on, and layer 1's frames still on their way are ignored.
        rx.push(7, 2, &tx.send(7, 0, 2, true, 2, &frame(10, 0)).unwrap()[0], &mut out);
        rx.push(8, 1, &tx.send(8, 0, 1, false, 4, &frame(10, 0)).unwrap()[0], &mut out);
        rx.push(9, 2, &tx.send(9, 0, 2, false, 3, &frame(10, 0)).unwrap()[0], &mut out);
        assert_eq!(layers(&out), [(1, true), (1, false), (1, false), (1, false), (2, true), (2, false)]);
        assert!(!out.contains(&Action::RequestKeyframe { track: 0, layer: 1 }), "the old layer is not asked for");
        // Back to layer 1, later: from its keyframe.
        out.clear();
        rx.push(2000, 1, &tx.send(2000, 0, 1, false, 9, &frame(10, 0)).unwrap()[0], &mut out);
        assert!(layers(&out).is_empty());
        rx.push(2001, 1, &tx.send(2001, 0, 1, true, 10, &frame(10, 0)).unwrap()[0], &mut out);
        assert_eq!(layers(&out), [(1, true)]);
    }
}
