//! A Tokio driver for [`Peer`]: real WebSockets (with TLS for `wss://`),
//! real timers, and an async API.
//!
//! ```no_run
//! # async fn demo(invite: &str) -> Result<(), Box<dyn std::error::Error>> {
//! use peerconnect::{Event, Room, State, client};
//!
//! let room = Room::join(&invite.parse()?);
//! // Persist `room` now: it holds the identity the inviter will pin.
//! let mut conn = client::connect(room);
//! while let Some(event) = conn.next().await {
//!     match event {
//!         Event::State(State::Connected) => conn.send(b"hello").await?,
//!         Event::Message(data) => println!("{}", String::from_utf8_lossy(&data)),
//!         _ => {}
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! The driver never stops reading the socket: incoming events are buffered
//! without bound (like a browser WebSocket), so an application that is slow
//! to call [`Connection::next`] costs memory but can never deadlock the
//! connection. Sending is paced: [`Connection::send`] waits while more than
//! 1 MiB is queued for the socket.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::{CloseReason, Event, Output, Peer, Room, SendError, State, wire};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type Connecting = Pin<Box<dyn Future<Output = Option<Ws>> + Send>>;

/// `send` waits while more than this is queued for the socket.
const HIGH_WATER: usize = 1 << 20;
/// Bytes written before each flush, at most.
const BATCH: usize = 256 << 10;
/// How long a closing socket may take to flush what is queued (including,
/// after [`Connection::close`], the message ending the room) before it is dropped.
const FLUSH_GRACE: Duration = Duration::from_secs(30);

/// Connects to the room's relay and keeps the room connected, reconnecting
/// as needed, until the connection is left, closed or dropped. Must be
/// called within a Tokio runtime.
pub fn connect(room: Room) -> Connection {
    let shared = Arc::new(Mutex::new(room.clone()));
    let (commands, rx) = mpsc::channel(64);
    let (stop, stopped) = oneshot::channel();
    let (tx, events) = mpsc::unbounded_channel();
    tokio::spawn(drive(Peer::new(room, 0), rx, stopped, tx, shared.clone()));
    Connection { sender: Sender { commands }, stop: Some(stop), events, room: shared }
}

/// A live room connection. Dropping it leaves the room.
pub struct Connection {
    sender: Sender,
    stop: Option<oneshot::Sender<Stop>>,
    events: mpsc::UnboundedReceiver<Event>,
    room: Arc<Mutex<Room>>,
}

impl Connection {
    /// The next event; `None` once the connection has stopped.
    pub async fn next(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// Sends a message if the peer is connected (see [`Peer::send`]). Waits
    /// while the socket has a large backlog.
    pub async fn send(&self, data: impl Into<Vec<u8>>) -> Result<(), SendError> {
        self.sender.send(data).await
    }

    /// A handle for sending from other tasks.
    pub fn sender(&self) -> Sender {
        self.sender.clone()
    }

    /// The room as it currently stands; persist it after [`Event::Paired`].
    pub fn room(&self) -> Room {
        self.room.lock().unwrap().clone()
    }

    /// Disconnects, keeping the room for later.
    pub async fn leave(mut self) {
        self.stop(Stop::Leave).await;
    }

    /// Ends the room for both peers (see [`Peer::close`]). Returns once the
    /// peer has been told, as far as that is possible: the message ending
    /// the room follows anything still queued, and is given up on if the
    /// connection does not drain within 30 s.
    pub async fn close(mut self) {
        self.stop(Stop::Close).await;
    }

    async fn stop(&mut self, stop: Stop) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(stop);
        }
        while self.events.recv().await.is_some() {}
    }
}

#[derive(Clone)]
pub struct Sender {
    commands: mpsc::Sender<Command>,
}

impl Sender {
    pub async fn send(&self, data: impl Into<Vec<u8>>) -> Result<(), SendError> {
        let (reply, result) = oneshot::channel();
        let data = data.into();
        if self.commands.send(Command::Send(data, reply)).await.is_err() {
            return Err(SendError::NotConnected);
        }
        result.await.unwrap_or(Err(SendError::NotConnected))
    }
}

enum Command {
    Send(Vec<u8>, oneshot::Sender<Result<(), SendError>>),
}

/// How the application stops the connection. Dropping the sender leaves.
enum Stop {
    Leave,
    Close,
}

