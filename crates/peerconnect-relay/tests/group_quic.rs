//! Groups over QUIC end to end (GROUP.md §3.6): raw QUIC and WebTransport
//! against the in-process relay, mixed with WebSockets in one group; lost
//! packets, a change of address, UDP that does not get through, and
//! certificates.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use peerconnect::client::group::{self, GroupConnection, Via};
use peerconnect::group::transport::{self, Deframer, Transport};
use peerconnect::group::{Channel, Class, Config, Event, Group, Identity, Member, MemberId, Output, State, wire};
use peerconnect::wire::close;
use rustls::pki_types::CertificateDer;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use web_transport_quinn::{SendStream, Session};

// The relay.

struct Relay {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Relay {
    /// A relay on TCP and UDP ports of the same number, or TCP only.
    async fn start(config: peerconnect_relay::Config, quic: bool) -> Relay {
        let (listener, udp) = loop {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            if !quic {
                break (listener, None);
            }
            if let Ok(udp) = std::net::UdpSocket::bind(listener.local_addr().unwrap()) {
                break (listener, Some(udp));
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
        Relay { addr, stop: Some(stop), task: Some(task) }
    }

    fn url(&self, scheme: &str) -> String {
        format!("{scheme}://{}", self.addr)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.stop.take().map(|stop| stop.send(()));
        let _ = self.task.take();
    }
}

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

// Raw sessions: the relay protocol by hand, over raw QUIC or WebTransport.

/// Accepts any certificate (raw QUIC members do not verify the relay).
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

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

fn client_config(alpn: &[u8], roots: Option<rustls::RootCertStore>) -> quinn::ClientConfig {
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
enum Got {
    Frame(Class, Vec<u8>),
    Datagram(Vec<u8>),
    Pong,
    Closed(Option<u32>),
}

/// One QUIC session to the relay, spoken by hand.
struct Wire {
    session: Session,
    streams: [Option<SendStream>; 4],
    got: mpsc::UnboundedReceiver<Got>,
    _endpoint: quinn::Endpoint,
}

impl Wire {
    /// Raw QUIC to `addr`, asking for `path` first.
    async fn raw(addr: SocketAddr, path: &str) -> Wire {
        let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let conn =
            endpoint.connect_with(client_config(transport::ALPN, None), addr, "anything").unwrap().await.unwrap();
        let mut wire = Wire::start(Session::raw(conn), endpoint);
        wire.send(Class::Command, path.as_bytes()).await;
        wire
    }

    /// A WebTransport session to `url` (on 127.0.0.1, whatever its host),
    /// verifying the relay against `roots` if given.
    async fn webtransport(url: &str, roots: Option<rustls::RootCertStore>) -> Result<Wire, String> {
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
        let (s, t) = (session.clone(), tx.clone());
        tokio::spawn(async move {
            while let Ok(mut stream) = s.accept_uni().await {
                let t = t.clone();
                tokio::spawn(async move {
                    let mut header = [0];
                    if stream.read_exact(&mut header).await.is_err() {
                        return;
                    }
                    let class = transport::stream_class(header[0]).expect("a class");
                    let mut d = Deframer::default();
                    while let Ok(Some(chunk)) = stream.read_chunk(1 << 16, true).await {
                        d.push(&chunk.bytes);
                        while let Some(m) = d.message().unwrap() {
                            let _ = t.send(if m.is_empty() { Got::Pong } else { Got::Frame(class, m.to_vec()) });
                        }
                    }
                });
            }
        });
        let (s, t) = (session.clone(), tx.clone());
        tokio::spawn(async move {
            while let Ok(d) = s.read_datagram().await {
                let _ = t.send(Got::Datagram(d.to_vec()));
            }
        });
        let s = session.clone();
        tokio::spawn(async move {
            let _ = tx.send(Got::Closed(close_code(&s.closed().await)));
        });
        Wire { session, streams: Default::default(), got, _endpoint: endpoint }
    }

    async fn send(&mut self, class: Class, message: &[u8]) {
        let mut bytes = Vec::new();
        transport::encode(message, &mut bytes);
        self.write(class, &bytes).await;
    }

    async fn write(&mut self, class: Class, bytes: &[u8]) {
        let stream = &mut self.streams[class.index()];
        if stream.is_none() {
            let mut opened = self.session.open_uni().await.unwrap();
            opened.write_all(&[transport::stream_header(class)]).await.unwrap();
            *stream = Some(opened);
        }
        let _ = stream.as_mut().unwrap().write_all(bytes).await;
    }

    fn datagram(&self, frame: &[u8]) {
        self.session.send_datagram(frame.to_vec().into()).unwrap();
    }

    async fn next(&mut self) -> Got {
        timeout(Duration::from_secs(10), self.got.recv()).await.expect("nothing arrived").unwrap_or(Got::Closed(None))
    }

    /// The next frame, from a stream or a datagram.
    async fn frame(&mut self) -> Vec<u8> {
        loop {
            match self.next().await {
                Got::Frame(_, f) | Got::Datagram(f) => return f,
                Got::Pong => {}
                Got::Closed(code) => panic!("closed with {code:?}"),
            }
        }
    }

    async fn closed(&mut self) -> Option<u32> {
        loop {
            if let Got::Closed(code) = self.next().await {
                return code;
            }
        }
    }

    /// HELLO, JOIN, WELCOME: returns the slot and the bitmap.
    async fn join(&mut self, group: &Group, identity: &Identity) -> (u8, u32) {
        let hello = self.frame().await;
        assert_eq!(hello[0], wire::HELLO);
        self.send(Class::Command, &join_frame(group, identity, &hello)).await;
        let welcome = self.frame().await;
        assert_eq!(welcome[0], wire::WELCOME, "{welcome:?}");
        (welcome[1], u32::from_be_bytes(welcome[2..6].try_into().unwrap()))
    }
}

/// The application close code, from raw QUIC or WebTransport.
fn close_code(err: &web_transport_quinn::SessionError) -> Option<u32> {
    use web_transport_quinn::{SessionError, WebTransportError};
    match err {
        SessionError::WebTransportError(WebTransportError::Closed(code, _)) => Some(*code),
        SessionError::ConnectionError(quinn::ConnectionError::ApplicationClosed(c)) => {
            Some(c.error_code.into_inner() as u32)
        }
        _ => None,
    }
}

/// The JOIN a member sends for this HELLO.
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

fn path(group: &Group) -> String {
    format!("/g1/{}", group.id())
}

// Members through the Tokio client.

fn join(group: &Group, name: &str) -> GroupConnection {
    let config = Config { profile: name.as_bytes().to_vec(), ..Config::default() };
    group::join(group.clone(), Identity::generate(), config).unwrap()
}

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

async fn admitted(conn: &mut GroupConnection, count: usize) {
    let mut seen = std::collections::HashSet::new();
    while seen.len() < count {
        match wait(conn, |e| matches!(e, Event::Joined { .. } | Event::Left { .. })).await {
            Event::Joined { member, .. } => seen.insert(member),
            Event::Left { member } => seen.remove(&member),
            _ => unreachable!(),
        };
    }
}

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
                            let Some(Transport::Quic(target)) = transport::transports(&url).into_iter().next() else {
                                panic!("{url}")
                            };
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
                    got = async { wire.as_mut().unwrap().got.recv().await }, if wire.is_some() => match got {
                        Some(Got::Frame(_, f) | Got::Datagram(f)) => member.handle_binary(now(), &f),
                        Some(Got::Pong) => member.handle_text(now(), wire::PONG),
                        Some(Got::Closed(code)) => { wire = None; member.handle_close(now(), code.map(|c| c as u16)) }
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

// A network between member and relay, for one member.

/// A UDP proxy (and a TCP one on the port of the same number) that drops
/// a share of the datagrams each way, delays them, can drop everything,
/// and can move to a new address towards the relay, as a NAT does.
struct Path {
    addr: SocketAddr,
    loss_per_mille: Arc<AtomicU32>,
    blocked: Arc<AtomicBool>,
    rebind: mpsc::UnboundedSender<()>,
}

impl Path {
    async fn new(relay: SocketAddr, one_way: Duration) -> Path {
        let (tcp, udp) = loop {
            let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
            if let Ok(udp) = UdpSocket::bind(tcp.local_addr().unwrap()).await {
                break (tcp, udp);
            }
        };
        let addr = udp.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = tcp.accept().await {
                tokio::spawn(async move {
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(relay).await else { return };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        let loss_per_mille = Arc::new(AtomicU32::new(0));
        let blocked = Arc::new(AtomicBool::new(false));
        let (rebind, mut rebinds) = mpsc::unbounded_channel::<()>();
        let downstream = Arc::new(udp);
        let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
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
        tokio::spawn(async move {
            let mut buf = vec![0; 65536];
            while let Ok((n, from)) = downstream.recv_from(&mut buf).await {
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
        Path { addr, loss_per_mille, blocked, rebind }
    }

    fn url(&self, scheme: &str) -> String {
        format!("{scheme}://{}", self.addr)
    }
}

// The tests.

#[tokio::test]
async fn raw_quic_and_webtransport_follow_the_relay_protocol() {
    let relay = Relay::start(peerconnect_relay::Config { max_group_members: 3, ..Default::default() }, true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let (ia, ib, ic) = (Identity::generate(), Identity::generate(), Identity::generate());

    // Raw QUIC: the path first, then HELLO and so on, on streams.
    let mut a = Wire::raw(relay.addr, &path(&group)).await;
    assert_eq!(a.join(&group, &ia).await, (0, 0));
    a.send(Class::Command, &[]).await;
    assert_eq!(a.next().await, Got::Pong, "the empty message is ping, and pong");

    // WebTransport: the path in the CONNECT request.
    let mut b = Wire::webtransport(&format!("https://{}{}", relay.addr, path(&group)), None).await.unwrap();
    assert_eq!(b.join(&group, &ib).await, (1, 1));
    assert_eq!(a.frame().await, wire::member(1, true));

    // And a WebSocket in the same group.
    let (mut c, _) = tokio_tungstenite::connect_async(format!("{}{}", relay.url("ws"), path(&group))).await.unwrap();
    let hello = next_ws(&mut c).await;
    c.send(Message::binary(join_frame(&group, &ic, &hello))).await.unwrap();
    assert_eq!(next_ws(&mut c).await, wire::welcome(2, 0b11));
    assert_eq!(a.frame().await, wire::member(2, true));
    assert_eq!(b.frame().await, wire::member(2, true));

    // Each class on its stream; voice as datagrams both ways, and on its
    // stream when sent there; frames arrive verbatim whatever the transport.
    let command = vec![wire::COMMAND, 0, wire::BROADCAST, 1, 2, 3];
    a.send(Class::Command, &command).await;
    assert_eq!(b.next().await, Got::Frame(Class::Command, command.clone()));
    assert_eq!(next_ws(&mut c).await, command);
    let voice = vec![wire::VOICE, 1, wire::BROADCAST, 9, 9, 9];
    b.datagram(&voice);
    assert_eq!(a.next().await, Got::Datagram(voice.clone()));
    assert_eq!(next_ws(&mut c).await, voice);
    let big_voice = [&[wire::VOICE, 2, 0][..], &vec![5; 4000]].concat();
    c.send(Message::binary(big_voice.clone())).await.unwrap();
    assert_eq!(a.next().await, Got::Frame(Class::Voice, big_voice), "too big for a datagram");
    let chat = vec![wire::CHAT, 2, 1, 4];
    c.send(Message::binary(chat.clone())).await.unwrap();
    assert_eq!(b.next().await, Got::Frame(Class::Chat, chat));
    let bulk = [&[wire::BULK, 1, 0][..], &vec![7; wire::MAX_FRAME_LEN - 3]].concat();
    b.send(Class::Bulk, &bulk).await;
    assert_eq!(a.next().await, Got::Frame(Class::Bulk, bulk));

    // Full.
    let mut d = Wire::raw(relay.addr, &path(&group)).await;
    let hello = d.frame().await;
    d.send(Class::Command, &join_frame(&group, &Identity::generate(), &hello)).await;
    assert_eq!(d.closed().await, Some(close::FULL as u32));

    // The same identity again takes its slot, over the other transport.
    let mut a2 = Wire::webtransport(&format!("https://{}{}", relay.addr, path(&group)), None).await.unwrap();
    assert_eq!(a2.join(&group, &ia).await.0, 0);
    assert_eq!(a.closed().await, Some(close::REPLACED as u32));
    assert_eq!(b.frame().await, wire::member(0, true));

    // Lying about the slot, and messages too long, close with the WebSocket codes.
    b.send(Class::Chat, &[wire::CHAT, 0, wire::BROADCAST]).await;
    assert_eq!(b.closed().await, Some(close::PROTOCOL_ERROR as u32));
    assert_eq!(a2.frame().await, wire::member(1, false));
    a2.write(Class::Chat, &((wire::MAX_FRAME_LEN + 1) as u16).to_be_bytes()).await;
    assert_eq!(a2.closed().await, Some(close::TOO_BIG as u32));
}

#[tokio::test]
async fn the_relay_refuses_what_is_not_a_group() {
    let relay = Relay::start(Default::default(), true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    // Raw QUIC: a path that names no group.
    let mut a = Wire::raw(relay.addr, "/v1/not-a-group").await;
    assert_eq!(a.closed().await, Some(close::NOT_FOUND as u32));
    // A stream of no class.
    let mut b = Wire::raw(relay.addr, &path(&group)).await;
    assert_eq!(b.frame().await[0], wire::HELLO);
    let mut stray = b.session.open_uni().await.unwrap();
    stray.write_all(&[9, 0, 0]).await.unwrap();
    assert_eq!(b.closed().await, Some(close::PROTOCOL_ERROR as u32));
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
async fn members_on_raw_quic_webtransport_and_websocket_share_a_group() {
    let relay = Relay::start(Default::default(), true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let mut a = join(&group, "quic");
    let mut b = join(&group.with_relay(&relay.url("ws")).unwrap(), "websocket");
    let mut c = WebTransportMember::join(&group.with_relay(&relay.url("https")).unwrap(), None);
    admitted(&mut a, 2).await;
    admitted(&mut b, 2).await;
    for _ in 0..2 {
        c.wait(|e| matches!(e, Event::Joined { .. })).await;
    }
    assert_eq!(a.via(), Some(Via::Quic));
    assert_eq!(b.via(), Some(Via::WebSocket));

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

    // A newcomer over QUIC syncs from the WebSocket member.
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
    assert_eq!((a.via(), b.via()), (Some(Via::Quic), Some(Via::Quic)));
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
async fn members_fall_back_to_websocket_when_udp_does_not_get_through() {
    // A relay without QUIC: UDP is refused at once.
    let relay = Relay::start(Default::default(), false).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let started = Instant::now();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    assert_eq!((a.via(), b.via()), (Some(Via::WebSocket), Some(Via::WebSocket)));
    eprintln!("joined over WebSocket, QUIC refused: {:?}", started.elapsed());

    // UDP that vanishes without a trace: WebSocket after QUIC's head start.
    let relay = Relay::start(Default::default(), true).await;
    let path = Path::new(relay.addr, Duration::ZERO).await;
    path.blocked.store(true, Ordering::Relaxed);
    let group = Group::create(&path.url("quic")).unwrap();
    let started = Instant::now();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    let took = started.elapsed();
    eprintln!("joined over WebSocket, UDP black-holed: {took:?}");
    assert_eq!((a.via(), b.via()), (Some(Via::WebSocket), Some(Via::WebSocket)));
    assert!(took < group::QUIC_HEAD_START + Duration::from_secs(2), "{took:?}");
    a.send(Channel::Chat, "over TCP").await.unwrap();
    let e = wait(&mut b, |e| matches!(e, Event::Message { .. })).await;
    assert_eq!(e, Event::Message { from: a.id(), channel: Channel::Chat, data: b"over TCP".to_vec(), direct: false });
}

#[tokio::test]
async fn a_certificate_serves_wss_and_webtransport_next_to_plain_websockets() {
    let cert = TestCertificate::new();
    let config = peerconnect_relay::Config { tls: Some(cert.files.clone()), ..Default::default() };
    let relay = Relay::start(config, true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let port = relay.addr.port();

    // wss:// and ws:// on one port.
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(cert.roots())
        .with_no_client_auth();
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls));
    let (mut secure, _) = tokio_tungstenite::connect_async_tls_with_config(
        format!("wss://localhost:{port}{}", path(&group)),
        None,
        false,
        Some(connector),
    )
    .await
    .unwrap();
    assert_eq!(next_ws(&mut secure).await[0], wire::HELLO);
    let (mut plain, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}{}", path(&group))).await.unwrap();
    assert_eq!(next_ws(&mut plain).await[0], wire::HELLO);

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
}

#[tokio::test]
async fn leaving_over_quic_delivers_what_was_sent_first() {
    let relay = Relay::start(Default::default(), true).await;
    let group = Group::create(&relay.url("quic")).unwrap();
    let mut a = join(&group, "a");
    let mut b = join(&group, "b");
    admitted(&mut a, 1).await;
    admitted(&mut b, 1).await;
    assert_eq!(a.via(), Some(Via::Quic));
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
    // A Rust member verifies an https:// relay, over QUIC and over wss://:
    // with a certificate from an authority it does not know, it never joins.
    let cert = TestCertificate::new();
    let config = peerconnect_relay::Config { tls: Some(cert.files.clone()), ..Default::default() };
    let relay = Relay::start(config, true).await;
    let group = Group::create(&format!("https://localhost:{}", relay.addr.port())).unwrap();
    let mut a = join(&group, "a");
    let outcome = timeout(group::QUIC_HEAD_START + Duration::from_secs(3), async {
        loop {
            match a.next().await {
                Some(Event::State(State::Online)) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await;
    assert!(outcome.is_err(), "joined a relay it cannot trust");
    assert_eq!(a.via(), None);
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
            sender.send_video(0, keyframe, at, data);
        }
    });
    let mut frames = Vec::new();
    let mut requests = 0;
    let deadline = start + Duration::from_secs(6);
    while Instant::now() < deadline {
        tokio::select! {
            e = b.next() => match e {
                Some(Event::Video { timestamp, data, .. }) => {
                    let i = frames.len();
                    let _ = i;
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
async fn video_flows_over_quic_and_websocket_and_recovers_from_loss() {
    for (name, quic, loss) in [("WebSocket", false, 0), ("QUIC", true, 0), ("QUIC, 3% loss each way", true, 30)] {
        let relay = Relay::start(Default::default(), true).await;
        let one_way = Duration::from_millis(15);
        let (pa, pb) = (Path::new(relay.addr, one_way).await, Path::new(relay.addr, one_way).await);
        let scheme = if quic { "quic" } else { "ws" };
        let group = Group::create(&pa.url(scheme)).unwrap();
        let mut a = join(&group, "a");
        let mut b = join(&group.with_relay(&pb.url(scheme)).unwrap(), "b");
        admitted(&mut a, 1).await;
        admitted(&mut b, 1).await;
        assert_eq!(b.via(), Some(if quic { Via::Quic } else { Via::WebSocket }));
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
