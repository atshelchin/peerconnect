//! The C ABI end to end: members driven only through `pc_client_*`
//! functions, each connecting over QUIC by itself, through a real relay.
//! Also compiles a C program against the header and the static library,
//! where a C compiler is available; it exercises `pc_member_*` offline.

use std::ffi::{CStr, CString};
use std::time::{Duration, Instant};

use peerconnect_ffi::*;
use tokio::net::TcpListener;

/// What a game loop would do each frame: poll the client, and collect events.
struct Driver {
    c: *mut PcClient,
    seed: [u8; 32],
    /// The transfer id of the latest PC_SYNC_REQUEST.
    transfer: Option<u32>,
    events: Vec<(i32, [u8; 32], Vec<u8>)>,
    /// PC_VIDEO: track, keyframe, timestamp, data (layer 0).
    videos: Vec<(u8, u8, u32, Vec<u8>)>,
    /// PC_VIDEO_DEMAND: the layers wanted of us.
    demands: Vec<u8>,
}

impl Driver {
    fn new(invite: &str, name: &str) -> Driver {
        let mut seed = [0u8; 32];
        unsafe { pc_identity_generate(seed.as_mut_ptr()) };
        Driver::with_seed(invite, name, seed)
    }

    fn with_seed(invite: &str, name: &str, seed: [u8; 32]) -> Driver {
        let invite = CString::new(invite).unwrap();
        let c = unsafe { pc_client_join(invite.as_ptr(), seed.as_ptr(), name.as_ptr(), name.len()) };
        assert!(!c.is_null());
        Driver { c, seed, transfer: None, events: Vec::new(), videos: Vec::new(), demands: Vec::new() }
    }

    fn id(&self) -> [u8; 32] {
        let mut id = [0u8; 32];
        unsafe { pc_client_id(self.c, id.as_mut_ptr()) };
        id
    }

