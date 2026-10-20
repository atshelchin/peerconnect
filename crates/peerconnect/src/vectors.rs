//! Conformance test vectors (`test-vectors/v1.json`).
//!
//! Every value is computed from fixed inputs by the same code paths the
//! protocol uses. The test fails if the checked-in file differs; run it with
//! `PEERCONNECT_WRITE_VECTORS=1` to regenerate the file instead.

use std::fmt::Write;

use x25519_dalek::{PublicKey, StaticSecret};

use crate::noise::{self, Initiator};
use crate::wire::{self, Role};
use crate::{Invite, Room, base64};

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-vectors/v1.json");
const RELAY: &str = "wss://relay.example.com";

fn bytes(seed: u8) -> [u8; 32] {
    std::array::from_fn(|i| seed.wrapping_add(i as u8))
}

fn hex(data: &[u8]) -> String {
    data.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    })
}

fn record(role: Role, secret: [u8; 32], identity: [u8; 32], peer: Option<[u8; 32]>) -> Room {
    let mut raw = vec![1, role as u8];
    raw.extend_from_slice(&secret);
    raw.extend_from_slice(&identity);
    match peer {
        Some(peer) => {
            raw.push(1);
            raw.extend_from_slice(&peer);
        }
        None => raw.push(0),
    }
    raw.extend_from_slice(RELAY.as_bytes());
    Room::from_bytes(&raw).unwrap()
}

/// Encrypts one message the way `Peer::send` does: a FINAL chunk, preceded
/// by MORE chunks if it is long.
fn seal_message(transport: &mut noise::Transport, data: &[u8]) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> =
        if data.is_empty() { vec![&[]] } else { data.chunks(noise::MAX_PLAINTEXT_LEN - 1).collect() };
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| seal(transport, if i == last { 0x00 } else { 0x01 }, chunk))
        .collect()
}

fn seal(transport: &mut noise::Transport, kind: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![wire::MESSAGE];
    transport.encrypt(&[&[kind], data].concat(), &mut out).unwrap();
    out
}

