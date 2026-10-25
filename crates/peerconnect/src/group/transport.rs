//! How members reach a group's relay (GROUP.md §3.6): WebSocket, or QUIC,
//! where each class has a stream of its own and voice travels as datagrams,
//! so that a lost packet holds up only its own class.
//!
//! The relay URL's scheme says what a relay offers:
//!
//! | Relay URL | Tried in turn |
//! |---|---|
//! | `wss://host/p`, `ws://host/p` | that WebSocket |
//! | `https://host[:port]/p` | QUIC on UDP `host:port` (443), WebTransport for browsers; then `wss://host[:port]/p` |
//! | `quic://host[:port]/p` | raw QUIC on UDP `host:port` (443), certificate not verified; then `ws://host[:port]/p` |
//!
//! Everything here is free of I/O, for drivers to share.

use super::wire::{self, Class};
use crate::Error;

/// ALPN of raw QUIC. WebTransport negotiates `h3`.
pub const ALPN: &[u8] = b"peerconnect-g1";
/// QUIC close codes are the WebSocket ones (GROUP.md §3.5).
pub use crate::wire::close;

/// One way to reach the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// A WebSocket to this URL.
    WebSocket(String),
    Quic(QuicTarget),
}

/// A relay's QUIC endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuicTarget {
    /// A DNS name or an IP address (IPv6 without brackets).
    pub host: String,
    pub port: u16,
    /// The request path, `…/g1/<group id>`.
    pub path: String,
    /// Whether to verify the relay's certificate for `host`: true for
    /// `https://`, where browsers insist, false for `quic://`, whose relays
    /// need neither a domain nor a certificate anyone signed. Members do not
    /// rely on the relay for confidentiality or authenticity in either case.
    pub verify: bool,
}

impl QuicTarget {
    /// The `https://` URL of a WebTransport session to this endpoint.
    pub fn url(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        format!("https://{host}:{}{}", self.port, self.path)
    }
}

/// The ways to reach `endpoint` (a group endpoint, or any URL under a relay
/// URL), in the order to try them.
pub fn transports(endpoint: &str) -> Vec<Transport> {
    let Some((scheme, rest)) = endpoint.split_once("://") else { return Vec::new() };
    let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
    let fallback = match scheme {
        "ws" | "wss" => return vec![Transport::WebSocket(endpoint.to_owned())],
        "https" => "wss",
        "quic" => "ws",
        _ => return Vec::new(),
    };
    let websocket = Transport::WebSocket(format!("{fallback}://{authority}{path}"));
    match split_authority(authority) {
        Some((host, port)) => vec![
            Transport::Quic(QuicTarget {
                host: host.to_owned(),
                port: port.unwrap_or(443),
                path: if path.is_empty() { "/".to_owned() } else { path.to_owned() },
                verify: scheme == "https",
            }),
            websocket,
        ],
        None => vec![websocket],
    }
}

/// `host[:port]`, with IPv6 hosts in brackets.
fn split_authority(authority: &str) -> Option<(&str, Option<u16>)> {
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (host, rest) = v6.split_once(']')?;
            (host, rest.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let port = match port {
        Some(port) => Some(port.parse().ok().filter(|&p| p != 0)?),
        None => None,
    };
    (!host.is_empty()).then_some((host, port))
}

/// Checks and normalizes a group's relay URL: `ws://`, `wss://`, `https://`
/// or `quic://`, without a query or fragment.
pub(crate) fn relay_url(url: &str) -> Result<String, Error> {
    let (scheme, rest) = url.split_once("://").ok_or(Error::InvalidRelayUrl)?;
    if !matches!(scheme, "ws" | "wss" | "https" | "quic") {
        return Err(Error::InvalidRelayUrl);
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let valid =
        split_authority(authority).is_some() && rest.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'?' | b'#'));
    if !valid {
        return Err(Error::InvalidRelayUrl);
    }
    Ok(url.trim_end_matches('/').to_owned())
}

// Framing on QUIC streams.

/// The first byte of every stream: the class of the frames it carries.
pub fn stream_header(class: Class) -> u8 {
    class as u8
}

/// The class a stream carries, from its first byte.
pub fn stream_class(header: u8) -> Option<Class> {
    Class::ALL.get(header as usize).copied()
}

