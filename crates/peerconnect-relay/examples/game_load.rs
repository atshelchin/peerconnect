//! A load test that behaves like a game: many groups of ten players, each
//! over its own real WebSocket, end-to-end encrypted, through a relay.
//!
//! Every player, all the time:
//! - sends its position 20 times a second (datagrams, 48 bytes);
//! - sends a game command twice a second (60–200 bytes);
//! - sends a chat line every 10 s;
//! - and in each group three players talk at once, 50 voice frames a second
//!   (80 bytes), the talkers changing every 3 s.
//!
//! Halfway through, a newcomer joins every group and fetches a 2 MB world
//! snapshot from the first player, while all of that carries on.
//!
//! Options: `--players N` per group (default 10), `--light` (commands only,
//! to measure a path's own delay), `--no-sync` (no newcomers),
//! `--shape KBPS:MS` to put every player behind its own emulated access
//! link of that rate and one-way delay, with a deep buffer, and `--quic`
//! to have the spawned relay's players connect over QUIC.
//!
//! Reported: per class, the time from the sender's `send` to the receiver's
//! event (both in this process, one clock), how much of what was sent
//! arrived, and the relay's CPU and memory if it was started by this tool.
//!
//! ```text
//! cargo build --release -p peerconnect-relay --examples
//! target/release/examples/game_load --spawn target/release/peerconnect-relay --groups 100 --seconds 30
//! target/release/examples/game_load --relay wss://relay.example.com --groups 2 --seconds 20
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use peerconnect::client::group;
use peerconnect::group::{Channel, Config, Event, Group, Identity, MemberId};

const TALKERS: usize = 3;
const SNAPSHOT: usize = 2 << 20;

/// Latency histogram: 10 µs buckets up to 2 s, then one overflow bucket.
struct Histogram {
    buckets: Vec<AtomicU64>,
}

const BUCKET_US: u64 = 10;
const BUCKETS: usize = 200_000;

impl Histogram {
    fn new() -> Histogram {
        Histogram { buckets: (0..=BUCKETS).map(|_| AtomicU64::new(0)).collect() }
    }

    fn record(&self, us: u64) {
        let i = ((us / BUCKET_US) as usize).min(BUCKETS);
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
    }

    fn count(&self) -> u64 {
        self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).sum()
    }

    fn quantile(&self, q: f64) -> f64 {
        let total = self.count();
        if total == 0 {
            return f64::NAN;
        }
        let target = ((total as f64) * q).ceil() as u64;
        let mut seen = 0;
        for (i, b) in self.buckets.iter().enumerate() {
            seen += b.load(Ordering::Relaxed);
            if seen >= target {
                return ((i as u64 + 1) * BUCKET_US) as f64 / 1000.0;
            }
        }
        f64::INFINITY
    }
}

/// What one class of traffic did.
struct ClassStats {
    name: &'static str,
    /// Copies expected: each send counts once per other member present.
    expected: AtomicU64,
    latency: Histogram,
}

struct Stats {
    start: Instant,
    command: ClassStats,
    chat: ClassStats,
    voice: ClassStats,
    pose: ClassStats,
    /// Commands received by members other than the newcomers while a
    /// snapshot was on its way somewhere.
    command_during_sync: Histogram,
    voice_during_sync: Histogram,
    syncs_running: AtomicU64,
    snapshots: Histogram,
    failures: AtomicU64,
    /// Players who were connected over QUIC, and over a WebSocket, when they first heard from the group.
    over_quic: AtomicU64,
    over_websocket: AtomicU64,
    stop: AtomicBool,
}

impl Stats {
    fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
}

fn class(name: &'static str) -> ClassStats {
    ClassStats { name, expected: AtomicU64::new(0), latency: Histogram::new() }
}

/// `kind ‖ sent_us ‖ filler`: kind 0 pose, 1 voice, 2 command, 3 chat.
fn payload(kind: u8, now: u64, len: usize) -> Vec<u8> {
    let mut p = vec![0u8; len.max(9)];
    p[0] = kind;
    p[1..9].copy_from_slice(&now.to_be_bytes());
    p
}

