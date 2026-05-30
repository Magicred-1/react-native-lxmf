//! LXMF signature spec-compliance tests.
//!
//! The signed region is `dest + src + payload_without_stamp + SHA256(that)`
//! (Python `LXMessage.pack`, LXMF-rs `WireMessage::sign`). These tests prove the
//! crate signs and verifies that exact region — so it interoperates with
//! Sideband / standard LXMF clients — plus a legacy fallback for pre-spec crate
//! releases, and correct stamp-stripping for 5-element payloads.

use std::collections::HashMap;

use rns_transport::destination::{DestinationDesc, DestinationName};
use rns_transport::identity::PrivateIdentity;

use crate::node::{
    build_fields_msgpack, encode_lxmf_msgpack, lxmf_message_hash, lxmf_sign,
    lxmf_signed_payload, verify_lxmf_signature,
};

fn sender() -> PrivateIdentity {
    PrivateIdentity::new_from_name("lxmf-signature-spec-test")
}

fn src_hash(id: &PrivateIdentity) -> [u8; 16] {
    let mut s = [0u8; 16];
    s.copy_from_slice(&id.address_hash().as_slice()[..16]);
    s
}

fn peer_map(id: &PrivateIdentity, src: [u8; 16]) -> HashMap<[u8; 16], DestinationDesc> {
    let mut m = HashMap::new();
    m.insert(
        src,
        DestinationDesc {
            identity: *id.as_identity(),
            address_hash: *id.address_hash(),
            name: DestinationName::new("lxmf", "delivery"),
        },
    );
    m
}

fn payload(body: &[u8]) -> Vec<u8> {
    encode_lxmf_msgpack(1_700_000_000.0, b"", body, &build_fields_msgpack(None))
}

/// Assemble a wire packet `[dest(16)][src(16)][sig(64)][payload]`.
fn wire(dest: [u8; 16], src: [u8; 16], sig: [u8; 64], pl: &[u8]) -> Vec<u8> {
    let mut w = Vec::with_capacity(96 + pl.len());
    w.extend_from_slice(&dest);
    w.extend_from_slice(&src);
    w.extend_from_slice(&sig);
    w.extend_from_slice(pl);
    w
}

const DEST: [u8; 16] = [0xd0; 16];

// ── spec round-trip ───────────────────────────────────────────────────────────

#[test]
fn spec_signed_packet_verifies() {
    let id = sender();
    let src = src_hash(&id);
    let pl = payload(b"hello spec");
    let sig = lxmf_sign(&id, &DEST, &src, &pl);
    let w = wire(DEST, src, sig, &pl);
    assert_eq!(verify_lxmf_signature(&w, &peer_map(&id, src)), Some(true));
}

#[test]
fn tampered_payload_is_rejected() {
    let id = sender();
    let src = src_hash(&id);
    let pl = payload(b"original");
    let sig = lxmf_sign(&id, &DEST, &src, &pl);
    let mut w = wire(DEST, src, sig, &pl);
    *w.last_mut().unwrap() ^= 0xff; // flip a payload byte after signing
    assert_eq!(verify_lxmf_signature(&w, &peer_map(&id, src)), Some(false));
}

#[test]
fn wrong_signer_is_rejected() {
    let id = sender();
    let other = PrivateIdentity::new_from_name("someone-else");
    let src = src_hash(&id);
    let pl = payload(b"msg");
    // signed by `other`, but the map maps src -> id's identity
    let sig = lxmf_sign(&other, &DEST, &src, &pl);
    let w = wire(DEST, src, sig, &pl);
    assert_eq!(verify_lxmf_signature(&w, &peer_map(&id, src)), Some(false));
}

#[test]
fn unknown_peer_returns_none() {
    let id = sender();
    let src = src_hash(&id);
    let pl = payload(b"msg");
    let sig = lxmf_sign(&id, &DEST, &src, &pl);
    let w = wire(DEST, src, sig, &pl);
    // empty peer map -> sender not cached -> None (accept-with-warning path)
    assert_eq!(verify_lxmf_signature(&w, &HashMap::new()), None);
}

// ── legacy fallback (pre-spec crate clients) ──────────────────────────────────

#[test]
fn legacy_signed_packet_still_verifies() {
    let id = sender();
    let src = src_hash(&id);
    let pl = payload(b"legacy msg");
    // OLD scheme: sign dest+src+payload with NO appended hash.
    let mut legacy_signed = Vec::new();
    legacy_signed.extend_from_slice(&DEST);
    legacy_signed.extend_from_slice(&src);
    legacy_signed.extend_from_slice(&pl);
    let sig = id.sign(&legacy_signed).to_bytes();
    let w = wire(DEST, src, sig, &pl);
    assert_eq!(verify_lxmf_signature(&w, &peer_map(&id, src)), Some(true),
        "legacy fallback must accept old-region signatures during rollout");
}

// ── stamp handling (5-element payload, signature over 4 elements) ──────────────

#[test]
fn five_element_stamp_payload_verifies_over_stripped_four() {
    let id = sender();
    let src = src_hash(&id);
    let four = payload(b"stamped"); // 0x94 + [ts,title,content,fields]
    // Build a 5-element wire payload: array header 0x94 -> 0x95, append a stamp.
    let mut five = four.clone();
    five[0] = 0x95;
    five.extend_from_slice(&[0xc4, 0x04, b's', b't', b'm', b'p']); // bin8 "stmp"
    // Reference clients sign over the 4-element (stamp-less) form.
    let sig = lxmf_sign(&id, &DEST, &src, &four);
    let w = wire(DEST, src, sig, &five);
    assert_eq!(verify_lxmf_signature(&w, &peer_map(&id, src)), Some(true),
        "verifier must strip the stamp before checking the signature");
}

#[test]
fn signed_payload_strips_stamp_to_canonical_four() {
    let four = payload(b"x");
    let mut five = four.clone();
    five[0] = 0x95;
    five.extend_from_slice(&[0xc4, 0x04, b's', b't', b'm', b'p']);
    let mut wire96 = vec![0u8; 96];
    wire96.extend_from_slice(&five);
    let stripped = lxmf_signed_payload(&wire96).expect("strip");
    assert_eq!(stripped, four, "stamp-stripped payload == canonical 4-element msgpack");
}

// ── hash helper sanity ────────────────────────────────────────────────────────

#[test]
fn message_hash_is_sha256_32_bytes() {
    let h = lxmf_message_hash(b"abc");
    // SHA-256("abc")
    assert_eq!(
        hex::encode(h),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
