//! JNI bridge for Android — maps Kotlin native method declarations to LxmfNode API

use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jint, jlong, jboolean, jshort, jstring, jbyteArray};
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

    log::info!("LxmfModule: starting node mode={} interfaces={} name={} beacon={}", mode, interfaces_json, display_name_str, is_beacon != 0);

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
            log::info!("LxmfModule: node started successfully");
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
    env: JNIEnv,
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
    env: JNIEnv,
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
    env: JNIEnv,
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
    env: JNIEnv,
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
    let json = node.beacon_mgr.lock().map(|m| m.beacons_json()).unwrap_or_else(|_| "[]".to_string());
    match env.new_string(&json) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeFetchMessages(
    env: JNIEnv,
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

// --- Group Chat ---

/// Create or join a group by name + 16-byte key hex. Returns group address hex string.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeCreateGroup(
    mut env: JNIEnv,
    _class: JClass,
    name: JString,
    key_hex: JString,
) -> jstring {
    let name_str: String = match env.get_string(&name) {
        Ok(s) => s.into(),
        Err(_) => return std::ptr::null_mut(),
    };
    let key_str: String = match env.get_string(&key_hex) {
        Ok(s) => s.into(),
        Err(_) => return std::ptr::null_mut(),
    };
    match LxmfNode::create_group(&name_str, &key_str) {
        Ok(addr_hex) => match env.new_string(&addr_hex) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        },
        Err(e) => { error!("nativeCreateGroup: {e}"); std::ptr::null_mut() }
    }
}

/// Join a group by pre-known address hex + key hex.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeJoinGroup(
    mut env: JNIEnv,
    _class: JClass,
    addr_hex: JString,
    key_hex: JString,
) -> jint {
    let addr_str: String = match env.get_string(&addr_hex) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    let key_str: String = match env.get_string(&key_hex) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    match LxmfNode::join_group(&addr_str, &key_str) {
        Ok(()) => 0,
        Err(e) => { error!("nativeJoinGroup: {e}"); -1 }
    }
}

/// Leave a group.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeLeaveGroup(
    mut env: JNIEnv,
    _class: JClass,
    addr_hex: JString,
) -> jint {
    let addr_str: String = match env.get_string(&addr_hex) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    match LxmfNode::leave_group(&addr_str) {
        Ok(()) => 0,
        Err(e) => { error!("nativeLeaveGroup: {e}"); -1 }
    }
}

/// Send a message to a group channel. Returns seq number or -1 on error.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeSendGroup(
    mut env: JNIEnv,
    _class: JClass,
    addr_hex: JString,
    body_b64: JString,
    fields_json: JString,
) -> jlong {
    let addr_str: String = match env.get_string(&addr_hex) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    let body_b64_str: String = match env.get_string(&body_b64) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    use base64::Engine as _;
    let body = match base64::engine::general_purpose::STANDARD.decode(&body_b64_str) {
        Ok(b) => b,
        Err(e) => { error!("nativeSendGroup: base64 decode failed: {e}"); return -1; }
    };
    let media_str: Option<String> = env.get_string(&fields_json).ok().map(|s| s.into());
    match LxmfNode::send_group(&addr_str, &body, media_str.as_deref()) {
        Ok(seq) => seq as jlong,
        Err(e) => { error!("nativeSendGroup: {e}"); -1 }
    }
}

// --- Beacon RPC ---