fn sent_at(data: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(1..9)?.try_into().ok()?))
}

struct Args {
    relay: Option<String>,
    spawn: Option<String>,
    groups: usize,
    seconds: u64,
    players: usize,
    light: bool,
    /// Each player's access link: (kbit/s, one-way delay in ms).
    shape: Option<(u64, u64)>,
    /// No newcomers: the same traffic without state transfer, for comparison.
    no_sync: bool,
    quic: bool,
}

fn args() -> Args {
    let mut args = Args {
        relay: None,
        spawn: None,
        groups: 10,
        seconds: 30,
        players: 10,
        light: false,
        shape: None,
        no_sync: false,
        quic: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().expect("a value after the flag");
        match flag.as_str() {
            "--relay" => args.relay = Some(value()),
            "--spawn" => args.spawn = Some(value()),
            "--groups" => args.groups = value().parse().expect("a number"),
            "--seconds" => args.seconds = value().parse().expect("a number"),
            "--players" => args.players = value().parse().expect("a number"),
            // Commands only, ten a second: to tell the path's own delay from queueing.
            "--light" => args.light = true,
            "--no-sync" => args.no_sync = true,
            "--quic" => args.quic = true,
            // Each player behind its own link of this rate and delay, with a
            // deep buffer, as home and mobile links have: e.g. 2000:40.
            "--shape" => {
                let v = value();
                let (rate, delay) = v.split_once(':').expect("kbit/s:ms");
                args.shape = Some((rate.parse().unwrap(), delay.parse().unwrap()));
            }
            other => panic!("unknown flag {other}"),
        }
    }
    args
}

