//! BLE interface for rns-transport — phone-to-phone Reticulum without internet.
//!
//! # Architecture
//!
//! Rust cannot access BLE hardware on Android/iOS directly — the OS BLE stack
//! (Android BLE API / Core Bluetooth) only exposes through Java/Kotlin/Swift.
//!
//! Split of responsibilities:
//!   [Kotlin BleManager] <--(JNI)--> [BleInterface] <--> [rns-transport]
//!
//!   Kotlin: BLE scan, advertise, GATT connect/disconnect, characteristic write/notify.
//!   Rust:   HDLC framing, segmentation, Reticulum packet serialize/deserialize, transport.
//!
//! # Wire format (same as Reticulum serial interface)
//!
//!   HDLC( Reticulum_packet_bytes )
//!
//! HDLC flag = 0x7E, escape = 0x7D, same codec as `crate::framing::hdlc_encode`.
//!
//! Write limit is negotiated per-peer via ATT MTU exchange (target 247B = ATT_MTU 250 − 3,
//! matching the lxmf-sdk MobileBleCapabilities::max_payload_bytes contract).
//!
//! # Segmentation (for packets that exceed the negotiated write limit)
//!
//! Each BLE characteristic write is prefixed with a 2-byte segment header:
//!   [seg_idx: u8][total_segs: u8][payload...]
//! Single-frame packets use header [0, 1]. Receiver reassembles before HDLC decode.
//!
//! Packets > BLE_MAX_PACKET (500 B) are dropped — use Reticulum Resources for large payloads.
//! RX queue is capped at BLE_RX_QUEUE_MAX (64) frames; overflow drops newest arrivals.
//!
//! # GATT profile
//!
//! Service:    RNS_BLE_SERVICE_UUID
//! TX char:    write-without-response (central writes to peripheral)
//! RX char:    notify (peripheral notifies central on incoming data)

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use rns_transport::buffer::{InputBuffer, OutputBuffer};
use rns_transport::iface::{Interface, InterfaceContext, RxMessage};
use rns_transport::packet::Packet;
use rns_transport::serde::Serialize;

use crate::framing::{hdlc_encode, HdlcDeframer};

// ── GATT UUIDs ───────────────────────────────────────────────────────────────

/// Reticulum BLE service UUID — matches RNode/anon0mesh convention.
pub const RNS_BLE_SERVICE_UUID: &str = "5f3bafcd-2bb7-4de0-9c6f-2c5f88b6b8f2";
/// Central writes here (peripheral → central direction in Kotlin, i.e. what remote sends us).
pub const RNS_BLE_RX_CHAR_UUID: &str = "3b28e4f6-5a30-4a5f-b700-68bb74d1b036";
/// We write here to send data (central → peripheral, what we write to remote).
pub const RNS_BLE_TX_CHAR_UUID: &str = "8b6ded1a-ea65-4a1e-a1f0-5cf69d5dc2ad";

// ── Limits ────────────────────────────────────────────────────────────────────

/// BLE characteristic write MTU (bytes). BLE 5.0 DLE = 251 B max; iOS caps at 244 B.
pub const BLE_MTU: usize = 244;

/// Maximum Reticulum packet size we'll attempt to send over BLE.
/// Bigger packets are silently dropped — Reticulum Resources handle large transfers.
pub const BLE_MAX_PACKET: usize = 500;

/// Bytes consumed by the segmentation header.
const SEG_HEADER: usize = 2;
/// Maximum payload bytes per BLE write after subtracting segment header.
const SEG_PAYLOAD: usize = BLE_MTU - SEG_HEADER;

// ── Shared queues (Kotlin ↔ Rust) ────────────────────────────────────────────

/// One incoming BLE frame from a remote peer.
pub struct BleRxFrame {
    /// Sender's 6-byte Bluetooth MAC address.
    pub peer_addr: [u8; 6],
    /// Raw characteristic value (may be a single segment).
    pub data: Vec<u8>,
}

