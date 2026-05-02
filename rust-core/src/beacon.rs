//! Beacon announce/discovery + JSON-RPC dispatch — anon0mesh protocol.
//!
//! Protocol: https://github.com/anonmesh/anon0mesh_cli
//!
//! Beacon flow:
//!   1. Beacons announce with app_data starting with ANNOUNCE_DATA prefix
//!   2. Clients discover via startswith filter, open Reticulum links
//!   3. Client sends JSON-RPC 2.0 requests compressed with zlib (magic b"\x00zl")
//!   4. Beacon forwards to Solana RPC, returns compressed response
//!   5. cosignTransaction: client sends partially-signed tx → beacon co-signs + submits
//!
//! This module is pure state — no tokio, no transport.
//! node.rs owns the transport and drives the actual RPC send/receive loop.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::node::{DestHash, LxmfEvent};

// ── Protocol constants ────────────────────────────────────────────────────────

/// Reticulum destination aspect — used for link establishment by the CLI.
pub const APP_ASPECT: &str = "rpc_beacon";

/// Request path the beacon registers; client sends JSON-RPC here.
pub const RPC_PATH: &str = "/rpc";

/// Announce detection prefix (no null byte — startswith comparison).
pub const ANNOUNCE_DATA: &[u8] = b"anonmesh::beacon::v1";

/// Full prefix as stored in announce data (with null separator before name).
const ANNOUNCE_PREFIX_FULL: &[u8] = b"anonmesh::beacon::v1\0";

/// Magic prefix for zlib-compressed payloads.
const COMPRESS_MAGIC: &[u8; 3] = b"\x00zl";

// Announce schedule (matches anon0mesh_cli burst/steady state)
const ANNOUNCE_BURST_INTERVAL: Duration = Duration::from_secs(15);
const ANNOUNCE_BURST_DURATION:  Duration = Duration::from_secs(120);
const ANNOUNCE_STEADY_INTERVAL: Duration = Duration::from_secs(300);

/// Exponential backoff for reconnection attempts (seconds).
const BACKOFF_SCHEDULE: &[u64] = &[5, 10, 20, 40, 60, 120, 300];

// ── Announce helpers ──────────────────────────────────────────────────────────

/// Returns true if `app_data` is from an anon0mesh beacon.
pub fn is_beacon_announce(app_data: &[u8]) -> bool {
    app_data.starts_with(ANNOUNCE_DATA)
}

/// Extract display name from beacon announce data (after the null separator).
pub fn extract_display_name(app_data: &[u8]) -> Option<&str> {
    // ANNOUNCE_PREFIX_FULL is 21 bytes (prefix + null separator)
    if app_data.len() > ANNOUNCE_PREFIX_FULL.len() && app_data.starts_with(ANNOUNCE_PREFIX_FULL) {
        std::str::from_utf8(&app_data[ANNOUNCE_PREFIX_FULL.len()..]).ok()
    } else {
        None
    }
}

// ── Compression (zlib, magic b"\x00zl") ──────────────────────────────────────

/// Compress with zlib level 6. Prepends magic only if smaller than raw.
pub fn compress_payload(data: &[u8]) -> Vec<u8> {
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(6));
    if enc.write_all(data).is_ok() {
        if let Ok(compressed) = enc.finish() {
            if compressed.len() + COMPRESS_MAGIC.len() < data.len() {
                let mut out = Vec::with_capacity(COMPRESS_MAGIC.len() + compressed.len());
                out.extend_from_slice(COMPRESS_MAGIC);
                out.extend_from_slice(&compressed);
                return out;
            }
        }
    }
    data.to_vec()
}

/// Decompress if magic present; pass through otherwise.
pub fn decompress_payload(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.starts_with(COMPRESS_MAGIC) {
        use flate2::read::ZlibDecoder;
        use std::io::Read;
        let mut dec = ZlibDecoder::new(&data[COMPRESS_MAGIC.len()..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).map_err(|e| e.to_string())?;
        Ok(out)
    } else {
        Ok(data.to_vec())
    }
}

// ── JSON-RPC 2.0 types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: u32,
    pub method: String,
    pub params: serde_json::Value,
}

