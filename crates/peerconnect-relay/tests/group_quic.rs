//! Groups over QUIC end to end (GROUP.md §3.6): raw QUIC and WebTransport
//! against the in-process relay, in one group; lost packets, a change of
//! address, UDP that does not get through, and certificates.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use common::{Got, LocalRelay as Relay, Path, Wire, admitted, client_config, events_for, join, join_frame, wait};
use futures_util::StreamExt;
use peerconnect::client::group::GroupConnection;
use peerconnect::group::transport;
use peerconnect::group::{Channel, Class, Config, Event, Group, Identity, Member, MemberId, Output, State, wire};
use peerconnect::wire::close;
use rustls::pki_types::CertificateDer;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

/// A certificate authority and a certificate for `localhost` it signed,
/// written to files for the relay.
struct TestCertificate {
    ca: CertificateDer<'static>,
    files: peerconnect_relay::Tls,
    dir: std::path::PathBuf,
}

impl TestCertificate {
    fn new() -> TestCertificate {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .unwrap()
            .signed_by(&key, &issuer)
            .unwrap();
        let dir = std::env::temp_dir().join(format!("peerconnect-quic-{}-{}", std::process::id(), rand_u32()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = peerconnect_relay::Tls { cert: dir.join("relay.crt"), key: dir.join("relay.key") };
        std::fs::write(&files.cert, leaf.pem()).unwrap();
        std::fs::write(&files.key, key.serialize_pem()).unwrap();
        TestCertificate { ca: ca.der().clone(), files, dir }
    }

    fn roots(&self) -> rustls::RootCertStore {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.ca.clone()).unwrap();
        roots
    }
}

impl Drop for TestCertificate {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Distinct per certificate, for directory names.
fn rand_u32() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn path(group: &Group) -> String {
    format!("/g1/{}", group.id())
}

// Members over WebTransport.

/// A member over WebTransport, as a browser would be: a [`Member`] driven
/// by hand over a [`Wire`].
struct WebTransportMember {
    id: MemberId,
    actions: mpsc::UnboundedSender<(Channel, Vec<u8>)>,
    events: mpsc::UnboundedReceiver<Event>,
}

impl WebTransportMember {
    fn join(group: &Group, roots: Option<rustls::RootCertStore>) -> WebTransportMember {
        let mut member = Member::new(group.clone(), Identity::generate(), Config::default(), 0).unwrap();
        let id = member.id();
        let (actions, mut todo) = mpsc::unbounded_channel::<(Channel, Vec<u8>)>();
        let (tx, events) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let start = Instant::now();
            let now = || start.elapsed().as_millis() as u64;
            let mut wire: Option<Wire> = None;
            loop {
                while let Some(out) = member.poll_output() {
                    match out {
                        Output::Connect(url) => {
                            let target = transport::target(&url).expect("a QUIC URL");
                            match Wire::webtransport(&target.url(), roots.clone()).await {
                                Ok(w) => {
                                    wire = Some(w);
                                    member.handle_open(now());
                                }
                                Err(_) => member.handle_close(now(), None),
                            }
                        }
                        Output::Send(frame, class) => {
                            if let Some(w) = &mut wire {
                                if class.is_realtime() && w.session.send_datagram(frame.clone().into()).is_ok() {
                                    continue;
                                }
                                w.send(class, &frame).await;
                            }
                        }
                        Output::Ping => {
                            if let Some(w) = &mut wire {
                                w.send(Class::Command, &[]).await;
                            }
                        }
                        Output::Disconnect => wire = None,
                        Output::Event(e) => {
                            if tx.send(e).is_err() {
                                return;
                            }
                        }
                    }
                }
                let deadline =
                    member.poll_timeout().map(|t| tokio::time::Instant::from_std(start) + Duration::from_millis(t));
                tokio::select! {
                    () = async { tokio::time::sleep_until(deadline.unwrap()).await }, if deadline.is_some() => member.handle_timeout(now()),
                    got = async { wire.as_mut().unwrap().recv().await }, if wire.is_some() => match got {
                        Some(Got::Frame(_, f) | Got::Datagram(f)) => member.handle_binary(now(), &f),
                        Some(Got::Pong) => member.handle_text(now(), wire::PONG),
                        Some(Got::Closed(code)) => { wire = None; member.handle_close(now(), code) }
                        None => { wire = None; member.handle_close(now(), None) }
                    },
                    action = todo.recv() => match action {
                        Some((channel, data)) => { member.send(channel, &data).unwrap(); }
                        None => return,
                    },
                }
            }
        });
        WebTransportMember { id, actions, events }
    }