    fn tick(&mut self) {
        loop {
            let mut out = std::mem::MaybeUninit::<PcOutput>::uninit();
            let kind = unsafe { pc_client_poll(self.c, out.as_mut_ptr()) };
            if kind == PC_NONE {
                break;
            }
            let out = unsafe { out.assume_init() };
            let data = unsafe { std::slice::from_raw_parts(out.data, out.len) }.to_vec();
            assert!(!matches!(kind, PC_CONNECT | PC_SEND | PC_PING | PC_DISCONNECT), "a client gives events only");
            if kind == PC_SYNC_REQUEST {
                self.transfer = Some(out.transfer);
            }
            if kind == PC_VIDEO {
                self.videos.push((out.track, out.keyframe, out.timestamp, data.clone()));
            }
            if kind == PC_VIDEO_DEMAND {
                self.demands.push(out.layer);
            }
            self.events.push((kind, out.member, data))
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
        unsafe { pc_client_free(self.c) };
    }
}

/// A relay serving groups over QUIC, and the invite of a new group on it.
async fn relay_with(config: peerconnect_relay::Config) -> String {
    // QUIC on the UDP port of the same number.
    let (listener, udp) = loop {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        if let Ok(udp) = std::net::UdpSocket::bind(listener.local_addr().unwrap()) {
            break (listener, udp);
        }
    };
    let relay = format!("quic://{}", listener.local_addr().unwrap());
    tokio::spawn(peerconnect_relay::serve_all(listener, Some(udp), config, std::future::pending()));
    let relay_c = CString::new(relay).unwrap();
    let invite = unsafe { pc_group_create(relay_c.as_ptr()) };
    assert!(!invite.is_null());
    let text = unsafe { CStr::from_ptr(invite) }.to_str().unwrap().to_owned();
    unsafe { pc_string_free(invite) };
    text
}

/// Polls every driver, as a game loop would once per frame, until `done`.
async fn until(drivers: &mut [&mut Driver], done: impl Fn(&[&mut Driver]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done(drivers) {
        assert!(Instant::now() < deadline, "timed out: {:?}", drivers.iter().map(|d| d.kinds()).collect::<Vec<_>>());
        for d in drivers.iter_mut() {
            d.tick();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_members_through_the_c_abi() {
    let invite = relay_with(Default::default()).await;
    let mut a = Driver::new(&invite, "alice");
    let mut b = Driver::new(&invite, "bob");
    until(&mut [&mut a, &mut b], |d| d[0].has(PC_JOINED, b"bob") && d[1].has(PC_JOINED, b"alice")).await;
    assert_eq!(unsafe { pc_client_connected(a.c) }, 1);
    let mut members = [[0u8; 32]; 4];
    assert_eq!(unsafe { pc_client_members(a.c, members.as_mut_ptr(), 4) }, 1);
    assert_eq!(members[0], b.id());

    let mut id = 99;
    assert_eq!(unsafe { pc_client_send(a.c, 0, b"place block".as_ptr(), 11, &mut id) }, PC_OK);
    assert_eq!(id, 0);
    unsafe { pc_client_send_voice(a.c, 3, b"opus".as_ptr(), 4) };
    let picture: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
    unsafe { pc_client_send_video(a.c, 2, 0, 1, 4321, picture.as_ptr(), picture.len()) };
    let b_id = b.id();
    assert_eq!(unsafe { pc_client_send_to(a.c, b_id.as_ptr(), 1, b"psst".as_ptr(), 4) }, PC_OK);
    assert_eq!(unsafe { pc_client_send(a.c, 7, b"x".as_ptr(), 1, std::ptr::null_mut()) }, PC_INVALID);
    let big = vec![0u8; 20_000];
    assert_eq!(unsafe { pc_client_send(a.c, 0, big.as_ptr(), big.len(), std::ptr::null_mut()) }, PC_TOO_LARGE);
    let stranger = [7u8; 32];
    assert_eq!(unsafe { pc_client_send_to(a.c, stranger.as_ptr(), 0, b"x".as_ptr(), 1) }, PC_NOT_PRESENT);

    until(&mut [&mut a, &mut b], |d| {
        let b = &d[1];
        b.has(PC_MESSAGE, b"place block")
            && b.has(PC_VOICE, b"opus")
            && b.has(PC_MESSAGE, b"psst")
            && !b.videos.is_empty()
    })
    .await;
    assert!(a.has(PC_MESSAGE, b"place block"), "own messages are delivered too");
    assert_eq!(b.videos, [(2, 1, 4321, picture)], "a video frame, whole");

    // a asks for layer 2 of b's video: the relay tells b.
    let a_id = a.id();
    assert_eq!(unsafe { pc_client_subscribe(a.c, b_id.as_ptr(), 2) }, PC_OK);
    assert_eq!(unsafe { pc_client_subscribe(a.c, b_id.as_ptr(), 9) }, PC_INVALID);
    until(&mut [&mut a, &mut b], |d| d[1].demands.last() == Some(&0b100)).await;
    assert_eq!(unsafe { pc_client_subscribe(b.c, a_id.as_ptr(), 0xFF) }, PC_OK);
    until(&mut [&mut a, &mut b], |d| d[0].demands.last() == Some(&0)).await;

    drop(b);
    until(&mut [&mut a], |d| d[0].events.iter().any(|(k, m, _)| *k == PC_LEFT && *m == b_id)).await;
    assert_eq!(unsafe { pc_client_members(a.c, std::ptr::null_mut(), 0) }, 0);
}

#[test]
fn null_pointers_are_refused_not_followed() {
    unsafe {
        assert!(pc_group_create(std::ptr::null()).is_null());
        assert!(pc_member_new(std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, 0).is_null());
        assert!(pc_client_join(std::ptr::null(), std::ptr::null(), std::ptr::null(), 0).is_null());
        let bad = CString::new("not an invite").unwrap();
        assert!(pc_member_new(bad.as_ptr(), [0u8; 32].as_ptr(), std::ptr::null(), 0, 0).is_null());
        assert!(pc_client_join(bad.as_ptr(), [0u8; 32].as_ptr(), std::ptr::null(), 0).is_null());
        assert_eq!(pc_member_send(std::ptr::null_mut(), 0, std::ptr::null(), 0, std::ptr::null_mut()), PC_INVALID);
        assert_eq!(pc_client_send(std::ptr::null_mut(), 0, std::ptr::null(), 0, std::ptr::null_mut()), PC_INVALID);
        assert_eq!(pc_member_poll(std::ptr::null_mut(), std::ptr::null_mut()), PC_NONE);
        assert_eq!(pc_client_poll(std::ptr::null_mut(), std::ptr::null_mut()), PC_NONE);
        assert_eq!(pc_member_poll_timeout(std::ptr::null()), -1);
        assert_eq!(pc_client_connected(std::ptr::null()), 0);
        pc_member_free(std::ptr::null_mut());
        pc_client_free(std::ptr::null_mut());
        pc_string_free(std::ptr::null_mut());
    }
}

#[test]
fn groups_have_no_websocket_relays() {
    let ws = CString::new("wss://relay.example.com").unwrap();
    assert!(unsafe { pc_group_create(ws.as_ptr()) }.is_null());
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
    unsafe { pc_client_send_datagram(a.c, ib.as_ptr(), b"pose".as_ptr(), 4) };
    unsafe { pc_client_send_datagram(a.c, std::ptr::null(), b"all".as_ptr(), 3) };
    assert_eq!(unsafe { pc_client_request_sync(b.c, ia.as_ptr()) }, PC_OK);
    assert_eq!(unsafe { pc_client_request_sync(b.c, ia.as_ptr()) }, PC_BUSY);
    assert_eq!(unsafe { pc_client_request_sync(b.c, std::ptr::null()) }, PC_INVALID);
    until(&mut [&mut a, &mut b], |d| d[0].kinds().contains(&PC_SYNC_REQUEST)).await;
    let transfer = a.transfer.unwrap();
    let world = b"the whole world";
    assert_eq!(unsafe { pc_client_provide_sync(a.c, ib.as_ptr(), transfer, world.as_ptr(), world.len()) }, PC_OK);
    until(&mut [&mut a, &mut b], |d| d[1].has(PC_SNAPSHOT, world)).await;
    until(&mut [&mut a, &mut b], |d| d[1].has(PC_DATAGRAM, b"pose") && d[1].has(PC_DATAGRAM, b"all")).await;

    // A declined transfer.
    assert_eq!(unsafe { pc_client_request_sync(b.c, ia.as_ptr()) }, PC_OK);
    a.transfer = None;
    until(&mut [&mut a, &mut b], |d| d[0].transfer.is_some()).await;
    unsafe { pc_client_decline_sync(a.c, ib.as_ptr(), a.transfer.unwrap()) };
    until(&mut [&mut a, &mut b], |d| d[1].kinds().contains(&PC_SYNC_FAILED)).await;

    // The relay admits two: a third is refused.
    let mut c = Driver::new(&invite, "c");
    until(&mut [&mut a, &mut b, &mut c], |d| d[2].kinds().contains(&PC_FULL)).await;
    drop(c);

    // b's identity joining again elsewhere replaces b.
    let mut b2 = Driver::with_seed(&invite, "b again", b.seed);
    until(&mut [&mut a, &mut b, &mut b2], |d| d[1].kinds().contains(&PC_REPLACED) && d[0].has(PC_JOINED, b"b again"))
        .await;
}
