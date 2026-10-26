//! Groups over QUIC (GROUP.md §3.6), on one UDP socket: WebTransport for
//! browsers (ALPN `h3`) and raw QUIC for everything else.
//!
//! Each side sends each class on a unidirectional stream of its own, so a
//! lost packet holds up only its class, and voice as datagrams, which wait
//! for nothing. The relay reads a member's command stream first, and holds
//! its sender back through QUIC flow control, as it does through TCP for
//! WebSockets; a voice datagram that finds the relay busy is dropped.

use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use peerconnect::RoomId;
use peerconnect::group::transport::{self, Deframer};
use peerconnect::group::{Class, wire as gwire};
use peerconnect::relay;
use peerconnect::wire::close;
use tokio::sync::{OwnedSemaphorePermit, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use web_transport_quinn::{RecvStream, SendStream, Session};

use crate::group::{Link, Outbox, Received};
use crate::tls::Certificate;
use crate::{AddressSlot, Relay};

/// Room in the datagram buffer that video leaves for voice, which goes
/// out in the order queued: so voice waits behind little video.
const VOICE_RESERVE: usize = 4 << 10;
/// How often video waiting for room in the datagram buffer looks again.
const VIDEO_RETRY: Duration = Duration::from_millis(2);
/// Messages read ahead per class before the relay stops reading a stream.
const READ_AHEAD: usize = 16;

/// The transport parameters both sides use: small buffers, since nothing
/// here moves much data at once, and a datagram buffer of a fraction of a
/// second of voice, beyond which the oldest is dropped.
pub fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.max_concurrent_uni_streams(16u32.into()) // four classes, and HTTP/3's own
        .max_concurrent_bidi_streams(4u32.into()) // the WebTransport CONNECT
        .max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()))
        .stream_receive_window((256u32 << 10).into())
        .receive_window((1u32 << 20).into())
        .send_window(1 << 20)
        // Room for a keyframe's burst of datagrams; a send buffer small
        // enough that voice behind video waits little (16 KiB at 5 Mbit/s:
        // 26 ms), large enough for 1080p.
        .datagram_receive_buffer_size(Some(512 << 10))
        .datagram_send_buffer_size(16 << 10);
    t
}

/// Starts the QUIC endpoint on `socket`.
pub(crate) fn endpoint(socket: UdpSocket, certificate: &Arc<Certificate>) -> std::io::Result<quinn::Endpoint> {
    enlarge_buffers(&socket);
    let tls = certificate.server_config(&[web_transport_quinn::ALPN.as_bytes(), transport::ALPN], true);
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).map_err(std::io::Error::other)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()));
    quinn::Endpoint::new(quinn::EndpointConfig::default(), Some(config), socket, Arc::new(quinn::TokioRuntime))
}

/// Asks for socket buffers that hold a burst of packets from many members
/// (the kernel may grant less: see `net.core.rmem_max`).
fn enlarge_buffers(socket: &UdpSocket) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let size: libc::c_int = 4 << 20;
        for option in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
            // SAFETY: a valid socket descriptor and a correctly sized option value.
            unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    (&size as *const libc::c_int).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }
    }
    #[cfg(not(unix))]
    let _ = socket;
}

impl Relay {
    pub(crate) async fn serve_quic(self: Arc<Self>, endpoint: quinn::Endpoint) {
        let mut stop = self.stop.subscribe();
        loop {
            let incoming = tokio::select! {
                biased;
                _ = stop.wait_for(|stop| *stop) => break,
                incoming = endpoint.accept() => match incoming {
                    Some(incoming) => incoming,
                    None => break,
                },
            };
            let Some(per_address) = self.admit(incoming.remote_address().ip()) else {
                incoming.refuse();
                continue;
            };
            let Ok(slot) = self.slots.clone().try_acquire_owned() else {
                incoming.refuse();
                continue;
            };
            tokio::spawn(self.clone().quic_connection(incoming, per_address, slot));
        }
    }