    fn send(&self, channel: Channel, data: &str) {
        self.actions.send((channel, data.as_bytes().to_vec())).unwrap();
    }

    async fn wait(&mut self, what: impl Fn(&Event) -> bool) -> Event {
        timeout(Duration::from_secs(20), async {
            loop {
                let event = self.events.recv().await.expect("member stopped");
                if what(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("timed out")
    }
}

// The tests.

#[tokio::test]
async fn raw_quic_and_webtransport_follow_the_relay_protocol() {
    let relay = Relay::start(peerconnect_relay::Config { max_group_members: 3, ..Default::default() }, true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let (ia, ib, ic) = (Identity::generate(), Identity::generate(), Identity::generate());

    // Raw QUIC: the path first, then HELLO and so on, on streams.
    let mut a = Wire::raw(&group.endpoint()).await;
    assert_eq!(a.join(&group, &ia).await, (0, 0));
    a.send(Class::Command, &[]).await;
    assert_eq!(a.next().await, Got::Pong, "the empty message is ping, and pong");

    // WebTransport: the path in the CONNECT request.
    let mut b = Wire::webtransport(&format!("https://{}{}", relay.addr, path(&group)), None).await.unwrap();
    assert_eq!(b.join(&group, &ib).await, (1, 1));
    assert_eq!(a.frame().await, wire::member(1, true));

    // And another raw QUIC member.
    let mut c = Wire::raw(&group.endpoint()).await;
    assert_eq!(c.join(&group, &ic).await, (2, 0b11));
    assert_eq!(a.frame().await, wire::member(2, true));
    assert_eq!(b.frame().await, wire::member(2, true));

    // Each class on its stream; voice as datagrams both ways, and on its
    // stream when sent there; frames arrive verbatim whatever the transport.
    let command = vec![wire::COMMAND, 0, wire::BROADCAST, 1, 2, 3];
    a.send(Class::Command, &command).await;
    assert_eq!(b.next().await, Got::Frame(Class::Command, command.clone()));
    assert_eq!(c.next().await, Got::Frame(Class::Command, command));
    let voice = vec![wire::VOICE, 1, wire::BROADCAST, 9, 9, 9];
    b.datagram(&voice);
    assert_eq!(a.next().await, Got::Datagram(voice.clone()));
    assert_eq!(c.next().await, Got::Datagram(voice));
    let big_voice = [&[wire::VOICE, 2, 0][..], &vec![5; 4000]].concat();
    c.send(Class::Voice, &big_voice).await;
    assert_eq!(a.next().await, Got::Frame(Class::Voice, big_voice), "too big for a datagram");
    let chat = vec![wire::CHAT, 2, 1, 4];
    c.send(Class::Chat, &chat).await;
    assert_eq!(b.next().await, Got::Frame(Class::Chat, chat));
    let bulk = [&[wire::BULK, 1, 0][..], &vec![7; wire::MAX_FRAME_LEN - 3]].concat();
    b.send(Class::Bulk, &bulk).await;
    assert_eq!(a.next().await, Got::Frame(Class::Bulk, bulk));

    // Full.
    let mut d = Wire::raw(&group.endpoint()).await;
    let hello = d.frame().await;
    d.send(Class::Command, &join_frame(&group, &Identity::generate(), &hello)).await;
    assert_eq!(d.closed().await, Some(close::FULL));

    // The same identity again takes its slot, over the other transport.
    let mut a2 = Wire::webtransport(&format!("https://{}{}", relay.addr, path(&group)), None).await.unwrap();
    assert_eq!(a2.join(&group, &ia).await.0, 0);
    assert_eq!(a.closed().await, Some(close::REPLACED));
    assert_eq!(b.frame().await, wire::member(0, true));

    // Lying about the slot, and messages too long, close with the WebSocket codes.
    b.send(Class::Chat, &[wire::CHAT, 0, wire::BROADCAST]).await;
    assert_eq!(b.closed().await, Some(close::PROTOCOL_ERROR));
    assert_eq!(a2.frame().await, wire::member(1, false));
    a2.write(Class::Chat, &((wire::MAX_FRAME_LEN + 1) as u16).to_be_bytes()).await;
    assert_eq!(a2.closed().await, Some(close::TOO_BIG));
}

#[tokio::test]
async fn the_relay_refuses_what_is_not_a_group() {
    let relay = Relay::start(Default::default(), true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    // Raw QUIC: a path that names no group.
    let mut a = Wire::raw(&format!("{}/v1/not-a-group", relay.url("quic"))).await;
    assert_eq!(a.closed().await, Some(close::NOT_FOUND));
    // A stream of no class.
    let mut b = Wire::raw(&group.endpoint()).await;
    assert_eq!(b.frame().await[0], wire::HELLO);
    let mut stray = b.session.open_uni().await.unwrap();
    stray.write_all(&[9, 0, 0]).await.unwrap();
    assert_eq!(b.closed().await, Some(close::PROTOCOL_ERROR));
    // The first stream must be the command stream.
    let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let conn = endpoint.connect_with(client_config(transport::ALPN, None), relay.addr, "x").unwrap().await.unwrap();
    let mut chat = conn.open_uni().await.unwrap();
    chat.write_all(&[transport::stream_header(Class::Chat), 0, 1, b'/']).await.unwrap();
    let err = timeout(Duration::from_secs(10), conn.closed()).await.unwrap();
    assert!(
        matches!(err, quinn::ConnectionError::ApplicationClosed(c) if c.error_code.into_inner() == close::PROTOCOL_ERROR as u64)
    );
    // WebTransport to a path that is not a group is refused at CONNECT.
    let refused = Wire::webtransport(&format!("https://{}/v1/x", relay.addr), None).await;
    assert!(refused.is_err());
    // A connection that never asks for anything is dropped after the join timeout.
    let started = Instant::now();
    let idle = endpoint.connect_with(client_config(transport::ALPN, None), relay.addr, "x").unwrap().await.unwrap();
    timeout(Duration::from_secs(15), idle.closed()).await.unwrap();
    assert!(started.elapsed() >= wire::JOIN_TIMEOUT - Duration::from_millis(500));
}

async fn next_ws(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> Vec<u8> {
    loop {
        match timeout(Duration::from_secs(10), ws.next()).await.unwrap() {
            Some(Ok(Message::Binary(b))) => return b.to_vec(),
            Some(Ok(_)) => {}
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn members_on_raw_quic_and_webtransport_share_a_group() {
    let relay = Relay::start(Default::default(), true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    let mut c = WebTransportMember::join(&group.with_relay(&relay.url("https")).unwrap(), None);
    admitted(&mut a, 2).await;
    admitted(&mut b, 2).await;
    for _ in 0..2 {
        c.wait(|e| matches!(e, Event::Joined { .. })).await;
    }

    for i in 0..50 {
        a.send(Channel::Command, format!("a{i}")).await.unwrap();
        b.send(Channel::Chat, format!("b{i}")).await.unwrap();
        c.send(Channel::Command, &format!("c{i}"));
    }
    let collect = |from: MemberId, channel: Channel| move |e: &Event| matches!(e, Event::Message { from: f, channel: ch, direct: false, .. } if *f == from && *ch == channel);
    let text = |e: Event| match e {
        Event::Message { data, .. } => String::from_utf8(data).unwrap(),
        _ => unreachable!(),
    };
    for i in 0..50 {
        assert_eq!(text(wait(&mut b, collect(a.id(), Channel::Command)).await), format!("a{i}"));
        assert_eq!(text(c.wait(collect(b.id(), Channel::Chat)).await), format!("b{i}"));
        assert_eq!(text(wait(&mut a, collect(c.id, Channel::Command)).await), format!("c{i}"));
    }
    // Voice: small frames as datagrams, a big one on the voice stream.
    a.send_voice(7, vec![1; 80]);
    a.send_voice(8, vec![2; 5000]);
    let mut seqs = Vec::new();
    while seqs.len() < 2 {
        if let Event::Voice { seq, data, .. } = c.wait(|e| matches!(e, Event::Voice { .. })).await {
            assert_eq!(data.len(), if seq == 7 { 80 } else { 5000 });
            seqs.push(seq);
        }
    }
    seqs.sort();
    assert_eq!(seqs, [7, 8]);

    // A newcomer syncs from another member.
    let mut d = join(&group, "newcomer");
    admitted(&mut d, 3).await;
    d.request_sync(b.id()).await.unwrap();
    let Event::SyncRequest { from, transfer } = wait(&mut b, |e| matches!(e, Event::SyncRequest { .. })).await else {
        unreachable!()
    };
    let world: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    b.provide_sync(from, transfer, world.clone());
    let Event::Snapshot { data, .. } = wait(&mut d, |e| matches!(e, Event::Snapshot { .. })).await else {
        unreachable!()
    };
    assert!(data == world);
}

#[tokio::test]
async fn voice_and_commands_keep_flowing_when_packets_are_lost() {
    let relay = Relay::start(Default::default(), true).await;
    // Each member reaches the relay over a path that loses 5% of packets
    // each way and takes 20 ms each way: 40 ms between them, without loss.
    let one_way = Duration::from_millis(20);
    let (pa, pb) = (Path::new(relay.addr, one_way).await, Path::new(relay.addr, one_way).await);
    let group = Group::create(&pa.url("quic")).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group.with_relay(&pb.url("quic")).unwrap(), "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    pa.loss_per_mille.store(50, Ordering::Relaxed);
    pb.loss_per_mille.store(50, Ordering::Relaxed);

    // 5 s of voice (20 ms frames) and a command every 100 ms, as a game would.
    let start = Instant::now();
    let sender = a.sender();
    let talk = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        for seq in 0..250u16 {
            tick.tick().await;
            let at = start.elapsed().as_micros() as u64;
            sender.send_voice(seq, [&at.to_be_bytes()[..], &[0; 72]].concat());
            if seq % 5 == 0 {
                sender
                    .send(Channel::Command, [&at.to_be_bytes()[..], &(seq / 5).to_be_bytes()].concat())
                    .await
                    .unwrap();
            }
        }
    });
    let (mut voice, mut commands) = (Vec::new(), Vec::new());
    let mut seqs = Vec::new();
    while commands.len() < 50 {
        let e = wait(&mut b, |e| matches!(e, Event::Voice { .. } | Event::Message { .. })).await;
        let latency =
            |data: &[u8]| start.elapsed().as_micros() as u64 - u64::from_be_bytes(data[..8].try_into().unwrap());
        match e {
            Event::Voice { seq, data, .. } => {
                voice.push(latency(&data));
                seqs.push(seq);
            }
            Event::Message { data, .. } => {
                commands.push((latency(&data), u16::from_be_bytes(data[8..10].try_into().unwrap())));
            }
            _ => unreachable!(),
        }
    }
    talk.await.unwrap();
    // Reliable and in order, despite the loss.
    assert_eq!(commands.iter().map(|c| c.1).collect::<Vec<_>>(), (0..50).collect::<Vec<_>>());
    let pct = |v: &mut Vec<u64>, p: usize| {
        v.sort();
        v[(v.len() - 1) * p / 100] / 1000
    };
    let mut command_latency: Vec<u64> = commands.iter().map(|c| c.0).collect();
    let received = voice.len();
    eprintln!(
        "5% loss each way, 40 ms apart: voice {received}/250 frames, p50 {} ms, p99 {} ms; commands p50 {} ms, p99 {} ms, max {} ms",
        pct(&mut voice, 50),
        pct(&mut voice, 99),
        pct(&mut command_latency, 50),
        pct(&mut command_latency, 99),
        pct(&mut command_latency, 100),
    );
    // About 10% of frames are lost on two lossy hops; none waits for a
    // retransmission, so no frame is much later than the path.
    assert!(received >= 200, "{received} frames");
    assert!(pct(&mut voice, 99) < 120, "voice p99 {} ms", pct(&mut voice, 99));
    // Frames that arrive out of order are still played.
    assert!(seqs.windows(2).all(|w| w[0] != w[1]));
}

#[tokio::test]
async fn a_quic_member_rides_out_a_change_of_address() {
    let relay = Relay::start(Default::default(), true).await;
    let path = Path::new(relay.addr, Duration::ZERO).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group.with_relay(&path.url("quic")).unwrap(), "moving");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    for round in 0..3 {
        path.rebind.send(()).unwrap();
        for i in 0..5 {
            b.send(Channel::Command, format!("{round}.{i}")).await.unwrap();
        }
        for i in 0..5 {
            let e = wait(&mut a, |e| matches!(e, Event::Message { .. } | Event::Left { .. })).await;
            assert_eq!(
                e,
                Event::Message {
                    from: b.id(),
                    channel: Channel::Command,
                    data: format!("{round}.{i}").into(),
                    direct: false
                }
            );
        }
    }
    // Never offline: the same QUIC connection throughout.
    let mut seen = Vec::new();
    while let Ok(Some(e)) = timeout(Duration::from_millis(100), b.next()).await {
        seen.push(e);
    }
    assert!(!seen.iter().any(|e| matches!(e, Event::State(_) | Event::Left { .. })), "{seen:?}");
}

#[tokio::test]
async fn a_member_whose_udp_does_not_get_through_stays_offline() {
    let joined = |e: &Event| matches!(e, Event::Joined { .. } | Event::State(State::Online));
    // A relay without QUIC: groups have no other way in.
    let relay = Relay::start(Default::default(), false).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let mut a = join(&group, "a");
    let b = join(&group, "b");
    let seen = events_for(&mut a, Duration::from_secs(3)).await;
    assert!(!seen.iter().any(joined), "{seen:?}");
    assert!(!a.connected() && !b.connected());

    // UDP that vanishes without a trace: offline for as long as it does,
    // and in once it gets through, by trying again on its own.
    let relay = Relay::start(Default::default(), true).await;
    let path = Path::new(relay.addr, Duration::ZERO).await;
    path.blocked.store(true, Ordering::Relaxed);
    let group = Group::create(&relay.url("quic")).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group.with_relay(&path.url("quic")).unwrap(), "b");
    let seen = events_for(&mut b, Duration::from_secs(4)).await;
    assert!(!seen.iter().any(joined), "{seen:?}");
    assert!(!b.connected());
    assert!(a.connected(), "a has the relay to itself");
    let seen = events_for(&mut a, Duration::from_millis(100)).await;
    assert!(!seen.iter().any(|e| matches!(e, Event::Joined { .. })), "{seen:?}");
    path.blocked.store(false, Ordering::Relaxed);
    let started = Instant::now();
    admitted(&mut b, 1).await;
    admitted(&mut a, 1).await;
    eprintln!("joined {:?} after UDP got through", started.elapsed());
}

#[tokio::test]
async fn a_certificate_serves_webtransport_and_rooms_over_wss() {
    let cert = TestCertificate::new();
    let config = peerconnect_relay::Config { tls: Some(cert.files.clone()), ..Default::default() };
    let relay = Relay::start(config, true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let port = relay.addr.port();

    // WebTransport, verified as a browser would, for both names.
    for host in ["localhost", "127.0.0.1"] {
        let mut wt =
            Wire::webtransport(&format!("https://{host}:{port}{}", path(&group)), Some(cert.roots())).await.unwrap();
        assert_eq!(wt.frame().await[0], wire::HELLO);
    }
    // And a member over it, among raw QUIC members.
    let mut a = join(&group, "a");
    let mut c =
        WebTransportMember::join(&group.with_relay(&format!("https://localhost:{port}")).unwrap(), Some(cert.roots()));
    admitted(&mut a, 1).await;
    c.wait(|e| matches!(e, Event::Joined { .. })).await;
    c.send(Channel::Chat, "secure");
    let e = wait(&mut a, |e| matches!(e, Event::Message { .. })).await;
    assert_eq!(e, Event::Message { from: c.id, channel: Channel::Chat, data: b"secure".to_vec(), direct: false });
    // A certificate it does not trust is refused.
    let other = TestCertificate::new();
    assert!(
        Wire::webtransport(&format!("https://localhost:{port}{}", path(&group)), Some(other.roots())).await.is_err()
    );

    // On TCP, the same certificate serves two-peer rooms over wss://, next
    // to ws:// on the same port; groups are not served there at all.
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(cert.roots())
        .with_no_client_auth();
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls));
    let wss = |path: String| {
        let connector = connector.clone();
        async move {
            let url = format!("wss://localhost:{port}{path}");
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector)).await
        }
    };
    let room = peerconnect::Room::create(&format!("wss://localhost:{port}")).unwrap();
    let (mut secure, _) = wss(format!("/v1/{}", room.id())).await.unwrap();
    assert_eq!(next_ws(&mut secure).await[0], peerconnect::wire::HELLO);
    let (mut plain, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/{}", room.id())).await.unwrap();
    assert_eq!(next_ws(&mut plain).await[0], peerconnect::wire::HELLO);
    match wss(path(&group)).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => assert_eq!(response.status(), 404),
        other => panic!("{:?}", other.map(|(_, response)| response)),
    }
}

#[tokio::test]
async fn leaving_over_quic_delivers_what_was_sent_first() {
    let relay = Relay::start(Default::default(), true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    let filler = "x".repeat(4000);
    for i in 0..50 {
        a.send(Channel::Chat, format!("{i} {filler}")).await.unwrap();
    }
    let ia = a.id();
    a.leave().await; // returns once the connection has stopped
    for i in 0..50 {
        let e = wait(&mut b, |e| matches!(e, Event::Message { .. } | Event::Left { .. })).await;
        let Event::Message { data, .. } = e else { panic!("left before message {i}") };
        assert!(data.starts_with(format!("{i} ").as_bytes()));
    }
    assert_eq!(wait(&mut b, |e| matches!(e, Event::Left { .. })).await, Event::Left { member: ia });
}

#[tokio::test]
async fn an_https_relay_whose_certificate_is_not_trusted_is_refused() {
    // A Rust member verifies an https:// relay: with a certificate from an
    // authority it does not know, it never joins.
    let cert = TestCertificate::new();
    let config = peerconnect_relay::Config { tls: Some(cert.files.clone()), ..Default::default() };
    let relay = Relay::start(config, true).await;
    let group = Group::create(&format!("https://localhost:{}", relay.addr.port())).unwrap();
    let mut a = join(&group, "a");
    let seen = events_for(&mut a, Duration::from_secs(5)).await;
    assert!(!seen.contains(&Event::State(State::Online)), "joined a relay it cannot trust: {seen:?}");
    assert!(!a.connected());
}

/// Video at about 1 Mbit/s for 4 s (15 frames a second, a 20 KB keyframe
/// every second, 7 KB otherwise) from a to b; returns the frames b got
/// (timestamp, bytes, latency) and the keyframe requests a heard.
async fn stream_video(a: &mut GroupConnection, b: &mut GroupConnection) -> (Vec<(u32, usize, u64)>, usize) {
    let start = Instant::now();
    let sender = a.sender();
    let send = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(66));
        for i in 0..60u32 {
            tick.tick().await;
            let keyframe = i % 15 == 0;
            let at = start.elapsed().as_millis() as u32;
            let size = if keyframe { 20_000 } else { 7_000 };
            let data: Vec<u8> = (0..size).map(|j| (i as usize + j) as u8).collect();
            sender.send_video(0, 0, keyframe, at, data);
        }
    });
    let mut frames = Vec::new();
    let mut requests = 0;
    let deadline = start + Duration::from_secs(6);
    while Instant::now() < deadline {
        tokio::select! {
            e = b.next() => match e {
                Some(Event::Video { timestamp, data, .. }) => {
                    frames.push((timestamp, data.len(), start.elapsed().as_millis() as u64 - timestamp as u64));
                }
                Some(_) => {}
                None => break,
            },
            e = a.next() => if let Some(Event::KeyframeRequest { .. }) = e { requests += 1 },
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
    send.await.unwrap();
    (frames, requests)
}

#[tokio::test]
async fn video_flows_over_quic_and_recovers_from_loss() {
    for (name, loss) in [("QUIC", 0), ("QUIC, 3% loss each way", 30)] {
        let relay = Relay::start(Default::default(), true).await;
        let one_way = Duration::from_millis(15);
        let (pa, pb) = (Path::new(relay.addr, one_way).await, Path::new(relay.addr, one_way).await);
        let group = Group::create(&pa.url("quic")).unwrap();
        let mut a = join(&group, "a");
        let mut b = join(&group.with_relay(&pb.url("quic")).unwrap(), "b");
        admitted(&mut a, 1).await;
        admitted(&mut b, 1).await;
        pa.loss_per_mille.store(loss, Ordering::Relaxed);
        pb.loss_per_mille.store(loss, Ordering::Relaxed);
        let (frames, requests) = stream_video(&mut a, &mut b).await;
        let mut latency: Vec<u64> = frames.iter().map(|f| f.2).collect();
        latency.sort();
        let p = |q: usize| latency.get((latency.len().max(1) - 1) * q / 100).copied().unwrap_or(0);
        eprintln!(
            "video over {name}: {}/60 frames, latency p50 {} ms, p99 {} ms; {requests} keyframe requests",
            frames.len(),
            p(50),
            p(99)
        );
        // Whole frames only, in order.
        assert!(frames.iter().all(|&(t, len, _)| len == if t == 0 || len == 20_000 { 20_000 } else { 7_000 }));
        assert!(frames.windows(2).all(|w| w[0].0 < w[1].0), "in order");
        if loss == 0 {
            assert_eq!(frames.len(), 60, "{name}: everything");
            assert!(p(99) < 150, "{name}: p99 {} ms", p(99));
        } else {
            // Lost fragments are resent; frames lost for good cost a few
            // more, until the next keyframe.
            assert!(frames.len() >= 45, "{name}: {} frames", frames.len());
        }
    }
}