fn generate() -> String {
    let secret = bytes(0x00);
    let inviter_identity = bytes(0x20);
    let joiner_identity = bytes(0x40);
    let joiner_ephemeral = bytes(0x60);
    let inviter_ephemeral = bytes(0x80);
    let nonce = bytes(0xa0);

    let inviter_public = PublicKey::from(&StaticSecret::from(inviter_identity)).to_bytes();
    let joiner_public = PublicKey::from(&StaticSecret::from(joiner_identity)).to_bytes();
    let inviter = record(Role::Inviter, secret, inviter_identity, None);
    let invite = inviter.invite().unwrap().to_string();
    let joiner = Room::join(&invite.parse::<Invite>().unwrap());
    let joiner = record(Role::Joiner, secret, joiner_identity, joiner.peer_key());

    let (initiator, init) = Initiator::start(
        joiner.identity(),
        &PublicKey::from(inviter_public),
        joiner.psk(),
        StaticSecret::from(joiner_ephemeral),
    )
    .unwrap();
    let response =
        noise::respond(inviter.identity(), inviter.psk(), &init, StaticSecret::from(inviter_ephemeral)).unwrap();
    assert_eq!(response.peer.to_bytes(), joiner_public);
    let mut to_inviter = initiator.finish(&response.reply).unwrap();
    let mut to_joiner = response.transport;

    let long: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let mut messages = String::new();
    let mut message = |direction: &str, plaintext: &str, frames: Vec<Vec<u8>>, hashed: bool| {
        use sha2::{Digest, Sha256};
        let frames: Vec<String> =
            frames.iter().map(|f| format!("\"{}\"", if hashed { hex(&Sha256::digest(f)) } else { hex(f) })).collect();
        let key = if hashed { "frames_sha256" } else { "frames" };
        writeln!(
            messages,
            "    {{ \"direction\": \"{direction}\", \"message\": {plaintext}, \"{key}\": [{}] }},",
            frames.join(", ")
        )
        .unwrap();
    };
    message("joiner->inviter", "\"utf8:hello\"", seal_message(&mut to_inviter, b"hello"), false);
    message("inviter->joiner", "\"utf8:\"", seal_message(&mut to_joiner, b""), false);
    message("joiner->inviter", "\"100000 bytes: byte i = i mod 251\"", seal_message(&mut to_inviter, &long), true);
    message("inviter->joiner", "\"utf8:world\"", seal_message(&mut to_joiner, b"world"), false);
    message("inviter->joiner", "\"CLOSE\"", vec![seal(&mut to_joiner, 0x02, &[])], false);
    let messages = messages.trim_end().trim_end_matches(',');

    format!(
        r#"{{
  "description": "PeerConnect v1 test vectors. Byte strings are hex unless named *_text. Key pairs are given by their private keys; X25519 private keys are used as-is (clamping happens inside X25519).",
  "relay_url": "{RELAY}",
  "room_secret": "{secret}",
  "room_seed": "{seed}",
  "room_id": "{room_id}",
  "room_id_text": "{room_id_text}",
  "psk": "{psk}",
  "endpoint": "{endpoint}",
  "inviter_identity_private": "{inviter_identity}",
  "inviter_identity_public": "{inviter_public}",
  "invite": "{invite}",
  "hello_nonce": "{nonce}",
  "hello_frame": "{hello}",
  "join_frame_inviter": "{join_inviter}",
  "join_frame_joiner": "{join_joiner}",
  "joiner_identity_private": "{joiner_identity}",
  "joiner_identity_public": "{joiner_public}",
  "joiner_ephemeral_private": "{joiner_ephemeral}",
  "inviter_ephemeral_private": "{inviter_ephemeral}",
  "init_frame": "{init_frame}",
  "reply_frame": "{reply_frame}",
  "messages": [
{messages}
  ]
}}
"#,
        secret = hex(&secret),
        seed = hex(&noise::hmac(&secret, &[b"peerconnect/v1 room"])),
        room_id = hex(inviter.id().as_bytes()),
        room_id_text = inviter.id(),
        psk = hex(inviter.psk()),
        endpoint = inviter.endpoint(),
        inviter_identity = hex(&inviter_identity),
        inviter_public = hex(&inviter_public),
        nonce = hex(&nonce),
        hello = hex(&wire::hello(&nonce)),
        join_inviter = hex(&inviter.join_frame(&nonce)),
        join_joiner = hex(&joiner.join_frame(&nonce)),
        joiner_identity = hex(&joiner_identity),
        joiner_public = hex(&joiner_public),
        joiner_ephemeral = hex(&joiner_ephemeral),
        inviter_ephemeral = hex(&inviter_ephemeral),
        init_frame = hex(&[&[wire::INIT], &init[..]].concat()),
        reply_frame = hex(&[&[wire::REPLY], &response.reply[..]].concat()),
    )
}

#[test]
fn test_vectors_are_current() {
    let vectors = generate();
    if std::env::var_os("PEERCONNECT_WRITE_VECTORS").is_some() {
        std::fs::write(PATH, &vectors).unwrap();
    }
    let on_disk = std::fs::read_to_string(PATH).expect("test-vectors/v1.json (set PEERCONNECT_WRITE_VECTORS=1)");
    assert!(on_disk == vectors, "test-vectors/v1.json is out of date");
}

#[test]
fn invite_in_vectors_round_trips() {
    let vectors = generate();
    let invite = vectors.lines().find(|l| l.contains("\"invite\"")).unwrap();
    let invite = invite.split('"').nth(3).unwrap();
    let fragment = invite.split_once('#').unwrap().1;
    assert_eq!(base64::decode(fragment).unwrap().len(), 64);
    assert!(invite.parse::<Invite>().is_ok());
}
