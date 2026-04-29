//! C FFI exports — called by iOS (Swift) and directly by Android when JNI is not preferred
//!
//! All functions are `#[no_mangle] extern "C"` with pointer+length patterns.
//! The native layer (Swift/Kotlin) calls these, and they delegate to LxmfNode.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::slice;

use log::warn;
use crate::node::{LxmfNode, DestHash};

pub const STATUS_OK: i32 = 0;
pub const STATUS_ERR: i32 = -1;
pub const STATUS_NOT_INIT: i32 = -2;

// --- Lifecycle ---

#[no_mangle]
pub unsafe extern "C" fn lxmf_init(db_path: *const c_char) -> i32 {
    let path = if db_path.is_null() {
        None
    } else {
        CStr::from_ptr(db_path).to_str().ok()
    };

    match LxmfNode::init(path) {
        Ok(()) => STATUS_OK,
        Err(_) => STATUS_ERR,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lxmf_start(
    identity_hex: *const c_char,
    address_hex: *const c_char,
    mode: u32,
    announce_interval_ms: u64,
    ble_mtu_hint: u16,
    tcp_interfaces_json: *const c_char,
    display_name: *const c_char,
    is_beacon: u8,
) -> i32 {
    let id = if identity_hex.is_null() { "" } else {
        match CStr::from_ptr(identity_hex).to_str() { Ok(s) => s, Err(_) => return STATUS_ERR }
    };
    let addr = if address_hex.is_null() { "" } else {
        match CStr::from_ptr(address_hex).to_str() { Ok(s) => s, Err(_) => return STATUS_ERR }
    };
    let interfaces = if tcp_interfaces_json.is_null() { "[]" } else {
        match CStr::from_ptr(tcp_interfaces_json).to_str() { Ok(s) => s, Err(_) => return STATUS_ERR }
    };
    let name = if display_name.is_null() { "" } else {
        match CStr::from_ptr(display_name).to_str() { Ok(s) => s, Err(_) => return STATUS_ERR }
    };

    match LxmfNode::start(id, addr, mode, announce_interval_ms, ble_mtu_hint, interfaces, name, is_beacon != 0) {
        Ok(()) => STATUS_OK,
        Err(_) => STATUS_ERR,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lxmf_stop() -> i32 {
    match LxmfNode::stop() {
        Ok(()) => STATUS_OK,
        Err(_) => STATUS_ERR,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lxmf_is_running() -> i32 {
    if LxmfNode::is_running() { 1 } else { 0 }
}

// --- Identity ---

/// Write the full 128-char private identity hex into `out_buf` for persistence.
///
/// Returns the number of bytes written (always 128 on success), 0 if no node is
/// initialized, or a negative error code. The identity hex contains the private
/// key — callers must persist it to encrypted/secure storage.
#[no_mangle]
pub unsafe extern "C" fn lxmf_get_identity_hex(out_buf: *mut u8, out_capacity: usize) -> i32 {
    if out_buf.is_null() { return STATUS_ERR; }
    let hex = match LxmfNode::get_identity_hex() {
        Some(s) => s,
        None => return 0,
    };
    let bytes = hex.as_bytes();
    if bytes.len() > out_capacity { return STATUS_ERR; }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
    bytes.len() as i32
}

// --- Status ---

#[no_mangle]
pub unsafe extern "C" fn lxmf_get_status(out_buf: *mut u8, out_capacity: usize) -> i32 {
    let json = match LxmfNode::get_status_json() {
        Ok(s) => s,
        Err(_) => return STATUS_ERR,
    };
    let bytes = json.as_bytes();
    if bytes.len() > out_capacity { return STATUS_ERR; }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
    bytes.len() as i32
}

// --- Events ---

#[no_mangle]
pub unsafe extern "C" fn lxmf_poll_events(
    _timeout_ms: u64,
    out_buf: *mut u8,
    out_capacity: usize,
) -> i32 {
    let events = LxmfNode::drain_events();
    if events.is_empty() { return 0; }

    let json = events_to_json(&events);
    let bytes = json.as_bytes();
    if bytes.len() > out_capacity { return STATUS_ERR; }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
    bytes.len() as i32
}

// --- Beacons ---

#[no_mangle]
pub unsafe extern "C" fn lxmf_get_beacons(out_buf: *mut u8, out_capacity: usize) -> i32 {
    let guard = match LxmfNode::global().lock() {
        Ok(g) => g,
        Err(_) => return STATUS_ERR,
    };
    let node = match guard.as_ref() {
        Some(n) => n,
        None => return STATUS_NOT_INIT,
    };

    let json = node.beacon_mgr.beacons_json();
    let bytes = json.as_bytes();
    if bytes.len() > out_capacity { return STATUS_ERR; }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
    bytes.len() as i32
}

#[no_mangle]
pub unsafe extern "C" fn lxmf_on_announce(
    dest_hash_ptr: *const u8,
    app_data_ptr: *const u8,
    app_data_len: usize,
) -> i32 {
    if dest_hash_ptr.is_null() || app_data_ptr.is_null() { return STATUS_ERR; }

    let mut dest_hash: DestHash = [0u8; 16];
    dest_hash.copy_from_slice(slice::from_raw_parts(dest_hash_ptr, 16));
    let app_data = slice::from_raw_parts(app_data_ptr, app_data_len);

    let mut guard = match LxmfNode::global().lock() {
        Ok(g) => g,
        Err(_) => return STATUS_ERR,
    };
    let node = match guard.as_mut() {
        Some(n) => n,
        None => return STATUS_NOT_INIT,
    };

    node.beacon_mgr.on_announce_received(dest_hash, app_data);
    STATUS_OK
}

// --- Messages ---

#[no_mangle]
pub unsafe extern "C" fn lxmf_fetch_messages(
    limit: u32,
    out_buf: *mut u8,
    out_capacity: usize,
) -> i32 {
    let guard = match LxmfNode::global().lock() {
        Ok(g) => g,
        Err(_) => return STATUS_ERR,
    };
    let node = match guard.as_ref() {
        Some(n) => n,
        None => return STATUS_NOT_INIT,
    };

    let json = match &node.store {
        Some(store) => store.fetch_messages(limit).unwrap_or_else(|_| "[]".into()),
        None => "[]".into(),
    };
    let bytes = json.as_bytes();
    if bytes.len() > out_capacity { return STATUS_ERR; }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
    bytes.len() as i32
}

// --- Messaging ---

/// Send an LXMF message to a single destination.
///
/// `dest_ptr` — pointer to 16-byte destination hash.
/// `body_ptr` — pointer to message body bytes.
/// `body_len` — length of body.
///
/// Returns 0 on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn lxmf_send(
    dest_ptr: *const u8,
    body_ptr: *const u8,
    body_len: usize,
) -> i64 {
    if dest_ptr.is_null() || body_ptr.is_null() { return -1; }
    if body_len > 65536 { return -1; } // sanity cap

    let dest_bytes = slice::from_raw_parts(dest_ptr, 16);
    let dest_hex = hex::encode(dest_bytes);
    let body = slice::from_raw_parts(body_ptr, body_len);

    match LxmfNode::send_to(&dest_hex, body) {
        Ok(seq) => seq as i64,
        Err(e) => {
            warn!("lxmf_send failed: {}", e);
            -1
        }
    }
}

/// Broadcast an LXMF message to multiple destinations.
///
/// `dests_ptr`  — pointer to flat array of 16-byte destination hashes.
/// `dest_count` — number of destinations (each 16 bytes).
/// `body_ptr`   — pointer to message body bytes.
/// `body_len`   — length of body.
///
/// Returns number of successful sends, or -1 on invalid input.
#[no_mangle]
pub unsafe extern "C" fn lxmf_broadcast(
    dests_ptr: *const u8,
    dest_count: usize,
    body_ptr: *const u8,
    body_len: usize,
) -> i64 {
    if dests_ptr.is_null() || body_ptr.is_null() { return -1; }
    if dest_count == 0 { return 0; }
    if body_len > 65536 { return -1; }

    let dests = slice::from_raw_parts(dests_ptr, dest_count * 16);
    let body = slice::from_raw_parts(body_ptr, body_len);

    let mut sent: i64 = 0;
    for i in 0..dest_count {
        let dest_hex = hex::encode(&dests[i * 16..(i + 1) * 16]);
        if LxmfNode::send_to(&dest_hex, body).is_ok() {
            sent += 1;
        }
    }
    sent
}

// --- Config ---

#[no_mangle]
pub unsafe extern "C" fn lxmf_set_log_level(level: u32) -> i32 {
    crate::log_bridge::set_max_level_from_u32(level);
    STATUS_OK
}

#[no_mangle]
pub unsafe extern "C" fn lxmf_abi_version() -> u32 { LxmfNode::abi_version() }

// --- BLE Interface (iOS C FFI) ---
//
// These mirror the JNI BLE functions in jni_bridge.rs.
// Swift calls these via @_silgen_name from BLEManager / LxmfModule.

/// Push inbound BLE data from a peer into the Rust transport engine.
///
/// `peer_addr` — pointer to 6-byte peer address (pseudo-MAC derived from CoreBluetooth UUID).
/// `data`      — pointer to raw BLE characteristic bytes.
/// `data_len`  — number of bytes.
#[no_mangle]
pub unsafe extern "C" fn lxmf_ble_receive(
    peer_addr: *const u8,
    data: *const u8,
    data_len: usize,
) -> i32 {
    if peer_addr.is_null() || data.is_null() { return STATUS_ERR; }
    if data_len == 0 || data_len > 4096 { return STATUS_ERR; }
    let mut addr = [0u8; 6];
    addr.copy_from_slice(slice::from_raw_parts(peer_addr, 6));
    let bytes = slice::from_raw_parts(data, data_len).to_vec();
    crate::ble_iface::on_ble_rx(addr, bytes);
    STATUS_OK
}

/// Dequeue the next outbound BLE frame from the Rust transport engine.
///
/// `out_peer`     — pointer to a 6-byte buffer; receives the target peer address.
/// `out_data`     — pointer to a data buffer; receives the frame bytes.
/// `out_capacity` — size of the data buffer.
///
/// Returns: positive = number of data bytes written, 0 = nothing queued, negative = error.
#[no_mangle]
pub unsafe extern "C" fn lxmf_ble_poll_tx(
    out_peer: *mut u8,
    out_data: *mut u8,
    out_capacity: usize,
) -> i32 {
    if out_peer.is_null() || out_data.is_null() { return STATUS_ERR; }
    match crate::ble_iface::next_ble_tx() {
        Some(frame) => {
            if frame.data.len() > out_capacity { return STATUS_ERR; }
            std::ptr::copy_nonoverlapping(frame.peer_addr.as_ptr(), out_peer, 6);
            std::ptr::copy_nonoverlapping(frame.data.as_ptr(), out_data, frame.data.len());
            frame.data.len() as i32
        }
        None => 0,
    }
}

/// Notify Rust that a BLE peer has connected.
///
/// `peer_addr` — pointer to 6-byte peer address.
#[no_mangle]
pub unsafe extern "C" fn lxmf_ble_connected(peer_addr: *const u8) -> i32 {
    if peer_addr.is_null() { return STATUS_ERR; }
    let mut addr = [0u8; 6];
    addr.copy_from_slice(slice::from_raw_parts(peer_addr, 6));
    crate::ble_iface::on_ble_connected(addr);
    STATUS_OK
}

/// Notify Rust that a BLE peer has disconnected.
///
/// `peer_addr` — pointer to 6-byte peer address.
#[no_mangle]
pub unsafe extern "C" fn lxmf_ble_disconnected(peer_addr: *const u8) -> i32 {
    if peer_addr.is_null() { return STATUS_ERR; }
    let mut addr = [0u8; 6];
    addr.copy_from_slice(slice::from_raw_parts(peer_addr, 6));
    crate::ble_iface::on_ble_disconnected(addr);
    STATUS_OK
}

/// Returns the number of currently connected BLE peers.
#[no_mangle]
pub extern "C" fn lxmf_ble_peer_count() -> i32 {
    crate::ble_iface::ble_peer_count() as i32
}

/// Notify Rust of the negotiated BLE write limit for a peer.
///
/// `peer_addr`   — pointer to 6-byte peer address.
/// `write_limit` — maximum characteristic write payload in bytes (iOS:
///                 `maximumWriteValueLength(for: .withoutResponse)` or
///                 `central.maximumUpdateValueLength`; Android: ATT MTU − 3).
#[no_mangle]
pub unsafe extern "C" fn lxmf_ble_mtu_negotiated(peer_addr: *const u8, write_limit: u32) -> i32 {
    if peer_addr.is_null() { return STATUS_ERR; }
    let mut addr = [0u8; 6];
    addr.copy_from_slice(slice::from_raw_parts(peer_addr, 6));
    crate::ble_iface::on_mtu_negotiated(addr, write_limit as usize);
    STATUS_OK
}

// --- NUS Interface (RNode BLE via Nordic UART Service) ---
//
// Swift calls these for RNode connectivity. Data is KISS-framed
// on the Rust side — Swift just passes raw NUS characteristic bytes.

/// Push raw bytes received from the RNode's NUS RX characteristic.
///
/// `data`     — pointer to raw bytes from NUS notification.
/// `data_len` — number of bytes.
///
/// The bytes may contain partial KISS frames — the Rust-side KissDeframer
/// handles accumulation across multiple notifications.
#[no_mangle]
pub unsafe extern "C" fn lxmf_nus_receive(
    data: *const u8,
    data_len: usize,
) -> i32 {
    if data.is_null() { return STATUS_ERR; }
    if data_len == 0 || data_len > 4096 { return STATUS_ERR; }
    let bytes = slice::from_raw_parts(data, data_len).to_vec();
    crate::nus_iface::on_nus_rx(bytes);
    STATUS_OK
}

/// Dequeue the next KISS-framed bytes to write to the RNode's NUS TX char.
///
/// `out_data`     — pointer to output buffer.
/// `out_capacity` — size of output buffer.
///
/// Returns: positive = bytes written, 0 = nothing queued, negative = error.
#[no_mangle]
pub unsafe extern "C" fn lxmf_nus_poll_tx(
    out_data: *mut u8,
    out_capacity: usize,
) -> i32 {
    if out_data.is_null() { return STATUS_ERR; }
    match crate::nus_iface::next_nus_tx() {
        Some(frame) => {
            if frame.len() > out_capacity { return STATUS_ERR; }
            std::ptr::copy_nonoverlapping(frame.as_ptr(), out_data, frame.len());
            frame.len() as i32
        }
        None => 0,
    }
}

// --- Internal ---

pub fn events_to_json_internal(events: &[crate::node::LxmfEvent]) -> String {
    events_to_json(events)
}

fn events_to_json(events: &[crate::node::LxmfEvent]) -> String {
    use crate::node::LxmfEvent;

    let arr: Vec<serde_json::Value> = events.iter().map(|e| match e {
        LxmfEvent::StatusChanged { running, lifecycle } => serde_json::json!({
            "type": "statusChanged", "running": running, "lifecycle": lifecycle,
        }),
        LxmfEvent::PacketReceived { source, data } => serde_json::json!({
            "type": "packetReceived", "source": hex::encode(source), "data": hex::encode(data),
        }),
        LxmfEvent::TxReceived { data } => serde_json::json!({
            "type": "txReceived", "data": hex::encode(data),
        }),
        LxmfEvent::BeaconDiscovered { dest_hash, app_data } => serde_json::json!({
            "type": "beaconDiscovered", "destHash": hex::encode(dest_hash),
            "appData": String::from_utf8_lossy(app_data).to_string(),
        }),
        LxmfEvent::MessageReceived { source, content, timestamp } => serde_json::json!({
            "type": "messageReceived", "source": hex::encode(source),
            "content": hex::encode(content), "timestamp": timestamp,
        }),
        LxmfEvent::AnnounceReceived { dest_hash, app_data, hops } => serde_json::json!({
            "type": "announceReceived", "destHash": hex::encode(dest_hash),
            "appData": String::from_utf8_lossy(app_data).to_string(), "hops": hops,
        }),
        LxmfEvent::Log { level, message } => serde_json::json!({
            "type": "log", "level": level, "message": message,
        }),
        LxmfEvent::Error { code, message } => serde_json::json!({
            "type": "error", "code": code, "message": message,
        }),
    }).collect();

    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into())
}
