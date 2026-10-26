//! Deterministic simulation of groups: several members and a relay that
//! follows GROUP.md (classes, purging, voice dropping, replacement), with
//! simulated time and injected faults.
//!
//! With `PEERCONNECT_SIM_QUIC=<seed>`, every test runs as over QUIC (GROUP.md
//! §3.6), where each class has its own stream: classes overtake each other
//! at random, voice arrives out of order, and frames already sent survive
//! the purge of a slot.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use peerconnect::RoomId;
use peerconnect::group::{Channel, Class, Config, Event, Group, Identity, Member, MemberId, Output, State, wire};
use peerconnect::relay::{GroupAction, GroupConn, Incoming, VideoRouter};
use peerconnect::wire::close;

struct Conn {
    node: usize,
    state: GroupConn,
    token: Option<[u8; 16]>,
    queues: [VecDeque<Vec<u8>>; Class::COUNT],
    pongs: usize,
    /// Closed by the relay with this code, to be reported to the node.
    closing: Option<u16>,
}

/// Decides what a node gets instead of a frame forwarded to it.
type Tamper = Box<dyn FnMut(usize, &[u8]) -> Vec<Vec<u8>>>;

struct Relay {
    group: RoomId,
    limit: usize,
    conns: HashMap<usize, Conn>,
    slots: [Option<usize>; wire::MAX_SLOTS],
    next_id: usize,
    /// Every frame forwarded, for replay attacks.
    log: Vec<Vec<u8>>,
    /// Queued command and voice bytes beyond which voice is dropped.
    voice_limit: usize,
    /// Misbehaviour: drop this many COMMAND frames to this node.
    lose: HashMap<usize, usize>,
    /// Misbehaviour: decides, for each peer frame forwarded to a node, what
    /// that node gets instead (nothing, the frame, the frame twice...).
    tamper: Option<Tamper>,
    /// Misbehaviour: seat every connection in a new slot, never replacing.
    no_replacement: bool,
    /// Deliver as QUIC would: a random class each time, voice in any order,
    /// and only commands purged (the rest is already on its way).
    quic: Option<u64>,
    /// Which layer of whose video goes to whom.
    router: VideoRouter,
    /// The simulation's clock, for the router.
    now: u64,
}

impl Relay {
    fn random(&mut self) -> u64 {
        let x = self.quic.as_mut().expect("QUIC mode");
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        *x
    }

    /// The next frame for a connection, most urgent first, or as QUIC mode says.
    fn next_frame(&mut self, id: usize) -> Option<Vec<u8>> {
        if self.quic.is_none() {
            let queues = &mut self.conns.get_mut(&id).unwrap().queues;
            return Class::PRIORITY.iter().find_map(|c| queues[c.index()].pop_front());
        }
        let ready: Vec<usize> = (0..Class::COUNT).filter(|&c| !self.conns[&id].queues[c].is_empty()).collect();
        if ready.is_empty() {
            return None;
        }
        let class = ready[(self.random() % ready.len() as u64) as usize];
        let at = if class == Class::Voice.index() || class == Class::Video.index() {
            (self.random() % self.conns[&id].queues[class].len() as u64) as usize
        } else {
            0
        };
        self.conns.get_mut(&id).unwrap().queues[class].remove(at)
    }
}

impl Relay {
    fn open(&mut self, node: usize) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let (state, hello) = GroupConn::new();
        let mut conn = Conn { node, state, token: None, queues: Default::default(), pongs: 0, closing: None };
        conn.queues[0].push_back(hello.to_vec());
        self.conns.insert(id, conn);
        id
    }

    fn slot_of(&self, id: usize) -> Option<u8> {
        self.slots.iter().position(|s| *s == Some(id)).map(|s| s as u8)
    }

    fn receive(&mut self, id: usize, message: Incoming<'_>) {
        let Some(conn) = self.conns.get(&id) else { return };
        if conn.closing.is_some() {
            return;
        }
        match conn.state.receive(&self.group, message) {
            GroupAction::Pong => self.conns.get_mut(&id).unwrap().pongs += 1,
            GroupAction::Join(token) => self.join(id, token),
            GroupAction::Subscribe { slot, layer } => {
                let from = self.slot_of(id).unwrap();
                self.router.subscribe(from, slot, layer);
                self.tell_demands();
            }
            GroupAction::Forward { to, class } => {
                let Incoming::Binary(frame) = message else { unreachable!() };
                self.log.push(frame.to_vec());
                let from = self.slot_of(id).unwrap();
                let mut targets: Vec<usize> = match to {
                    Some(to) => self.slots[to as usize].into_iter().collect(),
                    None => self.slots.iter().flatten().copied().filter(|&c| c != id).collect(),
                };
                if class == Class::Video && to.is_none() {
                    let (now, kind) = (self.now, frame[0]);
                    targets.retain(|&t| {
                        let to = self.slots.iter().position(|s| *s == Some(t)).unwrap() as u8;
                        // As the Linux relay: video queued past 32 KiB is a link not keeping up.
                        let queued: usize = self.conns[&t].queues[Class::Video.index()].iter().map(Vec::len).sum();
                        self.router.link(now, to, queued > 32 << 10, false);
                        self.router.forward(now, from, to, kind)
                    });
                    self.tell_demands();
                }
                for target in targets {
                    let conn = self.conns.get_mut(&target).unwrap();
                    if frame[0] == wire::COMMAND
                        && let Some(n) = self.lose.get_mut(&conn.node).filter(|n| **n > 0)
                    {
                        *n -= 1;
                        continue;
                    }
                    if class == Class::Voice {
                        let urgent: usize = conn.queues[..2].iter().flatten().map(Vec::len).sum();
                        if urgent + frame.len() > self.voice_limit {
                            continue;
                        }
                    }
                    let node = conn.node;
                    let frames = match &mut self.tamper {
                        Some(tamper) => tamper(node, frame),
                        None => vec![frame.to_vec()],
                    };
                    let conn = self.conns.get_mut(&target).unwrap();
                    for frame in frames {
                        conn.queues[Class::of(frame[0]).index()].push_back(frame);
                    }
                }
            }
            GroupAction::Close(code) => self.kill(id, Some(code)),
        }
    }

    fn join(&mut self, id: usize, token: [u8; 16]) {
        let existing = self.slots.iter().position(|s| s.is_some_and(|c| self.conns[&c].token == Some(token)));
        let existing = existing.filter(|_| !self.no_replacement);
        let slot = match existing {
            Some(slot) => {
                let old = self.slots[slot].unwrap();
                self.slots[slot] = None;
                self.conns.get_mut(&old).unwrap().closing = Some(close::REPLACED);
                self.purge(slot as u8);
                slot
            }
            None => {
                let occupied = self.slots.iter().flatten().count();
                match self.slots.iter().position(Option::is_none) {
                    Some(slot) if occupied < self.limit => slot,
                    _ => return self.kill(id, Some(close::FULL)),
                }
            }
        };
        let present = self.slots.iter().enumerate().filter(|(_, s)| s.is_some()).fold(0u32, |m, (i, _)| m | 1 << i);
        self.slots[slot] = Some(id);
        let conn = self.conns.get_mut(&id).unwrap();
        conn.token = Some(token);
        conn.state = GroupConn::Seated(slot as u8);
        conn.queues[0].push_back(wire::welcome(slot as u8, present).to_vec());
        self.announce(slot as u8, true);
        self.router.seat(slot as u8);
        self.tell_demands();
    }

    fn tell_demands(&mut self) {
        for (slot, layers) in self.router.demands() {
            if let Some(c) = self.slots[slot as usize] {
                self.conns.get_mut(&c).unwrap().queues[0].push_back(wire::demand(layers).to_vec());
            }
        }
        for (slot, layer) in self.router.limits() {
            if let Some(c) = self.slots[slot as usize] {
                self.conns.get_mut(&c).unwrap().queues[0].push_back(wire::limit(layer).to_vec());
            }
        }
    }

    fn announce(&mut self, slot: u8, present: bool) {
        for other in self.slots.iter().flatten() {
            if *other != self.slots[slot as usize].unwrap_or(usize::MAX) {
                self.conns.get_mut(other).unwrap().queues[0].push_back(wire::member(slot, present).to_vec());
            }
        }
    }

    /// Discards queued frames from `slot` (GROUP.md §3.2). Over QUIC, what
    /// was queued here may already be on its way: none is discarded.
    fn purge(&mut self, slot: u8) {
        if self.quic.is_some() {
            return;
        }
        for conn in self.conns.values_mut() {
            for q in &mut conn.queues {
                q.retain(|f| !(f[0] < 0x80 && f[1] == slot));
            }
        }
    }

    /// Ends a connection: the relay closes it (`code`) or it went away.
    fn kill(&mut self, id: usize, code: Option<u16>) {
        if let Some(slot) = self.slot_of(id) {
            self.slots[slot as usize] = None;
            self.purge(slot);
            self.announce(slot, false);
            self.router.leave(slot);
            self.tell_demands();
        }
        if let Some(conn) = self.conns.get_mut(&id) {
            conn.closing = Some(code.unwrap_or(1006));
            conn.queues = Default::default();
        }
    }
}

