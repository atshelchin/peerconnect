//! PeerConnect relay for ordinary servers (SPEC §5).
//!
//! Each connection has a reader task, which forwards frames straight into
//! the partner's outbox, and a writer task, which drains its own outbox into
//! the socket. An outbox holds at most a fixed number of bytes: a slow
//! reader holds back its sender through TCP backpressure instead of growing
//! memory, and one that accepts nothing for 30 s is disconnected rather than
//! allowed to hold its partner hostage. The only shared state is the seat
//! table, one small mutex per room.
//!
//! Groups (`/g1/<id>`) work differently, without backpressure: see [`group`].
//! They can also be reached over QUIC, on a UDP socket ([`quic`]): raw QUIC
//! for native members, and WebTransport for browsers, which needs a
//! certificate they trust ([`Tls`]). With one, the TCP listener also
//! accepts `wss://` next to plain `ws://`, told apart by their first byte.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use peerconnect::RoomId;
use peerconnect::relay::{self, Action, Conn};
use peerconnect::wire::{self, Role, close};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role as WsRole, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

mod group;
pub mod quic;
mod tls;

pub use tls::Tls;

/// A connection that accepts nothing forwarded to it for this long is closed.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// A connection the relay has written nothing to for this long gets a `pong`,
/// so that a peer whose own pings are stuck behind its data still hears from us.
const KEEPALIVE: Duration = Duration::from_secs(30);
/// Bytes written before each flush, at most.
const BATCH: usize = 256 << 10;

#[derive(Debug, Clone)]
pub struct Config {
    /// Connections beyond this are refused with HTTP 503.
    pub max_connections: usize,
    /// Connections from one client address (an IPv6 /64 counts as one)
    /// beyond this are refused with HTTP 429. Many users can share one
    /// address behind carrier-grade or corporate NAT: keep it generous.
    pub max_per_address: usize,
    /// Take the client address from the last `X-Forwarded-For` entry, as
    /// appended by a proxy in front of the relay. Only enable this behind
    /// such a proxy: clients could otherwise claim any address.
    pub trust_proxy: bool,
    /// Bytes queued towards one connection before its partner is held back.
    /// Worst-case buffer memory is about `max_connections × queue_bytes`.
    pub queue_bytes: usize,
    /// Members per group, at most 32; more are refused with close code 4429.
    pub max_group_members: usize,
    /// Reliable bytes queued towards one group connection beyond which it
    /// is not keeping up and is closed.
    pub group_queue_bytes: usize,
    /// Command and voice bytes queued towards a group connection beyond
    /// which voice frames for it are dropped: bounds how late audio gets.
    pub voice_queue_bytes: usize,
    /// Video bytes queued towards a group connection beyond which its
    /// oldest video is dropped: bounds how late video gets.
    pub video_queue_bytes: usize,
    /// What one group connection may send, in bytes per second, sustained
    /// and in bursts. Beyond it voice is dropped and the rest delayed.
    pub group_rate: usize,
    pub group_burst: usize,
    /// The certificate for `wss://` and WebTransport, reloaded when its
    /// files change. Without it the relay serves plain `ws://`, and raw QUIC
    /// with a certificate of its own making.
    pub tls: Option<Tls>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_connections: 10_000,
            max_per_address: 256,
            trust_proxy: false,
            queue_bytes: 256 << 10,
            max_group_members: 10,
            group_queue_bytes: 2 << 20,
            voice_queue_bytes: 8 << 10,
            video_queue_bytes: 64 << 10,
            group_rate: 2 << 20,
            group_burst: 4 << 20,
            tls: None,
        }
    }
}

/// Serves the relay on `listener` until `shutdown` completes, then closes
/// every connection with 1001 (going away) and waits briefly for them.
///
/// Panics if [`Config::tls`] names files that do not hold a certificate;
/// [`serve_all`] reports that instead.
pub async fn serve(listener: TcpListener, config: Config, shutdown: impl Future<Output = ()>) {
    if let Err(err) = serve_all(listener, None, config, shutdown).await {
        panic!("{err}");
    }
}