#[tokio::main]
async fn main() {
    let args = args();
    let members = args.players;
    let mut relay_process = None;
    let relay = match (&args.relay, &args.spawn) {
        (Some(url), _) => url.clone(),
        (None, Some(binary)) => {
            let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let child = std::process::Command::new(binary)
                .env("PEERCONNECT_LISTEN", format!("127.0.0.1:{port}"))
                .env("PEERCONNECT_MAX_PER_ADDRESS", "100000")
                .env("PEERCONNECT_MAX_CONNECTIONS", "100000")
                .env("PEERCONNECT_MAX_GROUP_MEMBERS", "16")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("relay binary");
            relay_process = Some(child);
            tokio::time::sleep(Duration::from_millis(300)).await;
            // The relay serves QUIC on the UDP port of the same number.
            format!("{}://127.0.0.1:{port}", if args.quic { "quic" } else { "ws" })
        }
        (None, None) => panic!("--relay <url> or --spawn <relay binary>"),
    };

    let stats = Arc::new(Stats {
        start: Instant::now(),
        command: class("command"),
        chat: class("chat"),
        voice: class("voice"),
        pose: class("position"),
        command_during_sync: Histogram::new(),
        voice_during_sync: Histogram::new(),
        syncs_running: AtomicU64::new(0),
        snapshots: Histogram::new(),
        failures: AtomicU64::new(0),
        over_quic: AtomicU64::new(0),
        over_websocket: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });
    let duration = Duration::from_secs(args.seconds);
    eprintln!("{} groups × {members} players (+1 newcomer each) on {relay} for {}s", args.groups, duration.as_secs());

    // Players reach the relay through their shaped links, if asked.
    let relay = match args.shape {
        Some((kbps, delay)) => shaped_link(&relay, kbps, delay).await,
        None => relay,
    };
    let mut tasks = Vec::new();
    for g in 0..args.groups {
        let group = Group::create(&relay).unwrap();
        for m in 0..members {
            tasks.push(tokio::spawn(player(group.clone(), g, m, members, args.light, stats.clone())));
        }
        if args.light || args.no_sync {
            continue;
        }
        let newcomer_at = duration / 2 + Duration::from_millis((g as u64 * 37) % 2000);
        tasks.push(tokio::spawn(newcomer(group, newcomer_at, stats.clone())));
        // Do not open every connection in the same millisecond.
        if g % 10 == 9 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // Sample the relay's CPU and memory, if it is ours.
    let pid = relay_process.as_ref().map(|c| c.id());
    let sampling = stats.clone();
    let sampler = tokio::spawn(async move {
        let mut samples = Vec::new();
        let Some(pid) = pid else { return samples };
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        while !sampling.stop.load(Ordering::Relaxed) {
            tick.tick().await;
            let out = std::process::Command::new("ps").args(["-o", "%cpu=,rss=", "-p", &pid.to_string()]).output();
            let Ok(out) = out else { break };
            let text = String::from_utf8_lossy(&out.stdout);
            let mut it = text.split_whitespace();
            match (it.next().and_then(|c| c.parse::<f64>().ok()), it.next().and_then(|r| r.parse::<u64>().ok())) {
                (Some(cpu), Some(rss)) => samples.push((cpu, rss)),
                _ => break,
            }
        }
        samples
    });

    tokio::time::sleep(duration).await;
    stats.stop.store(true, Ordering::Relaxed);
    // Let everyone leave, all at once, for at most a few seconds.
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        for t in tasks {
            let _ = t.await;
        }
    })
    .await;
    let samples = sampler.await.unwrap_or_default();

    println!("\n{} groups × {} players, {} s, relay {relay}", args.groups, members, args.seconds);
    println!(
        "{:<10} {:>12} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "class", "received", "arrived", "p50 ms", "p99 ms", "p99.9", "max ms"
    );
    for c in [&stats.command, &stats.chat, &stats.voice, &stats.pose] {
        let got = c.latency.count();
        let expected = c.expected.load(Ordering::Relaxed).max(1);
        println!(
            "{:<10} {:>12} {:>8.2}% {:>9.2} {:>9.2} {:>9.2} {:>9.2}",
            c.name,
            got,
            100.0 * got as f64 / expected as f64,
            c.latency.quantile(0.5),
            c.latency.quantile(0.99),
            c.latency.quantile(0.999),
            c.latency.quantile(1.0),
        );
    }
    let during = &stats.command_during_sync;
    println!(
        "commands while 2 MB snapshots were in flight: {} received, p50 {:.2} ms, p99 {:.2} ms",
        during.count(),
        during.quantile(0.5),
        during.quantile(0.99)
    );
    let during = &stats.voice_during_sync;
    println!(
        "voice while 2 MB snapshots were in flight: {} received, p50 {:.2} ms, p99 {:.2} ms",
        during.count(),
        during.quantile(0.5),
        during.quantile(0.99)
    );
    let snaps = &stats.snapshots;
    println!(
        "snapshots: {} of {} arrived, request to snapshot p50 {:.0} ms, max {:.0} ms; failures {}",
        snaps.count(),
        args.groups,
        snaps.quantile(0.5),
        snaps.quantile(1.0),
        stats.failures.load(Ordering::Relaxed)
    );
    println!(
        "players over QUIC: {}, over WebSocket: {}",
        stats.over_quic.load(Ordering::Relaxed),
        stats.over_websocket.load(Ordering::Relaxed)
    );
    if !samples.is_empty() {
        let busy: Vec<&(f64, u64)> = samples.iter().skip(3).collect();
        let cpu = busy.iter().map(|s| s.0).sum::<f64>() / busy.len().max(1) as f64;
        let rss = samples.iter().map(|s| s.1).max().unwrap_or(0);
        println!("relay: {cpu:.0}% CPU on average (100% = one core), {} MB resident at most", rss / 1024);
    }
    if let Some(mut child) = relay_process {
        let _ = child.kill();
    }
}