/// One outgoing BLE frame destined for a remote peer.
pub struct BleTxFrame {
    /// Recipient's 6-byte Bluetooth MAC address.
    pub peer_addr: [u8; 6],
    /// Bytes to write to the TX characteristic.
    pub data: Vec<u8>,
}

fn ble_rx_queue() -> Arc<Mutex<VecDeque<BleRxFrame>>> {
    static Q: OnceLock<Arc<Mutex<VecDeque<BleRxFrame>>>> = OnceLock::new();
    Q.get_or_init(|| Arc::new(Mutex::new(VecDeque::new()))).clone()
}

fn ble_tx_queue() -> Arc<Mutex<VecDeque<BleTxFrame>>> {
    static Q: OnceLock<Arc<Mutex<VecDeque<BleTxFrame>>>> = OnceLock::new();
    Q.get_or_init(|| Arc::new(Mutex::new(VecDeque::new()))).clone()
}

fn ble_peers() -> Arc<Mutex<Vec<[u8; 6]>>> {
    static P: OnceLock<Arc<Mutex<Vec<[u8; 6]>>>> = OnceLock::new();
    P.get_or_init(|| Arc::new(Mutex::new(Vec::new()))).clone()
}

/// Per-peer rate-limit state: (window_start, frame_count_in_window).
fn ble_rx_rate_state() -> Arc<Mutex<HashMap<[u8; 6], (std::time::Instant, usize)>>> {
    static RL: OnceLock<Arc<Mutex<HashMap<[u8; 6], (std::time::Instant, usize)>>>> = OnceLock::new();
    RL.get_or_init(|| Arc::new(Mutex::new(HashMap::new()))).clone()
}

/// Returns true if the frame should be accepted; false if the peer has exceeded BLE_RX_PPS_LIMIT.
fn check_ble_rx_rate(peer_addr: [u8; 6]) -> bool {
    let state = ble_rx_rate_state();
    let Ok(mut map) = state.lock() else { return true };
    let now = std::time::Instant::now();
    let entry = map.entry(peer_addr).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() >= 1 {
        *entry = (now, 1);
        true
    } else {
        entry.1 += 1;
        entry.1 <= BLE_RX_PPS_LIMIT
    }
}

/// Notifier fired whenever a new BLE peer completes GATT connect.
/// The node task awaits this to trigger an immediate re-announce, so
/// peers don't have to wait for the 5s periodic timer after first connect.
fn ble_peer_connected_notify() -> Arc<tokio::sync::Notify> {
    static N: OnceLock<Arc<tokio::sync::Notify>> = OnceLock::new();
    N.get_or_init(|| Arc::new(tokio::sync::Notify::new())).clone()
}

/// Public accessor for the node task to await peer-connect events.
pub fn peer_connected_notify() -> Arc<tokio::sync::Notify> {
    ble_peer_connected_notify()
}

// ── JNI-callable entry points ─────────────────────────────────────────────────
// These are called from Kotlin via JNI — no tokio context required.

/// Maximum frames buffered from Kotlin before backpressure drops newest arrivals.
/// Matches lxmf-sdk MobileBleCapabilities::max_notification_queue contract.
const BLE_RX_QUEUE_MAX: usize = 64;
/// Maximum BLE frames accepted per peer per second. Excess frames are dropped
/// to prevent a single rogue device from flooding the transport pipeline.
const BLE_RX_PPS_LIMIT: usize = 50;

/// Kotlin calls this when a BLE characteristic notification arrives from a peer.
pub fn on_ble_rx(peer_addr: [u8; 6], data: Vec<u8>) {
    if !check_ble_rx_rate(peer_addr) {
        log::warn!("BleInterface: rate limit ({} pps) exceeded from {:02x?}, dropping",
            BLE_RX_PPS_LIMIT, peer_addr);
        return;
    }
    if let Ok(mut q) = ble_rx_queue().lock() {
        if q.len() >= BLE_RX_QUEUE_MAX {
            log::warn!("BleInterface: RX queue full ({} frames), dropping from {:02x?}", BLE_RX_QUEUE_MAX, peer_addr);
            return;
        }
        q.push_back(BleRxFrame { peer_addr, data });
    }
}