/// Like [`serve`], and serves groups over QUIC on `quic` too.
pub async fn serve_all(
    listener: TcpListener,
    quic: Option<std::net::UdpSocket>,
    config: Config,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    let certificate = tls::Certificate::new(config.tls.as_ref())?;
    let acceptor = certificate
        .is_configured()
        .then(|| tokio_rustls::TlsAcceptor::from(Arc::new(certificate.server_config(&[b"http/1.1"], false))));
    let endpoint = match quic {
        Some(socket) => Some(quic::endpoint(socket, &certificate).map_err(|err| format!("cannot serve QUIC: {err}"))?),
        None => None,
    };
    let relay = Arc::new(Relay {
        rooms: Mutex::new(HashMap::new()),
        groups: Mutex::new(HashMap::new()),
        addresses: Arc::new(Mutex::new(HashMap::new())),
        slots: Arc::new(Semaphore::new(config.max_connections)),
        next_id: AtomicU64::new(0),
        stop: watch::channel(false).0,
        config,
    });
    let watcher = tokio::spawn(certificate.clone().watch());
    if let Some(endpoint) = &endpoint {
        tokio::spawn(relay.clone().serve_quic(endpoint.clone()));
    }
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                let _ = stream.set_nodelay(true);
                limit_unsent(&stream);
                tokio::spawn(relay.clone().tcp(stream, peer.ip(), acceptor.clone()));
            }
            () = &mut shutdown => break,
        }
    }
    relay.stop.send_replace(true);
    let all = relay.config.max_connections as u32;
    let _ = timeout(Duration::from_secs(5), relay.slots.acquire_many(all)).await;
    watcher.abort();
    if let Some(endpoint) = endpoint {
        let _ = timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    }
    Ok(())
}

struct Relay {
    rooms: Mutex<HashMap<RoomId, Arc<Room>>>,
    groups: Mutex<HashMap<RoomId, Arc<group::GroupRoom>>>,
    /// Open WebSockets per client address.
    addresses: Arc<Mutex<HashMap<IpAddr, usize>>>,
    slots: Arc<Semaphore>,
    next_id: AtomicU64,
    stop: watch::Sender<bool>,
    config: Config,
}

/// The two seats of one room.
#[derive(Default)]
struct Room {
    seats: Mutex<[Option<Seat>; 2]>,
}

struct Seat {
    id: u64,
    outbox: Arc<Outbox>,
}

/// What a connection's writer will send: frames, under a byte budget, and
/// the order to close.
struct Outbox {
    tx: mpsc::UnboundedSender<(Message, Option<OwnedSemaphorePermit>)>,
    budget: Arc<Semaphore>,
    /// The budget in bytes.
    size: usize,
    close: watch::Sender<Option<u16>>,
}

type Queue = mpsc::UnboundedReceiver<(Message, Option<OwnedSemaphorePermit>)>;

impl Outbox {
    fn new(bytes: usize) -> (Arc<Outbox>, Queue) {
        let (tx, rx) = mpsc::unbounded_channel();
        let outbox =
            Outbox { tx, budget: Arc::new(Semaphore::new(bytes)), size: bytes, close: watch::Sender::new(None) };
        (Arc::new(outbox), rx)
    }

    /// Waits for room for `len` bytes. A connection that makes no room in
    /// time is not reading: it is closed, and `None` returned.
    async fn reserve(&self, len: usize) -> Option<OwnedSemaphorePermit> {
        // Never wait for more than the whole budget, so any frame fits.
        let len = len.min(self.size) as u32;
        match timeout(STALL_TIMEOUT, self.budget.clone().acquire_many_owned(len)).await {
            Ok(permit) => permit.ok(),
            Err(_) => {
                self.close(close::TIMEOUT);
                None
            }
        }
    }

