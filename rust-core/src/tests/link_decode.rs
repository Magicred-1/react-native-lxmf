//! Regression tests for the "garbled received message" bug.
//!
//! Reference LXMF clients (Sideband / python-LXMF) strip the leading 16-byte
//! destination hash for **opportunistic** delivery — they send `packed[16:]`,
//! i.e. an 80-byte header `[src(16)][sig(64)][msgpack]` instead of the canonical
//! 96-byte `[dest(16)][src(16)][sig(64)][msgpack]`. Reticulum hands that
//! decrypted payload straight up; our parser previously assumed the msgpack
//! started at offset 96, failed, and leaked the raw wire into the message body
//! as a base64 blob.
//!
//! [`normalize_lxmf_wire`] reconstructs the canonical form by prepending the
//! local address when the destination hash is absent. These tests prove both
//! framings decode to the same body, that the tolerant msgpack decode accepts
//! foreign-client variance, and that nothing undecodable ever becomes a body.

use crate::node::{
    build_fields_msgpack, decode_lxmf_payload, encode_lxmf_msgpack,
    lxmf_event_from_bytes, normalize_lxmf_wire, LxmfEvent,
};

const DEST: [u8; 16] = [0xd0; 16];
const SRC: [u8; 16] = [0x50; 16];
const MY_ADDR: [u8; 16] = [0xaa; 16]; // our local LXMF address (distinct from DEST/SRC)

/// Canonical 96-byte-header wire as our own encoder produces it.
fn canonical_wire(body: &[u8]) -> Vec<u8> {
    let mp = encode_lxmf_msgpack(1_700_000_000.0, b"", body, &build_fields_msgpack(None));
    let mut w = Vec::with_capacity(96 + mp.len());
    w.extend_from_slice(&DEST);
    w.extend_from_slice(&SRC);
    w.extend_from_slice(&[0u8; 64]); // placeholder signature
    w.extend_from_slice(&mp);
    w
}

/// Opportunistic (dest-stripped) wire: `[src(16)][sig(64)][msgpack]`.
fn dest_stripped_wire(body: &[u8]) -> Vec<u8> {
    canonical_wire(body)[16..].to_vec()
}

// ── canonical form passes through unchanged ───────────────────────────────────

#[test]
fn canonical_wire_passes_through() {
    let w = canonical_wire(b"hello world");
    let norm = normalize_lxmf_wire(&w, &MY_ADDR).expect("canonical recognized");
    assert_eq!(norm, w, "canonical wire must be returned unchanged");
    let dec = decode_lxmf_payload(&norm).expect("decode");
    assert_eq!(dec.body, b"hello world");
}

// ── the actual bug: dest-stripped opportunistic framing ───────────────────────

#[test]
fn dest_stripped_wire_is_reconstructed() {
    let stripped = dest_stripped_wire(b"hello world");
    // Before the fix this 80-byte-header payload decoded as garbage.
    let norm = normalize_lxmf_wire(&stripped, &MY_ADDR).expect("stripped recognized");
    assert_eq!(norm.len(), stripped.len() + 16, "destination hash prepended");
    assert_eq!(&norm[0..16], &MY_ADDR, "our address used as destination");
    assert_eq!(&norm[16..32], &SRC, "sender address preserved");
    let dec = decode_lxmf_payload(&norm).expect("decode after reconstruction");
    assert_eq!(dec.body, b"hello world");
}

#[test]
fn dest_stripped_src_extraction_is_correct() {
    // The reply address (canonical[16..32]) must be the real sender, not sig bytes.
    let stripped = dest_stripped_wire(b"reply test");
    let norm = normalize_lxmf_wire(&stripped, &MY_ADDR).unwrap();
    assert_eq!(&norm[16..32], &SRC);
}

#[test]
fn dest_stripped_end_to_end_yields_message_event() {
    let stripped = dest_stripped_wire(b"end to end");
    let norm = normalize_lxmf_wire(&stripped, &MY_ADDR).unwrap();
    let mut src = [0u8; 16];
    src.copy_from_slice(&norm[16..32]);
    let ev = lxmf_event_from_bytes(src, norm, None).expect("decodable");
    match ev {
        LxmfEvent::MessageReceived { body, source, .. } => {
            assert_eq!(body, b"end to end");
            assert_eq!(source, SRC);
        }
        _ => panic!("expected MessageReceived"),
    }
}

// ── tolerant msgpack: foreign-client timestamp / array variance ───────────────

/// Build a wire with a hand-crafted msgpack payload (96-byte zero header + `mp`).
fn wire_with_mp(mp: &[u8]) -> Vec<u8> {
    let mut w = vec![0u8; 96];
    w.extend_from_slice(mp);
    w
}

fn bin8(b: &[u8]) -> Vec<u8> {
    let mut v = vec![0xc4, b.len() as u8];
    v.extend_from_slice(b);
    v
}

