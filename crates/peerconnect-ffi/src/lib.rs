//! C ABI for PeerConnect groups: see `include/peerconnect.h`, whose comments
//! are the documentation. `pc_client_*` wraps the Tokio client
//! ([`peerconnect::client::group`]), which connects over QUIC on the
//! library's own threads; `pc_member_*` wraps [`peerconnect::group::Member`]
//! one to one, for applications that bring their own QUIC.
//!
//! Every function checks its pointers for NULL; beyond that, callers must
//! pass valid pointers of the stated lengths.

#![allow(clippy::missing_safety_doc)] // the header documents the contract

use std::ffi::{CStr, CString, c_char};
use std::future::Future;
use std::slice;
use std::sync::OnceLock;

use peerconnect::client::group::{self as client, GroupConnection};
use peerconnect::group::{Channel, Config, Event, Group, Identity, Member, MemberId, Output, SendError, State};
use tokio::runtime::Runtime;

pub const PC_OK: i32 = 0;
pub const PC_TOO_LARGE: i32 = 1;
pub const PC_NOT_PRESENT: i32 = 2;
pub const PC_BUSY: i32 = 3;
pub const PC_CLOSED: i32 = 4;
pub const PC_INVALID: i32 = -1;

pub const PC_NONE: i32 = 0;
pub const PC_CONNECT: i32 = 1;
pub const PC_SEND: i32 = 2;
pub const PC_PING: i32 = 3;
pub const PC_DISCONNECT: i32 = 4;
pub const PC_STATE: i32 = 10;
pub const PC_JOINED: i32 = 11;
pub const PC_LEFT: i32 = 12;
pub const PC_MESSAGE: i32 = 13;
pub const PC_VOICE: i32 = 14;
pub const PC_DATAGRAM: i32 = 15;
pub const PC_MISSED: i32 = 16;
pub const PC_SYNC_REQUEST: i32 = 17;
pub const PC_SNAPSHOT: i32 = 18;
pub const PC_SYNC_FAILED: i32 = 19;
pub const PC_REPLACED: i32 = 20;
pub const PC_FULL: i32 = 21;
pub const PC_VIDEO: i32 = 22;
pub const PC_KEYFRAME_REQUEST: i32 = 23;
pub const PC_VIDEO_DEMAND: i32 = 24;
pub const PC_VIDEO_LIMIT: i32 = 25;

#[repr(C)]
pub struct PcOutput {
    pub kind: i32,
    pub member: [u8; 32],
    pub channel: u8,
    pub direct: u8,
    pub class_: u8,
    pub state: u8,
    pub seq: u16,
    pub transfer: u32,
    pub count: u64,
    pub data: *const u8,
    pub len: usize,
    pub track: u8,
    pub keyframe: u8,
    pub timestamp: u32,
    pub layer: u8,
}

impl PcOutput {
    const NONE: PcOutput = PcOutput {
        kind: PC_NONE,
        member: [0; 32],
        channel: 0,
        direct: 0,
        class_: 0,
        state: 0,
        seq: 0,
        transfer: 0,
        count: 0,
        data: std::ptr::null(),
        len: 0,
        track: 0,
        keyframe: 0,
        timestamp: 0,
        layer: 0,
    };
}

pub struct PcMember {
    member: Member,
    /// The bytes the last output points at.
    held: Vec<u8>,
}

unsafe fn bytes<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if data.is_null() || len == 0 { &[] } else { unsafe { slice::from_raw_parts(data, len) } }
}

unsafe fn id(member: *const u8) -> Option<MemberId> {
    (!member.is_null()).then(|| MemberId(unsafe { *(member as *const [u8; 32]) }))
}

fn channel(channel: u8) -> Option<Channel> {
    match channel {
        0 => Some(Channel::Command),
        1 => Some(Channel::Chat),
        _ => None,
    }
}

