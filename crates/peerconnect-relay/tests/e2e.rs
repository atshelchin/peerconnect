//! End-to-end tests over real sockets: the Tokio client against a relay.
//!
//! By default each test starts an in-process relay. Set `PEERCONNECT_RELAY`
//! (e.g. `ws://127.0.0.1:8787`) to run the same tests against any relay,
//! such as the Cloudflare Worker under `wrangler dev` or the Docker image.
//! Tests that need to restart the relay only run in-process.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use peerconnect::client::{self, Connection};
use peerconnect::wire::{self, close};
use peerconnect::{CloseReason, Event, Room, State};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

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
        Relay::bind("127.0.0.1:0").await
    }

    async fn bind(addr: &str) -> Relay {
        let listener = TcpListener::bind(addr).await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            peerconnect_relay::serve(listener, Default::default(), async {
                let _ = stopped.await;
            })
            .await
        });
        Relay { url, stop: Some(stop), task: Some(task) }
    }

    fn external(&self) -> bool {
        self.task.is_none()
    }

    async fn shutdown(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.take().unwrap().await.unwrap();
    }
}

async fn wait(conn: &mut Connection, what: impl Fn(&Event) -> bool) -> Event {
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

async fn state(conn: &mut Connection, state: State) {
    wait(conn, |e| *e == Event::State(state)).await;
}

async fn message(conn: &mut Connection) -> Vec<u8> {
    match wait(conn, |e| matches!(e, Event::Message(_))).await {
        Event::Message(m) => m,
        _ => unreachable!(),
    }
}

async fn pair(relay: &Relay) -> (Connection, Connection, Room) {
    let inviter = Room::create(&relay.url).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap().to_string().parse().unwrap());
    let mut a = client::connect(inviter);
    state(&mut a, State::Waiting).await;
    let mut b = client::connect(joiner.clone());
    wait(&mut a, |e| *e == Event::Paired).await;
    state(&mut a, State::Connected).await;
    state(&mut b, State::Connected).await;
    (a, b, joiner)
}

#[tokio::test]
async fn pairs_and_exchanges_messages() {
    let relay = Relay::start().await;
    let (mut a, mut b, _) = pair(&relay).await;
    assert!(a.room().is_paired());

    a.send(b"hello").await.unwrap();
    assert_eq!(message(&mut b).await, b"hello");
    b.send(&b"world"[..]).await.unwrap();
    assert_eq!(message(&mut a).await, b"world");

    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    a.send(big.clone()).await.unwrap();
    assert_eq!(message(&mut b).await, big);

    // Many small messages, in order.
    let sender = b.sender();
    tokio::spawn(async move {
        for i in 0..2_000u32 {
            sender.send(i.to_be_bytes()).await.unwrap();
        }
    });
    for i in 0..2_000u32 {
        assert_eq!(message(&mut a).await, i.to_be_bytes());
    }
}

#[tokio::test]
async fn paired_room_reconnects_from_its_record() {
    let relay = Relay::start().await;
    let (a, mut b, _) = pair(&relay).await;
    let record = a.room().to_string(); // what the inviter persisted
    a.leave().await;
    state(&mut b, State::Waiting).await;

    let mut a = client::connect(record.parse().unwrap());
    state(&mut a, State::Connected).await;
    state(&mut b, State::Connected).await;
    b.send(b"again").await.unwrap();
    assert_eq!(message(&mut a).await, b"again");
}

#[tokio::test]
async fn survives_relay_restart() {
    let relay = Relay::start().await;
    if relay.external() {
        return;
    }
    let (mut a, mut b, _) = pair(&relay).await;
    let addr = relay.url.trim_start_matches("ws://").to_owned();
    relay.shutdown().await;
    state(&mut a, State::Offline).await;
    state(&mut b, State::Offline).await;

    let relay = Relay::bind(&addr).await; // a fresh relay with no memory of the room
    state(&mut a, State::Connected).await;
    state(&mut b, State::Connected).await;
    a.send(b"after restart").await.unwrap();
    assert_eq!(message(&mut b).await, b"after restart");
    drop(relay);
}

#[tokio::test]
async fn newer_connection_replaces_older() {
    let relay = Relay::start().await;
    let (mut a, mut b, joiner) = pair(&relay).await;
    let mut b2 = client::connect(joiner);
    wait(&mut b, |e| *e == Event::Replaced).await;
    state(&mut b2, State::Connected).await;
    state(&mut a, State::Connected).await;
    a.send(b"hi").await.unwrap();
    assert_eq!(message(&mut b2).await, b"hi");
}

