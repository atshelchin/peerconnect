//! A Tokio driver for [`group::Member`]: real WebSockets, real timers, and
//! a send queue per class, so that commands never wait behind chat or a
//! state transfer, and voice is dropped rather than delayed when the
//! connection backs up.
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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use super::{Ws, open_with};
use crate::Error;
use crate::group::{self, Channel, Class, Config, Event, Group, Identity, Member, MemberId, Output, SendError, State};

type Connecting = Pin<Box<dyn Future<Output = Option<Ws>> + Send>>;

/// Bytes written before each flush, at most: small, so that an urgent
/// frame queued meanwhile goes next.
const BATCH: usize = 16 << 10;
/// Bytes waiting to be written beyond which voice frames are dropped.
const VOICE_BACKLOG: usize = 4 << 10;
/// How long a closing socket may take to flush what is queued.
const FLUSH_GRACE: Duration = Duration::from_secs(5);

/// Joins the group and stays in it, reconnecting as needed, until left or
/// dropped. Must be called within a Tokio runtime.
pub fn join(group: Group, identity: Identity, config: Config) -> Result<GroupConnection, Error> {
    let member = Member::new(group, identity, config, 0)?;
    let id = member.id();
    let (commands, rx) = mpsc::unbounded_channel();
    let (tx, events) = mpsc::unbounded_channel();
    tokio::spawn(drive(member, rx, tx));
    Ok(GroupConnection { sender: GroupSender { commands, id }, events })
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
}

impl GroupSender {
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
) {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut socket: Option<Socket> = None;
    let mut connecting: Option<Connecting> = None;
    let mut controlled = true;

    loop {
        while let Some(output) = member.poll_output() {
            match output {
                Output::Connect(url) => connecting = Some(Box::pin(open_with(url, group::wire::MAX_FRAME_LEN))),
                Output::Send(frame, class) => {
                    if let Some(socket) = &socket {
                        socket.queue.push(Some(class), Message::binary(frame));
                    }
                }
                Output::Ping => {
                    if let Some(socket) = &socket {
                        socket.queue.push(None, Message::text(group::wire::PING));
                    }
                }
                Output::Disconnect => {
                    connecting = None;
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
            ws = async { connecting.as_mut().unwrap().await }, if connecting.is_some() => {
                connecting = None;
                match ws {
                    Some(ws) => {
                        socket = Some(Socket::new(ws));
                        member.handle_open(now());
                    }
                    None => member.handle_close(now(), None),
                }
            }
            msg = async { socket.as_mut().unwrap().stream.next().await }, if socket.is_some() => match msg {
                Some(Ok(Message::Binary(frame))) => member.handle_binary(now(), &frame),
                Some(Ok(Message::Text(text))) => member.handle_text(now(), &text),
                Some(Ok(Message::Close(frame))) => {
                    socket.take().unwrap().close();
                    member.handle_close(now(), frame.map(|f| f.code.into()));
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    socket.take().unwrap().close();
                    member.handle_close(now(), None);
                }
            },
        }
    }
}

/// An open WebSocket: the driver reads it, a writer task drains its queues.
struct Socket {
    stream: SplitStream<Ws>,
    queue: Arc<Queue>,
    writer: tokio::task::JoinHandle<()>,
}

impl Socket {
    fn new(ws: Ws) -> Socket {
        let (sink, stream) = ws.split();
        let queue = Arc::new(Queue::default());
        let writer = tokio::spawn(write(sink, queue.clone()));
        Socket { stream, queue, writer }
    }

    /// Closes with 1000 once what is queued is written, abandoning a socket
    /// that does not drain in time.
    fn close(self) {
        let Socket { stream, queue, mut writer } = self;
        queue.inner.lock().unwrap().closing = true;
        queue.ready.notify_one();
        tokio::spawn(async move {
            if timeout(FLUSH_GRACE, &mut writer).await.is_err() {
                writer.abort();
            }
            drop(stream);
        });
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
    classes: [VecDeque<Message>; 4],
    bytes: usize,
    closing: bool,
}

impl Queue {
    /// Queues a frame of `class`, or a keepalive (`None`).
    fn push(&self, class: Option<Class>, msg: Message) {
        let mut q = self.inner.lock().unwrap();
        match class {
            None => q.pings.push_back(msg),
            Some(class) => {
                if class.is_realtime() && q.bytes > VOICE_BACKLOG {
                    return; // late audio is worse than lost audio
                }
                q.bytes += msg.len();
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
            let Some(class) = (0..4).find(|&c| !q.classes[c].is_empty()) else { break };
            let msg = q.classes[class].pop_front().unwrap();
            q.bytes -= msg.len();
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
