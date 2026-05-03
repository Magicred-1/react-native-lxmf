//! LxmfNode — full Reticulum node using rns-transport
//!
//! Mode 0: BLE only (embedded FFI)
//! Mode 3: Standard Reticulum TCP (rns-transport with real protocol)
//!
//! The rns-transport mode creates a proper Reticulum node that speaks the
//! real wire protocol, generates identity, sends announces, and is visible
//! to all other nodes on the network.

use std::sync::{Arc, Mutex, OnceLock};
use std::collections::{VecDeque, HashMap};

use log::{info, warn};
use serde_json;

use crate::beacon::BeaconManager;
use crate::ble_iface::BleInterface;
use crate::nus_iface::NusInterface;
use crate::store::MessageStore;

use rns_transport::transport::Transport;

/// Destination hash: 16 bytes identifying a Reticulum destination
pub type DestHash = [u8; 16];

/// Identity key: 32 bytes for the node's cryptographic identity
pub type IdentityKey = [u8; 32];

/// LXMF address: 16 bytes
pub type LxmfAddress = [u8; 16];

/// Events emitted to the native layer (Swift/Kotlin) for forwarding to JS
#[derive(Debug, Clone)]
pub enum LxmfEvent {
    StatusChanged { running: bool, lifecycle: u32 },
    PacketReceived { source: DestHash, data: Vec<u8> },
    TxReceived { data: Vec<u8> },
    BeaconDiscovered { dest_hash: DestHash, app_data: Vec<u8> },
    MessageReceived {
        source: LxmfAddress,
        title: Vec<u8>,
        body: Vec<u8>,
        image: Option<(String, Vec<u8>)>,
        files: Vec<(String, Vec<u8>)>,
        timestamp: u64,
        /// Set for group channel messages: the group destination address.
        /// JS routes the message to the group thread instead of a DM thread.
        group_dest: Option<LxmfAddress>,
    },
    AnnounceReceived { dest_hash: DestHash, app_data: Vec<u8>, hops: u8 },
    MessageQueued { seq: u64, dest_hex: String },
    MessageDelivered { seq: u64, dest_hex: String },
    MessageFailed { seq: u64, dest_hex: String, reason: String },
    Log { level: u32, message: String },
    Error { code: u32, message: String },
    /// Result of a JSON-RPC call dispatched through a beacon.
    RpcResponse { id: u32, method: String, result_json: String, is_error: bool },
}

pub type EventQueue = Arc<Mutex<VecDeque<LxmfEvent>>>;

/// Global singleton
static NODE: OnceLock<Mutex<Option<LxmfNode>>> = OnceLock::new();

/// Handle to the tokio runtime (one per process)
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn get_runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime")
    })
}

/// A message queued for opportunistic delivery: stored as a pre-signed LXMF payload,
/// retried automatically when the destination peer sends an announce.
struct PendingSend {
    seq: u64,
    dest: [u8; 16],
    lxmf_payload: Vec<u8>,
    store_id: Option<i64>,
}

/// The main LXMF node — wraps either embedded FFI or full rns-transport
pub struct LxmfNode {
    /// Event queue polled by native layer
    pub events: EventQueue,
    /// Beacon manager — Arc so async tasks can share it without the global lock.
    pub beacon_mgr: Arc<std::sync::Mutex<BeaconManager>>,
    /// Message persistence
    pub store: Option<Arc<MessageStore>>,
    /// Running state
    running: bool,
    /// Identity hex (for display)
    pub identity_hex: String,
    /// Address hex (for display)
    pub address_hex: String,
    /// The mode we started with
    mode: u32,
    /// Private identity bytes (64 bytes, persisted)
    identity_bytes: Option<Vec<u8>>,
    /// Reticulum transport handle (mode 3 only)
    transport: Option<Arc<tokio::sync::Mutex<Transport>>>,
    /// Runtime counters
    pub outbound_sent: u64,
    pub inbound_accepted: u64,
    pub announces_received: u64,
    pub messages_received: u64,
    /// Opportunistic send queue: messages awaiting a peer announce before delivery
    pending_sends: Arc<Mutex<Vec<PendingSend>>>,
    /// Peer identity cache: address_hash → DestinationDesc (populated from announces).
    /// Used to construct links for large payloads that exceed the 464B packet MTU.
    peer_identities: Arc<Mutex<HashMap<[u8; 16], rns_transport::destination::DestinationDesc>>>,
    /// JoinHandles for every task spawned during start_*. Aborted on stop()
    /// to prevent zombie tasks accumulating across Stop/Start cycles and mode
    /// switches (each leaked task fires its own timers and pollutes logs).
    task_handles: Vec<tokio::task::JoinHandle<()>>,
}

// Access through Mutex
unsafe impl Send for LxmfNode {}

