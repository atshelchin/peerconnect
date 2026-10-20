//! Deterministic simulation: peers and an in-memory relay exchanging
//! messages on a virtual clock. Exercises the peer state machine end to end
//! (pairing and the move to the pair ID, reliable delivery across
//! reconnections, flow control, replacement, closing, keepalive, tampering)
//! through its public API only.

use std::collections::{HashMap, VecDeque};

use peerconnect::relay::{Action, Conn, Incoming};
use peerconnect::wire::{self, Role, close};
use peerconnect::{CloseReason, Event, Invite, Output, Peer, Room, RoomId, SendError, State};

type SocketId = u32;

#[derive(Debug, Clone)]
enum Msg {
    Text(String),
    Binary(Vec<u8>),
}

enum Delivery {
    ToRelay(SocketId, Msg),
    /// The peer closed its socket (after everything it sent before).
    Hangup(SocketId),
    ToPeer(usize, SocketId, Msg),
    Closed(usize, SocketId, Option<u16>),
}

struct Node {
    peer: Peer,
    socket: Option<SocketId>,
    events: Vec<Event>,
}

struct Socket {
    node: usize,
    room: RoomId,
    conn: Conn,
}

#[derive(Default)]
struct Relay {
    down: bool,
    sockets: HashMap<SocketId, Socket>,
    seats: HashMap<(RoomId, Role), SocketId>,
    next_socket: SocketId,
    /// Drop `pong`s, as if the connection had silently died.
    blackhole: bool,
    /// Flip a bit in the next forwarded `MESSAGE`.
    tamper: bool,
    /// Drop forwarded `INIT`s.
    drop_init: bool,
    /// Close every connection right after it joins.
    kick_after_join: bool,
    /// Cut the joiner's connection right after delivering a REPLY to it.
    kick_after_reply: bool,
    /// Cut the receiving connection after this many forwarded MESSAGE frames.
    cut_every: Option<u32>,
    forwarded: u32,
    /// Hold every frame for this node instead of delivering it.
    hold_for: Option<usize>,
    held: Vec<(usize, SocketId, Vec<u8>)>,
    /// Connections accepted so far.
    connects: u32,
}

struct Sim {
    now: u64,
    nodes: Vec<Node>,
    relay: Relay,
    queue: VecDeque<Delivery>,
}

impl Sim {
    fn new() -> Sim {
        Sim { now: 1_000, nodes: Vec::new(), relay: Relay::default(), queue: VecDeque::new() }
    }

    fn add(&mut self, room: Room) -> usize {
        self.nodes.push(Node { peer: Peer::new(room, self.now), socket: None, events: Vec::new() });
        self.run();
        self.nodes.len() - 1
    }

    /// Processes outputs and deliveries until everything is quiet.
    fn run(&mut self) {
        loop {
            let mut progress = false;
            for i in 0..self.nodes.len() {
                while let Some(output) = self.nodes[i].peer.poll_output() {
                    progress = true;
                    self.output(i, output);
                }
            }
            while let Some(delivery) = self.queue.pop_front() {
                progress = true;
                self.deliver(delivery);
            }
            if !progress {
                return;
            }
        }
    }

    /// Advances the clock by `ms`, firing every timer on the way.
    fn advance(&mut self, ms: u64) {
        let end = self.now + ms;
        loop {
            let next = self.nodes.iter().filter_map(|n| n.peer.poll_timeout()).min();
            match next {
                Some(t) if t <= end => {
                    self.now = self.now.max(t);
                    for node in &mut self.nodes {
                        if node.peer.poll_timeout().is_some_and(|t| t <= self.now) {
                            node.peer.handle_timeout(self.now);
                        }
                    }
                    self.run();
                }
                _ => break,
            }
        }
        self.now = end;
    }