struct Node {
    member: Member,
    conn: Option<usize>,
    events: Vec<Event>,
    /// The network is up: connections succeed.
    up: bool,
    /// Half-open: frames for the node are not delivered, nor are its own
    /// sends delivered, and nothing tells it.
    frozen: bool,
    /// Connection attempts hang: never opened, never refused.
    hang: bool,
}

struct Sim {
    now: u64,
    relay: Relay,
    nodes: Vec<Node>,
    group: Group,
}

impl Sim {
    fn new() -> Sim {
        let group = Group::create("https://relay.test").unwrap();
        let relay = Relay {
            group: group.id(),
            limit: wire::MAX_SLOTS,
            conns: HashMap::new(),
            slots: Default::default(),
            next_id: 0,
            log: Vec::new(),
            voice_limit: 8 << 10,
            lose: HashMap::new(),
            tamper: None,
            no_replacement: false,
            router: VideoRouter::default(),
            now: 0,
            // The value seeds the order: `PEERCONNECT_SIM_QUIC=7`, say.
            quic: std::env::var("PEERCONNECT_SIM_QUIC")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|seed| (seed ^ 0x9E37_79B9_7F4A_7C15).max(1)),
        };
        Sim { now: 0, relay, nodes: Vec::new(), group }
    }

    fn add(&mut self, identity: Identity, name: &str) -> usize {
        let config = Config { profile: name.as_bytes().to_vec(), ..Config::default() };
        let member = Member::new(self.group.clone(), identity, config, self.now).unwrap();
        self.nodes.push(Node { member, conn: None, events: Vec::new(), up: true, frozen: false, hang: false });
        self.nodes.len() - 1
    }

    fn id(&self, n: usize) -> MemberId {
        self.nodes[n].member.id()
    }

    /// Runs until nothing more happens at this instant.
    fn settle(&mut self) {
        for _ in 0..100_000 {
            if !self.step() {
                return;
            }
        }
        panic!("no quiescence");
    }

    fn step(&mut self) -> bool {
        let mut progress = false;
        for n in 0..self.nodes.len() {
            while let Some(out) = self.nodes[n].member.poll_output() {
                progress = true;
                match out {
                    Output::Connect(url) => {
                        assert!(url.ends_with(&format!("/g1/{}", self.group.id())));
                        if self.nodes[n].hang {
                            // Nothing happens.
                        } else if self.nodes[n].up {
                            let id = self.relay.open(n);
                            self.nodes[n].conn = Some(id);
                            self.nodes[n].member.handle_open(self.now);
                        } else {
                            self.nodes[n].member.handle_close(self.now, None);
                        }
                    }
                    Output::Send(frame, class) => {
                        if frame[0] < 0x80 {
                            assert_eq!(wire::Class::of(frame[0]), class);
                        }
                        if self.nodes[n].frozen {
                            continue;
                        }
                        if let Some(id) = self.nodes[n].conn {
                            self.relay.now = self.now;
                            self.relay.receive(id, Incoming::Binary(&frame));
                        }
                    }
                    Output::Ping => {
                        if self.nodes[n].frozen {
                            continue;
                        }
                        if let Some(id) = self.nodes[n].conn {
                            self.relay.receive(id, Incoming::Text("ping"));
                        }
                    }
                    Output::Disconnect => {
                        if let Some(id) = self.nodes[n].conn.take() {
                            self.relay.kill(id, None);
                            self.relay.conns.remove(&id);
                        }
                    }
                    Output::Event(e) => self.nodes[n].events.push(e),
                }
            }
        }
        // Deliver one frame per connection per step, most urgent first, so
        // that classes interleave as they would on a real relay.
        let ids: Vec<usize> = self.relay.conns.keys().copied().collect();
        for id in ids {
            let conn = self.relay.conns.get_mut(&id).unwrap();
            let node = conn.node;
            if self.nodes[node].frozen && self.nodes[node].conn == Some(id) {
                continue;
            }
            if conn.pongs > 0 {
                conn.pongs -= 1;
                progress = true;
                self.nodes[node].member.handle_text(self.now, "pong");
                continue;
            }
            if let Some(frame) = self.relay.next_frame(id) {
                progress = true;
                self.nodes[node].member.handle_binary(self.now, &frame);
                continue;
            }
            if let Some(code) = self.relay.conns[&id].closing {
                progress = true;
                self.relay.conns.remove(&id);
                if self.nodes[node].conn == Some(id) {
                    self.nodes[node].conn = None;
                    self.nodes[node].member.handle_close(self.now, Some(code));
                }
            }
        }
        progress
    }

    /// Advances simulated time by `ms`, firing timers on the way.
    fn run(&mut self, ms: u64) {
        let end = self.now + ms;
        self.settle();
        loop {
            let next = self.nodes.iter().filter_map(|n| n.member.poll_timeout()).min().unwrap_or(end);
            if next > end {
                break;
            }
            self.now = next.max(self.now);
            for node in &mut self.nodes {
                if node.member.poll_timeout().is_some_and(|t| t <= self.now) {
                    node.member.handle_timeout(self.now);
                }
            }
            self.settle();
            if self.now == end {
                break;
            }
        }
        self.now = end;
    }

    /// Cuts a node's network: its connection dies, and reconnecting fails
    /// until `restore`.
    fn cut(&mut self, n: usize) {
        self.nodes[n].up = false;
        if let Some(id) = self.nodes[n].conn.take() {
            self.relay.kill(id, None);
            self.relay.conns.remove(&id);
            self.nodes[n].member.handle_close(self.now, None);
        }
    }

    fn restore(&mut self, n: usize) {
        self.nodes[n].up = true;
    }

    fn take(&mut self, n: usize) -> Vec<Event> {
        std::mem::take(&mut self.nodes[n].events)
    }

    /// Stream messages node `n` received on `channel`, as text.
    fn messages(&mut self, n: usize, channel: Channel) -> Vec<(MemberId, String)> {
        self.nodes[n]
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Message { from, channel: c, data, direct: false } if *c == channel => {
                    Some((*from, String::from_utf8(data.clone()).unwrap()))
                }
                _ => None,
            })
            .collect()
    }

    fn joined(&self, n: usize) -> HashSet<MemberId> {
        self.nodes[n].member.members().into_iter().map(|(m, _)| m).collect()
    }

    fn send(&mut self, n: usize, channel: Channel, text: &str) {
        self.nodes[n].member.send(channel, text.as_bytes()).unwrap();
    }
}

