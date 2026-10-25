//! Frame encryption (GROUP.md §4.1, §4.3): `KEY` frames under a key derived
//! from the group secret, everything else under the sender's sender key.

use chacha20poly1305::{AeadInOut, ChaCha20Poly1305, KeyInit};
use ed25519_dalek::{Signature, VerifyingKey};
use zeroize::Zeroizing;

use super::wire::{self, HEADER_LEN, KEY, SEALED_HEADER_LEN, TAG_LEN};
use super::{Group, Identity, MemberId};
use crate::noise::hmac;

const SALT_LEN: usize = 16;
pub const CHALLENGE_LEN: usize = 16;
/// `sender_key challenge echo incarnation command_next chat_next`.
const BODY_LEN: usize = 32 + CHALLENGE_LEN + CHALLENGE_LEN + 8 + 8 + 8;
const KEY_PLAINTEXT_LEN: usize = 32 + 64 + BODY_LEN;

pub type Key = Zeroizing<[u8; 32]>;
pub type Challenge = [u8; CHALLENGE_LEN];

/// What a `KEY` frame announces.
#[derive(Clone)]
pub struct Announce {
    pub identity: MemberId,
    pub key: Key,
    pub challenge: Challenge,
    /// The recipient's challenge, or zeros.
    pub echo: Challenge,
    pub incarnation: u64,
    /// The ids the sender's command and chat streams will give their next messages.
    pub next: [u64; 2],
    pub profile: Vec<u8>,
}

pub fn seal_key(group: &Group, identity: &Identity, from: u8, to: u8, announce: &Announce) -> Vec<u8> {
    seal_key_salted(group, identity, from, to, announce, crate::random())
}

/// [`seal_key`] with a given salt (for test vectors).
pub fn seal_key_salted(
    group: &Group,
    identity: &Identity,
    from: u8,
    to: u8,
    announce: &Announce,
    salt: [u8; SALT_LEN],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(BODY_LEN + announce.profile.len());
    body.extend_from_slice(&announce.key[..]);
    body.extend_from_slice(&announce.challenge);
    body.extend_from_slice(&announce.echo);
    body.extend_from_slice(&announce.incarnation.to_be_bytes());
    body.extend_from_slice(&announce.next[0].to_be_bytes());
    body.extend_from_slice(&announce.next[1].to_be_bytes());
    body.extend_from_slice(&announce.profile);
    let signature = identity.sign(&signed(group, from, to, &body));

    let mut plaintext = Zeroizing::new(Vec::with_capacity(96 + body.len()));
    plaintext.extend_from_slice(identity.id().as_bytes());
    plaintext.extend_from_slice(&signature);
    plaintext.extend_from_slice(&body);
    let mut frame = vec![KEY, from, to];
    frame.extend_from_slice(&salt);
    let key = Zeroizing::new(hmac(group.key_secret(), &[&salt]));
    encrypt(&key, [0; 12], &[KEY, from, to], &plaintext, &mut frame);
    frame
}

/// Opens and verifies a `KEY` frame.
pub fn open_key(group: &Group, frame: &[u8]) -> Option<Announce> {
    let (header, rest) = frame.split_at_checked(HEADER_LEN)?;
    let (salt, ciphertext) = rest.split_at_checked(SALT_LEN)?;
    let key = Zeroizing::new(hmac(group.key_secret(), &[salt]));
    let plaintext = Zeroizing::new(decrypt(&key, [0; 12], header, ciphertext)?);
    if plaintext.len() < KEY_PLAINTEXT_LEN || plaintext.len() > KEY_PLAINTEXT_LEN + wire::MAX_PROFILE_LEN {
        return None;
    }
    let identity: [u8; 32] = plaintext[..32].try_into().unwrap();
    let signature = Signature::from_bytes(plaintext[32..96].try_into().unwrap());
    let body = &plaintext[96..];
    VerifyingKey::from_bytes(&identity)
        .ok()?
        .verify_strict(&signed(group, header[1], header[2], body), &signature)
        .ok()?;
    let u64_at = |at: usize| u64::from_be_bytes(body[at..at + 8].try_into().unwrap());
    Some(Announce {
        identity: MemberId(identity),
        key: Zeroizing::new(body[..32].try_into().unwrap()),
        challenge: body[32..48].try_into().unwrap(),
        echo: body[48..64].try_into().unwrap(),
        incarnation: u64_at(64),
        next: [u64_at(72), u64_at(80)],
        profile: body[BODY_LEN..].to_vec(),
    })
}