    /// Queues a small relay message if there is room, dropping it otherwise.
    fn offer(&self, msg: Message) {
        if let Ok(permit) = self.budget.clone().try_acquire_many_owned(msg.len() as u32) {
            self.push(msg, Some(permit));
        }
    }

    fn push(&self, msg: Message, permit: Option<OwnedSemaphorePermit>) {
        let _ = self.tx.send((msg, permit));
    }

    /// Tells the connection to close with `code`; the first code wins.
    fn close(&self, code: u16) {
        self.close.send_if_modified(|current| current.is_none() && current.replace(code).is_none());
    }
}

pub(crate) type Ws = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

impl Relay {
    /// Serves a TCP connection: TLS if it starts with a TLS handshake
    /// record and a certificate is configured, plain HTTP otherwise.
    async fn tcp(self: Arc<Self>, stream: tokio::net::TcpStream, peer: IpAddr, tls: Option<tokio_rustls::TlsAcceptor>) {
        let Some(tls) = tls else { return self.http(stream, peer).await };
        let mut first = [0];
        match timeout(wire::JOIN_TIMEOUT, stream.peek(&mut first)).await {
            Ok(Ok(1)) if first[0] == 0x16 => {
                if let Ok(Ok(stream)) = timeout(wire::JOIN_TIMEOUT, tls.accept(stream)).await {
                    self.http(stream, peer).await;
                }
            }
            Ok(Ok(1)) => self.http(stream, peer).await,
            _ => {}
        }
    }

    async fn http(self: Arc<Self>, stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static, peer: IpAddr) {
        let service = hyper::service::service_fn(move |req| {
            let relay = self.clone();
            async move { Ok::<_, Infallible>(relay.route(req, peer)) }
        });
        // A connection that never finishes its request must not linger.
        let conn = hyper::server::conn::http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(wire::JOIN_TIMEOUT)
            .serve_connection(TokioIo::new(stream), service)
            .with_upgrades();
        let _ = conn.await;
    }