async fn drive(
    mut peer: Peer,
    mut commands: mpsc::Receiver<Command>,
    mut stop: oneshot::Receiver<Stop>,
    events: mpsc::UnboundedSender<Event>,
    room: Arc<Mutex<Room>>,
) {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let drained = Arc::new(Notify::new());
    let mut socket: Option<Socket> = None;
    let mut connecting: Option<Connecting> = None;
    let mut flushing: Option<tokio::task::JoinHandle<()>> = None;
    let mut stopping = false;

    loop {
        while let Some(output) = peer.poll_output() {
            match output {
                Output::Connect(url) => connecting = Some(Box::pin(open(url))),
                Output::Send(frame) => {
                    if let Some(socket) = &socket {
                        socket.send(frame);
                    }
                }
                Output::Ping => {
                    if let Some(socket) = &socket {
                        let _ = socket.control.send(Message::text(wire::PING));
                    }
                }
                Output::Disconnect => {
                    connecting = None;
                    if let Some(socket) = socket.take() {
                        flushing = Some(socket.close());
                    }
                }
                Output::Event(event) => {
                    if event == Event::Paired {
                        *room.lock().unwrap() = peer.room().clone();
                    }
                    if events.send(event).is_err() {
                        peer.leave(); // nobody is listening any more
                    }
                }
            }
        }
        if let State::Closed(reason) = peer.state() {
            // Ending the room is only done once the peer could have been told.
            if reason == CloseReason::Closed
                && let Some(flushing) = flushing
            {
                let _ = flushing.await;
            }
            return;
        }

        let deadline = peer.poll_timeout().map(|t| start + Duration::from_millis(t));
        let backlogged = socket.as_ref().is_some_and(|s| s.queued.load(Ordering::Relaxed) >= HIGH_WATER);
        tokio::select! {
            ws = async { connecting.as_mut().unwrap().await }, if connecting.is_some() => {
                connecting = None;
                match ws {
                    Some(ws) => {
                        socket = Some(Socket::new(ws, drained.clone()));
                        peer.handle_open(now());
                    }
                    None => peer.handle_close(now(), None),
                }
            }
            msg = async { socket.as_mut().unwrap().stream.next().await }, if socket.is_some() => match msg {
                Some(Ok(Message::Binary(frame))) => peer.handle_binary(now(), &frame),
                Some(Ok(Message::Text(text))) => peer.handle_text(now(), &text),
                Some(Ok(Message::Close(frame))) => {
                    socket.take().unwrap().close();
                    peer.handle_close(now(), frame.map(|f| f.code.into()));
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    socket.take().unwrap().close();
                    peer.handle_close(now(), None);
                }
            },
            Some(Command::Send(data, reply)) = commands.recv(), if !backlogged => {
                let _ = reply.send(peer.send(&data));
            }
            how = &mut stop, if !stopping => {
                stopping = true;
                match how {
                    Ok(Stop::Close) => peer.close(),
                    Ok(Stop::Leave) | Err(_) => peer.leave(),
                }
            }
            () = drained.notified(), if backlogged => {}
            () = async { sleep_until(deadline.unwrap()).await }, if deadline.is_some() => {
                peer.handle_timeout(now());
            }
        }
    }
}

/// An open WebSocket: the driver reads it, a separate task writes it, so a
/// slow write never holds up reading.
struct Socket {
    stream: SplitStream<Ws>,
    data: mpsc::UnboundedSender<Message>,
    /// Keepalives, written ahead of queued data.
    control: mpsc::UnboundedSender<Message>,
    /// Bytes queued but not yet written.
    queued: Arc<AtomicUsize>,
    writer: tokio::task::JoinHandle<()>,
}

impl Socket {
    fn new(ws: Ws, drained: Arc<Notify>) -> Socket {
        let (sink, stream) = ws.split();
        let (data, data_rx) = mpsc::unbounded_channel();
        let (control, control_rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let writer = tokio::spawn(write(sink, control_rx, data_rx, queued.clone(), drained));
        Socket { stream, data, control, queued, writer }
    }

    fn send(&self, frame: Vec<u8>) {
        self.queued.fetch_add(frame.len(), Ordering::Relaxed);
        let _ = self.data.send(Message::binary(frame));
    }

    /// Closes with 1000 once everything queued is written, abandoning a
    /// socket that does not drain in time. The returned task finishes then.
    fn close(self) -> tokio::task::JoinHandle<()> {
        let Socket { stream, data, control, mut writer, .. } = self;
        drop((data, control));
        tokio::spawn(async move {
            if timeout(FLUSH_GRACE, &mut writer).await.is_err() {
                writer.abort();
            }
            drop(stream);
        })
    }
}

async fn write(
    mut sink: SplitSink<Ws, Message>,
    mut control: mpsc::UnboundedReceiver<Message>,
    mut data: mpsc::UnboundedReceiver<Message>,
    queued: Arc<AtomicUsize>,
    drained: Arc<Notify>,
) {
    loop {
        let first = tokio::select! {
            biased;
            Some(msg) = control.recv() => msg,
            msg = data.recv() => match msg {
                Some(msg) => msg,
                None => break,
            },
        };
        // Write what is ready, keepalives first, then flush once.
        let mut written = 0;
        let mut next = Some(first);
        while let Some(msg) = next {
            if let Message::Binary(frame) = &msg {
                written += frame.len();
            }
            if sink.feed(msg).await.is_err() {
                return;
            }
            next = control.try_recv().ok().or_else(|| (written < BATCH).then(|| data.try_recv().ok()).flatten());
        }
        if sink.flush().await.is_err() {
            return;
        }
        if written > 0 {
            queued.fetch_sub(written, Ordering::Relaxed);
            drained.notify_one();
        }
    }
    let _ = sink.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
}

async fn open(url: String) -> Option<Ws> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(wire::MAX_FRAME_LEN))
        .max_frame_size(Some(wire::MAX_FRAME_LEN));
    let (ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), true).await.ok()?;
    Some(ws)
}
