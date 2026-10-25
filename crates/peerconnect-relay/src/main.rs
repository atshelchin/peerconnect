//! `peerconnect-relay`: the PeerConnect relay as a standalone server.
//!
//! Configured through the environment:
//!
//! - `PEERCONNECT_LISTEN` — address to listen on (default `0.0.0.0:8080`)
//! - `PEERCONNECT_MAX_CONNECTIONS` — concurrent WebSockets (default 10000)
//! - `PEERCONNECT_MAX_PER_ADDRESS` — concurrent WebSockets per client
//!   address, IPv6 counted per /64 (default 256)
//! - `PEERCONNECT_MAX_GROUP_MEMBERS` — members per group, up to 32 (default 10)
//! - `PEERCONNECT_TRUST_PROXY=1` — take the client address from the last
//!   `X-Forwarded-For` entry (only behind a proxy that sets it)
//!
//! It speaks plain `ws://`; put a TLS-terminating proxy in front of it for
//! `wss://`. `peerconnect-relay healthcheck` exits successfully if a relay
//! answers on `PEERCONNECT_LISTEN` (for container health checks).

use std::env;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use peerconnect_relay::Config;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};

#[tokio::main]
async fn main() -> ExitCode {
    let listen = env::var("PEERCONNECT_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    if env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck(&listen);
    }
    let mut config = Config::default();
    for (name, value) in [
        ("PEERCONNECT_MAX_CONNECTIONS", &mut config.max_connections),
        ("PEERCONNECT_MAX_PER_ADDRESS", &mut config.max_per_address),
        ("PEERCONNECT_MAX_GROUP_MEMBERS", &mut config.max_group_members),
    ] {
        if let Ok(text) = env::var(name) {
            match text.parse() {
                Ok(number) => *value = number,
                Err(_) => {
                    eprintln!("{name} must be a number, got {text:?}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    config.trust_proxy = env::var("PEERCONNECT_TRUST_PROXY").is_ok_and(|v| v == "1" || v == "true");
    let listener = match TcpListener::bind(&listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("cannot listen on {listen}: {err}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "peerconnect relay {} listening on {} (max {} connections, {} per address, {} per group{})",
        env!("CARGO_PKG_VERSION"),
        listener.local_addr().map_or(listen, |a| a.to_string()),
        config.max_connections,
        config.max_per_address,
        config.max_group_members,
        if config.trust_proxy { ", client address from X-Forwarded-For" } else { "" }
    );

    let mut term = signal(SignalKind::terminate()).expect("signal handler");
    let shutdown = async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        eprintln!("shutting down");
    };
    peerconnect_relay::serve(listener, config, shutdown).await;
    ExitCode::SUCCESS
}

/// GETs `/healthz` from the local relay.
fn healthcheck(listen: &str) -> ExitCode {
    let check = || -> std::io::Result<bool> {
        let mut addr: SocketAddr = listen.parse().map_err(std::io::Error::other)?;
        if addr.ip().is_unspecified() {
            addr.set_ip([127, 0, 0, 1].into());
        }
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response.starts_with("HTTP/1.1 200"))
    };
    match check() {
        Ok(true) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
