//! Groups (GROUP.md): up to 32 members, one group secret, end-to-end
//! encrypted commands, chat, voice and state transfer over an untrusted,
//! stateless relay.
//!
//! - [`Group`]: the group secret and its invite, the only credential.
//! - [`Identity`]: a member's Ed25519 key, chosen and kept by the application.
//! - [`Member`]: the member state machine, free of I/O like [`crate::Peer`].
//!   Drive it with any WebSocket, or use [`crate::client::group`].

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::noise::hmac;
use crate::room::RoomId;
use crate::{Error, base64};
use transport::relay_url;

#[cfg(feature = "certificate")]
pub mod certificate;
mod member;
mod seal;
pub mod transport;
mod video;
pub mod wire;

pub use member::{Channel, Config, Event, Member, Output, SendError, State};
pub use wire::Class;

/// A group: the relay it meets on and its secret. Its string form is the
/// invite, `<relay URL>#<base64url(secret)>`, which is all a member needs and
/// all anyone needs to become one. Treat it as a secret.
#[derive(Clone)]
pub struct Group {
    relay: String,
    secret: Zeroizing<[u8; 32]>,
    /// Signs `JOIN`; its public key is the group ID.
    signer: SigningKey,
    /// Derives the keys that protect `KEY` frames.
    key_secret: Zeroizing<[u8; 32]>,
}

impl Group {
    /// Creates a new group on `relay`: an `https://` or `quic://` URL. Groups
    /// travel over QUIC only (see [`transport`]); a WebSocket URL is refused.
    pub fn create(relay: &str) -> Result<Group, Error> {
        Ok(Group::new(relay_url(relay)?, crate::random()))
    }

    pub(crate) fn new(relay: String, secret: [u8; 32]) -> Group {
        let seed = Zeroizing::new(hmac(&secret, &[b"peerconnect/g1 group"]));
        Group {
            relay,
            signer: SigningKey::from_bytes(&seed),
            key_secret: Zeroizing::new(hmac(&secret, &[b"peerconnect/g1 keys"])),
            secret: Zeroizing::new(secret),
        }
    }

    /// Where the group meets on its relay.
    pub fn id(&self) -> RoomId {
        RoomId::from_key(self.signer.verifying_key().to_bytes())
    }

    pub fn relay(&self) -> &str {
        &self.relay
    }

    /// The same group on another relay (all members must agree on it).
    pub fn with_relay(&self, relay: &str) -> Result<Group, Error> {
        Ok(Group::new(relay_url(relay)?, *self.secret))
    }

    /// The URL of the group on its relay, which [`transport::target`] turns
    /// into the QUIC endpoint to connect to.
    pub fn endpoint(&self) -> String {
        format!("{}/g1/{}", self.relay, self.id())
    }

    pub(crate) fn key_secret(&self) -> &[u8; 32] {
        &self.key_secret
    }

    /// The `JOIN` frame answering a relay's `HELLO` nonce.
    pub(crate) fn join_frame(&self, nonce: &[u8; 32], token: &[u8; wire::TOKEN_LEN]) -> [u8; wire::JOIN_LEN] {
        let signature = self.signer.sign(&wire::join_message(nonce, token));
        let mut frame = [0u8; wire::JOIN_LEN];
        frame[0] = wire::JOIN;
        frame[1..17].copy_from_slice(token);
        frame[17..].copy_from_slice(&signature.to_bytes());
        frame
    }
}

impl FromStr for Group {
    type Err = Error;

    /// Parses an invite.
    fn from_str(s: &str) -> Result<Self, Error> {
        let (relay, fragment) = s.trim().split_once('#').ok_or(Error::InvalidInvite)?;
        let secret = Zeroizing::new(base64::decode_array::<32>(fragment).ok_or(Error::InvalidInvite)?);
        Ok(Group::new(relay_url(relay)?, *secret))
    }
}

/// The invite.
impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.relay, base64::encode(&self.secret[..]))
    }
}

impl fmt::Debug for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Group({}, {})", self.relay, self.id())
    }
}

impl RoomId {
    /// Relay side: checks a group `JOIN` frame against the `HELLO` nonce and
    /// returns the member's join token, or the close code to reject it with.
    pub fn verify_group_join(&self, nonce: &[u8; 32], frame: &[u8]) -> Result<[u8; wire::TOKEN_LEN], u16> {
        use crate::wire::close;
        if frame.len() != wire::JOIN_LEN || frame[0] != wire::JOIN {
            return Err(close::PROTOCOL_ERROR);
        }
        let token: [u8; wire::TOKEN_LEN] = frame[1..17].try_into().unwrap();
        let signature = Signature::from_bytes(frame[17..].try_into().unwrap());
        let key = VerifyingKey::from_bytes(self.as_bytes()).map_err(|_| close::UNAUTHORIZED)?;
        key.verify_strict(&wire::join_message(nonce, &token), &signature).map_err(|_| close::UNAUTHORIZED)?;
        Ok(token)
    }
}