#[test]
fn integer_timestamp_decodes() {
    // [ uint32 ts, bin "", bin body, fixmap{} ]
    let mut mp = vec![0x94];
    mp.extend_from_slice(&[0xce, 0x65, 0x4b, 0x2c, 0x00]); // uint32 timestamp
    mp.extend_from_slice(&bin8(b""));
    mp.extend_from_slice(&bin8(b"int ts body"));
    mp.push(0x80); // empty fields map
    let dec = decode_lxmf_payload(&wire_with_mp(&mp)).expect("int timestamp tolerated");
    assert_eq!(dec.body, b"int ts body");
}

#[test]
fn float32_timestamp_decodes() {
    let mut mp = vec![0x94];
    mp.extend_from_slice(&[0xca, 0x4f, 0x00, 0x00, 0x00]); // float32 timestamp
    mp.extend_from_slice(&bin8(b""));
    mp.extend_from_slice(&bin8(b"f32 body"));
    mp.push(0x80);
    let dec = decode_lxmf_payload(&wire_with_mp(&mp)).expect("float32 timestamp tolerated");
    assert_eq!(dec.body, b"f32 body");
}

#[test]
fn positive_fixint_timestamp_decodes() {
    let mut mp = vec![0x94, 0x2a]; // ts = positive fixint 42
    mp.extend_from_slice(&bin8(b""));
    mp.extend_from_slice(&bin8(b"fixint body"));
    mp.push(0x80);
    let dec = decode_lxmf_payload(&wire_with_mp(&mp)).expect("fixint timestamp tolerated");
    assert_eq!(dec.body, b"fixint body");
}

#[test]
fn array_with_extra_trailing_elements_decodes() {
    // [ f64 ts, bin "", bin body, fixmap{}, nil ]  — array length 5, extra ignored
    let mut mp = vec![0x95, 0xcb];
    mp.extend_from_slice(&1_700_000_000.0f64.to_be_bytes());
    mp.extend_from_slice(&bin8(b""));
    mp.extend_from_slice(&bin8(b"five elem"));
    mp.push(0x80);
    mp.push(0xc0); // trailing nil (e.g. a stamp/ticket slot)
    let dec = decode_lxmf_payload(&wire_with_mp(&mp)).expect("array>4 tolerated");
    assert_eq!(dec.body, b"five elem");
}

#[test]
fn str_title_and_body_decode() {
    // title/content as fixstr instead of bin
    let mut mp = vec![0x94, 0xcb];
    mp.extend_from_slice(&1_700_000_000.0f64.to_be_bytes());
    mp.extend_from_slice(&[0xa5, b'T', b'i', b't', b'l', b'e']); // fixstr "Title"
    mp.extend_from_slice(&[0xa4, b'b', b'o', b'd', b'y']);       // fixstr "body"
    mp.push(0x80);
    let dec = decode_lxmf_payload(&wire_with_mp(&mp)).expect("str title/body tolerated");
    assert_eq!(dec.title, b"Title");
    assert_eq!(dec.body, b"body");
}

#[test]
fn array_too_short_is_rejected() {
    // Only 3 elements — not a valid LXMF payload.
    let mut mp = vec![0x93, 0xcb];
    mp.extend_from_slice(&1_700_000_000.0f64.to_be_bytes());
    mp.extend_from_slice(&bin8(b""));
    mp.extend_from_slice(&bin8(b"x"));
    assert!(decode_lxmf_payload(&wire_with_mp(&mp)).is_none(), "array<4 must reject");
}

// ── undecodable input never becomes a body ────────────────────────────────────

#[test]
fn blob_without_msgpack_marker_is_not_normalized() {
    // 135-byte high-entropy blob — the shape of the original bug report. The
    // bytes at the two candidate msgpack offsets (80, 96) are non-array markers,
    // so normalization correctly refuses to coerce it into an LXMF wire.
    //
    // NOTE: genuinely random ciphertext can occasionally present an array+numeric
    // marker at offset 80/96 and be mis-normalized; the real backstop is that
    // such a payload then fails `decode_lxmf_payload` and is dropped, never shown
    // as a body (see event_decode::high_entropy_blob_is_dropped_not_rendered).
    let mut blob: Vec<u8> = (0..135u16).map(|i| (i.wrapping_mul(167) as u8) ^ 0x3c).collect();
    blob[80] = 0x00; // positive fixint — not an array marker
    blob[96] = 0x00;
    assert!(normalize_lxmf_wire(&blob, &MY_ADDR).is_none(),
        "bytes with no msgpack array marker must not be coerced into an LXMF wire");
}

#[test]
fn too_short_for_either_offset_is_none() {
    assert!(normalize_lxmf_wire(&[0u8; 40], &MY_ADDR).is_none());
    assert!(normalize_lxmf_wire(&[], &MY_ADDR).is_none());
}
