mod msgpack_encode;
mod msgpack_decode;
mod media_fields;
mod beacon_announce;
mod interface_parsing;
mod event_decode;
mod store_persist;
mod lxmf_wire;
mod link_decode;
mod node_queue;
mod ble_synthetic_announce;
mod ble_pending_flush;

/// Shared serialization lock for all tests that touch the global LxmfNode singleton.
/// Every test that calls LxmfNode::init / start / stop must hold this lock.
/// Separate per-file statics cause concurrent test threads to race on the singleton.
pub(super) static NODE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
