//! A group connection over raw QUIC (GROUP.md §3.6): a stream per class,
//! each with a writer task, voice as datagrams, and one reader per stream.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::group::transport::{self, Deframer, QuicTarget};
use crate::group::wire::{self, Class};

/// Room in the datagram buffer that video leaves for voice, which goes
/// out in the order queued: so voice waits behind little video.
const VOICE_RESERVE: usize = 4 << 10;
/// Video waiting beyond which the oldest is dropped: room for the largest
/// frame (a 1080p screen's keyframe) passing through.
const VIDEO_BACKLOG: usize = 1 << 20;
/// How often video waiting for room in the datagram buffer looks again.
const VIDEO_RETRY: Duration = Duration::from_millis(2);

/// What arrived, or that the connection ended (with the relay's close code).
#[derive(Debug)]
pub(crate) enum Incoming {
    Binary(Bytes),
    Text(&'static str),
    Closed(Option<u16>),
}

/// The transport parameters: as the relay's.
fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.max_concurrent_uni_streams(16u32.into())
        .max_concurrent_bidi_streams(0u32.into())
        .max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()))
        .stream_receive_window((1u32 << 20).into())
        .receive_window((4u32 << 20).into())
        .send_window(1 << 20)
        // Room for a keyframe's burst of datagrams; a send buffer small
        // enough that voice behind video waits little (16 KiB at 5 Mbit/s:
        // 26 ms), large enough for 1080p.
        .datagram_receive_buffer_size(Some(512 << 10))
        .datagram_send_buffer_size(16 << 10);
    t
}

pub(crate) struct QuicSocket {
    conn: quinn::Connection,
    endpoint: quinn::Endpoint,
    queues: Arc<Queues>,
    incoming: mpsc::UnboundedReceiver<Incoming>,
    writers: Vec<JoinHandle<()>>,
    readers: Vec<JoinHandle<()>>,
}

impl QuicSocket {
    /// Connects, and asks for the group at `target.path`.
    pub(crate) async fn connect(target: &QuicTarget) -> Option<QuicSocket> {
        let address = resolve(target).await?;
        let bind: SocketAddr = if address.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" }.parse().unwrap();
        let mut endpoint = quinn::Endpoint::client(bind).ok()?;
        endpoint.set_default_client_config(client_config(target.verify));
        let conn = endpoint.connect(address, &target.host).ok()?.await.ok()?;

        let queues = Arc::new(Queues::default());
        // The path goes first on the command stream.
        let mut path = Vec::new();
        transport::encode(target.path.as_bytes(), &mut path);
        queues.push(Class::Command, Bytes::from(path));
        let writers = Class::ALL
            .iter()
            .map(|&class| match class {
                Class::Video => tokio::spawn(write_video(conn.clone(), queues.clone())),
                _ => tokio::spawn(write_class(conn.clone(), queues.clone(), class)),
            })
            .collect();
        let (tx, incoming) = mpsc::unbounded_channel();
        let readers = vec![
            tokio::spawn(accept_streams(conn.clone(), tx.clone())),
            tokio::spawn(read_datagrams(conn.clone(), tx.clone())),
            tokio::spawn(watch_close(conn.clone(), tx)),
        ];
        Some(QuicSocket { conn, endpoint, queues, incoming, writers, readers })
    }

    /// Queues a frame; a voice frame goes out at once as a datagram, where
    /// it fits, and video as soon as there is room for it (see [`write_video`]).
    pub(crate) fn send(&self, class: Class, frame: Vec<u8>) {
        if class == Class::Voice && frame.len() <= self.conn.max_datagram_size().unwrap_or(0) {
            // Dropped if the connection is backed up: late audio is worse than lost.
            let _ = self.conn.send_datagram(Bytes::from(frame));
            return;
        }
        if class == Class::Video {
            return self.queues.push_video(Bytes::from(frame));
        }
        let mut message = Vec::with_capacity(frame.len() + 2);
        transport::encode(&frame, &mut message);
        self.queues.push(class, Bytes::from(message));
    }