fn signed(group: &Group, from: u8, to: u8, body: &[u8]) -> Vec<u8> {
    [wire::KEY_CONTEXT, group.id().as_bytes(), &[from, to], body].concat()
}

/// A sealed frame: `type from to counter ‖ ciphertext ‖ tag`.
pub fn seal(key: &[u8; 32], kind: u8, from: u8, to: u8, counter: u32, plaintext: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(SEALED_HEADER_LEN + plaintext.len() + TAG_LEN);
    frame.extend_from_slice(&[kind, from, to]);
    frame.extend_from_slice(&counter.to_be_bytes());
    let header: [u8; SEALED_HEADER_LEN] = frame[..].try_into().unwrap();
    encrypt(key, nonce(kind, counter), &header, plaintext, &mut frame);
    frame
}

/// The counter and plaintext of a sealed frame, if it authenticates.
pub fn open(key: &[u8; 32], frame: &[u8]) -> Option<(u32, Vec<u8>)> {
    let (header, ciphertext) = frame.split_at_checked(SEALED_HEADER_LEN)?;
    let counter = u32::from_be_bytes(header[3..].try_into().unwrap());
    Some((counter, decrypt(key, nonce(header[0], counter), header, ciphertext)?))
}

fn nonce(kind: u8, counter: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[0] = kind;
    nonce[8..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

fn encrypt(key: &[u8; 32], nonce: [u8; 12], ad: &[u8], plaintext: &[u8], out: &mut Vec<u8>) {
    let start = out.len();
    out.extend_from_slice(plaintext);
    let tag = ChaCha20Poly1305::new(key.into())
        .encrypt_inout_detached(&nonce.into(), ad, (&mut out[start..]).into())
        .expect("frames are far below ChaCha20-Poly1305's length limit");
    out.extend_from_slice(&tag);
}

fn decrypt(key: &[u8; 32], nonce: [u8; 12], ad: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let len = ciphertext.len().checked_sub(TAG_LEN)?;
    let mut plaintext = ciphertext[..len].to_vec();
    let tag: [u8; TAG_LEN] = ciphertext[len..].try_into().unwrap();
    ChaCha20Poly1305::new(key.into())
        .decrypt_inout_detached(&nonce.into(), ad, plaintext.as_mut_slice().into(), &tag.into())
        .ok()?;
    Some(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announce(identity: &Identity) -> Announce {
        Announce {
            identity: identity.id(),
            key: Zeroizing::new([5; 32]),
            challenge: [6; 16],
            echo: [0; 16],
            incarnation: 77,
            next: [3, 4],
            profile: b"alice".to_vec(),
        }
    }

    #[test]
    fn key_frames_open_only_in_their_group_and_position() {
        let group = Group::create("wss://r").unwrap();
        let identity = Identity::generate();
        let frame = seal_key(&group, &identity, 2, wire::BROADCAST, &announce(&identity));
        let back = open_key(&group, &frame).unwrap();
        assert_eq!(
            (back.identity, back.incarnation, back.next, &back.profile[..]),
            (identity.id(), 77, [3, 4], &b"alice"[..])
        );
        assert_eq!(back.key[..], [5; 32]);

        assert!(open_key(&Group::create("wss://r").unwrap(), &frame).is_none());
        // Moving it to another slot or recipient breaks it.
        for i in [1, 2] {
            let mut moved = frame.clone();
            moved[i] ^= 1;
            assert!(open_key(&group, &moved).is_none());
        }
        let mut tampered = frame.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open_key(&group, &tampered).is_none());
        // A member that signs with another identity than it claims is refused.
        let mut forged = announce(&identity);
        forged.identity = Identity::generate().id();
        let other = Identity::generate();
        let frame = seal_key(&group, &other, 2, 3, &forged);
        assert_eq!(open_key(&group, &frame).unwrap().identity, other.id());
    }

    #[test]
    fn sealed_frames_bind_header_and_counter() {
        let key = [9u8; 32];
        let frame = seal(&key, wire::COMMAND, 1, 0xFF, 41, b"hi");
        assert_eq!(frame.len(), SEALED_HEADER_LEN + 2 + TAG_LEN);
        assert_eq!(open(&key, &frame), Some((41, b"hi".to_vec())));
        for i in 0..SEALED_HEADER_LEN {
            let mut bad = frame.clone();
            bad[i] ^= 1;
            assert!(open(&key, &bad).is_none(), "byte {i}");
        }
        assert!(open(&[8; 32], &frame).is_none());
    }
}
