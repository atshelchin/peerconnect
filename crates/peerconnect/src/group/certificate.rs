//! The weekly certificate (GROUP.md §3.7): what a relay without certificate
//! files serves over QUIC, and what browsers pin for a `quic://` relay.
//!
//! Browsers accept a certificate nobody signed for WebTransport if they are
//! given its SHA-256 hash in advance and it is valid for at most 14 days.
//! So the certificate for each week is a pure function of the week: its key
//! is derived from public data, its fields are fixed, and its signature is
//! deterministic (RFC 6979). Any member can compute the hashes of the
//! certificates a relay may be serving now, with no domain, no certificate
//! authority, and nothing to distribute. That the key is public gives up
//! nothing members rely on: the relay is untrusted anyway (§9), and TLS
//! still hides the traffic from anyone merely watching the network.

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use sha2::{Digest, Sha256};

/// One week, in seconds: certificates follow each other at this interval.
pub const WEEK: u64 = 7 * 24 * 3600;
/// A certificate is valid from a day before its week to 12 days into it:
/// 13 days in all (browsers accept up to 14), overlapping the next one.
const BEFORE: u64 = 24 * 3600;
const AFTER: u64 = 12 * 24 * 3600;
const CONTEXT: &[u8] = b"peerconnect/g1 webtransport certificate";
const NAME: &[u8] = b"peerconnect";

/// The certificate for one week, with its private key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeeklyCertificate {
    /// X.509, DER.
    pub der: Vec<u8>,
    /// The private key: SEC 1 `ECPrivateKey`, DER.
    pub key: Vec<u8>,
    /// SHA-256 of `der`, as browsers pin it.
    pub hash: [u8; 32],
}

/// The week `unix_seconds` falls in.
pub fn week_of(unix_seconds: u64) -> u64 {
    unix_seconds / WEEK
}

/// The hashes to pin at `unix_seconds`: those of last week's, this week's
/// and next week's certificates, so that a relay whose clock is a day off
/// from the member's still matches.
pub fn hashes(unix_seconds: u64) -> [[u8; 32]; 3] {
    let week = week_of(unix_seconds);
    [week.saturating_sub(1), week, week + 1].map(|w| certificate(w).hash)
}

/// The certificate for `week`.
pub fn certificate(week: u64) -> WeeklyCertificate {
    let signing = signing_key(week);
    let point = signing.verifying_key().to_sec1_point(false);
    let public = point.as_bytes();

    let name = seq(&[&set(&[&seq(&[&oid(OID_COMMON_NAME), &tlv(0x0C, NAME)])])]);
    let start = week * WEEK;
    let tbs = seq(&[
        &tlv(0xA0, &integer(&[2])), // version: v3
        &integer(&(week + 1).to_be_bytes()),
        &seq(&[&oid(OID_ECDSA_SHA256)]),
        &name,
        &seq(&[&utc_time(start - BEFORE.min(start)), &utc_time(start + AFTER)]),
        &name,
        &seq(&[&seq(&[&oid(OID_EC_PUBLIC_KEY), &oid(OID_P256)]), &bit_string(public)]),
        // Extensions: not a certificate authority.
        &tlv(0xA3, &seq(&[&seq(&[&oid(OID_BASIC_CONSTRAINTS), &tlv(0x04, &seq(&[]))])])),
    ]);
    let signature: Signature = signing.sign(&tbs);
    let (r, s) = signature.split_bytes();
    let der = seq(&[&tbs, &seq(&[&oid(OID_ECDSA_SHA256)]), &bit_string(&seq(&[&integer(&r), &integer(&s)]))]);

    let secret = signing.to_bytes();
    let key = seq(&[&integer(&[1]), &tlv(0x04, &secret), &tlv(0xA0, &oid(OID_P256)), &tlv(0xA1, &bit_string(public))]);
    let hash = Sha256::digest(&der).into();
    WeeklyCertificate { der, key, hash }
}

/// The week's key: SHA-256 of the context and the week, hashed again in the
/// (vanishingly unlikely) case that is not a valid P-256 scalar.
fn signing_key(week: u64) -> SigningKey {
    let mut seed: [u8; 32] = Sha256::new().chain_update(CONTEXT).chain_update(week.to_be_bytes()).finalize().into();
    loop {
        if let Ok(key) = SigningKey::from_bytes(&seed.into()) {
            return key;
        }
        seed = Sha256::digest(seed).into();
    }
}