    /// The empty message, which the relay answers with an empty one.
    pub(crate) fn ping(&self) {
        self.queues.push(Class::Command, Bytes::from_static(&[0, 0]));
    }

    pub(crate) async fn recv(&mut self) -> Incoming {
        self.incoming.recv().await.unwrap_or(Incoming::Closed(None))
    }

    /// Closes once what is queued has been delivered, or after `grace`.
    pub(crate) fn close(self, grace: Duration) {
        let QuicSocket { conn, endpoint, queues, writers, readers, .. } = self;
        queues.inner.lock().unwrap().closing = true;
        for ready in &queues.ready {
            ready.notify_one();
        }
        tokio::spawn(async move {
            let flushed = async {
                for writer in writers {
                    let _ = writer.await;
                }
            };
            let _ = timeout(grace, flushed).await;
            conn.close(transport::close::NORMAL.into(), b"");
            for reader in readers {
                reader.abort();
            }
            let _ = timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
        });
    }
}

async fn resolve(target: &QuicTarget) -> Option<SocketAddr> {
    if let Ok(ip) = target.host.parse::<std::net::IpAddr>() {
        return Some(SocketAddr::new(ip, target.port));
    }
    // IPv4 first: UDP over IPv6 is the less dependable of the two on many
    // networks, and the WebSocket fallback tries every address anyway.
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((target.host.as_str(), target.port)).await.ok()?.collect();
    addresses.iter().find(|a| a.is_ipv4()).or(addresses.first()).copied()
}

fn client_config(verify: bool) -> quinn::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3");
    let mut tls = if verify {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder.dangerous().with_custom_certificate_verifier(Arc::new(AnyCertificate(provider))).with_no_client_auth()
    };
    tls.alpn_protocols = vec![transport::ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("a QUIC-capable TLS configuration");
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()));
    config
}

/// Accepts whatever certificate a `quic://` relay presents, still checking
/// that the relay holds its key. Members do not rely on the relay for
/// confidentiality or authenticity (GROUP.md §9).
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Encoded messages waiting, per class.
/// Video is kept as frames, not yet encoded: most go out as datagrams.
#[derive(Default)]
struct Queues {
    inner: Mutex<Pending>,
    ready: [Notify; Class::COUNT],
}

#[derive(Default)]
struct Pending {
    classes: [VecDeque<Bytes>; Class::COUNT],
    video_bytes: usize,
    closing: bool,
}

impl Queues {
    fn push(&self, class: Class, message: Bytes) {
        self.inner.lock().unwrap().classes[class.index()].push_back(message);
        self.ready[class.index()].notify_one();
    }

    /// Queues a video frame, dropping the oldest past [`VIDEO_BACKLOG`].
    fn push_video(&self, frame: Bytes) {
        let mut q = self.inner.lock().unwrap();
        let video = Class::Video.index();
        while q.video_bytes + frame.len() > VIDEO_BACKLOG {
            let Some(old) = q.classes[video].pop_front() else { break };
            q.video_bytes -= old.len();
        }
        q.video_bytes += frame.len();
        q.classes[video].push_back(frame);
        drop(q);
        self.ready[video].notify_one();
    }

    /// Video frames from the front adding up to at most `room` bytes, and
    /// whether any are left; `None` once closing.
    fn take_video(&self, room: usize) -> Option<(Vec<Bytes>, bool)> {
        let mut q = self.inner.lock().unwrap();
        if q.closing {
            return None;
        }
        let video = Class::Video.index();
        let (mut batch, mut taken) = (Vec::new(), 0);
        while let Some(front) = q.classes[video].front() {
            if taken + front.len() > room {
                break;
            }
            let frame = q.classes[video].pop_front().unwrap();
            q.video_bytes -= frame.len();
            taken += frame.len();
            batch.push(frame);
        }
        let left = !q.classes[video].is_empty();
        Some((batch, left))
    }

    /// Everything queued for `class`; `None` once closing and drained.
    fn take(&self, class: Class) -> Option<Vec<Bytes>> {
        let mut q = self.inner.lock().unwrap();
        let taken: Vec<Bytes> = q.classes[class.index()].drain(..).collect();
        (!taken.is_empty() || !q.closing).then_some(taken)
    }
}

