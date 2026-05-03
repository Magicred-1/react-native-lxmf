/// Tests for the LXMF wire payload format.
///
/// Wire layout (per LXMF spec and our encoder in node.rs):
///   [0..16]  dest hash  (16 bytes)
///   [16..32] source hash (16 bytes)  ← used as reply address
///   [32..96] Ed25519 signature (64 bytes)
///   [96+]    msgpack payload [timestamp: f64, title: bin, body: bin, fields: map]

use crate::node::{
    decode_lxmf_payload, encode_lxmf_msgpack, build_fields_msgpack,
    lxmf_event_from_bytes, LxmfEvent,
};

fn dest() -> [u8; 16] { [0xd0; 16] }
fn source() -> [u8; 16] { [0x50; 16] }

/// Build a synthetic LXMF wire payload as a sender would produce it:
/// [dest(16)][src(16)][sig(64)][msgpack(body)].
fn build_wire_payload(dest_hash: &[u8; 16], src_hash: &[u8; 16], body: &[u8]) -> Vec<u8> {
    let fields = build_fields_msgpack(None);
    let mp = encode_lxmf_msgpack(1_700_000_000.0, b"", body, &fields);
    let sig = vec![0u8; 64]; // placeholder signature
    let mut payload = Vec::with_capacity(16 + 16 + 64 + mp.len());
    payload.extend_from_slice(dest_hash);
    payload.extend_from_slice(src_hash);
    payload.extend_from_slice(&sig);
    payload.extend_from_slice(&mp);
    payload
}

// ── wire layout assertions ────────────────────────────────────────────────────

#[test]
fn dest_hash_at_bytes_0_to_16() {
    let d = dest();
    let payload = build_wire_payload(&d, &source(), b"x");
    assert_eq!(&payload[0..16], &d);
}

#[test]
fn source_hash_at_bytes_16_to_32() {
    let s = source();
    let payload = build_wire_payload(&dest(), &s, b"x");
    assert_eq!(&payload[16..32], &s);
}

#[test]
fn signature_region_at_bytes_32_to_96() {
    // sig region is bytes 32..96 (64 bytes); decoder skips everything [0..96]
    let payload = build_wire_payload(&dest(), &source(), b"hello");
    assert_eq!(payload.len(), 16 + 16 + 64 + /* msgpack overhead */ payload.len() - 96);
    // decoder starts at [96] — confirm msgpack fixarray marker there
    assert_eq!(payload[96], 0x94, "msgpack fixarray(4) at byte 96");
}

#[test]
fn decode_ignores_header_and_extracts_body() {
    let body = b"expected body content";
    let payload = build_wire_payload(&dest(), &source(), body);
    let dec = decode_lxmf_payload(&payload).expect("decode succeeds");
    assert_eq!(dec.body, body);
}

#[test]
fn too_short_payload_does_not_decode() {
    // < 97 bytes → no msgpack
    let short: Vec<u8> = vec![0u8; 96];
    assert!(decode_lxmf_payload(&short).is_none());
}

#[test]
fn minimum_valid_payload_length_is_97_bytes() {
    // 96 header + 1 byte msgpack (fixarray starts at 97th byte)
    let mp = encode_lxmf_msgpack(0.0, b"", b"", &[0x80]);
    let mut payload = vec![0u8; 96];
    payload.extend_from_slice(&mp);
    assert!(payload.len() >= 97);
    assert!(decode_lxmf_payload(&payload).is_some());
}

// ── source address used for reply ─────────────────────────────────────────────

/// The source address in a received MessageReceived event must match
/// whatever hash was used as `received.destination` by rns_transport —
/// in our data receiver we pass that directly as `src` to lxmf_event_from_bytes.
/// This test verifies lxmf_event_from_bytes preserves that address verbatim,
/// since it's what the app uses as the reply-to address.
#[test]
fn received_event_source_matches_transport_source_addr() {
    let transport_src: [u8; 16] = [0xab; 16];
    let payload = build_wire_payload(&dest(), &source(), b"hi");
    let event = lxmf_event_from_bytes(transport_src, payload, None);
    match event {
        LxmfEvent::MessageReceived { source, .. } => {
            assert_eq!(source, transport_src,
                "reply address must match transport-level source, not LXMF wire source");
        }
        _ => panic!("expected MessageReceived"),
    }
}

/// Different transport_src values produce different reply addresses.
/// Ensures isolation between two senders.
#[test]
fn two_senders_produce_distinct_reply_addresses() {
    let src_a: [u8; 16] = [0x01; 16];
    let src_b: [u8; 16] = [0x02; 16];
    let payload = build_wire_payload(&dest(), &[0u8; 16], b"hi");

    let ev_a = lxmf_event_from_bytes(src_a, payload.clone(), None);
    let ev_b = lxmf_event_from_bytes(src_b, payload, None);

    let addr_a = match ev_a { LxmfEvent::MessageReceived { source, .. } => source, _ => panic!() };
    let addr_b = match ev_b { LxmfEvent::MessageReceived { source, .. } => source, _ => panic!() };
    assert_ne!(addr_a, addr_b);
}

/// Verify that a payload with mismatched LXMF wire source (bytes 16..32)
/// still uses the transport-level source for the reply address — confirming
/// we correctly use rns_transport's routing info, not the self-reported source.
#[test]
fn transport_source_wins_over_lxmf_wire_source() {
    let transport_src: [u8; 16] = [0xff; 16];
    let lxmf_wire_src: [u8; 16] = [0x00; 16]; // different from transport_src
    let payload = build_wire_payload(&dest(), &lxmf_wire_src, b"msg");

    let event = lxmf_event_from_bytes(transport_src, payload, None);
    match event {
        LxmfEvent::MessageReceived { source, .. } => {
            assert_eq!(source, transport_src);
            assert_ne!(source, lxmf_wire_src);
        }
        _ => panic!("expected MessageReceived"),
    }
}

// ── body + title roundtrip ────────────────────────────────────────────────────

#[test]
fn body_and_title_survive_wire_roundtrip() {
    let fields = build_fields_msgpack(None);
    let mp = encode_lxmf_msgpack(1_700_000_000.0, b"the title", b"the body", &fields);
    let mut payload = vec![0u8; 96];
    payload.extend_from_slice(&mp);

    let dec = decode_lxmf_payload(&payload).expect("decode");
    assert_eq!(dec.title, b"the title");
    assert_eq!(dec.body, b"the body");
}

#[test]
fn empty_title_is_empty_vec_after_decode() {
    let fields = build_fields_msgpack(None);
    let mp = encode_lxmf_msgpack(0.0, b"", b"body only", &fields);
    let mut payload = vec![0u8; 96];
    payload.extend_from_slice(&mp);

    let dec = decode_lxmf_payload(&payload).expect("decode");
    assert!(dec.title.is_empty());
    assert_eq!(dec.body, b"body only");
}
