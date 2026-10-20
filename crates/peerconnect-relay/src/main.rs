//! `peerconnect-relay`: the PeerConnect relay as a standalone server.
//!
//! Configured through the environment:
//!
//! - `PEERCONNECT_LISTEN` — address to listen on (default `0.0.0.0:8080`)
//! - `PEERCONNECT_MAX_CONNECTIONS` — concurrent WebSockets (default 10000)
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
    if let Ok(max) = env::var("PEERCONNECT_MAX_CONNECTIONS") {
        match max.parse() {
            Ok(max) => config.max_connections = max,
            Err(_) => {
                eprintln!("PEERCONNECT_MAX_CONNECTIONS must be a number, got {max:?}");
                return ExitCode::FAILURE;
            }
        }
    }
    let listener = match TcpListener::bind(&listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("cannot listen on {listen}: {err}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "peerconnect relay {} listening on {} (max {} connections)",
        env!("CARGO_PKG_VERSION"),
        listener.local_addr().map_or(listen, |a| a.to_string()),
        config.max_connections
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
