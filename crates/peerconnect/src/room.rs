//! Room credentials: the room secret, what is derived from it, the invite,
//! and the record a peer keeps to reconnect (SPEC §3, §4).

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::noise::hmac;
use crate::wire::{self, Role, close};
use crate::{Error, base64};

/// Identifies a room on a relay. It is the room's Ed25519 public key, so it
/// locates the room without granting access to it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomId([u8; 32]);

impl RoomId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Relay side: checks a `JOIN` frame against the `HELLO` nonce and returns
    /// the seat it claims, or the close code to reject it with.
    pub fn verify_join(&self, nonce: &[u8; wire::NONCE_LEN], frame: &[u8]) -> Result<Role, u16> {
        if frame.len() != wire::JOIN_LEN || frame[0] != wire::JOIN {
            return Err(close::PROTOCOL_ERROR);
        }
        let role = Role::from_byte(frame[1]).ok_or(close::PROTOCOL_ERROR)?;
        let signature = Signature::from_bytes(frame[2..].try_into().unwrap());
        let key = VerifyingKey::from_bytes(&self.0).map_err(|_| close::UNAUTHORIZED)?;
        key.verify_strict(&wire::join_message(nonce, role), &signature).map_err(|_| close::UNAUTHORIZED)?;
        Ok(role)
    }
}

impl FromStr for RoomId {
    type Err = Error;

    /// Parses the base64url form, accepting only valid Ed25519 public keys.
    fn from_str(s: &str) -> Result<Self, Error> {
        let bytes = base64::decode_array(s).ok_or(Error::InvalidRoomId)?;
        VerifyingKey::from_bytes(&bytes).map_err(|_| Error::InvalidRoomId)?;
        Ok(RoomId(bytes))
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&base64::encode(&self.0))
    }
}

impl fmt::Debug for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RoomId({self})")
    }
}

/// Everything derived from a room secret.
#[derive(Clone)]
struct Keys {
    secret: Zeroizing<[u8; 32]>,
    signer: SigningKey,
    psk: Zeroizing<[u8; 32]>,
}

impl Keys {
    fn derive(secret: [u8; 32]) -> Keys {
        let seed = Zeroizing::new(hmac(&secret, &[b"peerconnect/v1 room"]));
        Keys {
            signer: SigningKey::from_bytes(&seed),
            psk: Zeroizing::new(hmac(&secret, &[b"peerconnect/v1 psk"])),
            secret: Zeroizing::new(secret),
        }
    }

    fn id(&self) -> RoomId {
        RoomId(self.signer.verifying_key().to_bytes())
    }
}

/// An invitation to a room: `<relay URL>#<base64url(secret ‖ inviter key)>`.
///
/// The invite is a bearer credential. Whoever holds it can pair with the
/// inviter (once) and can occupy the joiner's seat on the relay, so it must
/// travel over a channel that keeps it confidential and authentic.
#[derive(Clone)]
pub struct Invite {
    relay: String,
    secret: Zeroizing<[u8; 32]>,
    inviter: PublicKey,
}

impl Invite {
    pub fn relay(&self) -> &str {
        &self.relay
    }

    pub fn room_id(&self) -> RoomId {
        Keys::derive(*self.secret).id()
    }
}

impl FromStr for Invite {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let (relay, fragment) = s.trim().split_once('#').ok_or(Error::InvalidInvite)?;
        let bytes = Zeroizing::new(base64::decode_array::<64>(fragment).ok_or(Error::InvalidInvite)?);
        let inviter = PublicKey::from(<[u8; 32]>::try_from(&bytes[32..]).unwrap());
        if !usable(&inviter) {
            return Err(Error::InvalidInvite);
        }
        Ok(Invite { relay: relay_url(relay)?, secret: Zeroizing::new(bytes[..32].try_into().unwrap()), inviter })
    }
}

impl fmt::Display for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = Zeroizing::new([&self.secret[..], self.inviter.as_bytes()].concat());
        write!(f, "{}#{}", self.relay, base64::encode(&bytes))
    }
}

impl fmt::Debug for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invite({}, {})", self.relay, self.room_id())
    }
}

/// One peer's side of a room: everything it needs to reconnect, at any time,
/// to any relay that speaks the protocol. Persist it with [`Room::to_bytes`]
/// (or its string form) and treat it as a secret.
#[derive(Clone)]
pub struct Room {
    relay: String,
    role: Role,
    keys: Keys,
    identity: StaticSecret,
    peer: Option<PublicKey>,
}

const RECORD_VERSION: u8 = 1;