    fn route(self: Arc<Self>, req: Request<Incoming>, peer: IpAddr) -> Response<Full<Bytes>> {
        let path = req.uri().path();
        if path == "/" || path == "/healthz" {
            return text(StatusCode::OK, "peerconnect relay\n");
        }
        let endpoint = match (relay::room_from_path(path), relay::group_from_path(path)) {
            (Some(room), _) => Endpoint::Room(room),
            (_, Some(group)) => Endpoint::Group(group),
            _ => return text(StatusCode::NOT_FOUND, "not found\n"),
        };
        let Some(key) = websocket_key(&req) else {
            return text(StatusCode::UPGRADE_REQUIRED, "expected a WebSocket upgrade\n");
        };
        let address = if self.config.trust_proxy { forwarded_for(&req).unwrap_or(peer) } else { peer };
        let Some(per_address) = self.admit(address) else {
            return text(StatusCode::TOO_MANY_REQUESTS, "too many connections from this address\n");
        };
        let Ok(slot) = self.slots.clone().try_acquire_owned() else {
            return text(StatusCode::SERVICE_UNAVAILABLE, "relay full\n");
        };
        let accept = derive_accept_key(key.as_bytes());
        let upgrade = hyper::upgrade::on(req);
        tokio::spawn(async move {
            let Ok(upgraded) = upgrade.await else { return };
            // Small buffers: most connections are idle, and frames are at
            // most 64 KiB (16 KiB in groups).
            let max = match endpoint {
                Endpoint::Room(_) => wire::MAX_FRAME_LEN,
                Endpoint::Group(_) => peerconnect::group::wire::MAX_FRAME_LEN,
            };
            let config = WebSocketConfig::default()
                .read_buffer_size(16 << 10)
                .write_buffer_size(16 << 10)
                .max_message_size(Some(max))
                .max_frame_size(Some(max));
            let ws = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), WsRole::Server, Some(config)).await;
            match endpoint {
                Endpoint::Room(room) => self.connection(room, ws, slot).await,
                Endpoint::Group(group) => self.group_websocket(group, ws, slot).await,
            }
            drop(per_address);
        });
        let mut res = Response::new(Full::default());
        *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        let headers = res.headers_mut();
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(header::SEC_WEBSOCKET_ACCEPT, HeaderValue::from_str(&accept).unwrap());
        res
    }

    async fn connection(self: Arc<Self>, room_id: RoomId, ws: Ws, _slot: OwnedSemaphorePermit) {
        let (sink, mut stream) = ws.split();
        let (outbox, queue) = Outbox::new(self.config.queue_bytes);
        let mut closing = outbox.close.subscribe();
        let writer = tokio::spawn(write(sink, queue, outbox.close.subscribe(), self.stop.subscribe()));

        let (pending, hello) = Conn::new();
        outbox.push(Message::binary(hello.to_vec()), None);
        let join_deadline = Instant::now() + wire::JOIN_TIMEOUT;
        let mut seated: Option<(Arc<Room>, u64, Role)> = None;

        loop {
            let deadline = if seated.is_some() { Instant::now() + wire::IDLE_TIMEOUT } else { join_deadline };
            let msg = tokio::select! {
                biased;
                _ = closing.changed() => break, // replaced, or closed by the relay
                msg = timeout_at(deadline, stream.next()) => msg,
            };
            let msg = match msg {
                Ok(Some(Ok(msg))) => msg,
                Ok(Some(Err(WsError::Capacity(_)))) => break outbox.close(close::TOO_BIG),
                Ok(_) => break,
                Err(_) => break outbox.close(close::TIMEOUT),
            };
            let conn = seated.as_ref().map_or(pending, |&(_, _, role)| Conn::Seated(role));
            match receive(&conn, &room_id, &msg) {
                Some(Action::Join(role)) => {
                    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                    let room = self.rooms.lock().unwrap().entry(room_id).or_default().clone();
                    room.seat(id, role, outbox.clone()).await;
                    seated = Some((room, id, role));
                }
                Some(Action::Forward) => {
                    let (room, id, role) = seated.as_ref().expect("only seated connections forward");
                    if !room.forward(*id, *role, msg).await {
                        break; // replaced
                    }
                }
                Some(Action::Pong) => outbox.offer(Message::text(wire::PONG)),
                Some(Action::Close(code)) => break outbox.close(code),
                None => {}
            }
        }

        if let Some((room, id, role)) = seated {
            room.leave(id, role).await;
            drop(room);
            self.collect(&room_id);
        }
        outbox.close(close::NORMAL);
        let _ = writer.await;
    }

    /// Counts a connection against its client address, if under the limit.
    fn admit(&self, address: IpAddr) -> Option<AddressSlot> {
        let key = address_key(address);
        let mut addresses = self.addresses.lock().unwrap();
        let count = addresses.entry(key).or_default();
        if *count >= self.config.max_per_address {
            return None;
        }
        *count += 1;
        Some(AddressSlot { addresses: self.addresses.clone(), key })
    }

    /// Forgets a room once nobody sits in it or is about to.
    fn collect(&self, room_id: &RoomId) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(room) = rooms.get(room_id) {
            let empty = room.seats.lock().unwrap().iter().all(Option::is_none);
            if empty && Arc::strong_count(room) == 1 {
                rooms.remove(room_id);
            }
        }
    }
}

