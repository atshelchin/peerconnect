//! A terminal chat over PeerConnect.
//!
//! ```text
//! chat new  <relay-url> <room-file>   create a room, print the invite
//! chat join <invite>    <room-file>   accept an invite
//! chat open <room-file>               reconnect to a saved room
//! ```
//!
//! Lines typed are sent to the peer, queued while it is away; the peer's
//! messages are printed. The room is saved to `<room-file>` (a secret!) so
//! it can be reopened later. `/close` ends the room for both sides. For a
//! relay that requires a token, set `PEERCONNECT_TOKEN`.
//!
//! Run with `cargo run -p peerconnect --features client --example chat -- …`.

use std::error::Error;
use std::{env, fs};

use peerconnect::{Event, Room, State, client};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let (room, file) = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["new", relay, file] => {
            let room = Room::create(relay)?;
            println!("invite: {}", room.invite().unwrap());
            (room, file)
        }
        ["join", invite, file] => (Room::join(&invite.parse()?), file),
        ["open", file] => (fs::read_to_string(file)?.parse()?, file),
        _ => return Err("usage: chat new <relay> <file> | join <invite> <file> | open <file>".into()),
    };
    fs::write(file, room.to_string())?;

    let token = env::var("PEERCONNECT_TOKEN").ok();
    let mut conn = client::connect_with_token(room, token);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            event = conn.next() => match event {
                Some(Event::Message(data)) => println!("< {}", String::from_utf8_lossy(&data)),
                Some(Event::Paired) => {
                    eprintln!("* paired");
                    fs::write(file, conn.room().to_string())?;
                }
                Some(Event::Replaced) => eprintln!("* this room was opened elsewhere; retrying in a minute"),
                Some(Event::Unauthorized) => {
                    eprintln!("* the relay requires a token: set PEERCONNECT_TOKEN");
                    return Ok(());
                }
                Some(Event::Delivered(_)) => {}
                Some(Event::State(State::Closed(reason))) => {
                    eprintln!("* closed: {reason:?}");
                    if reason != peerconnect::CloseReason::Left {
                        let _ = fs::remove_file(file);
                    }
                    return Ok(());
                }
                Some(Event::State(state)) => eprintln!("* {state:?}"),
                None => return Ok(()),
            },
            line = lines.next_line() => match line? {
                Some(line) if line == "/close" => {
                    conn.close(); // finishes when the peer has been told
                    fs::write(file, conn.room().to_string())?;
                }
                Some(line) => {
                    if let Err(err) = conn.send(line).await {
                        eprintln!("* not sent: {err}");
                    }
                }
                None => {
                    conn.leave().await;
                    return Ok(());
                }
            },
        }
    }
}
