//! Load generator for a PeerConnect relay.
//!
//! ```text
//! bench <relay-url> latency    [rooms] [round-trips per room]   relay only
//! bench <relay-url> throughput [rooms] [frame bytes] [seconds]  relay only
//! bench <relay-url> joins      [connections] [concurrency]      relay only
//! bench <relay-url> idle       [rooms]                          relay only, holds until Enter
//! bench <relay-url> e2e        [rooms] [round-trips per room]   full client, end-to-end encrypted
//! bench <relay-url> bulk       [message bytes] [seconds]        full client, one room
//! ```
//!
//! "Relay only" scenarios use raw joined sockets that exchange peer frames,
//! so they measure the relay itself; the others go through `peerconnect::client`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use peerconnect::client::{self, Connection};
use peerconnect::wire;
use peerconnect::{Event, Output, Peer, Room, State};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize, default: u64| args.get(i).and_then(|a| a.parse().ok()).unwrap_or(default);
    let relay = args.first().expect("relay URL").clone();
    match args.get(1).map(String::as_str) {
        Some("latency") => latency(&relay, arg(2, 100) as usize, arg(3, 1000) as usize).await,
        Some("throughput") => throughput(&relay, arg(2, 100) as usize, arg(3, 1024) as usize, arg(4, 10)).await,
        Some("joins") => joins(&relay, arg(2, 10_000) as usize, arg(3, 64) as usize).await,
        Some("idle") => idle(&relay, arg(2, 1000) as usize).await,
        Some("e2e") => e2e(&relay, arg(2, 100) as usize, arg(3, 1000) as usize).await,
        Some("bulk") => bulk(&relay, arg(2, 1 << 20) as usize, arg(3, 10)).await,
        _ => eprintln!("unknown scenario; see the source for usage"),
    }
}

/// Opens a socket to the room's seat and joins it.
async fn join(room: &Room) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(room.endpoint()).await.expect("connect");
    let hello = match ws.next().await {
        Some(Ok(Message::Binary(b))) => b,
        other => panic!("expected HELLO, got {other:?}"),
    };
    // Let a Peer produce the JOIN, exactly as a real peer would.
    let mut peer = Peer::new(room.clone(), 0);
    peer.poll_output();
    peer.handle_open(0);
    peer.handle_binary(0, &hello);
    let Some(Output::Send(join)) = peer.poll_output() else { panic!("no JOIN") };
    ws.send(Message::binary(join)).await.unwrap();
    ws
}

/// Two joined sockets in a fresh room, after both have seen their PRESENCE.
async fn raw_pair(relay: &str) -> (Ws, Ws) {
    let inviter = Room::create(relay).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let mut a = join(&inviter).await;
    expect_frame(&mut a).await; // PRESENCE(0)
    let mut b = join(&joiner).await;
    expect_frame(&mut b).await; // PRESENCE(1)
    expect_frame(&mut a).await; // PRESENCE(1)
    (a, b)
}

async fn expect_frame(ws: &mut Ws) -> bytes::Bytes {
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(b))) => return b,
            Some(Ok(_)) => continue,
            other => panic!("socket ended: {other:?}"),
        }
    }
}

async fn latency(relay: &str, rooms: usize, trips: usize) {
    let mut pairs = Vec::new();
    for _ in 0..rooms {
        pairs.push(raw_pair(relay).await);
    }
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for (mut a, mut b) in pairs {
        tasks.spawn(async move {
            let echo = tokio::spawn(async move {
                for _ in 0..trips {
                    let frame = expect_frame(&mut b).await;
                    b.send(Message::binary(frame)).await.unwrap();
                }
            });
            let mut samples = Vec::with_capacity(trips);
            let payload = vec![wire::MESSAGE; 64];
            for _ in 0..trips {
                let sent = Instant::now();
                a.send(Message::binary(payload.clone())).await.unwrap();
                expect_frame(&mut a).await;
                samples.push(sent.elapsed());
            }
            echo.await.unwrap();
            samples
        });
    }
    let samples: Vec<Duration> = tasks.join_all().await.into_iter().flatten().collect();
    report_latency(&format!("relay round trip, {rooms} rooms × {trips}, 64 B frames"), samples, started.elapsed());
}