// DER, just as much as a certificate needs.

const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
const OID_ECDSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1D, 0x13];

fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match value.len() {
        n if n < 0x80 => out.push(n as u8),
        n if n < 0x100 => out.extend_from_slice(&[0x81, n as u8]),
        n => out.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]),
    }
    out.extend_from_slice(value);
    out
}

fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn set(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x31, &parts.concat())
}

fn oid(der: &[u8]) -> Vec<u8> {
    tlv(0x06, der)
}

fn bit_string(bytes: &[u8]) -> Vec<u8> {
    tlv(0x03, &[&[0][..], bytes].concat())
}

/// A non-negative INTEGER from big-endian bytes.
fn integer(bytes: &[u8]) -> Vec<u8> {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len().saturating_sub(1));
    let bytes = &bytes[start..];
    if bytes.first().is_some_and(|&b| b & 0x80 != 0) {
        tlv(0x02, &[&[0][..], bytes].concat())
    } else {
        tlv(0x02, bytes)
    }
}

/// UTCTime, `YYMMDDHHMMSSZ` (years 1950–2049).
fn utc_time(unix_seconds: u64) -> Vec<u8> {
    let days = (unix_seconds / 86_400) as i64;
    let secs = unix_seconds % 86_400;
    // Days to civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let text =
        format!("{:02}{:02}{:02}{:02}{:02}{:02}Z", year % 100, month, day, secs / 3600, secs / 60 % 60, secs % 60);
    tlv(0x17, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificates_are_a_function_of_the_week() {
        let a = certificate(2900);
        assert_eq!(a, certificate(2900), "deterministic");
        assert_ne!(a.hash, certificate(2901).hash);
        assert_eq!(a.hash, <[u8; 32]>::from(Sha256::digest(&a.der)));
        // Week 2900 starts 2025-07-31; valid from the day before for 13 days.
        assert!(a.der.windows(13).any(|w| w == b"250730000000Z"));
        assert!(a.der.windows(13).any(|w| w == b"250812000000Z"));
        let now = 2900 * WEEK + 3600;
        assert_eq!(hashes(now), [certificate(2899).hash, a.hash, certificate(2901).hash]);
        assert_eq!(week_of(now), 2900);
    }

    /// `test-vectors/g1-certificate.json`; `PEERCONNECT_WRITE_VECTORS=1` regenerates it.
    #[test]
    fn certificate_vectors_are_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-vectors/g1-certificate.json");
        let hex = |b: &[u8]| b.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let entries: Vec<String> = [2965u64, 3000]
            .iter()
            .map(|&week| {
                let c = certificate(week);
                format!(
                    "    {{\"week\": {week}, \"private_key\": \"{}\", \"certificate\": \"{}\", \"sha256\": \"{}\"}}",
                    hex(&signing_key(week).to_bytes()),
                    hex(&c.der),
                    hex(&c.hash)
                )
            })
            .collect();
        let vectors = format!(
            "{{\n  \"description\": \"GROUP.md §3.7: the weekly certificate for some weeks\",\n  \"context\": \"{}\",\n  \"weeks\": [\n{}\n  ]\n}}\n",
            std::str::from_utf8(CONTEXT).unwrap(),
            entries.join(",\n")
        );
        if std::env::var_os("PEERCONNECT_WRITE_VECTORS").is_some() {
            std::fs::write(path, &vectors).unwrap();
        }
        let on_disk = std::fs::read_to_string(path).expect("the vectors (set PEERCONNECT_WRITE_VECTORS=1)");
        assert!(on_disk == vectors, "test-vectors/g1-certificate.json is out of date");
    }

    #[test]
    fn utc_times_and_integers() {
        assert_eq!(utc_time(0), tlv(0x17, b"700101000000Z"));
        assert_eq!(utc_time(951_782_400), tlv(0x17, b"000229000000Z")); // a leap day
        assert_eq!(utc_time(1_792_976_461), tlv(0x17, b"261026010101Z"));
        assert_eq!(integer(&[0, 0, 5]), vec![2, 1, 5]);
        assert_eq!(integer(&[0x80]), vec![2, 2, 0, 0x80]);
        assert_eq!(integer(&[0]), vec![2, 1, 0]);
        assert_eq!(tlv(4, &[0; 200])[..3], [4, 0x81, 200]);
        assert_eq!(tlv(4, &[0; 300])[..4], [4, 0x82, 1, 44]);
    }
}
