//! Group conformance vectors (`test-vectors/g1.json`).
//!
//! Two members, S in slot 2 and R in slot 0, are driven through a whole
//! exchange: joining, admission, every kind of frame, a lost message and
//! its resend, a state transfer and a declined one. Every frame recorded is
//! one a member actually produced (the members' random keys, challenges and
//! salts are replaced by fixed ones first), and is checked to have the
//! effect it should at the other member. The test fails if the checked-in
//! file differs; `PEERCONNECT_WRITE_VECTORS=1` regenerates it.

use std::fmt::Write;

use zeroize::Zeroizing;

use super::*;
use crate::group::seal::{open, open_key, seal_key_salted};

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-vectors/g1.json");
const RELAY: &str = "https://relay.example.com";

fn bytes<const N: usize>(seed: u8) -> [u8; N] {
    std::array::from_fn(|i| seed.wrapping_add(i as u8))
}

fn hex(data: &[u8]) -> String {
    data.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    })
}

/// One recorded frame: its name, the sender's key (to open it), and the
/// fields a verifier needs to rebuild its plaintext from the spec.
struct Recorded {
    name: &'static str,
    frame: Vec<u8>,
    plaintext: Vec<u8>,
    fields: Vec<(&'static str, String)>,
}

struct Fixed {
    key: [u8; 32],
    challenge: [u8; 16],
    incarnation: u64,
}

/// Replaces a just-welcomed member's random values with fixed ones.
fn fix(m: &mut Member, f: &Fixed) {
    m.incarnation = f.incarnation;
    m.next_transfer = 0x0102_0304;
    let joined = m.joined_mut().unwrap();
    joined.key = Zeroizing::new(f.key);
    joined.challenge = f.challenge;
}

/// Drains a member's outputs: (frames sent, events).
fn drain(m: &mut Member) -> (Vec<Vec<u8>>, Vec<Event>) {
    let (mut frames, mut events) = (Vec::new(), Vec::new());
    while let Some(out) = m.poll_output() {
        match out {
            Output::Send(frame, _) => frames.push(frame),
            Output::Event(e) => events.push(e),
            _ => {}
        }
    }
    (frames, events)
}

fn only(frames: Vec<Vec<u8>>) -> Vec<u8> {
    assert_eq!(frames.len(), 1, "{frames:?}");
    frames.into_iter().next().unwrap()
}

fn sealed(name: &'static str, key: &[u8; 32], frame: Vec<u8>, fields: Vec<(&'static str, String)>) -> Recorded {
    let (_, plaintext) = open(key, &frame).expect("opens with its sender's key");
    Recorded { name, frame, plaintext, fields }
}

fn text(s: &str) -> String {
    format!("\"{s}\"")
}