fn texts(list: &[(MemberId, String)], from: MemberId) -> Vec<String> {
    list.iter().filter(|(f, _)| *f == from).map(|(_, t)| t.clone()).collect()
}

fn numbered(prefix: &str, range: std::ops::Range<usize>) -> Vec<String> {
    range.map(|i| format!("{prefix}{i}")).collect()
}

#[test]
fn members_admit_each_other_and_streams_arrive_in_order() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "alice");
    let b = sim.add(Identity::generate(), "bob");
    let c = sim.add(Identity::generate(), "carol");
    sim.run(100);
    for n in [a, b, c] {
        assert_eq!(sim.nodes[n].member.state(), State::Online);
        assert_eq!(sim.joined(n).len(), 2, "node {n}");
    }
    let profiles: BTreeMap<MemberId, Vec<u8>> = sim.nodes[a].member.members().into_iter().collect();
    assert_eq!(profiles[&sim.id(b)], b"bob");

    for i in 0..50 {
        sim.send(a, Channel::Command, &format!("a{i}"));
        sim.send(b, Channel::Command, &format!("b{i}"));
        sim.send(c, Channel::Chat, &format!("c{i}"));
    }
    sim.run(100);
    for n in [a, b, c] {
        let commands = sim.messages(n, Channel::Command);
        assert_eq!(texts(&commands, sim.id(a)), numbered("a", 0..50), "node {n}");
        assert_eq!(texts(&commands, sim.id(b)), numbered("b", 0..50), "node {n}");
        let chat = sim.messages(n, Channel::Chat);
        assert_eq!(texts(&chat, sim.id(c)), numbered("c", 0..50), "node {n}");
        assert!(!sim.nodes[n].events.iter().any(|e| matches!(e, Event::Missed { .. })));
    }
}

#[test]
fn direct_voice_and_datagrams() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    let c = sim.add(Identity::generate(), "c");
    sim.run(100);
    let (ib, ic) = (sim.id(b), sim.id(c));
    sim.nodes[a].member.send_to(ib, Channel::Command, b"psst").unwrap();
    assert!(sim.nodes[a].member.send_voice(7, b"opus"));
    assert!(sim.nodes[a].member.send_datagram(Some(ic), b"pose"));
    sim.run(10);
    let eb = sim.take(b);
    let ec = sim.take(c);
    let ia = sim.id(a);
    assert!(eb.contains(&Event::Message { from: ia, channel: Channel::Command, data: b"psst".to_vec(), direct: true }));
    assert!(!ec.iter().any(|e| matches!(e, Event::Message { direct: true, .. })));
    for events in [&eb, &ec] {
        assert!(events.contains(&Event::Voice { from: ia, seq: 7, data: b"opus".to_vec() }));
    }
    assert!(ec.contains(&Event::Datagram { from: ia, data: b"pose".to_vec() }));
    assert!(!eb.iter().any(|e| matches!(e, Event::Datagram { .. })));
}

#[test]
fn a_member_that_drops_out_gets_what_it_missed_and_sends_what_it_wrote() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    for i in 0..10 {
        sim.send(a, Channel::Command, &format!("a{i}"));
    }
    sim.run(10);
    sim.cut(b);
    for i in 10..30 {
        sim.send(a, Channel::Command, &format!("a{i}"));
        sim.send(a, Channel::Chat, &format!("chat{i}"));
        sim.send(b, Channel::Command, &format!("b{i}")); // while offline
    }
    sim.run(3_000);
    assert_eq!(sim.nodes[b].member.state(), State::Offline);
    sim.restore(b);
    sim.run(30_000);
    assert_eq!(sim.joined(b).len(), 1);
    let (ia, ib) = (sim.id(a), sim.id(b));
    let at_b = sim.messages(b, Channel::Command);
    assert_eq!(texts(&at_b, ia), numbered("a", 0..30));
    assert_eq!(texts(&sim.messages(b, Channel::Chat), ia), numbered("chat", 10..30));
    let at_a = sim.messages(a, Channel::Command);
    assert_eq!(texts(&at_a, ib), numbered("b", 10..30));
    // Left and joined again, from both sides.
    let ea = sim.take(a);
    assert!(ea.contains(&Event::Left { member: ib }));
    assert_eq!(ea.iter().filter(|e| matches!(e, Event::Joined { .. })).count(), 2);
}

#[test]
fn a_large_backlog_is_resent_in_bursts() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    sim.cut(b);
    let big = "x".repeat(10_000);
    for i in 0..200 {
        sim.send(a, Channel::Command, &format!("{i}{big}")); // 2 MB in all
    }
    sim.restore(b);
    sim.run(30_000);
    let ia = sim.id(a);
    let got = texts(&sim.messages(b, Channel::Command), ia);
    assert_eq!(got.len(), 200);
    for (i, text) in got.iter().enumerate() {
        assert!(text.starts_with(&i.to_string()));
    }
}

