//! What the group tests share: an in-process relay, the relay protocol
//! spoken by hand over raw QUIC or WebTransport, members through the Tokio
//! client, and a network path that loses, blocks or moves packets.

#![allow(dead_code)] // each test target uses a part of it

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peerconnect::client::group::{self, GroupConnection};
use peerconnect::group::transport::{self, Deframer};
use peerconnect::group::{Class, Config, Event, Group, Identity, Member, Output, wire};
use rustls::pki_types::CertificateDer;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use web_transport_quinn::{SendStream, Session};

// The relay.

pub struct LocalRelay {
    pub addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl LocalRelay {
    /// A relay on TCP and UDP ports of the same number, or TCP only.
    pub async fn start(config: peerconnect_relay::Config, quic: bool) -> LocalRelay {
        LocalRelay::bind("127.0.0.1:0", config, quic).await
    }

    /// The same, on `addr` (port 0 for any).
    pub async fn bind(addr: &str, config: peerconnect_relay::Config, quic: bool) -> LocalRelay {
        let mut tries = 0;
        let (listener, udp) = loop {
            let listener = TcpListener::bind(addr).await.unwrap();
            if !quic {
                break (listener, None);
            }
            match std::net::UdpSocket::bind(listener.local_addr().unwrap()) {
                Ok(udp) => break (listener, Some(udp)),
                Err(_) if addr.ends_with(":0") => continue,
                // A relay stopped a moment ago may not have let go of it yet.
                Err(_) if tries < 50 => {
                    drop(listener);
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(err) => panic!("{err}"),
            }
        };
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            peerconnect_relay::serve_all(listener, udp, config, async {
                let _ = stopped.await;
            })
            .await
            .unwrap()
        });
        LocalRelay { addr, stop: Some(stop), task: Some(task) }
    }

    pub fn url(&self, scheme: &str) -> String {
        format!("{scheme}://{}", self.addr)
    }

    /// Stops the relay, and waits until it has.
    pub async fn shutdown(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.take().unwrap().await.unwrap();
    }
}

impl Drop for LocalRelay {
    fn drop(&mut self) {
        let _ = self.stop.take().map(|stop| stop.send(()));
        let _ = self.task.take();
    }
}

// Raw sessions: the relay protocol by hand, over raw QUIC or WebTransport.

/// Accepts any certificate (raw QUIC members do not verify the relay).
#[derive(Debug)]
pub struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

pub fn client_config(alpn: &[u8], roots: Option<rustls::RootCertStore>) -> quinn::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap();
    let mut tls = match roots {
        Some(roots) => builder.with_root_certificates(roots).with_no_client_auth(),
        None => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
            .with_no_client_auth(),
    };
    tls.alpn_protocols = vec![alpn.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(peerconnect_relay::quic::transport_config()));
    config
}

#[derive(Debug, PartialEq)]
pub enum Got {
    Frame(Class, Vec<u8>),
    Datagram(Vec<u8>),
    Pong,
    Closed(Option<u16>),
}

/// One QUIC session to the relay, spoken by hand.
pub struct Wire {
    pub session: Session,
    streams: [Option<SendStream>; Class::COUNT],
    got: mpsc::UnboundedReceiver<Got>,
    /// Whether to read what arrives; while not, QUIC flow control holds the
    /// relay back, as it would for a member that stopped reading.
    reading: watch::Sender<bool>,
    /// The DEMANDs the relay sent (GROUP.md §3.3), kept apart from the rest.
    pub demands: Vec<u8>,
    _endpoint: quinn::Endpoint,
}

impl Wire {
    /// Raw QUIC to the endpoint `url` (`quic://` or `https://`, its
    /// certificate not verified), asking for its path first.
    pub async fn raw(url: &str) -> Wire {
        let target = transport::target(url).expect("a QUIC URL");
        let addr = tokio::net::lookup_host((target.host.as_str(), target.port)).await.unwrap().next().unwrap();
        let bind = if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
        let endpoint = quinn::Endpoint::client(bind.parse().unwrap()).unwrap();
        let conn =
            endpoint.connect_with(client_config(transport::ALPN, None), addr, &target.host).unwrap().await.unwrap();
        let mut wire = Wire::start(Session::raw(conn), endpoint);
        wire.send(Class::Command, target.path.as_bytes()).await;
        wire
    }

