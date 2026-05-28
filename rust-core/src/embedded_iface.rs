//! Embedded Reticulum interface — bridges RNodes running their own embedded Reticulum stack.
//!
//! Some RNode firmware builds run `rns-embedded-runtime` directly on the device.
//! The phone connects via NUS BLE and bridges raw PacketFrame bytes through
//! `BleShimTransport` + `EmbeddedNodeRuntime`.
//!
//! This is SEPARATE from `nus_iface.rs` (KISS-mode RNode radio) and `ble_iface.rs`
//! (phone↔phone HDLC mesh). All three interfaces register on the same Transport
//! instance in `start_ble()` and can run concurrently.
//!
//! # Wire format (embedded runtime)
//!
//!   encode_frame(PacketFrame) — 2-byte big-endian length prefix + payload
//!
//! # Native ↔ Rust FFI
//!
//!   `lxmf_embedded_rx(data, len)` — Kotlin/Swift calls when bytes arrive from device
//!   `lxmf_embedded_poll_tx(out, cap) -> i32` — Kotlin/Swift polls for bytes to send

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rns_embedded_core::{
    store::JournaledEmbeddedStore,
    transport::LinkState,
};
use rns_embedded_runtime::{
    EmbeddedNodeRuntime, NodeTransportMode, RuntimeConfig,
    ble::{BleShimConfig, BleShimTransport},
};

// ── Global state ─────────────────────────────────────────────────────────────

struct EmbeddedState {
    runtime: EmbeddedNodeRuntime,
    shim: BleShimTransport,
    store: JournaledEmbeddedStore,
    /// Outbound wire bytes queued for the native layer to send via NUS.
    tx_queue: VecDeque<Vec<u8>>,
}

fn embedded_state() -> &'static Mutex<Option<EmbeddedState>> {
    static STATE: OnceLock<Mutex<Option<EmbeddedState>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

// ── Public init ──────────────────────────────────────────────────────────────

/// Initialise the embedded runtime with the node's identity and LXMF address.
/// Call once after `lxmf_init()`, before `start_ble()`.
pub fn init_embedded(store_identity: [u8; 32], lxmf_address: [u8; 16]) {
    let config = RuntimeConfig {
        store_identity,
        lxmf_address,
        node_mode: NodeTransportMode::BleOnly,
        announce_interval_ms: 10_000,
        max_outbound_queue: 8,
        max_events: 32,
        ..Default::default()
    };
    let runtime = match EmbeddedNodeRuntime::new(config) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("embedded_iface: EmbeddedNodeRuntime::new failed: {:?}", e);
            return;
        }
    };
    let shim = match BleShimTransport::new(BleShimConfig::default()) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("embedded_iface: BleShimTransport::new failed: {:?}", e);
            return;
        }
    };
    let store = JournaledEmbeddedStore::new();
    if let Ok(mut g) = embedded_state().lock() {
        *g = Some(EmbeddedState { runtime, shim, store, tx_queue: VecDeque::new() });
    }
}

// ── Inbound (native → Rust) ──────────────────────────────────────────────────

/// Called by Kotlin/Swift when raw PacketFrame bytes arrive from an embedded-stack RNode.
/// Thread-safe; can be called from any thread.
pub fn on_embedded_rx(data: Vec<u8>) {
    let Ok(mut g) = embedded_state().lock() else { return };
    let Some(state) = g.as_mut() else { return };
    state.shim.set_link_state(LinkState::Up);
    if let Err(e) = state.shim.push_inbound_wire(&data) {
        log::warn!("embedded_iface: push_inbound_wire failed: {:?}", e);
    }
    tick_embedded(state);
}

// ── Outbound (Rust → native) ─────────────────────────────────────────────────

/// Dequeue the next wire bytes to send to the embedded-stack RNode via NUS.
/// Returns `None` when nothing is queued.
pub fn next_embedded_tx() -> Option<Vec<u8>> {
    let mut g = embedded_state().lock().ok()?;
    g.as_mut()?.tx_queue.pop_front()
}

// ── Tick ─────────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn tick_embedded(state: &mut EmbeddedState) {
    if let Err(e) = state.runtime.tick(now_ms(), &mut state.shim, &mut state.store) {
        log::warn!("embedded_iface: tick failed: {:?}", e);
        return;
    }
    for bytes in state.shim.drain_outbound_wire() {
        state.tx_queue.push_back(bytes);
    }
    for event in state.runtime.drain_events() {
        log::debug!("embedded_iface: event {:?}", event);
    }
}

/// Periodic tick — call from the background timer loop or directly after `on_embedded_rx`.
pub fn tick() {
    if let Ok(mut g) = embedded_state().lock() {
        if let Some(state) = g.as_mut() {
            tick_embedded(state);
        }
    }
}