impl Room {
    /// Seats a connection, replacing whoever held the seat, and tells both
    /// sides who is there. Nothing can reach the new connection before its
    /// own `PRESENCE`, and the partner sees `PRESENCE(1)` before any frame
    /// from it.
    async fn seat(&self, id: u64, role: Role, outbox: Arc<Outbox>) {
        let partner = {
            let mut seats = self.seats.lock().unwrap();
            let partner = seats[role.other().index()].as_ref().map(|s| (s.id, s.outbox.clone()));
            outbox.push(presence(partner.is_some()), None);
            if let Some(old) = seats[role.index()].replace(Seat { id, outbox }) {
                old.outbox.close(close::REPLACED);
            }
            partner
        };
        if let Some((partner_id, partner)) = partner {
            let Some(permit) = partner.reserve(wire::PRESENCE_LEN).await else { return };
            let seats = self.seats.lock().unwrap();
            if holds(&seats, role, id) && holds(&seats, role.other(), partner_id) {
                partner.push(presence(true), Some(permit));
            }
        }
    }

    /// Forwards a peer frame to the other seat, waiting for room in its
    /// outbox. Returns false once this connection no longer holds its seat.
    async fn forward(&self, id: u64, role: Role, msg: Message) -> bool {
        let (partner_id, partner) = {
            let seats = self.seats.lock().unwrap();
            if !holds(&seats, role, id) {
                return false;
            }
            match &seats[role.other().index()] {
                Some(s) => (s.id, s.outbox.clone()),
                None => return true, // nobody to forward to: drop
            }
        };
        let Some(permit) = partner.reserve(msg.len()).await else { return true };
        // Seats may have changed while waiting; a frame meant for a partner
        // connection that has since been replaced is stale, so drop it.
        let seats = self.seats.lock().unwrap();
        if !holds(&seats, role, id) {
            return false;
        }
        if holds(&seats, role.other(), partner_id) {
            partner.push(msg, Some(permit));
        }
        true
    }

    async fn leave(&self, id: u64, role: Role) {
        let partner = {
            let mut seats = self.seats.lock().unwrap();
            if !holds(&seats, role, id) {
                return;
            }
            seats[role.index()] = None;
            seats[role.other().index()].as_ref().map(|s| (s.id, s.outbox.clone()))
        };
        if let Some((partner_id, partner)) = partner {
            let Some(permit) = partner.reserve(wire::PRESENCE_LEN).await else { return };
            let seats = self.seats.lock().unwrap();
            // If someone has already taken the seat, the partner was told.
            if seats[role.index()].is_none() && holds(&seats, role.other(), partner_id) {
                partner.push(presence(false), Some(permit));
            }
        }
    }
}

/// Drains a connection's outbox into its socket, batching writes, until
/// told to close. A write that cannot complete does not hold up a close.
async fn write(
    mut sink: SplitSink<Ws, Message>,
    mut queue: Queue,
    mut close: watch::Receiver<Option<u16>>,
    mut stop: watch::Receiver<bool>,
) {
    let code = loop {
        let keepalive = Instant::now() + KEEPALIVE;
        let first = tokio::select! {
            biased;
            _ = close.changed() => break close.borrow().unwrap_or(close::NORMAL),
            _ = stop.wait_for(|stop| *stop) => break close::GOING_AWAY,
            item = queue.recv() => match item {
                Some((msg, _permit)) => msg,
                None => break close::NORMAL,
            },
            () = sleep_until(keepalive) => Message::text(wire::PONG),
        };
        let batch = async {
            let mut written = first.len();
            sink.feed(first).await?;
            while written < BATCH {
                let Ok((msg, _permit)) = queue.try_recv() else { break };
                written += msg.len();
                sink.feed(msg).await?;
            }
            sink.flush().await
        };
        // A close interrupts a write that is taking its time; the close
        // frame then gets one more second, which a dead socket will not use.
        tokio::select! {
            biased;
            _ = close.changed() => break close.borrow().unwrap_or(close::NORMAL),
            _ = stop.wait_for(|stop| *stop) => break close::GOING_AWAY,
            result = batch => if result.is_err() { return },
        }
    };
    let _ = timeout(Duration::from_secs(1), sink.send(close_message(code))).await;
}