    fn output(&mut self, i: usize, output: Output) {
        let now = self.now;
        let socket = self.nodes[i].socket;
        match output {
            Output::Connect(url) => {
                assert!(socket.is_none(), "a peer opens one socket at a time");
                if self.relay.down {
                    self.nodes[i].peer.handle_close(now, None);
                    return;
                }
                let path = url.split_once("://").unwrap().1;
                let room = peerconnect::relay::room_from_path(&path[path.find('/').unwrap()..]).unwrap();
                let id = self.relay.next_socket;
                self.relay.next_socket += 1;
                let (conn, hello) = Conn::new();
                self.relay.connects += 1;
                self.relay.sockets.insert(id, Socket { node: i, room, conn });
                self.nodes[i].socket = Some(id);
                self.nodes[i].peer.handle_open(now);
                self.queue.push_back(Delivery::ToPeer(i, id, Msg::Binary(hello.to_vec())));
            }
            // Writing to a socket that has already closed goes nowhere.
            Output::Send(frame) => {
                if let Some(socket) = socket {
                    self.queue.push_back(Delivery::ToRelay(socket, Msg::Binary(frame)));
                }
            }
            Output::Ping => {
                if let Some(socket) = socket {
                    self.queue.push_back(Delivery::ToRelay(socket, Msg::Text("ping".into())));
                }
            }
            Output::Disconnect => {
                if let Some(id) = self.nodes[i].socket.take() {
                    self.queue.push_back(Delivery::Hangup(id));
                }
            }
            Output::Event(event) => self.nodes[i].events.push(event),
        }
    }

    fn deliver(&mut self, delivery: Delivery) {
        let now = self.now;
        match delivery {
            Delivery::ToPeer(i, id, msg) => {
                if self.nodes[i].socket != Some(id) {
                    return;
                }
                match msg {
                    Msg::Text(text) => self.nodes[i].peer.handle_text(now, &text),
                    Msg::Binary(frame) => self.nodes[i].peer.handle_binary(now, &frame),
                }
            }
            Delivery::Hangup(id) => self.drop_socket(id, None),
            Delivery::Closed(i, id, code) => {
                if self.nodes[i].socket == Some(id) {
                    self.nodes[i].socket = None;
                    self.nodes[i].peer.handle_close(now, code);
                }
            }
            Delivery::ToRelay(id, msg) => {
                let Some(socket) = self.relay.sockets.get(&id) else { return };
                let incoming = match &msg {
                    Msg::Text(text) => Incoming::Text(text),
                    Msg::Binary(frame) => Incoming::Binary(frame),
                };
                match socket.conn.receive(&socket.room, incoming) {
                    Action::Pong => {
                        if !self.relay.blackhole {
                            self.queue.push_back(Delivery::ToPeer(socket.node, id, Msg::Text("pong".into())));
                        }
                    }
                    Action::Join(role) => self.join(id, role),
                    Action::Forward => {
                        let Msg::Binary(mut frame) = msg else { unreachable!() };
                        let partner = self.relay.seats.get(&(socket.room, role_of(socket).other())).copied();
                        if frame[0] == wire::INIT && self.relay.drop_init {
                            return;
                        }
                        if frame[0] == wire::MESSAGE && std::mem::take(&mut self.relay.tamper) {
                            frame[5] ^= 1;
                        }
                        if let Some(p) = partner {
                            let node = self.relay.sockets[&p].node;
                            let kind = frame[0];
                            if self.relay.hold_for == Some(node) {
                                self.relay.held.push((node, p, frame));
                                return;
                            }
                            self.queue.push_back(Delivery::ToPeer(node, p, Msg::Binary(frame)));
                            if kind == wire::REPLY && self.relay.kick_after_reply {
                                self.relay.kick_after_reply = false;
                                self.queue.push_back(Delivery::Hangup(p));
                                self.queue.push_back(Delivery::Closed(node, p, None));
                            }
                            if kind == wire::MESSAGE
                                && let Some(n) = self.relay.cut_every
                            {
                                self.relay.forwarded += 1;
                                if self.relay.forwarded.is_multiple_of(n) {
                                    self.queue.push_back(Delivery::Hangup(p));
                                    self.queue.push_back(Delivery::Closed(node, p, None));
                                }
                            }
                        }
                    }
                    Action::Close(code) => self.drop_socket(id, Some(code)),
                }
            }
        }
    }

    fn join(&mut self, id: SocketId, role: Role) {
        let room = self.relay.sockets[&id].room;
        if let Some(old) = self.relay.seats.remove(&(room, role)) {
            let old = self.relay.sockets.remove(&old).map(|s| (s.node, old)).unwrap();
            self.queue.push_back(Delivery::Closed(old.0, old.1, Some(close::REPLACED)));
        }
        self.relay.seats.insert((room, role), id);
        let socket = self.relay.sockets.get_mut(&id).unwrap();
        socket.conn = Conn::Seated(role);
        let node = socket.node;
        let partner = self.relay.seats.get(&(room, role.other())).copied();
        self.queue.push_back(Delivery::ToPeer(node, id, Msg::Binary(wire::presence(partner.is_some()).to_vec())));
        if let Some(p) = partner {
            let pnode = self.relay.sockets[&p].node;
            self.queue.push_back(Delivery::ToPeer(pnode, p, Msg::Binary(wire::presence(true).to_vec())));
        }
        if self.relay.kick_after_join {
            self.drop_socket(id, Some(close::GOING_AWAY));
        }
    }

