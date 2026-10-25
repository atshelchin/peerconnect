//! A Tokio driver for [`group::Member`]: real sockets, real timers, and
//! a send queue per class, so that commands never wait behind chat or a
//! state transfer, and voice is dropped rather than delayed when the
//! connection backs up.
//!
//! For an `https://` or `quic://` relay it connects over raw QUIC, where a
//! lost packet holds up only its own class, and falls back to the relay's
//! WebSocket if QUIC does not connect within [`QUIC_HEAD_START`] (UDP
//! blocked, say), as [`group::transport`] describes.
//!
//! ```no_run
//! # async fn demo(invite: &str) -> Result<(), Box<dyn std::error::Error>> {
//! use peerconnect::client::group;
//! use peerconnect::group::{Channel, Config, Event, Identity};
//!
//! let identity = Identity::generate(); // persist identity.seed() to be recognised later
//! let config = Config { profile: b"alice".to_vec(), ..Config::default() };
//! let mut conn = group::join(invite.parse()?, identity, config)?;
//! conn.send(Channel::Chat, b"hello").await?;
//! while let Some(event) = conn.next().await {
//!     match event {
//!         Event::Joined { member, profile } => println!("{} joined", String::from_utf8_lossy(&profile)),
//!         Event::Message { from, data, .. } => println!("{from}: {}", String::from_utf8_lossy(&data)),
//!         _ => {}
//!     }
//! }
//! # Ok(()) }
//! ```

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use super::quic::{self, QuicSocket};
use super::{Ws, open_with};
use crate::Error;
use crate::group::transport::{self, Transport};
use crate::group::{self, Channel, Class, Config, Event, Group, Identity, Member, MemberId, Output, SendError, State};

type Connecting = Pin<Box<dyn Future<Output = Option<Socket>> + Send>>;

/// How long QUIC has to connect before the WebSocket is tried as well.
pub const QUIC_HEAD_START: Duration = Duration::from_millis(1500);

/// Bytes written before each flush, at most: small, so that an urgent
/// frame queued meanwhile goes next.
const BATCH: usize = 16 << 10;
/// Command and voice bytes waiting to be written beyond which voice frames
/// are dropped.
const VOICE_BACKLOG: usize = 4 << 10;
/// Video bytes waiting beyond which the oldest video is dropped.
const VIDEO_BACKLOG: usize = 64 << 10;
/// How long a closing socket may take to flush what is queued.
const FLUSH_GRACE: Duration = Duration::from_secs(5);

/// Joins the group and stays in it, reconnecting as needed, until left or
/// dropped. Must be called within a Tokio runtime.
pub fn join(group: Group, identity: Identity, config: Config) -> Result<GroupConnection, Error> {
    let member = Member::new(group, identity, config, 0)?;
    let id = member.id();
    let (commands, rx) = mpsc::unbounded_channel();
    let (tx, events) = mpsc::unbounded_channel();
    let via = Arc::new(AtomicU8::new(0));
    tokio::spawn(drive(member, rx, tx, via.clone()));
    Ok(GroupConnection { sender: GroupSender { commands, id, via }, events })
}

/// A live group membership. Dropping it leaves the group.
pub struct GroupConnection {
    sender: GroupSender,
    events: mpsc::UnboundedReceiver<Event>,
}

impl GroupConnection {
    /// The next event; `None` once the connection has stopped.
    pub async fn next(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// A handle for sending from other tasks.
    pub fn sender(&self) -> GroupSender {
        self.sender.clone()
    }

    pub fn id(&self) -> MemberId {
        self.sender.id
    }

    /// Leaves the group, and waits until the connection has stopped.
    pub async fn leave(mut self) {
        let _ = self.sender.commands.send(Command::Leave);
        while self.events.recv().await.is_some() {}
    }
}

impl std::ops::Deref for GroupConnection {
    type Target = GroupSender;

    fn deref(&self) -> &GroupSender {
        &self.sender
    }
}

/// Sends to the group (see the methods of [`group::Member`] of the same names).
#[derive(Clone)]
pub struct GroupSender {
    commands: mpsc::UnboundedSender<Command>,
    id: MemberId,
    via: Arc<AtomicU8>,
}

/// What a connection runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    WebSocket = 1,
    Quic = 2,
}

