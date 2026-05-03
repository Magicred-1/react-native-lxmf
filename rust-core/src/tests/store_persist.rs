use crate::node::{LxmfEvent, persist_inbound_message};
use crate::store::MessageStore;
use std::sync::Arc;

fn open_mem() -> MessageStore {
    MessageStore::open(":memory:").expect("in-memory SQLite")
}

fn src(b: u8) -> [u8; 16] { [b; 16] }
fn dst(b: u8) -> [u8; 16] { [b; 16] }

// ── inbound persistence ───────────────────────────────────────────────────────

#[test]
fn inbound_basic_roundtrip() {
    let store = open_mem();
    let source = src(0xaa);
    let dest   = dst(0x00);
    store.insert_inbound_message(&source, &dest, b"hello", b"world", None, &[], 1_700_000_000)
        .expect("insert");
    let json = store.fetch_messages(10).expect("fetch");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    // body is base64 of b"world"
    let body = row["body"].as_str().unwrap();
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body).unwrap();
    assert_eq!(decoded, b"world");
    assert!(!row["outbound"].as_bool().unwrap());
}

#[test]
fn inbound_source_hash_preserved_as_hex() {
    let store = open_mem();
    let source: [u8; 16] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
    ];
    store.insert_inbound_message(&source, &dst(0), b"", b"x", None, &[], 0).expect("insert");
    let json = store.fetch_messages(10).expect("fetch");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    let src_hex = rows[0]["source"].as_str().unwrap().to_lowercase();
    assert_eq!(src_hex, "0102030405060708090a0b0c0d0e0f10");
}

#[test]
fn inbound_with_image_stored_and_retrieved() {
    let store = open_mem();
    let img_data = b"\xff\xd8\xff\xe0fake_jpeg";
    store.insert_inbound_message(
        &src(1), &dst(0), b"", b"see pic",
        Some(("image/jpeg", img_data)), &[], 0,
    ).expect("insert");
    let json = store.fetch_messages(10).expect("fetch");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    let img = rows[0].get("image").expect("image field present");
    assert_eq!(img["mimeType"].as_str().unwrap(), "image/jpeg");
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        img["data"].as_str().unwrap(),
    ).unwrap();
    assert_eq!(decoded, img_data);
}

#[test]
fn inbound_with_files_stored_and_retrieved() {
    let store = open_mem();
    let files: Vec<(String, Vec<u8>)> = vec![
        ("doc.pdf".into(), b"pdf bytes".to_vec()),
        ("img.png".into(), b"png bytes".to_vec()),
    ];
    store.insert_inbound_message(&src(2), &dst(0), b"", b"files", None, &files, 0)
        .expect("insert");
    let json = store.fetch_messages(10).expect("fetch");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    let file_arr = rows[0]["files"].as_array().expect("files array");
    assert_eq!(file_arr.len(), 2);
    assert_eq!(file_arr[0]["name"].as_str().unwrap(), "doc.pdf");
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        file_arr[0]["data"].as_str().unwrap(),
    ).unwrap();
    assert_eq!(decoded, b"pdf bytes");
}

#[test]
fn inbound_title_stored_as_base64() {
    let store = open_mem();
    store.insert_inbound_message(&src(3), &dst(0), b"My Title", b"body", None, &[], 0)
        .expect("insert");
    let json = store.fetch_messages(10).expect("fetch");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    let title_b64 = rows[0]["title"].as_str().unwrap();
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, title_b64).unwrap();
    assert_eq!(decoded, b"My Title");
}

#[test]
fn persist_inbound_message_helper_inserts_message_received_event() {
    let store = Arc::new(open_mem());
    let src_addr = src(0xbb);
    let event = LxmfEvent::MessageReceived {
        source: src_addr,
        title: b"title".to_vec(),
        body: b"body text".to_vec(),
        image: None,
        files: vec![],
        timestamp: 1_700_000_000,
        group_dest: None,
    };
    persist_inbound_message(&Some(store.clone()), &event);
    let json = store.fetch_messages(10).expect("fetch");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.len(), 1);
    let body = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        rows[0]["body"].as_str().unwrap(),
    ).unwrap();
    assert_eq!(body, b"body text");
}

#[test]
fn persist_inbound_message_noop_on_none_store() {
    let event = LxmfEvent::MessageReceived {
        source: src(1),
        title: vec![], body: vec![], image: None, files: vec![], timestamp: 0, group_dest: None,
    };
    // must not panic
    persist_inbound_message(&None, &event);
}

#[test]
fn persist_inbound_message_noop_on_non_message_event() {
    let store = Arc::new(open_mem());
    let event = LxmfEvent::StatusChanged { running: true, lifecycle: 3 };
    persist_inbound_message(&Some(store.clone()), &event);
    let json = store.fetch_messages(10).expect("fetch");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert!(rows.is_empty());
}

// ── outbound queue ────────────────────────────────────────────────────────────

#[test]
fn outbound_queue_enqueue_and_list() {
    let store = open_mem();
    let dest: [u8; 16] = [0xde; 16];
    let payload = b"lxmf payload bytes";
    store.enqueue_outbound(42, &dest, payload).expect("enqueue");
    let queue = store.all_outbound_queue().expect("list");
    assert_eq!(queue.len(), 1);
    let (_, seq, queued_dest, queued_payload) = &queue[0];
    assert_eq!(*seq, 42u64);
    assert_eq!(*queued_dest, dest);
    assert_eq!(queued_payload, payload);
}

#[test]
fn outbound_queue_remove_clears_entry() {
    let store = open_mem();
    let dest: [u8; 16] = [0x11; 16];
    let id = store.enqueue_outbound(1, &dest, b"payload").expect("enqueue");
    store.remove_outbound(id).expect("remove");
    let queue = store.all_outbound_queue().expect("list");
    assert!(queue.is_empty());
}

#[test]
fn outbound_queue_bump_attempts_and_drain_expired() {
    let store = open_mem();
    let dest: [u8; 16] = [0x22; 16];
    let id = store.enqueue_outbound(7, &dest, b"x").expect("enqueue");
    for _ in 0..50 {
        store.bump_outbound_attempts(id).expect("bump");
    }
    // all_outbound_queue filters out entries with attempts >= 50
    let active = store.all_outbound_queue().expect("active");
    assert!(active.is_empty());
    // drain_expired_outbound returns and deletes them
    let expired = store.drain_expired_outbound(50).expect("drain");
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].1, 7u64);
}

#[test]
fn outbound_queue_multiple_dests_independent() {
    let store = open_mem();
    let d1: [u8; 16] = [0x01; 16];
    let d2: [u8; 16] = [0x02; 16];
    store.enqueue_outbound(10, &d1, b"msg for d1").expect("enqueue 1");
    store.enqueue_outbound(11, &d2, b"msg for d2").expect("enqueue 2");
    let queue = store.all_outbound_queue().expect("list");
    assert_eq!(queue.len(), 2);
    let seqs: Vec<u64> = queue.iter().map(|(_, s, _, _)| *s).collect();
    assert!(seqs.contains(&10));
    assert!(seqs.contains(&11));
}

#[test]
fn fetch_messages_limit_respected() {
    let store = open_mem();
    for i in 0u8..10 {
        store.insert_inbound_message(&src(i), &dst(0), b"", b"x", None, &[], 0).expect("insert");
    }
    let json = store.fetch_messages(3).expect("fetch");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.len(), 3);
}