fn receive(conn: &Conn, room: &RoomId, msg: &Message) -> Option<Action> {
    Some(match msg {
        Message::Text(text) => conn.receive(room, relay::Incoming::Text(text)),
        Message::Binary(frame) => conn.receive(room, relay::Incoming::Binary(frame)),
        _ => return None, // control frames are handled by the WebSocket layer
    })
}

#[derive(Clone, Copy)]
enum Endpoint {
    Room(RoomId),
    Group(RoomId),
}

/// Keeps little unsent data in the kernel's send buffer, so that it waits in
/// the relay's queues instead, where urgent frames overtake the rest and
/// late voice is dropped. Throughput is unaffected: this bounds only what
/// has not been sent yet, not what is in flight.
fn limit_unsent(stream: &tokio::net::TcpStream) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::fd::AsRawFd;
        // Not in the libc crate on every platform.
        #[cfg(target_os = "linux")]
        const TCP_NOTSENT_LOWAT: libc::c_int = 25;
        #[cfg(target_os = "macos")]
        const TCP_NOTSENT_LOWAT: libc::c_int = 0x201;
        let lowat: libc::c_int = 32 << 10;
        // SAFETY: a valid socket descriptor and a correctly sized option value.
        unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::IPPROTO_TCP,
                TCP_NOTSENT_LOWAT,
                (&lowat as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = stream;
}

pub(crate) fn close_message(code: u16) -> Message {
    Message::Close(Some(CloseFrame { code: code.into(), reason: "".into() }))
}

fn presence(present: bool) -> Message {
    Message::binary(wire::presence(present).to_vec())
}

fn holds(seats: &[Option<Seat>; 2], role: Role, id: u64) -> bool {
    seats[role.index()].as_ref().is_some_and(|s| s.id == id)
}

/// Releases a connection's count against its address.
struct AddressSlot {
    addresses: Arc<Mutex<HashMap<IpAddr, usize>>>,
    key: IpAddr,
}

impl Drop for AddressSlot {
    fn drop(&mut self) {
        let mut addresses = self.addresses.lock().unwrap();
        if let Some(count) = addresses.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                addresses.remove(&self.key);
            }
        }
    }
}

/// IPv4 addresses count individually; IPv6 ones by /64, which one client
/// usually controls whole.
fn address_key(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6((u128::from(v6) & !((1u128 << 64) - 1)).into()),
        },
        v4 => v4,
    }
}

/// The address the nearest proxy saw: the last `X-Forwarded-For` entry.
fn forwarded_for(req: &Request<Incoming>) -> Option<IpAddr> {
    let last = req.headers().get_all("x-forwarded-for").iter().next_back()?;
    last.to_str().ok()?.rsplit(',').next()?.trim().parse().ok()
}

fn websocket_key(req: &Request<Incoming>) -> Option<&str> {
    let headers = req.headers();
    let has = |name, value: &str| {
        headers
            .get_all(name)
            .iter()
            .any(|v| v.to_str().is_ok_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(value))))
    };
    let ok = req.method() == hyper::Method::GET
        && has(header::CONNECTION, "upgrade")
        && has(header::UPGRADE, "websocket")
        && headers.get(header::SEC_WEBSOCKET_VERSION).is_some_and(|v| v == "13");
    ok.then(|| headers.get(header::SEC_WEBSOCKET_KEY)?.to_str().ok()).flatten()
}

fn text(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *res.status_mut() = status;
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_counted_per_host_or_ipv6_64() {
        let key = |s: &str| address_key(s.parse().unwrap());
        assert_eq!(key("192.0.2.7"), key("192.0.2.7"));
        assert_ne!(key("192.0.2.7"), key("192.0.2.8"));
        assert_eq!(key("2001:db8:1:2:aaaa::1"), key("2001:db8:1:2:ffff::9"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("::ffff:192.0.2.7"), key("192.0.2.7"));
    }
}
