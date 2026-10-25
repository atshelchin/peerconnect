//! WebAssembly bindings for the JavaScript SDK (`js/`). They expose the
//! sans-I/O [`peerconnect::Peer`] unchanged; the JavaScript side supplies
//! the WebSocket and the timers.

use js_sys::{Object, Reflect, Uint8Array};
use peerconnect::{CloseReason, Event, Output, Role, State};
use wasm_bindgen::prelude::*;

/// Room credentials (see `peerconnect::Room`).
#[wasm_bindgen]
pub struct Room(peerconnect::Room);

#[wasm_bindgen]
impl Room {
    /// Creates a room on a relay (`ws://` or `wss://` URL); you are the inviter.
    pub fn create(relay: &str) -> Result<Room, JsError> {
        Ok(Room(peerconnect::Room::create(relay)?))
    }

    /// Accepts an invite; you are the joiner. Save the room before connecting:
    /// it holds the identity the inviter will pin.
    pub fn join(invite: &str) -> Result<Room, JsError> {
        Ok(Room(peerconnect::Room::join(&invite.parse()?)))
    }

    /// Restores a room saved with `toString()`.
    pub fn parse(record: &str) -> Result<Room, JsError> {
        Ok(Room(record.parse()?))
    }

    /// The room record, to persist (it is a secret).
    #[wasm_bindgen(js_name = toString)]
    pub fn to_record(&self) -> String {
        self.0.to_string()
    }

    /// The invite to hand to the other peer, until the inviter has paired.
    #[wasm_bindgen(getter)]
    pub fn invite(&self) -> Option<String> {
        self.0.invite().map(|i| i.to_string())
    }

    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.0.id().to_string()
    }

    /// `"inviter"` or `"joiner"`.
    #[wasm_bindgen(getter)]
    pub fn role(&self) -> String {
        match self.0.role() {
            Role::Inviter => "inviter".into(),
            Role::Joiner => "joiner".into(),
        }
    }

    #[wasm_bindgen(getter)]
    pub fn relay(&self) -> String {
        self.0.relay().into()
    }

    /// Whether pairing is complete (the room has moved to its pair ID).
    #[wasm_bindgen(getter)]
    pub fn paired(&self) -> bool {
        self.0.is_paired()
    }

    /// Whether the room is being ended.
    #[wasm_bindgen(getter)]
    pub fn closing(&self) -> bool {
        self.0.is_closing()
    }

    #[wasm_bindgen(getter, js_name = publicKey)]
    pub fn public_key(&self) -> Vec<u8> {
        self.0.public_key().to_vec()
    }

    #[wasm_bindgen(getter, js_name = peerKey)]
    pub fn peer_key(&self) -> Option<Vec<u8>> {
        self.0.peer_key().map(|k| k.to_vec())
    }
}

/// The peer state machine (see `peerconnect::Peer`). Times are milliseconds
/// on a monotonic clock.
#[wasm_bindgen]
pub struct Peer(peerconnect::Peer);

#[wasm_bindgen]
impl Peer {
    #[wasm_bindgen(constructor)]
    pub fn new(room: &Room, now: f64) -> Peer {
        Peer(peerconnect::Peer::new(room.0.clone(), now as u64))
    }

    pub fn room(&self) -> Room {
        Room(self.0.room().clone())
    }

    /// Bytes of messages waiting to be acknowledged by the peer.
    #[wasm_bindgen(getter)]
    pub fn queued(&self) -> f64 {
        self.0.queued() as f64
    }

    #[wasm_bindgen(js_name = handleOpen)]
    pub fn handle_open(&mut self, now: f64) {
        self.0.handle_open(now as u64);
    }

    #[wasm_bindgen(js_name = handleBinary)]
    pub fn handle_binary(&mut self, now: f64, frame: &[u8]) {
        self.0.handle_binary(now as u64, frame);
    }

    #[wasm_bindgen(js_name = handleText)]
    pub fn handle_text(&mut self, now: f64, text: &str) {
        self.0.handle_text(now as u64, text);
    }

    #[wasm_bindgen(js_name = handleClose)]
    pub fn handle_close(&mut self, now: f64, code: Option<u16>) {
        self.0.handle_close(now as u64, code);
    }