impl Room {
    /// Creates a new room on `relay` (a `ws://` or `wss://` URL). The caller
    /// becomes the inviter; hand [`Room::invite`] to the other peer.
    pub fn create(relay: &str) -> Result<Room, Error> {
        Ok(Room {
            relay: relay_url(relay)?,
            role: Role::Inviter,
            keys: Keys::derive(crate::random()),
            identity: StaticSecret::from(crate::random()),
            peer: None,
        })
    }

    /// Accepts an invite. The caller becomes the joiner, with a new identity
    /// that the inviter will pin: persist the room before connecting, since
    /// joining again from the same invite yields a different identity, which
    /// the inviter refuses once paired.
    pub fn join(invite: &Invite) -> Room {
        Room {
            relay: invite.relay.clone(),
            role: Role::Joiner,
            keys: Keys::derive(*invite.secret),
            identity: StaticSecret::from(crate::random()),
            peer: Some(invite.inviter),
        }
    }

    /// The invite for this room, while the inviter has not yet paired.
    pub fn invite(&self) -> Option<Invite> {
        (self.role == Role::Inviter && self.peer.is_none()).then(|| Invite {
            relay: self.relay.clone(),
            secret: self.keys.secret.clone(),
            inviter: PublicKey::from(&self.identity),
        })
    }

    pub fn id(&self) -> RoomId {
        self.keys.id()
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn relay(&self) -> &str {
        &self.relay
    }

    /// The WebSocket URL of this room on its relay.
    pub fn endpoint(&self) -> String {
        format!("{}/v1/{}", self.relay, self.id())
    }

    /// Whether the other peer's identity is known. Always true for a joiner;
    /// true for an inviter once the first handshake has completed.
    pub fn is_paired(&self) -> bool {
        self.peer.is_some()
    }

    /// This peer's static public key.
    pub fn public_key(&self) -> [u8; 32] {
        PublicKey::from(&self.identity).to_bytes()
    }

    /// The other peer's static public key, once known.
    pub fn peer_key(&self) -> Option<[u8; 32]> {
        self.peer.map(|k| k.to_bytes())
    }

    pub(crate) fn identity(&self) -> &StaticSecret {
        &self.identity
    }

    pub(crate) fn peer(&self) -> Option<&PublicKey> {
        self.peer.as_ref()
    }

    pub(crate) fn pin(&mut self, peer: PublicKey) {
        self.peer = Some(peer);
    }

    pub(crate) fn psk(&self) -> &[u8; 32] {
        &self.keys.psk
    }

    /// The `JOIN` frame answering a relay's `HELLO` nonce.
    pub(crate) fn join_frame(&self, nonce: &[u8; wire::NONCE_LEN]) -> [u8; wire::JOIN_LEN] {
        let signature = self.keys.signer.sign(&wire::join_message(nonce, self.role));
        let mut frame = [0u8; wire::JOIN_LEN];
        frame[0] = wire::JOIN;
        frame[1] = self.role as u8;
        frame[2..].copy_from_slice(&signature.to_bytes());
        frame
    }

    /// Serializes the record: `version role secret[32] identity[32] has_peer [peer[32]] relay`.
    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut out = Zeroizing::new(Vec::with_capacity(100 + self.relay.len()));
        out.extend_from_slice(&[RECORD_VERSION, self.role as u8]);
        out.extend_from_slice(&self.keys.secret[..]);
        out.extend_from_slice(self.identity.as_bytes());
        match &self.peer {
            Some(peer) => {
                out.push(1);
                out.extend_from_slice(peer.as_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(self.relay.as_bytes());
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Room, Error> {
        let bad = || Error::InvalidRecord;
        let take = |at: usize| -> Result<[u8; 32], Error> {
            bytes.get(at..at + 32).ok_or_else(bad).map(|b| b.try_into().unwrap())
        };
        if bytes.len() < 67 || bytes[0] != RECORD_VERSION {
            return Err(bad());
        }
        let role = Role::from_byte(bytes[1]).ok_or_else(bad)?;
        let (peer, rest) = match bytes[66] {
            0 => (None, 67),
            1 => {
                let peer = PublicKey::from(take(67)?);
                if !usable(&peer) {
                    return Err(bad());
                }
                (Some(peer), 99)
            }
            _ => return Err(bad()),
        };
        if role == Role::Joiner && peer.is_none() {
            return Err(bad());
        }
        let relay = std::str::from_utf8(bytes.get(rest..).ok_or_else(bad)?).map_err(|_| bad())?;
        Ok(Room {
            relay: relay_url(relay)?,
            role,
            keys: Keys::derive(take(2)?),
            identity: StaticSecret::from(take(34)?),
            peer,
        })
    }
}

/// The string form is base64url of [`Room::to_bytes`].
impl fmt::Display for Room {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&base64::encode(&self.to_bytes()))
    }
}

impl FromStr for Room {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let bytes = Zeroizing::new(base64::decode(s.trim()).ok_or(Error::InvalidRecord)?);
        Room::from_bytes(&bytes)
    }
}

impl fmt::Debug for Room {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Room")
            .field("id", &self.id())
            .field("role", &self.role)
            .field("relay", &self.relay)
            .field("paired", &self.is_paired())
            .finish_non_exhaustive()
    }
}

