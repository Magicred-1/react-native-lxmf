//! Group channel support for LXMF mesh group chat.
//!
//! Uses Reticulum's GROUP destination type (Fernet/AES shared-key encryption).
//! Group address = PlainInputDestination hash of "lxmf.group.<name>" (no identity component).
//!
//! Wire format inside the Fernet layer: standard LXMF
//!   [16B dest_hash=group_addr][16B src_hash=sender_lxmf_addr][64B Ed25519 sig][msgpack]
//!
//! Key distribution is the app's responsibility — share the 16-byte key via unicast LXMF.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rns_transport::destination::{DestinationName, EmptyIdentity, PlainInputDestination};

pub use rns_transport::destination::{group_decrypt, group_encrypt};

fn registry() -> Arc<Mutex<HashMap<[u8; 16], [u8; 16]>>> {
    static R: OnceLock<Arc<Mutex<HashMap<[u8; 16], [u8; 16]>>>> = OnceLock::new();
    R.get_or_init(|| Arc::new(Mutex::new(HashMap::new()))).clone()
}

/// Compute the 16-byte Reticulum GROUP address hash for a group name.
/// Deterministic: same name → same address on every device.
pub fn group_address_hash(name: &str) -> [u8; 16] {
    let dest_name = DestinationName::new("lxmf", &format!("group.{}", name));
    let plain = PlainInputDestination::new(EmptyIdentity, dest_name);
    let mut out = [0u8; 16];
    out.copy_from_slice(plain.desc.address_hash.as_slice());
    out
}

/// Register a group so inbound Group packets for this address are decrypted.
pub fn register(addr: [u8; 16], key: [u8; 16]) {
    if let Ok(mut m) = registry().lock() {
        m.insert(addr, key);
    }
}

/// Stop listening to a group.
pub fn unregister(addr: &[u8; 16]) {
    if let Ok(mut m) = registry().lock() {
        m.remove(addr);
    }
}

/// Look up the shared key for a group address. Returns None if not joined.
pub fn lookup_key(addr: &[u8; 16]) -> Option<[u8; 16]> {
    registry().lock().ok()?.get(addr).copied()
}