impl RpcRequest {
    pub fn new(id: u32, method: impl Into<String>, params: serde_json::Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, method: method.into(), params }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RpcResponse {
    pub id: Option<u32>,
    pub result: Option<serde_json::Value>,
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn into_result(self) -> Result<serde_json::Value, RpcError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        Ok(self.result.unwrap_or(serde_json::Value::Null))
    }
}

/// Parse a (possibly compressed) JSON-RPC response from raw link bytes.
pub fn parse_rpc_response(data: &[u8]) -> Result<RpcResponse, String> {
    let raw = decompress_payload(data)?;
    serde_json::from_slice::<RpcResponse>(&raw).map_err(|e| e.to_string())
}

// ── Pending RPC ───────────────────────────────────────────────────────────────

/// Outbound RPC call staged for transmission via `send_via_link`.
#[derive(Debug)]
pub struct PendingRpc {
    pub id: u32,
    pub dest: DestHash,
    pub method: String,
    /// Compressed JSON-RPC bytes, ready to send.
    pub payload: Vec<u8>,
}

/// Completed RPC call with correlated result.
#[derive(Debug)]
pub struct RpcResult {
    pub id: u32,
    pub method: String,
    pub result: Result<serde_json::Value, RpcError>,
}

// ── Beacon state ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum BeaconState {
    Discovered,
    Connecting,
    Connected,
    Disconnected,
    Failed,
}

#[derive(Debug, Clone)]
pub struct Beacon {
    pub dest_hash: DestHash,
    pub state: BeaconState,
    pub display_name: Option<String>,
    pub last_announce: Instant,
    pub last_connected: Option<Instant>,
    pub reconnect_attempts: u32,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DispatchStrategy {
    /// Send to all beacons simultaneously — first valid response wins.
    Race,
    /// Try beacons sequentially until one succeeds.
    Fallback,
}

// ── BeaconManager ─────────────────────────────────────────────────────────────

pub struct BeaconManager {
    beacons: HashMap<DestHash, Beacon>,
    strategy: DispatchStrategy,
    announce_active: bool,
    /// When announcing was started — used for burst vs steady-state timing.
    announce_start: Option<Instant>,
    last_announce: Option<Instant>,
    announce_count: u32,
    pending_events: Vec<LxmfEvent>,
    // RPC dispatch state
    next_rpc_id: u32,
    pending_rpcs: VecDeque<PendingRpc>,
    /// id → method name, for correlating responses.
    in_flight: HashMap<u32, String>,
}

impl BeaconManager {
    pub fn new() -> Self {
        Self {
            beacons: HashMap::new(),
            strategy: DispatchStrategy::Race,
            announce_active: false,
            announce_start: None,
            last_announce: None,
            announce_count: 0,
            pending_events: Vec::new(),
            next_rpc_id: 1,
            pending_rpcs: VecDeque::new(),
            in_flight: HashMap::new(),
        }
    }

    // ── Announce scheduling ───────────────────────────────────────────────────

    pub fn start_announce_schedule(&mut self) {
        let now = Instant::now();
        self.announce_active = true;
        self.announce_start = Some(now);
        self.last_announce = Some(now);
        self.announce_count = 0;
    }

    pub fn stop(&mut self) {
        self.announce_active = false;
    }

    /// True when the next announce is due.
    ///
    /// Burst phase (first 2 min from start): announce every 15 s.
    /// Steady state: announce every 300 s.
    /// Mirrors the anon0mesh_cli `announce_loop` burst_end logic.
    pub fn should_announce(&self) -> bool {
        if !self.announce_active {
            return false;
        }
        let Some(last) = self.last_announce else { return true; };
        let Some(start) = self.announce_start else { return true; };

        let since_last  = last.elapsed();
        let since_start = start.elapsed();

        if since_start < ANNOUNCE_BURST_DURATION {
            since_last >= ANNOUNCE_BURST_INTERVAL
        } else {
            since_last >= ANNOUNCE_STEADY_INTERVAL
        }
    }

