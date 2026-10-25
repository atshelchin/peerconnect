//! The C ABI end to end: two members driven only through `pc_*` functions,
//! each with its own WebSocket, through a real relay. Also compiles a C
//! program against the header and the static library, where a C compiler
//! is available.

use std::ffi::{CStr, CString};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use peerconnect_ffi::*;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// What a game loop would do each frame: drive the member, and collect events.
struct Driver {
    m: *mut PcMember,
    seed: [u8; 32],
    /// The transfer id of the latest PC_SYNC_REQUEST.
    transfer: Option<u32>,
    ws: Option<Ws>,
    start: Instant,
    events: Vec<(i32, [u8; 32], Vec<u8>)>,
}

impl Driver {
    fn new(invite: &str, name: &str) -> Driver {
        let mut seed = [0u8; 32];
        unsafe { pc_identity_generate(seed.as_mut_ptr()) };
        Driver::with_seed(invite, name, seed)
    }

    fn with_seed(invite: &str, name: &str, seed: [u8; 32]) -> Driver {
        let invite = CString::new(invite).unwrap();
        let m = unsafe { pc_member_new(invite.as_ptr(), seed.as_ptr(), name.as_ptr(), name.len(), 0) };
        assert!(!m.is_null());
        Driver { m, seed, transfer: None, ws: None, start: Instant::now(), events: Vec::new() }
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn id(&self) -> [u8; 32] {
        let mut id = [0u8; 32];
        unsafe { pc_member_id(self.m, id.as_mut_ptr()) };
        id
    }

    async fn tick(&mut self) {
        let now = self.now();
        let due = unsafe { pc_member_poll_timeout(self.m) };
        if due >= 0 && due as u64 <= now {
            unsafe { pc_member_handle_timeout(self.m, now) };
        }
        loop {
            let mut out = std::mem::MaybeUninit::<PcOutput>::uninit();
            let kind = unsafe { pc_member_poll(self.m, out.as_mut_ptr()) };
            if kind == PC_NONE {
                break;
            }
            let out = unsafe { out.assume_init() };
            let data = unsafe { std::slice::from_raw_parts(out.data, out.len) }.to_vec();
            match kind {
                PC_CONNECT => {
                    let url = unsafe { CStr::from_ptr(out.data as *const _) }.to_str().unwrap().to_owned();
                    assert_eq!(url.len(), out.len);
                    let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
                    self.ws = Some(ws);
                    unsafe { pc_member_handle_open(self.m, self.now()) };
                }
                PC_SEND => self.ws.as_mut().unwrap().send(Message::binary(data)).await.unwrap(),
                PC_PING => self.ws.as_mut().unwrap().send(Message::text("ping")).await.unwrap(),
                PC_DISCONNECT => self.ws = None,
                _ => {
                    if kind == PC_SYNC_REQUEST {
                        self.transfer = Some(out.transfer);
                    }
                    self.events.push((kind, out.member, data))
                }
            }
        }
        if let Some(ws) = &mut self.ws
            && let Ok(msg) = tokio::time::timeout(Duration::from_millis(5), ws.next()).await
        {
            let now = self.now();
            match msg {
                Some(Ok(Message::Binary(b))) => unsafe { pc_member_handle_binary(self.m, now, b.as_ptr(), b.len()) },
                Some(Ok(Message::Text(t))) => unsafe {
                    pc_member_handle_text(self.m, now, t.as_ptr() as *const _, t.len())
                },
                Some(Ok(Message::Close(frame))) => {
                    self.ws = None;
                    let code = frame.map_or(-1, |f| u16::from(f.code) as i32);
                    unsafe { pc_member_handle_close(self.m, now, code) };
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    self.ws = None;
                    unsafe { pc_member_handle_close(self.m, now, -1) };
                }
            }
        }
    }

    fn kinds(&self) -> Vec<i32> {
        self.events.iter().map(|(k, _, _)| *k).collect()
    }

    fn has(&self, kind: i32, data: &[u8]) -> bool {
        self.events.iter().any(|(k, _, d)| *k == kind && d == data)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        unsafe { pc_member_free(self.m) };
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_members_through_the_c_abi() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(peerconnect_relay::serve(listener, Default::default(), std::future::pending()));

    let relay_c = CString::new(relay).unwrap();
    let invite = unsafe { pc_group_create(relay_c.as_ptr()) };
    assert!(!invite.is_null());
    let text = unsafe { CStr::from_ptr(invite) }.to_str().unwrap().to_owned();
    unsafe { pc_string_free(invite) };

    let mut a = Driver::new(&text, "alice");
    let mut b = Driver::new(&text, "bob");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(a.has(PC_JOINED, b"bob") && b.has(PC_JOINED, b"alice")) {
        assert!(Instant::now() < deadline, "not admitted");
        a.tick().await;
        b.tick().await;
    }
    let mut members = [[0u8; 32]; 4];
    assert_eq!(unsafe { pc_member_members(a.m, members.as_mut_ptr(), 4) }, 1);
    assert_eq!(members[0], b.id());

    let mut id = 99;
    assert_eq!(unsafe { pc_member_send(a.m, 0, b"place block".as_ptr(), 11, &mut id) }, PC_OK);
    assert_eq!(id, 0);
    assert_eq!(unsafe { pc_member_send_voice(a.m, 3, b"opus".as_ptr(), 4) }, 1);
    let b_id = b.id();
    assert_eq!(unsafe { pc_member_send_to(a.m, b_id.as_ptr(), 1, b"psst".as_ptr(), 4) }, PC_OK);
    assert_eq!(unsafe { pc_member_send(a.m, 7, b"x".as_ptr(), 1, std::ptr::null_mut()) }, PC_INVALID);
    let big = vec![0u8; 20_000];
    assert_eq!(unsafe { pc_member_send(a.m, 0, big.as_ptr(), big.len(), std::ptr::null_mut()) }, PC_TOO_LARGE);
    let stranger = [7u8; 32];
    assert_eq!(unsafe { pc_member_send_to(a.m, stranger.as_ptr(), 0, b"x".as_ptr(), 1) }, PC_NOT_PRESENT);

    while !(b.has(PC_MESSAGE, b"place block") && b.has(PC_VOICE, b"opus") && b.has(PC_MESSAGE, b"psst")) {
        assert!(Instant::now() < deadline, "not delivered: {:?}", b.events);
        a.tick().await;
        b.tick().await;
    }
    assert!(a.has(PC_MESSAGE, b"place block"), "own messages are delivered too");

    unsafe { pc_member_leave(b.m) };
    while !a.events.iter().any(|(k, m, _)| *k == PC_LEFT && *m == b_id) {
        assert!(Instant::now() < deadline, "no PC_LEFT");
        a.tick().await;
        b.tick().await;
    }
}

#[test]
fn null_pointers_are_refused_not_followed() {
    unsafe {
        assert!(pc_group_create(std::ptr::null()).is_null());
        assert!(pc_member_new(std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, 0).is_null());
        let bad = CString::new("not an invite").unwrap();
        assert!(pc_member_new(bad.as_ptr(), [0u8; 32].as_ptr(), std::ptr::null(), 0, 0).is_null());
        assert_eq!(pc_member_send(std::ptr::null_mut(), 0, std::ptr::null(), 0, std::ptr::null_mut()), PC_INVALID);
        assert_eq!(pc_member_poll(std::ptr::null_mut(), std::ptr::null_mut()), PC_NONE);
        assert_eq!(pc_member_poll_timeout(std::ptr::null()), -1);
        pc_member_free(std::ptr::null_mut());
        pc_string_free(std::ptr::null_mut());
    }
}

#[test]
fn the_header_compiles_and_links_from_c() {
    let Ok(cc) = std::process::Command::new("cc").arg("--version").output() else {
        eprintln!("no C compiler; skipped");
        return;
    };
    assert!(cc.status.success());
    let dir = env!("CARGO_MANIFEST_DIR");
    // The static library is built next to the test binary's deps.
    let exe = std::env::current_exe().unwrap();
    let target = exe.parent().unwrap().parent().unwrap();
    let lib = target.join("libpeerconnect_ffi.a");
    if !lib.exists() {
        eprintln!("{} not built; skipped", lib.display());
        return;
    }
    let out = std::env::temp_dir().join(format!("peerconnect-c-{}", std::process::id()));
    let mut cmd = std::process::Command::new("cc");
    cmd.args(["-std=c99", "-Wall", "-Werror", "-I"]).arg(format!("{dir}/include"));
    cmd.arg(format!("{dir}/tests/smoke.c")).arg(&lib).arg("-o").arg(&out);
    if cfg!(target_os = "macos") {
        cmd.args(["-framework", "Security", "-framework", "CoreFoundation"]);
    } else {
        cmd.args(["-lpthread", "-ldl", "-lm"]);
    }
    let status = cmd.status().unwrap();
    assert!(status.success(), "C compilation failed");
    let run = std::process::Command::new(&out).output().unwrap();
    let _ = std::fs::remove_file(&out);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
}

async fn relay_with(config: peerconnect_relay::Config) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(peerconnect_relay::serve(listener, config, std::future::pending()));
    let relay_c = CString::new(relay).unwrap();
    let invite = unsafe { pc_group_create(relay_c.as_ptr()) };
    let text = unsafe { CStr::from_ptr(invite) }.to_str().unwrap().to_owned();
    unsafe { pc_string_free(invite) };
    text
}

async fn until(drivers: &mut [&mut Driver], done: impl Fn(&[&mut Driver]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done(drivers) {
        assert!(Instant::now() < deadline, "timed out: {:?}", drivers.iter().map(|d| d.kinds()).collect::<Vec<_>>());
        for d in drivers.iter_mut() {
            d.tick().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn state_transfer_datagrams_replacement_and_a_full_group_through_the_c_abi() {
    let config = peerconnect_relay::Config { max_group_members: 2, ..Default::default() };
    let invite = relay_with(config).await;
    let mut a = Driver::new(&invite, "a");
    let mut b = Driver::new(&invite, "b");
    until(&mut [&mut a, &mut b], |d| d[0].has(PC_JOINED, b"b") && d[1].has(PC_JOINED, b"a")).await;
    let (ia, ib) = (a.id(), b.id());
    let mut seed = [0u8; 32];
    unsafe { pc_identity_generate(seed.as_mut_ptr()) };
    let mut id = [0u8; 32];
    unsafe { pc_identity_id(seed.as_ptr(), id.as_mut_ptr()) };
    assert_ne!(id, [0; 32]);

    // A datagram, and a state transfer from a to b.
    assert_eq!(unsafe { pc_member_send_datagram(a.m, ib.as_ptr(), b"pose".as_ptr(), 4) }, 1);
    assert_eq!(unsafe { pc_member_send_datagram(a.m, std::ptr::null(), b"all".as_ptr(), 3) }, 1);
    assert_eq!(unsafe { pc_member_request_sync(b.m, ia.as_ptr()) }, PC_OK);
    assert_eq!(unsafe { pc_member_request_sync(b.m, ia.as_ptr()) }, PC_BUSY);
    assert_eq!(unsafe { pc_member_request_sync(b.m, std::ptr::null()) }, PC_INVALID);
    until(&mut [&mut a, &mut b], |d| d[0].kinds().contains(&PC_SYNC_REQUEST)).await;
    let transfer = a.transfer.unwrap();
    let world = b"the whole world";
    assert_eq!(unsafe { pc_member_provide_sync(a.m, ib.as_ptr(), transfer, world.as_ptr(), world.len()) }, PC_OK);
    assert_eq!(unsafe { pc_member_provide_sync(a.m, ib.as_ptr(), transfer, world.as_ptr(), 1) }, PC_BUSY);
    until(&mut [&mut a, &mut b], |d| d[1].has(PC_SNAPSHOT, world)).await;
    assert!(b.has(PC_DATAGRAM, b"pose") && b.has(PC_DATAGRAM, b"all"));

    // A declined transfer.
    assert_eq!(unsafe { pc_member_request_sync(b.m, ia.as_ptr()) }, PC_OK);
    a.transfer = None;
    until(&mut [&mut a, &mut b], |d| d[0].transfer.is_some()).await;
    unsafe { pc_member_decline_sync(a.m, ib.as_ptr(), a.transfer.unwrap()) };
    until(&mut [&mut a, &mut b], |d| d[1].kinds().contains(&PC_SYNC_FAILED)).await;

    // The group holds two: a third is refused.
    let mut c = Driver::new(&invite, "c");
    until(&mut [&mut a, &mut b, &mut c], |d| d[2].kinds().contains(&PC_FULL)).await;
    drop(c);

    // b's identity joining again elsewhere replaces b.
    let mut b2 = Driver::with_seed(&invite, "b again", b.seed);
    until(&mut [&mut a, &mut b, &mut b2], |d| d[1].kinds().contains(&PC_REPLACED) && d[0].has(PC_JOINED, b"b again"))
        .await;
    assert!(unsafe { pc_member_rtt(a.m) } >= -1);
}
