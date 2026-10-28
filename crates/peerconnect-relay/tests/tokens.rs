//! A relay that requires tokens (SPEC §4.1): `Config::tokens`, against the
//! Tokio client and raw WebSockets.

use std::time::Duration;

use futures_util::StreamExt;
use peerconnect::client::{self, Connection};
use peerconnect::token::{Claims, Keys};
use peerconnect::wire::{self, close};
use peerconnect::{Event, Room, State};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

const SECRET: &str = "a-test-secret-of-at-least-32-bytes!";

fn keys() -> Keys {
    Keys::parse(SECRET).unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn token(client: &str, app: &str) -> String {
    keys().sign(&Claims { app: Some(app.into()), exp: Some(now() + 3600), ..Claims::new(client) })
}

async fn relay() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let config = peerconnect_relay::Config { tokens: Some(keys()), ..Default::default() };
    tokio::spawn(peerconnect_relay::serve(listener, config, std::future::pending()));
    url
}

/// What the relay sends first: HELLO (`Ok(frame)`) or a close code.
async fn first(url: String) -> Result<Vec<u8>, Option<u16>> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    match timeout(Duration::from_secs(10), ws.next()).await.unwrap() {
        Some(Ok(Message::Binary(frame))) => Ok(frame.to_vec()),
        Some(Ok(Message::Close(frame))) => Err(frame.map(|f| f.code.into())),
        other => panic!("{other:?}"),
    }
}

async fn wait(conn: &mut Connection, what: impl Fn(&Event) -> bool) -> Event {
    timeout(Duration::from_secs(20), async {
        loop {
            let event = conn.next().await.expect("connection ended");
            if what(&event) {
                return event;
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

#[tokio::test]
async fn connections_without_a_valid_token_are_closed_before_hello() {
    let url = relay().await;
    let room = Room::create(&url).unwrap();
    let endpoint = format!("{url}/v1/{}", room.id());
    let expired = keys().sign(&Claims { exp: Some(now() - 3600), ..Claims::new("c-old") });
    let foreign = Keys::parse(&"x".repeat(32)).unwrap().sign(&Claims::new("c-x"));
    for query in ["", "?token=", "?token=nonsense", &format!("?token={expired}"), &format!("?token={foreign}")] {
        assert_eq!(first(format!("{endpoint}{query}")).await, Err(Some(close::TOKEN)), "{query}");
    }
    let hello = first(format!("{endpoint}?x=1&token={}", token("c-a", "app_a"))).await.unwrap();
    assert_eq!(hello[0], wire::HELLO);
}

#[tokio::test]
async fn peers_of_different_apps_share_a_room() {
    let url = relay().await;
    let inviter = Room::create(&url).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap().to_string().parse().unwrap());
    let mut a = client::connect_with_token(inviter, Some(token("c-alice", "app_chat")));
    let mut b = client::connect_with_token(joiner, Some(token("c-bob", "app_game")));
    wait(&mut a, |e| *e == Event::State(State::Connected)).await;
    wait(&mut b, |e| *e == Event::State(State::Connected)).await;
    b.send(b"hello".to_vec()).await.unwrap();
    // Pairing moves both to the pair ID, in whichever order: the token goes along.
    let (mut paired, mut hello) = (false, false);
    while !(paired && hello) {
        match wait(&mut a, |e| matches!(e, Event::Message(_) | Event::Paired)).await {
            Event::Paired => paired = true,
            message => {
                assert_eq!(message, Event::Message(b"hello".to_vec()));
                hello = true;
            }
        }
    }
    a.send(b"after pairing".to_vec()).await.unwrap();
    let got = wait(&mut b, |e| matches!(e, Event::Message(_))).await;
    assert_eq!(got, Event::Message(b"after pairing".to_vec()));
}

#[tokio::test]
async fn a_refused_peer_waits_for_a_token_then_connects() {
    let url = relay().await;
    let inviter = Room::create(&url).unwrap();
    let joiner = Room::join(&inviter.invite().unwrap().to_string().parse().unwrap());
    let mut a = client::connect_with_token(inviter, Some(token("c-alice", "app")));
    let mut b = client::connect(joiner);
    wait(&mut b, |e| *e == Event::Unauthorized).await;
    b.send(b"queued while refused".to_vec()).await.unwrap();
    b.set_token(Some(token("c-bob", "app")));
    let got = wait(&mut a, |e| matches!(e, Event::Message(_))).await;
    assert_eq!(got, Event::Message(b"queued while refused".to_vec()));
}
