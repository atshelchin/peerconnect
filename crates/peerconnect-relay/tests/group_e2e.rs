//! End-to-end tests of groups over real sockets: the Tokio group client
//! against a relay, over QUIC, the only way groups travel. As in `e2e.rs`,
//! set `PEERCONNECT_RELAY` to test any relay, here by its group relay URL
//! (`https://…` or `quic://…`); tests that restart the relay or change its
//! settings only run in-process. The in-process relay serves QUIC on the UDP
//! port of the same number as its TCP one.

mod common;

use std::time::{Duration, Instant};

use common::{LocalRelay, Path, Wire, admitted, join, join_frame, wait, wait_up_to};
use futures_util::StreamExt;
use peerconnect::client::group::GroupConnection;
use peerconnect::group::{Channel, Class, Event, Group, Identity, MemberId, wire};
use peerconnect::wire::close;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

/// The relay under test: in-process, or the one `PEERCONNECT_RELAY` names.
struct Relay {
    url: String,
    local: Option<LocalRelay>,
}

impl Relay {
    async fn start() -> Relay {
        if let Ok(url) = std::env::var("PEERCONNECT_RELAY") {
            assert!(
                url.starts_with("https://") || url.starts_with("quic://"),
                "PEERCONNECT_RELAY={url}: groups travel over QUIC only, so an https:// or quic:// URL"
            );
            return Relay { url, local: None };
        }
        Relay::bind_with("127.0.0.1:0", Default::default()).await
    }

    async fn bind_with(addr: &str, config: peerconnect_relay::Config) -> Relay {
        let local = LocalRelay::bind(addr, config, true).await;
        Relay { url: local.url("quic"), local: Some(local) }
    }

    fn external(&self) -> bool {
        self.local.is_none()
    }