/// Kotlin calls this to dequeue the next frame it should write to a peer characteristic.
/// Returns `None` when nothing is queued.
pub fn next_ble_tx() -> Option<BleTxFrame> {
    ble_tx_queue().lock().ok()?.pop_front()
}

/// Kotlin calls this when a GATT connection is established with a remote peer.
pub fn on_ble_connected(peer_addr: [u8; 6]) {
    let mut newly_added = false;
    if let Ok(mut peers) = ble_peers().lock() {
        if !peers.contains(&peer_addr) {
            peers.push(peer_addr);
            log::info!("BleInterface: peer connected {:02x?}", peer_addr);
            newly_added = true;
        }
    }
    if newly_added {
        ble_peer_connected_notify().notify_waiters();
    }
}

/// Kotlin calls this when a GATT connection is lost.
pub fn on_ble_disconnected(peer_addr: [u8; 6]) {
    if let Ok(mut peers) = ble_peers().lock() {
        let before = peers.len();
        peers.retain(|p| p != &peer_addr);
        if peers.len() < before {
            log::info!("BleInterface: peer disconnected {:02x?}", peer_addr);
        }
    }
}

/// Returns the current number of connected BLE peers.
pub fn ble_peer_count() -> usize {
    ble_peers().lock().map(|p| p.len()).unwrap_or(0)
}

/// Clears the peer list — called on node init to drop stale entries from a previous session.
pub fn clear_ble_peers() {
    if let Ok(mut peers) = ble_peers().lock() {
        peers.clear();
    }
}

fn ble_peer_mtu() -> Arc<Mutex<HashMap<[u8; 6], usize>>> {
    static M: OnceLock<Arc<Mutex<HashMap<[u8; 6], usize>>>> = OnceLock::new();
    M.get_or_init(|| Arc::new(Mutex::new(HashMap::new()))).clone()
}

/// Called from JNI after Kotlin's onMtuChanged. `char_write_limit` = ATT MTU − 3.
pub fn on_mtu_negotiated(peer_addr: [u8; 6], char_write_limit: usize) {
    if let Ok(mut map) = ble_peer_mtu().lock() {
        map.insert(peer_addr, char_write_limit);
        log::info!("BleInterface: peer {:02x?} write limit={}B", peer_addr, char_write_limit);
    }
}

fn peer_mtu(peer_addr: &[u8; 6]) -> usize {
    ble_peer_mtu()
        .lock()
        .ok()
        .and_then(|m| m.get(peer_addr).copied())
        // ATT_MTU 250 → 247B write payload (lxmf-sdk max_payload_bytes contract).
        // MTU is always negotiated before on_ble_connected fires, so this only
        // applies in error paths where negotiation was skipped entirely.
        .unwrap_or(247)
}

// ── Segmentation ──────────────────────────────────────────────────────────────

/// Encode `data` into one or more BLE-MTU-sized frames, each with a 2-byte segment header.
///
/// Header layout: `[seg_idx: u8][total_segs: u8][payload...]`
///
/// Single-frame path (data ≤ BLE_MTU): header is `[0, 1]`, one frame total.
pub fn segment(data: &[u8]) -> Vec<Vec<u8>> {
    if data.len() <= BLE_MTU {
        let mut frame = Vec::with_capacity(SEG_HEADER + data.len());
        frame.push(0u8); // seg_idx = 0
        frame.push(1u8); // total_segs = 1
        frame.extend_from_slice(data);
        return vec![frame];
    }

    let total_segs = (data.len() + SEG_PAYLOAD - 1) / SEG_PAYLOAD;
    debug_assert!(total_segs <= 255, "packet too large even for segmentation");
    data.chunks(SEG_PAYLOAD)
        .enumerate()
        .map(|(idx, chunk)| {
            let mut frame = Vec::with_capacity(SEG_HEADER + chunk.len());
            frame.push(idx as u8);
            frame.push(total_segs as u8);
            frame.extend_from_slice(chunk);
            frame
        })
        .collect()
}