    #[wasm_bindgen(js_name = handleTimeout)]
    pub fn handle_timeout(&mut self, now: f64) {
        self.0.handle_timeout(now as u64);
    }

    #[wasm_bindgen(js_name = pollTimeout)]
    pub fn poll_timeout(&self) -> Option<f64> {
        self.0.poll_timeout().map(|t| t as f64)
    }

    /// The next output as `{ type, ... }`, or `undefined`:
    /// `connect {url}`, `send {data}`, `ping`, `disconnect`,
    /// `state {state, reason?}`, `message {data}`, `delivered {id}`,
    /// `paired`, `replaced`.
    #[wasm_bindgen(js_name = pollOutput)]
    pub fn poll_output(&mut self) -> JsValue {
        let Some(output) = self.0.poll_output() else { return JsValue::UNDEFINED };
        let out = Object::new();
        let set = |key: &str, value: JsValue| {
            Reflect::set(&out, &key.into(), &value).unwrap();
        };
        match output {
            Output::Connect(url) => {
                set("type", "connect".into());
                set("url", url.into());
            }
            Output::Send(frame) => {
                set("type", "send".into());
                set("data", Uint8Array::from(frame.as_slice()).into());
            }
            Output::Ping => set("type", "ping".into()),
            Output::Disconnect => set("type", "disconnect".into()),
            Output::Event(Event::Message(data)) => {
                set("type", "message".into());
                set("data", Uint8Array::from(data.as_slice()).into());
            }
            Output::Event(Event::Delivered(id)) => {
                set("type", "delivered".into());
                set("id", (id as f64).into());
            }
            Output::Event(Event::Paired) => set("type", "paired".into()),
            Output::Event(Event::Replaced) => set("type", "replaced".into()),
            Output::Event(Event::State(state)) => {
                set("type", "state".into());
                let (name, reason) = match state {
                    State::Offline => ("offline", None),
                    State::Waiting => ("waiting", None),
                    State::Connected => ("connected", None),
                    State::Closed(reason) => ("closed", Some(reason)),
                };
                set("state", name.into());
                if let Some(reason) = reason {
                    set("reason", close_reason(reason).into());
                }
            }
        }
        out.into()
    }

    /// Queues a message and returns its id; throws if it is over 16 MiB,
    /// if too much is unacknowledged, or if the room is closed or closing.
    pub fn send(&mut self, data: &[u8]) -> Result<f64, JsError> {
        Ok(self.0.send(data)? as f64)
    }

    pub fn leave(&mut self) {
        self.0.leave();
    }

    pub fn close(&mut self) {
        self.0.close();
    }
}

fn close_reason(reason: CloseReason) -> &'static str {
    match reason {
        CloseReason::Left => "left",
        CloseReason::Closed => "closed",
        CloseReason::ClosedByPeer => "closed-by-peer",
    }
}

// Groups (GROUP.md). Member IDs travel as base64url strings.

use peerconnect::group::{self, Channel, Class};

/// A group: its relay and secret. `toString()` is the invite.
#[wasm_bindgen]
pub struct Group(group::Group);

#[wasm_bindgen]
impl Group {
    pub fn create(relay: &str) -> Result<Group, JsError> {
        Ok(Group(group::Group::create(relay)?))
    }

    /// Parses an invite.
    pub fn parse(invite: &str) -> Result<Group, JsError> {
        Ok(Group(invite.parse()?))
    }

    #[wasm_bindgen(js_name = toString)]
    pub fn to_invite(&self) -> String {
        self.0.to_string()
    }

    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.0.id().to_string()
    }

    #[wasm_bindgen(getter)]
    pub fn relay(&self) -> String {
        self.0.relay().into()
    }
}

/// A member identity: an Ed25519 key pair. Persist `seed`.
#[wasm_bindgen]
pub struct Identity(group::Identity);

#[wasm_bindgen]
impl Identity {
    pub fn generate() -> Identity {
        Identity(group::Identity::generate())
    }

