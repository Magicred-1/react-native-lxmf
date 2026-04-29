import Foundation
import CoreBluetooth

/// Dual-role BLE manager for Reticulum mesh networking
///
/// Handles two types of BLE connections:
///   1. Phone-to-phone mesh: custom GATT service, HDLC+segmentation via ble_iface.rs
///   2. RNode (Heltec V3): Nordic UART Service (NUS), KISS framing via nus_iface.rs
///
/// BLE data flows:
///   Mesh:  peer writes → lxmf_ble_receive → Rust BleInterface (HDLC)
///   RNode: NUS notify  → lxmf_nus_receive → Rust NusInterface (KISS)
class BLEManager: NSObject {
    // Phone-to-phone mesh UUIDs (must match ble_iface.rs)
    static let meshServiceUUID = CBUUID(string: "5f3bafcd-2bb7-4de0-9c6f-2c5f88b6b8f2")
    static let rxCharUUID      = CBUUID(string: "3b28e4f6-5a30-4a5f-b700-68bb74d1b036")
    static let txCharUUID      = CBUUID(string: "8b6ded1a-ea65-4a1e-a1f0-5cf69d5dc2ad")

    // RNode NUS UUIDs (Nordic UART Service — must match nus_iface.rs)
    static let nusServiceUUID  = CBUUID(string: "6e400001-b5a3-f393-e0a9-e50e24dcca9e")
    static let nusTxCharUUID   = CBUUID(string: "6e400002-b5a3-f393-e0a9-e50e24dcca9e")  // write TO RNode
    static let nusRxCharUUID   = CBUUID(string: "6e400003-b5a3-f393-e0a9-e50e24dcca9e")  // notify FROM RNode

    // Central (scanner/client)
    private var centralManager: CBCentralManager!
    private var connectedPeripherals: [UUID: CBPeripheral] = [:]
    private var txCharacteristics: [UUID: CBCharacteristic] = [:]

    // Peripheral (advertiser/server)
    private var peripheralManager: CBPeripheralManager!
    private var rxCharacteristic: CBMutableCharacteristic?
    private var txCharacteristic: CBMutableCharacteristic?
    private var subscribedCentrals: [CBCentral] = []

    // Peer address mapping — iOS uses 128-bit UUIDs, Rust uses 6-byte addrs.
    // We derive a 6-byte pseudo-MAC from each UUID and maintain reverse mappings
    // so lxmf_ble_poll_tx frames can be routed to the correct peer.
    private var addrToPeripheralUUID: [Data: UUID] = [:]
    private var addrToCentral: [Data: CBCentral] = [:]

    // RNode NUS connections — separate from mesh peers
    private var nusPeripherals: [UUID: CBPeripheral] = [:]
    private var nusTxChars: [UUID: CBCharacteristic] = [:]  // write TO RNode

    // Persisted set of peripheral UUIDs that have successfully completed
    // characteristic discovery (i.e. OS-level pairing succeeded).
    // Survives app restarts so we only auto-connect to known-good devices.
    private var bondedPeripherals: Set<UUID> = []

    // RNode peripherals discovered during scan but not yet paired via iOS Settings.
    // Exposed to the UI so it can prompt the user to pair in Settings first.
    private(set) var discoveredUnpairedRNodes: [UUID: CBPeripheral] = [:]

    private var isRunning = false

    /// Called when CoreBluetooth signals it is ready to accept more writes.
    /// LxmfModule sets this to re-trigger drainOutgoing() without a timer.
    var onReadyToSend: (() -> Void)?

    /// Per-launch random token embedded in our advertisement's local name, so
    /// our central role can detect and skip our own peripheral advertisement
    /// (CoreBluetooth does not auto-filter self when running both roles).
    private var instanceTokenHex: String = ""
    private static let advertNamePrefix = "lxmf-mesh-"

    private static let bondedKey = "lxmf.ble.bondedPeripheralUUIDs"

