//! A call's audio, end-to-end encrypted frame by frame, the way LiveKit's
//! clients do it (its "frame cryptor", in every LiveKit SDK and in the WebRTC
//! build FluffyChat ships).
//!
//! The media server forwards frames it cannot read. Each participant
//! encrypts what it sends with keys of its own and sends those keys to the
//! others over the chat service's own encryption (on Matrix, Olm to-device
//! messages); see `crate::matrix::rtc`. Keys are per participant, numbered,
//! and never ratcheted -- the settings FluffyChat 2.11 uses.
//!
//! The cipher key is derived from the key material with HKDF-SHA256, salt
//! `LKFrameEncryptionKey` and 128 zero bytes of info, into AES-128-GCM.
//!
//! An audio frame, as it travels:
//!
//! ```text
//! [ 1 byte in the clear: the Opus TOC ]
//! [ AES-128-GCM of the rest, with its 16-byte tag; the clear byte is the AAD ]
//! [ 12-byte IV ][ 12, the IV's length ][ which key ]
//! ```
//!
//! Frames the server makes up itself (silence, while a sender is paused) are
//! not encrypted, and end with the `sif_trailer` the server sent on joining.
//! They are not opened: nothing vouches for what is in them, and the server
//! is exactly who must not be able to put words in the caller's mouth. Not
//! decoding a few frames of silence is a few frames of silence.

use ring::aead::{AES_128_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use std::collections::HashMap;

/// LiveKit's default salt for deriving a frame key.
const SALT: &[u8] = b"LKFrameEncryptionKey";

/// How much of an audio frame stays in the clear: Opus's table-of-contents
/// byte, which the receiving side's packetiser looks at.
const CLEAR: usize = 1;

/// GCM's tag.
const TAG: usize = 16;

/// The trailer: the IV, its length, and the key's index.
const TRAILER: usize = NONCE_LEN + 2;

/// How many keys a participant may have. FluffyChat's key ring holds 255, so
/// an index of 255 or more is refused by it, and by this.
const KEY_RING: usize = 255;

/// One participant's key, ready to use.
struct Key {
    index: u8,
    cipher: LessSafeKey,
}

/// HKDF's info: 128 zero bytes, as LiveKit's clients pass it.
static INFO: [u8; 128] = [0; 128];

/// Derive the cipher key from `material`.
fn derive(material: &[u8]) -> LessSafeKey {
    let prk = Salt::new(HKDF_SHA256, SALT).extract(material);
    let info = [&INFO[..]];
    let okm =
        prk.expand(&info, &AES_128_GCM).expect("16 bytes is a length HKDF-SHA256 can expand to");
    LessSafeKey::new(UnboundKey::from(okm))
}

/// This side's key: what it encrypts with.
pub struct Sealer {
    key: Key,
    /// Every IV under one key must differ: four random bytes chosen with the
    /// key, then a count of frames.
    prefix: [u8; 4],
    sent: u64,
}

impl Sealer {
    pub fn new(index: u8, material: &[u8]) -> Self {
        let mut prefix = [0u8; 4];
        // A prefix that failed to be random would still be unique per frame
        // through the count; it is the key that has to be secret.
        let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut prefix);
        Self { key: Key { index, cipher: derive(material) }, prefix, sent: 0 }
    }

    /// `frame` (one Opus packet) encrypted, with its trailer.
    pub fn seal(&mut self, frame: &[u8]) -> Vec<u8> {
        let clear = frame.len().min(CLEAR);
        let mut iv = [0u8; NONCE_LEN];
        iv[..4].copy_from_slice(&self.prefix);
        iv[4..].copy_from_slice(&self.sent.to_be_bytes());
        self.sent += 1;

        let mut out = Vec::with_capacity(frame.len() + TAG + TRAILER);
        out.extend_from_slice(&frame[..clear]);
        let mut body = frame[clear..].to_vec();
        self.key
            .cipher
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(iv),
                Aad::from(&frame[..clear]),
                &mut body,
            )
            .expect("AES-GCM seals any frame shorter than 64 GB");
        out.extend_from_slice(&body);
        out.extend_from_slice(&iv);
        out.push(NONCE_LEN as u8);
        out.push(self.key.index);
        out
    }
}