/// Queue a JSON-RPC 2.0 call to a specific beacon via its dest hash hex.
///
/// `dest_hash_hex` — 32-char hex of the 16-byte beacon dest hash.
/// `method`        — JSON-RPC method name (e.g. "getLatestBlockhash").
/// `params_json`   — JSON-encoded params array (e.g. `[{"commitment":"confirmed"}]`).
///
/// Returns the correlation id (>= 1) which will appear in `LxmfEvent::RpcResponse`.
/// Returns -1 on error (bad dest, unparseable params, not initialized).
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeBeaconRpc(
    mut env: JNIEnv,
    _class: JClass,
    dest_hash_hex: JString,
    method: JString,
    params_json: JString,
) -> jlong {
    let dest_str: String = match env.get_string(&dest_hash_hex) {
        Ok(s) => s.into(),
        Err(_) => { throw_err(&mut env, "nativeBeaconRpc: invalid dest_hash_hex"); return -1; }
    };
    let method_str: String = match env.get_string(&method) {
        Ok(s) => s.into(),
        Err(_) => { throw_err(&mut env, "nativeBeaconRpc: invalid method"); return -1; }
    };
    let params_str: String = if params_json.is_null() {
        "[]".to_string()
    } else {
        env.get_string(&params_json).ok().map(|s| s.into()).unwrap_or_else(|| "[]".to_string())
    };

    let dest_bytes = match hex::decode(&dest_str) {
        Ok(b) if b.len() == 16 => b,
        _ => { throw_err(&mut env, "nativeBeaconRpc: dest_hash_hex must be 32 hex chars"); return -1; }
    };
    let mut dest: crate::node::DestHash = [0u8; 16];
    dest.copy_from_slice(&dest_bytes);

    let params: serde_json::Value = match serde_json::from_str(&params_str) {
        Ok(v) => v,
        Err(e) => {
            throw_err(&mut env, &format!("nativeBeaconRpc: params_json parse failed: {e}"));
            return -1;
        }
    };

    let guard = match LxmfNode::global().lock() {
        Ok(g) => g,
        Err(_) => return -1,
    };
    let node = match guard.as_ref() {
        Some(n) => n,
        None => return -1,
    };
    let rpc_id = match node.beacon_mgr.lock() {
        Ok(mut mgr) => mgr.queue_rpc(dest, &method_str, params) as jlong,
        Err(_) => -1,
    };
    if rpc_id >= 0 {
        node.rpc_notify.notify_one();
    }
    rpc_id
}

