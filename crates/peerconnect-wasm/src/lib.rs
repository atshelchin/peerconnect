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
    /// `token`: for a relay that requires one (SPEC §4.1).
    #[wasm_bindgen(constructor)]
    pub fn new(room: &Room, now: f64, token: Option<String>) -> Peer {
        Peer(peerconnect::Peer::with_token(room.0.clone(), token, now as u64))
    }

    /// A peer for a relay that requires a token not fetched yet: it stays
    /// offline until `setToken`.
    #[wasm_bindgen(js_name = awaitingToken)]
    pub fn awaiting_token(room: &Room, now: f64) -> Peer {
        Peer(peerconnect::Peer::awaiting_token(room.0.clone(), now as u64))
    }

    /// Replaces the relay token; after `unauthorized`, connects at once.
    #[wasm_bindgen(js_name = setToken)]
    pub fn set_token(&mut self, now: f64, token: Option<String>) {
        self.0.set_token(now as u64, token);
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
    /// `paired`, `replaced`, `unauthorized`.
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
            Output::Event(Event::Unauthorized) => set("type", "unauthorized".into()),
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