    pub fn did_announce(&mut self) {
        self.last_announce = Some(Instant::now());
        self.announce_count += 1;
    }

    // ── Discovery ─────────────────────────────────────────────────────────────

    /// Handle an incoming announce. Ignores non-beacon announces.
    /// Extracts display name from the null-separated suffix.
    pub fn on_announce_received(&mut self, dest_hash: DestHash, app_data: &[u8]) {
        if !is_beacon_announce(app_data) {
            return;
        }

        let display_name = extract_display_name(app_data).map(str::to_owned);
        let now = Instant::now();

        if let Some(beacon) = self.beacons.get_mut(&dest_hash) {
            beacon.last_announce = now;
            beacon.reconnect_attempts = 0;
            if let Some(name) = display_name {
                beacon.display_name = Some(name);
            }
            if matches!(beacon.state, BeaconState::Disconnected | BeaconState::Failed) {
                beacon.state = BeaconState::Connecting;
            }
        } else {
            self.beacons.insert(dest_hash, Beacon {
                dest_hash,
                state: BeaconState::Discovered,
                display_name: display_name.clone(),
                last_announce: now,
                last_connected: None,
                reconnect_attempts: 0,
                latency_ms: None,
            });
            self.pending_events.push(LxmfEvent::BeaconDiscovered {
                dest_hash,
                app_data: app_data.to_vec(),
            });
        }
    }

    pub fn on_beacon_connected(&mut self, dest_hash: &DestHash) {
        if let Some(b) = self.beacons.get_mut(dest_hash) {
            b.state = BeaconState::Connected;
            b.last_connected = Some(Instant::now());
            b.reconnect_attempts = 0;
        }
    }

    pub fn on_beacon_disconnected(&mut self, dest_hash: &DestHash) {
        if let Some(b) = self.beacons.get_mut(dest_hash) {
            b.state = BeaconState::Disconnected;
        }
    }

    pub fn reconnect_delay(&self, dest_hash: &DestHash) -> Duration {
        let attempts = self.beacons.get(dest_hash)
            .map(|b| b.reconnect_attempts as usize)
            .unwrap_or(0);
        Duration::from_secs(BACKOFF_SCHEDULE[attempts.min(BACKOFF_SCHEDULE.len() - 1)])
    }

    pub fn on_reconnect_attempt(&mut self, dest_hash: &DestHash) {
        if let Some(b) = self.beacons.get_mut(dest_hash) {
            b.reconnect_attempts += 1;
            b.state = BeaconState::Connecting;
        }
    }

    pub fn connected_beacons(&self) -> Vec<DestHash> {
        self.beacons.values()
            .filter(|b| b.state == BeaconState::Connected)
            .map(|b| b.dest_hash)
            .collect()
    }

    pub fn all_beacons(&self) -> Vec<&Beacon> {
        self.beacons.values().collect()
    }

    pub fn is_beacon(&self, dest_hash: &DestHash) -> bool {
        self.beacons.contains_key(dest_hash)
    }

    pub fn beacon_count(&self) -> usize { self.beacons.len() }

    pub fn connected_count(&self) -> usize {
        self.beacons.values().filter(|b| b.state == BeaconState::Connected).count()
    }

    pub fn set_strategy(&mut self, s: DispatchStrategy) { self.strategy = s; }
    pub fn strategy(&self) -> DispatchStrategy { self.strategy }

    pub fn remove_beacon(&mut self, dest_hash: &DestHash) {
        self.beacons.remove(dest_hash);
    }

    pub fn drain_events(&mut self) -> Vec<LxmfEvent> {
        std::mem::take(&mut self.pending_events)
    }

    // ── RPC dispatch ──────────────────────────────────────────────────────────

    /// Queue a JSON-RPC call to `dest`. Returns correlation id.
    /// Payload is compressed before queuing.
    pub fn queue_rpc(
        &mut self,
        dest: DestHash,
        method: &str,
        params: serde_json::Value,
    ) -> u32 {
        let id = self.next_rpc_id;
        self.next_rpc_id = self.next_rpc_id.wrapping_add(1).max(1);

        let req = RpcRequest::new(id, method, params);
        let payload = compress_payload(&req.to_bytes());

        self.in_flight.insert(id, method.to_owned());
        self.pending_rpcs.push_back(PendingRpc {
            id,
            dest,
            method: method.to_owned(),
            payload,
        });
        id
    }