    private func loadBondedPeripherals() {
        if let uuids = UserDefaults.standard.array(forKey: Self.bondedKey) as? [String] {
            bondedPeripherals = Set(uuids.compactMap { UUID(uuidString: $0) })
        }
    }

    private func saveBondedPeripherals() {
        let uuids = bondedPeripherals.map { $0.uuidString }
        UserDefaults.standard.set(uuids, forKey: Self.bondedKey)
    }

    override init() {
        super.init()
    }

    func start() {
        guard !isRunning else { return }
        isRunning = true
        loadBondedPeripherals()
        discoveredUnpairedRNodes.removeAll()

        // Generate fresh per-launch token for self-loop detection.
        // CoreBluetooth doesn't filter our own peripheral when scanning, so we
        // embed a random token in the advertised local name and skip matches.
        var tokenBytes = [UInt8](repeating: 0, count: 4)
        guard SecRandomCopyBytes(kSecRandomDefault, tokenBytes.count, &tokenBytes) == errSecSuccess else {
            // CSPRNG failed — fall back to UUID bytes so two failing devices
            // don't converge to all-zero tokens (deterministic collision).
            let fallback = UUID().uuid
            tokenBytes = [fallback.0, fallback.1, fallback.2, fallback.3]
            instanceTokenHex = tokenBytes.map { String(format: "%02x", $0) }.joined()
            // TODO(sentinel): remove or downgrade to debug before production
            NSLog("[BLE] instance token (fallback): %@", instanceTokenHex)
            // Use restoration identifiers for background BLE
            centralManager = CBCentralManager(
                delegate: self,
                queue: DispatchQueue(label: "lxmf.ble.central"),
                options: [CBCentralManagerOptionRestoreIdentifierKey: "lxmf-central"]
            )
            peripheralManager = CBPeripheralManager(
                delegate: self,
                queue: DispatchQueue(label: "lxmf.ble.peripheral"),
                options: [CBPeripheralManagerOptionRestoreIdentifierKey: "lxmf-peripheral"]
            )
            return
        }
        instanceTokenHex = tokenBytes.map { String(format: "%02x", $0) }.joined()
        // TODO(sentinel): remove or downgrade to debug before production
        NSLog("[BLE] instance token: %@", instanceTokenHex)

        // Use restoration identifiers for background BLE
        centralManager = CBCentralManager(
            delegate: self,
            queue: DispatchQueue(label: "lxmf.ble.central"),
            options: [CBCentralManagerOptionRestoreIdentifierKey: "lxmf-central"]
        )

        peripheralManager = CBPeripheralManager(
            delegate: self,
            queue: DispatchQueue(label: "lxmf.ble.peripheral"),
            options: [CBPeripheralManagerOptionRestoreIdentifierKey: "lxmf-peripheral"]
        )
    }

    func stop() {
        guard isRunning else { return }
        isRunning = false

        centralManager?.stopScan()
        for (_, peripheral) in connectedPeripherals {
            centralManager?.cancelPeripheralConnection(peripheral)
        }
        for (_, peripheral) in nusPeripherals {
            centralManager?.cancelPeripheralConnection(peripheral)
        }
        connectedPeripherals.removeAll()
        txCharacteristics.removeAll()
        addrToPeripheralUUID.removeAll()
        addrToCentral.removeAll()
        nusPeripherals.removeAll()
        nusTxChars.removeAll()
        discoveredUnpairedRNodes.removeAll()
        // Don't clear bondedPeripherals — they persist across sessions

        peripheralManager?.stopAdvertising()
        peripheralManager?.removeAllServices()
        subscribedCentrals.removeAll()
    }

    /// Send data to all connected peers via TX characteristic
    func sendToAll(_ data: Data) {
        // Send via peripheral role (to subscribed centrals)
        if let txChar = txCharacteristic {
            for central in subscribedCentrals {
                peripheralManager?.updateValue(data, for: txChar, onSubscribedCentrals: [central])
            }
        }

        // Send via central role (write to connected peripherals' RX)
        for (uuid, char) in txCharacteristics {
            if let peripheral = connectedPeripherals[uuid] {
                peripheral.writeValue(data, for: char, type: .withoutResponse)
            }
        }
    }