    async fn shutdown(self) {
        self.local.unwrap().shutdown().await;
    }
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

// Raw protocol checks against the relay, over raw QUIC: the path first on
// the command stream, then each class on a stream of its own, voice as
// datagrams.

/// Connects and joins by hand; returns the session, the slot and the bitmap.
async fn raw_join(group: &Group, identity: &Identity) -> (Wire, u8, u32) {
    let mut wire = Wire::raw(&group.endpoint()).await;
    let (slot, present) = wire.join(group, identity).await;
    (wire, slot, present)
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
    assert_eq!(a.frame().await, wire::member(1, true));

    // Frames go to everyone else, or to one slot, verbatim, on their class's stream.
    let broadcast = vec![wire::COMMAND, slot_b, wire::BROADCAST, 9, 9];
    b.send(Class::Command, &broadcast).await;
    assert_eq!(a.next().await, common::Got::Frame(Class::Command, broadcast));
    let (mut c, slot_c, _) = raw_join(&group, &ic).await;
    assert_eq!(a.frame().await, wire::member(slot_c, true));
    assert_eq!(b.frame().await, wire::member(slot_c, true));
    let direct = vec![wire::CHAT, slot_a, slot_c, 1];
    a.send(Class::Chat, &direct).await;
    assert_eq!(c.next().await, common::Got::Frame(Class::Chat, direct));
    let largest = [&[wire::BULK, slot_a, slot_b][..], &vec![7; wire::MAX_FRAME_LEN - 3]].concat();
    a.send(Class::Bulk, &largest).await;
    assert_eq!(b.next().await, common::Got::Frame(Class::Bulk, largest));
    let voice = vec![wire::VOICE, slot_c, wire::BROADCAST, 5, 5];
    c.datagram(&voice);
    assert_eq!(a.next().await, common::Got::Datagram(voice.clone()));
    assert_eq!(b.next().await, common::Got::Datagram(voice));

    // The same identity takes its old slot back; the others hear of a new connection there.
    let (mut b2, slot, _) = raw_join(&group, &ib).await;
    assert_eq!(slot, slot_b);
    assert_eq!(b.closed().await, Some(close::REPLACED));
    assert_eq!(a.frame().await, wire::member(slot_b, true));
    assert_eq!(c.frame().await, wire::member(slot_b, true));

    // The replaced connection's frames go nowhere.
    b.send(Class::Command, &[wire::COMMAND, slot_b, wire::BROADCAST, 6]).await;

    // Lying about the slot, relay frames and oversized frames are refused.
    b2.send(Class::Command, &[wire::COMMAND, slot_a, wire::BROADCAST]).await;
    assert_eq!(b2.closed().await, Some(close::PROTOCOL_ERROR));
    assert_eq!(a.frame().await, wire::member(slot_b, false));
    assert_eq!(c.frame().await, wire::member(slot_b, false));
    let oversized = [&[wire::BULK, slot_c, wire::BROADCAST][..], &vec![0; wire::MAX_FRAME_LEN]].concat();
    c.write(Class::Bulk, &[&(oversized.len() as u16).to_be_bytes()[..], &oversized].concat()).await;
    assert_eq!(c.closed().await, Some(close::TOO_BIG));
    assert_eq!(a.frame().await, wire::member(slot_c, false));
    a.send(Class::Command, &[wire::WELCOME, 0, 0, 0, 0, 0]).await;
    assert_eq!(a.closed().await, Some(close::PROTOCOL_ERROR));

    // A JOIN for another nonce, or for a two-peer room, is refused.
    let mut d = Wire::raw(&group.endpoint()).await;
    d.frame().await;
    d.send(Class::Command, &join_frame(&group, &ia, &[&[wire::HELLO][..], &[0; 32]].concat())).await;
    assert_eq!(d.closed().await, Some(close::UNAUTHORIZED));
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
    let mut extra = Wire::raw(&group.endpoint()).await;
    let hello = extra.frame().await;
    extra.send(Class::Command, &join_frame(&group, &Identity::generate(), &hello)).await;
    assert_eq!(extra.closed().await, Some(close::FULL));
}

#[tokio::test]
async fn groups_are_not_served_over_websockets() {
    let relay = Relay::start().await;
    let Some(local) = &relay.local else {
        return; // where an external relay serves TCP, if at all, is its own business
    };
    let group = Group::create(&relay.url).unwrap();
    // A group path is refused before the upgrade.
    let refused = tokio_tungstenite::connect_async(format!("{}/g1/{}", local.url("ws"), group.id())).await;
    match refused {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => assert_eq!(response.status(), 404),
        other => panic!("{:?}", other.map(|(_, response)| response)),
    }
    // A two-peer room on the same port still upgrades.
    let room = peerconnect::Room::create(&local.url("ws")).unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{}/v1/{}", local.url("ws"), room.id())).await.unwrap();
    match timeout(Duration::from_secs(10), ws.next()).await.unwrap() {
        Some(Ok(Message::Binary(hello))) => assert_eq!(hello[0], peerconnect::wire::HELLO),
        other => panic!("{other:?}"),
    }
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
    // A member that joins and then never reads again: QUIC flow control
    // soon stops the relay sending to it.
    let (stuck, _, _) = raw_join(&group, &Identity::generate()).await;
    stuck.read(false);
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
    slow.read(false);
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
    slow.read(true);
    let mut chat_before = 0;
    loop {
        let frame = slow.frame().await;
        match frame[0] {
            wire::CHAT => chat_before += 1,
            wire::COMMAND => break,
            _ => {}
        }
    }
    eprintln!("the command overtook {} of 60 chat messages (slot {slot})", 60 - chat_before);
    // Some were already in the chat stream's window; the rest it overtook.
    assert!(chat_before < 60, "{chat_before}");
}

#[tokio::test]
async fn keepalives_flow_and_measure_the_round_trip() {
    // Longer than the 10 s ping interval: pings (the empty message on the
    // command stream) go out, pongs come back, and nobody is dropped for
    // being quiet.
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
    assert!(a.connected() && b.connected());
}

#[tokio::test]
async fn a_connection_that_never_joins_is_closed() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let started = Instant::now(); // the relay's clock starts later: at its end of the connection
    let mut w = Wire::raw(&group.endpoint()).await;
    assert_eq!(w.frame().await[0], wire::HELLO);
    // The empty message is ping, answered with the empty message, pong.
    w.send(Class::Command, &[]).await;
    assert_eq!(w.next().await, common::Got::Pong);
    assert_eq!(w.closed().await, Some(close::TIMEOUT));
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

#[tokio::test]
async fn an_abruptly_cut_connection_recovers() {
    let relay = Relay::start().await;
    let Some(local) = &relay.local else { return };
    let (pa, pb) = (Path::new(local.addr, Duration::ZERO).await, Path::new(local.addr, Duration::ZERO).await);
    let group = Group::create(&pa.url("quic")).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group.with_relay(&pb.url("quic")).unwrap(), "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    // Both connections end without a goodbye: no close, no reset, nothing
    // gets through. Each member notices only once the relay has been silent
    // for too long, and connects again (from a new port, which gets through).
    pa.cut();
    pb.cut();
    for i in 0..5 {
        a.send(Channel::Command, format!("{i}")).await.unwrap();
    }
    let limit = wire::RECEIVE_TIMEOUT + Duration::from_secs(20);
    let first = wait_up_to(&mut b, limit, |e| matches!(e, Event::Message { .. })).await;
    assert_eq!(first, Event::Message { from: a.id(), channel: Channel::Command, data: b"0".to_vec(), direct: false });
    for i in 1..5 {
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

#[tokio::test]
async fn each_member_gets_the_video_layer_it_subscribed_to() {
    let relay = Relay::start().await;
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    let mut c = join(&group, "c");
    for conn in [&mut a, &mut b, &mut c] {
        admitted(conn, 2).await;
    }
    // a sends layers 1 and 2; b wants 2, c says nothing and gets the smallest sent.
    b.subscribe(a.id(), 0, Some(2));
    let sender = a.sender();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    let mut keyframe_due = [true; 3];
    let mut demand = 0u8;
    for i in 0..90u32 {
        tick.tick().await;
        for layer in [1u8, 2] {
            let keyframe = keyframe_due[layer as usize] || i % 60 == 0;
            keyframe_due[layer as usize] = false;
            sender.send_video(0, layer, keyframe, i, vec![layer; if keyframe { 6000 } else { 1500 }]);
        }
        while let Some(event) = a.try_next() {
            match event {
                Event::KeyframeRequest { layer, .. } => keyframe_due[layer as usize] = true,
                Event::VideoDemand { track: 0, layers } => {
                    for (layer, due) in keyframe_due.iter_mut().enumerate().skip(1) {
                        *due |= layers & !demand & 1 << layer != 0;
                    }
                    demand = layers;
                }
                _ => {}
            }
        }
    }
    assert_eq!(demand, 0b111, "layer 2 for b; for c, layer 0, and layer 1, which it gets");
    let layers = |conn: &mut GroupConnection, from: MemberId| {
        let mut got = Vec::new();
        while let Some(event) = conn.try_next() {
            if let Event::Video { from: f, layer, data, .. } = event
                && f == from
            {
                assert!(data.iter().all(|&v| v == layer));
                got.push(layer);
            }
        }
        got
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (at_b, at_c) = (layers(&mut b, a.id()), layers(&mut c, a.id()));
    assert!(at_c.len() >= 80 && at_c.iter().all(|&l| l == 1), "c: {at_c:?}");
    let moved = at_b.iter().position(|&l| l == 2).expect("b gets layer 2");
    assert!(at_b.len() >= 80 && moved <= 10 && at_b[moved..].iter().all(|&l| l == 2), "b: {at_b:?}");
}

#[tokio::test]
async fn a_slow_link_gets_smaller_video_and_more_again_once_it_keeps_up() {
    let relay = Relay::start().await;
    let Some(local) = &relay.local else { return }; // shapes the path
    let slow = Path::new(local.addr, Duration::ZERO).await;
    slow.down_kbps.store(1000, std::sync::atomic::Ordering::Relaxed);
    let group = Group::create(&relay.url).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group.with_relay(&slow.url("quic")).unwrap(), "b");
    let mut c = join(&group, "c");
    for conn in [&mut a, &mut b, &mut c] {
        admitted(conn, 2).await;
    }
    for conn in [&b, &c] {
        conn.subscribe(a.id(), 0, Some(2));
    }
    // a sends 360p at 400 kbit/s and 720p at 1.5 Mbit/s, 30 frames a second;
    // b's link takes 1 Mbit/s for the first 6 s, then whatever comes.
    let sender = a.sender();
    let start = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    let (mut keyframe_due, mut demand) = ([true; 3], 0u8);
    let (mut at_b, mut at_c, mut limits) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..(22_000 / 33) {
        tick.tick().await;
        if start.elapsed() > Duration::from_secs(6) {
            slow.down_kbps.store(0, std::sync::atomic::Ordering::Relaxed);
        }
        for (layer, size) in [(1u8, 1666), (2, 6250)] {
            let keyframe = keyframe_due[layer as usize] || i % 60 == 0;
            keyframe_due[layer as usize] = false;
            sender.send_video(0, layer, keyframe, i, vec![layer; if keyframe { 4 * size } else { size }]);
        }
        while let Some(event) = a.try_next() {
            match event {
                Event::KeyframeRequest { layer, .. } => keyframe_due[layer as usize] = true,
                Event::VideoDemand { track: 0, layers } => {
                    for (layer, due) in keyframe_due.iter_mut().enumerate().skip(1) {
                        *due |= layers & !demand & 1 << layer != 0;
                    }
                    demand = layers;
                }
                _ => {}
            }
        }
        let t = start.elapsed().as_millis();
        while let Some(event) = b.try_next() {
            match event {
                Event::Video { layer, .. } => at_b.push((t, layer)),
                Event::VideoLimit { layer } => limits.push((t, layer)),
                _ => {}
            }
        }
        while let Some(event) = c.try_next() {
            if let Event::Video { layer, .. } = event {
                at_c.push((t, layer));
            }
        }
    }
    let layers_between = |got: &[(u128, u8)], from: u128, to: u128| -> Vec<u8> {
        got.iter().filter(|(t, _)| (from..to).contains(t)).map(|&(_, l)| l).collect()
    };
    eprintln!("b's link limits: {limits:?}");
    // c, on a fast link, gets 720p throughout.
    assert!(layers_between(&at_c, 1000, 22_000).iter().all(|&l| l == 2));
    // b is moved down within a few seconds, and gets 360p while its link is slow…
    let (down_at, down) = limits.first().copied().expect("b's link was limited");
    assert!(down_at < 4000 && down == Some(1), "{limits:?}");
    let slow_part = layers_between(&at_b, down_at + 1000, 6000);
    assert!(!slow_part.is_empty() && slow_part.iter().all(|&l| l == 1), "{slow_part:?}");
    // …and back to 720p once it keeps up again.
    assert!(limits.iter().any(|&(t, l)| t > 6000 && l == Some(3)), "{limits:?}");
    assert_eq!(at_b.last().map(|&(_, l)| l), Some(2), "720p again at the end");
}