    /// A WebTransport session to `url` (on 127.0.0.1, whatever its host),
    /// verifying the relay against `roots` if given.
    pub async fn webtransport(url: &str, roots: Option<rustls::RootCertStore>) -> Result<Wire, String> {
        let parsed = url::Url::parse(url).unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], parsed.port().unwrap()));
        let host = parsed.host_str().unwrap().to_owned();
        let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let conn = endpoint
            .connect_with(client_config(b"h3", roots), addr, &host)
            .unwrap()
            .await
            .map_err(|e| e.to_string())?;
        let session = Session::connect(conn, parsed).await.map_err(|e| e.to_string())?;
        Ok(Wire::start(session, endpoint))
    }

    fn start(session: Session, endpoint: quinn::Endpoint) -> Wire {
        let (tx, got) = mpsc::unbounded_channel();
        let reading = watch::Sender::new(true);
        let (s, t, gate) = (session.clone(), tx.clone(), reading.subscribe());
        tokio::spawn(async move {
            while let Ok(mut stream) = s.accept_uni().await {
                let (t, mut gate) = (t.clone(), gate.clone());
                tokio::spawn(async move {
                    let mut header = [0];
                    if !may_read(&mut gate).await || stream.read_exact(&mut header).await.is_err() {
                        return;
                    }
                    let class = transport::stream_class(header[0]).expect("a class");
                    let mut d = Deframer::default();
                    while may_read(&mut gate).await
                        && let Ok(Some(chunk)) = stream.read_chunk(1 << 16, true).await
                    {
                        d.push(&chunk.bytes);
                        while let Some(m) = d.message().unwrap() {
                            let _ = t.send(if m.is_empty() { Got::Pong } else { Got::Frame(class, m.to_vec()) });
                        }
                    }
                });
            }
        });
        let (s, t, mut gate) = (session.clone(), tx.clone(), reading.subscribe());
        tokio::spawn(async move {
            while may_read(&mut gate).await
                && let Ok(d) = s.read_datagram().await
            {
                let _ = t.send(Got::Datagram(d.to_vec()));
            }
        });
        let s = session.clone();
        tokio::spawn(async move {
            let _ = tx.send(Got::Closed(close_code(&s.closed().await)));
        });
        Wire { session, streams: Default::default(), got, reading, demands: Vec::new(), _endpoint: endpoint }
    }

    /// Stops reading, or reads again.
    pub fn read(&self, reading: bool) {
        self.reading.send_replace(reading);
    }

    pub async fn send(&mut self, class: Class, message: &[u8]) {
        let mut bytes = Vec::new();
        transport::encode(message, &mut bytes);
        self.write(class, &bytes).await;
    }

    /// Writes `bytes` as they are to the class's stream.
    pub async fn write(&mut self, class: Class, bytes: &[u8]) {
        let stream = &mut self.streams[class.index()];
        if stream.is_none() {
            let Ok(mut opened) = self.session.open_uni().await else { return };
            let _ = opened.write_all(&[transport::stream_header(class)]).await;
            *stream = Some(opened);
        }
        let _ = stream.as_mut().unwrap().write_all(bytes).await;
    }

    pub fn datagram(&self, frame: &[u8]) {
        self.session.send_datagram(frame.to_vec().into()).unwrap();
    }

    /// What arrives next; `None` once all of it has been taken.
    pub async fn recv(&mut self) -> Option<Got> {
        self.got.recv().await
    }

    pub async fn next(&mut self) -> Got {
        loop {
            // Longer than the join timeout, for tests that wait it out.
            let got = timeout(Duration::from_secs(20), self.got.recv()).await.expect("nothing arrived");
            match got.unwrap_or(Got::Closed(None)) {
                Got::Frame(Class::Command, f) if f.len() == 2 && f[0] == wire::DEMAND => self.demands.push(f[1]),
                got => return got,
            }
        }
    }

    /// The next frame, from a stream or a datagram.
    pub async fn frame(&mut self) -> Vec<u8> {
        loop {
            match self.next().await {
                Got::Frame(_, f) | Got::Datagram(f) => return f,
                Got::Pong => {}
                Got::Closed(code) => panic!("closed with {code:?}"),
            }
        }
    }

    /// Waits for the end of the session; returns its close code.
    pub async fn closed(&mut self) -> Option<u16> {
        loop {
            if let Got::Closed(code) = self.next().await {
                return code;
            }
        }
    }

    /// HELLO, JOIN, WELCOME: returns the slot and the bitmap.
    pub async fn join(&mut self, group: &Group, identity: &Identity) -> (u8, u32) {
        let hello = self.frame().await;
        assert_eq!(hello[0], wire::HELLO);
        self.send(Class::Command, &join_frame(group, identity, &hello)).await;
        let welcome = self.frame().await;
        assert_eq!(welcome.len(), wire::WELCOME_LEN);
        assert_eq!(welcome[0], wire::WELCOME, "{welcome:?}");
        (welcome[1], u32::from_be_bytes(welcome[2..6].try_into().unwrap()))
    }
}

