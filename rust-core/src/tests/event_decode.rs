use crate::node::{lxmf_event_from_bytes, encode_lxmf_msgpack, build_fields_msgpack, LxmfEvent};

fn addr(b: u8) -> [u8; 16] { [b; 16] }

fn wire_bytes(title: &[u8], body: &[u8], fields_mp: &[u8]) -> Vec<u8> {
    let mut w = vec![0u8; 96];
    w.extend_from_slice(&encode_lxmf_msgpack(1_700_000_000.0, title, body, fields_mp));
    w
}

// ── invalid / unparseable payloads are dropped (never emitted as raw body) ─────
//
// Regression guard: previously these fell back to `MessageReceived { body: data }`,
// leaking raw wire/ciphertext into the UI as a base64 blob ("🔒 couldn't decrypt").
// The fix drops undecodable payloads — lxmf_event_from_bytes returns None.

#[test]
fn too_short_is_dropped() {
    let ev = lxmf_event_from_bytes(addr(1), vec![0u8; 10], None);
    assert!(ev.is_none(), "short payload must drop, not emit raw body");
}

#[test]
fn garbage_bytes_are_dropped() {
    let data = vec![0xdeu8, 0xad, 0xbe, 0xef, 0x00, 0x11];
    let ev = lxmf_event_from_bytes(addr(2), data, None);
    assert!(ev.is_none(), "garbage must drop, not emit raw body");
}

#[test]
fn empty_vec_is_dropped() {
    let ev = lxmf_event_from_bytes(addr(3), vec![], None);
    assert!(ev.is_none());
}

/// A full-size high-entropy blob (e.g. an undecryptable 135-byte wire) must
/// never surface as a message body. This is the exact shape of the original bug.
#[test]
fn high_entropy_blob_is_dropped_not_rendered() {
    let blob: Vec<u8> = (0..135u16).map(|i| (i.wrapping_mul(167) ^ 0x5a) as u8).collect();
    let ev = lxmf_event_from_bytes(addr(0x7e), blob, None);
    assert!(ev.is_none(), "ciphertext-looking blob must drop, never become a body");
}

// ── well-formed LXMF payload decoded correctly ────────────────────────────────

#[test]
fn valid_payload_extracts_body() {
    let data = wire_bytes(b"", b"hello world", &[0x80]);
    let ev = lxmf_event_from_bytes(addr(4), data, None).expect("decodable");
    match ev {
        LxmfEvent::MessageReceived { body, .. } => assert_eq!(body, b"hello world"),
        _ => panic!("expected MessageReceived"),
    }
}

#[test]
fn valid_payload_extracts_title() {
    let data = wire_bytes(b"My Subject", b"body text", &[0x80]);
    let ev = lxmf_event_from_bytes(addr(5), data, None).expect("decodable");
    match ev {
        LxmfEvent::MessageReceived { title, body, .. } => {
            assert_eq!(title, b"My Subject");
            assert_eq!(body, b"body text");
        }
        _ => panic!("expected MessageReceived"),
    }
}

#[test]
fn valid_payload_source_address_preserved() {
    let src = addr(0xab);
    let data = wire_bytes(b"", b"msg", &[0x80]);
    let ev = lxmf_event_from_bytes(src, data, None).expect("decodable");
    match ev {
        LxmfEvent::MessageReceived { source, .. } => assert_eq!(source, src),
        _ => panic!("expected MessageReceived"),
    }
}

#[test]
fn valid_payload_no_media_yields_none_image_and_empty_files() {
    let data = wire_bytes(b"", b"text only", &build_fields_msgpack(None));
    let ev = lxmf_event_from_bytes(addr(6), data, None).expect("decodable");
    match ev {
        LxmfEvent::MessageReceived { image, files, .. } => {
            assert!(image.is_none());
            assert!(files.is_empty());
        }
        _ => panic!("expected MessageReceived"),
    }
}

#[test]
fn valid_payload_with_image_decoded() {
    use base64::Engine as _;
    let img = b"\xff\xd8\xff\xe0fake";
    let json = format!(
        r#"{{"image":{{"mimeType":"image/jpeg","data":"{}"}}}}"#,
        base64::engine::general_purpose::STANDARD.encode(img)
    );
    let data = wire_bytes(b"", b"caption", &build_fields_msgpack(Some(&json)));
    let ev = lxmf_event_from_bytes(addr(7), data, None).expect("decodable");
    match ev {
        LxmfEvent::MessageReceived { image, body, .. } => {
            let (mime, bytes) = image.expect("image present");
            assert_eq!(mime, "image/jpeg");
            assert_eq!(bytes, img);
            assert_eq!(body, b"caption");
        }
        _ => panic!("expected MessageReceived"),
    }
}

#[test]
fn valid_payload_with_files_decoded() {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let json = format!(
        r#"{{"files":[{{"name":"doc.pdf","data":"{}"}},{{"name":"img.png","data":"{}"}}]}}"#,
        b64.encode(b"pdf bytes"),
        b64.encode(b"png bytes"),
    );
    let data = wire_bytes(b"", b"", &build_fields_msgpack(Some(&json)));
    let ev = lxmf_event_from_bytes(addr(8), data, None).expect("decodable");
    match ev {
        LxmfEvent::MessageReceived { files, .. } => {
            assert_eq!(files.len(), 2);
            assert_eq!(files[0].0, "doc.pdf");
            assert_eq!(files[0].1, b"pdf bytes");
            assert_eq!(files[1].0, "img.png");
        }
        _ => panic!("expected MessageReceived"),
    }
}

// ── timestamp is set (non-zero unix epoch for post-2020 systems) ──────────────

#[test]
fn timestamp_is_reasonable() {
    let data = wire_bytes(b"", b"body", &[0x80]);
    let ev = lxmf_event_from_bytes(addr(9), data, None).expect("decodable");
    match ev {
        LxmfEvent::MessageReceived { timestamp, .. } => {
            assert!(timestamp > 1_600_000_000, "timestamp should be post-2020: {timestamp}");
        }
        _ => panic!("expected MessageReceived"),
    }
}