    /// Removes a socket; `code` is what the relay closes it with (None when
    /// the peer closed it).
    fn drop_socket(&mut self, id: SocketId, code: Option<u16>) {
        let Some(socket) = self.relay.sockets.remove(&id) else { return };
        if code.is_some() {
            self.queue.push_back(Delivery::Closed(socket.node, id, code));
        }
        if let Conn::Seated(role) = socket.conn {
            self.relay.seats.remove(&(socket.room, role));
            if let Some(&p) = self.relay.seats.get(&(socket.room, role.other())) {
                let pnode = self.relay.sockets[&p].node;
                self.queue.push_back(Delivery::ToPeer(pnode, p, Msg::Binary(wire::presence(false).to_vec())));
            }
        }
    }

    /// The relay restarts: every connection drops, all state is lost.
    fn restart_relay(&mut self) {
        for (id, socket) in std::mem::take(&mut self.relay.sockets) {
            self.queue.push_back(Delivery::Closed(socket.node, id, Some(close::GOING_AWAY)));
        }
        self.relay.seats.clear();
        self.run();
    }

    fn send(&mut self, i: usize, data: &[u8]) -> Result<u64, SendError> {
        let result = self.nodes[i].peer.send(data);
        self.run();
        result
    }

    /// Lets the frames held for a node through.
    fn release(&mut self) {
        self.relay.hold_for = None;
        for (node, socket, frame) in std::mem::take(&mut self.relay.held) {
            self.queue.push_back(Delivery::ToPeer(node, socket, Msg::Binary(frame)));
        }
        self.run();
    }

    /// The room ID a node is connected at, if any.
    fn at(&self, i: usize) -> Option<RoomId> {
        self.relay.sockets.get(&self.nodes[i].socket?).map(|s| s.room)
    }

    fn state(&self, i: usize) -> State {
        self.nodes[i].peer.state()
    }

    fn messages(&mut self, i: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        self.nodes[i].events.retain(|e| match e {
            Event::Message(m) => {
                out.push(m.clone());
                false
            }
            _ => true,
        });
        out
    }

    fn events(&mut self, i: usize) -> Vec<Event> {
        std::mem::take(&mut self.nodes[i].events)
    }
}

fn role_of(socket: &Socket) -> Role {
    match socket.conn {
        Conn::Seated(role) => role,
        Conn::Pending(_) => unreachable!("only seated connections forward"),
    }
}

/// Pairs two peers. Returns their nodes and their paired room records.
fn pair(sim: &mut Sim) -> (usize, usize, Room, Room) {
    let inviter = Room::create("wss://relay.test").unwrap();
    let invite_id = inviter.id();
    let invite: Invite = inviter.invite().unwrap().to_string().parse().unwrap();
    let joiner = Room::join(&invite);
    let a = sim.add(inviter);
    assert_eq!(sim.state(a), State::Waiting);
    let b = sim.add(joiner);
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
    // Pairing moved both to the pair ID, where the invite does not reach.
    let (ra, rb) = (sim.nodes[a].peer.room().clone(), sim.nodes[b].peer.room().clone());
    assert!(ra.is_paired() && rb.is_paired());
    assert_eq!(ra.id(), rb.id());
    assert_ne!(ra.id(), invite_id);
    assert_eq!((sim.at(a), sim.at(b)), (Some(ra.id()), Some(ra.id())));
    (a, b, ra, rb)
}

fn count(events: &[Event], what: &Event) -> usize {
    events.iter().filter(|e| *e == what).count()
}

#[test]
fn pairs_moves_to_the_pair_id_and_exchanges_messages() {
    let mut sim = Sim::new();
    let (a, b, inviter, joiner) = pair(&mut sim);
    assert_eq!(count(&sim.events(a), &Event::Paired), 1);
    assert_eq!(count(&sim.events(b), &Event::Paired), 1);
    assert_eq!(inviter.peer_key(), Some(joiner.public_key()));
    assert_eq!(joiner.peer_key(), Some(inviter.public_key()));

    assert_eq!(sim.send(a, b"hello"), Ok(0));
    assert_eq!(sim.send(b, b""), Ok(0));
    assert_eq!(sim.send(b, b"world"), Ok(1));
    assert_eq!(sim.messages(b), vec![b"hello".to_vec()]);
    assert_eq!(sim.messages(a), vec![b"".to_vec(), b"world".to_vec()]);
    sim.advance(100); // acknowledgements are batched briefly
    assert!(sim.events(a).contains(&Event::Delivered(1)));
    assert!(sim.events(b).contains(&Event::Delivered(2)));
    assert_eq!(sim.nodes[a].peer.queued(), 0);
}

