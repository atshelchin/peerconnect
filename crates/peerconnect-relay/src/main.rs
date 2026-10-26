//! `peerconnect-relay`: the PeerConnect relay as a standalone server.
//!
//! Configured through the environment:
//!
//! - `PEERCONNECT_LISTEN` — address to listen on (default `0.0.0.0:8080`)
//! - `PEERCONNECT_MAX_CONNECTIONS` — concurrent WebSockets (default 10000)
//! - `PEERCONNECT_MAX_PER_ADDRESS` — concurrent WebSockets per client
//!   address, IPv6 counted per /64 (default 256)
//! - `PEERCONNECT_MAX_GROUP_MEMBERS` — members per group, 1 to 4 (default 4,
//!   the protocol's limit)
//! - `PEERCONNECT_TRUST_PROXY=1` — take the client address from the last
//!   `X-Forwarded-For` entry (only behind a proxy that sets it)
//! - `PEERCONNECT_QUIC_LISTEN` — UDP address for groups over QUIC (default:
//!   the same address as `PEERCONNECT_LISTEN`; `off` to turn QUIC off)
//! - `PEERCONNECT_TLS_CERT`, `PEERCONNECT_TLS_KEY` — PEM certificate chain
//!   and key, re-read when they change: `wss://` on the TCP port (next to
//!   plain `ws://`), and WebTransport for browsers. Without them the relay
//!   serves QUIC with the weekly certificate (GROUP.md §3.7), which browsers
//!   pin by hash for a `quic://` relay URL: no domain needed.
//! - `PEERCONNECT_MONTHLY_GB` — what the relay may send group members a
//!   calendar month (UTC), in gigabytes: from 80% of it everyone's video
//!   shrinks, and at 100% stops (voice and the rest go on)
//! - `PEERCONNECT_MAX_MBPS` — what it may send them a second, in megabits:
//!   beyond it everyone's video shrinks a layer at a time
//! - `PEERCONNECT_USAGE_FILE` — where the month's usage is kept across restarts
//!
//! `peerconnect-relay healthcheck` exits successfully if a relay answers on
//! `PEERCONNECT_LISTEN` (for container health checks).

use std::env;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use peerconnect_relay::{Budget, Config, Tls};
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
    let slots = peerconnect::group::wire::MAX_SLOTS;
    if !(1..=slots).contains(&config.max_group_members) {
        eprintln!(
            "PEERCONNECT_MAX_GROUP_MEMBERS must be 1 to {slots} (GROUP.md §3.2), got {}",
            config.max_group_members
        );
        return ExitCode::FAILURE;
    }
    config.trust_proxy = env::var("PEERCONNECT_TRUST_PROXY").is_ok_and(|v| v == "1" || v == "true");
    let set = |name| env::var_os(name).filter(|v| !v.is_empty());
    let number = |name: &str, unit: f64| -> Result<Option<u64>, ()> {
        match env::var(name) {
            Ok(text) if !text.is_empty() => match text.parse::<f64>() {
                Ok(n) if n > 0.0 => Ok(Some((n * unit) as u64)),
                _ => {
                    eprintln!("{name} must be a positive number, got {text:?}");
                    Err(())
                }
            },
            _ => Ok(None),
        }
    };
    let (Ok(monthly_bytes), Ok(max_bits_per_second)) =
        (number("PEERCONNECT_MONTHLY_GB", 1e9), number("PEERCONNECT_MAX_MBPS", 1e6))
    else {
        return ExitCode::FAILURE;
    };
    config.budget =
        Budget { monthly_bytes, max_bits_per_second, usage_file: set("PEERCONNECT_USAGE_FILE").map(Into::into) };
    config.tls = match (set("PEERCONNECT_TLS_CERT"), set("PEERCONNECT_TLS_KEY")) {
        (Some(cert), Some(key)) => Some(Tls { cert: cert.into(), key: key.into() }),
        (None, None) => None,
        _ => {
            eprintln!("set both PEERCONNECT_TLS_CERT and PEERCONNECT_TLS_KEY, or neither");
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind(&listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("cannot listen on {listen}: {err}");
            return ExitCode::FAILURE;
        }
    };
    // By default, the UDP port of the same number as the TCP one (which
    // may have been chosen by the system).
    let quic_listen = env::var("PEERCONNECT_QUIC_LISTEN")
        .unwrap_or_else(|_| listener.local_addr().map_or_else(|_| listen.clone(), |a| a.to_string()));
    let quic = match quic_listen.as_str() {
        "off" | "" => None,
        address => match std::net::UdpSocket::bind(address) {
            Ok(socket) => Some(socket),
            Err(err) => {
                eprintln!("cannot listen on UDP {address}: {err}");
                return ExitCode::FAILURE;
            }
        },
    };
    let local = |a: std::io::Result<SocketAddr>| a.map_or_else(|_| "?".into(), |a| a.to_string());
    eprintln!(
        "peerconnect relay {} listening on {} for ws://{} (max {} connections, {} per address, {} per group{})",
        env!("CARGO_PKG_VERSION"),
        local(listener.local_addr()),
        if config.tls.is_some() { " and wss://" } else { "" },
        config.max_connections,
        config.max_per_address,
        config.max_group_members,
        if config.trust_proxy { ", client address from X-Forwarded-For" } else { "" }
    );
    let budget = &config.budget;
    if budget.monthly_bytes.is_some() || budget.max_bits_per_second.is_some() {
        eprintln!(
            "group traffic budget: {} a month, {} at most",
            budget.monthly_bytes.map_or("unlimited".into(), |b| format!("{:.0} GB", b as f64 / 1e9)),
            budget.max_bits_per_second.map_or("unlimited".into(), |b| format!("{:.0} Mbit/s", b as f64 / 1e6)),
        );
    }
    match &quic {
        Some(socket) => eprintln!(
            "groups over QUIC on UDP {}: raw QUIC{}",
            local(socket.local_addr()),
            if config.tls.is_some() {
                " and WebTransport"
            } else {
                " and WebTransport, with the weekly certificate (quic:// relay URLs)"
            }
        ),
        None => eprintln!("QUIC is off"),
    }

    let mut term = signal(SignalKind::terminate()).expect("signal handler");
    let shutdown = async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        eprintln!("shutting down");
    };
    match peerconnect_relay::serve_all(listener, quic, config, shutdown).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
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