async fn player(group: Group, g: usize, m: usize, members: usize, light: bool, stats: Arc<Stats>) {
    let config = Config { profile: format!("player {g}.{m}").into_bytes(), ..Config::default() };
    let Ok(mut conn) = group::join(group, Identity::generate(), config) else { return };
    let sender = conn.sender();
    let mut present: usize = 0;
    let mut players = std::collections::HashSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ticks: u64 = (m as u64 * 7) % 50; // spread the players' schedules
    let mut voice_seq: u16 = 0;
    let mut counted = false;
    let me = conn.id();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if stats.stop.load(Ordering::Relaxed) {
                    break;
                }
                ticks += 1;
                if present == 0 {
                    continue; // nobody to talk to yet
                }
                let now = stats.now_us();
                let others = present as u64;
                if light {
                    if ticks.is_multiple_of(10) && sender.send(Channel::Command, payload(2, now, 100)).await.is_ok() {
                        stats.command.expected.fetch_add(others, Ordering::Relaxed);
                    }
                    continue;
                }
                if ticks.is_multiple_of(5) {
                    sender.send_datagram(None, payload(0, now, 48));
                    stats.pose.expected.fetch_add(others, Ordering::Relaxed);
                }
                let round = (now / 3_000_000) as usize;
                let talking = (0..TALKERS).any(|k| (round * TALKERS + k + g) % members == m);
                if ticks.is_multiple_of(2) {
                    voice_seq = voice_seq.wrapping_add(1);
                    if talking {
                        sender.send_voice(voice_seq, payload(1, now, 80));
                        stats.voice.expected.fetch_add(others, Ordering::Relaxed);
                    }
                }
                if ticks.is_multiple_of(50) {
                    let len = 60 + (ticks as usize * 13 + m * 31) % 140;
                    if sender.send(Channel::Command, payload(2, now, len)).await.is_ok() {
                        stats.command.expected.fetch_add(others, Ordering::Relaxed);
                    }
                }
                if ticks % 1000 == (m as u64 * 97) % 1000
                    && sender.send(Channel::Chat, payload(3, now, 50)).await.is_ok()
                {
                    stats.chat.expected.fetch_add(others, Ordering::Relaxed);
                }
            }
            event = conn.next() => match event {
                // Count only players: newcomers do not record what they get.
                Some(Event::Joined { member, profile }) if profile.starts_with(b"player") => {
                    if players.is_empty() && !counted {
                        counted = true;
                        let over = if conn.via() == Some(group::Via::Quic) { &stats.over_quic } else { &stats.over_websocket };
                        over.fetch_add(1, Ordering::Relaxed);
                    }
                    players.insert(member);
                    present = players.len();
                }
                Some(Event::Left { member }) => {
                    players.remove(&member);
                    present = players.len();
                }
                Some(Event::Message { from, channel, data, direct: false }) if from != me => {
                    record(&stats, &data, match channel { Channel::Command => &stats.command, Channel::Chat => &stats.chat });
                    if channel == Channel::Command
                        && stats.syncs_running.load(Ordering::Relaxed) > 0
                        && let Some(sent) = sent_at(&data)
                    {
                        stats.command_during_sync.record(stats.now_us().saturating_sub(sent));
                    }
                }
                Some(Event::Voice { data, .. }) => {
                    record(&stats, &data, &stats.voice);
                    if stats.syncs_running.load(Ordering::Relaxed) > 0
                        && let Some(sent) = sent_at(&data)
                    {
                        stats.voice_during_sync.record(stats.now_us().saturating_sub(sent));
                    }
                }
                Some(Event::Datagram { data, .. }) => record(&stats, &data, &stats.pose),
                Some(Event::SyncRequest { from, transfer }) => {
                    // The world as it stands: here, 2 MB of filler.
                    sender.provide_sync(from, transfer, vec![7u8; SNAPSHOT]);
                }
                Some(_) => {}
                None => break,
            },
        }
    }
    conn.leave().await;
}

fn record(stats: &Stats, data: &[u8], class: &ClassStats) {
    if let Some(sent) = sent_at(data) {
        class.latency.record(stats.now_us().saturating_sub(sent));
    }
}