#[test]
fn either_peer_may_arrive_first() {
    let mut sim = Sim::new();
    let inviter = Room::create("wss://relay.test").unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let b = sim.add(joiner);
    assert_eq!(sim.state(b), State::Waiting);
    sim.advance(5_000);
    let a = sim.add(inviter);
    sim.advance(25_000); // the joiner may be looking at the pair ID meanwhile
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
    assert!(sim.nodes[a].peer.room().is_paired() && sim.nodes[b].peer.room().is_paired());
}

#[test]
fn splits_and_reassembles_large_messages() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    let big: Vec<u8> = (0..1_000_000u32).map(|i| (i * 7) as u8).collect();
    let exact = vec![1u8; 65518];
    sim.send(a, &big).unwrap();
    sim.send(a, &exact).unwrap();
    sim.send(a, &[2u8; 65519]).unwrap();
    let got = sim.messages(b);
    assert_eq!(got.len(), 3);
    assert_eq!(got[0], big);
    assert_eq!(got[1], exact);
    assert_eq!(got[2], vec![2u8; 65519]);

    assert_eq!(sim.send(a, &vec![0; wire::MAX_MESSAGE_LEN + 1]), Err(SendError::TooLarge));
    sim.send(a, &vec![3; wire::MAX_MESSAGE_LEN]).unwrap();
    assert_eq!(sim.messages(b)[0].len(), wire::MAX_MESSAGE_LEN);
}

#[test]
fn messages_sent_while_apart_arrive_after_reconnecting() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    sim.events(a);

    sim.relay.down = true;
    sim.restart_relay();
    assert_eq!((sim.state(a), sim.state(b)), (State::Offline, State::Offline));
    // Sending while apart queues the messages.
    for i in 0..5u8 {
        sim.send(a, &[i]).unwrap();
    }
    sim.send(b, b"from b").unwrap();
    assert!(sim.nodes[a].peer.queued() > 0);

    sim.advance(5_000); // retries fail while the relay is down, backing off
    sim.relay.down = false;
    sim.advance(10_000);
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
    assert_eq!(sim.messages(b), (0..5u8).map(|i| vec![i]).collect::<Vec<_>>());
    assert_eq!(sim.messages(a), vec![b"from b".to_vec()]);
    assert!(!sim.events(a).contains(&Event::Paired), "pairing happens once");
    sim.advance(100);
    assert_eq!(sim.nodes[a].peer.queued(), 0);
}

#[test]
fn nothing_is_lost_or_duplicated_however_often_connections_drop() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    sim.relay.cut_every = Some(7); // cut the receiving connection every 7 frames
    let mut sent = Vec::new();
    for round in 0..40u32 {
        for i in 0..10u32 {
            let message = (round * 10 + i).to_be_bytes().to_vec();
            if sim.send(a, &message).is_ok() {
                sent.push(message);
            }
        }
        sim.send(b, &round.to_be_bytes()).unwrap();
        sim.advance(2_000);
    }
    sim.relay.cut_every = None;
    sim.advance(60_000);
    assert_eq!(sim.messages(b), sent);
    assert_eq!(sim.messages(a), (0..40u32).map(|r| r.to_be_bytes().to_vec()).collect::<Vec<_>>());
    assert_eq!(sim.nodes[a].peer.queued(), 0);
}

#[test]
fn senders_respect_the_window_and_the_outbox_limit() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    sim.relay.hold_for = Some(b); // b receives nothing, so acknowledges nothing
    let chunk = vec![9u8; 1 << 20];
    let mut accepted = 0;
    while sim.send(a, &chunk).is_ok() {
        accepted += 1;
    }
    assert_eq!(accepted, peerconnect::OUTBOX_LIMIT / chunk.len());
    assert_eq!(sim.send(a, &chunk), Err(SendError::Full));
    // Only about a window's worth went onto the wire.
    let held: usize = sim.relay.held.iter().map(|(_, _, f)| f.len()).sum();
    assert!(held >= wire::WINDOW && held < wire::WINDOW + 2 * chunk.len(), "{held}");
    // Once b catches up, everything flows and the outbox drains.
    sim.release();
    sim.advance(10_000);
    assert_eq!(sim.messages(b).len(), accepted);
    assert_eq!(sim.nodes[a].peer.queued(), 0);
    assert!(sim.send(a, &chunk).is_ok());
}

