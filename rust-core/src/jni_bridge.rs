//! JNI bridge for Android — maps Kotlin native method declarations to LxmfNode API

use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jint, jlong, jboolean, jshort, jstring};
use log::error;
use serde_json;

use crate::node::LxmfNode;
use crate::ble_iface;

const JNI_TRUE: jboolean = 1;
const JNI_FALSE: jboolean = 0;

fn throw_err(env: &mut JNIEnv, msg: &str) {
    error!("LxmfModule JNI: {}", msg);
    let _ = env.throw_new("java/lang/RuntimeException", msg);
}

// --- Lifecycle ---

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeInit(
    mut env: JNIEnv,
    _class: JClass,
    db_path: JString,
) -> jint {
    crate::log_bridge::init_logger(log::LevelFilter::Debug);

    let path: Option<String> = if db_path.is_null() {
        None
    } else {
        env.get_string(&db_path).ok().map(|s| s.into())
    };

    match LxmfNode::init(path.as_deref()) {
        Ok(()) => 0,
        Err(e) => {
            throw_err(&mut env, &format!("init failed: {}", e));
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeStart(
    mut env: JNIEnv,
    _class: JClass,
    identity_hex: JString,
    lxmf_address_hex: JString,
    mode: jint,
    announce_interval_ms: jlong,
    ble_mtu_hint: jshort,
    tcp_interfaces_json: JString,
    display_name: JString,
    is_beacon: jboolean,
) -> jint {
    let id_str: String = match env.get_string(&identity_hex) {
        Ok(s) => s.into(),
        Err(e) => { throw_err(&mut env, &format!("bad identity: {}", e)); return -1; }
    };
    let addr_str: String = match env.get_string(&lxmf_address_hex) {
        Ok(s) => s.into(),
        Err(e) => { throw_err(&mut env, &format!("bad address: {}", e)); return -1; }
    };

    let interfaces_json: String = if tcp_interfaces_json.is_null() {
        "[]".to_string()
    } else {
        env.get_string(&tcp_interfaces_json).ok().map(|s| s.into()).unwrap_or_else(|| "[]".to_string())
    };

    let display_name_str: String = if display_name.is_null() {
        String::new()
    } else {
        env.get_string(&display_name).ok().map(|s| s.into()).unwrap_or_default()
    };

    error!("LxmfModule: starting node mode={} interfaces={} name={} beacon={}", mode, interfaces_json, display_name_str, is_beacon != 0);

    match LxmfNode::start(
        &id_str,
        &addr_str,
        mode as u32,
        announce_interval_ms as u64,
        ble_mtu_hint as u16,
        &interfaces_json,
        &display_name_str,
        is_beacon != 0,
    ) {
        Ok(()) => {
            error!("LxmfModule: node started successfully");
            0
        }
        Err(e) => {
            throw_err(&mut env, &format!("start failed: {}", e));
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeStop(
    mut env: JNIEnv,
    _class: JClass,
) -> jint {
    match LxmfNode::stop() {
        Ok(()) => 0,
        Err(e) => { throw_err(&mut env, &format!("stop failed: {}", e)); -1 }
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeIsRunning(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    if LxmfNode::is_running() { JNI_TRUE } else { JNI_FALSE }
}

// --- Events ---

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativePollEvents(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let events = LxmfNode::drain_events();
    if events.is_empty() {
        return std::ptr::null_mut();
    }
    let json = crate::ffi::events_to_json_internal(&events);
    match env.new_string(&json) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// --- Identity ---

/// Return the full 128-char private identity hex for persistence,
/// or null if no node is initialized.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeGetIdentityHex(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    match LxmfNode::get_identity_hex() {
        Some(hex) => match env.new_string(&hex) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        },
        None => std::ptr::null_mut(),
    }
}

// --- Status ---

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeGetStatus(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    match LxmfNode::get_status_json() {
        Ok(json) => match env.new_string(&json) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        },
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeGetBeacons(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let guard = match LxmfNode::global().lock() {
        Ok(g) => g,
        Err(_) => return std::ptr::null_mut(),
    };
    let node = match guard.as_ref() {
        Some(n) => n,
        None => return std::ptr::null_mut(),
    };
    let json = node.beacon_mgr.beacons_json();
    match env.new_string(&json) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeFetchMessages(
    mut env: JNIEnv,
    _class: JClass,
    limit: jint,
) -> jstring {
    let guard = match LxmfNode::global().lock() {
        Ok(g) => g,
        Err(_) => return std::ptr::null_mut(),
    };
    let node = match guard.as_ref() {
        Some(n) => n,
        None => return std::ptr::null_mut(),
    };
    let json = match &node.store {
        Some(store) => store.fetch_messages(limit as u32).unwrap_or_else(|_| "[]".into()),
        None => "[]".into(),
    };
    match env.new_string(&json) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// --- Messaging ---

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeSend(
    mut env: JNIEnv,
    _class: JClass,
    dest_hex: JString,
    body_base64: JString,
    fields_json: JString,
) -> jlong {
    let dest: String = match env.get_string(&dest_hex) {
        Ok(s) => s.into(),
        Err(_) => { throw_err(&mut env, "nativeSend: invalid dest_hex string"); return -1; }
    };
    let body_b64: String = match env.get_string(&body_base64) {
        Ok(s) => s.into(),
        Err(_) => { throw_err(&mut env, "nativeSend: invalid body_base64 string"); return -1; }
    };
    let media_str: Option<String> = if fields_json.is_null() { None } else {
        env.get_string(&fields_json).ok().map(|s| s.into())
    };

    use base64::Engine as _;
    let data = match base64::engine::general_purpose::STANDARD.decode(&body_b64) {
        Ok(d) => d,
        Err(e) => {
            throw_err(&mut env, &format!("nativeSend: base64 decode failed: {e}"));
            return -1;
        }
    };

    match LxmfNode::send_to(&dest, &data, media_str.as_deref()) {
        Ok(seq) => seq as jlong,
        Err(e) => {
            error!("LxmfModule: send_to failed: {}", e);
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeBroadcast(
    mut env: JNIEnv,
    _class: JClass,
    dests_json: JString,
    body_base64: JString,
    fields_json: JString,
) -> jlong {
    let dests_str: String = match env.get_string(&dests_json) {
        Ok(s) => s.into(),
        Err(_) => { throw_err(&mut env, "nativeBroadcast: invalid dests_json string"); return -1; }
    };
    let body_b64: String = match env.get_string(&body_base64) {
        Ok(s) => s.into(),
        Err(_) => { throw_err(&mut env, "nativeBroadcast: invalid body_base64 string"); return -1; }
    };
    let media_str: Option<String> = if fields_json.is_null() { None } else {
        env.get_string(&fields_json).ok().map(|s| s.into())
    };

    use base64::Engine as _;
    let data = match base64::engine::general_purpose::STANDARD.decode(&body_b64) {
        Ok(d) => d,
        Err(e) => {
            throw_err(&mut env, &format!("nativeBroadcast: base64 decode failed: {e}"));
            return -1;
        }
    };

    let dests: Vec<String> = match serde_json::from_str(&dests_str) {
        Ok(v) => v,
        Err(e) => {
            throw_err(&mut env, &format!("nativeBroadcast: JSON parse failed: {e}"));
            return -1;
        }
    };

    let mut sent: i64 = 0;
    for dest in &dests {
        match LxmfNode::send_to(dest, &data, media_str.as_deref()) {
            Ok(_) => sent += 1,
            Err(e) => error!("LxmfModule: broadcast send_to {} failed: {}", dest, e),
        }
    }
    sent
}

// --- Config ---

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeSetLogLevel(
    _env: JNIEnv,
    _class: JClass,
    level: jint,
) -> jint {
    crate::log_bridge::set_max_level_from_u32(level as u32);
    0
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeAbiVersion(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    LxmfNode::abi_version() as jint
}

// --- BLE Interface ---
//
// These JNI methods are called by the Kotlin BleManager, which owns all BLE hardware access.
// They push bytes into / pull bytes from the static queues that BleInterface polls.

/// Helper: copies a JByteArray into a Rust Vec<u8>.
fn jbytes_to_vec(env: &mut JNIEnv, arr: &JByteArray) -> Result<Vec<u8>, jni::errors::Error> {
    let len = env.get_array_length(arr)? as usize;
    let mut buf = vec![0i8; len];
    env.get_byte_array_region(arr, 0, &mut buf)?;
    Ok(buf.iter().map(|&b| b as u8).collect())
}

/// Helper: copies the first 6 bytes of a JByteArray into a [u8; 6] MAC address.
fn jbytes_to_mac(env: &mut JNIEnv, arr: &JByteArray) -> Option<[u8; 6]> {
    let bytes = jbytes_to_vec(env, arr).ok()?;
    if bytes.len() < 6 { return None; }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&bytes[..6]);
    Some(mac)
}

/// Called by Kotlin BleManager when a BLE characteristic notification arrives.
///
/// `peer_addr` — 6-byte Bluetooth MAC of the sending device.
/// `data`       — raw bytes from the characteristic value (one BLE segment).
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeBleReceive(
    mut env: JNIEnv,
    _class: JClass,
    peer_addr: JByteArray,
    data: JByteArray,
) {
    let mac = match jbytes_to_mac(&mut env, &peer_addr) {
        Some(m) => m,
        None => { error!("nativeBleReceive: peer_addr must be 6 bytes"); return; }
    };
    let bytes = match jbytes_to_vec(&mut env, &data) {
        Ok(b) => b,
        Err(e) => { error!("nativeBleReceive: failed to read data: {}", e); return; }
    };
    ble_iface::on_ble_rx(mac, bytes);
}

/// Called by Kotlin BleManager to dequeue the next frame it should write to a peer characteristic.
///
/// Returns a byte array: `[6-byte peer MAC][payload...]`, or `null` when nothing is queued.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeBlePollTx(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    match ble_iface::next_ble_tx() {
        None => std::ptr::null_mut(),
        Some(frame) => {
            // Encode as JSON: {"peer": "aabbccddeeff", "data": "<base64>"}
            use base64::Engine as _;
            let peer_hex = hex::encode(frame.peer_addr);
            let data_b64 = base64::engine::general_purpose::STANDARD.encode(&frame.data);
            let json = format!(r#"{{"peer":"{}","data":"{}"}}"#, peer_hex, data_b64);
            match env.new_string(&json) {
                Ok(s) => s.into_raw(),
                Err(_) => std::ptr::null_mut(),
            }
        }
    }
}

/// Called by Kotlin BleManager when a GATT connection to a peer is established.
///
/// `peer_addr` — 6-byte Bluetooth MAC of the connected peer.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeBleConnected(
    mut env: JNIEnv,
    _class: JClass,
    peer_addr: JByteArray,
) {
    if let Some(mac) = jbytes_to_mac(&mut env, &peer_addr) {
        ble_iface::on_ble_connected(mac);
    }
}

/// Called by Kotlin BleManager when a GATT connection to a peer is lost.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeBleDisconnected(
    mut env: JNIEnv,
    _class: JClass,
    peer_addr: JByteArray,
) {
    if let Some(mac) = jbytes_to_mac(&mut env, &peer_addr) {
        ble_iface::on_ble_disconnected(mac);
    }
}

/// Returns the number of currently connected BLE peers.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeBlePeerCount(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    ble_iface::ble_peer_count() as jint
}

/// Called by Kotlin after ATT MTU negotiation completes for a peer.
/// `att_mtu` is the raw value from onMtuChanged (includes 3-byte ATT header).
/// Stores `att_mtu - 3` as the per-peer characteristic write limit.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeOnMtuNegotiated(
    mut env: JNIEnv,
    _class: JClass,
    peer_addr: JByteArray,
    att_mtu: jint,
) {
    if let Some(mac) = jbytes_to_mac(&mut env, &peer_addr) {
        let char_write_limit = (att_mtu as usize).saturating_sub(3).max(20);
        ble_iface::on_mtu_negotiated(mac, char_write_limit);
    }
}