async fn newcomer(group: Group, at: Duration, stats: Arc<Stats>) {
    tokio::time::sleep(at).await;
    if stats.stop.load(Ordering::Relaxed) {
        return;
    }
    let config = Config { profile: b"newcomer".to_vec(), ..Config::default() };
    let Ok(mut conn) = group::join(group, Identity::generate(), config) else { return };
    let first: MemberId = loop {
        match conn.next().await {
            // Any member will do; it has the same state as the others.
            Some(Event::Joined { member, .. }) => break member,
            Some(Event::Full) => {
                stats.failures.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Some(_) => {}
            None => return,
        }
    };
    let asked = Instant::now();
    if conn.request_sync(first).await.is_err() {
        stats.failures.fetch_add(1, Ordering::Relaxed);
        return;
    }
    stats.syncs_running.fetch_add(1, Ordering::Relaxed);
    loop {
        match tokio::time::timeout(Duration::from_secs(30), conn.next()).await {
            Ok(Some(Event::Snapshot { data, .. })) => {
                assert_eq!(data.len(), SNAPSHOT);
                stats.snapshots.record(asked.elapsed().as_micros() as u64);
                eprintln!("snapshot of {} bytes in {:?}", data.len(), asked.elapsed());
                break;
            }
            other @ (Ok(Some(Event::SyncFailed { .. })) | Ok(None) | Err(_)) => {
                let why = match other {
                    Ok(Some(_)) => "the transfer failed",
                    Ok(None) => "the connection ended",
                    Err(_) => "nothing for 30 s",
                };
                eprintln!("snapshot failed after {:?}: {why}", asked.elapsed());
                stats.failures.fetch_add(1, Ordering::Relaxed);
                break;
            }
            Ok(Some(Event::State(state))) => eprintln!("newcomer: {state:?} at {:?}", asked.elapsed()),
            Ok(Some(Event::Left { .. })) => eprintln!("newcomer: someone left at {:?}", asked.elapsed()),
            Ok(Some(_)) => {}
        }
    }
    stats.syncs_running.fetch_sub(1, Ordering::Relaxed);
    while !stats.stop.load(Ordering::Relaxed) {
        if tokio::time::timeout(Duration::from_millis(200), conn.next()).await.is_ok_and(|e| e.is_none()) {
            break;
        }
    }
    conn.leave().await;
}

/// A local proxy that gives every connection through it its own link of
/// `kbps` each way and `delay_ms` one way: bytes queue behind each other at
/// the link's rate, then take the delay, like a real access link with a
/// deep buffer. Returns the URL to use instead of `relay`.
async fn shaped_link(relay: &str, kbps: u64, delay_ms: u64) -> String {
    let target = relay.trim_start_matches("ws://").to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else { return };
            let _ = client.set_nodelay(true);
            let target = target.clone();
            tokio::spawn(async move {
                let Ok(upstream) = tokio::net::TcpStream::connect(target).await else { return };
                let _ = upstream.set_nodelay(true);
                let (cr, cw) = client.into_split();
                let (ur, uw) = upstream.into_split();
                tokio::spawn(link(cr, uw, kbps, delay_ms));
                link(ur, cw, kbps, delay_ms).await;
            });
        }
    });
    url
}

async fn link(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    kbps: u64,
    delay_ms: u64,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
    let writer = tokio::spawn(async move {
        while let Some((at, chunk)) = rx.recv().await {
            tokio::time::sleep_until(at).await;
            if to.write_all(&chunk).await.is_err() {
                return;
            }
        }
    });
    let bytes_per_ms = (kbps as f64 / 8.0).max(1.0);
    let mut free_at = tokio::time::Instant::now();
    let mut buf = vec![0u8; 16 << 10];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // Serialised at the link's rate behind what is already queued,
        // then the propagation delay.
        let now = tokio::time::Instant::now();
        free_at = free_at.max(now) + Duration::from_secs_f64(n as f64 / bytes_per_ms / 1000.0);
        if tx.send((free_at + Duration::from_millis(delay_ms), buf[..n].to_vec())).is_err() {
            break;
        }
    }
    drop(tx);
    let _ = writer.await;
}