/// Waits until reading is on; false once the [`Wire`] is gone.
async fn may_read(gate: &mut watch::Receiver<bool>) -> bool {
    gate.wait_for(|r| *r).await.is_ok()
}

/// The application close code, from raw QUIC or WebTransport.
pub fn close_code(err: &web_transport_quinn::SessionError) -> Option<u16> {
    use web_transport_quinn::{SessionError, WebTransportError};
    let code = match err {
        SessionError::WebTransportError(WebTransportError::Closed(code, _)) => u64::from(*code),
        SessionError::ConnectionError(quinn::ConnectionError::ApplicationClosed(c)) => c.error_code.into_inner(),
        _ => return None,
    };
    u16::try_from(code).ok()
}

/// The JOIN a member sends for this HELLO, without going through a driver.
pub fn join_frame(group: &Group, identity: &Identity, hello: &[u8]) -> Vec<u8> {
    let mut m = Member::new(group.clone(), Identity::from_seed(*identity.seed()), Config::default(), 0).unwrap();
    m.poll_output();
    m.handle_open(0);
    m.handle_binary(0, hello);
    match m.poll_output() {
        Some(Output::Send(frame, _)) => frame,
        other => panic!("{other:?}"),
    }
}

// Members through the Tokio client.

pub fn join(group: &Group, name: &str) -> GroupConnection {
    let config = Config { profile: name.as_bytes().to_vec(), ..Config::default() };
    group::join(group.clone(), Identity::generate(), config).unwrap()
}

pub async fn wait(conn: &mut GroupConnection, what: impl Fn(&Event) -> bool) -> Event {
    wait_up_to(conn, Duration::from_secs(20), what).await
}

