//! A terminal group chat over PeerConnect.
//!
//! ```text
//! group new  <relay-url> <name>   create a group, print its invite
//! group join <invite>    <name>   join a group
//! ```
//!
//! Lines typed are sent as chat; lines starting with `!` as commands; the
//! others' messages are printed as `<name> <channel>: <text>`. `/quit` leaves.
//! The relay is a `quic://` or `https://` URL: groups travel over QUIC (UDP).
//!
//! Run with `cargo run -p peerconnect --features client --example group -- …`.

use std::collections::HashMap;
use std::env;
use std::error::Error;

use peerconnect::client::group;
use peerconnect::group::{Channel, Config, Event, Group, Identity};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let (group, name) = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["new", relay, name] => {
            let group = Group::create(relay)?;
            println!("invite: {group}");
            (group, name.to_string())
        }
        ["join", invite, name] => (invite.parse()?, name.to_string()),
        _ => return Err("usage: group new <relay> <name> | join <invite> <name>".into()),
    };
    let config = Config { profile: name.into_bytes(), ..Config::default() };
    let mut conn = group::join(group, Identity::generate(), config)?;
    let mut names = HashMap::new();
    names.insert(conn.id(), "me".to_string());
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            event = conn.next() => match event {
                Some(Event::Joined { member, profile }) => {
                    let name = String::from_utf8_lossy(&profile).into_owned();
                    eprintln!("* {name} joined");
                    names.insert(member, name);
                }
                Some(Event::Left { member }) => eprintln!("* {} left", names.get(&member).map_or("?", |n| n)),
                Some(Event::Message { from, channel, data, direct }) if from != conn.id() => {
                    let name = names.get(&from).map_or("?", |n| n);
                    let channel = match (channel, direct) {
                        (_, true) => "direct",
                        (Channel::Command, _) => "command",
                        (Channel::Chat, _) => "chat",
                    };
                    println!("{name} {channel}: {}", String::from_utf8_lossy(&data));
                }
                Some(Event::State(state)) => eprintln!("* {state:?}"),
                Some(_) => {}
                None => return Ok(()),
            },
            line = lines.next_line() => match line? {
                Some(line) if line == "/quit" => break,
                Some(line) => {
                    let (channel, text) = match line.strip_prefix('!') {
                        Some(command) => (Channel::Command, command.to_string()),
                        None => (Channel::Chat, line),
                    };
                    conn.send(channel, text).await?;
                }
                None => break,
            },
        }
    }
    conn.leave().await;
    Ok(())
}