async fn throughput(relay: &str, rooms: usize, size: usize, seconds: u64) {
    let mut pairs = Vec::new();
    for _ in 0..rooms {
        pairs.push(raw_pair(relay).await);
    }
    let received = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut tasks = JoinSet::new();
    for (mut a, mut b) in pairs {
        let received = received.clone();
        tasks.spawn(async move {
            let mut frame = vec![0u8; size];
            frame[0] = wire::MESSAGE;
            let reader = tokio::spawn(async move {
                while let Some(Ok(msg)) = b.next().await {
                    if let Message::Binary(_) = msg {
                        received.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
            'send: while Instant::now() < deadline {
                for _ in 0..32 {
                    if a.feed(Message::binary(frame.clone())).await.is_err() {
                        break 'send; // e.g. the OS ran out of socket buffers
                    }
                }
                if a.flush().await.is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            reader.abort();
        });
    }
    let started = Instant::now();
    let at_start = received.load(Ordering::Relaxed);
    tasks.join_all().await;
    let frames = received.load(Ordering::Relaxed) - at_start;
    let secs = started.elapsed().as_secs_f64();
    println!(
        "relay throughput, {rooms} rooms, {size} B frames: {:.0} frames/s, {:.1} MB/s forwarded",
        frames as f64 / secs,
        frames as f64 * size as f64 / secs / 1e6
    );
}

async fn joins(relay: &str, count: usize, concurrency: usize) {
    let rooms: Vec<Room> = (0..concurrency).map(|_| Room::create(relay).unwrap()).collect();
    let next = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for room in rooms {
        let next = next.clone();
        tasks.spawn(async move {
            let mut samples = Vec::new();
            while (next.fetch_add(1, Ordering::Relaxed) as usize) < count {
                let t = Instant::now();
                let mut ws = join(&room).await;
                expect_frame(&mut ws).await; // PRESENCE
                samples.push(t.elapsed());
                let _ = ws.close(None).await;
            }
            samples
        });
    }
    let samples: Vec<Duration> = tasks.join_all().await.into_iter().flatten().collect();
    let elapsed = started.elapsed();
    println!(
        "joins: {:.0}/s ({} in {:.2?}, {concurrency} concurrent)",
        samples.len() as f64 / elapsed.as_secs_f64(),
        samples.len(),
        elapsed
    );
    report_latency("connect + HELLO + JOIN + PRESENCE", samples, elapsed);
}

async fn idle(relay: &str, rooms: usize) {
    let mut held = Vec::new();
    for i in 0..rooms {
        held.push(raw_pair(relay).await);
        if (i + 1) % 1000 == 0 {
            eprintln!("{} rooms open", i + 1);
        }
    }
    println!("{rooms} rooms, {} connections open; press Enter to close", rooms * 2);
    let _ = tokio::task::spawn_blocking(|| std::io::stdin().read_line(&mut String::new())).await;
}

async fn connected_pair(relay: &str) -> (Connection, Connection) {
    let inviter = Room::create(relay).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap());
    let mut a = client::connect(inviter);
    let mut b = client::connect(joiner);
    until(&mut a, State::Connected).await;
    until(&mut b, State::Connected).await;
    (a, b)
}

async fn until(conn: &mut Connection, state: State) {
    while let Some(event) = conn.next().await {
        if event == Event::State(state) {
            return;
        }
    }
    panic!("connection ended");
}

async fn message(conn: &mut Connection) -> Vec<u8> {
    while let Some(event) = conn.next().await {
        if let Event::Message(m) = event {
            return m;
        }
    }
    panic!("connection ended");
}

async fn e2e(relay: &str, rooms: usize, trips: usize) {
    let mut handshakes = Vec::new();
    let mut pairs = Vec::new();
    for _ in 0..rooms {
        let t = Instant::now();
        pairs.push(connected_pair(relay).await);
        handshakes.push(t.elapsed());
    }
    report_latency("pairing: both peers connect, join and handshake", handshakes, Duration::ZERO);
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for (mut a, mut b) in pairs {
        tasks.spawn(async move {
            let echo = tokio::spawn(async move {
                for _ in 0..trips {
                    let m = message(&mut b).await;
                    b.send(m).await.unwrap();
                }
                b
            });
            let mut samples = Vec::with_capacity(trips);
            for _ in 0..trips {
                let sent = Instant::now();
                a.send(vec![7u8; 64]).await.unwrap();
                message(&mut a).await;
                samples.push(sent.elapsed());
            }
            let _b = echo.await.unwrap();
            samples
        });
    }
    let samples: Vec<Duration> = tasks.join_all().await.into_iter().flatten().collect();
    report_latency(
        &format!("end-to-end encrypted round trip, {rooms} rooms × {trips}, 64 B"),
        samples,
        started.elapsed(),
    );
}

async fn bulk(relay: &str, size: usize, seconds: u64) {
    let (a, mut b) = connected_pair(relay).await;
    let sender = a.sender();
    let writer = tokio::spawn(async move {
        let data = vec![1u8; size];
        while sender.send(data.clone()).await.is_ok() {}
    });
    let mut received = 0u64;
    let window = Duration::from_secs(seconds);
    let started = Instant::now();
    let _ = tokio::time::timeout(window, async {
        loop {
            received += message(&mut b).await.len() as u64;
        }
    })
    .await;
    let secs = started.elapsed().as_secs_f64();
    println!("end-to-end bulk, one room, {size} B messages: {:.1} MB/s received", received as f64 / secs / 1e6);
    writer.abort();
    drop(a);
}

fn report_latency(what: &str, mut samples: Vec<Duration>, elapsed: Duration) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    let rate = if elapsed.is_zero() {
        String::new()
    } else {
        format!(", {:.0}/s", samples.len() as f64 / elapsed.as_secs_f64())
    };
    println!(
        "{what}: n={}{rate}, p50 {:.2?}, p90 {:.2?}, p99 {:.2?}, max {:.2?}",
        samples.len(),
        at(0.5),
        at(0.9),
        at(0.99),
        samples[samples.len() - 1]
    );
}