#[tokio::test]
async fn close_ends_the_room_for_both() {
    let relay = Relay::start().await;
    let (a, mut b, _) = pair(&relay).await;
    a.close().await;
    state(&mut b, State::Closed(CloseReason::ClosedByPeer)).await;
    assert!(b.next().await.is_none());
}

// Raw protocol checks against the relay.

type RawWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn raw(relay: &Relay, room: &Room) -> (RawWs, [u8; 32]) {
    let url = format!("{}/v1/{}", relay.url, room.id());
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let hello = next_binary(&mut ws).await;
    assert_eq!(hello.len(), wire::HELLO_LEN);
    assert_eq!(hello[0], wire::HELLO);
    (ws, hello[1..].try_into().unwrap())
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

/// Signs a JOIN like a peer would, without going through `Peer`.
fn join_frame(room: &Room, nonce: &[u8; 32]) -> Vec<u8> {
    let mut sim = peerconnect::Peer::new(room.clone(), 0);
    sim.poll_output(); // Connect
    sim.handle_open(0);
    sim.handle_binary(0, &wire::hello(nonce));
    match sim.poll_output() {
        Some(peerconnect::Output::Send(frame)) => frame,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn relay_conformance() {
    let relay = Relay::start().await;
    let inviter = Room::create(&relay.url).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());

    // ping works before joining, and a valid JOIN gets PRESENCE.
    let (mut a, nonce) = raw(&relay, &inviter).await;
    a.send(Message::text("ping")).await.unwrap();
    assert_eq!(a.next().await.unwrap().unwrap(), Message::text("pong"));
    a.send(Message::binary(join_frame(&inviter, &nonce))).await.unwrap();
    assert_eq!(next_binary(&mut a).await, wire::presence(false));

    // Peer frames are forwarded verbatim to the other seat, nowhere else.
    let (mut b, nonce) = raw(&relay, &joiner).await;
    b.send(Message::binary(join_frame(&joiner, &nonce))).await.unwrap();
    assert_eq!(next_binary(&mut b).await, wire::presence(true));
    assert_eq!(next_binary(&mut a).await, wire::presence(true));
    for frame in [vec![wire::MESSAGE, 1, 2, 3], vec![0x7f], vec![0x00; wire::MAX_FRAME_LEN]] {
        b.send(Message::binary(frame.clone())).await.unwrap();
        assert_eq!(next_binary(&mut a).await, frame);
    }

    // A JOIN signed for another nonce is refused.
    let (mut c, _) = raw(&relay, &joiner).await;
    c.send(Message::binary(join_frame(&joiner, &[0; 32]))).await.unwrap();
    assert_eq!(close_code(&mut c).await, Some(close::UNAUTHORIZED));

    // Relay frames from a peer, oversized frames and stray text are refused.
    b.send(Message::binary(vec![wire::PRESENCE, 1])).await.unwrap();
    assert_eq!(close_code(&mut b).await, Some(close::PROTOCOL_ERROR));
    assert_eq!(next_binary(&mut a).await, wire::presence(false));
    let (mut b, nonce) = raw(&relay, &joiner).await;
    b.send(Message::binary(join_frame(&joiner, &nonce))).await.unwrap();
    next_binary(&mut b).await;
    next_binary(&mut a).await;
    b.send(Message::binary(vec![wire::MESSAGE; wire::MAX_FRAME_LEN + 1])).await.unwrap();
    assert_eq!(close_code(&mut b).await, Some(close::TOO_BIG));
    next_binary(&mut a).await;

    // A second connection to a seat replaces the first.
    let (mut a2, nonce) = raw(&relay, &inviter).await;
    a2.send(Message::binary(join_frame(&inviter, &nonce))).await.unwrap();
    assert_eq!(next_binary(&mut a2).await, wire::presence(false));
    assert_eq!(close_code(&mut a).await, Some(close::REPLACED));
    a2.send(Message::Close(Some(CloseFrame { code: 1000.into(), reason: "".into() }))).await.unwrap();
}

#[tokio::test]
async fn relay_closes_connections_that_never_join() {
    let relay = Relay::start().await;
    let room = Room::create(&relay.url).unwrap();
    let (mut ws, _) = raw(&relay, &room).await;
    let started = std::time::Instant::now();
    assert_eq!(close_code(&mut ws).await, Some(close::TIMEOUT));
    assert!(started.elapsed() >= wire::JOIN_TIMEOUT - Duration::from_millis(100));
}

#[tokio::test]
async fn http_routes() {
    let relay = Relay::start().await;
    if relay.url.starts_with("wss://") {
        return;
    }
    let base = relay.url.replacen("ws://", "http://", 1);
    let room = Room::create(&relay.url).unwrap();
    for (path, status) in [("/", 200), ("/nope", 404), ("/v1/notakey", 404), (&*format!("/v1/{}", room.id()), 426)] {
        let got = http_status(&format!("{base}{path}")).await;
        assert_eq!(got, status, "{path}");
    }
}

async fn http_status(url: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.trim_start_matches("http://");
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap();
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf)[9..12].parse().unwrap()
}

/// A room left idle long enough for the relay to drop it from memory (on
/// Cloudflare: hibernate the Durable Object) keeps working, and keepalives
/// alone keep both peers connected. Slow, so opt-in:
/// `cargo test -- --ignored idle_room`.
#[tokio::test]
#[ignore]
async fn idle_room_keeps_working() {
    let relay = Relay::start().await;
    let (mut a, mut b, _) = pair(&relay).await;
    let idle = std::env::var("PEERCONNECT_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(120);
    let quiet = timeout(Duration::from_secs(idle), async { tokio::join!(a.next(), b.next()) }).await;
    assert!(quiet.is_err(), "no events while idle, got {quiet:?}");
    a.send(b"still here").await.unwrap();
    assert_eq!(message(&mut b).await, b"still here");
    b.send(b"me too").await.unwrap();
    assert_eq!(message(&mut a).await, b"me too");
}

// Flow control: nothing may deadlock, and a peer that stops reading must not
// hold its partner hostage.

#[tokio::test]
async fn large_messages_both_ways_at_once() {
    let relay = Relay::start().await;
    let (mut a, mut b, _) = pair(&relay).await;
    let big = vec![7u8; wire::MAX_MESSAGE_LEN];
    let (sa, sb) = (a.sender(), b.sender());
    let (x, y) = (big.clone(), big.clone());
    let sends = tokio::spawn(async move { tokio::try_join!(sa.send(x), sb.send(y)) });
    let (ma, mb) = timeout(Duration::from_secs(60), async { tokio::join!(message(&mut a), message(&mut b)) })
        .await
        .expect("deadlocked");
    assert!(ma == big && mb == big);
    sends.await.unwrap().unwrap();
}

#[tokio::test]
async fn awaiting_send_inside_the_event_loop_cannot_deadlock() {
    let relay = Relay::start().await;
    let (a, mut b, _) = pair(&relay).await;
    // b echoes every message from inside its event loop.
    let echo = tokio::spawn(async move {
        while let Some(event) = b.next().await {
            if let Event::Message(m) = event {
                b.send(m).await.unwrap();
            }
        }
    });
    let sender = a.sender();
    let n = 3_000u32;
    tokio::spawn(async move {
        for i in 0..n {
            sender.send([i.to_be_bytes().to_vec(), vec![0; 1000]].concat()).await.unwrap();
        }
    });
    let mut a = a;
    let all = timeout(Duration::from_secs(60), async {
        for i in 0..n {
            assert_eq!(message(&mut a).await[..4], i.to_be_bytes());
        }
    });
    all.await.expect("deadlocked");
    echo.abort();
}

#[tokio::test]
async fn a_peer_that_stops_reading_is_dropped_not_waited_for() {
    let relay = Relay::start().await;
    if relay.external() {
        return; // exercises this relay's flow control
    }
    let inviter = Room::create(&relay.url).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let (mut a, nonce) = raw(&relay, &inviter).await;
    a.send(Message::binary(join_frame(&inviter, &nonce))).await.unwrap();
    assert_eq!(next_binary(&mut a).await, wire::presence(false));
    let (mut b, nonce) = raw(&relay, &joiner).await;
    b.send(Message::binary(join_frame(&joiner, &nonce))).await.unwrap();
    assert_eq!(next_binary(&mut a).await, wire::presence(true));
    // From here on b reads nothing, while a floods it.
    let started = std::time::Instant::now();
    let (mut sink, mut stream) = a.split();
    let flood = tokio::spawn(async move {
        let frame = vec![wire::MESSAGE; wire::MAX_FRAME_LEN];
        while sink.send(Message::binary(frame.clone())).await.is_ok() {}
    });
    // The relay gives up on b, tells a, and a stays connected throughout.
    let told = timeout(Duration::from_secs(60), async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Binary(f))) if f[..] == wire::presence(false) => return,
                Some(Ok(_)) => {}
                other => panic!("a lost its connection: {other:?}"),
            }
        }
    });
    told.await.expect("the relay kept waiting for b");
    let waited = started.elapsed();
    assert!(waited >= Duration::from_secs(25) && waited < Duration::from_secs(45), "{waited:?}");
    flood.abort();
    drop(b);
}

