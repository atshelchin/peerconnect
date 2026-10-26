//! A Tokio driver for [`group::Member`]: real sockets, real timers, and
//! a send queue per class, so that commands never wait behind chat or a
//! state transfer, and voice is dropped rather than delayed when the
//! connection backs up.
//!
//! It connects over raw QUIC, where a lost packet holds up only its own
//! class, to the `https://` or `quic://` relay of the invite, as
//! [`group::transport`] describes. Groups have no WebSocket transport: where
//! UDP does not get through, the member stays offline and keeps trying.
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

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};

use super::quic::{self, QuicSocket};
use crate::Error;
use crate::group::transport;
use crate::group::{Channel, Config, Event, Group, Identity, Member, MemberId, Output, SendError, State};

type Connecting = Pin<Box<dyn Future<Output = Option<QuicSocket>> + Send>>;

/// How long a closing connection may take to flush what is queued.
const FLUSH_GRACE: Duration = Duration::from_secs(5);

/// Joins the group and stays in it, reconnecting as needed, until left or
/// dropped. Must be called within a Tokio runtime.
pub fn join(group: Group, identity: Identity, config: Config) -> Result<GroupConnection, Error> {
    let member = Member::new(group, identity, config, 0)?;
    let id = member.id();
    let (commands, rx) = mpsc::unbounded_channel();
    let (tx, events) = mpsc::unbounded_channel();
    let connected = Arc::new(AtomicBool::new(false));
    tokio::spawn(drive(member, rx, tx, connected.clone()));
    Ok(GroupConnection { sender: GroupSender { commands, id, connected }, events })
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

    /// The next event if there is one now, for callers that poll (a game
    /// loop, say) instead of awaiting.
    pub fn try_next(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
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
    connected: Arc<AtomicBool>,
}

impl GroupSender {
    /// Whether a QUIC connection to the relay is open now.
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
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

    /// Sends a video frame of `layer` of `track` to everyone (see
    /// [`group::Member::send_video`]); fragments are dropped, oldest first,
    /// when the connection backs up.
    pub fn send_video(&self, track: u8, layer: u8, keyframe: bool, timestamp: u32, data: impl Into<Vec<u8>>) {
        let _ = self.command(Command::Video(track, layer, keyframe, timestamp, data.into()));
    }

    /// Asks the relay for `layer` of `member`'s video `track`, `None` for
    /// none (see [`group::Member::subscribe`]).
    pub fn subscribe(&self, member: MemberId, track: u8, layer: Option<u8>) {
        let _ = self.command(Command::Subscribe(member, track, layer));
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
    Video(u8, u8, bool, u32, Vec<u8>),
    Subscribe(MemberId, u8, Option<u8>),
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
    connected: Arc<AtomicBool>,
) {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut socket: Option<QuicSocket> = None;
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
                    connected.store(false, Ordering::Relaxed);
                    if let Some(socket) = socket.take() {
                        socket.close(FLUSH_GRACE);
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
                Some(Command::Video(track, layer, keyframe, timestamp, data)) => {
                    member.send_video(track, layer, keyframe, timestamp, &data);
                }
                Some(Command::Subscribe(of, track, layer)) => {
                    let _ = member.subscribe(of, track, layer);
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
                        connected.store(true, Ordering::Relaxed);
                        socket = Some(opened);
                        member.handle_open(now());
                    }
                    None => member.handle_close(now(), None),
                }
            }
            received = async { socket.as_mut().unwrap().recv().await }, if socket.is_some() => match received {
                quic::Incoming::Binary(frame) => member.handle_binary(now(), &frame),
                quic::Incoming::Text(text) => member.handle_text(now(), text),
                quic::Incoming::Closed(code) => {
                    socket.take().unwrap().close(FLUSH_GRACE);
                    connected.store(false, Ordering::Relaxed);
                    member.handle_close(now(), code);
                }
            },
        }
    }
}

/// Opens a QUIC connection to the group's endpoint.
async fn open(url: String) -> Option<QuicSocket> {
    QuicSocket::connect(&transport::target(&url)?).await
}