    #[wasm_bindgen(js_name = fromSeed)]
    pub fn from_seed(seed: &[u8]) -> Result<Identity, JsError> {
        let seed: [u8; 32] = seed.try_into().map_err(|_| JsError::new("an identity seed is 32 bytes"))?;
        Ok(Identity(group::Identity::from_seed(seed)))
    }

    #[wasm_bindgen(getter)]
    pub fn seed(&self) -> Vec<u8> {
        self.0.seed().to_vec()
    }

    /// The member ID others know this identity by.
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.0.id().to_string()
    }
}

/// The member state machine (see `peerconnect::group::Member`).
#[wasm_bindgen]
pub struct Member(group::Member);

#[wasm_bindgen]
impl Member {
    #[wasm_bindgen(constructor)]
    pub fn new(group: &Group, identity: &Identity, profile: &[u8], now: f64) -> Result<Member, JsError> {
        let config = group::Config { profile: profile.to_vec(), ..group::Config::default() };
        Ok(Member(group::Member::new(group.0.clone(), identity.0.clone(), config, now as u64)?))
    }

    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.0.id().to_string()
    }

    /// The latest round trip to the relay in milliseconds, if measured.
    #[wasm_bindgen(getter)]
    pub fn rtt(&self) -> Option<f64> {
        self.0.rtt().map(|t| t as f64)
    }

    /// The admitted members: `[{ id, profile }]`.
    pub fn members(&self) -> js_sys::Array {
        self.0
            .members()
            .into_iter()
            .map(|(id, profile)| {
                let entry = Object::new();
                Reflect::set(&entry, &"id".into(), &id.to_string().into()).unwrap();
                Reflect::set(&entry, &"profile".into(), &Uint8Array::from(profile.as_slice())).unwrap();
                JsValue::from(entry)
            })
            .collect()
    }

    #[wasm_bindgen(js_name = handleOpen)]
    pub fn handle_open(&mut self, now: f64) {
        self.0.handle_open(now as u64);
    }

    #[wasm_bindgen(js_name = handleBinary)]
    pub fn handle_binary(&mut self, now: f64, frame: &[u8]) {
        self.0.handle_binary(now as u64, frame);
    }

    #[wasm_bindgen(js_name = handleText)]
    pub fn handle_text(&mut self, now: f64, text: &str) {
        self.0.handle_text(now as u64, text);
    }

    #[wasm_bindgen(js_name = handleClose)]
    pub fn handle_close(&mut self, now: f64, code: Option<u16>) {
        self.0.handle_close(now as u64, code);
    }

    #[wasm_bindgen(js_name = handleTimeout)]
    pub fn handle_timeout(&mut self, now: f64) {
        self.0.handle_timeout(now as u64);
    }

    #[wasm_bindgen(js_name = pollTimeout)]
    pub fn poll_timeout(&self) -> Option<f64> {
        self.0.poll_timeout().map(|t| t as f64)
    }

    /// The next output as `{ type, ... }`, or `undefined`: `connect {url}`,
    /// `send {data, class}` (0 command, 1 voice, 2 chat, 3 bulk), `ping`,
    /// `disconnect`, or an event (`state`, `joined`, `left`, `message`,
    /// `voice`, `datagram`, `missed`, `syncrequest`, `snapshot`,
    /// `syncfailed`, `replaced`, `full`).
    #[wasm_bindgen(js_name = pollOutput)]
    pub fn poll_output(&mut self) -> JsValue {
        use group::{Event, Output, State};
        let Some(output) = self.0.poll_output() else { return JsValue::UNDEFINED };
        let out = Object::new();
        let set = |key: &str, value: JsValue| {
            Reflect::set(&out, &key.into(), &value).unwrap();
        };
        let bytes = |data: &[u8]| JsValue::from(Uint8Array::from(data));
        let id = |member: group::MemberId| JsValue::from(member.to_string());
        let kind = |name: &str| set("type", name.into());
        match output {
            Output::Connect(url) => {
                kind("connect");
                set("url", url.into());
            }
            Output::Send(frame, class) => {
                kind("send");
                set("data", bytes(&frame));
                set("class", (class as u8).into());
            }
            Output::Ping => kind("ping"),
            Output::Disconnect => kind("disconnect"),
            Output::Event(event) => match event {
                Event::State(state) => {
                    kind("state");
                    let name = match state {
                        State::Offline => "offline",
                        State::Online => "online",
                        State::Closed => "closed",
                    };
                    set("state", name.into());
                }
                Event::Joined { member, profile } => {
                    kind("joined");
                    set("member", id(member));
                    set("profile", bytes(&profile));
                }
                Event::Left { member } => {
                    kind("left");
                    set("member", id(member));
                }
                Event::Message { from, channel, data, direct } => {
                    kind("message");
                    set("from", id(from));
                    set("channel", channel_name(channel).into());
                    set("data", bytes(&data));
                    set("direct", direct.into());
                }
                Event::Voice { from, seq, data } => {
                    kind("voice");
                    set("from", id(from));
                    set("seq", seq.into());
                    set("data", bytes(&data));
                }
                Event::Datagram { from, data } => {
                    kind("datagram");
                    set("from", id(from));
                    set("data", bytes(&data));
                }
                Event::Missed { from, channel, count } => {
                    kind("missed");
                    set("from", id(from));
                    set("channel", channel_name(channel).into());
                    set("count", (count as f64).into());
                }
                Event::SyncRequest { from, transfer } => {
                    kind("syncrequest");
                    set("from", id(from));
                    set("transfer", transfer.into());
                }
                Event::Snapshot { from, data } => {
                    kind("snapshot");
                    set("from", id(from));
                    set("data", bytes(&data));
                }
                Event::SyncFailed { from } => {
                    kind("syncfailed");
                    set("from", id(from));
                }
                Event::Replaced => kind("replaced"),
                Event::Full => kind("full"),
            },
        }
        out.into()
    }

    /// Broadcasts a message on `"command"` or `"chat"`; returns its id.
    pub fn send(&mut self, channel: &str, data: &[u8]) -> Result<f64, JsError> {
        Ok(self.0.send(channel_of(channel)?, data)? as f64)
    }

    #[wasm_bindgen(js_name = sendTo)]
    pub fn send_to(&mut self, member: &str, channel: &str, data: &[u8]) -> Result<(), JsError> {
        Ok(self.0.send_to(member_of(member)?, channel_of(channel)?, data)?)
    }

    #[wasm_bindgen(js_name = sendVoice)]
    pub fn send_voice(&mut self, seq: u16, data: &[u8]) -> bool {
        self.0.send_voice(seq, data)
    }

    #[wasm_bindgen(js_name = sendDatagram)]
    pub fn send_datagram(&mut self, to: Option<String>, data: &[u8]) -> Result<bool, JsError> {
        let to = to.as_deref().map(member_of).transpose()?;
        Ok(self.0.send_datagram(to, data))
    }

    #[wasm_bindgen(js_name = requestSync)]
    pub fn request_sync(&mut self, from: &str) -> Result<(), JsError> {
        Ok(self.0.request_sync(member_of(from)?)?)
    }

    #[wasm_bindgen(js_name = provideSync)]
    pub fn provide_sync(&mut self, to: &str, transfer: u32, snapshot: &[u8]) -> Result<(), JsError> {
        Ok(self.0.provide_sync(member_of(to)?, transfer, snapshot.to_vec())?)
    }

    #[wasm_bindgen(js_name = declineSync)]
    pub fn decline_sync(&mut self, to: &str, transfer: u32) -> Result<(), JsError> {
        self.0.decline_sync(member_of(to)?, transfer);
        Ok(())
    }

    pub fn leave(&mut self) {
        self.0.leave();
    }
}

fn channel_of(name: &str) -> Result<Channel, JsError> {
    match name {
        "command" => Ok(Channel::Command),
        "chat" => Ok(Channel::Chat),
        _ => Err(JsError::new("channel must be \"command\" or \"chat\"")),
    }
}

fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Command => "command",
        Channel::Chat => "chat",
    }
}

fn member_of(id: &str) -> Result<group::MemberId, JsError> {
    id.parse().map_err(|_| JsError::new("not a member ID"))
}

const _: () = assert!(Class::Command as u8 == 0 && Class::Bulk as u8 == 3);