/// Encode `data` into BLE segments using `seg_payload` bytes per segment payload.
pub fn segment_with_payload(data: &[u8], seg_payload: usize) -> Vec<Vec<u8>> {
    if data.len() <= seg_payload {
        let mut frame = Vec::with_capacity(SEG_HEADER + data.len());
        frame.push(0u8);
        frame.push(1u8);
        frame.extend_from_slice(data);
        return vec![frame];
    }
    let total_segs = (data.len() + seg_payload - 1) / seg_payload;
    debug_assert!(total_segs <= 255, "packet too large even for segmentation");
    data.chunks(seg_payload)
        .enumerate()
        .map(|(idx, chunk)| {
            let mut frame = Vec::with_capacity(SEG_HEADER + chunk.len());
            frame.push(idx as u8);
            frame.push(total_segs as u8);
            frame.extend_from_slice(chunk);
            frame
        })
        .collect()
}

/// Per-peer reassembly buffer.
struct PeerAssembly {
    total_segs: u8,
    slots: Vec<Option<Vec<u8>>>,
}

impl PeerAssembly {
    fn new(total: u8) -> Self {
        Self { total_segs: total, slots: vec![None; total as usize] }
    }

    /// Inserts segment at `idx`. Returns the reassembled payload when all segments are present.
    fn push(&mut self, idx: u8, payload: Vec<u8>) -> Option<Vec<u8>> {
        if idx as usize >= self.slots.len() {
            return None; // corrupt header
        }
        self.slots[idx as usize] = Some(payload);
        if self.slots.iter().all(Option::is_some) {
            Some(self.slots.iter().flat_map(|s| s.as_ref().unwrap().iter().copied()).collect())
        } else {
            None // still waiting
        }
    }
}

// ── BleInterface ──────────────────────────────────────────────────────────────

/// rns-transport interface implementation for BLE.
///
/// Plug into the transport like this (inside an async context):
///
/// ```no_run
/// let iface_mgr = transport.iface_manager();
/// let mut mgr = iface_mgr.lock().await;
/// mgr.spawn(BleInterface::new(), BleInterface::spawn);
/// ```
pub struct BleInterface;

impl BleInterface {
    pub fn new() -> Self {
        Self
    }

    /// Async worker function — drives the RX and TX loops.
    ///
    /// Polling is done on a 10 ms timer instead of blocking on a socket,
    /// since BLE bytes arrive asynchronously via JNI callbacks from Kotlin.
    pub async fn spawn(context: InterfaceContext<BleInterface>) {
        let iface_address = context.channel.address;
        let (rx_channel, mut tx_channel) = context.channel.split();
        let cancel = context.cancel.clone();

        let rx_queue = ble_rx_queue();
        let tx_queue = ble_tx_queue();
        let peers_arc = ble_peers();

        // Per-peer segment reassembly state.
        let mut assembly: HashMap<[u8; 6], PeerAssembly> = HashMap::new();
        // HDLC deframer — stateful, accumulates partial HDLC frames across polls.
        let mut deframer = HdlcDeframer::new();

        log::info!("BleInterface: started, iface={}", iface_address);

        // Buffer for packet serialization.
        let mut serialize_buf = [0u8; BLE_MAX_PACKET + 64];

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    log::info!("BleInterface: cancelled");
                    break;
                }