/// A member's identity: an Ed25519 key pair. Persist [`Identity::seed`] to
/// be recognised again; other members know you by [`Identity::id`].
#[derive(Clone)]
pub struct Identity(SigningKey);

impl Identity {
    pub fn generate() -> Identity {
        Identity::from_seed(crate::random())
    }

    pub fn from_seed(seed: [u8; 32]) -> Identity {
        Identity(SigningKey::from_bytes(&seed))
    }

    /// The secret to persist.
    pub fn seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes())
    }

    pub fn id(&self) -> MemberId {
        MemberId(self.0.verifying_key().to_bytes())
    }

    /// The join token in `group`: lets the relay replace this identity's old
    /// connection, and links nothing across groups.
    pub(crate) fn token(&self, group: &RoomId) -> [u8; wire::TOKEN_LEN] {
        let seed = self.seed();
        let mac = hmac(&seed[..], &[b"peerconnect/g1 token", group.as_bytes()]);
        mac[..wire::TOKEN_LEN].try_into().unwrap()
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.0.sign(message).to_bytes()
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Identity({})", self.id())
    }
}

/// A member, as others know it: its identity public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemberId(pub [u8; 32]);

impl MemberId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for MemberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&base64::encode(&self.0))
    }
}

impl fmt::Debug for MemberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.to_string();
        write!(f, "MemberId({})", &text[..8])
    }
}

impl FromStr for MemberId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        base64::decode_array(s).map(MemberId).ok_or(Error::InvalidRoomId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_round_trip() {
        let group = Group::create("https://relay.example.com/").unwrap();
        let invite = group.to_string();
        assert!(invite.starts_with("https://relay.example.com#"));
        assert_eq!(invite.len(), "https://relay.example.com#".len() + 43);
        let back: Group = invite.parse().unwrap();
        assert_eq!(back.id(), group.id());
        assert_eq!(back.key_secret(), group.key_secret());
        assert_eq!(back.endpoint(), format!("https://relay.example.com/g1/{}", group.id()));
        // A two-peer invite is not a group invite, and vice versa.
        let room = crate::Room::create("wss://r.example").unwrap().invite().unwrap().to_string();
        assert!(room.parse::<Group>().is_err());
        assert!(invite.parse::<crate::Invite>().is_err());
        // Groups have no WebSocket transport.
        assert!(Group::create("wss://relay.example.com").is_err());
        let secret = invite.split_once('#').unwrap().1;
        assert!(format!("wss://relay.example.com#{secret}").parse::<Group>().is_err());
    }

    #[test]
    fn join_verifies_only_for_its_nonce_and_token() {
        let group = Group::create("https://r.example").unwrap();
        let id = group.id();
        let frame = group.join_frame(&[1; 32], &[7; 16]);
        assert_eq!(id.verify_group_join(&[1; 32], &frame), Ok([7; 16]));
        assert!(id.verify_group_join(&[2; 32], &frame).is_err());
        let mut other = frame;
        other[1] ^= 1;
        assert!(id.verify_group_join(&[1; 32], &other).is_err());
        assert!(Group::create("https://r.example").unwrap().id().verify_group_join(&[1; 32], &frame).is_err());
        assert!(id.verify_group_join(&[1; 32], &frame[..80]).is_err());
    }

    #[test]
    fn moving_relays_keeps_the_group_and_ids_print_and_parse() {
        let group = Group::create("https://a.example").unwrap();
        let moved = group.with_relay("quic://b.example/sub/").unwrap();
        assert_eq!((moved.id(), moved.relay()), (group.id(), "quic://b.example/sub"));
        assert!(group.with_relay("http://nope").is_err());
        let quic = group.with_relay("quic://203.0.113.9:4433").unwrap();
        assert_eq!(quic.endpoint(), format!("quic://203.0.113.9:4433/g1/{}", group.id()));
        assert_eq!(quic.to_string().parse::<Group>().unwrap().relay(), "quic://203.0.113.9:4433");
        assert!(format!("{group:?}").starts_with("Group(https://a.example, "));
        let identity = Identity::generate();
        let id = identity.id();
        assert_eq!(id.to_string().parse::<MemberId>(), Ok(id));
        assert!("not an id".parse::<MemberId>().is_err());
        assert_eq!(format!("{id:?}"), format!("MemberId({})", &id.to_string()[..8]));
        assert_eq!(format!("{identity:?}"), format!("Identity({id})"));
    }

    #[test]
    fn tokens_are_per_identity_and_group() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let (g, h) = (Group::create("https://r").unwrap().id(), Group::create("https://r").unwrap().id());
        assert_eq!(a.token(&g), Identity::from_seed(*a.seed()).token(&g));
        assert_ne!(a.token(&g), a.token(&h));
        assert_ne!(a.token(&g), b.token(&g));
    }
}
