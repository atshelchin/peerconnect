//! End-to-end tests of groups over real sockets: the Tokio group client
//! against a relay. As in `e2e.rs`, set `PEERCONNECT_RELAY` to test any
//! relay; tests that restart the relay or change its settings only run
//! in-process. With `PEERCONNECT_TRANSPORT=quic`, the in-process relay also
//! serves QUIC and members connect over it (raw protocol checks still use
//! WebSockets, so groups mix both).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use peerconnect::client::group::{self, GroupConnection, Via};
use peerconnect::group::transport::{self, Transport};
use peerconnect::group::{Channel, Config, Event, Group, Identity, Member, MemberId, Output, wire};
use peerconnect::wire::close;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

struct Relay {
    url: String,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Relay {
    async fn start() -> Relay {
        if let Ok(url) = std::env::var("PEERCONNECT_RELAY") {
            return Relay { url, stop: None, task: None };
        }
        Relay::bind_with("127.0.0.1:0", Default::default()).await
    }

    async fn bind_with(addr: &str, config: peerconnect_relay::Config) -> Relay {
        let quic = std::env::var("PEERCONNECT_TRANSPORT").is_ok_and(|t| t == "quic");
        // QUIC on the UDP port of the same number.
        let (listener, udp) = loop {
            let listener = TcpListener::bind(addr).await.unwrap();
            if !quic {
                break (listener, None);
            }
            match std::net::UdpSocket::bind(listener.local_addr().unwrap()) {
                Ok(udp) => break (listener, Some(udp)),
                Err(_) if addr.ends_with(":0") => continue,
                Err(err) => panic!("{err}"),
            }
        };
        let url = format!("{}://{}", if quic { "quic" } else { "ws" }, listener.local_addr().unwrap());
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            peerconnect_relay::serve_all(listener, udp, config, async {
                let _ = stopped.await;
            })
            .await
            .unwrap()
        });
        Relay { url, stop: Some(stop), task: Some(task) }
    }

    /// The relay's WebSocket URL.
    fn websocket(&self) -> String {
        websocket_url(&self.url)
    }

    fn external(&self) -> bool {
        self.task.is_none()
    }

    async fn shutdown(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.take().unwrap().await.unwrap();
    }
}

fn join(group: &Group, name: &str) -> GroupConnection {
    let config = Config { profile: name.as_bytes().to_vec(), ..Config::default() };
    let conn = group::join(group.clone(), Identity::generate(), config).unwrap();
    if group.relay().starts_with("quic://") {
        OVER_QUIC.lock().unwrap().insert(conn.id());
    }
    conn
}

/// Members that must be connected over QUIC once admitted, not fallen back.
static OVER_QUIC: std::sync::Mutex<std::collections::BTreeSet<MemberId>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