fn code(result: Result<(), SendError>) -> i32 {
    match result {
        Ok(()) => PC_OK,
        Err(SendError::TooLarge) => PC_TOO_LARGE,
        Err(SendError::NotPresent) => PC_NOT_PRESENT,
        Err(SendError::Busy) => PC_BUSY,
        Err(SendError::Closed) => PC_CLOSED,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_group_create(relay: *const c_char) -> *mut c_char {
    if relay.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(relay) = unsafe { CStr::from_ptr(relay) }.to_str() else { return std::ptr::null_mut() };
    match Group::create(relay) {
        Ok(group) => CString::new(group.to_string()).unwrap().into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_identity_generate(seed: *mut u8) {
    if !seed.is_null() {
        unsafe { *(seed as *mut [u8; 32]) = *Identity::generate().seed() };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_identity_id(seed: *const u8, out: *mut u8) {
    if !seed.is_null() && !out.is_null() {
        let identity = Identity::from_seed(unsafe { *(seed as *const [u8; 32]) });
        unsafe { *(out as *mut [u8; 32]) = identity.id().0 };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_new(
    invite: *const c_char,
    seed: *const u8,
    profile: *const u8,
    profile_len: usize,
    now: u64,
) -> *mut PcMember {
    if invite.is_null() || seed.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(Ok(group)) = unsafe { CStr::from_ptr(invite) }.to_str().map(str::parse::<Group>) else {
        return std::ptr::null_mut();
    };
    let identity = Identity::from_seed(unsafe { *(seed as *const [u8; 32]) });
    let config = Config { profile: unsafe { bytes(profile, profile_len) }.to_vec(), ..Config::default() };
    match Member::new(group, identity, config, now) {
        Ok(member) => Box::into_raw(Box::new(PcMember { member, held: Vec::new() })),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_free(m: *mut PcMember) {
    if !m.is_null() {
        drop(unsafe { Box::from_raw(m) });
    }
}

/// Runs `f` on the member behind `m`, or returns `default` for NULL.
unsafe fn with<T>(m: *mut PcMember, default: T, f: impl FnOnce(&mut PcMember) -> T) -> T {
    match unsafe { m.as_mut() } {
        Some(m) => f(m),
        None => default,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_id(m: *const PcMember, out: *mut u8) {
    if let (Some(m), false) = (unsafe { m.as_ref() }, out.is_null()) {
        unsafe { *(out as *mut [u8; 32]) = m.member.id().0 };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_members(m: *const PcMember, ids: *mut [u8; 32], max: usize) -> usize {
    let Some(m) = (unsafe { m.as_ref() }) else { return 0 };
    let members = m.member.members();
    if !ids.is_null() {
        for (i, (id, _)) in members.iter().take(max).enumerate() {
            unsafe { *ids.add(i) = id.0 };
        }
    }
    members.len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_rtt(m: *const PcMember) -> i64 {
    unsafe { m.as_ref() }.and_then(|m| m.member.rtt()).map_or(-1, |t| t as i64)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_handle_open(m: *mut PcMember, now: u64) {
    unsafe { with(m, (), |m| m.member.handle_open(now)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_handle_binary(m: *mut PcMember, now: u64, data: *const u8, len: usize) {
    unsafe { with(m, (), |m| m.member.handle_binary(now, bytes(data, len))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_handle_text(m: *mut PcMember, now: u64, text: *const c_char, len: usize) {
    let text = unsafe { bytes(text as *const u8, len) };
    unsafe { with(m, (), |m| m.member.handle_text(now, std::str::from_utf8(text).unwrap_or(""))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_handle_close(m: *mut PcMember, now: u64, code: i32) {
    let code = u16::try_from(code).ok();
    unsafe { with(m, (), |m| m.member.handle_close(now, code)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_handle_timeout(m: *mut PcMember, now: u64) {
    unsafe { with(m, (), |m| m.member.handle_timeout(now)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_poll_timeout(m: *const PcMember) -> i64 {
    unsafe { m.as_ref() }.and_then(|m| m.member.poll_timeout()).map_or(-1, |t| t as i64)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_poll(m: *mut PcMember, out: *mut PcOutput) -> i32 {
    let (Some(m), Some(out)) = (unsafe { m.as_mut() }, unsafe { out.as_mut() }) else { return PC_NONE };
    *out = PcOutput::NONE;
    let Some(output) = m.member.poll_output() else { return PC_NONE };
    let (kind, held) = match output {
        Output::Connect(url) => {
            let mut url = url.into_bytes();
            out.len = url.len();
            url.push(0);
            (PC_CONNECT, url)
        }
        Output::Send(frame, class) => {
            out.class_ = class as u8;
            (PC_SEND, frame)
        }
        Output::Ping => (PC_PING, Vec::new()),
        Output::Disconnect => (PC_DISCONNECT, Vec::new()),
        Output::Event(event) => fill(event, out),
    };
    m.held = held;
    out.kind = kind;
    out.data = m.held.as_ptr();
    if kind != PC_CONNECT {
        out.len = m.held.len();
    }
    kind
}

/// Writes an event's fields into `out`; returns its kind and the bytes `data` points at.
fn fill(event: Event, out: &mut PcOutput) -> (i32, Vec<u8>) {
    match event {
        Event::State(state) => {
            out.state = match state {
                State::Offline => 0,
                State::Online => 1,
                State::Closed => 2,
            };
            (PC_STATE, Vec::new())
        }
        Event::Joined { member, profile } => {
            out.member = member.0;
            (PC_JOINED, profile)
        }
        Event::Left { member } => {
            out.member = member.0;
            (PC_LEFT, Vec::new())
        }
        Event::Message { from, channel, data, direct } => {
            out.member = from.0;
            out.channel = channel as u8;
            out.direct = direct as u8;
            (PC_MESSAGE, data)
        }
        Event::Voice { from, seq, data } => {
            out.member = from.0;
            out.seq = seq;
            (PC_VOICE, data)
        }
        Event::Datagram { from, data } => {
            out.member = from.0;
            (PC_DATAGRAM, data)
        }
        Event::Missed { from, channel, count } => {
            out.member = from.0;
            out.channel = channel as u8;
            out.count = count;
            (PC_MISSED, Vec::new())
        }
        Event::SyncRequest { from, transfer } => {
            out.member = from.0;
            out.transfer = transfer;
            (PC_SYNC_REQUEST, Vec::new())
        }
        Event::Snapshot { from, data } => {
            out.member = from.0;
            (PC_SNAPSHOT, data)
        }
        Event::SyncFailed { from } => {
            out.member = from.0;
            (PC_SYNC_FAILED, Vec::new())
        }
        Event::Video { from, track, layer, keyframe, timestamp, data } => {
            out.member = from.0;
            out.track = track;
            out.layer = layer;
            out.keyframe = keyframe as u8;
            out.timestamp = timestamp;
            (PC_VIDEO, data)
        }
        Event::KeyframeRequest { from, track, layer } => {
            out.member = from.0;
            out.track = track;
            out.layer = layer;
            (PC_KEYFRAME_REQUEST, Vec::new())
        }
        Event::VideoDemand { track, layers } => {
            out.track = track;
            out.layer = layers;
            (PC_VIDEO_DEMAND, Vec::new())
        }
        Event::VideoLimit { layer } => {
            out.layer = layer.unwrap_or(0xFF);
            (PC_VIDEO_LIMIT, Vec::new())
        }
        Event::Replaced => (PC_REPLACED, Vec::new()),
        Event::Full => (PC_FULL, Vec::new()),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_send(m: *mut PcMember, ch: u8, data: *const u8, len: usize, id: *mut u64) -> i32 {
    let Some(ch) = channel(ch) else { return PC_INVALID };
    let data = unsafe { bytes(data, len) };
    unsafe {
        with(m, PC_INVALID, |m| match m.member.send(ch, data) {
            Ok(sent) => {
                if let Some(id) = id.as_mut() {
                    *id = sent;
                }
                PC_OK
            }
            Err(e) => code(Err(e)),
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_send_to(
    m: *mut PcMember,
    to: *const u8,
    ch: u8,
    data: *const u8,
    len: usize,
) -> i32 {
    let (Some(to), Some(ch)) = (unsafe { id(to) }, channel(ch)) else { return PC_INVALID };
    let data = unsafe { bytes(data, len) };
    unsafe { with(m, PC_INVALID, |m| code(m.member.send_to(to, ch, data))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_send_voice(m: *mut PcMember, seq: u16, data: *const u8, len: usize) -> i32 {
    let data = unsafe { bytes(data, len) };
    unsafe { with(m, 0, |m| m.member.send_voice(seq, data) as i32) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_send_datagram(m: *mut PcMember, to: *const u8, data: *const u8, len: usize) -> i32 {
    let to = unsafe { id(to) };
    let data = unsafe { bytes(data, len) };
    unsafe { with(m, 0, |m| m.member.send_datagram(to, data) as i32) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_send_video(
    m: *mut PcMember,
    track: u8,
    layer: u8,
    keyframe: u8,
    timestamp: u32,
    data: *const u8,
    len: usize,
) -> i32 {
    let data = unsafe { bytes(data, len) };
    unsafe { with(m, 0, |m| m.member.send_video(track, layer, keyframe != 0, timestamp, data) as i32) }
}

/// `layer` 0xFF: none of `member`'s video `track`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_subscribe(m: *mut PcMember, member: *const u8, track: u8, layer: u8) -> i32 {
    let Some(member) = (unsafe { id(member) }) else { return PC_INVALID };
    let layer = (layer != 0xFF).then_some(layer);
    unsafe { with(m, PC_INVALID, |m| code(m.member.subscribe(member, track, layer))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_request_sync(m: *mut PcMember, from: *const u8) -> i32 {
    let Some(from) = (unsafe { id(from) }) else { return PC_INVALID };
    unsafe { with(m, PC_INVALID, |m| code(m.member.request_sync(from))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_provide_sync(
    m: *mut PcMember,
    to: *const u8,
    transfer: u32,
    data: *const u8,
    len: usize,
) -> i32 {
    let Some(to) = (unsafe { id(to) }) else { return PC_INVALID };
    let data = unsafe { bytes(data, len) }.to_vec();
    unsafe { with(m, PC_INVALID, |m| code(m.member.provide_sync(to, transfer, data))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_decline_sync(m: *mut PcMember, to: *const u8, transfer: u32) {
    if let Some(to) = unsafe { id(to) } {
        unsafe { with(m, (), |m| m.member.decline_sync(to, transfer)) }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_member_leave(m: *mut PcMember) {
    unsafe { with(m, (), |m| m.member.leave()) }
}

// A member that connects by itself: the Tokio client on the library's runtime.

pub struct PcClient {
    conn: GroupConnection,
    /// The admitted members, as the events polled so far tell.
    members: Vec<MemberId>,
    /// The bytes the last event points at.
    held: Vec<u8>,
}

/// The runtime every client runs on: two threads, started with the first client.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("peerconnect")
            .enable_all()
            .build()
            .expect("a Tokio runtime")
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_join(
    invite: *const c_char,
    seed: *const u8,
    profile: *const u8,
    profile_len: usize,
) -> *mut PcClient {
    if invite.is_null() || seed.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(Ok(group)) = unsafe { CStr::from_ptr(invite) }.to_str().map(str::parse::<Group>) else {
        return std::ptr::null_mut();
    };
    let identity = Identity::from_seed(unsafe { *(seed as *const [u8; 32]) });
    let config = Config { profile: unsafe { bytes(profile, profile_len) }.to_vec(), ..Config::default() };
    let _runtime = runtime().enter();
    match client::join(group, identity, config) {
        Ok(conn) => Box::into_raw(Box::new(PcClient { conn, members: Vec::new(), held: Vec::new() })),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Leaves the group (what was sent is still delivered) and frees the client.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_free(c: *mut PcClient) {
    if !c.is_null() {
        let _runtime = runtime().enter();
        drop(unsafe { Box::from_raw(c) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_id(c: *const PcClient, out: *mut u8) {
    if let (Some(c), false) = (unsafe { c.as_ref() }, out.is_null()) {
        unsafe { *(out as *mut [u8; 32]) = c.conn.id().0 };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_connected(c: *const PcClient) -> i32 {
    unsafe { c.as_ref() }.is_some_and(|c| c.conn.connected()) as i32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_members(c: *const PcClient, ids: *mut [u8; 32], max: usize) -> usize {
    let Some(c) = (unsafe { c.as_ref() }) else { return 0 };
    if !ids.is_null() {
        for (i, id) in c.members.iter().take(max).enumerate() {
            unsafe { *ids.add(i) = id.0 };
        }
    }
    c.members.len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_poll(c: *mut PcClient, out: *mut PcOutput) -> i32 {
    let (Some(c), Some(out)) = (unsafe { c.as_mut() }, unsafe { out.as_mut() }) else { return PC_NONE };
    *out = PcOutput::NONE;
    let Some(event) = c.conn.try_next() else { return PC_NONE };
    match &event {
        Event::Joined { member, .. } if !c.members.contains(member) => c.members.push(*member),
        Event::Left { member } => c.members.retain(|m| m != member),
        _ => {}
    }
    let (kind, held) = fill(event, out);
    c.held = held;
    out.kind = kind;
    out.data = c.held.as_ptr();
    out.len = c.held.len();
    kind
}

/// Runs `task` on the library's runtime and waits for it: from any thread,
/// including one that runs a Tokio runtime of its own.
fn wait<T: Send + 'static>(task: impl Future<Output = T> + Send + 'static) -> T {
    let (done, result) = std::sync::mpsc::sync_channel(1);
    runtime().spawn(async move {
        let _ = done.send(task.await);
    });
    result.recv().expect("the runtime outlives every task")
}

/// Runs `f` on the client behind `c`, or returns `default` for NULL.
unsafe fn with_client<T>(c: *mut PcClient, default: T, f: impl FnOnce(&mut PcClient) -> T) -> T {
    match unsafe { c.as_mut() } {
        Some(c) => f(c),
        None => default,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_send(c: *mut PcClient, ch: u8, data: *const u8, len: usize, id: *mut u64) -> i32 {
    let Some(ch) = channel(ch) else { return PC_INVALID };
    let data = unsafe { bytes(data, len) }.to_vec();
    unsafe {
        with_client(c, PC_INVALID, |c| {
            let sender = c.conn.sender();
            match wait(async move { sender.send(ch, data).await }) {
                Ok(sent) => {
                    if let Some(id) = id.as_mut() {
                        *id = sent;
                    }
                    PC_OK
                }
                Err(e) => code(Err(e)),
            }
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_send_to(
    c: *mut PcClient,
    to: *const u8,
    ch: u8,
    data: *const u8,
    len: usize,
) -> i32 {
    let (Some(to), Some(ch)) = (unsafe { id(to) }, channel(ch)) else { return PC_INVALID };
    let data = unsafe { bytes(data, len) }.to_vec();
    unsafe {
        with_client(c, PC_INVALID, |c| {
            let sender = c.conn.sender();
            code(wait(async move { sender.send_to(to, ch, data).await }))
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_send_voice(c: *mut PcClient, seq: u16, data: *const u8, len: usize) {
    let data = unsafe { bytes(data, len) }.to_vec();
    unsafe { with_client(c, (), |c| c.conn.send_voice(seq, data)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_send_datagram(c: *mut PcClient, to: *const u8, data: *const u8, len: usize) {
    let to = unsafe { id(to) };
    let data = unsafe { bytes(data, len) }.to_vec();
    unsafe { with_client(c, (), |c| c.conn.send_datagram(to, data)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_send_video(
    c: *mut PcClient,
    track: u8,
    layer: u8,
    keyframe: u8,
    timestamp: u32,
    data: *const u8,
    len: usize,
) {
    let data = unsafe { bytes(data, len) }.to_vec();
    unsafe { with_client(c, (), |c| c.conn.send_video(track, layer, keyframe != 0, timestamp, data)) }
}

/// `layer` 0xFF: none of `member`'s video `track`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_subscribe(c: *mut PcClient, member: *const u8, track: u8, layer: u8) -> i32 {
    let Some(member) = (unsafe { id(member) }) else { return PC_INVALID };
    use peerconnect::group::wire::{VIDEO_LAYERS, VIDEO_TRACKS};
    if track >= VIDEO_TRACKS || (layer != 0xFF && layer >= VIDEO_LAYERS) {
        return PC_INVALID;
    }
    let layer = (layer != 0xFF).then_some(layer);
    unsafe {
        with_client(c, PC_INVALID, |c| {
            c.conn.subscribe(member, track, layer);
            PC_OK
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_request_sync(c: *mut PcClient, from: *const u8) -> i32 {
    let Some(from) = (unsafe { id(from) }) else { return PC_INVALID };
    unsafe {
        with_client(c, PC_INVALID, |c| {
            let sender = c.conn.sender();
            code(wait(async move { sender.request_sync(from).await }))
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_provide_sync(
    c: *mut PcClient,
    to: *const u8,
    transfer: u32,
    data: *const u8,
    len: usize,
) -> i32 {
    let Some(to) = (unsafe { id(to) }) else { return PC_INVALID };
    let data = unsafe { bytes(data, len) }.to_vec();
    unsafe {
        with_client(c, PC_INVALID, |c| {
            c.conn.provide_sync(to, transfer, data);
            PC_OK
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_client_decline_sync(c: *mut PcClient, to: *const u8, transfer: u32) {
    if let Some(to) = unsafe { id(to) } {
        unsafe { with_client(c, (), |c| c.conn.decline_sync(to, transfer)) }
    }
}
