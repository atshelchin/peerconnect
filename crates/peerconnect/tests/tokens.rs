//! Relay tokens from the peer's side (SPEC §4.1, §4.7): the token rides on
//! every endpoint, 4403 stops the peer until it gets another, and 4429 makes
//! it wait.

use peerconnect::wire::close;
use peerconnect::{Event, Output, Peer, Room, State};

fn outputs(peer: &mut Peer) -> Vec<Output> {
    std::iter::from_fn(|| peer.poll_output()).collect()
}

fn connect_url(outputs: &[Output]) -> Option<&str> {
    outputs.iter().find_map(|o| match o {
        Output::Connect(url) => Some(url.as_str()),
        _ => None,
    })
}

#[test]
fn the_token_rides_on_every_endpoint() {
    let room = Room::create("wss://relay.example/base").unwrap();
    let id = room.id().to_string();
    let mut peer = Peer::with_token(room, Some("a.b.c".into()), 0);
    let first = outputs(&mut peer);
    assert_eq!(connect_url(&first), Some(format!("wss://relay.example/base/v1/{id}?token=a.b.c").as_str()));

    // An ordinary failure: the same token again, after the backoff.
    peer.handle_close(10, Some(1006));
    let retry = peer.poll_timeout().unwrap();
    peer.handle_timeout(retry);
    assert!(connect_url(&outputs(&mut peer)).unwrap().ends_with("?token=a.b.c"));
}

#[test]
fn without_a_token_the_endpoint_is_unchanged() {
    let room = Room::create("wss://relay.example").unwrap();
    let id = room.id().to_string();
    let mut peer = Peer::with_token(room, Some(String::new()), 0);
    assert_eq!(connect_url(&outputs(&mut peer)), Some(format!("wss://relay.example/v1/{id}").as_str()));
}

#[test]
fn a_refused_token_waits_for_another() {
    let mut peer = Peer::with_token(Room::create("wss://relay.example").unwrap(), Some("old".into()), 0);
    outputs(&mut peer);
    peer.handle_open(5);
    peer.handle_close(10, Some(close::TOKEN));
    let after = outputs(&mut peer);
    assert!(after.contains(&Output::Event(Event::Unauthorized)), "{after:?}");
    assert_eq!(peer.state(), State::Offline);
    assert_eq!(peer.poll_timeout(), None, "it never retries with the same token");
    peer.handle_timeout(10_000_000);
    assert_eq!(connect_url(&outputs(&mut peer)), None);

    peer.set_token(20, Some("new".into()));
    assert!(connect_url(&outputs(&mut peer)).unwrap().ends_with("?token=new"), "it connects at once");
}

#[test]
fn a_new_token_is_used_from_the_next_connection_on() {
    let mut peer = Peer::with_token(Room::create("wss://relay.example").unwrap(), Some("one".into()), 0);
    outputs(&mut peer);
    peer.set_token(1, Some("two".into()));
    assert_eq!(connect_url(&outputs(&mut peer)), None, "the current attempt carries on");
    peer.handle_close(2, Some(1006));
    let retry = peer.poll_timeout().unwrap();
    peer.handle_timeout(retry);
    assert!(connect_url(&outputs(&mut peer)).unwrap().ends_with("?token=two"));
}

#[test]
fn a_rate_limited_peer_waits_a_minute() {
    let mut peer = Peer::with_token(Room::create("wss://relay.example").unwrap(), Some("t".into()), 0);
    outputs(&mut peer);
    peer.handle_close(1_000, Some(close::RATE_LIMITED));
    let retry = peer.poll_timeout().unwrap();
    assert!(retry >= 61_000, "retries at {retry}");
}

#[test]
fn leaving_while_unauthorized_closes() {
    let mut peer = Peer::with_token(Room::create("wss://relay.example").unwrap(), None, 0);
    outputs(&mut peer);
    peer.handle_close(1, Some(close::TOKEN));
    peer.leave();
    assert_eq!(peer.state(), State::Closed(peerconnect::CloseReason::Left));
    assert_eq!(peer.poll_timeout(), None);
}

#[test]
fn a_peer_can_wait_for_its_first_token() {
    let mut peer = Peer::awaiting_token(Room::create("wss://relay.example").unwrap(), 0);
    assert_eq!(connect_url(&outputs(&mut peer)), None);
    assert_eq!(peer.poll_timeout(), None);
    assert!(peer.send(b"early").is_ok(), "messages queue meanwhile");
    peer.set_token(5, Some("first".into()));
    assert!(connect_url(&outputs(&mut peer)).unwrap().ends_with("?token=first"));
}

#[test]
fn a_refused_joiner_tries_the_same_id_again() {
    let inviter = Room::create("wss://relay.example").unwrap();
    let joiner = Room::join(&inviter.invite().unwrap().to_string().parse().unwrap());
    let mut peer = Peer::with_token(joiner, None, 0);
    let first = connect_url(&outputs(&mut peer)).unwrap().to_owned();
    peer.handle_close(1, Some(close::TOKEN));
    peer.set_token(2, Some("t".into()));
    assert_eq!(connect_url(&outputs(&mut peer)).unwrap(), format!("{first}?token=t"));
    peer.handle_close(3, Some(close::RATE_LIMITED));
    let retry = peer.poll_timeout().unwrap();
    peer.handle_timeout(retry);
    assert_eq!(connect_url(&outputs(&mut peer)).unwrap(), format!("{first}?token=t"));
}