    /// Send data to a specific peer by CoreBluetooth UUID
    func sendToPeer(_ peerUUID: UUID, data: Data) {
        if let peripheral = connectedPeripherals[peerUUID],
           let char = txCharacteristics[peerUUID] {
            peripheral.writeValue(data, for: char, type: .withoutResponse)
        }
    }

    /// Send data to a specific peer by 6-byte pseudo-MAC address.
    /// Returns false when CoreBluetooth's TX buffer is full — caller should stop
    /// draining and wait for onReadyToSend before resuming.
    @discardableResult
    func sendToPeerAddr(_ addr: Data, data: Data) -> Bool {
        // Peripheral role: push notification to subscribed central.
        // updateValue returns false when the subscriber's buffer is full;
        // peripheralManagerIsReady(toUpdateSubscribers:) fires when it drains.
        if let central = addrToCentral[addr], let txChar = txCharacteristic {
            let ok = peripheralManager?.updateValue(data, for: txChar, onSubscribedCentrals: [central]) ?? false
            return ok
        }

        // Central role: write to peer's RX characteristic.
        // canSendWriteWithoutResponse goes false when the internal queue is full;
        // peripheral(_:isReadyToSendWriteWithoutResponse:) fires when it drains.
        if let peripheralUUID = addrToPeripheralUUID[addr],
           let peripheral = connectedPeripherals[peripheralUUID],
           let char = txCharacteristics[peripheralUUID] {
            guard peripheral.canSendWriteWithoutResponse else { return false }
            peripheral.writeValue(data, for: char, type: .withoutResponse)
            return true
        }

        return true // peer not found — frame consumed, no retry needed
    }

    /// Write KISS-framed data to all connected RNodes via NUS TX characteristic.
    /// Called by drainNusOutgoing() in LxmfModule.
    func sendToNus(_ data: Data) {
        for (uuid, char) in nusTxChars {
            if let peripheral = nusPeripherals[uuid] {
                // Chunk data to fit NUS MTU. CoreBluetooth negotiates MTU
                // automatically; maximumWriteValueLength gives the usable size.
                let mtu = peripheral.maximumWriteValueLength(for: .withoutResponse)
                if data.count <= mtu {
                    peripheral.writeValue(data, for: char, type: .withoutResponse)
                } else {
                    // Chunk into MTU-sized writes
                    var offset = 0
                    while offset < data.count {
                        let end = min(offset + mtu, data.count)
                        let chunk = data[offset..<end]
                        peripheral.writeValue(chunk, for: char, type: .withoutResponse)
                        offset = end
                    }
                }
            }
        }
    }

    /// Check if any RNode (NUS) peripherals are connected.
    var hasNusConnection: Bool {
        return !nusPeripherals.isEmpty
    }

    /// Derive a 6-byte pseudo-MAC from a CoreBluetooth UUID.
    /// XOR-folds the 16-byte UUID into 6 bytes for stable peer identification.
    static func uuidToAddr(_ uuid: UUID) -> Data {
        let u = uuid.uuid
        let bytes: [UInt8] = [u.0, u.1, u.2, u.3, u.4, u.5, u.6, u.7,
                              u.8, u.9, u.10, u.11, u.12, u.13, u.14, u.15]
        return Data([
            bytes[0] ^ bytes[6] ^ bytes[12],
            bytes[1] ^ bytes[7] ^ bytes[13],
            bytes[2] ^ bytes[8] ^ bytes[14],
            bytes[3] ^ bytes[9] ^ bytes[15],
            bytes[4] ^ bytes[10],
            bytes[5] ^ bytes[11],
        ])
    }

    // MARK: - Peripheral Setup