    async fn quic_connection(
        self: Arc<Self>,
        incoming: quinn::Incoming,
        _per_address: AddressSlot,
        slot: OwnedSemaphorePermit,
    ) {
        // Unauthenticated time is bounded as for a WebSocket's request.
        let Ok(Some((group_id, session, first))) = timeout(gwire::JOIN_TIMEOUT, open(incoming)).await else { return };
        let outbox = self.group_outbox();
        let link = QuicLink::start(&session, first);
        let writer = tokio::spawn(write(session, outbox.clone(), self.stop.subscribe()));
        self.group_connection(group_id, link, outbox, writer, slot).await;
    }
}

/// Completes the handshake and finds the group: from the CONNECT request
/// for WebTransport, from the first message on the command stream for raw
/// QUIC (which then carries on as that stream).
async fn open(incoming: quinn::Incoming) -> Option<(RoomId, Session, Option<(RecvStream, Deframer)>)> {
    let conn = incoming.accept().ok()?.await.ok()?;
    let alpn = conn.handshake_data()?.downcast::<quinn::crypto::rustls::HandshakeData>().ok()?.protocol?;
    if alpn == web_transport_quinn::ALPN.as_bytes() {
        let request = web_transport_quinn::Request::accept(conn).await.ok()?;
        let Some(group) = relay::group_from_path(request.url.path()) else {
            let _ = request.reject(web_transport_quinn::http::StatusCode::NOT_FOUND).await;
            return None;
        };
        return Some((group, request.ok().await.ok()?, None));
    }
    let session = Session::raw(conn);
    let mut stream = session.accept_uni().await.ok()?;
    let mut header = [0];
    stream.read_exact(&mut header).await.ok()?;
    if transport::stream_class(header[0]) != Some(Class::Command) {
        session.close(close::PROTOCOL_ERROR.into(), b"");
        return None;
    }
    let mut deframer = Deframer::default();
    let path = loop {
        match deframer.message() {
            Ok(Some(path)) => break std::str::from_utf8(path).ok().and_then(relay::group_from_path),
            Ok(None) => {}
            Err(_) => break None,
        }
        let chunk = stream.read_chunk(4 << 10, true).await.ok()??;
        deframer.push(&chunk.bytes);
    };
    match path {
        Some(group) => Some((group, session, Some((stream, deframer)))),
        None => {
            session.close(close::NOT_FOUND.into(), b"");
            None
        }
    }
}

/// A member's streams and datagrams, read by tasks of their own into one
/// queue per class, taken most urgent first.
struct QuicLink {
    classes: [mpsc::Receiver<Received>; Class::COUNT],
    session: Session,
    tasks: Vec<JoinHandle<()>>,
}

impl QuicLink {
    fn start(session: &Session, first: Option<(RecvStream, Deframer)>) -> QuicLink {
        let (senders, classes): (Vec<_>, Vec<_>) = (0..Class::COUNT).map(|_| mpsc::channel(READ_AHEAD)).unzip();
        let senders: [mpsc::Sender<Received>; Class::COUNT] = senders.try_into().unwrap();
        let mut tasks = Vec::new();
        if let Some((stream, deframer)) = first {
            tasks.push(tokio::spawn(read_stream(stream, Some(Class::Command), deframer, senders.clone())));
        }
        tasks.push(tokio::spawn(accept_streams(session.clone(), senders.clone())));
        tasks.push(tokio::spawn(read_datagrams(session.clone(), senders.clone())));
        QuicLink { classes: classes.try_into().unwrap(), session: session.clone(), tasks }
    }
}