// --- Solana tx building ---

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativePartialSignExecutePayment(
    mut env: JNIEnv,
    _class: JClass,
    payer_key:     JByteArray,
    nonce_bh:      JByteArray,
    accounts_json: JString,
    params_json:   JString,
) -> jstring {
    use zeroize::Zeroize;
    let mut payer_bytes = match jbytes_to_vec(&mut env, &payer_key) {
        Ok(b) => b, Err(_) => return std::ptr::null_mut(),
    };
    if payer_bytes.len() != 32 { payer_bytes.zeroize(); return std::ptr::null_mut(); }

    let nonce_bytes = match jbytes_to_vec(&mut env, &nonce_bh) {
        Ok(b) => b, Err(_) => { payer_bytes.zeroize(); return std::ptr::null_mut(); }
    };
    if nonce_bytes.len() != 32 { payer_bytes.zeroize(); return std::ptr::null_mut(); }

    let accts_str: String = match env.get_string(&accounts_json) {
        Ok(s) => s.into(), Err(_) => { payer_bytes.zeroize(); return std::ptr::null_mut(); }
    };
    let params_str: String = match env.get_string(&params_json) {
        Ok(s) => s.into(), Err(_) => { payer_bytes.zeroize(); return std::ptr::null_mut(); }
    };

    let accts_c = match std::ffi::CString::new(accts_str) {
        Ok(s) => s, Err(_) => { payer_bytes.zeroize(); return std::ptr::null_mut(); }
    };
    let params_c = match std::ffi::CString::new(params_str) {
        Ok(s) => s, Err(_) => { payer_bytes.zeroize(); return std::ptr::null_mut(); }
    };

    let mut out = [0u8; 1024];
    let written = unsafe {
        crate::ffi::lxmf_partial_sign_execute_payment(
            payer_bytes.as_ptr(), nonce_bytes.as_ptr(),
            accts_c.as_ptr(), params_c.as_ptr(),
            out.as_mut_ptr(), out.len(),
        )
    };
    payer_bytes.zeroize();
    if written < 0 { return std::ptr::null_mut(); }

    let s = match std::str::from_utf8(&out[..written as usize]) {
        Ok(s) => s, Err(_) => return std::ptr::null_mut(),
    };
    match env.new_string(s) {
        Ok(js) => js.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeSignTx(
    mut env: JNIEnv,
    _class: JClass,
    payer_key: JByteArray,
    tx_b64: JString,
) -> jstring {
    use zeroize::Zeroize;
    let mut payer_bytes = match jbytes_to_vec(&mut env, &payer_key) {
        Ok(b) => b, Err(_) => return std::ptr::null_mut(),
    };
    if payer_bytes.len() != 32 { payer_bytes.zeroize(); return std::ptr::null_mut(); }

    let b64_str: String = match env.get_string(&tx_b64) {
        Ok(s) => s.into(), Err(_) => { payer_bytes.zeroize(); return std::ptr::null_mut(); }
    };
    let b64_c = match std::ffi::CString::new(b64_str) {
        Ok(s) => s, Err(_) => { payer_bytes.zeroize(); return std::ptr::null_mut(); }
    };

    let mut out = [0u8; 2048]; // signed tx is larger than 1024 bytes
    let written = unsafe {
        crate::ffi::lxmf_sign_tx(payer_bytes.as_ptr(), b64_c.as_ptr(), out.as_mut_ptr(), out.len())
    };
    payer_bytes.zeroize();
    if written < 0 { return std::ptr::null_mut(); }

    let s = match std::str::from_utf8(&out[..written as usize]) {
        Ok(s) => s, Err(_) => return std::ptr::null_mut(),
    };
    match env.new_string(s) {
        Ok(js) => js.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeExtractNonceBlockhash(
    mut env: JNIEnv,
    _class: JClass,
    account_data_b64: JString,
) -> jstring {
    let data_str: String = match env.get_string(&account_data_b64) {
        Ok(s) => s.into(), Err(_) => return std::ptr::null_mut(),
    };
    let data_c = match std::ffi::CString::new(data_str) {
        Ok(s) => s, Err(_) => return std::ptr::null_mut(),
    };
    let mut out = [0u8; 64];
    let written = unsafe {
        crate::ffi::lxmf_extract_nonce_blockhash(data_c.as_ptr(), out.as_mut_ptr(), out.len())
    };
    if written != 64 { return std::ptr::null_mut(); }
    match env.new_string(std::str::from_utf8(&out).unwrap_or("")) {
        Ok(js) => js.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// --- Beacon configuration ---

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeSetBeaconKeypair(
    mut env: JNIEnv,
    _class: JClass,
    key_bytes: JByteArray,
) -> jint {
    use zeroize::Zeroize;
    let mut bytes = match jbytes_to_vec(&mut env, &key_bytes) {
        Ok(b) => b,
        Err(_) => return -1,
    };
    if bytes.len() != 32 && bytes.len() != 64 { bytes.zeroize(); return -1; }
    let result = unsafe {
        crate::ffi::lxmf_beacon_set_keypair(bytes.as_ptr() as *const u8, bytes.len() as i32)
    };
    bytes.zeroize();
    result
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeSetBeaconSolanaRpc(
    mut env: JNIEnv,
    _class: JClass,
    url: JString,
) -> jint {
    let url_str: String = match env.get_string(&url) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    let c_str = match std::ffi::CString::new(url_str) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    unsafe { crate::ffi::lxmf_beacon_set_solana_rpc_url(c_str.as_ptr()) }
}

// --- Program ID ---

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeSetProgramId(
    mut env: JNIEnv,
    _class: JClass,
    program_id_hex: JString,
) -> jint {
    let hex_str: String = match env.get_string(&program_id_hex) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    let c_str = match std::ffi::CString::new(hex_str) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    unsafe { crate::ffi::lxmf_set_program_id(c_str.as_ptr()) }
}

#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeGetProgramId(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let mut out = [0u8; 64];
    let written = unsafe { crate::ffi::lxmf_get_program_id(out.as_mut_ptr(), out.len()) };
    if written != 64 { return std::ptr::null_mut(); }
    match env.new_string(std::str::from_utf8(&out).unwrap_or("")) {
        Ok(js) => js.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
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
    env: JNIEnv,
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

// --- NUS Interface (RNode BLE via Nordic UART Service) ---

/// Called by Kotlin NusManager when a NUS RX notification arrives from an RNode.
/// `data` — raw bytes from the NUS RX characteristic notification (partial KISS frame OK).
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeNusReceive(
    mut env: JNIEnv,
    _class: JClass,
    data: JByteArray,
) {
    match jbytes_to_vec(&mut env, &data) {
        Ok(bytes) => crate::nus_iface::on_nus_rx(bytes),
        Err(e) => error!("nativeNusReceive: failed to read data: {}", e),
    }
}

/// Called by Kotlin NusManager to dequeue the next KISS-framed bytes to write to the
/// RNode's NUS TX characteristic. Returns null when the queue is empty.
#[no_mangle]
pub extern "C" fn Java_expo_modules_lxmf_LxmfModule_nativeNusPollTx(
    env: JNIEnv,
    _class: JClass,
) -> jbyteArray {
    match crate::nus_iface::next_nus_tx() {
        None => std::ptr::null_mut(),
        Some(data) => match env.byte_array_from_slice(&data) {
            Ok(arr) => arr.into_raw(),
            Err(_) => std::ptr::null_mut(),
        },
    }
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