#[test]
fn messages_beyond_what_the_sender_keeps_are_reported_missing() {
    let mut sim = Sim::new();
    let config = Config { keep_messages: 5, ..Config::default() };
    let identity = Identity::generate();
    sim.nodes.push(Node {
        member: Member::new(sim.group.clone(), identity, config, 0).unwrap(),
        conn: None,
        events: Vec::new(),
        up: true,
        frozen: false,
        hang: false,
    });
    let a = 0;
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    sim.cut(b);
    for i in 0..12 {
        sim.send(a, Channel::Command, &format!("a{i}"));
    }
    sim.restore(b);
    sim.run(30_000);
    let ia = sim.id(a);
    assert_eq!(texts(&sim.messages(b, Channel::Command), ia), numbered("a", 7..12));
    assert!(sim.take(b).contains(&Event::Missed { from: ia, channel: Channel::Command, count: 7 }));
}

#[test]
fn the_newest_connection_of_an_identity_replaces_the_old_one() {
    let mut sim = Sim::new();
    let identity = Identity::generate();
    let a = sim.add(identity.clone(), "phone");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    let a2 = sim.add(identity, "laptop");
    sim.run(100);
    assert!(sim.take(a).contains(&Event::Replaced));
    assert_eq!(sim.nodes[a].member.state(), State::Offline);
    let members = sim.nodes[b].member.members();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].1, b"laptop");
    let _ = a2;
}

#[test]
fn a_full_group_refuses_more_members() {
    // Four at most (GROUP.md §3.2): the fifth is refused.
    let mut sim = Sim::new();
    for i in 0..5 {
        sim.add(Identity::generate(), &i.to_string());
    }
    sim.run(100);
    let full = (0..5).filter(|&n| sim.nodes[n].events.contains(&Event::Full)).count();
    assert_eq!(full, 1);
    for n in 0..5 {
        let online = sim.nodes[n].member.state() == State::Online;
        assert_eq!(sim.joined(n).len(), if online { 3 } else { 0 });
    }
}

#[test]
fn many_members_and_a_voice_flood_leave_commands_intact() {
    let mut sim = Sim::new();
    let nodes: Vec<usize> = (0..wire::MAX_SLOTS).map(|i| sim.add(Identity::generate(), &i.to_string())).collect();
    sim.run(200);
    for &n in &nodes {
        assert_eq!(sim.joined(n).len(), wire::MAX_SLOTS - 1);
    }
    for round in 0..50u16 {
        for &n in &nodes {
            sim.nodes[n].member.send_voice(round, &[0u8; 1000]);
            if round % 5 == 0 {
                sim.send(n, Channel::Command, &format!("{n}:{round}"));
            }
        }
        sim.run(20);
    }
    sim.run(1000);
    for &n in &nodes {
        let commands = sim.messages(n, Channel::Command);
        for &m in &nodes {
            let expected: Vec<String> = (0..50).step_by(5).map(|r| format!("{m}:{r}")).collect();
            assert_eq!(texts(&commands, sim.id(m)), expected);
        }
    }
}

/// A toy application: a set of commands applied, and a snapshot of it.
fn state(events: &[Event]) -> Vec<String> {
    let mut applied: Vec<String> = Vec::new();
    for e in events {
        match e {
            Event::Snapshot { data, .. } => {
                applied = String::from_utf8(data.clone())
                    .unwrap()
                    .split('\n')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
            }
            Event::Message { channel: Channel::Command, data, direct: false, .. } => {
                applied.push(String::from_utf8(data.clone()).unwrap());
            }
            _ => {}
        }
    }
    applied
}

/// Answers sync requests with the application state at the time of the request.
fn serve_sync(sim: &mut Sim, n: usize) {
    let events = sim.nodes[n].events.clone();
    for (i, e) in events.iter().enumerate() {
        if let Event::SyncRequest { from, transfer } = e {
            let already = events[i + 1..].iter().any(|later| matches!(later, Event::Snapshot { .. }));
            let _ = already;
            let snapshot = state(&events[..i]).join("\n");
            // A transfer is answered at most once; later calls fail harmlessly.
            let _ = sim.nodes[n].member.provide_sync(*from, *transfer, snapshot.into_bytes());
        }
    }
}

#[test]
fn state_transfer_gives_a_newcomer_exactly_the_state_of_the_others() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "host");
    let b = sim.add(Identity::generate(), "builder");
    sim.run(100);
    for i in 0..300 {
        sim.send(a, Channel::Command, &format!("a{i}"));
        sim.send(b, Channel::Command, &format!("b{i}"));
    }
    sim.run(100);
    let c = sim.add(Identity::generate(), "visitor");
    sim.run(100);
    let host = sim.id(a);
    sim.nodes[c].member.request_sync(host).unwrap();
    // Commands keep coming while the snapshot is under way.
    for i in 300..320 {
        sim.send(a, Channel::Command, &format!("a{i}"));
        sim.send(b, Channel::Command, &format!("b{i}"));
        sim.send(c, Channel::Command, &format!("c{i}"));
        sim.settle();
        serve_sync(&mut sim, a);
    }
    sim.run(1000);
    serve_sync(&mut sim, a);
    for i in 320..330 {
        sim.send(b, Channel::Command, &format!("b{i}"));
    }
    sim.run(1000);

    let events_c = sim.nodes[c].events.clone();
    assert!(events_c.iter().any(|e| matches!(e, Event::Snapshot { .. })), "{events_c:?}");
    let mut at_c = state(&events_c);
    let mut at_a = state(&sim.nodes[a].events);
    let mut at_b = state(&sim.nodes[b].events);
    // Every command exactly once, at every member.
    for list in [&mut at_a, &mut at_b, &mut at_c] {
        let unique: HashSet<&String> = list.iter().collect();
        assert_eq!(unique.len(), list.len(), "duplicates");
        list.sort();
    }
    assert_eq!(at_a.len(), 300 * 2 + 20 * 3 + 10);
    assert_eq!(at_c, at_a);
    assert_eq!(at_b, at_a);
}