impl Drop for QuicLink {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Link for QuicLink {
    async fn recv(&mut self) -> Received {
        // Most urgent first (Class::PRIORITY).
        let [command, voice, chat, bulk, video] = &mut self.classes;
        tokio::select! {
            biased;
            Some(m) = command.recv() => m,
            Some(m) = voice.recv() => m,
            Some(m) = chat.recv() => m,
            Some(m) = video.recv() => m,
            Some(m) = bulk.recv() => m,
            _ = self.session.closed() => Received::Closed,
        }
    }
}

async fn accept_streams(session: Session, senders: [mpsc::Sender<Received>; Class::COUNT]) {
    while let Ok(stream) = session.accept_uni().await {
        tokio::spawn(read_stream(stream, None, Deframer::default(), senders.clone()));
    }
}

/// Reads one stream's messages into its class's queue, waiting for room:
/// that is what holds a sender back.
pub(crate) async fn read_stream(
    mut stream: RecvStream,
    class: Option<Class>,
    mut deframer: Deframer,
    senders: [mpsc::Sender<Received>; Class::COUNT],
) {
    let class = match class {
        Some(class) => class,
        None => {
            let mut header = [0];
            if stream.read_exact(&mut header).await.is_err() {
                return;
            }
            match transport::stream_class(header[0]) {
                Some(class) => class,
                None => {
                    let _ = senders[0].send(Received::Invalid).await;
                    return;
                }
            }
        }
    };
    let tx = &senders[class.index()];
    loop {
        loop {
            let received = match deframer.message() {
                Ok(Some([])) => Received::Text(gwire::PING.to_owned()),
                Ok(Some(frame)) => Received::Binary(bytes::Bytes::copy_from_slice(frame)),
                Ok(None) => break,
                Err(_) => Received::TooBig,
            };
            let stop = matches!(received, Received::TooBig);
            if tx.send(received).await.is_err() || stop {
                return;
            }
        }
        match stream.read_chunk(64 << 10, true).await {
            Ok(Some(chunk)) => deframer.push(&chunk.bytes),
            _ => return, // finished, or the connection is gone
        }
    }
}

async fn read_datagrams(session: Session, senders: [mpsc::Sender<Received>; Class::COUNT]) {
    while let Ok(datagram) = session.read_datagram().await {
        // By the frame's class (voice or video); a busy relay drops them
        // rather than queue them.
        let class = datagram.first().map_or(Class::Voice, |&t| Class::of(t));
        let _ = senders[class.index()].try_send(Received::Binary(datagram));
    }
}

/// Drains the outbox, a writer per class, until told to close; then closes
/// the connection with the code.
async fn write(session: Session, outbox: Arc<Outbox>, mut stop: watch::Receiver<bool>) {
    let mut close = outbox.close.subscribe();
    let writers: Vec<JoinHandle<()>> =
        Class::ALL.iter().map(|&class| tokio::spawn(write_class(session.clone(), outbox.clone(), class))).collect();
    let code = tokio::select! {
        biased;
        _ = close.changed() => close.borrow().unwrap_or(close::NORMAL),
        _ = stop.wait_for(|stop| *stop) => close::GOING_AWAY,
        _ = session.closed() => close::NORMAL,
    };
    for writer in writers {
        writer.abort();
    }
    session.close(code.into(), b"");
}

/// Sends one class: voice and video as datagrams where they fit, the rest
/// on the class's stream, opened when first needed. Video goes into the
/// datagram buffer only while that leaves room for voice, and otherwise
/// waits in the outbox, which drops its oldest past the budget.
pub(crate) async fn write_class(session: Session, outbox: Arc<Outbox>, class: Class) {
    let mut stream: Option<SendStream> = None;
    let mut buf = Vec::new();
    loop {
        let batch = if class == Class::Video {
            // A peer without datagrams gets it all on the video stream.
            let datagrams = quinn::Connection::max_datagram_size(&session).is_some();
            let space = if datagrams { session.datagram_send_buffer_space() } else { usize::MAX };
            let room = space.saturating_sub(VOICE_RESERVE);
            let batch = outbox.take_within(class, room);
            if batch.is_empty() && outbox.has(class) {
                tokio::time::sleep(VIDEO_RETRY).await;
                continue;
            }
            batch
        } else {
            outbox.take_class(class)
        };
        if batch.is_empty() {
            outbox.ready(class).await;
            continue;
        }
        buf.clear();
        for frame in batch {
            if class.is_realtime() && !frame.is_empty() && session.send_datagram(frame.clone()).is_ok() {
                continue;
            }
            transport::encode(&frame, &mut buf);
        }
        if buf.is_empty() {
            continue;
        }
        if stream.is_none() {
            let Ok(opened) = session.open_uni().await else { return };
            let _ = opened.set_priority(i32::from(class.urgency()));
            buf.insert(0, transport::stream_header(class));
            stream = Some(opened);
        }
        if stream.as_mut().unwrap().write_all(&buf).await.is_err() {
            return;
        }
    }
}