#[test]
fn reconnects_after_partner_drops() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    let id = sim.nodes[b].socket.unwrap();
    sim.drop_socket(id, None); // b's connection vanishes...
    sim.queue.push_back(Delivery::Closed(b, id, None));
    sim.run();
    assert_eq!(sim.state(a), State::Waiting);
    assert_eq!(sim.state(b), State::Offline);
    sim.advance(1_000); // ...and comes back within the first backoff step
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
}

#[test]
fn newer_connection_replaces_older_one() {
    let mut sim = Sim::new();
    let (a, b, _, joiner) = pair(&mut sim);
    let b2 = sim.add(joiner); // the same room opened a second time
    assert!(sim.events(b).contains(&Event::Replaced));
    assert_eq!(sim.state(b), State::Offline);
    assert_eq!((sim.state(a), sim.state(b2)), (State::Connected, State::Connected));
    sim.send(a, b"to the new one").unwrap();
    assert_eq!(sim.messages(b2), vec![b"to the new one".to_vec()]);
    assert!(sim.messages(b).is_empty());

    // The replaced instance stays away for a minute, then takes the seat back
    // (unless the application tells it to leave).
    sim.advance(55_000);
    assert_eq!(sim.state(b), State::Offline);
    sim.advance(15_000);
    assert_eq!(sim.state(b), State::Connected);
    assert!(sim.events(b2).contains(&Event::Replaced));
    sim.nodes[b2].peer.leave();
    sim.run();
    sim.advance(120_000);
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
}

#[test]
fn closing_ends_the_room_on_both_sides() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    sim.send(a, b"last words").unwrap();
    sim.nodes[a].peer.close();
    assert!(sim.nodes[a].peer.room().is_closing());
    assert_eq!(sim.nodes[a].peer.send(b"too late"), Err(SendError::Closed));
    sim.run();
    assert_eq!(sim.messages(b), vec![b"last words".to_vec()]);
    assert_eq!(sim.state(a), State::Closed(CloseReason::Closed));
    assert_eq!(sim.state(b), State::Closed(CloseReason::ClosedByPeer));
    sim.advance(120_000);
    assert!(sim.nodes.iter().all(|n| n.socket.is_none() && n.peer.poll_timeout().is_none()));
}

#[test]
fn closing_reaches_a_peer_that_was_away() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    sim.nodes[b].peer.leave();
    sim.run();
    sim.nodes[a].peer.close();
    sim.run();
    sim.advance(60_000);
    assert_ne!(sim.state(a), State::Closed(CloseReason::Closed), "nobody has been told yet");

    // a stops and restarts from its saved record, which says it is closing.
    let saved: Room = sim.nodes[a].peer.room().to_string().parse().unwrap();
    assert!(saved.is_closing());
    sim.nodes[a].peer.leave();
    sim.run();
    let a2 = sim.add(saved);
    // b comes back, and is told.
    let b_saved = sim.nodes[b].peer.room().clone();
    let b2 = sim.add(b_saved);
    sim.advance(1_000);
    assert_eq!(sim.state(b2), State::Closed(CloseReason::ClosedByPeer));
    assert_eq!(sim.state(a2), State::Closed(CloseReason::Closed));
}

#[test]
fn leaving_keeps_the_room() {
    let mut sim = Sim::new();
    let (a, b, inviter, _) = pair(&mut sim);
    sim.nodes[a].peer.leave();
    sim.run();
    assert_eq!(sim.state(a), State::Closed(CloseReason::Left));
    assert_eq!(sim.state(b), State::Waiting);
    let a2 = sim.add(inviter);
    assert_eq!((sim.state(a2), sim.state(b)), (State::Connected, State::Connected));
}

#[test]
fn a_leaked_invite_reaches_nobody_after_pairing() {
    let mut sim = Sim::new();
    let inviter = Room::create("wss://relay.test").unwrap();
    let invite = inviter.invite().unwrap();
    let (a, b) = (sim.add(inviter), sim.add(Room::join(&invite)));
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));

    // A stranger with the invite finds an empty room: the peers have moved.
    let stranger = sim.add(Room::join(&invite));
    sim.advance(120_000);
    assert_ne!(sim.state(stranger), State::Connected);
    assert!(sim.events(a).iter().chain(sim.events(b).iter()).all(|e| *e != Event::Replaced));
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
    sim.send(a, b"still private").unwrap();
    assert_eq!(sim.messages(b), vec![b"still private".to_vec()]);
    assert!(sim.messages(stranger).is_empty());
}