#[test]
fn state_transfer_from_a_member_that_is_behind() {
    // The provider lost some of b's commands (it will get them again later)
    // when it takes its snapshot, while the newcomer already has them: the
    // newcomer must get them again after the snapshot, not lose them.
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    let c = sim.add(Identity::generate(), "c");
    sim.run(100);
    for i in 0..5 {
        sim.send(b, Channel::Command, &format!("b{i}"));
    }
    sim.run(100);
    sim.relay.lose.insert(a, 3);
    for i in 5..8 {
        sim.send(b, Channel::Command, &format!("b{i}"));
    }
    sim.settle();
    assert_eq!(texts(&sim.messages(c, Channel::Command), sim.id(b)), numbered("b", 0..8));
    assert_eq!(texts(&sim.messages(a, Channel::Command), sim.id(b)), numbered("b", 0..5));
    let ia = sim.id(a);
    sim.nodes[c].member.request_sync(ia).unwrap();
    sim.settle();
    serve_sync(&mut sim, a);
    sim.run(20_000);
    let events_c = sim.nodes[c].events.clone();
    let snapshot = events_c.iter().position(|e| matches!(e, Event::Snapshot { .. })).expect("a snapshot");
    // After the snapshot (b0..b4), b5..b7 again.
    let after: Vec<String> = state(&events_c[snapshot..]);
    assert_eq!(after, numbered("b", 0..8));
    // And a recovered its own gap in the meantime.
    assert_eq!(texts(&sim.messages(a, Channel::Command), sim.id(b)), numbered("b", 0..8));
}

#[test]
fn a_failed_transfer_releases_held_commands() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    let c = sim.add(Identity::generate(), "c");
    sim.run(100);
    let ia = sim.id(a);
    sim.nodes[c].member.request_sync(ia).unwrap();
    sim.send(b, Channel::Command, "held");
    sim.run(10);
    assert!(sim.messages(c, Channel::Command).is_empty());
    sim.cut(a);
    sim.run(100);
    let ec = sim.take(c);
    assert!(ec.contains(&Event::SyncFailed { from: ia }));
    assert!(ec.iter().any(|e| matches!(e, Event::Message { data, .. } if data == b"held")));
}

#[test]
fn a_relay_cannot_admit_a_member_by_replaying_an_earlier_session() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    sim.send(a, Channel::Command, "secret plan");
    sim.run(100);
    let log = sim.relay.log.clone();
    let ia = sim.id(a);

    // A fresh member, and a relay that pretends a's old connection is in
    // slot 0 and plays back everything it recorded.
    let mut victim = Member::new(sim.group.clone(), Identity::generate(), Config::default(), 0).unwrap();
    while victim.poll_output().is_some() {}
    victim.handle_open(0);
    victim.handle_binary(0, &[&[wire::HELLO][..], &[0; 32]].concat());
    let a_slot = log.iter().find(|f| f[0] == wire::KEY).unwrap()[1];
    let my_slot = (0..32).find(|&s| s != a_slot).unwrap();
    victim.handle_binary(0, &wire::welcome(my_slot, 1 << a_slot));
    for frame in &log {
        let mut frame = frame.clone();
        if frame[2] != wire::BROADCAST {
            frame[2] = my_slot; // as if addressed to the victim (breaks the MAC, too)
        }
        victim.handle_binary(1, &frame);
    }
    while let Some(out) = victim.poll_output() {
        if let Output::Event(e) = out {
            assert!(!matches!(e, Event::Joined { .. } | Event::Message { .. }), "replay produced {e:?} (a is {ia:?})");
        }
    }
    let _ = b;
}

#[test]
fn voice_is_dropped_for_a_slow_receiver_but_reliable_traffic_is_not() {
    let mut sim = Sim::new();
    sim.relay.voice_limit = 0; // every voice frame dropped
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    for i in 0..20 {
        sim.nodes[a].member.send_voice(i, b"v");
        sim.send(a, Channel::Chat, &format!("t{i}"));
    }
    sim.run(100);
    let ia = sim.id(a);
    assert_eq!(texts(&sim.messages(b, Channel::Chat), ia), numbered("t", 0..20));
    assert!(!sim.nodes[b].events.iter().any(|e| matches!(e, Event::Voice { .. })));
}

#[test]
fn leaving_stops_everything() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    sim.nodes[a].member.leave();
    sim.run(100);
    let ia = sim.id(a);
    assert!(sim.take(b).contains(&Event::Left { member: ia }));
    assert_eq!(sim.nodes[a].member.state(), State::Closed);
    assert_eq!(sim.nodes[a].member.poll_timeout(), None);
    assert!(sim.nodes[a].member.send(Channel::Command, b"x").is_err());
}

#[test]
fn quiet_members_stay_connected_and_measure_rtt() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(5 * 60_000);
    for n in [a, b] {
        assert_eq!(sim.nodes[n].member.state(), State::Online);
        assert_eq!(sim.joined(n).len(), 1);
        assert!(sim.nodes[n].member.rtt().is_some());
        let lefts = sim.nodes[n].events.iter().filter(|e| matches!(e, Event::Left { .. })).count();
        assert_eq!(lefts, 0);
    }
}

// Faults, one path at a time.

use std::cell::Cell;
use std::rc::Rc;

/// A tamper that drops the `nth` (from 0) frame matching `what` to `node`.
fn drop_nth(node: usize, nth: usize, what: impl Fn(&[u8]) -> bool + 'static) -> Tamper {
    let seen = Rc::new(Cell::new(0));
    Box::new(move |to, frame| {
        if to == node && what(frame) {
            let n = seen.get();
            seen.set(n + 1);
            if n == nth {
                return Vec::new();
            }
        }
        vec![frame.to_vec()]
    })
}

fn missed(events: &[Event]) -> Vec<u64> {
    events.iter().filter_map(|e| if let Event::Missed { count, .. } = e { Some(*count) } else { None }).collect()
}

#[test]
fn a_frame_lost_mid_stream_is_held_around_and_recovered() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    sim.relay.tamper = Some(drop_nth(b, 2, |f| f[0] == wire::COMMAND && f[2] == wire::BROADCAST));
    for i in 0..10 {
        sim.send(a, Channel::Command, &format!("a{i}"));
    }
    sim.run(1000);
    let ia = sim.id(a);
    assert_eq!(texts(&sim.messages(b, Channel::Command), ia), numbered("a", 0..10));
    assert!(missed(&sim.nodes[b].events).is_empty());
}

#[test]
fn frames_replayed_within_a_session_are_ignored() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    // Every frame to b twice, and now and then one from earlier again.
    let mut log: Vec<Vec<u8>> = Vec::new();
    sim.relay.tamper = Some(Box::new(move |to, frame| {
        if to != b || frame[0] == wire::KEY {
            return vec![frame.to_vec()];
        }
        log.push(frame.to_vec());
        let mut out = vec![frame.to_vec(), frame.to_vec()];
        if log.len().is_multiple_of(3) {
            out.push(log[log.len() / 2].clone());
        }
        out
    }));
    for i in 0..20u16 {
        sim.send(a, Channel::Command, &format!("a{i}"));
        sim.send(a, Channel::Chat, &format!("c{i}"));
        sim.nodes[a].member.send_voice(i, b"v");
        sim.nodes[a].member.send_datagram(None, b"d");
    }
    let ib = sim.id(b);
    sim.nodes[a].member.send_to(ib, Channel::Command, b"direct").unwrap();
    sim.run(1000);
    let ia = sim.id(a);
    assert_eq!(texts(&sim.messages(b, Channel::Command), ia), numbered("a", 0..20));
    assert_eq!(texts(&sim.messages(b, Channel::Chat), ia), numbered("c", 0..20));
    let events = &sim.nodes[b].events;
    let mut voices: Vec<u16> =
        events.iter().filter_map(|e| if let Event::Voice { seq, .. } = e { Some(*seq) } else { None }).collect();
    voices.sort(); // in any order over QUIC, but each once
    assert_eq!(voices, (0..20).collect::<Vec<_>>());
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Datagram { .. })).count(), 20);
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Message { direct: true, .. })).count(), 1);
}

