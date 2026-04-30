//! LxmfNode — full Reticulum node using rns-transport
//!
//! Mode 0: BLE only (embedded FFI)
//! Mode 3: Standard Reticulum TCP (rns-transport with real protocol)
//!
//! The rns-transport mode creates a proper Reticulum node that speaks the
//! real wire protocol, generates identity, sends announces, and is visible
//! to all other nodes on the network.

use std::sync::{Arc, Mutex, OnceLock};
use std::collections::VecDeque;

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
    },
    AnnounceReceived { dest_hash: DestHash, app_data: Vec<u8>, hops: u8 },
    MessageQueued { seq: u64, dest_hex: String },
    MessageDelivered { seq: u64, dest_hex: String },
    MessageFailed { seq: u64, dest_hex: String, reason: String },
    Log { level: u32, message: String },
    Error { code: u32, message: String },
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
    /// Beacon manager
    pub beacon_mgr: BeaconManager,
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
            beacon_mgr: BeaconManager::new(),
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

        info!("LxmfNode: identity={}", &id_hex[..16]);

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
        let pending_for_ann = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            Arc::clone(&node.pending_sends)
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
        let mut announce_rx = announce_rx;
        task_handles.push(rt.spawn(async move {
            let pending_for_ann = pending_for_ann_task;
            loop {
                match announce_rx.recv().await {
                    Ok(event) => {
                        let dest = event.destination.lock().await;
                        let hash_bytes = dest.desc.address_hash;
                        let mut dh = [0u8; 16];
                        dh.copy_from_slice(hash_bytes.as_slice());
                        let app_data = event.app_data.as_slice().to_vec();
                        info!("LxmfNode: announce from {} ({} hops)", hex::encode(&dh), event.hops);
                        drop(dest);

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
                                match outcome {
                                    SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => {
                                        if let Some(id) = store_id {
                                            if let Some(s) = &store_ann { let _ = s.remove_outbound(*id); }
                                        }
                                        if let Ok(mut eq) = events_ann.lock() {
                                            eq.push_back(LxmfEvent::MessageDelivered { seq: *seq, dest_hex: hex::encode(&dh) });
                                        }
                                    }
                                    _ => {
                                        let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                                        q.push(PendingSend { seq: *seq, dest: dh, lxmf_payload: payload.clone(), store_id: *store_id });
                                    }
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
        task_handles.push(rt.spawn(async move {
            loop {
                match data_rx.recv().await {
                    Ok(received) => {
                        let mut src = [0u8; 16];
                        src.copy_from_slice(received.destination.as_slice());
                        let data = received.data.as_slice().to_vec();
                        info!("LxmfNode: received {} bytes from {}", data.len(), hex::encode(&src));
                        if let Ok(mut eq) = events_data.lock() {
                            eq.push_back(lxmf_event_from_bytes(src, data));
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
        task_handles.push(rt.spawn(async move {
            use rns_transport::resource::ResourceEventKind;
            loop {
                match resource_rx.recv().await {
                    Ok(event) => {
                        if let ResourceEventKind::Complete(complete) = event.kind {
                            let mut src = [0u8; 16];
                            src.copy_from_slice(event.link_id.as_slice());
                            let data = complete.data;
                            info!("LxmfNode: resource complete {} bytes from {}", data.len(), hex::encode(&src));
                            if let Ok(mut eq) = events_res.lock() {
                                eq.push_back(lxmf_event_from_bytes(src, data));
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
                    let transport = transport_retry.lock().await;
                    for (seq, store_id, payload, dest) in snapshot {
                        let packet = Packet {
                            destination: AddressHash::new(dest),
                            data: PacketDataBuffer::new_from_slice(&payload),
                            ..Default::default()
                        };
                        let outcome = transport.send_packet_with_outcome(packet).await;
                        match outcome {
                            SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => {
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
                            }
                            _ => {
                                if let Some(id) = store_id {
                                    if let Some(s) = &store_retry { let _ = s.bump_outbound_attempts(id); }
                                }
                            }
                        }
                    }
                }
            }
        }));

        info!("LxmfNode: LXMF delivery address = {}", addr_hex);

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
        info!("LxmfNode full: identity={}", &id_hex[..16]);
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

        let pending_for_ann = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            Arc::clone(&node.pending_sends)
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
        let mut announce_rx = announce_rx;
        task_handles.push(rt.spawn(async move {
            let pending_for_ann = pending_for_ann_task;
            loop {
                match announce_rx.recv().await {
                    Ok(event) => {
                        let dest = event.destination.lock().await;
                        let hash_bytes = dest.desc.address_hash;
                        let mut dh = [0u8; 16];
                        dh.copy_from_slice(hash_bytes.as_slice());
                        let app_data = event.app_data.as_slice().to_vec();
                        info!("LxmfNode full: announce from {} ({} hops)", hex::encode(&dh), event.hops);
                        drop(dest);

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
                                match outcome {
                                    SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => {
                                        if let Some(id) = store_id {
                                            if let Some(s) = &store_ann { let _ = s.remove_outbound(*id); }
                                        }
                                        if let Ok(mut eq) = events_ann.lock() {
                                            eq.push_back(LxmfEvent::MessageDelivered { seq: *seq, dest_hex: hex::encode(&dh) });
                                        }
                                    }
                                    _ => {
                                        let mut q = pending_for_ann.lock().unwrap_or_else(|p| p.into_inner());
                                        q.push(PendingSend { seq: *seq, dest: dh, lxmf_payload: payload.clone(), store_id: *store_id });
                                    }
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
        task_handles.push(rt.spawn(async move {
            loop {
                match data_rx.recv().await {
                    Ok(received) => {
                        let mut src = [0u8; 16];
                        src.copy_from_slice(received.destination.as_slice());
                        let data = received.data.as_slice().to_vec();
                        info!("LxmfNode full: received {} bytes from {}", data.len(), hex::encode(&src));
                        if let Ok(mut eq) = events_data.lock() {
                            eq.push_back(lxmf_event_from_bytes(src, data));
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
        task_handles.push(rt.spawn(async move {
            use rns_transport::resource::ResourceEventKind;
            loop {
                match resource_rx.recv().await {
                    Ok(event) => {
                        if let ResourceEventKind::Complete(complete) = event.kind {
                            let mut src = [0u8; 16];
                            src.copy_from_slice(event.link_id.as_slice());
                            let data = complete.data;
                            info!("LxmfNode full: resource complete {} bytes from {}", data.len(), hex::encode(&src));
                            if let Ok(mut eq) = events_res.lock() {
                                eq.push_back(lxmf_event_from_bytes(src, data));
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
                    let transport = transport_retry.lock().await;
                    for (seq, store_id, payload, dest) in snapshot {
                        let packet = Packet {
                            destination: AddressHash::new(dest),
                            data: PacketDataBuffer::new_from_slice(&payload),
                            ..Default::default()
                        };
                        let outcome = transport.send_packet_with_outcome(packet).await;
                        match outcome {
                            SendPacketOutcome::SentDirect | SendPacketOutcome::SentBroadcast => {
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
                            }
                            _ => {
                                if let Some(id) = store_id {
                                    if let Some(s) = &store_retry { let _ = s.bump_outbound_attempts(id); }
                                }
                            }
                        }
                    }
                }
            }
        }));

        info!("LxmfNode full: TCP+BLE delivery address = {}", addr_hex);

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

        let (transport, identity_bytes, source_hash_bytes, seq, pending_sends, events, store) = {
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
            (transport, id_bytes, src, seq, pending, events, store)
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
                    q.push(PendingSend { seq, dest: dest_arr, lxmf_payload, store_id });
                }
                if let Ok(mut eq) = events.lock() {
                    eq.push_back(LxmfEvent::MessageQueued { seq, dest_hex: dest_hex.to_string() });
                }
                Ok(seq)
            }
            SendPacketOutcome::DroppedNoRoute => {
                warn!("LxmfNode::send_to: queued seq={} (no route to {dest_hex})", seq);
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
            SendPacketOutcome::DroppedCiphertextTooLarge => {
                Err("message payload too large after encryption".to_string())
            }
            SendPacketOutcome::DroppedEncryptFailed => {
                Err(format!("failed to encrypt packet for /{dest_hex}/"))
            }
        }
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

        let events = {
            let guard = Self::global().lock().map_err(|e| e.to_string())?;
            let node = guard.as_ref().ok_or("Node not initialized")?;
            Arc::clone(&node.events)
        };

        let rt = get_runtime();
        let display_name = display_name.to_owned();
        // Clone for the periodic re-announce task spawned below; the original
        // is moved into the rt.block_on async block.
        let display_name_reann = display_name.clone();

        let (transport_arc, my_dest, mut data_rx, announce_rx, addr_hex) =
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
                let announce_rx = transport.recv_announces().await;
                let arc = Arc::new(tokio::sync::Mutex::new(transport));
                (arc, my_dest, data_rx, announce_rx, addr_hex)
            });

        info!("LxmfNode BLE: LXMF delivery address = {}", addr_hex);

        // Push status event.
        if let Ok(mut eq) = events.lock() {
            eq.push_back(LxmfEvent::StatusChanged { running: true, lifecycle: 0 });
        }

        // Collect JoinHandles so stop() can abort every spawned task and
        // prevent zombie task accumulation across Stop/Start cycles.
        let mut task_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        // Spawn announce receiver.
        let events_ann = Arc::clone(&events);
        let mut announce_rx = announce_rx;
        task_handles.push(rt.spawn(async move {
            loop {
                match announce_rx.recv().await {
                    Ok(event) => {
                        let dest = event.destination.lock().await;
                        let hash_bytes = dest.desc.address_hash;
                        let mut dh = [0u8; 16];
                        dh.copy_from_slice(hash_bytes.as_slice());
                        let app_data = event.app_data.as_slice().to_vec();
                        info!("LxmfNode BLE: announce from {} ({} hops)", hex::encode(&dh), event.hops);
                        if let Ok(mut eq) = events_ann.lock() {
                            eq.push_back(LxmfEvent::AnnounceReceived {
                                dest_hash: dh,
                                app_data,
                                hops: event.hops,
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
        task_handles.push(rt.spawn(async move {
            loop {
                match data_rx.recv().await {
                    Ok(received) => {
                        let mut src = [0u8; 16];
                        src.copy_from_slice(received.destination.as_slice());
                        let data = received.data.as_slice().to_vec();
                        info!("LxmfNode BLE: received {} bytes", data.len());
                        if let Ok(mut eq) = events_data.lock() {
                            eq.push_back(lxmf_event_from_bytes(src, data));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("LxmfNode BLE: lagged {} data events", n);
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
        node.beacon_mgr.stop();
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
            "lifecycle": if node.running { 3 } else { 0 },
            "epoch": 0,
            "pendingOutbound": 0,
            "outboundSent": node.outbound_sent,
            "inboundAccepted": node.inbound_accepted,
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

        events.extend(node.beacon_mgr.drain_events());

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

/// Decode an inbound LXMF wire payload and return a MessageReceived event.
/// Falls back to raw body if the payload cannot be parsed.
pub(crate) fn lxmf_event_from_bytes(src: LxmfAddress, data: Vec<u8>) -> LxmfEvent {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Some(dec) = decode_lxmf_payload(&data) {
        LxmfEvent::MessageReceived {
            source: src, title: dec.title, body: dec.body,
            image: dec.image, files: dec.files, timestamp: ts,
        }
    } else {
        LxmfEvent::MessageReceived {
            source: src, title: vec![], body: data,
            image: None, files: vec![], timestamp: ts,
        }
    }
}

pub(crate) struct DecodedLxmf {
    pub(crate) title: Vec<u8>,
    pub(crate) body: Vec<u8>,
    pub(crate) image: Option<(String, Vec<u8>)>,
    pub(crate) files: Vec<(String, Vec<u8>)>,
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
    *pos += 4; Some(v)
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
    let b = match data.get(*pos) { Some(&b) => { *pos += 1; b } None => return };
    match b {
        0x00..=0x7f | 0xe0..=0xff | 0xc0 | 0xc2 | 0xc3 => {}
        0xca | 0xce | 0xd2 => { *pos = pos.saturating_add(4); }
        0xcb | 0xcf | 0xd3 => { *pos = pos.saturating_add(8); }
        0xcc | 0xd0 => { *pos = pos.saturating_add(1); }
        0xcd | 0xd1 => { *pos = pos.saturating_add(2); }
        b if b & 0xe0 == 0xa0 => { *pos = pos.saturating_add((b & 0x1f) as usize); }
        0xd9 | 0xc4 => { if let Some(&l) = data.get(*pos) { *pos += 1 + l as usize; } }
        0xda | 0xc5 | 0xdc => { if let Some(n) = mp_u16(data, pos) { *pos += n; } }
        0xdb | 0xc6 | 0xdd => { if let Some(n) = mp_u32(data, pos) { *pos += n; } }
        b if b & 0xf0 == 0x90 => { let n = (b & 0x0f) as usize; for _ in 0..n { mp_skip(data, pos); } }
        b if b & 0xf0 == 0x80 => { let n = (b & 0x0f) as usize; for _ in 0..n { mp_skip(data, pos); mp_skip(data, pos); } }
        0xde => { if let Some(n) = mp_u16(data, pos) { for _ in 0..n { mp_skip(data, pos); mp_skip(data, pos); } } }
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
    let name_bytes = &name.as_bytes()[..name.len().min(32)];
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