/// Appends one message, `length[2] ‖ message`. The empty message stands for
/// the WebSocket text messages: `ping` from a member, `pong` from the relay.
pub fn encode(message: &[u8], out: &mut Vec<u8>) {
    debug_assert!(message.len() <= wire::MAX_FRAME_LEN);
    out.extend_from_slice(&(message.len() as u16).to_be_bytes());
    out.extend_from_slice(message);
}

/// A message that is longer than any frame may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooBig;

/// Splits what arrives on a stream into messages.
#[derive(Debug, Default)]
pub struct Deframer {
    buf: Vec<u8>,
    start: usize,
}

impl Deframer {
    pub fn push(&mut self, data: &[u8]) {
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start >= 64 << 10 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// The next whole message, if there is one.
    pub fn message(&mut self) -> Result<Option<&[u8]>, TooBig> {
        let rest = &self.buf[self.start..];
        let Some(&[a, b]) = rest.get(..2) else { return Ok(None) };
        let len = u16::from_be_bytes([a, b]) as usize;
        if len > wire::MAX_FRAME_LEN {
            return Err(TooBig);
        }
        if rest.len() < 2 + len {
            return Ok(None);
        }
        let at = self.start + 2;
        self.start = at + len;
        Ok(Some(&self.buf[at..at + len]))
    }

    /// Bytes received but not yet returned as messages.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quic(host: &str, port: u16, path: &str, verify: bool) -> Transport {
        Transport::Quic(QuicTarget { host: host.into(), port, path: path.into(), verify })
    }

    #[test]
    fn schemes_say_what_to_try() {
        assert_eq!(transports("wss://r.example/g1/x"), [Transport::WebSocket("wss://r.example/g1/x".into())]);
        assert_eq!(transports("ws://10.0.0.1:8080/g1/x"), [Transport::WebSocket("ws://10.0.0.1:8080/g1/x".into())]);
        assert_eq!(
            transports("https://r.example/sub/g1/x"),
            [quic("r.example", 443, "/sub/g1/x", true), Transport::WebSocket("wss://r.example/sub/g1/x".into())]
        );
        assert_eq!(
            transports("quic://203.0.113.9:4433/g1/x"),
            [quic("203.0.113.9", 4433, "/g1/x", false), Transport::WebSocket("ws://203.0.113.9:4433/g1/x".into())]
        );
        assert_eq!(
            transports("quic://[2001:db8::1]:9/g1/x"),
            [quic("2001:db8::1", 9, "/g1/x", false), Transport::WebSocket("ws://[2001:db8::1]:9/g1/x".into())]
        );
        assert_eq!(
            transports("quic://r.example"),
            [quic("r.example", 443, "/", false), Transport::WebSocket("ws://r.example".into())]
        );
        assert_eq!(transports("http://r.example/g1/x"), []);
        assert_eq!(transports("nonsense"), []);
        match &transports("quic://[::1]:5/g1/x")[0] {
            Transport::Quic(t) => assert_eq!(t.url(), "https://[::1]:5/g1/x"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn relay_urls() {
        for ok in
            ["wss://r.example", "ws://r:8080/sub", "https://r.example:8443", "quic://1.2.3.4:4433", "quic://[::1]"]
        {
            assert_eq!(relay_url(&format!("{ok}/")).unwrap(), ok);
        }
        for bad in ["http://r", "quic://", "quic://r:0", "quic://r:99999", "https://r?x", "quic://[::1", "r.example"] {
            assert!(relay_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn framing_round_trips_in_any_pieces() {
        let messages: Vec<Vec<u8>> = vec![vec![], vec![0x03, 1, 0xFF], vec![7; wire::MAX_FRAME_LEN], vec![1]];
        let mut wire_bytes = Vec::new();
        for m in &messages {
            encode(m, &mut wire_bytes);
        }
        for piece in [1, 2, 3, 1000, wire_bytes.len()] {
            let mut d = Deframer::default();
            let mut got = Vec::new();
            for chunk in wire_bytes.chunks(piece) {
                d.push(chunk);
                while let Some(m) = d.message().unwrap() {
                    got.push(m.to_vec());
                }
            }
            assert_eq!(got, messages, "in pieces of {piece}");
            assert_eq!(d.pending(), 0);
        }
        let mut d = Deframer::default();
        d.push(&((wire::MAX_FRAME_LEN + 1) as u16).to_be_bytes());
        assert_eq!(d.message(), Err(TooBig));
        assert_eq!(stream_class(stream_header(Class::Chat)), Some(Class::Chat));
        assert_eq!(stream_class(4), Some(Class::Video));
        assert_eq!(stream_class(5), None);
    }
}