    private func setupPeripheral() {
        let rxChar = CBMutableCharacteristic(
            type: BLEManager.rxCharUUID,
            properties: [.write, .writeWithoutResponse],
            value: nil,
            permissions: [.writeable]
        )

        let txChar = CBMutableCharacteristic(
            type: BLEManager.txCharUUID,
            properties: [.notify, .read],
            value: nil,
            permissions: [.readable]
        )

        let service = CBMutableService(type: BLEManager.meshServiceUUID, primary: true)
        service.characteristics = [rxChar, txChar]

        self.rxCharacteristic = rxChar
        self.txCharacteristic = txChar

        peripheralManager.add(service)
    }

    private func startAdvertising() {
        peripheralManager.startAdvertising([
            CBAdvertisementDataServiceUUIDsKey: [BLEManager.meshServiceUUID],
            CBAdvertisementDataLocalNameKey: BLEManager.advertNamePrefix + instanceTokenHex
        ])
    }

    // MARK: - Central Setup

    private func startScanning() {
        centralManager.scanForPeripherals(
            withServices: [BLEManager.meshServiceUUID, BLEManager.nusServiceUUID],
            options: [CBCentralManagerScanOptionAllowDuplicatesKey: false]
        )
    }
}

// MARK: - CBCentralManagerDelegate

extension BLEManager: CBCentralManagerDelegate {
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state == .poweredOn && isRunning {
            startScanning()
        }
    }

    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any], rssi RSSI: NSNumber) {
        guard connectedPeripherals[peripheral.identifier] == nil else { return }

        // Self-loop filter: if the advertised local name carries our own
        // instance token, this is our own peripheral being seen by our own
        // central. Skip — CoreBluetooth does not auto-filter this.
        if let advertName = advertisementData[CBAdvertisementDataLocalNameKey] as? String,
           advertName == BLEManager.advertNamePrefix + instanceTokenHex {
            return
        }

        // Check if this is an RNode (NUS service) that we haven't paired with yet.
        // If so, don't auto-connect — the user needs to pair in iOS Settings first.
        let advertisedServices = advertisementData[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID] ?? []
        let isNus = advertisedServices.contains(BLEManager.nusServiceUUID)

        if isNus && !bondedPeripherals.contains(peripheral.identifier) {
            // Track as discovered-but-unpaired so UI can prompt user
            discoveredUnpairedRNodes[peripheral.identifier] = peripheral
            return
        }

        // Bonded RNode or mesh peer — connect normally
        connectedPeripherals[peripheral.identifier] = peripheral
        peripheral.delegate = self
        central.connect(peripheral, options: nil)
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        // Don't register with Rust yet — wait until service discovery tells us
        // whether this is a mesh peer (→ lxmf_ble_connected) or RNode (→ NUS path).
        connectedPeripherals[peripheral.identifier] = peripheral
        peripheral.discoverServices([BLEManager.meshServiceUUID, BLEManager.nusServiceUUID])
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        connectedPeripherals.removeValue(forKey: peripheral.identifier)
        // Retry after short delay for bonded devices
        if isRunning && bondedPeripherals.contains(peripheral.identifier) {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
                guard let self = self, self.isRunning else { return }
                central.connect(peripheral, options: nil)
            }
        }
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        let isNus = nusPeripherals[peripheral.identifier] != nil

        // Always remove from connectedPeripherals (used by both mesh and NUS)
        connectedPeripherals.removeValue(forKey: peripheral.identifier)

        if isNus {
            // RNode NUS disconnect
            nusPeripherals.removeValue(forKey: peripheral.identifier)
            nusTxChars.removeValue(forKey: peripheral.identifier)
        } else {
            // Mesh peer disconnect — notify Rust
            let addr = BLEManager.uuidToAddr(peripheral.identifier)
            addrToPeripheralUUID.removeValue(forKey: addr)
            addr.withUnsafeBytes { ptr in
                _ = lxmf_ble_disconnected(ptr.baseAddress?.assumingMemoryBound(to: UInt8.self))
            }
            txCharacteristics.removeValue(forKey: peripheral.identifier)
        }

        // Auto-reconnect bonded devices only
        if isRunning && bondedPeripherals.contains(peripheral.identifier) {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
                guard let self = self, self.isRunning else { return }
                central.connect(peripheral, options: nil)
            }
        }
    }

    // Background restoration
    func centralManager(_ central: CBCentralManager, willRestoreState dict: [String: Any]) {
        if let peripherals = dict[CBCentralManagerRestoredStatePeripheralsKey] as? [CBPeripheral] {
            for peripheral in peripherals {
                connectedPeripherals[peripheral.identifier] = peripheral
                peripheral.delegate = self
            }
        }
    }
}