/// A large message to a slow reader takes longer than the receive timeout;
/// the sender must stay connected while its upload is held back. Slow, so
/// opt-in: `cargo test -- --ignored slow_reader`.
#[tokio::test]
#[ignore]
async fn slow_reader_still_gets_a_large_message() {
    let relay = Relay::start().await;
    let inviter = Room::create(&relay.url).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let mut a = client::connect(inviter);
    state(&mut a, State::Waiting).await;
    // b reads one frame every 300 ms: 16 MiB takes about 80 s.
    let (mut b, mut peer) = hand_driven_joiner(&relay, &joiner).await;
    state(&mut a, State::Connected).await;
    let big: Vec<u8> = (0..wire::MAX_MESSAGE_LEN).map(|i| i as u8).collect();
    a.send(big.clone()).await.unwrap();
    let received = 'read: loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let frame = next_binary(&mut b).await;
        peer.handle_binary(0, &frame);
        while let Some(out) = peer.poll_output() {
            if let peerconnect::Output::Event(Event::Message(m)) = out {
                break 'read m;
            }
        }
    };
    assert_eq!(received, big);
    // a never dropped its connection meanwhile.
    while let Ok(Some(event)) = timeout(Duration::from_millis(100), a.next()).await {
        assert_ne!(event, Event::State(State::Offline), "the sender lost its connection");
    }
}