/// Writes one class's stream, opened when first needed; on closing, waits
/// until the relay has it all.
async fn write_class(conn: quinn::Connection, queues: Arc<Queues>, class: Class) {
    let mut stream: Option<quinn::SendStream> = None;
    while let Some(mut batch) = queues.take(class) {
        if batch.is_empty() {
            queues.ready[class.index()].notified().await;
            continue;
        }
        if stream.is_none() {
            let Ok(opened) = conn.open_uni().await else { return };
            let _ = opened.set_priority(i32::from(class.urgency()));
            batch.insert(0, Bytes::from(vec![transport::stream_header(class)]));
            stream = Some(opened);
        }
        if stream.as_mut().unwrap().write_all_chunks(&mut batch).await.is_err() {
            return;
        }
    }
    if let Some(mut stream) = stream
        && stream.finish().is_ok()
    {
        let _ = stream.stopped().await;
    }
}

/// Sends video as datagrams while the datagram buffer leaves room for
/// voice, frames too big for one on the video stream; frames that find no
/// room wait (the queue drops the oldest).
async fn write_video(conn: quinn::Connection, queues: Arc<Queues>) {
    let mut stream: Option<quinn::SendStream> = None;
    loop {
        let max = conn.max_datagram_size();
        let room = match max {
            Some(_) => conn.datagram_send_buffer_space().saturating_sub(VOICE_RESERVE),
            None => usize::MAX,
        };
        let Some((batch, left)) = queues.take_video(room) else { return };
        if batch.is_empty() {
            if left {
                tokio::time::sleep(VIDEO_RETRY).await;
            } else {
                queues.ready[Class::Video.index()].notified().await;
            }
            continue;
        }
        let mut message = Vec::new();
        for frame in batch {
            if max.is_some_and(|max| frame.len() <= max) && conn.send_datagram(frame.clone()).is_ok() {
                continue;
            }
            transport::encode(&frame, &mut message);
        }
        if message.is_empty() {
            continue;
        }
        if stream.is_none() {
            let Ok(opened) = conn.open_uni().await else { return };
            let _ = opened.set_priority(i32::from(Class::Video.urgency()));
            message.insert(0, transport::stream_header(Class::Video));
            stream = Some(opened);
        }
        if stream.as_mut().unwrap().write_all(&message).await.is_err() {
            return;
        }
    }
}

async fn accept_streams(conn: quinn::Connection, tx: mpsc::UnboundedSender<Incoming>) {
    while let Ok(stream) = conn.accept_uni().await {
        tokio::spawn(read_stream(stream, tx.clone()));
    }
}

async fn read_stream(mut stream: quinn::RecvStream, tx: mpsc::UnboundedSender<Incoming>) {
    let mut header = [0];
    if stream.read_exact(&mut header).await.is_err() || transport::stream_class(header[0]).is_none() {
        return;
    }
    let mut deframer = Deframer::default();
    while let Ok(Some(chunk)) = stream.read_chunk(64 << 10, true).await {
        deframer.push(&chunk.bytes);
        loop {
            let incoming = match deframer.message() {
                Ok(Some([])) => Incoming::Text(wire::PONG),
                Ok(Some(frame)) => Incoming::Binary(Bytes::copy_from_slice(frame)),
                Ok(None) => break,
                Err(_) => return,
            };
            if tx.send(incoming).is_err() {
                return;
            }
        }
    }
}

async fn read_datagrams(conn: quinn::Connection, tx: mpsc::UnboundedSender<Incoming>) {
    while let Ok(datagram) = conn.read_datagram().await {
        if tx.send(Incoming::Binary(datagram)).is_err() {
            return;
        }
    }
}

async fn watch_close(conn: quinn::Connection, tx: mpsc::UnboundedSender<Incoming>) {
    let code = match conn.closed().await {
        quinn::ConnectionError::ApplicationClosed(close) => u16::try_from(close.error_code.into_inner()).ok(),
        _ => None,
    };
    let _ = tx.send(Incoming::Closed(code));
}