pub async fn wait_up_to(conn: &mut GroupConnection, limit: Duration, what: impl Fn(&Event) -> bool) -> Event {
    timeout(limit, async {
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
pub async fn admitted(conn: &mut GroupConnection, count: usize) {
    let mut seen = HashSet::new();
    while seen.len() < count {
        match wait(conn, |e| matches!(e, Event::Joined { .. } | Event::Left { .. })).await {
            Event::Joined { member, .. } => seen.insert(member),
            Event::Left { member } => seen.remove(&member),
            _ => unreachable!(),
        };
    }
    assert!(conn.connected());
}

/// The events of the next `period`.
pub async fn events_for(conn: &mut GroupConnection, period: Duration) -> Vec<Event> {
    let mut seen = Vec::new();
    let _ = timeout(period, async {
        while let Some(e) = conn.next().await {
            seen.push(e);
        }
    })
    .await;
    seen
}

// A network between member and relay, for one member.

/// A UDP proxy that drops a share of the datagrams each way, delays them,
/// can drop everything, can move to a new address towards the relay, as a
/// NAT does, and can cut a connection off without a goodbye.
pub struct Path {
    pub addr: SocketAddr,
    pub loss_per_mille: Arc<AtomicU32>,
    pub blocked: Arc<AtomicBool>,
    pub rebind: mpsc::UnboundedSender<()>,
    client: Arc<Mutex<Option<SocketAddr>>>,
    cut_off: Arc<Mutex<HashSet<SocketAddr>>>,
}

impl Path {
    pub async fn new(relay: SocketAddr, one_way: Duration) -> Path {
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = udp.local_addr().unwrap();
        let loss_per_mille = Arc::new(AtomicU32::new(0));
        let blocked = Arc::new(AtomicBool::new(false));
        let (rebind, mut rebinds) = mpsc::unbounded_channel::<()>();
        let downstream = Arc::new(udp);
        let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
        let cut_off: Arc<Mutex<HashSet<SocketAddr>>> = Arc::default();
        let rng = Arc::new(Mutex::new(0x2545_F491_4F6C_DD1Du64));
        let drop_it = {
            let (loss, blocked, rng) = (loss_per_mille.clone(), blocked.clone(), rng.clone());
            move || {
                if blocked.load(Ordering::Relaxed) {
                    return true;
                }
                let mut x = rng.lock().unwrap();
                *x ^= *x << 13;
                *x ^= *x >> 7;
                *x ^= *x << 17;
                (*x % 1000) < loss.load(Ordering::Relaxed) as u64
            }
        };
        // Delay lines, one per direction, in order.
        let line = |socket: Arc<UdpSocket>| {
            let (tx, mut rx) = mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>, SocketAddr)>();
            tokio::spawn(async move {
                while let Some((at, packet, to)) = rx.recv().await {
                    tokio::time::sleep_until(at).await;
                    let _ = socket.send_to(&packet, to).await;
                }
            });
            tx
        };
        let to_client = line(downstream.clone());
        let upstream = Arc::new(Mutex::new(Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap())));
        let to_relay: Arc<Mutex<mpsc::UnboundedSender<_>>> =
            Arc::new(Mutex::new(line(upstream.lock().unwrap().clone())));
        // Relay → member, from whichever upstream socket is current.
        let read_upstream = {
            let (client, to_client, drop_it) = (client.clone(), to_client.clone(), drop_it.clone());
            move |socket: Arc<UdpSocket>| {
                let (client, to_client, drop_it) = (client.clone(), to_client.clone(), drop_it.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0; 65536];
                    while let Ok((n, _)) = socket.recv_from(&mut buf).await {
                        let Some(to) = *client.lock().unwrap() else { continue };
                        if !drop_it() {
                            let _ = to_client.send((tokio::time::Instant::now() + one_way, buf[..n].to_vec(), to));
                        }
                    }
                })
            }
        };
        let mut reader = read_upstream(upstream.lock().unwrap().clone());
        {
            let (upstream, to_relay) = (upstream.clone(), to_relay.clone());
            tokio::spawn(async move {
                while rebinds.recv().await.is_some() {
                    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
                    reader.abort();
                    reader = read_upstream(socket.clone());
                    *to_relay.lock().unwrap() = line(socket.clone());
                    *upstream.lock().unwrap() = socket;
                }
            });
        }
        // Member → relay.
        {
            let (client, cut_off) = (client.clone(), cut_off.clone());
            tokio::spawn(async move {
                let mut buf = vec![0; 65536];
                while let Ok((n, from)) = downstream.recv_from(&mut buf).await {
                    if cut_off.lock().unwrap().contains(&from) {
                        continue;
                    }
                    *client.lock().unwrap() = Some(from);
                    if !drop_it() {
                        let _ = to_relay.lock().unwrap().send((
                            tokio::time::Instant::now() + one_way,
                            buf[..n].to_vec(),
                            relay,
                        ));
                    }
                }
            });
        }
        Path { addr, loss_per_mille, blocked, rebind, client, cut_off }
    }

    pub fn url(&self, scheme: &str) -> String {
        format!("{scheme}://{}", self.addr)
    }

    /// Ends the member's connection without a word to either side: its
    /// packets go nowhere from now on, each way. A new connection, from
    /// another port, gets through.
    pub fn cut(&self) {
        if let Some(client) = self.client.lock().unwrap().take() {
            self.cut_off.lock().unwrap().insert(client);
        }
        let _ = self.rebind.send(());
    }
}