// MARK: - CBPeripheralDelegate

extension BLEManager: CBPeripheralDelegate {
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard let services = peripheral.services else { return }
        for service in services {
            if service.uuid == BLEManager.nusServiceUUID {
                // RNode NUS — discover NUS characteristics
                peripheral.discoverCharacteristics(
                    [BLEManager.nusTxCharUUID, BLEManager.nusRxCharUUID],
                    for: service
                )
            } else if service.uuid == BLEManager.meshServiceUUID {
                // Phone-to-phone mesh — discover mesh characteristics
                peripheral.discoverCharacteristics(
                    [BLEManager.rxCharUUID, BLEManager.txCharUUID],
                    for: service
                )
            }
        }
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard let chars = service.characteristics else { return }

        if service.uuid == BLEManager.nusServiceUUID {
            // RNode NUS characteristics — mark as bonded (pairing succeeded)
            bondedPeripherals.insert(peripheral.identifier)
            saveBondedPeripherals()
            discoveredUnpairedRNodes.removeValue(forKey: peripheral.identifier)
            nusPeripherals[peripheral.identifier] = peripheral
            for char in chars {
                if char.uuid == BLEManager.nusTxCharUUID {
                    // NUS TX — we write TO the RNode on this char
                    nusTxChars[peripheral.identifier] = char
                } else if char.uuid == BLEManager.nusRxCharUUID {
                    // NUS RX — subscribe for notifications FROM RNode
                    peripheral.setNotifyValue(true, for: char)
                }
            }
            return
        }

        // Phone-to-phone mesh: store RX char and subscribe to TX notifications.
        // lxmf_ble_connected is deferred until didUpdateNotificationStateFor confirms
        // the CCCD write — otherwise Rust tries to TX before the pipe is open.
        bondedPeripherals.insert(peripheral.identifier)
        saveBondedPeripherals()
        let addr = BLEManager.uuidToAddr(peripheral.identifier)
        addrToPeripheralUUID[addr] = peripheral.identifier

        for char in chars {
            if char.uuid == BLEManager.rxCharUUID {
                txCharacteristics[peripheral.identifier] = char
            } else if char.uuid == BLEManager.txCharUUID {
                peripheral.setNotifyValue(true, for: char)
            }
        }
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard let value = characteristic.value, !value.isEmpty else { return }

        if characteristic.uuid == BLEManager.nusRxCharUUID {
            // Inbound data from RNode via NUS — push raw bytes into Rust NusInterface.
            // KISS deframing is handled on the Rust side (stateful).
            value.withUnsafeBytes { dataPtr in
                _ = lxmf_nus_receive(
                    dataPtr.baseAddress?.assumingMemoryBound(to: UInt8.self),
                    value.count
                )
            }
            return
        }