                // Poll every 10 ms — low enough latency for interactive mesh use.
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(10)) => {

                    // ── RX: drain Kotlin-filled queue ─────────────────────
                    let incoming: Vec<BleRxFrame> = {
                        match rx_queue.lock() {
                            Ok(mut q) => q.drain(..).collect(),
                            Err(_) => Vec::new(),
                        }
                    };

                    for frame in incoming {
                        if frame.data.len() < SEG_HEADER {
                            log::warn!(
                                "BleInterface: RX frame too short ({}B) from {:02x?}, dropping",
                                frame.data.len(), frame.peer_addr
                            );
                            continue;
                        }

                        let seg_idx   = frame.data[0];
                        let total_seg = frame.data[1];
                        let payload   = frame.data[SEG_HEADER..].to_vec();

                        // Reassemble multi-segment frames; single-segment pass through.
                        let full_hdlc: Option<Vec<u8>> = if total_seg == 1 {
                            // Single-segment — no assembly needed.
                            assembly.remove(&frame.peer_addr);
                            Some(payload)
                        } else {
                            let state = assembly
                                .entry(frame.peer_addr)
                                .or_insert_with(|| PeerAssembly::new(total_seg));
                            // Reset if total_segs changed (new message).
                            if state.total_segs != total_seg {
                                *state = PeerAssembly::new(total_seg);
                            }
                            let complete = state.push(seg_idx, payload);
                            if complete.is_some() {
                                assembly.remove(&frame.peer_addr);
                            }
                            complete
                        };

                        let hdlc_data = match full_hdlc {
                            Some(d) => d,
                            None    => continue, // still waiting for more segments
                        };

                        // HDLC decode — may yield multiple Reticulum packets from one blob.
                        let raw_packets = deframer.feed(&hdlc_data);
                        for raw in raw_packets {
                            let mut inp = InputBuffer::new(&raw);
                            match Packet::deserialize(&mut inp) {
                                Ok(packet) => {
                                    log::debug!(
                                        "BleInterface: RX packet dst={} {}B from {:02x?}",
                                        packet.destination, raw.len(), frame.peer_addr
                                    );
                                    let _ = rx_channel.send(RxMessage {
                                        address: iface_address,
                                        packet,
                                        source: Default::default(),
                                    }).await;
                                }
                                Err(e) => {
                                    log::warn!(
                                        "BleInterface: packet deserialize failed from {:02x?}: {:?}",
                                        frame.peer_addr, e
                                    );
                                }
                            }
                        }
                    }

                    // ── TX: drain transport's outgoing queue ──────────────
                    while let Ok(msg) = tx_channel.try_recv() {
                        let packet = msg.packet;

                        // Serialize Reticulum packet into our scratch buffer.
                        let serialized_len = {
                            let mut out = OutputBuffer::new(&mut serialize_buf);
                            match packet.serialize(&mut out) {
                                Ok(_) => out.offset(),
                                Err(e) => {
                                    log::warn!("BleInterface: TX serialize failed: {:?}", e);
                                    continue;
                                }
                            }
                        };

                        if serialized_len > BLE_MAX_PACKET {
                            log::warn!(
                                "BleInterface: TX packet {}B > BLE_MAX_PACKET {}B, dropping",
                                serialized_len, BLE_MAX_PACKET
                            );
                            continue;
                        }

                        let serialized = &serialize_buf[..serialized_len];

                        // HDLC encode.
                        let hdlc_frame = hdlc_encode(serialized);

                        // Get current peer list.
                        let peer_addrs: Vec<[u8; 6]> = peers_arc
                            .lock()
                            .map(|p| p.clone())
                            .unwrap_or_default();

                        if peer_addrs.is_empty() {
                            log::debug!("BleInterface: TX drop — no connected peers");
                            continue;
                        }

                        // Segment per-peer using negotiated write limit.
                        if let Ok(mut tx_q) = tx_queue.lock() {
                            for peer in &peer_addrs {
                                let write_limit = peer_mtu(peer);
                                let seg_payload = write_limit.saturating_sub(SEG_HEADER).max(1);
                                for seg in segment_with_payload(&hdlc_frame, seg_payload) {
                                    log::debug!(
                                        "BleInterface: TX seg {}B to {:02x?} (limit={}B)",
                                        seg.len(), peer, write_limit
                                    );
                                    tx_q.push_back(BleTxFrame {
                                        peer_addr: *peer,
                                        data: seg,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        log::info!("BleInterface: stopped");
    }
}

impl Interface for BleInterface {
    fn mtu() -> usize {
        BLE_MTU
    }
}

impl Default for BleInterface {
    fn default() -> Self {
        Self::new()
    }
}