/// Whether an X25519 public key can take part in a handshake: low-order
/// points, which would make a Diffie-Hellman output all zeros, cannot.
fn usable(key: &PublicKey) -> bool {
    // Clamped scalars are multiples of the cofactor, so any low-order point yields zero.
    StaticSecret::from([1; 32]).diffie_hellman(key).was_contributory()
}

/// Validates a relay base URL and strips any trailing slash.
fn relay_url(url: &str) -> Result<String, Error> {
    let rest = url.strip_prefix("wss://").or_else(|| url.strip_prefix("ws://")).ok_or(Error::InvalidRelayUrl)?;
    let host = rest.split('/').next().unwrap_or_default();
    let valid = !host.is_empty() && rest.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'?' | b'#'));
    if !valid {
        return Err(Error::InvalidRelayUrl);
    }
    Ok(url.trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_round_trip_and_room_derivation() {
        let inviter = Room::create("wss://relay.example.com/").unwrap();
        assert_eq!(inviter.relay(), "wss://relay.example.com");
        let text = inviter.invite().unwrap().to_string();
        assert!(text.starts_with("wss://relay.example.com#"));
        assert_eq!(text.len(), "wss://relay.example.com#".len() + 86);

        let invite: Invite = text.parse().unwrap();
        let joiner = Room::join(&invite);
        assert_eq!(joiner.id(), inviter.id());
        assert_eq!(joiner.psk(), inviter.psk());
        assert_eq!(joiner.peer_key(), Some(inviter.public_key()));
        assert_eq!(joiner.endpoint(), format!("wss://relay.example.com/v1/{}", inviter.id()));
        assert!(joiner.invite().is_none());
    }

    #[test]
    fn record_round_trip() {
        let mut inviter = Room::create("ws://127.0.0.1:8080/relay").unwrap();
        for _ in 0..2 {
            let back: Room = inviter.to_string().parse().unwrap();
            assert_eq!(back.to_bytes(), inviter.to_bytes());
            assert_eq!(back.id(), inviter.id());
            inviter.pin(PublicKey::from([9u8; 32]));
        }
        assert!(inviter.invite().is_none());
    }

    #[test]
    fn join_signature_verifies_only_for_its_nonce_and_role() {
        let room = Room::create("wss://r.example").unwrap();
        let id: RoomId = room.id().to_string().parse().unwrap();
        let frame = room.join_frame(&[1; 32]);
        assert_eq!(id.verify_join(&[1; 32], &frame), Ok(Role::Inviter));
        assert_eq!(id.verify_join(&[2; 32], &frame), Err(close::UNAUTHORIZED));
        let mut other_role = frame;
        other_role[1] = 1;
        assert_eq!(id.verify_join(&[1; 32], &other_role), Err(close::UNAUTHORIZED));
        other_role[1] = 2;
        assert_eq!(id.verify_join(&[1; 32], &other_role), Err(close::PROTOCOL_ERROR));
        let stranger = Room::create("wss://r.example").unwrap().id();
        assert_eq!(stranger.verify_join(&[1; 32], &frame), Err(close::UNAUTHORIZED));
        assert_eq!(id.verify_join(&[1; 32], &frame[..65]), Err(close::PROTOCOL_ERROR));
    }

    #[test]
    fn rejects_bad_inputs() {
        for url in ["", "http://x", "wss://", "wss:///path", "wss://h?q=1", "wss://h#f", "wss://h h"] {
            assert!(Room::create(url).is_err(), "{url:?}");
        }
        let good = Room::create("wss://h").unwrap().invite().unwrap().to_string();
        for bad in [&good[..good.len() - 1], "wss://h", "wss://h#", &good.replace("wss", "https")] {
            assert!(bad.parse::<Invite>().is_err(), "{bad:?}");
        }
        // An inviter key of low order (here u = 0 and u = 1) could never complete a handshake.
        for low_order in [[0u8; 32], {
            let mut k = [0u8; 32];
            k[0] = 1;
            k
        }] {
            let fragment = base64::encode(&[[7u8; 32], low_order].concat());
            assert_eq!(format!("wss://h#{fragment}").parse::<Invite>().err(), Some(Error::InvalidInvite));
        }
        assert!("AAAA".parse::<RoomId>().is_err());
        assert!("".parse::<Room>().is_err());
    }
}