#[tokio::test]
async fn a_busy_connection_that_is_replaced_is_told_why() {
    let relay = Relay::start().await;
    let inviter = Room::create(&relay.url).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let (mut a, nonce) = raw(&relay, &inviter).await;
    a.send(Message::binary(join_frame(&inviter, &nonce))).await.unwrap();
    next_binary(&mut a).await;
    let (mut b, nonce) = raw(&relay, &joiner).await;
    b.send(Message::binary(join_frame(&joiner, &nonce))).await.unwrap();
    next_binary(&mut b).await;
    next_binary(&mut a).await;

    // a floods b faster than b reads, so b's writer is stuck mid-write when
    // b gets replaced.
    let (mut sink, _stream) = a.split();
    let flood = tokio::spawn(async move {
        let frame = vec![wire::MESSAGE; 64 << 10];
        while sink.send(Message::binary(frame.clone())).await.is_ok() {}
    });
    let replaced = tokio::spawn(async move {
        loop {
            match b.next().await {
                Some(Ok(Message::Close(frame))) => return frame.map(|f| u16::from(f.code)),
                Some(Ok(_)) => tokio::time::sleep(Duration::from_millis(2)).await,
                _ => return None,
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (mut b2, nonce) = raw(&relay, &joiner).await;
    b2.send(Message::binary(join_frame(&joiner, &nonce))).await.unwrap();
    assert_eq!(replaced.await.unwrap(), Some(close::REPLACED));
    flood.abort();
}

/// A joiner on a raw socket, driven by hand up to an established session,
/// so a test can decide when (and whether) it reads.
async fn hand_driven_joiner(relay: &Relay, joiner: &Room) -> (RawWs, peerconnect::Peer) {
    let (mut ws, nonce) = raw(relay, joiner).await;
    let mut peer = peerconnect::Peer::new(joiner.clone(), 0);
    peer.poll_output(); // Connect
    peer.handle_open(0);
    peer.handle_binary(0, &wire::hello(&nonce));
    loop {
        while let Some(out) = peer.poll_output() {
            if let peerconnect::Output::Send(frame) = out {
                ws.send(Message::binary(frame)).await.unwrap();
            }
        }
        if peer.state() == State::Connected {
            return (ws, peer);
        }
        let frame = next_binary(&mut ws).await;
        peer.handle_binary(0, &frame);
    }
}

#[tokio::test]
async fn leaving_does_not_wait_for_the_send_backlog() {
    let relay = Relay::start().await;
    let inviter = Room::create(&relay.url).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let mut a = client::connect(inviter);
    state(&mut a, State::Waiting).await;
    let (b, _peer) = hand_driven_joiner(&relay, &joiner).await; // and then never reads
    state(&mut a, State::Connected).await;
    a.send(vec![1u8; wire::MAX_MESSAGE_LEN]).await.unwrap();
    let started = std::time::Instant::now();
    timeout(Duration::from_secs(5), a.leave()).await.expect("leave waited for the backlog");
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(b);
}

#[tokio::test]
async fn close_right_after_a_large_message_still_ends_the_room() {
    let relay = Relay::start().await;
    let (a, mut b, _) = pair(&relay).await;
    let big = vec![2u8; wire::MAX_MESSAGE_LEN];
    a.send(big.clone()).await.unwrap();
    a.close().await;
    assert_eq!(message(&mut b).await, big);
    state(&mut b, State::Closed(CloseReason::ClosedByPeer)).await;
}
