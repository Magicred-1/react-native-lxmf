//! Nordic UART Service (NUS) interface for RNode BLE connectivity.
//!
//! RNode firmware on Heltec V3 exposes BLE via NUS (Nordic UART Service).
//! This interface handles KISS framing over the NUS serial link — same wire
//! format as USB serial to RNode.
//!
//! # Architecture
//!
//!   [Swift BLEManager] <--(C FFI)--> [NusInterface] <--> [rns-transport]
//!
//!   Swift:  CoreBluetooth NUS discovery, characteristic write/notify.
//!   Rust:   KISS framing, Reticulum packet serialize/deserialize, transport.
//!
//! # Wire format
//!
//!   KISS( Reticulum_packet_bytes )
//!
//!   FEND=0xC0, FESC=0xDB, TFEND=0xDC, TFESC=0xDD
//!   Same codec as `crate::framing::kiss_encode` / `KissDeframer`.
//!
//! # NUS GATT profile
//!
//!   Service: 6e400001-b5a3-f393-e0a9-e50e24dcca9e
//!   TX char: 6e400002-... (phone writes TO RNode)
//!   RX char: 6e400003-... (phone receives FROM RNode, via notify)

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use rns_transport::buffer::{InputBuffer, OutputBuffer};
use rns_transport::iface::{Interface, InterfaceContext, RxMessage};
use rns_transport::packet::Packet;
use rns_transport::serde::Serialize;

use crate::framing::{kiss_encode, KissDeframer};

// ── NUS GATT UUIDs ──────────────────────────────────────────────────────────

pub const NUS_SERVICE_UUID: &str = "6e400001-b5a3-f393-e0a9-e50e24dcca9e";
/// Phone writes TO RNode on this characteristic.
pub const NUS_TX_CHAR_UUID: &str = "6e400002-b5a3-f393-e0a9-e50e24dcca9e";
/// Phone receives FROM RNode via notify on this characteristic.
pub const NUS_RX_CHAR_UUID: &str = "6e400003-b5a3-f393-e0a9-e50e24dcca9e";

// ── Limits ──────────────────────────────────────────────────────────────────

/// Maximum KISS-decoded Reticulum packet size we'll accept.
const NUS_MAX_PACKET: usize = 500;
/// Maximum number of frames buffered from Swift before the async task drains them.
const NUS_RX_QUEUE_MAX: usize = 64;

// ── Shared queues (Swift ↔ Rust) ────────────────────────────────────────────

fn nus_rx_queue() -> Arc<Mutex<VecDeque<Vec<u8>>>> {
    static Q: OnceLock<Arc<Mutex<VecDeque<Vec<u8>>>>> = OnceLock::new();
    Q.get_or_init(|| Arc::new(Mutex::new(VecDeque::new()))).clone()
}

fn nus_tx_queue() -> Arc<Mutex<VecDeque<Vec<u8>>>> {
    static Q: OnceLock<Arc<Mutex<VecDeque<Vec<u8>>>>> = OnceLock::new();
    Q.get_or_init(|| Arc::new(Mutex::new(VecDeque::new()))).clone()
}

// ── Swift-callable entry points ─────────────────────────────────────────────

/// Swift calls this when a NUS RX notification arrives from the RNode.
/// `data` is raw bytes from the BLE characteristic — may be a partial KISS frame.
pub fn on_nus_rx(data: Vec<u8>) {
    if let Ok(mut q) = nus_rx_queue().lock() {
        if q.len() < NUS_RX_QUEUE_MAX {
            q.push_back(data);
        } else {
            log::warn!("NusInterface: RX queue full ({}), dropping frame", NUS_RX_QUEUE_MAX);
        }
    }
}

/// Swift calls this to dequeue the next KISS-framed bytes to write to the
/// RNode's NUS TX characteristic.
/// Returns `None` when nothing is queued.
pub fn next_nus_tx() -> Option<Vec<u8>> {
    nus_tx_queue().lock().ok()?.pop_front()
}

// ── NusInterface ────────────────────────────────────────────────────────────

/// rns-transport interface for RNode BLE (NUS + KISS framing).
pub struct NusInterface;

impl NusInterface {
    pub fn new() -> Self {
        Self
    }