impl GroupSender {
    /// What the connection to the relay runs over; `None` while there is none.
    pub fn via(&self) -> Option<Via> {
        match self.via.load(Ordering::Relaxed) {
            1 => Some(Via::WebSocket),
            2 => Some(Via::Quic),
            _ => None,
        }
    }

    /// Broadcasts a message on `channel`; returns its id.
    pub async fn send(&self, channel: Channel, data: impl Into<Vec<u8>>) -> Result<u64, SendError> {
        let (reply, result) = oneshot::channel();
        self.command(Command::Send(channel, data.into(), reply))?;
        result.await.unwrap_or(Err(SendError::Closed))
    }

    pub async fn send_to(&self, member: MemberId, channel: Channel, data: impl Into<Vec<u8>>) -> Result<(), SendError> {
        let (reply, result) = oneshot::channel();
        self.command(Command::SendTo(member, channel, data.into(), reply))?;
        result.await.unwrap_or(Err(SendError::Closed))
    }

    /// Sends a voice frame; it is dropped if the connection is backed up.
    pub fn send_voice(&self, seq: u16, data: impl Into<Vec<u8>>) {
        let _ = self.command(Command::Voice(seq, data.into()));
    }

    /// Sends a video frame to everyone (see [`group::Member::send_video`]);
    /// fragments are dropped, oldest first, when the connection backs up.
    pub fn send_video(&self, track: u8, keyframe: bool, timestamp: u32, data: impl Into<Vec<u8>>) {
        let _ = self.command(Command::Video(track, keyframe, timestamp, data.into()));
    }

    /// Sends a datagram to one member or to all; dropped rather than delayed.
    pub fn send_datagram(&self, to: Option<MemberId>, data: impl Into<Vec<u8>>) {
        let _ = self.command(Command::Datagram(to, data.into()));
    }

    pub async fn request_sync(&self, from: MemberId) -> Result<(), SendError> {
        let (reply, result) = oneshot::channel();
        self.command(Command::RequestSync(from, reply))?;
        result.await.unwrap_or(Err(SendError::Closed))
    }

    /// Answers [`Event::SyncRequest`] with the snapshot taken when it was received.
    pub fn provide_sync(&self, to: MemberId, transfer: u32, snapshot: Vec<u8>) {
        let _ = self.command(Command::ProvideSync(to, transfer, snapshot));
    }

    pub fn decline_sync(&self, to: MemberId, transfer: u32) {
        let _ = self.command(Command::DeclineSync(to, transfer));
    }

    fn command(&self, command: Command) -> Result<(), SendError> {
        self.commands.send(command).map_err(|_| SendError::Closed)
    }
}

enum Command {
    Send(Channel, Vec<u8>, oneshot::Sender<Result<u64, SendError>>),
    SendTo(MemberId, Channel, Vec<u8>, oneshot::Sender<Result<(), SendError>>),
    Voice(u16, Vec<u8>),
    Video(u8, bool, u32, Vec<u8>),
    Datagram(Option<MemberId>, Vec<u8>),
    RequestSync(MemberId, oneshot::Sender<Result<(), SendError>>),
    ProvideSync(MemberId, u32, Vec<u8>),
    DeclineSync(MemberId, u32),
    Leave,
}