#[test]
fn a_lost_confirm_is_recovered() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    sim.run(100);
    // b's CONFIRM to a is the first CONTROL frame a gets.
    sim.relay.tamper = Some(drop_nth(a, 0, |f| f[0] == wire::CONTROL));
    let b = sim.add(Identity::generate(), "b");
    sim.run(1000);
    assert_eq!(sim.joined(b).len(), 1, "b admitted a");
    assert_eq!(sim.joined(a).len(), 0, "a still waits for the lost CONFIRM");
    sim.run(6000);
    assert_eq!(sim.joined(a).len(), 1, "a's KEY retry got the CONFIRM again");
    sim.send(b, Channel::Command, "hello");
    sim.run(100);
    assert_eq!(texts(&sim.messages(a, Channel::Command), sim.id(b)), ["hello"]);
}

#[test]
fn a_lost_key_reply_is_recovered() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    sim.run(100);
    let b = sim.add(Identity::generate(), "b");
    // a's answer to b's broadcast KEY is the first KEY addressed to b alone.
    sim.relay.tamper = Some(drop_nth(b, 0, |f| f[0] == wire::KEY && f[2] != wire::BROADCAST));
    sim.run(1000);
    assert_eq!(sim.joined(a).len() + sim.joined(b).len(), 0);
    sim.run(6000);
    assert_eq!((sim.joined(a).len(), sim.joined(b).len()), (1, 1));
}

#[test]
fn a_half_open_connection_is_detected_and_replaced() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    let start = sim.now;
    sim.nodes[b].frozen = true; // nothing in, nothing out, and nobody told
    for i in 0..5 {
        sim.send(a, Channel::Command, &format!("a{i}"));
    }
    sim.run(24_000);
    assert_eq!(sim.nodes[b].member.state(), State::Online, "not yet: 25 s without a frame");
    sim.run(1_100);
    assert!(sim.take(b).contains(&Event::State(State::Offline)), "dead after 25 s");
    sim.nodes[b].frozen = false;
    sim.run(5_000);
    let ia = sim.id(a);
    assert_eq!(texts(&sim.messages(b, Channel::Command), ia), numbered("a", 0..5));
    assert_eq!(sim.joined(a).len(), 1);
    assert!(sim.now - start < 31_000);
}

#[test]
fn hanging_connection_attempts_time_out_and_are_retried() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    sim.nodes[a].hang = true;
    sim.run(14_000);
    assert_eq!(sim.nodes[a].member.state(), State::Offline);
    sim.nodes[a].hang = false;
    sim.run(3_000); // the attempt times out at 15 s, and the next one works
    assert_eq!(sim.nodes[a].member.state(), State::Online);
}

#[test]
fn a_relay_that_never_says_hello_or_welcome_is_left() {
    let group = Group::create("https://relay.test").unwrap();
    let mut m = Member::new(group, Identity::generate(), Config::default(), 0).unwrap();
    let drain = |m: &mut Member| std::iter::from_fn(|| m.poll_output()).collect::<Vec<_>>();
    assert!(matches!(drain(&mut m)[..], [Output::Connect(_)]));
    m.handle_open(0);
    m.handle_timeout(10_000);
    assert!(drain(&mut m).contains(&Output::Disconnect), "no HELLO in 10 s");

    // A frame out of place while joining also ends the connection.
    let next = m.poll_timeout().unwrap();
    m.handle_timeout(next);
    assert!(matches!(drain(&mut m)[..], [Output::Connect(_)]));
    m.handle_open(next);
    m.handle_binary(next, &wire::welcome(0, 0));
    assert!(drain(&mut m).contains(&Output::Disconnect), "WELCOME before HELLO");
}

#[test]
fn one_identity_in_two_slots_keeps_only_the_newer() {
    let mut sim = Sim::new();
    sim.relay.no_replacement = true;
    let identity = Identity::generate();
    let x = sim.add(identity.clone(), "first");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    sim.add(identity, "second");
    sim.run(100);
    let members = sim.nodes[b].member.members();
    assert_eq!(members, [(sim.id(x), b"second".to_vec())]);
    assert!(sim.take(b).contains(&Event::Left { member: sim.id(x) }));
}

#[test]
fn a_gap_that_does_not_close_is_given_up_after_15_s() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    // Lose a's second message to b, and every resend of it.
    let seen = Rc::new(Cell::new(0));
    sim.relay.tamper = Some(Box::new(move |to, frame| {
        if to == b && frame[0] == wire::COMMAND {
            if frame[2] != wire::BROADCAST {
                return Vec::new();
            }
            seen.set(seen.get() + 1);
            if seen.get() == 2 {
                return Vec::new();
            }
        }
        vec![frame.to_vec()]
    }));
    for i in 0..5 {
        sim.send(a, Channel::Command, &format!("a{i}"));
    }
    sim.run(14_000);
    let ia = sim.id(a);
    assert_eq!(texts(&sim.messages(b, Channel::Command), ia), ["a0"], "held, waiting");
    sim.run(2_000);
    assert_eq!(texts(&sim.messages(b, Channel::Command), ia), ["a0", "a2", "a3", "a4"]);
    assert_eq!(missed(&sim.nodes[b].events), [1]);
}

#[test]
fn a_hold_that_overflows_is_given_up_at_once() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    let seen = Rc::new(Cell::new(0));
    sim.relay.tamper = Some(Box::new(move |to, frame| {
        if to == b && frame[0] == wire::COMMAND {
            if frame[2] != wire::BROADCAST {
                return Vec::new();
            }
            seen.set(seen.get() + 1);
            if seen.get() == 1 {
                return Vec::new();
            }
        }
        vec![frame.to_vec()]
    }));
    for i in 0..1100 {
        sim.send(a, Channel::Command, &format!("a{i}"));
    }
    sim.run(100); // far less than the 15 s gap timeout
    let ia = sim.id(a);
    assert_eq!(texts(&sim.messages(b, Channel::Command), ia), numbered("a", 1..1100));
    assert_eq!(missed(&sim.nodes[b].events), [1]);
}