#[test]
fn pairing_completes_even_if_its_confirmation_is_lost() {
    let mut sim = Sim::new();
    sim.relay.kick_after_reply = true; // the joiner's confirmation never arrives
    let inviter = Room::create("wss://relay.test").unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let (a, b) = (sim.add(inviter), sim.add(joiner));
    assert!(!sim.nodes[a].peer.room().is_paired());
    // The joiner looks at both IDs in turn until it finds the inviter again.
    sim.advance(90_000);
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
    assert!(sim.nodes[a].peer.room().is_paired() && sim.nodes[b].peer.room().is_paired());
    assert_eq!(sim.at(a), Some(sim.nodes[a].peer.room().id()));
}

#[test]
fn an_inviter_restarted_before_pairing_completes_still_pairs() {
    let mut sim = Sim::new();
    sim.relay.kick_after_reply = true;
    let inviter = Room::create("wss://relay.test").unwrap();
    let record = inviter.to_string(); // saved before pairing
    let joiner = Room::join(&inviter.invite().unwrap());
    let (a, b) = (sim.add(inviter), sim.add(joiner));
    sim.nodes[a].peer.leave(); // a forgets its tentative pin
    sim.run();
    let a2 = sim.add(record.parse().unwrap());
    sim.advance(90_000);
    assert_eq!((sim.state(a2), sim.state(b)), (State::Connected, State::Connected));
    assert_eq!(sim.nodes[a2].peer.room().peer_key(), Some(sim.nodes[b].peer.room().public_key()));
}

#[test]
fn keepalive_detects_a_dead_connection() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    sim.advance(10 * 60_000); // pongs keep everything up
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));

    sim.relay.blackhole = true;
    sim.advance(wire::RECEIVE_TIMEOUT.as_millis() as u64 + 1);
    assert_ne!(sim.state(a), State::Connected);
    sim.relay.blackhole = false;
    sim.advance(60_000);
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
}

#[test]
fn unanswered_handshake_is_retried() {
    let mut sim = Sim::new();
    sim.relay.drop_init = true;
    let inviter = Room::create("wss://relay.test").unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let (a, b) = (sim.add(inviter), sim.add(joiner));
    assert_eq!((sim.state(a), sim.state(b)), (State::Waiting, State::Waiting));
    sim.advance(wire::HANDSHAKE_TIMEOUT.as_millis() as u64 + 1);
    assert_eq!(sim.state(b), State::Offline);
    sim.relay.drop_init = false;
    sim.advance(60_000);
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
}

#[test]
fn tampering_breaks_the_session_but_loses_nothing() {
    let mut sim = Sim::new();
    let (a, b, ..) = pair(&mut sim);
    sim.relay.tamper = true;
    sim.send(a, b"original").unwrap();
    assert!(sim.messages(b).is_empty());
    assert_eq!(sim.state(b), State::Offline);
    sim.send(a, b"next").unwrap();
    sim.advance(5_000);
    assert_eq!((sim.state(a), sim.state(b)), (State::Connected, State::Connected));
    // The tampered copy was rejected, and the real one resent.
    assert_eq!(sim.messages(b), vec![b"original".to_vec(), b"next".to_vec()]);
}

#[test]
fn backoff_grows_while_the_relay_is_down() {
    let mut sim = Sim::new();
    sim.relay.down = true;
    let a = sim.add(Room::create("wss://relay.test").unwrap());
    let mut gaps = Vec::new();
    let mut last = sim.now;
    for _ in 0..10 {
        let next = sim.nodes[a].peer.poll_timeout().unwrap();
        gaps.push(next - last);
        last = next;
        sim.advance(next - sim.now);
    }
    assert!(gaps[1] <= 1_000 && gaps[9] >= 15_000 && gaps[9] <= 30_000, "{gaps:?}");
}

#[test]
fn connections_that_fail_right_after_joining_back_off() {
    let mut sim = Sim::new();
    sim.relay.kick_after_join = true;
    sim.add(Room::create("wss://relay.test").unwrap());
    sim.advance(120_000);
    // Constant half-second retries would make ~240 attempts in two minutes.
    assert!(sim.relay.connects < 15, "{} attempts", sim.relay.connects);
}