/// Why a frame was not opened. Only logged: a frame lost is a few
/// milliseconds of silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unopened {
    /// Shorter than any encrypted frame can be.
    TooShort,
    /// It names a key the sender has not sent (yet).
    NoKey { participant: String, index: u8 },
    /// The key did not open it: wrong key, or damaged on the way.
    Damaged,
    /// The trailer is not one this side writes or reads.
    Malformed,
    /// Made up by the server (its silence), so not the caller's to hand on.
    FromTheServer,
}

/// The keys of everybody else, to open what they send.
#[derive(Default)]
pub struct Openers {
    keys: HashMap<String, Vec<Key>>,
    /// The server's mark on frames it made up itself, when it sent one.
    sif_trailer: Vec<u8>,
}

impl Openers {
    pub fn set_sif_trailer(&mut self, trailer: Vec<u8>) {
        self.sif_trailer = trailer;
    }

    /// `participant`'s key number `index`. A key sent again with the same
    /// number replaces it. Whether it was taken.
    pub fn add(&mut self, participant: &str, index: u8, material: &[u8]) -> bool {
        if usize::from(index) >= KEY_RING || material.is_empty() {
            return false;
        }
        let keys = self.keys.entry(participant.to_string()).or_default();
        keys.retain(|k| k.index != index);
        keys.push(Key { index, cipher: derive(material) });
        true
    }

    #[cfg(test)]
    pub fn has_keys_for(&self, participant: &str) -> bool {
        self.keys.get(participant).is_some_and(|keys| !keys.is_empty())
    }