async fn wait(conn: &mut GroupConnection, what: impl Fn(&Event) -> bool) -> Event {
    timeout(Duration::from_secs(20), async {
        loop {
            let event = conn.next().await.expect("connection ended");
            if what(&event) {
                return event;
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

/// Waits until `conn` has admitted `count` other members.
async fn admitted(conn: &mut GroupConnection, count: usize) {
    let mut seen = HashSet::new();
    while seen.len() < count {
        match wait(conn, |e| matches!(e, Event::Joined { .. } | Event::Left { .. })).await {
            Event::Joined { member, .. } => seen.insert(member),
            Event::Left { member } => seen.remove(&member),
            _ => unreachable!(),
        };
    }
    let expected = if OVER_QUIC.lock().unwrap().contains(&conn.id()) { Via::Quic } else { Via::WebSocket };
    assert_eq!(conn.via(), Some(expected));
}

async fn message(conn: &mut GroupConnection, from: MemberId) -> (Channel, String) {
    match wait(conn, |e| matches!(e, Event::Message { from: f, direct: false, .. } if *f == from)).await {
        Event::Message { channel, data, .. } => (channel, String::from_utf8(data).unwrap()),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn members_talk() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "alice");
    let mut b = join(&group, "bob");
    let mut c = join(&group, "carol");
    for conn in [&mut a, &mut b, &mut c] {
        admitted(conn, 2).await;
    }
    for i in 0..20 {
        a.send(Channel::Command, format!("build {i}")).await.unwrap();
        a.send(Channel::Chat, format!("hi {i}")).await.unwrap();
    }
    for conn in [&mut b, &mut c] {
        let mut commands = Vec::new();
        let mut chat = Vec::new();
        while commands.len() < 20 || chat.len() < 20 {
            match message(conn, a.id()).await {
                (Channel::Command, text) => commands.push(text),
                (Channel::Chat, text) => chat.push(text),
            }
        }
        assert_eq!(commands, (0..20).map(|i| format!("build {i}")).collect::<Vec<_>>());
        assert_eq!(chat, (0..20).map(|i| format!("hi {i}")).collect::<Vec<_>>());
    }
    b.send_to(c.id(), Channel::Command, "psst").await.unwrap();
    let got = wait(&mut c, |e| matches!(e, Event::Message { direct: true, .. })).await;
    assert_eq!(got, Event::Message { from: b.id(), channel: Channel::Command, data: b"psst".to_vec(), direct: true });
    a.send_voice(1, b"opus frame");
    let got = wait(&mut b, |e| matches!(e, Event::Voice { .. })).await;
    assert_eq!(got, Event::Voice { from: a.id(), seq: 1, data: b"opus frame".to_vec() });
    c.send_datagram(None, b"pose");
    let got = wait(&mut a, |e| matches!(e, Event::Datagram { .. })).await;
    assert_eq!(got, Event::Datagram { from: c.id(), data: b"pose".to_vec() });

    let ib = b.id();
    b.leave().await;
    assert_eq!(wait(&mut a, |e| matches!(e, Event::Left { .. })).await, Event::Left { member: ib });
}

#[tokio::test]
async fn four_members_and_voice_latency() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let mut members: Vec<GroupConnection> = (0..4).map(|i| join(&group, &format!("m{i}"))).collect();
    for m in &mut members {
        admitted(m, 3).await;
    }
    // Three people talk at once, 20 ms frames of about 80 bytes, for 2 s.
    let start = Instant::now();
    let speakers: Vec<_> = members[..3].iter().map(|m| m.sender()).collect();
    let talk = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        for seq in 0..100u16 {
            tick.tick().await;
            let sent = start.elapsed().as_micros() as u64;
            for s in &speakers {
                s.send_voice(seq, [&sent.to_be_bytes()[..], &[0u8; 72]].concat());
            }
        }
    });
    let mut latencies = Vec::new();
    let listener = members.last_mut().unwrap();
    let mut frames = 0;
    while frames < 300 {
        let Event::Voice { data, .. } = wait(listener, |e| matches!(e, Event::Voice { .. })).await else {
            unreachable!()
        };
        let sent = u64::from_be_bytes(data[..8].try_into().unwrap());
        latencies.push(start.elapsed().as_micros() as u64 - sent);
        frames += 1;
    }
    talk.await.unwrap();
    latencies.sort();
    let p50 = latencies[latencies.len() / 2];
    let p99 = latencies[latencies.len() * 99 / 100];
    eprintln!("voice through the relay, 4 members, 3 speakers: p50 {p50} µs, p99 {p99} µs");
    if !relay.external() {
        assert!(p99 < 50_000, "p99 {p99} µs");
    }
}

#[tokio::test]
async fn messages_survive_a_relay_restart() {
    let relay = Relay::start().await;
    if relay.external() {
        return;
    }
    let address = relay.url.split_once("://").unwrap().1.to_string();
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    relay.shutdown().await;
    wait(&mut b, |e| matches!(e, Event::Left { .. })).await;
    for i in 0..10 {
        a.send(Channel::Command, format!("{i}")).await.unwrap();
    }
    let relay = Relay::bind_with(&address, Default::default()).await;
    for i in 0..10 {
        assert_eq!(message(&mut b, a.id()).await, (Channel::Command, i.to_string()));
    }
    relay.shutdown().await;
}

/// A world of `n` bytes, as an application would snapshot it.
fn world(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

#[tokio::test]
async fn a_newcomer_syncs_a_4_mb_world_while_commands_flow() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let mut host = join(&group, "host");
    let mut builder = join(&group, "builder");
    admitted(&mut host, 1).await;
    admitted(&mut builder, 1).await;
    for i in 0..100 {
        builder.send(Channel::Command, format!("op {i}")).await.unwrap();
    }
    // The host applies the builder's 100 ops (and its own echo of none).
    for _ in 0..100 {
        message(&mut host, builder.id()).await;
    }

    let mut visitor = join(&group, "visitor");
    admitted(&mut visitor, 2).await;
    visitor.request_sync(host.id()).await.unwrap();
    let Event::SyncRequest { from, transfer } = wait(&mut host, |e| matches!(e, Event::SyncRequest { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(from, visitor.id());
    // The snapshot holds ops 0..100; the builder keeps working meanwhile,
    // and its commands must not wait for the 4 MB.
    // 4 MB locally; over a real network, where state transfer yields to
    // everything else and can take a while, 512 KB.
    let size = if relay.external() { 512 << 10 } else { 4 << 20 };
    let snapshot = [&world(size)[..], b"|ops 0..100"].concat();
    host.provide_sync(from, transfer, snapshot.clone());
    let started = Instant::now();
    for i in 100..120 {
        builder.send(Channel::Command, format!("op {i}")).await.unwrap();
    }
    let mut got_snapshot = None;
    let mut after = Vec::new();
    // 4 MiB over a real network can take a while: up to 90 s for a relay
    // under test elsewhere, 20 s locally.
    let limit = Duration::from_secs(if relay.external() { 90 } else { 20 });
    while after.len() < 20 {
        let event = timeout(limit, async {
            loop {
                let event = visitor.next().await.expect("connection ended");
                if matches!(event, Event::Snapshot { .. } | Event::Message { .. }) {
                    return event;
                }
            }
        });
        match event.await.expect("timed out waiting for the snapshot") {
            Event::Snapshot { data, from } => {
                assert_eq!(from, host.id());
                assert!(after.is_empty(), "commands held until the snapshot");
                got_snapshot = Some((data, started.elapsed()));
            }
            Event::Message { channel: Channel::Command, data, .. } => after.push(String::from_utf8(data).unwrap()),
            _ => {}
        }
    }
    let (data, took) = got_snapshot.expect("a snapshot");
    assert!(data == snapshot, "snapshot intact");
    assert_eq!(after, (100..120).map(|i| format!("op {i}")).collect::<Vec<_>>());
    eprintln!("{} KiB snapshot in {took:?}", size >> 10);
}

// Raw protocol checks against the relay.

type RawWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connects and joins by hand; returns the socket, the slot and the bitmap.
async fn raw_join(group: &Group, identity: &Identity) -> (RawWs, u8, u32) {
    let mut ws = raw(group).await;
    let hello = next_binary(&mut ws).await;
    ws.send(Message::binary(join_frame(group, identity, &hello))).await.unwrap();
    let welcome = next_binary(&mut ws).await;
    assert_eq!(welcome.len(), wire::WELCOME_LEN);
    assert_eq!(welcome[0], wire::WELCOME);
    (ws, welcome[1], u32::from_be_bytes(welcome[2..].try_into().unwrap()))
}

async fn raw(group: &Group) -> RawWs {
    tokio_tungstenite::connect_async(websocket_url(&group.endpoint())).await.unwrap().0
}

/// The WebSocket a relay URL (or a URL under one) falls back to.
fn websocket_url(url: &str) -> String {
    match transport::transports(url).pop() {
        Some(Transport::WebSocket(url)) => url,
        other => panic!("{other:?}"),
    }
}

/// The JOIN a member sends for this HELLO, without going through a driver.
fn join_frame(group: &Group, identity: &Identity, hello: &[u8]) -> Vec<u8> {
    let mut m = Member::new(group.clone(), Identity::from_seed(*identity.seed()), Config::default(), 0).unwrap();
    m.poll_output();
    m.handle_open(0);
    m.handle_binary(0, hello);
    match m.poll_output() {
        Some(Output::Send(frame, _)) => frame,
        other => panic!("{other:?}"),
    }
}

async fn next_binary(ws: &mut RawWs) -> Vec<u8> {
    loop {
        match timeout(Duration::from_secs(20), ws.next()).await.unwrap() {
            Some(Ok(Message::Binary(b))) => return b.to_vec(),
            Some(Ok(Message::Text(t))) if t == "pong" => {}
            other => panic!("expected a binary frame, got {other:?}"),
        }
    }
}

async fn close_code(ws: &mut RawWs) -> Option<u16> {
    loop {
        match timeout(Duration::from_secs(20), ws.next()).await.expect("no close") {
            Some(Ok(Message::Close(frame))) => return frame.map(|f| f.code.into()),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return None,
        }
    }
}

#[tokio::test]
async fn relay_conformance() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let (ia, ib, ic) = (Identity::generate(), Identity::generate(), Identity::generate());

    let (mut a, slot_a, present) = raw_join(&group, &ia).await;
    assert_eq!((slot_a, present), (0, 0));
    let (mut b, slot_b, present) = raw_join(&group, &ib).await;
    assert_eq!((slot_b, present), (1, 1));
    assert_eq!(next_binary(&mut a).await, wire::member(1, true));

    // Frames go to everyone else, or to one slot, verbatim.
    let broadcast = vec![wire::COMMAND, slot_b, wire::BROADCAST, 9, 9];
    b.send(Message::binary(broadcast.clone())).await.unwrap();
    assert_eq!(next_binary(&mut a).await, broadcast);
    let (mut c, slot_c, _) = raw_join(&group, &ic).await;
    assert_eq!(next_binary(&mut a).await, wire::member(slot_c, true));
    assert_eq!(next_binary(&mut b).await, wire::member(slot_c, true));
    let direct = vec![wire::CHAT, slot_a, slot_c, 1];
    a.send(Message::binary(direct.clone())).await.unwrap();
    assert_eq!(next_binary(&mut c).await, direct);
    let largest = [&[wire::BULK, slot_a, slot_b][..], &vec![7; wire::MAX_FRAME_LEN - 3]].concat();
    a.send(Message::binary(largest.clone())).await.unwrap();
    assert_eq!(next_binary(&mut b).await, largest);

    // The same identity takes its old slot back; the others hear of a new connection there.
    let (mut b2, slot, _) = raw_join(&group, &ib).await;
    assert_eq!(slot, slot_b);
    assert_eq!(close_code(&mut b).await, Some(close::REPLACED));
    assert_eq!(next_binary(&mut a).await, wire::member(slot_b, true));
    assert_eq!(next_binary(&mut c).await, wire::member(slot_b, true));

    // The replaced connection's frames go nowhere.
    let _ = b.send(Message::binary(vec![wire::COMMAND, slot_b, wire::BROADCAST, 6])).await;

    // Lying about the slot, relay frames and oversized frames are refused.
    b2.send(Message::binary(vec![wire::COMMAND, slot_a, wire::BROADCAST])).await.unwrap();
    assert_eq!(close_code(&mut b2).await, Some(close::PROTOCOL_ERROR));
    assert_eq!(next_binary(&mut a).await, wire::member(slot_b, false));
    assert_eq!(next_binary(&mut c).await, wire::member(slot_b, false));
    c.send(Message::binary([&[wire::BULK, slot_c, wire::BROADCAST][..], &vec![0; wire::MAX_FRAME_LEN]].concat()))
        .await
        .unwrap();
    assert_eq!(close_code(&mut c).await, Some(close::TOO_BIG));
    assert_eq!(next_binary(&mut a).await, wire::member(slot_c, false));
    a.send(Message::binary(vec![wire::WELCOME, 0, 0, 0, 0, 0])).await.unwrap();
    assert_eq!(close_code(&mut a).await, Some(close::PROTOCOL_ERROR));

    // A JOIN for another nonce, or for a two-peer room, is refused.
    let mut d = raw(&group).await;
    next_binary(&mut d).await;
    d.send(Message::binary(join_frame(&group, &ia, &[&[wire::HELLO][..], &[0; 32]].concat()))).await.unwrap();
    assert_eq!(close_code(&mut d).await, Some(close::UNAUTHORIZED));
}

#[tokio::test]
async fn a_full_group_refuses_the_next_member() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let mut seated = Vec::new();
    // Four at most, on every relay (GROUP.md §3.2).
    for _ in 0..wire::MAX_SLOTS {
        seated.push(raw_join(&group, &Identity::generate()).await);
    }
    let mut extra = raw(&group).await;
    let hello = next_binary(&mut extra).await;
    extra.send(Message::binary(join_frame(&group, &Identity::generate(), &hello))).await.unwrap();
    assert_eq!(close_code(&mut extra).await, Some(close::FULL));
}

#[tokio::test]
async fn a_member_that_stops_reading_costs_the_others_nothing() {
    let relay = Relay::start().await;
    if relay.external() {
        return; // depends on the relay's queue sizes
    }
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    // A member that joins and then never reads again.
    let (stuck, _, _) = raw_join(&group, &Identity::generate()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Lots of traffic: voice for the stuck one is dropped, then its
    // reliable backlog overflows and the relay drops it. Meanwhile a and b
    // carry on, and do not wait for it.
    let started = Instant::now();
    let filler = "x".repeat(8000);
    for i in 0..400 {
        a.send_voice(i, vec![0u8; 200]);
        a.send(Channel::Chat, format!("{i} {filler}")).await.unwrap();
    }
    let mut count = 0;
    while count < 400 {
        if let (Channel::Chat, _) = message(&mut b, a.id()).await {
            count += 1;
        }
    }
    // At most one wait for the stuck member (5 s) before it is dropped.
    eprintln!("3.2 MB to a member while another was stuck: {:?}", started.elapsed());
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    drop(stuck);
}

#[tokio::test]
async fn commands_overtake_a_backlog_of_chat() {
    // A receiver that is not reading builds up a backlog at the relay; a
    // command sent after all of it must come out near the front.
    let relay = Relay::start().await;
    if relay.external() {
        return;
    }
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "a");
    let (mut slow, slot, _) = raw_join(&group, &Identity::generate()).await;
    wait(&mut a, |e| matches!(e, Event::State(_))).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let filler = "c".repeat(15_000);
    for i in 0..60 {
        a.send(Channel::Chat, format!("{i} {filler}")).await.unwrap(); // 900 KB
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.send(Channel::Command, "urgent").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Now read: KEY retries and the like aside, count the chat frames that
    // come before the command.
    let mut chat_before = 0;
    loop {
        let frame = next_binary(&mut slow).await;
        match frame[0] {
            wire::CHAT => chat_before += 1,
            wire::COMMAND => break,
            _ => {}
        }
    }
    eprintln!("the command overtook {} of 60 chat messages (slot {slot})", 60 - chat_before);
    // Some were already in the kernel's buffers; the rest it overtook.
    assert!(chat_before < 60, "{chat_before}");
}

#[tokio::test]
async fn keepalives_flow_and_measure_the_round_trip() {
    // Longer than the 10 s ping interval: pings go out, pongs come back,
    // and nobody is dropped for being quiet.
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    tokio::time::sleep(Duration::from_secs(12)).await;
    a.send(Channel::Chat, "still here").await.unwrap();
    assert_eq!(message(&mut b, a.id()).await, (Channel::Chat, "still here".into()));
    let mut seen = Vec::new();
    while let Ok(Some(e)) = timeout(Duration::from_millis(50), b.next()).await {
        seen.push(e);
    }
    assert!(!seen.iter().any(|e| matches!(e, Event::Left { .. })), "{seen:?}");
}

#[tokio::test]
async fn a_connection_that_never_joins_is_closed() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let started = Instant::now(); // the relay's clock starts later: at its end of the connection
    let mut ws = raw(&group).await;
    next_binary(&mut ws).await; // HELLO
    ws.send(Message::text("ping")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("pong"));
    assert_eq!(close_code(&mut ws).await, Some(close::TIMEOUT));
    assert!(started.elapsed() >= wire::JOIN_TIMEOUT - Duration::from_millis(500));
}

#[tokio::test]
async fn a_sender_over_the_rate_limit_loses_voice_and_waits_for_the_rest() {
    let relay = Relay::start().await;
    if relay.external() {
        return;
    }
    drop(relay);
    let config = peerconnect_relay::Config { group_rate: 100 << 10, group_burst: 100 << 10, ..Default::default() };
    let relay = Relay::bind_with("127.0.0.1:0", config).await;
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    let started = Instant::now();
    let filler = "x".repeat(10_000);
    for i in 0..30u16 {
        a.send(Channel::Chat, format!("{i} {filler}")).await.unwrap(); // 300 KB at 100 KB/s
        for _ in 0..10 {
            a.send_voice(i, vec![0u8; 1000]);
        }
    }
    let (mut chat, mut voice) = (0, 0);
    while chat < 30 {
        match wait(&mut b, |e| matches!(e, Event::Message { .. } | Event::Voice { .. })).await {
            Event::Message { .. } => chat += 1,
            _ => voice += 1,
        }
    }
    let took = started.elapsed();
    eprintln!("300 KB of chat at 100 KB/s took {took:?}; {voice} of 300 voice frames got through");
    assert!(took >= Duration::from_millis(1500), "paced: {took:?}");
    assert!(voice < 300, "voice over the rate is dropped");
}

/// A TCP proxy whose connections can be cut without a goodbye.
async fn cuttable_proxy(target: &str) -> (String, tokio::sync::watch::Sender<u32>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let target = target.trim_start_matches("ws://").to_string();
    let (cut, cuts) = tokio::sync::watch::channel(0u32);
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else { return };
            let target = target.clone();
            let mut cuts = cuts.clone();
            cuts.mark_unchanged();
            tokio::spawn(async move {
                let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await else { return };
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
                    _ = cuts.changed() => {} // dropped: an abrupt end, no close frame
                }
            });
        }
    });
    (url, cut)
}

#[tokio::test]
async fn an_abruptly_cut_connection_recovers() {
    let relay = Relay::start().await;
    if relay.external() {
        return;
    }
    let (proxy, cut) = cuttable_proxy(&relay.websocket()).await;
    let group = Group::create(&proxy).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    cut.send_modify(|n| *n += 1);
    for i in 0..5 {
        a.send(Channel::Command, format!("{i}")).await.unwrap();
    }
    for i in 0..5 {
        assert_eq!(message(&mut b, a.id()).await, (Channel::Command, i.to_string()));
    }
}

#[tokio::test]
async fn a_declined_sync_fails_cleanly() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    b.request_sync(a.id()).await.unwrap();
    let Event::SyncRequest { from, transfer } = wait(&mut a, |e| matches!(e, Event::SyncRequest { .. })).await else {
        unreachable!()
    };
    a.decline_sync(from, transfer);
    assert_eq!(wait(&mut b, |e| matches!(e, Event::SyncFailed { .. })).await, Event::SyncFailed { from: a.id() });
}