#[test]
fn many_short_lived_members_leave_bounded_state() {
    // More streams than a member remembers: the oldest positions go, and
    // delivery to those still present is unaffected.
    let mut sim = Sim::new();
    let host = sim.add(Identity::generate(), "host");
    sim.run(100);
    for i in 0..140 {
        let n = sim.add(Identity::generate(), &format!("guest{i}"));
        sim.run(50);
        sim.send(n, Channel::Command, &format!("hi from {i}"));
        sim.send(n, Channel::Chat, &format!("chat from {i}"));
        sim.run(50);
        sim.nodes[n].member.leave();
        sim.run(10);
    }
    let got: Vec<String> = sim.messages(host, Channel::Command).into_iter().map(|(_, t)| t).collect();
    assert_eq!(got, (0..140).map(|i| format!("hi from {i}")).collect::<Vec<_>>());
    let late = sim.add(Identity::generate(), "late");
    sim.run(100);
    sim.send(late, Channel::Command, "still works");
    sim.run(100);
    assert!(sim.messages(host, Channel::Command).iter().any(|(_, t)| t == "still works"));
}

fn sync_events(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Snapshot { .. } => Some("snapshot"),
            Event::SyncFailed { .. } => Some("failed"),
            Event::SyncRequest { .. } => Some("request"),
            _ => None,
        })
        .collect()
}

fn three() -> (Sim, usize, usize, usize) {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    let c = sim.add(Identity::generate(), "c");
    sim.run(100);
    (sim, a, b, c)
}

#[test]
fn a_declined_transfer_fails_and_releases_held_commands() {
    let (mut sim, a, b, c) = three();
    let ia = sim.id(a);
    sim.nodes[c].member.request_sync(ia).unwrap();
    assert_eq!(sim.nodes[c].member.request_sync(ia), Err(peerconnect::group::SendError::Busy));
    sim.send(b, Channel::Command, "held");
    sim.settle();
    let Some(Event::SyncRequest { from, transfer }) =
        sim.nodes[a].events.iter().rev().find(|e| matches!(e, Event::SyncRequest { .. })).cloned()
    else {
        panic!("no request")
    };
    sim.nodes[a].member.decline_sync(from, transfer);
    assert!(sim.nodes[a].member.provide_sync(from, transfer, vec![1]).is_err(), "gone once declined");
    sim.run(100);
    assert_eq!(sync_events(&sim.nodes[c].events), ["failed"]);
    assert_eq!(texts(&sim.messages(c, Channel::Command), sim.id(b)), ["held"]);
}

#[test]
fn a_member_that_is_syncing_itself_declines_to_provide() {
    let (mut sim, a, b, c) = three();
    let (ia, ib) = (sim.id(a), sim.id(b));
    sim.nodes[b].member.request_sync(ia).unwrap();
    sim.nodes[c].member.request_sync(ib).unwrap();
    sim.run(100);
    assert_eq!(sync_events(&sim.nodes[c].events), ["failed"]);
    assert!(!sync_events(&sim.nodes[b].events).contains(&"request"));
}

#[test]
fn snapshots_too_large_and_empty() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let config = Config { max_snapshot: 10, ..Config::default() };
    sim.nodes.push(Node {
        member: Member::new(sim.group.clone(), Identity::generate(), config, 0).unwrap(),
        conn: None,
        events: Vec::new(),
        up: true,
        frozen: false,
        hang: false,
    });
    let small = 1;
    let c = sim.add(Identity::generate(), "c");
    sim.run(100);
    let ia = sim.id(a);
    sim.nodes[small].member.request_sync(ia).unwrap();
    sim.nodes[c].member.request_sync(ia).unwrap();
    sim.settle();
    let requests: Vec<(MemberId, u32)> = sim.nodes[a]
        .events
        .iter()
        .filter_map(|e| if let Event::SyncRequest { from, transfer } = e { Some((*from, *transfer)) } else { None })
        .collect();
    assert_eq!(requests.len(), 2);
    for (from, transfer) in requests {
        let data = if from == sim.id(small) { vec![0; 11] } else { Vec::new() };
        sim.nodes[a].member.provide_sync(from, transfer, data).unwrap();
    }
    sim.run(100);
    assert_eq!(sync_events(&sim.nodes[small].events), ["failed"], "11 bytes > 10");
    assert!(sim.nodes[c].events.contains(&Event::Snapshot { from: ia, data: Vec::new() }));
}

#[test]
fn a_transfer_nobody_answers_times_out_on_both_sides() {
    let (mut sim, a, _b, c) = three();
    let ia = sim.id(a);
    sim.nodes[c].member.request_sync(ia).unwrap();
    sim.run(14_000);
    assert!(sync_events(&sim.nodes[c].events).is_empty());
    sim.run(2_000);
    assert_eq!(sync_events(&sim.nodes[c].events), ["failed"]);
    let Some(Event::SyncRequest { from, transfer }) =
        sim.nodes[a].events.iter().find(|e| matches!(e, Event::SyncRequest { .. })).cloned()
    else {
        panic!()
    };
    assert!(sim.nodes[a].member.provide_sync(from, transfer, vec![1]).is_err(), "given up");
}

#[test]
fn state_transfer_covers_members_the_newcomer_has_not_met() {
    // d sends commands, then drops out before the newcomer arrives; e
    // arrives after the snapshot was taken. The newcomer must end up with
    // the provider's state, including what d sends once it is back.
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let d = sim.add(Identity::generate(), "d");
    sim.run(100);
    for i in 0..3 {
        sim.send(d, Channel::Command, &format!("d{i}"));
    }
    sim.run(100);
    sim.cut(d);
    sim.send(d, Channel::Command, "d3"); // while away
    sim.run(100);
    let c = sim.add(Identity::generate(), "c");
    sim.run(100);
    let ia = sim.id(a);
    sim.nodes[c].member.request_sync(ia).unwrap();
    sim.settle();
    let e = sim.add(Identity::generate(), "e");
    sim.run(100);
    sim.send(e, Channel::Command, "e0");
    sim.settle();
    serve_sync(&mut sim, a);
    sim.run(100);
    sim.restore(d);
    sim.run(5_000);
    sim.send(d, Channel::Command, "d4");
    sim.run(1_000);
    let mut at_a = state(&sim.nodes[a].events);
    let mut at_c = state(&sim.nodes[c].events);
    at_a.sort();
    at_c.sort();
    assert_eq!(at_a, ["d0", "d1", "d2", "d3", "d4", "e0"]);
    assert_eq!(at_c, at_a);
}