impl LxmfNode {
    pub fn global() -> &'static Mutex<Option<LxmfNode>> {
        NODE.get_or_init(|| Mutex::new(None))
    }

    /// Initialize — create the node shell. Does not start networking yet.
    pub fn init(db_path: Option<&str>) -> Result<(), String> {
        // Install the log bridge so Rust info!/warn!/error! logs flow to the
        // native event queue and appear in the UI's Debug Logs section.
        crate::log_bridge::init_logger(log::LevelFilter::Debug);

        let store = db_path.map(|p| {
            MessageStore::open(p)
                .map_err(|e| format!("SQLite open failed: {e}"))
                .map(Arc::new)
        }).transpose()?;

        let node = LxmfNode {
            events: Arc::new(Mutex::new(VecDeque::with_capacity(256))),
            beacon_mgr: Arc::new(std::sync::Mutex::new(BeaconManager::new())),
            store,
            running: false,
            identity_hex: String::new(),
            address_hex: String::new(),
            mode: 0,
            identity_bytes: None,
            transport: None,
            outbound_sent: 0,
            inbound_accepted: 0,
            announces_received: 0,
            messages_received: 0,
            pending_sends: Arc::new(Mutex::new(Vec::new())),
            peer_identities: Arc::new(Mutex::new(HashMap::new())),
            task_handles: Vec::new(),
        };

        // Clear stale BLE peer list from any previous session in this process
        crate::ble_iface::clear_ble_peers();

        let mut guard = Self::global().lock().map_err(|e| e.to_string())?;
        *guard = Some(node);
        info!("LxmfNode: initialized");
        Ok(())
    }

    /// Start the node.
    /// mode 0: BLE only (embedded FFI)
    /// mode 3: Standard Reticulum TCP via rns-transport
    ///
    /// `interfaces_json` is a JSON array of `{"host": "...", "port": 1234}` objects.
    /// At least one entry is required for mode 3.
    pub fn start(
        identity_hex: &str,
        address_hex: &str,
        mode: u32,
        announce_interval_ms: u64,
        _ble_mtu_hint: u16,
        interfaces_json: &str,
        display_name: &str,
        is_beacon: bool,
    ) -> Result<(), String> {
        info!("LxmfNode::start mode={} interfaces={} name={} beacon={}", mode, interfaces_json, display_name, is_beacon);

        // BLE can't sustain frequent announce broadcasts — clamp to 60s minimum for
        // any mode that includes a BLE interface to avoid tx queue saturation.
        const BLE_MIN_ANNOUNCE_MS: u64 = 60_000;
        let announce_interval_ms = match mode {
            0 | 4 => announce_interval_ms.max(BLE_MIN_ANNOUNCE_MS),
            _ => announce_interval_ms,
        };

        match mode {
            3 => {
                let interfaces = parse_interfaces_json(interfaces_json)?;
                Self::start_reticulum(identity_hex, &interfaces, announce_interval_ms, display_name, is_beacon)
            }
            0 => Self::start_ble(identity_hex, address_hex, display_name, is_beacon),
            4 => {
                let interfaces = parse_interfaces_json(interfaces_json)?;
                Self::start_full(identity_hex, &interfaces, announce_interval_ms, display_name, is_beacon)
            }
            _ => Err(format!("Unsupported mode: {}. Use 0 (BLE), 3 (TCP), or 4 (TCP+BLE)", mode)),
        }
    }

    /// Start with full Reticulum transport (mode 3)
    fn start_reticulum(
        identity_hex: &str,
        interfaces: &[(String, u16)],
        announce_interval_ms: u64,
        display_name: &str,
        is_beacon: bool,
    ) -> Result<(), String> {
        use rns_transport::identity::PrivateIdentity;
        use rns_transport::transport::TransportConfig;
        use rns_transport::destination::DestinationName;
        use rns_transport::iface::tcp_client::TcpClient;

        if interfaces.is_empty() {
            return Err("Reticulum TCP mode requires at least one interface".into());
        }

        // Create or restore identity
        let private_identity = if identity_hex.len() == 128 {
            // 64 bytes = full private key
            PrivateIdentity::new_from_hex_string(identity_hex)
                .map_err(|e| format!("Invalid identity hex: {:?}", e))?
        } else {
            // Generate new identity
            info!("LxmfNode: generating new identity");
            PrivateIdentity::new_from_rand(rand_core::OsRng)
        };

        let id_hex = private_identity.to_hex_string();
        // addr_hex is set later from my_dest.desc.address_hash (LXMF delivery destination hash)

        info!("LxmfNode: identity addr={}", hex::encode(private_identity.address_hash().as_slice()));

        // Store identity bytes for persistence
        let id_bytes = private_identity.to_private_key_bytes().to_vec();

        // Get event queue handle
        let events = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            Arc::clone(&node.events)
        };

        let rt = get_runtime();

        let name_bytes: Vec<u8> = build_app_data(display_name, is_beacon);

        // Set up transport synchronously so we can store the handle
        let name_bytes_init = name_bytes.clone();
        let (transport_arc, my_dest, mut data_rx, mut resource_rx, announce_rx, lxmf_addr_hex) = rt.block_on(async move {
            let config = TransportConfig::new("lxmf-mobile", &private_identity, true);
            let mut transport = Transport::new(config);

            // Add all TCP interfaces
            {
                let iface_mgr = transport.iface_manager();
                let mut mgr = iface_mgr.lock().await;
                for (host, port) in interfaces {
                    let addr = format!("{}:{}", host, port);
                    info!("LxmfNode: connecting TCP to {}", addr);
                    mgr.spawn(TcpClient::new(&addr), TcpClient::spawn);
                }
            }

            // Register LXMF delivery destination
            let dest_name = DestinationName::new("lxmf", "delivery");
            let my_dest = transport.add_destination(private_identity.clone(), dest_name).await;

            // Extract the LXMF delivery destination hash while we still have async context.
            // This is Hash(name_hash + identity_address_hash) — NOT the raw identity hash.
            // Peers must send to this address, and we embed it as source in outgoing messages.
            let lxmf_addr_hex = {
                let d = my_dest.lock().await;
                hex::encode(d.desc.address_hash.as_slice())
            };

            // Give the TCP interface time to connect
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            // Send initial announce with display name as app_data
            info!("LxmfNode: sending announce as {}", lxmf_addr_hex);
            transport.send_announce(&my_dest, Some(name_bytes_init.as_slice())).await;

            // Subscribe to received_data_events() rather than in_link_events().
            // received_data_events() is the wider stream — it surfaces both
            // single-shot destination-encrypted packets (what send_to produces)
            // AND link-carried data. in_link_events() only fires for packets
            // inside an established Reticulum Link, so direct unicast was being
            // silently dropped from the user-visible event stream.
            let data_rx = transport.received_data_events();
            let resource_rx = transport.resource_events();
            let announce_rx = transport.recv_announces().await;

            let arc = Arc::new(tokio::sync::Mutex::new(transport));
            (arc, my_dest, data_rx, resource_rx, announce_rx, lxmf_addr_hex)
        });

        let addr_hex = lxmf_addr_hex;

        // Push status event
        if let Ok(mut eq) = events.lock() {
            eq.push_back(LxmfEvent::StatusChanged { running: true, lifecycle: 3 });
        }

        // Grab the pending-send queue so the announce task can flush it
        let (pending_for_ann, peer_ids_arc) = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            (Arc::clone(&node.pending_sends), Arc::clone(&node.peer_identities))
        };
        let beacon_arc = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            Arc::clone(&node.beacon_mgr)
        };
        let transport_for_ann = Arc::clone(&transport_arc);

        let store_arc: Option<Arc<MessageStore>> = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            node.store.clone()
        };
        if let Some(s) = &store_arc {
            if let Ok(rows) = s.all_outbound_queue() {
                let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                let existing_seqs: std::collections::HashSet<u64> = q.iter().map(|p| p.seq).collect();
                for (id, seq, dest, payload) in rows {
                    if !existing_seqs.contains(&seq) {
                        q.push(PendingSend { seq, dest, lxmf_payload: payload, store_id: Some(id) });
                    }
                }
            }
        }

        // Collect JoinHandles so stop() can abort every spawned task and
        // prevent zombie task accumulation across Stop/Start cycles.
        let mut task_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        // Spawn announce receiver
        let events_ann = Arc::clone(&events);
        let store_ann = store_arc.clone();
        let pending_for_ann_task = Arc::clone(&pending_for_ann);
        let peer_ids_ann = Arc::clone(&peer_ids_arc);
        let beacon_arc_ann = Arc::clone(&beacon_arc);
        let mut announce_rx = announce_rx;
        task_handles.push(rt.spawn(async move {
            let pending_for_ann = pending_for_ann_task;
            loop {
                match announce_rx.recv().await {
                    Ok(event) => {
                        let dest = event.destination.lock().await;
                        let desc = dest.desc;
                        let hash_bytes = desc.address_hash;
                        let mut dh = [0u8; 16];
                        dh.copy_from_slice(hash_bytes.as_slice());
                        let app_data = event.app_data.as_slice().to_vec();
                        info!("LxmfNode: announce from {} ({} hops)", hex::encode(&dh), event.hops);
                        drop(dest);

                        // Cache peer identity for large-payload link sends
                        if let Ok(mut ids) = peer_ids_ann.lock() {
                            ids.insert(dh, desc);
                        }

                        // Track beacon announces for RPC dispatch
                        if let Ok(mut mgr) = beacon_arc_ann.lock() {
                            mgr.on_announce_received(dh, &app_data);
                        }

                        // Flush any queued opportunistic sends for this peer
                        let to_retry: Vec<(u64, Option<i64>, Vec<u8>)> = {
                            let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                            let (matched, rest): (Vec<_>, Vec<_>) =
                                q.drain(..).partition(|s| s.dest == dh);
                            *q = rest;
                            matched.into_iter().map(|s| (s.seq, s.store_id, s.lxmf_payload)).collect()
                        };

                        if !to_retry.is_empty() {
                            use rns_transport::hash::AddressHash;
                            use rns_transport::packet::{Packet, PacketDataBuffer};
                            use rns_transport::transport::SendPacketOutcome;
                            use rns_transport::delivery::send_via_link;
                            let transport = transport_for_ann.lock().await;
                            for (seq, store_id, payload) in &to_retry {
                                let mut dest_arr = [0u8; 16];
                                dest_arr.copy_from_slice(&payload[..16]);
                                let packet = Packet {
                                    destination: AddressHash::new(dest_arr),
                                    data: PacketDataBuffer::new_from_slice(payload),
                                    ..Default::default()
                                };
                                let outcome = transport.send_packet_with_outcome(packet).await;
                                info!("LxmfNode: opportunistic retry seq={} -> {:?}", seq, outcome);
                                let delivered = match outcome {
                                    SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => true,
                                    SendPacketOutcome::DroppedCiphertextTooLarge => {
                                        // Large payload — attempt link + resource transfer
                                        match send_via_link(&transport, desc, payload, std::time::Duration::from_secs(15)).await {
                                            Ok(_) => { info!("LxmfNode: opportunistic link-send seq={} ok", seq); true }
                                            Err(e) => { warn!("LxmfNode: opportunistic link-send seq={} failed: {e}", seq); false }
                                        }
                                    }
                                    _ => false,
                                };
                                if delivered {
                                    if let Some(id) = store_id {
                                        if let Some(s) = &store_ann { let _ = s.remove_outbound(*id); }
                                    }
                                    if let Ok(mut eq) = events_ann.lock() {
                                        eq.push_back(LxmfEvent::MessageDelivered { seq: *seq, dest_hex: hex::encode(&dh) });
                                    }
                                } else {
                                    let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                                    q.push(PendingSend { seq: *seq, dest: dh, lxmf_payload: payload.clone(), store_id: *store_id });
                                }
                            }
                        }

                        if let Ok(mut eq) = events_ann.lock() {
                            eq.push_back(LxmfEvent::AnnounceReceived {
                                dest_hash: dh,
                                app_data,
                                hops: event.hops,
                            });
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode: lagged {} announce events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Spawn data receiver — consumes received_data_events() (single-shot
        // destination-encrypted packets + link-carried data, see subscription
        // comment above). Mirrors the BLE-mode receiver shape.
        let events_data = Arc::clone(&events);
        let store_data = store_arc.clone();
        let transport_data = Arc::clone(&transport_arc);
        let peer_ids_verify = Arc::clone(&peer_ids_arc);
        let beacon_arc_data = Arc::clone(&beacon_arc);
        task_handles.push(rt.spawn(async move {
            loop {
                match data_rx.recv().await {
                    Ok(received) => {
                        let data = received.data.as_slice().to_vec();
                        // Source = LXMF sender address (wire bytes 16..32), not the transport destination.
                        let src = if data.len() >= 32 {
                            let mut s = [0u8; 16]; s.copy_from_slice(&data[16..32]); s
                        } else { [0u8; 16] };
                        info!("LxmfNode: received {} bytes from {}", data.len(), hex::encode(&src));

                        // Beacon RPC response: JSON or compressed, not LXMF-framed.
                        if looks_like_rpc_response(&data) {
                            if let Ok(mut mgr) = beacon_arc_data.lock() {
                                if let Some(result) = mgr.on_rpc_bytes(&data) {
                                    let is_error = result.result.is_err();
                                    let result_json = match result.result {
                                        Ok(v)  => v.to_string(),
                                        Err(e) => serde_json::json!({"code": e.code, "message": e.message}).to_string(),
                                    };
                                    if let Ok(mut eq) = events_data.lock() {
                                        if eq.len() < 1024 {
                                            eq.push_back(LxmfEvent::RpcResponse {
                                                id: result.id, method: result.method, result_json, is_error,
                                            });
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        // Fire request_path unconditionally so the sender re-announces
                        // and their identity enters peer_identities. Critical for the
                        // Sideband cold-start case: we may receive a message before their
                        // announce has reached us, and without this we'd never trigger
                        // a re-announce and every message would be silently dropped.
                        if src != [0u8; 16] {
                            let t = Arc::clone(&transport_data);
                            let sender = src;
                            tokio::spawn(async move {
                                use rns_transport::hash::AddressHash;
                                t.lock().await.request_path(&AddressHash::new(sender), None, None).await;
                            });
                        }

                        // Verify Ed25519 LXMF signature.
                        let sig_result = {
                            let ids = peer_ids_verify.lock().unwrap_or_else(|p| p.into_inner());
                            verify_lxmf_signature(&data, &ids)
                        };
                        match sig_result {
                            Some(true) => {}
                            Some(false) => {
                                warn!("LxmfNode: invalid LXMF signature from {}, dropping",
                                    hex::encode(&src));
                                continue;
                            }
                            None => {
                                // Identity not cached yet — accept anyway per LXMF spec.
                                // Sender identity is embedded in the packet; request_path
                                // already fired so the peer will re-announce and future
                                // messages will be signature-verified.
                                warn!("LxmfNode: accepting message from unknown peer {} (unverified, path requested)",
                                    hex::encode(&src));
                            }
                        }
                        let event = lxmf_event_from_bytes(src, data, None);
                        persist_inbound_message(&store_data, &event);
                        if let Ok(mut eq) = events_data.lock() {
                            if eq.len() < 1024 {
                                eq.push_back(event);
                            } else {
                                warn!("LxmfNode: event queue full, dropping inbound message");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode: lagged {} data events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Spawn resource receiver — handles large messages (>MTU) delivered via resource transfer
        let events_res = Arc::clone(&events);
        let store_res = store_arc.clone();
        task_handles.push(rt.spawn(async move {
            use rns_transport::resource::ResourceEventKind;
            loop {
                match resource_rx.recv().await {
                    Ok(event) => {
                        if let ResourceEventKind::Complete(complete) = event.kind {
                            let data = complete.data;
                            let src = if data.len() >= 32 {
                                let mut s = [0u8; 16]; s.copy_from_slice(&data[16..32]); s
                            } else { [0u8; 16] };
                            info!("LxmfNode: resource complete {} bytes from {}", data.len(), hex::encode(&src));
                            let lxmf_event = lxmf_event_from_bytes(src, data, None);
                            persist_inbound_message(&store_res, &lxmf_event);
                            if let Ok(mut eq) = events_res.lock() {
                                eq.push_back(lxmf_event);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode: lagged {} resource events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Beacon RPC dispatch — drains queued calls and sends via Reticulum links.
        let beacon_arc_rpc = Arc::clone(&beacon_arc);
        let peer_ids_rpc   = Arc::clone(&peer_ids_arc);
        let transport_rpc  = Arc::clone(&transport_arc);
        let events_rpc     = Arc::clone(&events);
        task_handles.push(rt.spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let calls = match beacon_arc_rpc.lock() {
                    Ok(mut m) => m.drain_pending_rpcs(),
                    Err(_) => continue,
                };
                if calls.is_empty() { continue; }
                use rns_transport::delivery::send_via_link;
                let transport = transport_rpc.lock().await;
                for rpc in calls {
                    let desc = peer_ids_rpc.lock().ok().and_then(|ids| ids.get(&rpc.dest).copied());
                    let Some(desc) = desc else {
                        warn!("LxmfNode: RPC id={} {}: no route to beacon {}", rpc.id, rpc.method, hex::encode(&rpc.dest));
                        if let Ok(mut eq) = events_rpc.lock() {
                            if eq.len() < 1024 {
                                eq.push_back(LxmfEvent::RpcResponse {
                                    id: rpc.id, method: rpc.method,
                                    result_json: r#"{"code":-32001,"message":"No route to beacon"}"#.to_owned(),
                                    is_error: true,
                                });
                            }
                        }
                        continue;
                    };
                    match send_via_link(&transport, desc, &rpc.payload, std::time::Duration::from_secs(30)).await {
                        Ok(_) => info!("LxmfNode: RPC id={} {} sent", rpc.id, rpc.method),
                        Err(e) => {
                            warn!("LxmfNode: RPC id={} {} failed: {}", rpc.id, rpc.method, e);
                            if let Ok(mut eq) = events_rpc.lock() {
                                if eq.len() < 1024 {
                                    eq.push_back(LxmfEvent::RpcResponse {
                                        id: rpc.id, method: rpc.method,
                                        result_json: format!(r#"{{"code":-32000,"message":"Send failed: {e}"}}"#),
                                        is_error: true,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }));

        // Spawn periodic re-announce
        let transport_reannounce = Arc::clone(&transport_arc);
        let interval_ms = if announce_interval_ms > 0 { announce_interval_ms } else { 300_000 };
        task_handles.push(rt.spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(interval_ms)).await;
                info!("LxmfNode: periodic re-announce");
                transport_reannounce
                    .lock()
                    .await
                    .send_announce(&my_dest, Some(name_bytes.as_slice()))
                    .await;
            }
        }));

        // Periodic retry task (every 60s)
        let transport_retry = Arc::clone(&transport_arc);
        let pending_retry = Arc::clone(&pending_for_ann);
        let store_retry = store_arc.clone();
        let events_retry = Arc::clone(&events);
        let peer_ids_retry = Arc::clone(&peer_ids_arc);
        task_handles.push(rt.spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                if let Some(s) = &store_retry {
                    if let Ok(expired) = s.drain_expired_outbound(50) {
                        for (_, seq, dest) in expired {
                            warn!("LxmfNode: giving up on seq={} after 50 attempts", seq);
                            if let Ok(mut eq) = events_retry.lock() {
                                eq.push_back(LxmfEvent::MessageFailed {
                                    seq,
                                    dest_hex: hex::encode(&dest),
                                    reason: "max attempts reached".to_string(),
                                });
                            }
                        }
                    }
                }
                let snapshot: Vec<(u64, Option<i64>, Vec<u8>, [u8; 16])> = {
                    let q = pending_retry.lock().unwrap_or_else(|p| p.into_inner());
                    q.iter().map(|s| (s.seq, s.store_id, s.lxmf_payload.clone(), s.dest)).collect()
                };
                if !snapshot.is_empty() {
                    use rns_transport::hash::AddressHash;
                    use rns_transport::packet::{Packet, PacketDataBuffer};
                    use rns_transport::transport::SendPacketOutcome;
                    use rns_transport::delivery::send_via_link;
                    let transport = transport_retry.lock().await;
                    for (seq, store_id, payload, dest) in snapshot {
                        let packet = Packet {
                            destination: AddressHash::new(dest),
                            data: PacketDataBuffer::new_from_slice(&payload),
                            ..Default::default()
                        };
                        let outcome = transport.send_packet_with_outcome(packet).await;
                        let delivered = match outcome {
                            SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => true,
                            SendPacketOutcome::DroppedCiphertextTooLarge => {
                                let cached_desc = peer_ids_retry.lock().ok().and_then(|ids| ids.get(&dest).copied());
                                match cached_desc {
                                    Some(desc) => match send_via_link(&transport, desc, &payload, std::time::Duration::from_secs(15)).await {
                                        Ok(_) => { info!("LxmfNode: periodic link-send seq={} ok", seq); true }
                                        Err(e) => { warn!("LxmfNode: periodic link-send seq={} failed: {e}", seq); false }
                                    },
                                    None => false,
                                }
                            }
                            _ => false,
                        };
                        if delivered {
                            {
                                let mut q = pending_retry.lock().unwrap_or_else(|p| p.into_inner());
                                q.retain(|s| s.seq != seq);
                            }
                            if let Some(id) = store_id {
                                if let Some(s) = &store_retry { let _ = s.remove_outbound(id); }
                            }
                            if let Ok(mut eq) = events_retry.lock() {
                                eq.push_back(LxmfEvent::MessageDelivered { seq, dest_hex: hex::encode(&dest) });
                            }
                        } else {
                            if let Some(id) = store_id {
                                if let Some(s) = &store_retry { let _ = s.bump_outbound_attempts(id); }
                            }
                        }
                    }
                }
            }
        }));

        info!("LxmfNode: LXMF delivery address = {}", addr_hex);

        // Spawn group channel receiver — intercepts raw GROUP-type packets for joined groups.
        let events_group = Arc::clone(&events);
        let store_group = store_arc.clone();
        let group_iface_rx = rt.block_on(async { transport_arc.lock().await.iface_rx() });
        task_handles.push(rt.spawn(async move {
            let mut rx = group_iface_rx;
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        // Match on destination address — don't gate on DestinationType::Group
                        // because the wire type bit may differ from what we set in send_packet.
                        let Ok(dest): Result<[u8; 16], _> = msg.packet.destination.as_slice().try_into() else { continue };
                        let Some(key) = crate::group::lookup_key(&dest) else { continue };
                        let raw = msg.packet.data.as_slice();
                        match crate::group::group_decrypt(&key, raw) {
                            Ok(dec) if dec.len() >= 97 => {
                                let mut src = [0u8; 16];
                                src.copy_from_slice(&dec[16..32]);
                                let event = lxmf_event_from_bytes(src, dec.clone(), Some(dest));
                                persist_inbound_message(&store_group, &event);
                                if let Ok(mut eq) = events_group.lock() {
                                    if eq.len() < 1024 { eq.push_back(event); }
                                }
                            }
                            Ok(_) => warn!("Group RX: decrypted payload too short"),
                            Err(e) => warn!("Group RX: decrypt failed for {}: {:?}", hex::encode(&dest), e),
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode Group RX: lagged {} events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Update node state
        let mut guard = Self::global().lock().map_err(|e| e.to_string())?;
        let node = guard.as_mut().ok_or("Node not initialized")?;
        node.running = true;
        node.mode = 3;
        node.identity_hex = id_hex;
        node.address_hex = addr_hex;
        node.identity_bytes = Some(id_bytes);
        node.transport = Some(transport_arc);
        node.task_handles = task_handles;

        info!("LxmfNode: Reticulum transport started");
        Ok(())
    }

    /// Start with full Reticulum TCP + BLE transport simultaneously (mode 4).
    ///
    /// Identical to start_reticulum except:
    /// - BleInterface and NusInterface are also registered on the same Transport
    /// - 200ms BLE poll warmup after the 2s TCP connect delay
    /// - Two extra tasks: event-driven re-announce on BLE peer connect, and
    ///   initial 5s post-connect re-announce followed by a 300s steady loop.
    fn start_full(
        identity_hex: &str,
        interfaces: &[(String, u16)],
        announce_interval_ms: u64,
        display_name: &str,
        is_beacon: bool,
    ) -> Result<(), String> {
        use rns_transport::identity::PrivateIdentity;
        use rns_transport::transport::TransportConfig;
        use rns_transport::destination::DestinationName;
        use rns_transport::iface::tcp_client::TcpClient;

        if interfaces.is_empty() {
            return Err("Mode 4 (TCP+BLE) requires at least one TCP interface".into());
        }

        let private_identity = if identity_hex.len() == 128 {
            PrivateIdentity::new_from_hex_string(identity_hex)
                .map_err(|e| format!("Invalid identity hex: {:?}", e))?
        } else {
            info!("LxmfNode full: generating new identity");
            PrivateIdentity::new_from_rand(rand_core::OsRng)
        };

        let id_hex = private_identity.to_hex_string();
        info!("LxmfNode full: identity addr={}", hex::encode(private_identity.address_hash().as_slice()));
        let id_bytes = private_identity.to_private_key_bytes().to_vec();

        let events = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            Arc::clone(&node.events)
        };

        let rt = get_runtime();

        let name_bytes: Vec<u8> = build_app_data(display_name, is_beacon);

        let name_bytes_init = name_bytes.clone();
        let (transport_arc, my_dest, mut data_rx, mut resource_rx, announce_rx, lxmf_addr_hex) = rt.block_on(async move {
            let config = TransportConfig::new("lxmf-mobile", &private_identity, true);
            let mut transport = Transport::new(config);

            {
                let iface_mgr = transport.iface_manager();
                let mut mgr = iface_mgr.lock().await;
                for (host, port) in interfaces {
                    let addr = format!("{}:{}", host, port);
                    info!("LxmfNode full: connecting TCP to {}", addr);
                    mgr.spawn(TcpClient::new(&addr), TcpClient::spawn);
                }
                // BLE interfaces on the same transport instance
                mgr.spawn(BleInterface::new(), BleInterface::spawn);
                mgr.spawn(NusInterface::new(), NusInterface::spawn);
            }

            let dest_name = DestinationName::new("lxmf", "delivery");
            let my_dest = transport.add_destination(private_identity.clone(), dest_name).await;

            let lxmf_addr_hex = {
                let d = my_dest.lock().await;
                hex::encode(d.desc.address_hash.as_slice())
            };

            // TCP connect delay, then BLE poll warmup
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

            info!("LxmfNode full: sending announce as {}", lxmf_addr_hex);
            transport.send_announce(&my_dest, Some(name_bytes_init.as_slice())).await;

            let data_rx = transport.received_data_events();
            let resource_rx = transport.resource_events();
            let announce_rx = transport.recv_announces().await;

            let arc = Arc::new(tokio::sync::Mutex::new(transport));
            (arc, my_dest, data_rx, resource_rx, announce_rx, lxmf_addr_hex)
        });

        let addr_hex = lxmf_addr_hex;

        if let Ok(mut eq) = events.lock() {
            eq.push_back(LxmfEvent::StatusChanged { running: true, lifecycle: 4 });
        }

        let (pending_for_ann, peer_ids_arc) = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            (Arc::clone(&node.pending_sends), Arc::clone(&node.peer_identities))
        };
        let beacon_arc = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            Arc::clone(&node.beacon_mgr)
        };
        let transport_for_ann = Arc::clone(&transport_arc);

        let store_arc: Option<Arc<MessageStore>> = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            node.store.clone()
        };
        if let Some(s) = &store_arc {
            if let Ok(rows) = s.all_outbound_queue() {
                let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                let existing_seqs: std::collections::HashSet<u64> = q.iter().map(|p| p.seq).collect();
                for (id, seq, dest, payload) in rows {
                    if !existing_seqs.contains(&seq) {
                        q.push(PendingSend { seq, dest, lxmf_payload: payload, store_id: Some(id) });
                    }
                }
            }
        }

        let mut task_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        // Announce receiver (identical to mode 3, with opportunistic flush)
        let events_ann = Arc::clone(&events);
        let store_ann = store_arc.clone();
        let pending_for_ann_task = Arc::clone(&pending_for_ann);
        let peer_ids_ann = Arc::clone(&peer_ids_arc);
        let beacon_arc_ann = Arc::clone(&beacon_arc);
        let mut announce_rx = announce_rx;
        task_handles.push(rt.spawn(async move {
            let pending_for_ann = pending_for_ann_task;
            loop {
                match announce_rx.recv().await {
                    Ok(event) => {
                        let dest = event.destination.lock().await;
                        let desc = dest.desc;
                        let hash_bytes = desc.address_hash;
                        let mut dh = [0u8; 16];
                        dh.copy_from_slice(hash_bytes.as_slice());
                        let app_data = event.app_data.as_slice().to_vec();
                        info!("LxmfNode full: announce from {} ({} hops)", hex::encode(&dh), event.hops);
                        drop(dest);

                        // Cache peer identity for large-payload link sends
                        if let Ok(mut ids) = peer_ids_ann.lock() {
                            ids.insert(dh, desc);
                        }

                        // Track beacon announces for RPC dispatch
                        if let Ok(mut mgr) = beacon_arc_ann.lock() {
                            mgr.on_announce_received(dh, &app_data);
                        }

                        let to_retry: Vec<(u64, Option<i64>, Vec<u8>)> = {
                            let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                            let (matched, rest): (Vec<_>, Vec<_>) =
                                q.drain(..).partition(|s| s.dest == dh);
                            *q = rest;
                            matched.into_iter().map(|s| (s.seq, s.store_id, s.lxmf_payload)).collect()
                        };

                        if !to_retry.is_empty() {
                            use rns_transport::hash::AddressHash;
                            use rns_transport::packet::{Packet, PacketDataBuffer};
                            use rns_transport::transport::SendPacketOutcome;
                            use rns_transport::delivery::send_via_link;
                            let transport = transport_for_ann.lock().await;
                            for (seq, store_id, payload) in &to_retry {
                                let mut dest_arr = [0u8; 16];
                                dest_arr.copy_from_slice(&payload[..16]);
                                let packet = Packet {
                                    destination: AddressHash::new(dest_arr),
                                    data: PacketDataBuffer::new_from_slice(payload),
                                    ..Default::default()
                                };
                                let outcome = transport.send_packet_with_outcome(packet).await;
                                info!("LxmfNode full: opportunistic retry seq={} -> {:?}", seq, outcome);
                                let delivered = match outcome {
                                    SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => true,
                                    SendPacketOutcome::DroppedCiphertextTooLarge => {
                                        match send_via_link(&transport, desc, payload, std::time::Duration::from_secs(15)).await {
                                            Ok(_) => { info!("LxmfNode full: opportunistic link-send seq={} ok", seq); true }
                                            Err(e) => { warn!("LxmfNode full: opportunistic link-send seq={} failed: {e}", seq); false }
                                        }
                                    }
                                    _ => false,
                                };
                                if delivered {
                                    if let Some(id) = store_id {
                                        if let Some(s) = &store_ann { let _ = s.remove_outbound(*id); }
                                    }
                                    if let Ok(mut eq) = events_ann.lock() {
                                        eq.push_back(LxmfEvent::MessageDelivered { seq: *seq, dest_hex: hex::encode(&dh) });
                                    }
                                } else {
                                    let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                                    q.push(PendingSend { seq: *seq, dest: dh, lxmf_payload: payload.clone(), store_id: *store_id });
                                }
                            }
                        }

                        if let Ok(mut eq) = events_ann.lock() {
                            eq.push_back(LxmfEvent::AnnounceReceived { dest_hash: dh, app_data, hops: event.hops });
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode full: lagged {} announce events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Data receiver
        let events_data = Arc::clone(&events);
        let store_data = store_arc.clone();
        let transport_data = Arc::clone(&transport_arc);
        let peer_ids_verify = Arc::clone(&peer_ids_arc);
        let beacon_arc_data = Arc::clone(&beacon_arc);
        task_handles.push(rt.spawn(async move {
            loop {
                match data_rx.recv().await {
                    Ok(received) => {
                        let data = received.data.as_slice().to_vec();
                        let src = if data.len() >= 32 {
                            let mut s = [0u8; 16]; s.copy_from_slice(&data[16..32]); s
                        } else { [0u8; 16] };
                        info!("LxmfNode full: received {} bytes from {}", data.len(), hex::encode(&src));

                        // Beacon RPC response: JSON or compressed, not LXMF-framed.
                        if looks_like_rpc_response(&data) {
                            if let Ok(mut mgr) = beacon_arc_data.lock() {
                                if let Some(result) = mgr.on_rpc_bytes(&data) {
                                    let is_error = result.result.is_err();
                                    let result_json = match result.result {
                                        Ok(v)  => v.to_string(),
                                        Err(e) => serde_json::json!({"code": e.code, "message": e.message}).to_string(),
                                    };
                                    if let Ok(mut eq) = events_data.lock() {
                                        if eq.len() < 1024 {
                                            eq.push_back(LxmfEvent::RpcResponse {
                                                id: result.id, method: result.method, result_json, is_error,
                                            });
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        if src != [0u8; 16] {
                            let t = Arc::clone(&transport_data);
                            let sender = src;
                            tokio::spawn(async move {
                                use rns_transport::hash::AddressHash;
                                t.lock().await.request_path(&AddressHash::new(sender), None, None).await;
                            });
                        }

                        let sig_result = {
                            let ids = peer_ids_verify.lock().unwrap_or_else(|p| p.into_inner());
                            verify_lxmf_signature(&data, &ids)
                        };
                        match sig_result {
                            Some(true) => {}
                            Some(false) => {
                                warn!("LxmfNode full: invalid LXMF signature from {}, dropping",
                                    hex::encode(&src));
                                continue;
                            }
                            None => {
                                warn!("LxmfNode full: accepting message from unknown peer {} (unverified, path requested)",
                                    hex::encode(&src));
                            }
                        }
                        let event = lxmf_event_from_bytes(src, data, None);
                        persist_inbound_message(&store_data, &event);
                        if let Ok(mut eq) = events_data.lock() {
                            if eq.len() < 1024 {
                                eq.push_back(event);
                            } else {
                                warn!("LxmfNode full: event queue full, dropping inbound message");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode full: lagged {} data events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Resource receiver (large messages)
        let events_res = Arc::clone(&events);
        let store_res = store_arc.clone();
        task_handles.push(rt.spawn(async move {
            use rns_transport::resource::ResourceEventKind;
            loop {
                match resource_rx.recv().await {
                    Ok(event) => {
                        if let ResourceEventKind::Complete(complete) = event.kind {
                            let data = complete.data;
                            let src = if data.len() >= 32 {
                                let mut s = [0u8; 16]; s.copy_from_slice(&data[16..32]); s
                            } else { [0u8; 16] };
                            info!("LxmfNode full: resource complete {} bytes from {}", data.len(), hex::encode(&src));
                            let lxmf_event = lxmf_event_from_bytes(src, data, None);
                            persist_inbound_message(&store_res, &lxmf_event);
                            if let Ok(mut eq) = events_res.lock() {
                                eq.push_back(lxmf_event);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode full: lagged {} resource events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Beacon RPC dispatch
        let beacon_arc_rpc = Arc::clone(&beacon_arc);
        let peer_ids_rpc   = Arc::clone(&peer_ids_arc);
        let transport_rpc  = Arc::clone(&transport_arc);
        let events_rpc     = Arc::clone(&events);
        task_handles.push(rt.spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let calls = match beacon_arc_rpc.lock() {
                    Ok(mut m) => m.drain_pending_rpcs(),
                    Err(_) => continue,
                };
                if calls.is_empty() { continue; }
                use rns_transport::delivery::send_via_link;
                let transport = transport_rpc.lock().await;
                for rpc in calls {
                    let desc = peer_ids_rpc.lock().ok().and_then(|ids| ids.get(&rpc.dest).copied());
                    let Some(desc) = desc else {
                        warn!("LxmfNode full: RPC id={} {}: no route to beacon {}", rpc.id, rpc.method, hex::encode(&rpc.dest));
                        if let Ok(mut eq) = events_rpc.lock() {
                            if eq.len() < 1024 {
                                eq.push_back(LxmfEvent::RpcResponse {
                                    id: rpc.id, method: rpc.method,
                                    result_json: r#"{"code":-32001,"message":"No route to beacon"}"#.to_owned(),
                                    is_error: true,
                                });
                            }
                        }
                        continue;
                    };
                    match send_via_link(&transport, desc, &rpc.payload, std::time::Duration::from_secs(30)).await {
                        Ok(_) => info!("LxmfNode full: RPC id={} {} sent", rpc.id, rpc.method),
                        Err(e) => {
                            warn!("LxmfNode full: RPC id={} {} failed: {}", rpc.id, rpc.method, e);
                            if let Ok(mut eq) = events_rpc.lock() {
                                if eq.len() < 1024 {
                                    eq.push_back(LxmfEvent::RpcResponse {
                                        id: rpc.id, method: rpc.method,
                                        result_json: format!(r#"{{"code":-32000,"message":"Send failed: {e}"}}"#),
                                        is_error: true,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }));

        // TCP periodic re-announce
        let transport_reannounce = Arc::clone(&transport_arc);
        let my_dest_tcp = Arc::clone(&my_dest);
        let name_bytes_tcp = name_bytes.clone();
        let interval_ms = if announce_interval_ms > 0 { announce_interval_ms } else { 300_000 };
        task_handles.push(rt.spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(interval_ms)).await;
                info!("LxmfNode full: TCP periodic re-announce");
                transport_reannounce.lock().await.send_announce(&my_dest_tcp, Some(name_bytes_tcp.as_slice())).await;
            }
        }));

        // BLE event-driven re-announce on peer connect
        let transport_pc = Arc::clone(&transport_arc);
        let my_dest_pc = Arc::clone(&my_dest);
        let name_bytes_pc = name_bytes.clone();
        let notify = crate::ble_iface::peer_connected_notify();
        task_handles.push(rt.spawn(async move {
            loop {
                notify.notified().await;
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                info!("LxmfNode full: re-announce on BLE peer connect");
                transport_pc.lock().await.send_announce(&my_dest_pc, Some(name_bytes_pc.as_slice())).await;
            }
        }));

        // BLE initial post-connect (5s) + steady 300s re-announce
        let transport_ble = Arc::clone(&transport_arc);
        let name_bytes_ble = name_bytes.clone();
        task_handles.push(rt.spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            info!("LxmfNode full: BLE initial post-connect re-announce");
            transport_ble.lock().await.send_announce(&my_dest, Some(name_bytes_ble.as_slice())).await;
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                info!("LxmfNode full: BLE periodic re-announce");
                transport_ble.lock().await.send_announce(&my_dest, Some(name_bytes_ble.as_slice())).await;
            }
        }));

        // Periodic retry task (every 60s)
        let transport_retry = Arc::clone(&transport_arc);
        let pending_retry = Arc::clone(&pending_for_ann);
        let store_retry = store_arc.clone();
        let events_retry = Arc::clone(&events);
        let peer_ids_retry = Arc::clone(&peer_ids_arc);
        task_handles.push(rt.spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                if let Some(s) = &store_retry {
                    if let Ok(expired) = s.drain_expired_outbound(50) {
                        for (_, seq, dest) in expired {
                            warn!("LxmfNode full: giving up on seq={} after 50 attempts", seq);
                            if let Ok(mut eq) = events_retry.lock() {
                                eq.push_back(LxmfEvent::MessageFailed {
                                    seq,
                                    dest_hex: hex::encode(&dest),
                                    reason: "max attempts reached".to_string(),
                                });
                            }
                        }
                    }
                }
                let snapshot: Vec<(u64, Option<i64>, Vec<u8>, [u8; 16])> = {
                    let q = pending_retry.lock().unwrap_or_else(|p| p.into_inner());
                    q.iter().map(|s| (s.seq, s.store_id, s.lxmf_payload.clone(), s.dest)).collect()
                };
                if !snapshot.is_empty() {
                    use rns_transport::hash::AddressHash;
                    use rns_transport::packet::{Packet, PacketDataBuffer};
                    use rns_transport::transport::SendPacketOutcome;
                    use rns_transport::delivery::send_via_link;
                    let transport = transport_retry.lock().await;
                    for (seq, store_id, payload, dest) in snapshot {
                        let packet = Packet {
                            destination: AddressHash::new(dest),
                            data: PacketDataBuffer::new_from_slice(&payload),
                            ..Default::default()
                        };
                        let outcome = transport.send_packet_with_outcome(packet).await;
                        let delivered = match outcome {
                            SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => true,
                            SendPacketOutcome::DroppedCiphertextTooLarge => {
                                let cached_desc = peer_ids_retry.lock().ok().and_then(|ids| ids.get(&dest).copied());
                                match cached_desc {
                                    Some(desc) => match send_via_link(&transport, desc, &payload, std::time::Duration::from_secs(15)).await {
                                        Ok(_) => { info!("LxmfNode full: periodic link-send seq={} ok", seq); true }
                                        Err(e) => { warn!("LxmfNode full: periodic link-send seq={} failed: {e}", seq); false }
                                    },
                                    None => false,
                                }
                            }
                            _ => false,
                        };
                        if delivered {
                            {
                                let mut q = pending_retry.lock().unwrap_or_else(|p| p.into_inner());
                                q.retain(|s| s.seq != seq);
                            }
                            if let Some(id) = store_id {
                                if let Some(s) = &store_retry { let _ = s.remove_outbound(id); }
                            }
                            if let Ok(mut eq) = events_retry.lock() {
                                eq.push_back(LxmfEvent::MessageDelivered { seq, dest_hex: hex::encode(&dest) });
                            }
                        } else {
                            if let Some(id) = store_id {
                                if let Some(s) = &store_retry { let _ = s.bump_outbound_attempts(id); }
                            }
                        }
                    }
                }
            }
        }));

        info!("LxmfNode full: TCP+BLE delivery address = {}", addr_hex);

        // Group channel receiver
        let events_group = Arc::clone(&events);
        let store_group = store_arc.clone();
        let group_iface_rx = rt.block_on(async { transport_arc.lock().await.iface_rx() });
        task_handles.push(rt.spawn(async move {
            let mut rx = group_iface_rx;
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        let Ok(dest): Result<[u8; 16], _> = msg.packet.destination.as_slice().try_into() else { continue };
                        let Some(key) = crate::group::lookup_key(&dest) else { continue };
                        let raw = msg.packet.data.as_slice();
                        match crate::group::group_decrypt(&key, raw) {
                            Ok(dec) if dec.len() >= 97 => {
                                let mut src = [0u8; 16];
                                src.copy_from_slice(&dec[16..32]);
                                let event = lxmf_event_from_bytes(src, dec.clone(), Some(dest));
                                persist_inbound_message(&store_group, &event);
                                if let Ok(mut eq) = events_group.lock() {
                                    if eq.len() < 1024 { eq.push_back(event); }
                                }
                            }
                            Ok(_) => warn!("Group RX full: decrypted payload too short"),
                            Err(e) => warn!("Group RX full: decrypt failed for {}: {:?}", hex::encode(&dest), e),
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode full Group RX: lagged {} events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        let mut guard = Self::global().lock().map_err(|e| e.to_string())?;
        let node = guard.as_mut().ok_or("Node not initialized")?;
        node.running = true;
        node.mode = 4;
        node.identity_hex = id_hex;
        node.address_hex = addr_hex;
        node.identity_bytes = Some(id_bytes);
        node.transport = Some(transport_arc);
        node.task_handles = task_handles;

        info!("LxmfNode: TCP+BLE transport started");
        Ok(())
    }

    /// Send an LXMF message to a destination.
    ///
    /// Encodes content as a proper LXMF wire message:
    ///   [16B dest_hash][16B source_hash][64B Ed25519 sig][msgpack payload]
    /// where msgpack payload = [timestamp: f64, title: bytes, content: bytes, fields: {}]
    ///
    /// The Reticulum transport then encrypts and routes the packet.
    /// Returns `Ok(seq)` on dispatch or opportunistic queue, `Err` on hard failure.
    /// `seq` is a monotonic counter starting at 0, unique per send attempt.
    pub fn send_to(dest_hex: &str, content: &[u8], media_json: Option<&str>) -> Result<u64, String> {
        use rns_transport::hash::AddressHash;
        use rns_transport::identity::PrivateIdentity;
        use rns_transport::packet::{Packet, PacketDataBuffer};
        use rns_transport::transport::SendPacketOutcome;

        let (transport, identity_bytes, source_hash_bytes, seq, pending_sends, events, store, peer_identities) = {
            let mut guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_mut().ok_or("Node not initialized")?;
            let transport = node.transport.clone().ok_or("Transport not started (mode 3 only)")?;
            let id_bytes = node.identity_bytes.clone().ok_or("No identity available")?;
            let addr_hex = node.address_hex.clone();
            let src = hex::decode(&addr_hex).map_err(|e| format!("Bad address hex: {e}"))?;
            let seq = node.outbound_sent;
            node.outbound_sent += 1;
            let pending = Arc::clone(&node.pending_sends);
            let events = Arc::clone(&node.events);
            let store = node.store.clone();
            let peer_ids = Arc::clone(&node.peer_identities);
            (transport, id_bytes, src, seq, pending, events, store, peer_ids)
        };

        let dest_bytes = hex::decode(dest_hex)
            .map_err(|e| format!("Invalid dest hex: {e}"))?;
        if dest_bytes.len() != 16 {
            return Err(format!("dest must be 16 bytes (32 hex chars), got {}", dest_bytes.len()));
        }
        if source_hash_bytes.len() != 16 {
            return Err(format!("source address must be 16 bytes, got {}", source_hash_bytes.len()));
        }

        let mut dest_arr = [0u8; 16];
        dest_arr.copy_from_slice(&dest_bytes);

        let private_identity = PrivateIdentity::from_private_key_bytes(&identity_bytes)
            .map_err(|e| format!("Failed to restore identity: {:?}", e))?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let fields_mp = build_fields_msgpack(media_json);
        let msgpack = encode_lxmf_msgpack(timestamp, b"", content, &fields_mp);

        let mut sign_data = Vec::with_capacity(16 + 16 + msgpack.len());
        sign_data.extend_from_slice(&dest_arr);
        sign_data.extend_from_slice(&source_hash_bytes);
        sign_data.extend_from_slice(&msgpack);
        let signature = private_identity.sign(&sign_data).to_bytes();

        let mut lxmf_payload = Vec::with_capacity(16 + 16 + 64 + msgpack.len());
        lxmf_payload.extend_from_slice(&dest_arr);
        lxmf_payload.extend_from_slice(&source_hash_bytes);
        lxmf_payload.extend_from_slice(&signature);
        lxmf_payload.extend_from_slice(&msgpack);

        info!("LxmfNode::send_to: seq={} dest={} payload={}B", seq, dest_hex, lxmf_payload.len());

        let packet = Packet {
            destination: AddressHash::new(dest_arr),
            data: PacketDataBuffer::new_from_slice(&lxmf_payload),
            ..Default::default()
        };

        let transport_for_link = Arc::clone(&transport);
        let outcome = get_runtime().block_on(async move {
            let transport = transport.lock().await;
            transport.send_packet_with_outcome(packet).await
        });

        match outcome {
            SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => {
                info!("LxmfNode::send_to: dispatched seq={} ({outcome:?})", seq);
                Ok(seq)
            }
            SendPacketOutcome::DroppedMissingDestinationIdentity => {
                warn!("LxmfNode::send_to: queued seq={} (no identity for {dest_hex})", seq);
                let store_id = store.as_ref().and_then(|s| {
                    s.enqueue_outbound(seq, &dest_arr, &lxmf_payload).ok()
                });
                if let Ok(mut q) = pending_sends.lock() {
                    if q.len() < 1000 {
                        q.push(PendingSend { seq, dest: dest_arr, lxmf_payload, store_id });
                    } else {
                        warn!("LxmfNode::send_to: pending_sends cap (1000) reached, dropping seq={}", seq);
                    }
                }
                if let Ok(mut eq) = events.lock() {
                    eq.push_back(LxmfEvent::MessageQueued { seq, dest_hex: dest_hex.to_string() });
                }
                // Trigger a Reticulum path request so the peer re-announces and
                // populates the transport's identity table — enabling the queued
                // message to be delivered at the next announce_rx event rather
                // than waiting for the peer's next periodic announce (up to 60s).
                let transport_path = Arc::clone(&transport_for_link);
                let dest_hex_owned = dest_hex.to_string();
                get_runtime().spawn(async move {
                    use rns_transport::hash::AddressHash;
                    info!("LxmfNode::send_to: sending path request for {}", dest_hex_owned);
                    transport_path.lock().await.request_path(
                        &AddressHash::new(dest_arr), None, None,
                    ).await;
                });
                Ok(seq)
            }
            SendPacketOutcome::DroppedNoRoute => {
                warn!("LxmfNode::send_to: queued seq={} (no route to {dest_hex})", seq);
                let store_id = store.as_ref().and_then(|s| {
                    s.enqueue_outbound(seq, &dest_arr, &lxmf_payload).ok()
                });
                if let Ok(mut q) = pending_sends.lock() {
                    if q.len() < 1000 {
                        q.push(PendingSend { seq, dest: dest_arr, lxmf_payload, store_id });
                    } else {
                        warn!("LxmfNode::send_to: pending_sends cap (1000) reached, dropping seq={}", seq);
                    }
                }
                if let Ok(mut eq) = events.lock() {
                    eq.push_back(LxmfEvent::MessageQueued { seq, dest_hex: dest_hex.to_string() });
                }
                Ok(seq)
            }
            SendPacketOutcome::DroppedCiphertextTooLarge => {
                // Large payload (e.g. image attachment). Try link + resource transfer.
                // Requires peer identity from announce cache; if absent, queue for retry.
                let cached_desc = peer_identities.lock().ok().and_then(|ids| ids.get(&dest_arr).copied());
                match cached_desc {
                    Some(desc) => {
                        use rns_transport::delivery::send_via_link;
                        info!("LxmfNode::send_to: seq={} large payload {}B — trying link send", seq, lxmf_payload.len());
                        let payload_for_link = lxmf_payload.clone();
                        let result = get_runtime().block_on(async move {
                            let transport = transport_for_link.lock().await;
                            send_via_link(&transport, desc, &payload_for_link, std::time::Duration::from_secs(15)).await
                        });
                        match result {
                            Ok(_) => {
                                info!("LxmfNode::send_to: link-send seq={} ok", seq);
                                Ok(seq)
                            }
                            Err(e) => {
                                warn!("LxmfNode::send_to: link-send seq={seq} failed: {e:?}, queuing");
                                let store_id = store.as_ref().and_then(|s| {
                                    s.enqueue_outbound(seq, &dest_arr, &lxmf_payload).ok()
                                });
                                if let Ok(mut q) = pending_sends.lock() {
                                    q.push(PendingSend { seq, dest: dest_arr, lxmf_payload, store_id });
                                }
                                if let Ok(mut eq) = events.lock() {
                                    eq.push_back(LxmfEvent::MessageQueued { seq, dest_hex: dest_hex.to_string() });
                                }
                                Ok(seq)
                            }
                        }
                    }
                    None => {
                        // No identity cached yet — queue; will retry via link when peer announces
                        warn!("LxmfNode::send_to: seq={} large payload, no identity cached for {dest_hex}, queuing", seq);
                        let store_id = store.as_ref().and_then(|s| {
                            s.enqueue_outbound(seq, &dest_arr, &lxmf_payload).ok()
                        });
                        if let Ok(mut q) = pending_sends.lock() {
                            q.push(PendingSend { seq, dest: dest_arr, lxmf_payload, store_id });
                        }
                        if let Ok(mut eq) = events.lock() {
                            eq.push_back(LxmfEvent::MessageQueued { seq, dest_hex: dest_hex.to_string() });
                        }
                        Ok(seq)
                    }
                }
            }
            SendPacketOutcome::DroppedEncryptFailed => {
                Err(format!("failed to encrypt packet for /{dest_hex}/"))
            }
        }
    }

    /// Create a new group channel and register it for inbound decryption.
    ///
    /// `name`    — human-readable group name; used to derive the deterministic group address.
    /// `key_hex` — 32 hex chars (16 bytes) shared AES key; all members must use the same key.
    ///
    /// Returns the group address hex (32 chars) that peers send messages to.
    pub fn create_group(name: &str, key_hex: &str) -> Result<String, String> {
        let key_bytes = hex::decode(key_hex)
            .map_err(|e| format!("Invalid key hex: {e}"))?;
        if key_bytes.len() != 16 {
            return Err(format!("key must be 16 bytes (32 hex chars), got {}", key_bytes.len()));
        }
        let mut key = [0u8; 16];
        key.copy_from_slice(&key_bytes);
        let addr = crate::group::group_address_hash(name);
        crate::group::register(addr, key);
        info!("Group: created/joined '{}' addr={}", name, hex::encode(&addr));
        Ok(hex::encode(&addr))
    }

    /// Join an existing group channel by its address and shared key.
    ///
    /// Use when you know the group address already (received from another member).
    pub fn join_group(group_addr_hex: &str, key_hex: &str) -> Result<(), String> {
        let addr_bytes = hex::decode(group_addr_hex)
            .map_err(|e| format!("Invalid addr hex: {e}"))?;
        if addr_bytes.len() != 16 {
            return Err(format!("group addr must be 16 bytes, got {}", addr_bytes.len()));
        }
        let key_bytes = hex::decode(key_hex)
            .map_err(|e| format!("Invalid key hex: {e}"))?;
        if key_bytes.len() != 16 {
            return Err(format!("key must be 16 bytes, got {}", key_bytes.len()));
        }
        let mut addr = [0u8; 16];
        addr.copy_from_slice(&addr_bytes);
        let mut key = [0u8; 16];
        key.copy_from_slice(&key_bytes);
        crate::group::register(addr, key);
        info!("Group: joined {}", group_addr_hex);
        Ok(())
    }

    /// Leave a group channel — stop receiving its messages.
    pub fn leave_group(group_addr_hex: &str) -> Result<(), String> {
        let addr_bytes = hex::decode(group_addr_hex)
            .map_err(|e| format!("Invalid addr hex: {e}"))?;
        if addr_bytes.len() != 16 {
            return Err(format!("group addr must be 16 bytes, got {}", addr_bytes.len()));
        }
        let mut addr = [0u8; 16];
        addr.copy_from_slice(&addr_bytes);
        crate::group::unregister(&addr);
        info!("Group: left {}", group_addr_hex);
        Ok(())
    }

    /// Send a message to a group channel.
    ///
    /// Builds a signed LXMF payload, Fernet-encrypts it with the shared group key,
    /// and dispatches as a Reticulum GROUP packet (broadcast to all connected interfaces).
    pub fn send_group(group_addr_hex: &str, content: &[u8], media_json: Option<&str>) -> Result<u64, String> {
        use rns_transport::hash::AddressHash;
        use rns_transport::identity::PrivateIdentity;
        use rns_transport::packet::{DestinationType, Header, Packet, PacketDataBuffer};

        let dest_bytes = hex::decode(group_addr_hex)
            .map_err(|e| format!("Invalid group addr hex: {e}"))?;
        if dest_bytes.len() != 16 {
            return Err(format!("group addr must be 16 bytes, got {}", dest_bytes.len()));
        }
        let mut dest_arr = [0u8; 16];
        dest_arr.copy_from_slice(&dest_bytes);

        let key = crate::group::lookup_key(&dest_arr)
            .ok_or_else(|| format!("Not joined to group {group_addr_hex}"))?;

        let (transport, identity_bytes, source_hash_bytes, seq, events) = {
            let mut guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_mut().ok_or("Node not initialized")?;
            let transport = node.transport.clone().ok_or("Transport not started (mode 3/4 only)")?;
            let id_bytes = node.identity_bytes.clone().ok_or("No identity available")?;
            let addr_hex = node.address_hex.clone();
            let src = hex::decode(&addr_hex).map_err(|e| format!("Bad address hex: {e}"))?;
            let seq = node.outbound_sent;
            node.outbound_sent += 1;
            let events = Arc::clone(&node.events);
            (transport, id_bytes, src, seq, events)
        };

        if source_hash_bytes.len() != 16 {
            return Err(format!("source address must be 16 bytes, got {}", source_hash_bytes.len()));
        }

        let private_identity = PrivateIdentity::from_private_key_bytes(&identity_bytes)
            .map_err(|e| format!("Failed to restore identity: {:?}", e))?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let fields_mp = build_fields_msgpack(media_json);
        let msgpack = encode_lxmf_msgpack(timestamp, b"", content, &fields_mp);

        // Sign dest+src+msgpack so recipients can verify authorship once they cache our identity.
        let mut sign_data = Vec::with_capacity(16 + 16 + msgpack.len());
        sign_data.extend_from_slice(&dest_arr);
        sign_data.extend_from_slice(&source_hash_bytes);
        sign_data.extend_from_slice(&msgpack);
        let signature = private_identity.sign(&sign_data).to_bytes();

        let mut lxmf_payload = Vec::with_capacity(16 + 16 + 64 + msgpack.len());
        lxmf_payload.extend_from_slice(&dest_arr);
        lxmf_payload.extend_from_slice(&source_hash_bytes);
        lxmf_payload.extend_from_slice(&signature);
        lxmf_payload.extend_from_slice(&msgpack);

        let encrypted = crate::group::group_encrypt(&key, &lxmf_payload)
            .map_err(|e| format!("Group encrypt failed: {:?}", e))?;

        let packet = Packet {
            header: Header {
                destination_type: DestinationType::Group,
                ..Header::default()
            },
            destination: AddressHash::new(dest_arr),
            data: PacketDataBuffer::new_from_slice(&encrypted),
            ..Packet::default()
        };

        get_runtime().block_on(async move {
            transport.lock().await.send_packet(packet).await;
        });

        info!("LxmfNode::send_group: seq={} group={}", seq, group_addr_hex);
        if let Ok(mut eq) = events.lock() {
            eq.push_back(LxmfEvent::MessageQueued { seq, dest_hex: group_addr_hex.to_string() });
        }
        Ok(seq)
    }

    /// Start in BLE-only mode (mode 0).
    ///
    /// Sets up a full rns-transport instance with BleInterface instead of TcpClient.
    /// The Kotlin BleManager must be started separately (it owns hardware access).
    /// Call `nativeBleConnected` / `nativeBleDisconnected` / `nativeBleReceive` from Kotlin
    /// as BLE peers connect and send data.
    fn start_ble(identity_hex: &str, _address_hex: &str, display_name: &str, is_beacon: bool) -> Result<(), String> {
        use rns_transport::identity::PrivateIdentity;
        use rns_transport::transport::TransportConfig;
        use rns_transport::destination::DestinationName;

        if Self::is_running() {
            return Err("Node already running".into());
        }

        // Create or restore identity.
        let private_identity = if identity_hex.len() == 128 {
            PrivateIdentity::new_from_hex_string(identity_hex)
                .map_err(|e| format!("Invalid identity hex: {:?}", e))?
        } else {
            info!("LxmfNode BLE: generating new identity");
            PrivateIdentity::new_from_rand(rand_core::OsRng)
        };

        let id_hex = private_identity.to_hex_string();
        let id_bytes = private_identity.to_private_key_bytes().to_vec();

        let (events, store_arc) = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            (Arc::clone(&node.events), node.store.clone())
        };

        let rt = get_runtime();
        let display_name = display_name.to_owned();
        // Clone for the periodic re-announce task spawned below; the original
        // is moved into the rt.block_on async block.
        let display_name_reann = display_name.clone();

        let (transport_arc, my_dest, mut data_rx, mut resource_rx, announce_rx, addr_hex) =
            rt.block_on(async move {
                let config = TransportConfig::new("lxmf-ble", &private_identity, true);
                let mut transport = Transport::new(config);

                // Register BLE interface — phone-to-phone mesh (HDLC + segmentation).
                // Register NUS interface — RNode BLE (KISS framing).
                {
                    let iface_mgr = transport.iface_manager();
                    let mut mgr = iface_mgr.lock().await;
                    mgr.spawn(BleInterface::new(), BleInterface::spawn);
                    mgr.spawn(NusInterface::new(), NusInterface::spawn);
                }

                // Register LXMF delivery destination.
                let dest_name = DestinationName::new("lxmf", "delivery");
                let my_dest = transport.add_destination(private_identity.clone(), dest_name).await;

                let addr_hex = {
                    let d = my_dest.lock().await;
                    hex::encode(d.desc.address_hash.as_slice())
                };

                // Brief pause to let BleInterface start its poll loop.
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

                // Send initial announce (broadcast to any connected BLE peers).
                info!("LxmfNode BLE: sending announce as {}", addr_hex);
                let ble_name = build_app_data(&display_name, is_beacon);
                transport.send_announce(&my_dest, Some(ble_name.as_slice())).await;

                let data_rx = transport.received_data_events();
                let resource_rx = transport.resource_events();
                let announce_rx = transport.recv_announces().await;
                let arc = Arc::new(tokio::sync::Mutex::new(transport));
                (arc, my_dest, data_rx, resource_rx, announce_rx, addr_hex)
            });

        info!("LxmfNode BLE: LXMF delivery address = {}", addr_hex);

        // Push status event.
        if let Ok(mut eq) = events.lock() {
            eq.push_back(LxmfEvent::StatusChanged { running: true, lifecycle: 0 });
        }

        // Extract pending-send queue and identity cache — mirrors start_reticulum.
        // Required so the announce receiver can flush queued outbound messages
        // when a BLE peer announces after a DroppedMissingDestinationIdentity send.
        let (pending_for_ann, peer_ids_arc) = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            (Arc::clone(&node.pending_sends), Arc::clone(&node.peer_identities))
        };
        let transport_for_ann = Arc::clone(&transport_arc);
        // Reload persistent outbound queue from SQLite so messages queued in a
        // previous stop/start cycle are retried once the peer re-announces.
        if let Some(s) = &store_arc {
            if let Ok(rows) = s.all_outbound_queue() {
                let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                let existing: std::collections::HashSet<u64> = q.iter().map(|p| p.seq).collect();
                for (id, seq, dest, payload) in rows {
                    if !existing.contains(&seq) {
                        q.push(PendingSend { seq, dest, lxmf_payload: payload, store_id: Some(id) });
                    }
                }
            }
        }

        // Collect JoinHandles so stop() can abort every spawned task and
        // prevent zombie task accumulation across Stop/Start cycles.
        let mut task_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        // Spawn announce receiver.
        let events_ann = Arc::clone(&events);
        let store_ann = store_arc.clone();
        let pending_for_ann_task = Arc::clone(&pending_for_ann);
        let peer_ids_ann = Arc::clone(&peer_ids_arc);
        let transport_ann = Arc::clone(&transport_for_ann);
        let mut announce_rx = announce_rx;
        task_handles.push(rt.spawn(async move {
            loop {
                match announce_rx.recv().await {
                    Ok(event) => {
                        let (dh, desc, app_data, hops) = {
                            let dest = event.destination.lock().await;
                            let desc = dest.desc;
                            let hash_bytes = desc.address_hash;
                            let mut dh = [0u8; 16];
                            dh.copy_from_slice(hash_bytes.as_slice());
                            let app_data = event.app_data.as_slice().to_vec();
                            let hops = event.hops;
                            (dh, desc, app_data, hops)
                        };
                        info!("LxmfNode BLE: announce from {} ({} hops)", hex::encode(&dh), hops);

                        // Cache identity for large-payload link sends (DroppedCiphertextTooLarge path).
                        if let Ok(mut ids) = peer_ids_ann.lock() {
                            ids.insert(dh, desc);
                        }

                        // Flush any outbound messages queued while peer identity was unknown.
                        // Mirrors the TCP-mode announce receiver so BLE replies aren't held
                        // indefinitely after a DroppedMissingDestinationIdentity → path-request cycle.
                        let to_retry: Vec<(u64, Option<i64>, Vec<u8>)> = {
                            let mut q = pending_for_ann_task.lock().unwrap_or_else(|p| p.into_inner());
                            let (matched, rest): (Vec<_>, Vec<_>) = q.drain(..).partition(|s| s.dest == dh);
                            *q = rest;
                            matched.into_iter().map(|s| (s.seq, s.store_id, s.lxmf_payload)).collect()
                        };
                        if !to_retry.is_empty() {
                            use rns_transport::hash::AddressHash;
                            use rns_transport::packet::{Packet, PacketDataBuffer};
                            use rns_transport::transport::SendPacketOutcome;
                            use rns_transport::delivery::send_via_link;
                            let transport = transport_ann.lock().await;
                            for (seq, store_id, payload) in &to_retry {
                                let mut dest_arr = [0u8; 16];
                                dest_arr.copy_from_slice(&payload[..16]);
                                let packet = Packet {
                                    destination: AddressHash::new(dest_arr),
                                    data: PacketDataBuffer::new_from_slice(payload),
                                    ..Default::default()
                                };
                                let outcome = transport.send_packet_with_outcome(packet).await;
                                info!("LxmfNode BLE: opportunistic retry seq={} -> {:?}", seq, outcome);
                                let delivered = match outcome {
                                    SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => true,
                                    SendPacketOutcome::DroppedCiphertextTooLarge => {
                                        match send_via_link(&transport, desc, payload, std::time::Duration::from_secs(15)).await {
                                            Ok(_) => { info!("LxmfNode BLE: link-send seq={} ok", seq); true }
                                            Err(e) => { warn!("LxmfNode BLE: link-send seq={} failed: {e}", seq); false }
                                        }
                                    }
                                    _ => false,
                                };
                                if delivered {
                                    if let Some(id) = store_id {
                                        if let Some(s) = &store_ann { let _ = s.remove_outbound(*id); }
                                    }
                                    if let Ok(mut eq) = events_ann.lock() {
                                        eq.push_back(LxmfEvent::MessageDelivered { seq: *seq, dest_hex: hex::encode(&dh) });
                                    }
                                } else {
                                    let mut q = pending_for_ann_task.lock().unwrap_or_else(|p| p.into_inner());
                                    q.push(PendingSend { seq: *seq, dest: dh, lxmf_payload: payload.clone(), store_id: *store_id });
                                }
                            }
                        }

                        if let Ok(mut eq) = events_ann.lock() {
                            eq.push_back(LxmfEvent::AnnounceReceived {
                                dest_hash: dh,
                                app_data,
                                hops,
                            });
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode BLE: lagged {} announce events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Spawn data receiver.
        let events_data = Arc::clone(&events);
        let store_data = store_arc.clone();
        let transport_data = Arc::clone(&transport_arc);
        let peer_ids_verify = Arc::clone(&peer_ids_arc);
        task_handles.push(rt.spawn(async move {
            loop {
                match data_rx.recv().await {
                    Ok(received) => {
                        let data = received.data.as_slice().to_vec();
                        let src = if data.len() >= 32 {
                            let mut s = [0u8; 16]; s.copy_from_slice(&data[16..32]); s
                        } else { [0u8; 16] };
                        info!("LxmfNode BLE: received {} bytes from {}", data.len(), hex::encode(&src));

                        // Beacon RPC responses are JSON or zlib-compressed, not LXMF-framed.
                        if looks_like_rpc_response(&data) {
                            // BLE-only mode has no RPC dispatch task — silently discard.
                            continue;
                        }

                        if data.len() >= 32 {
                            let sender_hash = src;

                            // Fire request_path unconditionally so the peer re-announces
                            // and their identity enters peer_identities — future messages
                            // from this source will then be signature-verified.
                            let t = Arc::clone(&transport_data);
                            tokio::spawn(async move {
                                use rns_transport::hash::AddressHash;
                                t.lock().await.request_path(&AddressHash::new(sender_hash), None, None).await;
                            });

                            let sig_result = {
                                let ids = peer_ids_verify.lock().unwrap_or_else(|p| p.into_inner());
                                verify_lxmf_signature(&data, &ids)
                            };
                            match sig_result {
                                Some(true) => {}
                                Some(false) => {
                                    warn!("LxmfNode BLE: invalid LXMF signature from {}, dropping",
                                        hex::encode(&sender_hash));
                                    continue;
                                }
                                None => {
                                    warn!("LxmfNode BLE: accepting message from unknown peer {} (unverified, path requested)",
                                        hex::encode(&sender_hash));
                                }
                            }
                        }

                        let event = lxmf_event_from_bytes(src, data, None);
                        persist_inbound_message(&store_data, &event);
                        if let Ok(mut eq) = events_data.lock() {
                            if eq.len() < 1024 {
                                eq.push_back(event);
                            } else {
                                warn!("LxmfNode BLE: event queue full, dropping inbound message");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode BLE: lagged {} data events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Resource receiver — large messages delivered via Reticulum resource transfer.
        let events_res = Arc::clone(&events);
        let store_res = store_arc.clone();
        task_handles.push(rt.spawn(async move {
            use rns_transport::resource::ResourceEventKind;
            loop {
                match resource_rx.recv().await {
                    Ok(event) => {
                        if let ResourceEventKind::Complete(complete) = event.kind {
                            let data = complete.data;
                            let src = if data.len() >= 32 {
                                let mut s = [0u8; 16]; s.copy_from_slice(&data[16..32]); s
                            } else { [0u8; 16] };
                            info!("LxmfNode BLE: resource complete {} bytes from {}", data.len(), hex::encode(&src));
                            let lxmf_event = lxmf_event_from_bytes(src, data, None);
                            persist_inbound_message(&store_res, &lxmf_event);
                            if let Ok(mut eq) = events_res.lock() {
                                eq.push_back(lxmf_event);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode BLE: lagged {} resource events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Spawn periodic re-announce — mirrors mode 3 (TCP). The initial announce
        // emitted inside the setup block above fires before BLE peers have
        // completed their GATT subscribe handshake (~1-2s race), so peers miss
        // it. This loop fires another announce 5s after start (giving peers time
        // to subscribe), then continues at the configured interval. Without this,
        // peers never learn each other's identity and unicast send_to remains
        // queued in the opportunistic-retry buffer indefinitely.
        let transport_reannounce = Arc::clone(&transport_arc);
        let dest_reannounce = Arc::clone(&my_dest);
        let name_bytes_reann: Vec<u8> = build_app_data(&display_name_reann, is_beacon);
        // Event-driven re-announce on BLE peer connect. The 5s periodic timer
        // below handles the cold-start case; this handles the case where a peer
        // (re)connects later and would otherwise wait up to 300s for the next
        // periodic announce. Cheap: a Notify is essentially free until fired.
        let transport_pc = Arc::clone(&transport_arc);
        let dest_pc = Arc::clone(&my_dest);
        let name_bytes_pc = name_bytes_reann.clone();
        let notify = crate::ble_iface::peer_connected_notify();
        task_handles.push(rt.spawn(async move {
            loop {
                notify.notified().await;
                // Small grace window for GATT subscribe to complete on the new peer.
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                info!("LxmfNode BLE: re-announce on peer connect");
                transport_pc
                    .lock()
                    .await
                    .send_announce(&dest_pc, Some(name_bytes_pc.as_slice()))
                    .await;
            }
        }));

        task_handles.push(rt.spawn(async move {
            // Initial post-connect re-announce: 5s after start gives peers time
            // to complete the GATT subscribe handshake.
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            info!("LxmfNode BLE: periodic re-announce (initial post-connect)");
            transport_reannounce
                .lock()
                .await
                .send_announce(&dest_reannounce, Some(name_bytes_reann.as_slice()))
                .await;

            // Steady-state loop — same 5min default as TCP mode.
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                info!("LxmfNode BLE: periodic re-announce");
                transport_reannounce
                    .lock()
                    .await
                    .send_announce(&dest_reannounce, Some(name_bytes_reann.as_slice()))
                    .await;
            }
        }));

        // Group channel receiver (BLE mode)
        let events_group = Arc::clone(&events);
        let store_group = store_arc.clone();
        let group_iface_rx = rt.block_on(async { transport_arc.lock().await.iface_rx() });
        task_handles.push(rt.spawn(async move {
            let mut rx = group_iface_rx;
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        let Ok(dest): Result<[u8; 16], _> = msg.packet.destination.as_slice().try_into() else { continue };
                        let Some(key) = crate::group::lookup_key(&dest) else { continue };
                        let raw = msg.packet.data.as_slice();
                        match crate::group::group_decrypt(&key, raw) {
                            Ok(dec) if dec.len() >= 97 => {
                                let mut src = [0u8; 16];
                                src.copy_from_slice(&dec[16..32]);
                                let event = lxmf_event_from_bytes(src, dec.clone(), Some(dest));
                                persist_inbound_message(&store_group, &event);
                                if let Ok(mut eq) = events_group.lock() {
                                    if eq.len() < 1024 { eq.push_back(event); }
                                }
                            }
                            Ok(_) => warn!("Group RX BLE: decrypted payload too short"),
                            Err(e) => warn!("Group RX BLE: decrypt failed for {}: {:?}", hex::encode(&dest), e),
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode BLE Group RX: lagged {} events", n);
                    }
                    Err(_) => break,
                }
            }
        }));

        // Update node state.
        let mut guard = Self::global().lock().map_err(|e| e.to_string())?;
        let node = guard.as_mut().ok_or("Node not initialized")?;
        node.running = true;
        node.mode = 0;
        node.identity_hex = id_hex;
        node.address_hex = addr_hex;
        node.identity_bytes = Some(id_bytes);
        node.transport = Some(transport_arc);
        node.task_handles = task_handles;

        info!("LxmfNode: BLE transport started");
        Ok(())
    }

    /// Stop the node
    pub fn stop() -> Result<(), String> {
        let mut guard = Self::global().lock().map_err(|e| e.to_string())?;
        let node = guard.as_mut().ok_or("Node not initialized")?;

        if !node.running {
            return Ok(());
        }

        // Abort every task spawned by start_*. Without this, peer-connect
        // listeners and periodic re-announce loops survive Stop and accumulate
        // across Stop/Start cycles and mode switches, polluting logs and
        // sending stray announces from prior sessions.
        let aborted = node.task_handles.len();
        for h in node.task_handles.drain(..) {
            h.abort();
        }
        if aborted > 0 {
            info!("LxmfNode: aborted {} background tasks", aborted);
        }

        // TODO: graceful transport shutdown
        node.running = false;
        if let Ok(mut m) = node.beacon_mgr.lock() { m.stop(); }
        info!("LxmfNode: stopped");
        Ok(())
    }

    /// Check if running
    pub fn is_running() -> bool {
        Self::global()
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|n| n.running))
            .unwrap_or(false)
    }

    /// Get status as JSON
    pub fn get_status_json() -> Result<String, String> {
        let guard = Self::global().lock().map_err(|e| e.to_string())?;
        let node = guard.as_ref().ok_or("Node not initialized")?;

        let json = serde_json::json!({
            "running": node.running,
            "mode": node.mode,
            "identityHex": &node.identity_hex[..std::cmp::min(32, node.identity_hex.len())],
            "addressHex": &node.address_hex,
            "lifecycle": if node.running { node.mode } else { 0 },
            "epoch": 0,
            "pendingOutbound": 0,
            "outboundSent": node.outbound_sent,
            "inboundAccepted": node.messages_received,
            "announcesReceived": node.announces_received,
            "lxmfMessagesReceived": node.messages_received,
            "blePeerCount": crate::ble_iface::ble_peer_count() as u32,
        }).to_string();

        Ok(json)
    }

    /// Drain pending events
    pub fn drain_events() -> Vec<LxmfEvent> {
        let mut guard = match Self::global().lock() {
            Ok(g) => g,
            Err(_) => return vec![],
        };
        let node = match guard.as_mut() {
            Some(n) => n,
            None => return vec![],
        };

        let mut events = Vec::new();
        if let Ok(mut eq) = node.events.lock() {
            while let Some(ev) = eq.pop_front() {
                events.push(ev);
            }
        }

        for log_line in crate::log_bridge::drain_logs() {
            events.push(LxmfEvent::Log {
                level: log_line.level,
                message: log_line.message,
            });
        }

        if let Ok(mut m) = node.beacon_mgr.lock() { events.extend(m.drain_events()); }

        // Update counters based on drained events
        for ev in &events {
            match ev {
                LxmfEvent::AnnounceReceived { .. } => node.announces_received += 1,
                LxmfEvent::MessageReceived { .. } => node.messages_received += 1,
                _ => {}
            }
        }

        events
    }

    /// Get the node's identity hex (full 128-char private key hex for persistence)
    pub fn get_identity_hex() -> Option<String> {
        Self::global()
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|n| n.identity_hex.clone()))
    }

    /// Get the node's address hex
    pub fn get_address_hex() -> Option<String> {
        Self::global()
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|n| n.address_hex.clone()))
    }

    pub fn abi_version() -> u32 {
        2 // v2 = rns-transport based
    }
}

/// True when data cannot be an LXMF packet and should be tried as an RPC response.
/// LXMF minimum is 97 bytes with a binary header; JSON starts with `{`, compressed with `\x00`.
fn looks_like_rpc_response(data: &[u8]) -> bool {
    data.len() < 97 || data.starts_with(b"{") || data.starts_with(b"\x00zl")
}

/// Decode an inbound LXMF wire payload and return a MessageReceived event.
/// Falls back to raw body if the payload cannot be parsed.
pub(crate) fn lxmf_event_from_bytes(src: LxmfAddress, data: Vec<u8>, group_dest: Option<LxmfAddress>) -> LxmfEvent {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Some(dec) = decode_lxmf_payload(&data) {
        LxmfEvent::MessageReceived {
            source: src, title: dec.title, body: dec.body,
            image: dec.image, files: dec.files, timestamp: ts, group_dest,
        }
    } else {
        LxmfEvent::MessageReceived {
            source: src, title: vec![], body: data,
            image: None, files: vec![], timestamp: ts, group_dest,
        }
    }
}

/// Persist an inbound MessageReceived event to the local SQLite store.
/// No-op when store is absent or the event variant is not MessageReceived.
pub(crate) fn persist_inbound_message(store: &Option<Arc<MessageStore>>, event: &LxmfEvent) {
    let s = match store { Some(s) => s, None => return };
    if let LxmfEvent::MessageReceived { source, title, body, image, files, timestamp, group_dest } = event {
        let zero = [0u8; 16];
        let dest = group_dest.as_ref().unwrap_or(&zero);
        let img_ref = image.as_ref().map(|(m, d)| (m.as_str(), d.as_slice()));
        if let Err(e) = s.insert_inbound_message(source, dest, title, body, img_ref, files, *timestamp) {
            warn!("persist_inbound_message: SQLite error: {e}");
        }
    }
}

pub(crate) struct DecodedLxmf {
    pub(crate) title: Vec<u8>,
    pub(crate) body: Vec<u8>,
    pub(crate) image: Option<(String, Vec<u8>)>,
    pub(crate) files: Vec<(String, Vec<u8>)>,
}

/// Verify the Ed25519 signature on an inbound LXMF wire packet.
///
/// LXMF wire: [0..16] dest | [16..32] src | [32..96] sig | [96..] msgpack
/// Signed region: data[0..32] + data[96..]  (dest + src + msgpack payload)
///
/// Returns:
/// - `Some(true)`  — sender known, signature valid
/// - `Some(false)` — sender known, signature INVALID → drop the packet
/// - `None`        — sender not in peer_identities (no announce yet) → accept with warning
pub(crate) fn verify_lxmf_signature(
    data: &[u8],
    peer_identities: &HashMap<[u8; 16], rns_transport::destination::DestinationDesc>,
) -> Option<bool> {
    if data.len() < 97 { return Some(false); }
    let mut sender_hash = [0u8; 16];
    sender_hash.copy_from_slice(&data[16..32]);
    let desc = peer_identities.get(&sender_hash)?;
    let Ok(sig) = ed25519_dalek::Signature::from_slice(&data[32..96]) else {
        return Some(false);
    };
    let mut signed = Vec::with_capacity(32 + data.len() - 96);
    signed.extend_from_slice(&data[0..32]);
    signed.extend_from_slice(&data[96..]);
    Some(desc.identity.verify(&signed, &sig).is_ok())
}

/// Parse an LXMF wire payload.
/// Format: [16B dest][16B src][64B sig][msgpack([f64 ts, bin title, bin body, map fields])]
pub(crate) fn decode_lxmf_payload(data: &[u8]) -> Option<DecodedLxmf> {
    if data.len() < 97 { return None; }
    let mp = &data[96..];
    if mp.first() != Some(&0x94) { return None; } // fixarray(4)
    let mut pos = 1usize;
    if mp.get(pos) != Some(&0xcb) { return None; } // float64 timestamp
    pos += 9;
    let title = mp_read_bytes(mp, &mut pos).unwrap_or_default();
    let body  = mp_read_bytes(mp, &mut pos).unwrap_or_default();
    let (image, files) = mp_read_lxmf_fields(mp, &mut pos);
    Some(DecodedLxmf { title, body, image, files })
}

pub(crate) fn mp_read_bytes(data: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    let b = *data.get(*pos)?; *pos += 1;
    let len: usize = match b {
        0xc4 => { let l = *data.get(*pos)? as usize; *pos += 1; l }
        0xc5 => mp_u16(data, pos)?,
        0xc6 => mp_u32(data, pos)?,
        b if b & 0xe0 == 0xa0 => (b & 0x1f) as usize,
        0xd9 => { let l = *data.get(*pos)? as usize; *pos += 1; l }
        0xda => mp_u16(data, pos)?,
        _ => return None,
    };
    let bytes = data.get(*pos..*pos + len)?.to_vec();
    *pos += len;
    Some(bytes)
}

pub(crate) fn mp_u16(data: &[u8], pos: &mut usize) -> Option<usize> {
    let v = ((*data.get(*pos)? as usize) << 8) | (*data.get(*pos + 1)? as usize);
    *pos += 2; Some(v)
}

pub(crate) fn mp_u32(data: &[u8], pos: &mut usize) -> Option<usize> {
    let v = ((*data.get(*pos)? as usize) << 24) | ((*data.get(*pos+1)? as usize) << 16)
          | ((*data.get(*pos+2)? as usize) << 8) | (*data.get(*pos+3)? as usize);
    *pos += 4;
    // Reject absurd blob lengths; `data.get()` would return None anyway, but this
    // prevents corrupted skip arithmetic from confusing subsequent parsing.
    if v > 16 * 1024 * 1024 { return None; }
    Some(v)
}

pub(crate) fn mp_read_array_len(data: &[u8], pos: &mut usize) -> Option<usize> {
    let b = *data.get(*pos)?; *pos += 1;
    match b {
        b if b & 0xf0 == 0x90 => Some((b & 0x0f) as usize),
        0xdc => mp_u16(data, pos),
        0xdd => mp_u32(data, pos),
        _ => None,
    }
}

pub(crate) fn mp_skip(data: &[u8], pos: &mut usize) {
    mp_skip_inner(data, pos, 0);
}

fn mp_skip_inner(data: &[u8], pos: &mut usize, depth: usize) {
    // Prevent stack overflow from maliciously deeply nested containers.
    if depth > 16 { return; }
    let b = match data.get(*pos) { Some(&b) => { *pos += 1; b } None => return };
    match b {
        0x00..=0x7f | 0xe0..=0xff | 0xc0 | 0xc2 | 0xc3 => {}
        0xca | 0xce | 0xd2 => { *pos = (*pos + 4).min(data.len()); }
        0xcb | 0xcf | 0xd3 => { *pos = (*pos + 8).min(data.len()); }
        0xcc | 0xd0 => { *pos = (*pos + 1).min(data.len()); }
        0xcd | 0xd1 => { *pos = (*pos + 2).min(data.len()); }
        b if b & 0xe0 == 0xa0 => { *pos = (*pos + (b & 0x1f) as usize).min(data.len()); }
        0xd9 | 0xc4 => { if let Some(&l) = data.get(*pos) { *pos = (*pos + 1 + l as usize).min(data.len()); } }
        0xda | 0xc5 | 0xdc => { if let Some(n) = mp_u16(data, pos) { *pos = (*pos + n).min(data.len()); } }
        0xdb | 0xc6 | 0xdd => {
            match mp_u32(data, pos) {
                Some(n) => { *pos = (*pos + n).min(data.len()); }
                // mp_u32 returned None (> 16 MiB cap or out of bounds).
                // Advance to end so no subsequent bytes are misread as keys.
                None => { *pos = data.len(); }
            }
        }
        b if b & 0xf0 == 0x90 => { let n = (b & 0x0f) as usize; for _ in 0..n { mp_skip_inner(data, pos, depth + 1); } }
        b if b & 0xf0 == 0x80 => { let n = (b & 0x0f) as usize; for _ in 0..n { mp_skip_inner(data, pos, depth + 1); mp_skip_inner(data, pos, depth + 1); } }
        0xde => { if let Some(n) = mp_u16(data, pos) { for _ in 0..n { mp_skip_inner(data, pos, depth + 1); mp_skip_inner(data, pos, depth + 1); } } }
        _ => {}
    }
}

pub(crate) fn mp_read_lxmf_fields(data: &[u8], pos: &mut usize) -> (Option<(String, Vec<u8>)>, Vec<(String, Vec<u8>)>) {
    let mut image: Option<(String, Vec<u8>)> = None;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let b = match data.get(*pos) { Some(&b) => { *pos += 1; b } None => return (image, files) };
    let map_len: usize = match b {
        b if b & 0xf0 == 0x80 => (b & 0x0f) as usize,
        0xde => match mp_u16(data, pos) { Some(n) => n, None => return (image, files) },
        _ => return (image, files),
    };
    for _ in 0..map_len {
        let key = match data.get(*pos) {
            Some(&k) if k < 0x80 => { *pos += 1; k }
            _ => break,
        };
        match key {
            0x06 => { // FIELD_IMAGE: fixarray(2) [mime_str, data_bin]
                if data.get(*pos).copied() == Some(0x92) {
                    *pos += 1;
                    if let (Some(m), Some(d)) = (mp_read_bytes(data, pos), mp_read_bytes(data, pos)) {
                        image = Some((String::from_utf8(m).unwrap_or_else(|_| "image/jpeg".into()), d));
                    }
                } else { mp_skip(data, pos); }
            }
            0x05 => { // FIELD_FILE_ATTACHMENTS: array [ fixarray(2) [name, data], ... ]
                let n = mp_read_array_len(data, pos).unwrap_or(0);
                for _ in 0..n {
                    if data.get(*pos).copied() == Some(0x92) {
                        *pos += 1;
                        if let (Some(nb), Some(fd)) = (mp_read_bytes(data, pos), mp_read_bytes(data, pos)) {
                            files.push((String::from_utf8(nb).unwrap_or_else(|_| "file".into()), fd));
                        }
                    } else { mp_skip(data, pos); }
                }
            }
            _ => { mp_skip(data, pos); }
        }
    }
    (image, files)
}

/// Build announce app_data.
/// Beacon nodes: `b"anonmesh::beacon::v1\0" + display_name` so CLI can discover them via startswith.
/// Non-beacon nodes: just the display name (legacy behaviour).
pub(crate) fn build_app_data(display_name: &str, is_beacon: bool) -> Vec<u8> {
    const PREFIX: &[u8] = b"anonmesh::beacon::v1\0";
    let name = if display_name.is_empty() { "lxmf-mobile" } else { display_name };
    // Truncate at a UTF-8 character boundary ≤ 32 bytes to avoid splitting multi-byte codepoints.
    let truncated_len = name.char_indices()
        .take_while(|(i, _)| *i < 32)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0)
        .min(name.len());
    let name_bytes = &name.as_bytes()[..truncated_len];
    if is_beacon {
        let mut data = Vec::with_capacity(PREFIX.len() + name_bytes.len());
        data.extend_from_slice(PREFIX);
        data.extend_from_slice(name_bytes);
        data
    } else {
        name_bytes.to_vec()
    }
}

/// Encode LXMF msgpack payload: fixarray(4) [timestamp:f64, title:bin, content:bin, fields:map]
pub(crate) fn encode_lxmf_msgpack(timestamp: f64, title: &[u8], content: &[u8], fields_mp: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 9 + 2 + title.len() + 2 + content.len() + fields_mp.len());

    buf.push(0x94); // fixarray(4)

    buf.push(0xcb); // float64
    buf.extend_from_slice(&timestamp.to_bits().to_be_bytes());

    buf.extend_from_slice(&mp_bin(title));
    buf.extend_from_slice(&mp_bin(content));
    buf.extend_from_slice(fields_mp);

    buf
}

/// Build LXMF fields msgpack from optional media JSON.
///
/// JSON shape: `{"image":{"mimeType":"image/jpeg","data":"<base64>"},
///               "files":[{"name":"x.pdf","data":"<base64>"}]}`
///
/// Produces a msgpack map keyed by LXMF field IDs:
///   0x05 = FIELD_FILE_ATTACHMENTS: [[name_str, data_bin], ...]
///   0x06 = FIELD_IMAGE: [mime_str, data_bin]
pub(crate) fn build_fields_msgpack(media_json: Option<&str>) -> Vec<u8> {
    use base64::Engine as _;

    let json_str = match media_json {
        Some(s) if !s.trim_matches(|c| c == ' ' || c == '\0').is_empty()
                   && s != "null" && s != "{}" => s,
        _ => return vec![0x80],
    };

    let v: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return vec![0x80],
    };
    let obj = match v.as_object() {
        Some(o) if !o.is_empty() => o,
        _ => return vec![0x80],
    };

    let mut fields: Vec<(u8, Vec<u8>)> = Vec::new();

    // FIELD_IMAGE (0x06): [mime_str, data_bin]
    if let Some(img) = obj.get("image").and_then(|v| v.as_object()) {
        let mime = img.get("mimeType").and_then(|v| v.as_str()).unwrap_or("image/jpeg");
        if let Some(data_b64) = img.get("data").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data_b64) {
                let mut val = vec![0x92u8]; // fixarray(2)
                val.extend_from_slice(&mp_str(mime.as_bytes()));
                val.extend_from_slice(&mp_bin(&data));
                fields.push((0x06, val));
            }
        }
    }

    // FIELD_FILE_ATTACHMENTS (0x05): [[name_str, data_bin], ...]
    if let Some(files) = obj.get("files").and_then(|v| v.as_array()) {
        let entries: Vec<Vec<u8>> = files.iter().filter_map(|f| {
            let name = f.get("name").and_then(|v| v.as_str())?;
            let data = base64::engine::general_purpose::STANDARD
                .decode(f.get("data").and_then(|v| v.as_str()).unwrap_or("")).ok()?;
            let mut e = vec![0x92u8]; // fixarray(2)
            e.extend_from_slice(&mp_str(name.as_bytes()));
            e.extend_from_slice(&mp_bin(&data));
            Some(e)
        }).collect();

        if !entries.is_empty() {
            let arr = mp_array(&entries);
            fields.push((0x05, arr));
        }
    }

    if fields.is_empty() {
        return vec![0x80];
    }

    let mut out = Vec::new();
    out.push(0x80 | fields.len() as u8); // fixmap(n)
    for (id, val) in &fields {
        out.push(*id); // fixint key
        out.extend_from_slice(val);
    }
    out
}

pub(crate) fn mp_bin(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    match data.len() {
        0..=255 => { out.push(0xc4); out.push(data.len() as u8); }
        256..=65535 => { out.push(0xc5); out.push((data.len() >> 8) as u8); out.push((data.len() & 0xff) as u8); }
        _ => { out.push(0xc6); out.extend_from_slice(&(data.len() as u32).to_be_bytes()); }
    }
    out.extend_from_slice(data);
    out
}

pub(crate) fn mp_str(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    match s.len() {
        0..=31  => out.push(0xa0 | s.len() as u8),
        32..=255 => { out.push(0xd9); out.push(s.len() as u8); }
        _ => { out.push(0xda); out.push((s.len() >> 8) as u8); out.push((s.len() & 0xff) as u8); }
    }
    out.extend_from_slice(s);
    out
}

pub(crate) fn mp_array(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    if entries.len() <= 15 {
        out.push(0x90 | entries.len() as u8);
    } else {
        out.push(0xdc);
        out.push((entries.len() >> 8) as u8);
        out.push((entries.len() & 0xff) as u8);
    }
    for e in entries { out.extend_from_slice(e); }
    out
}

/// Parse a JSON interfaces array: `[{"host":"...","port":1234}, ...]`
pub(crate) fn parse_interfaces_json(json: &str) -> Result<Vec<(String, u16)>, String> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| format!("Invalid interfaces JSON: {}", e))?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let host = v["host"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("Interface {}: missing or empty \"host\"", i))?
            .to_string();
        let port = v["port"]
            .as_u64()
            .filter(|&p| p > 0 && p <= 65535)
            .ok_or_else(|| format!("Interface {}: invalid \"port\"", i))? as u16;
        out.push((host, port));
    }
    Ok(out)
}