async fn drive(
    mut member: Member,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<Event>,
    via: Arc<AtomicU8>,
) {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut socket: Option<Socket> = None;
    let mut connecting: Option<Connecting> = None;
    let mut controlled = true;

    loop {
        while let Some(output) = member.poll_output() {
            match output {
                Output::Connect(url) => connecting = Some(Box::pin(open(url))),
                Output::Send(frame, class) => {
                    if let Some(socket) = &socket {
                        socket.send(class, frame);
                    }
                }
                Output::Ping => {
                    if let Some(socket) = &socket {
                        socket.ping();
                    }
                }
                Output::Disconnect => {
                    connecting = None;
                    via.store(0, Ordering::Relaxed);
                    if let Some(socket) = socket.take() {
                        socket.close();
                    }
                }
                Output::Event(event) => {
                    if events.send(event).is_err() {
                        member.leave(); // nobody is listening any more
                    }
                }
            }
        }
        if member.state() == State::Closed {
            return;
        }

        let deadline = member.poll_timeout().map(|t| start + Duration::from_millis(t));
        tokio::select! {
            biased;
            command = commands.recv(), if controlled => match command {
                Some(Command::Send(channel, data, reply)) => {
                    let _ = reply.send(member.send(channel, &data));
                }
                Some(Command::SendTo(to, channel, data, reply)) => {
                    let _ = reply.send(member.send_to(to, channel, &data));
                }
                Some(Command::Video(track, keyframe, timestamp, data)) => {
                    member.send_video(track, keyframe, timestamp, &data);
                }
                Some(Command::Voice(seq, data)) => {
                    member.send_voice(seq, &data);
                }
                Some(Command::Datagram(to, data)) => {
                    member.send_datagram(to, &data);
                }
                Some(Command::RequestSync(from, reply)) => {
                    let _ = reply.send(member.request_sync(from));
                }
                Some(Command::ProvideSync(to, transfer, data)) => {
                    let _ = member.provide_sync(to, transfer, data);
                }
                Some(Command::DeclineSync(to, transfer)) => member.decline_sync(to, transfer),
                Some(Command::Leave) | None => {
                    controlled = false;
                    member.leave();
                }
            },
            () = async { sleep_until(deadline.unwrap()).await }, if deadline.is_some() => {
                member.handle_timeout(now());
            }
            opened = async { connecting.as_mut().unwrap().await }, if connecting.is_some() => {
                connecting = None;
                match opened {
                    Some(opened) => {
                        via.store(opened.via() as u8, Ordering::Relaxed);
                        socket = Some(opened);
                        member.handle_open(now());
                    }
                    None => member.handle_close(now(), None),
                }
            }
            received = async { socket.as_mut().unwrap().recv().await }, if socket.is_some() => match received {
                Received::Binary(frame) => member.handle_binary(now(), &frame),
                Received::Text(text) => member.handle_text(now(), &text),
                Received::Closed(code) => {
                    socket.take().unwrap().close();
                    via.store(0, Ordering::Relaxed);
                    member.handle_close(now(), code);
                }
                Received::Nothing => {}
            },
        }
    }
}

/// Opens the first transport to work (see [`QUIC_HEAD_START`]).
async fn open(url: String) -> Option<Socket> {
    let mut quic = None;
    let mut websocket = None;
    for way in transport::transports(&url) {
        match way {
            Transport::Quic(target) => quic = Some(target),
            Transport::WebSocket(url) => websocket = Some(url),
        }
    }
    let websocket = async move { Some(Socket::websocket(open_with(websocket?, group::wire::MAX_FRAME_LEN).await?)) };
    let Some(target) = quic else { return websocket.await };
    let quic = async move { QuicSocket::connect(&target).await.map(Socket::Quic) };
    tokio::pin!(quic, websocket);
    tokio::select! {
        biased;
        opened = &mut quic => return match opened {
            Some(opened) => Some(opened),
            None => websocket.await,
        },
        () = tokio::time::sleep(QUIC_HEAD_START) => {}
    }
    // Both, now; the first to open wins.
    let (mut quic_failed, mut websocket_failed) = (false, false);
    while !(quic_failed && websocket_failed) {
        tokio::select! {
            biased;
            opened = &mut quic, if !quic_failed => match opened {
                Some(opened) => return Some(opened),
                None => quic_failed = true,
            },
            opened = &mut websocket, if !websocket_failed => match opened {
                Some(opened) => return Some(opened),
                None => websocket_failed = true,
            },
        }
    }
    None
}

enum Received {
    Binary(bytes::Bytes),
    Text(String),
    Closed(Option<u16>),
    /// Nothing for the member (a WebSocket control frame).
    Nothing,
}

/// An open connection: the driver reads it, writer tasks drain its queues.
enum Socket {
    WebSocket { stream: SplitStream<Ws>, queue: Arc<Queue>, writer: tokio::task::JoinHandle<()> },
    Quic(QuicSocket),
}

impl Socket {
    fn via(&self) -> Via {
        match self {
            Socket::WebSocket { .. } => Via::WebSocket,
            Socket::Quic(_) => Via::Quic,
        }
    }

    fn websocket(ws: Ws) -> Socket {
        let (sink, stream) = ws.split();
        let queue = Arc::new(Queue::default());
        let writer = tokio::spawn(write(sink, queue.clone()));
        Socket::WebSocket { stream, queue, writer }
    }