        // Inbound data from a mesh peer — push into Rust BleInterface
        let addr = BLEManager.uuidToAddr(peripheral.identifier)
        addr.withUnsafeBytes { addrPtr in
            value.withUnsafeBytes { dataPtr in
                _ = lxmf_ble_receive(
                    addrPtr.baseAddress?.assumingMemoryBound(to: UInt8.self),
                    dataPtr.baseAddress?.assumingMemoryBound(to: UInt8.self),
                    value.count
                )
            }
        }
    }

    // CCCD subscription confirmed (or failed) — now safe to register peer with Rust.
    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        guard characteristic.uuid == BLEManager.txCharUUID else { return }

        if let error = error {
            NSLog("[BLE] CCCD subscribe failed for %@: %@", peripheral.identifier.uuidString, error.localizedDescription)
            centralManager?.cancelPeripheralConnection(peripheral)
            return
        }
        guard characteristic.isNotifying else { return }

        let addr = BLEManager.uuidToAddr(peripheral.identifier)
        addr.withUnsafeBytes { ptr in
            _ = lxmf_ble_connected(ptr.baseAddress?.assumingMemoryBound(to: UInt8.self))
        }
        let writeLimit = peripheral.maximumWriteValueLength(for: .withoutResponse)
        addr.withUnsafeBytes { ptr in
            _ = lxmf_ble_mtu_negotiated(ptr.baseAddress?.assumingMemoryBound(to: UInt8.self), UInt32(writeLimit))
        }
        NSLog("[BLE] peer ready (client): %@, writeLimit=%d", peripheral.identifier.uuidString, writeLimit)
    }

    // Called when writeValue(.withoutResponse) exhausted the internal queue.
    // Re-trigger TX drain so buffered frames get sent now that there's room.
    func peripheralIsReady(toSendWriteWithoutResponse peripheral: CBPeripheral) {
        onReadyToSend?()
    }
}

// MARK: - CBPeripheralManagerDelegate

extension BLEManager: CBPeripheralManagerDelegate {
    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        if peripheral.state == .poweredOn && isRunning {
            setupPeripheral()
        }
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, didAdd service: CBService, error: Error?) {
        if error == nil {
            startAdvertising()
        }
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveWrite requests: [CBATTRequest]) {
        for request in requests {
            if request.characteristic.uuid == BLEManager.rxCharUUID,
               let value = request.value, !value.isEmpty {
                // Inbound write from a central peer — push into Rust BleInterface
                let addr = BLEManager.uuidToAddr(request.central.identifier)
                addr.withUnsafeBytes { addrPtr in
                    value.withUnsafeBytes { dataPtr in
                        _ = lxmf_ble_receive(
                            addrPtr.baseAddress?.assumingMemoryBound(to: UInt8.self),
                            dataPtr.baseAddress?.assumingMemoryBound(to: UInt8.self),
                            value.count
                        )
                    }
                }
            }
            peripheral.respond(to: request, withResult: .success)
        }
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, central: CBCentral,
                           didSubscribeTo characteristic: CBCharacteristic) {
        if !subscribedCentrals.contains(where: { $0.identifier == central.identifier }) {
            subscribedCentrals.append(central)
            // Register central as a peer with Rust
            let addr = BLEManager.uuidToAddr(central.identifier)
            addrToCentral[addr] = central
            addr.withUnsafeBytes { ptr in
                _ = lxmf_ble_connected(ptr.baseAddress?.assumingMemoryBound(to: UInt8.self))
            }
            // Report negotiated notification limit for this central.
            let writeLimit = central.maximumUpdateValueLength
            addr.withUnsafeBytes { ptr in
                _ = lxmf_ble_mtu_negotiated(ptr.baseAddress?.assumingMemoryBound(to: UInt8.self), UInt32(writeLimit))
            }
        }
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, central: CBCentral,
                           didUnsubscribeFrom characteristic: CBCharacteristic) {
        subscribedCentrals.removeAll { $0.identifier == central.identifier }
        // Notify Rust of central disconnection
        let addr = BLEManager.uuidToAddr(central.identifier)
        addrToCentral.removeValue(forKey: addr)
        addr.withUnsafeBytes { ptr in
            _ = lxmf_ble_disconnected(ptr.baseAddress?.assumingMemoryBound(to: UInt8.self))
        }
    }

    // Called when updateValue returned false and the subscriber buffer has drained.
    func peripheralManagerIsReady(toUpdateSubscribers peripheral: CBPeripheralManager) {
        onReadyToSend?()
    }

    // Background restoration
    func peripheralManager(_ peripheral: CBPeripheralManager, willRestoreState dict: [String: Any]) {
        // Re-setup services on restoration
        if isRunning {
            setupPeripheral()
        }
    }
}