    /// Async worker — drives the RX and TX loops.
    ///
    /// RX: drains NUS bytes from Swift, KISS-decodes, deserializes Reticulum packets.
    /// TX: serializes Reticulum packets, KISS-encodes, enqueues for Swift to write.
    pub async fn spawn(context: InterfaceContext<NusInterface>) {
        let iface_address = context.channel.address;
        let (rx_channel, mut tx_channel) = context.channel.split();
        let cancel = context.cancel.clone();

        let rx_queue = nus_rx_queue();
        let tx_queue = nus_tx_queue();

        // Stateful KISS deframer — accumulates partial frames across NUS notifications.
        let mut deframer = KissDeframer::new();

        // Serialization scratch buffer.
        let mut serialize_buf = [0u8; NUS_MAX_PACKET + 64];

        log::info!("NusInterface: started, iface={}", iface_address);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    log::info!("NusInterface: cancelled");
                    break;
                }

                // Poll every 10 ms — matches BleInterface cadence.
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(10)) => {

                    // ── RX: drain Swift-filled queue ─────────────────────
                    let incoming: Vec<Vec<u8>> = {
                        match rx_queue.lock() {
                            Ok(mut q) => q.drain(..).collect(),
                            Err(_) => Vec::new(),
                        }
                    };

                    for raw_bytes in incoming {
                        // Feed raw NUS bytes into the KISS deframer.
                        // May yield 0, 1, or multiple complete frames.
                        let frames = deframer.feed(&raw_bytes);

                        for (cmd, payload) in frames {
                            // Only process data frames (cmd 0x00).
                            if cmd != 0x00 {
                                log::debug!("NusInterface: ignoring KISS cmd 0x{:02x}", cmd);
                                continue;
                            }

                            if payload.len() > NUS_MAX_PACKET {
                                log::warn!(
                                    "NusInterface: KISS payload {}B > max {}B, dropping",
                                    payload.len(), NUS_MAX_PACKET
                                );
                                continue;
                            }

                            // Deserialize as Reticulum packet.
                            let mut inp = InputBuffer::new(&payload);
                            match Packet::deserialize(&mut inp) {
                                Ok(packet) => {
                                    log::debug!(
                                        "NusInterface: RX packet dst={} {}B",
                                        packet.destination, payload.len()
                                    );
                                    let _ = rx_channel.send(RxMessage {
                                        address: iface_address,
                                        packet,
                                        source: Default::default(),
                                    }).await;
                                }
                                Err(e) => {
                                    log::warn!(
                                        "NusInterface: packet deserialize failed: {:?} ({}B)",
                                        e, payload.len()
                                    );
                                }
                            }
                        }
                    }

                    // ── TX: drain transport's outgoing queue ──────────────
                    while let Ok(msg) = tx_channel.try_recv() {
                        let packet = msg.packet;

                        // Serialize Reticulum packet.
                        let serialized_len = {
                            let mut out = OutputBuffer::new(&mut serialize_buf);
                            match packet.serialize(&mut out) {
                                Ok(_) => out.offset(),
                                Err(e) => {
                                    log::warn!("NusInterface: TX serialize failed: {:?}", e);
                                    continue;
                                }
                            }
                        };

                        if serialized_len > NUS_MAX_PACKET {
                            log::warn!(
                                "NusInterface: TX packet {}B > max {}B, dropping",
                                serialized_len, NUS_MAX_PACKET
                            );
                            continue;
                        }

                        let serialized = &serialize_buf[..serialized_len];

                        // KISS encode — single frame, no segmentation.
                        let kiss_frame = kiss_encode(serialized);

                        log::debug!(
                            "NusInterface: TX {}B KISS-framed to RNode",
                            kiss_frame.len()
                        );

                        if let Ok(mut tx_q) = tx_queue.lock() {
                            tx_q.push_back(kiss_frame);
                        }
                    }
                }
            }
        }

        log::info!("NusInterface: stopped");
    }
}

impl Interface for NusInterface {
    fn mtu() -> usize {
        // NUS MTU — the KISS framing adds overhead, but the transport layer
        // sees the raw packet size. 244 matches BLE 5.0 DLE.
        244
    }
}

impl Default for NusInterface {
    fn default() -> Self {
        Self::new()
    }
}