    /// The Opus packet inside `frame`, from `participant`.
    pub fn open(&self, participant: &str, frame: &[u8]) -> Result<Vec<u8>, Unopened> {
        if !self.sif_trailer.is_empty() && frame.ends_with(&self.sif_trailer) {
            return Err(Unopened::FromTheServer);
        }
        if frame.len() < CLEAR + TAG + TRAILER {
            return Err(Unopened::TooShort);
        }
        let index = frame[frame.len() - 1];
        let iv_len = usize::from(frame[frame.len() - 2]);
        if iv_len != NONCE_LEN {
            return Err(Unopened::Malformed);
        }
        let key = self
            .keys
            .get(participant)
            .and_then(|keys| keys.iter().find(|k| k.index == index))
            .ok_or_else(|| Unopened::NoKey { participant: participant.to_string(), index })?;

        let body_end = frame.len() - TRAILER;
        let iv: [u8; NONCE_LEN] =
            frame[body_end..body_end + NONCE_LEN].try_into().map_err(|_| Unopened::Malformed)?;
        let mut body = frame[CLEAR..body_end].to_vec();
        let opened = key
            .cipher
            .open_in_place(Nonce::assume_unique_for_key(iv), Aad::from(&frame[..CLEAR]), &mut body)
            .map_err(|_| Unopened::Damaged)?;
        let mut packet = Vec::with_capacity(CLEAR + opened.len());
        packet.extend_from_slice(&frame[..CLEAR]);
        packet.extend_from_slice(opened);
        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MATERIAL: [u8; 16] = *b"0123456789abcdef";

    #[test]
    fn a_sealed_frame_opens_with_the_same_key() {
        let mut sealer = Sealer::new(3, &MATERIAL);
        let mut openers = Openers::default();
        assert!(openers.add("@me:hs:DEV", 3, &MATERIAL));
        let packet = b"\x78opus payload bytes";
        let sealed = sealer.seal(packet);
        assert_eq!(sealed.len(), packet.len() + TAG + TRAILER);
        assert_eq!(sealed[0], 0x78, "the TOC byte stays in the clear");
        assert_eq!(&sealed[sealed.len() - 2..], &[12, 3], "IV length, then key index");
        assert_eq!(openers.open("@me:hs:DEV", &sealed).unwrap(), packet);
    }

    #[test]
    fn every_frame_gets_a_different_iv() {
        let mut sealer = Sealer::new(0, &MATERIAL);
        let a = sealer.seal(b"\x78same");
        let b = sealer.seal(b"\x78same");
        let iv = |f: &[u8]| f[f.len() - TRAILER..f.len() - 2].to_vec();
        assert_ne!(iv(&a), iv(&b));
        assert_ne!(a, b);
    }

    #[test]
    fn a_frame_from_somebody_without_a_key_or_with_another_is_not_opened() {
        let mut sealer = Sealer::new(0, &MATERIAL);
        let sealed = sealer.seal(b"\x78hello");
        let mut openers = Openers::default();
        assert_eq!(
            openers.open("@me:hs:DEV", &sealed),
            Err(Unopened::NoKey { participant: "@me:hs:DEV".into(), index: 0 })
        );
        openers.add("@me:hs:DEV", 0, b"another key here");
        assert_eq!(openers.open("@me:hs:DEV", &sealed), Err(Unopened::Damaged));
        openers.add("@other:hs:DEV", 0, &MATERIAL);
        assert!(openers.open("@me:hs:DEV", &sealed).is_err(), "keys are per participant");
    }

    #[test]
    fn the_clear_byte_is_authenticated() {
        let mut sealer = Sealer::new(0, &MATERIAL);
        let mut sealed = sealer.seal(b"\x78hello");
        sealed[0] = 0x79;
        let mut openers = Openers::default();
        openers.add("p", 0, &MATERIAL);
        assert_eq!(openers.open("p", &sealed), Err(Unopened::Damaged));
    }

    #[test]
    fn short_and_strange_frames_are_refused() {
        let mut openers = Openers::default();
        openers.add("p", 0, &MATERIAL);
        assert_eq!(openers.open("p", &[0x78; 10]), Err(Unopened::TooShort));
        let mut odd = vec![0x78; 40];
        let at = odd.len() - 2;
        odd[at] = 16;
        assert_eq!(openers.open("p", &odd), Err(Unopened::Malformed));
    }

    #[test]
    fn a_key_index_outside_the_ring_is_not_taken() {
        let mut openers = Openers::default();
        assert!(!openers.add("p", 255, &MATERIAL));
        assert!(!openers.has_keys_for("p"));
        assert!(openers.add("p", 254, &MATERIAL));
    }

    #[test]
    fn frames_the_server_made_up_are_not_handed_on() {
        let mut openers = Openers::default();
        openers.add("p", 0, &MATERIAL);
        openers.set_sif_trailer(b"SIF!".to_vec());
        assert_eq!(openers.open("p", b"\xf8\xff\xfeSIF!"), Err(Unopened::FromTheServer));
    }

    #[test]
    fn the_key_is_derived_as_livekit_derives_it() {
        // HKDF-SHA256(ikm = "0123456789abcdef", salt = "LKFrameEncryptionKey",
        // info = 128 zero bytes, L = 16), computed independently with
        // Python's `cryptography`; then AES-128-GCM of nothing with an
        // all-zero IV, whose tag is a fingerprint of the key.
        let key = derive(&MATERIAL);
        let mut nothing = Vec::new();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key([0; NONCE_LEN]),
            Aad::empty(),
            &mut nothing,
        )
        .unwrap();
        assert_eq!(hex(&nothing), EXPECTED_TAG);
    }

    /// The key comes out as 72329046560d259065b2d7278c74abaa.
    const EXPECTED_TAG: &str = "f0f7d8fdf2fa76a71bf501cea3f8f02d";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