    /// Queue the same RPC call to ALL connected beacons (Race strategy).
    /// Returns (dest_hash, rpc_id) pairs.
    pub fn queue_rpc_broadcast(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Vec<(DestHash, u32)> {
        let dests = self.connected_beacons();
        dests.into_iter().map(|dest| {
            let id = self.queue_rpc(dest, method, params.clone());
            (dest, id)
        }).collect()
    }

    /// Drain staged calls for node.rs to transmit via `send_via_link`.
    pub fn drain_pending_rpcs(&mut self) -> Vec<PendingRpc> {
        self.pending_rpcs.drain(..).collect()
    }

    /// Call when raw bytes arrive from a known beacon source.
    /// Parses as JSON-RPC response, correlates by id.
    pub fn on_rpc_bytes(&mut self, data: &[u8]) -> Option<RpcResult> {
        let resp = parse_rpc_response(data).ok()?;
        let id = resp.id?;
        let method = self.in_flight.remove(&id)?;
        Some(RpcResult { id, method, result: resp.into_result() })
    }

    // ── Convenience Solana RPC builders ──────────────────────────────────────

    /// Queue `getLatestBlockhash` to all connected beacons (Race).
    pub fn request_latest_blockhash(&mut self) -> Vec<(DestHash, u32)> {
        self.queue_rpc_broadcast(
            "getLatestBlockhash",
            serde_json::json!([{"commitment": "confirmed"}]),
        )
    }

    /// Queue `getAccountInfo` to `dest` (for nonce account or token accounts).
    pub fn request_account_info(&mut self, dest: DestHash, pubkey_b58: &str) -> u32 {
        self.queue_rpc(dest, "getAccountInfo", serde_json::json!([
            pubkey_b58,
            {"encoding": "base64", "commitment": "confirmed"},
        ]))
    }

    /// Queue `sendTransaction` with an already-fully-signed base64 transaction.
    pub fn request_send_transaction(&mut self, dest: DestHash, tx_b64: &str) -> u32 {
        self.queue_rpc(dest, "sendTransaction", serde_json::json!([
            tx_b64,
            {
                "encoding": "base64",
                "skipPreflight": true,
                "preflightCommitment": "confirmed",
            },
        ]))
    }

    /// Queue `cosignTransaction` — beacon co-signs and submits the partial tx.
    ///
    /// `partial_tx_b64`: base64 tx with client sig in slot 0, beacon slot zeros.
    pub fn request_cosign_transaction(&mut self, dest: DestHash, partial_tx_b64: &str) -> u32 {
        self.queue_rpc(dest, "cosignTransaction", serde_json::json!([partial_tx_b64]))
    }

    /// Queue `getBalance` for a pubkey.
    pub fn request_get_balance(&mut self, dest: DestHash, pubkey_b58: &str) -> u32 {
        self.queue_rpc(dest, "getBalance", serde_json::json!([
            pubkey_b58,
            {"commitment": "confirmed"},
        ]))
    }

    // ── JSON serialization ────────────────────────────────────────────────────

    pub fn beacons_json(&self) -> String {
        let v: Vec<serde_json::Value> = self.beacons.values().map(|b| {
            serde_json::json!({
                "destHash": hex::encode(b.dest_hash),
                "state": match b.state {
                    BeaconState::Discovered   => "discovered",
                    BeaconState::Connecting   => "connecting",
                    BeaconState::Connected    => "connected",
                    BeaconState::Disconnected => "disconnected",
                    BeaconState::Failed       => "failed",
                },
                "displayName": b.display_name,
                "reconnectAttempts": b.reconnect_attempts,
                "latencyMs": b.latency_ms,
            })
        }).collect();
        serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string())
    }
}

impl Default for BeaconManager {
    fn default() -> Self { Self::new() }
}