fn generate() -> String {
    let secret: [u8; 32] = bytes(0x00);
    let group = Group::new(RELAY.into(), secret);
    let s_identity = Identity::from_seed(bytes(0x20));
    let r_identity = Identity::from_seed(bytes(0x40));
    let nonce: [u8; 32] = bytes(0xa0);
    let s_fixed = Fixed { key: bytes(0x60), challenge: bytes(0x70), incarnation: 0x0102_0304_0506_0708 };
    let r_fixed = Fixed { key: bytes(0x80), challenge: bytes(0x90), incarnation: 0x1112_1314_1516_1718 };
    let (s_salt, r_salt): ([u8; 16], [u8; 16]) = (bytes(0xb0), bytes(0xc0));
    let config = |name: &str| Config { profile: name.as_bytes().to_vec(), ..Config::default() };
    let mut frames: Vec<Recorded> = Vec::new();

    // Joining: both answer the same HELLO.
    let mut joins = Vec::new();
    let mut members = Vec::new();
    for (identity, name, slot, present, fixed) in
        [(&s_identity, "alice", 2u8, 1u32, &s_fixed), (&r_identity, "bob", 0, 1 << 2, &r_fixed)]
    {
        let mut m = Member::new(group.clone(), identity.clone(), config(name), 0).unwrap();
        m.handle_open(0);
        m.handle_binary(0, &crate::wire::hello(&nonce));
        let join = only(drain(&mut m).0);
        assert_eq!(group.id().verify_group_join(&nonce, &join), Ok(identity.token(&group.id())));
        joins.push(join);
        m.handle_binary(0, &wire::welcome(slot, present));
        assert_eq!(m.state(), State::Online);
        drain(&mut m); // its broadcast KEY, with a random salt: replaced below
        fix(&mut m, fixed);
        members.push(m);
    }
    let [mut s, mut r] = <[Member; 2]>::try_from(members).ok().unwrap();
    let (s_id, r_id) = (s.id(), r.id());

    // Admission. S's broadcast KEY, as S would send it with this salt.
    let announce = |m: &Member, f: &Fixed, echo: [u8; 16], profile: &str| Announce {
        identity: m.id(),
        key: Zeroizing::new(f.key),
        challenge: f.challenge,
        echo,
        incarnation: f.incarnation,
        next: [m.streams[0].next, m.streams[1].next],
        profile: profile.as_bytes().to_vec(),
    };
    let s_key =
        seal_key_salted(&group, &s_identity, 2, wire::BROADCAST, &announce(&s, &s_fixed, [0; 16], "alice"), s_salt);
    r.handle_binary(0, &s_key);
    // R answers with a KEY echoing S's challenge (its salt is random; ours is fixed).
    let reply = open_key(&group, &only(drain(&mut r).0)).unwrap();
    assert_eq!((reply.echo, reply.identity, reply.challenge), (s_fixed.challenge, r_id, r_fixed.challenge));
    let r_key = seal_key_salted(&group, &r_identity, 0, 2, &announce(&r, &r_fixed, s_fixed.challenge, "bob"), r_salt);
    s.handle_binary(0, &r_key);
    let (out, events) = drain(&mut s);
    assert_eq!(events, [Event::Joined { member: r_id, profile: b"bob".to_vec() }]);
    let confirm = sealed("confirm", &s_fixed.key, only(out), vec![("echo", text(&hex(&r_fixed.challenge)))]);
    r.handle_binary(0, &confirm.frame);
    assert_eq!(drain(&mut r).1, [Event::Joined { member: s_id, profile: b"alice".to_vec() }]);
    frames.push(confirm);

    // Stream messages, a direct message, voice and a datagram.
    let mut r_events = Vec::new();
    let mut deliver = |r: &mut Member, frame: &[u8]| {
        r.handle_binary(0, frame);
        let (out, events) = drain(r);
        r_events.extend(events);
        out
    };
    s.send(Channel::Command, b"place block 7").unwrap();
    let message0 = only(drain(&mut s).0);
    assert!(deliver(&mut r, &message0).is_empty());
    frames.push(sealed(
        "command_message",
        &s_fixed.key,
        message0,
        vec![("id", "0".into()), ("payload", text("place block 7"))],
    ));
    s.send(Channel::Command, b"place block 8").unwrap();
    let lost = only(drain(&mut s).0); // never reaches R
    frames.push(sealed(
        "command_message_lost",
        &s_fixed.key,
        lost,
        vec![("id", "1".into()), ("payload", text("place block 8"))],
    ));
    s.send(Channel::Chat, b"hello").unwrap();
    let chat = only(drain(&mut s).0);
    deliver(&mut r, &chat);
    frames.push(sealed("chat_message", &s_fixed.key, chat, vec![("id", "0".into()), ("payload", text("hello"))]));
    s.send_to(r_id, Channel::Command, b"psst").unwrap();
    let direct = only(drain(&mut s).0);
    deliver(&mut r, &direct);
    frames.push(sealed("command_direct", &s_fixed.key, direct, vec![("payload", text("psst"))]));
    let voice_data = [&[12u8][..], b"OPUS"].concat();
    assert!(s.send_voice(7, &voice_data));
    let voice = only(drain(&mut s).0);
    deliver(&mut r, &voice);
    frames.push(sealed("voice", &s_fixed.key, voice, vec![("seq", "7".into()), ("audio", text(&hex(&voice_data)))]));
    assert!(s.send_datagram(None, b"pos 1 2 3"));
    let datagram = only(drain(&mut s).0);
    deliver(&mut r, &datagram);
    frames.push(sealed("datagram", &s_fixed.key, datagram, vec![("payload", text("pos 1 2 3"))]));

    // A second after its last message, S announces its heads; R notices
    // the lost message, asks for it, and S resends it.
    s.handle_timeout(1_000);
    let heads = only(drain(&mut s).0);
    let resend = only(deliver(&mut r, &heads));
    frames.push(sealed("heads", &s_fixed.key, heads, vec![("command_next", "2".into()), ("chat_next", "1".into())]));
    s.handle_binary(0, &resend);
    frames.push(sealed(
        "resend",
        &r_fixed.key,
        resend,
        vec![
            ("channel", "0".into()),
            ("incarnation", text(&format!("{:016x}", s_fixed.incarnation))),
            ("from", "1".into()),
        ],
    ));
    let (out, _) = drain(&mut s);
    let [resent_message, resent] = <[Vec<u8>; 2]>::try_from(out).unwrap();
    deliver(&mut r, &resent_message);
    deliver(&mut r, &resent);
    frames.push(sealed(
        "resent_message",
        &s_fixed.key,
        resent_message,
        vec![("id", "1".into()), ("payload", text("place block 8"))],
    ));
    frames.push(sealed("resent", &s_fixed.key, resent, vec![("next", "2".into())]));

    // State transfer: R asks, S answers with a 5-byte snapshot.
    r.request_sync(s_id).unwrap();
    let request = only(drain(&mut r).0);
    s.handle_binary(0, &request);
    let Some(Event::SyncRequest { from, transfer }) = drain(&mut s).1.pop() else { panic!() };
    assert_eq!((from, transfer), (r_id, 0x0102_0304));
    frames.push(sealed("sync_request", &r_fixed.key, request, vec![("transfer", "16909060".into())]));
    s.provide_sync(r_id, transfer, b"world".to_vec()).unwrap();
    let [begin, bulk] = <[Vec<u8>; 2]>::try_from(drain(&mut s).0).unwrap();
    deliver(&mut r, &begin);
    let ack = only(deliver(&mut r, &bulk));
    let frontier = format!("[[\"{:016x}\", 2], [\"{:016x}\", 0]]", s_fixed.incarnation, r_fixed.incarnation);
    frames.push(sealed(
        "sync_begin",
        &s_fixed.key,
        begin,
        vec![("transfer", "16909060".into()), ("length", "5".into()), ("frontier", frontier)],
    ));
    frames.push(sealed("bulk", &s_fixed.key, bulk, vec![("transfer", "16909060".into()), ("data", text("world"))]));
    s.handle_binary(0, &ack);
    frames.push(sealed("sync_ack", &r_fixed.key, ack, vec![("transfer", "16909060".into()), ("received", "5".into())]));

    // And one S declines.
    r.request_sync(s_id).unwrap();
    let declined = only(drain(&mut r).0);
    s.handle_binary(0, &declined);
    frames.push(sealed("sync_request_declined", &r_fixed.key, declined, vec![("transfer", "16909061".into())]));
    let Some(Event::SyncRequest { transfer, .. }) = drain(&mut s).1.pop() else { panic!() };
    s.decline_sync(r_id, transfer);
    let abort = only(drain(&mut s).0);
    deliver(&mut r, &abort);
    frames.push(sealed("sync_abort", &s_fixed.key, abort, vec![("transfer", "16909061".into())]));

    // Video (§7.4): a keyframe of layer 2 in two fragments, the second lost;
    // R asks for it again, and S resends it to R alone. Then a frame of
    // layer 1 of another track that cannot be decoded without its keyframe:
    // R asks for one.
    let picture = [&[1u8][..], &(0..1499u32).map(|i| i as u8).collect::<Vec<u8>>()].concat();
    assert!(s.send_video(1, 2, true, 9000, &picture));
    let [video0, video1] = <[Vec<u8>; 2]>::try_from(drain(&mut s).0).unwrap();
    assert!(deliver(&mut r, &video0).is_empty());
    let video_fields = |index: usize, data: &[u8]| {
        vec![
            ("track", "1".into()),
            ("layer", "2".into()),
            ("frame", "0".into()),
            ("index", index.to_string()),
            ("count", "2".into()),
            ("keyframe", "true".into()),
            ("timestamp", "9000".into()),
            ("data", text(&hex(data))),
        ]
    };
    let (part0, part1) = picture.split_at(wire::MAX_VIDEO_FRAGMENT);
    frames.push(sealed("video_fragment", &s_fixed.key, video0, video_fields(0, part0)));
    frames.push(sealed("video_fragment_lost", &s_fixed.key, video1.clone(), video_fields(1, part1)));
    r.handle_timeout(5);
    let nack = only(drain(&mut r).0);
    s.handle_binary(0, &nack);
    frames.push(sealed(
        "video_nack",
        &r_fixed.key,
        nack,
        vec![("track", "1".into()), ("layer", "2".into()), ("frame", "0".into()), ("indices", "[1]".into())],
    ));
    let again = only(drain(&mut s).0);
    deliver(&mut r, &again);
    frames.push(sealed("video_resent", &s_fixed.key, again, video_fields(1, part1)));
    assert!(s.send_video(2, 1, false, 9033, b"\x01delta"));
    let delta = only(drain(&mut s).0);
    let request = only(deliver(&mut r, &delta));
    frames.push(sealed(
        "video_delta",
        &s_fixed.key,
        delta,
        vec![
            ("track", "2".into()),
            ("layer", "1".into()),
            ("frame", "0".into()),
            ("index", "0".into()),
            ("count", "1".into()),
            ("keyframe", "false".into()),
            ("timestamp", "9033".into()),
            ("data", text(&hex(b"\x01delta"))),
        ],
    ));
    s.handle_binary(0, &request);
    assert_eq!(drain(&mut s).1, [Event::KeyframeRequest { from: r_id, track: 2, layer: 1 }]);
    frames.push(sealed("keyframe_request", &r_fixed.key, request, vec![("track", "2".into()), ("layer", "1".into())]));

    // What R's application saw, in order: everything, exactly once.
    let message = |from, channel, data: &[u8], direct| Event::Message { from, channel, data: data.to_vec(), direct };
    assert_eq!(
        r_events,
        [
            message(s_id, Channel::Command, b"place block 7", false),
            message(s_id, Channel::Chat, b"hello", false),
            message(s_id, Channel::Command, b"psst", true),
            Event::Voice { from: s_id, seq: 7, data: voice_data.clone() },
            Event::Datagram { from: s_id, data: b"pos 1 2 3".to_vec() },
            message(s_id, Channel::Command, b"place block 8", false),
            Event::Snapshot { from: s_id, data: b"world".to_vec() },
            Event::SyncFailed { from: s_id },
            Event::Video { from: s_id, track: 1, layer: 2, keyframe: true, timestamp: 9000, data: picture },
        ]
    );

    let mut out = String::new();
    let mut field = |name: &str, value: String| writeln!(out, "  \"{name}\": {value},").unwrap();
    field(
        "description",
        text(
            "PeerConnect groups v1 test vectors (GROUP.md). Byte strings are hex; *_text values are strings. \
             Ed25519 keys are given by their 32-byte seeds. S sits in slot 2, R in slot 0. Every frame was produced \
             by a member; `fields` give what its plaintext encodes.",
        ),
    );
    field("relay_url", text(RELAY));
    field("group_secret", text(&hex(&secret)));
    field("group_seed", text(&hex(&crate::noise::hmac(&secret, &[b"peerconnect/g1 group"]))));
    field("group_id", text(&hex(group.id().as_bytes())));
    field("group_id_text", text(&group.id().to_string()));
    field("key_secret", text(&hex(group.key_secret())));
    field("invite", text(&group.to_string()));
    field("endpoint", text(&group.endpoint()));
    field("hello_nonce", text(&hex(&nonce)));
    for (who, identity, fixed, join, salt, key, profile) in [
        ("s", &s_identity, &s_fixed, &joins[0], s_salt, &s_key, "alice"),
        ("r", &r_identity, &r_fixed, &joins[1], r_salt, &r_key, "bob"),
    ] {
        field(&format!("{who}_identity_seed"), text(&hex(&identity.seed()[..])));
        field(&format!("{who}_member_id"), text(&hex(identity.id().as_bytes())));
        field(&format!("{who}_token"), text(&hex(&identity.token(&group.id()))));
        field(&format!("{who}_join"), text(&hex(join)));
        field(&format!("{who}_sender_key"), text(&hex(&fixed.key)));
        field(&format!("{who}_challenge"), text(&hex(&fixed.challenge)));
        field(&format!("{who}_incarnation"), text(&format!("{:016x}", fixed.incarnation)));
        field(&format!("{who}_profile_text"), text(profile));
        field(&format!("{who}_key_salt"), text(&hex(&salt)));
        field(&format!("{who}_key"), text(&hex(key)));
    }
    field("welcome_s", text(&hex(&wire::welcome(2, 1))));
    field("welcome_r", text(&hex(&wire::welcome(0, 1 << 2))));
    // R asks for layer 2 of S's video; the relay tells S that layers 1 and 2 are wanted.
    field("subscribe_r", text(&hex(&wire::subscribe(2, Some(2)))));
    field("subscribe_r_none", text(&hex(&wire::subscribe(2, None))));
    field("demand_s", text(&hex(&wire::demand(0b110))));
    field("member_arrived", text(&hex(&wire::member(2, true))));
    field("member_left", text(&hex(&wire::member(2, false))));
    out.push_str("  \"frames\": [\n");
    for (i, f) in frames.iter().enumerate() {
        let fields: Vec<String> = f.fields.iter().map(|(k, v)| format!("\"{k}\": {v}")).collect();
        write!(
            out,
            "    {{\"name\": \"{}\", \"plaintext\": \"{}\", \"frame\": \"{}\", \"fields\": {{{}}}}}",
            f.name,
            hex(&f.plaintext),
            hex(&f.frame),
            fields.join(", ")
        )
        .unwrap();
        out.push_str(if i + 1 < frames.len() { ",\n" } else { "\n" });
    }
    out.push_str("  ]\n}\n");
    format!("{{\n{out}")
}

#[test]
fn group_vectors_are_current() {
    let vectors = generate();
    assert_eq!(generate(), vectors, "deterministic");
    if std::env::var_os("PEERCONNECT_WRITE_VECTORS").is_some() {
        std::fs::write(PATH, &vectors).unwrap();
    }
    let on_disk = std::fs::read_to_string(PATH).expect("test-vectors/g1.json (set PEERCONNECT_WRITE_VECTORS=1)");
    assert!(on_disk == vectors, "test-vectors/g1.json is out of date");
}