#[test]
fn frames_reordered_within_a_class_are_dropped_not_misdelivered() {
    // A relay that swaps two command frames: the later one is accepted,
    // the earlier one then looks like a replay and is dropped, and the
    // stream asks for it again.
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    sim.run(100);
    let mut held: Option<Vec<u8>> = None;
    sim.relay.tamper = Some(Box::new(move |to, frame| {
        if to != b || frame[0] != wire::COMMAND || frame[2] != wire::BROADCAST {
            return vec![frame.to_vec()];
        }
        match held.take() {
            None if frame[3..7] == [0, 0, 0, 1] => {
                held = Some(frame.to_vec()); // hold the second frame back
                Vec::new()
            }
            Some(earlier) => vec![frame.to_vec(), earlier],
            None => vec![frame.to_vec()],
        }
    }));
    for i in 0..5 {
        sim.send(a, Channel::Command, &format!("a{i}"));
    }
    sim.run(1000);
    assert_eq!(texts(&sim.messages(b, Channel::Command), sim.id(a)), numbered("a", 0..5));
    assert!(missed(&sim.nodes[b].events).is_empty());
}

#[test]
fn video_through_a_lossy_relay_arrives_whole_in_order_and_recovers_with_keyframes() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    let c = sim.add(Identity::generate(), "c");
    sim.run(100);
    // 8% of the video fragments to b vanish (resends too); c gets everything.
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    sim.relay.tamper = Some(Box::new(move |to, frame| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if to == b && wire::video_layer(frame[0]).is_some() && x % 100 < 8 { vec![] } else { vec![frame.to_vec()] }
    }));
    let ia = sim.id(a);
    let mut keyframe_due = true;
    let mut sent = 0u32;
    for i in 0..300u32 {
        // 30 frames a second: 3 fragments, or 12 for a keyframe.
        let keyframe = keyframe_due || i % 60 == 0;
        keyframe_due = false;
        let data: Vec<u8> = (0..if keyframe { 12_000 } else { 3_000 }).map(|j| (i as usize ^ j) as u8).collect();
        assert!(sim.nodes[a].member.send_video(0, 0, keyframe, i, &data));
        sent += 1;
        sim.run(33);
        if sim.take(a).iter().any(|e| matches!(e, Event::KeyframeRequest { .. })) {
            keyframe_due = true;
        }
    }
    sim.run(1000);
    let frames = |events: Vec<Event>| -> Vec<(u32, bool, usize)> {
        events
            .into_iter()
            .filter_map(|e| match e {
                Event::Video { from, timestamp, keyframe, data, .. } if from == ia => {
                    // Whole: exactly the bytes sent for that frame.
                    let ok = data.iter().enumerate().all(|(j, &v)| v == (timestamp as usize ^ j) as u8);
                    assert!(ok, "frame {timestamp} intact");
                    Some((timestamp, keyframe, data.len()))
                }
                _ => None,
            })
            .collect()
    };
    let (at_b, at_c) = (frames(sim.take(b)), frames(sim.take(c)));
    assert_eq!(at_c.len() as u32, sent, "a member losing nothing gets everything");
    assert!(at_b.windows(2).all(|w| w[0].0 < w[1].0), "in order");
    assert!(at_b.first().is_some_and(|f| f.1), "starting with a keyframe");
    // Resends fill most holes; what is lost for good costs frames until the next keyframe.
    assert!(at_b.len() as u32 >= sent * 8 / 10, "{} of {sent}", at_b.len());
    assert!((at_b.len() as u32) < sent, "with 8% of fragments vanishing, something was lost");
}

#[test]
fn each_member_gets_the_video_layer_it_subscribed_to() {
    let mut sim = Sim::new();
    let a = sim.add(Identity::generate(), "a");
    let b = sim.add(Identity::generate(), "b");
    let c = sim.add(Identity::generate(), "c");
    sim.run(100);
    let ia = sim.id(a);
    // a sends two layers of one track; b wants the larger, c says nothing (the smallest sent).
    sim.nodes[b].member.subscribe(ia, Some(2)).unwrap();
    let mut keyframe_due = [true; 3];
    let mut demand = 0u8;
    for i in 0..90u32 {
        for layer in [1, 2] {
            let keyframe = keyframe_due[layer as usize] || i % 60 == 0;
            keyframe_due[layer as usize] = false;
            let data = vec![layer; if keyframe { 3000 } else { 500 }];
            assert!(sim.nodes[a].member.send_video(0, layer, keyframe, i, &data));
        }
        sim.run(33);
        for e in sim.take(a) {
            match e {
                Event::KeyframeRequest { layer, .. } => keyframe_due[layer as usize] = true,
                // A layer newly wanted starts with a keyframe, so that the relay can move to it.
                Event::VideoDemand { layers } => {
                    for (layer, due) in keyframe_due.iter_mut().enumerate().skip(1) {
                        *due |= layers & !demand & 1 << layer != 0;
                    }
                    demand = layers;
                }
                _ => {}
            }
        }
    }
    sim.run(500);
    let layers = |events: Vec<Event>| -> Vec<u8> {
        events
            .into_iter()
            .filter_map(|e| match e {
                Event::Video { from, layer, data, .. } if from == ia => {
                    assert!(data.iter().all(|&v| v == layer), "a frame of the layer it says");
                    Some(layer)
                }
                _ => None,
            })
            .collect()
    };
    let (at_b, at_c) = (layers(sim.take(b)), layers(sim.take(c)));
    assert!(at_c.len() >= 85 && at_c.iter().all(|&l| l == 1), "c: layer 1 only, {at_c:?}");
    // b gets what is sent until the relay has its SUBSCRIBE and layer 2 a keyframe: a round trip.
    let moved = at_b.iter().position(|&l| l == 2).expect("b gets layer 2");
    assert!(at_b.len() >= 85 && moved <= 3 && at_b[moved..].iter().all(|&l| l == 2), "b: {at_b:?}");

    // b moves to the smaller layer: it keeps getting pictures, and then only layer 1.
    sim.nodes[b].member.subscribe(ia, Some(1)).unwrap();
    for i in 90..150u32 {
        for layer in [1, 2] {
            let keyframe = keyframe_due[layer as usize];
            keyframe_due[layer as usize] = false;
            let data = vec![layer; if keyframe { 3000 } else { 500 }];
            assert!(sim.nodes[a].member.send_video(0, layer, keyframe, i, &data));
        }
        sim.run(33);
        for e in sim.take(a) {
            match e {
                Event::KeyframeRequest { layer, .. } => keyframe_due[layer as usize] = true,
                // A layer newly wanted starts with a keyframe, so that the relay can move to it.
                Event::VideoDemand { layers } => {
                    for (layer, due) in keyframe_due.iter_mut().enumerate().skip(1) {
                        *due |= layers & !demand & 1 << layer != 0;
                    }
                    demand = layers;
                }
                _ => {}
            }
        }
    }
    sim.run(500);
    let at_b = layers(sim.take(b));
    assert!(at_b.len() >= 55, "no gap while moving: {} frames", at_b.len());
    let first_small = at_b.iter().position(|&l| l == 1).expect("moved to layer 1");
    assert!(at_b[first_small..].iter().all(|&l| l == 1), "and stayed: {at_b:?}");
}