    fn send(&self, class: Class, frame: Vec<u8>) {
        match self {
            Socket::WebSocket { queue, .. } => queue.push(Some(class), Message::binary(frame)),
            Socket::Quic(quic) => quic.send(class, frame),
        }
    }

    fn ping(&self) {
        match self {
            Socket::WebSocket { queue, .. } => queue.push(None, Message::text(group::wire::PING)),
            Socket::Quic(quic) => quic.ping(),
        }
    }

    async fn recv(&mut self) -> Received {
        match self {
            Socket::WebSocket { stream, .. } => match stream.next().await {
                Some(Ok(Message::Binary(frame))) => Received::Binary(frame),
                Some(Ok(Message::Text(text))) => Received::Text(text.as_str().to_owned()),
                Some(Ok(Message::Close(frame))) => Received::Closed(frame.map(|f| f.code.into())),
                Some(Ok(_)) => Received::Nothing,
                Some(Err(_)) | None => Received::Closed(None),
            },
            Socket::Quic(quic) => match quic.recv().await {
                quic::Incoming::Binary(frame) => Received::Binary(frame),
                quic::Incoming::Text(text) => Received::Text(text.to_owned()),
                quic::Incoming::Closed(code) => Received::Closed(code),
            },
        }
    }

    /// Closes with 1000 once what is queued is written, abandoning a socket
    /// that does not drain in time.
    fn close(self) {
        match self {
            Socket::WebSocket { stream, queue, mut writer } => {
                queue.inner.lock().unwrap().closing = true;
                queue.ready.notify_one();
                tokio::spawn(async move {
                    if timeout(FLUSH_GRACE, &mut writer).await.is_err() {
                        writer.abort();
                    }
                    drop(stream);
                });
            }
            Socket::Quic(quic) => quic.close(FLUSH_GRACE),
        }
    }
}

/// What the writer will send: keepalives first, then frames by class.
#[derive(Default)]
struct Queue {
    inner: Mutex<Pending>,
    ready: Notify,
}

#[derive(Default)]
struct Pending {
    pings: VecDeque<Message>,
    classes: [VecDeque<Message>; Class::COUNT],
    bytes: [usize; Class::COUNT],
    closing: bool,
}

impl Queue {
    /// Queues a frame of `class`, or a keepalive (`None`). Voice behind
    /// too much is dropped (late audio is worse than lost audio), and the
    /// oldest video past its budget.
    fn push(&self, class: Option<Class>, msg: Message) {
        let mut q = self.inner.lock().unwrap();
        match class {
            None => q.pings.push_back(msg),
            Some(class) => {
                let (command, voice, video) = (Class::Command.index(), Class::Voice.index(), Class::Video.index());
                if class == Class::Voice && q.bytes[command] + q.bytes[voice] > VOICE_BACKLOG {
                    return;
                }
                while class == Class::Video && q.bytes[video] + msg.len() > VIDEO_BACKLOG {
                    let Some(old) = q.classes[video].pop_front() else { break };
                    q.bytes[video] -= old.len();
                }
                q.bytes[class.index()] += msg.len();
                q.classes[class.index()].push_back(msg);
            }
        }
        drop(q);
        self.ready.notify_one();
    }

    /// Up to [`BATCH`] bytes, most urgent first; `None` once closing and empty.
    fn take(&self) -> Option<Vec<Message>> {
        let mut q = self.inner.lock().unwrap();
        let mut batch: Vec<Message> = q.pings.drain(..).collect();
        let mut taken = 0;
        while taken < BATCH {
            let Some(class) = Class::PRIORITY.iter().map(|c| c.index()).find(|&c| !q.classes[c].is_empty()) else {
                break;
            };
            let msg = q.classes[class].pop_front().unwrap();
            q.bytes[class] -= msg.len();
            taken += msg.len();
            batch.push(msg);
        }
        (!batch.is_empty() || !q.closing).then_some(batch)
    }
}

async fn write(mut sink: SplitSink<Ws, Message>, queue: Arc<Queue>) {
    while let Some(batch) = queue.take() {
        if batch.is_empty() {
            queue.ready.notified().await;
            continue;
        }
        for msg in batch {
            if sink.feed(msg).await.is_err() {
                return;
            }
        }
        if sink.flush().await.is_err() {
            return;
        }
    }
    let _ = sink.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
}
