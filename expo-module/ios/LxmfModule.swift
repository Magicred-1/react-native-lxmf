import ExpoModulesCore
import CoreBluetooth

// C FFI declarations — linked from the Rust staticlib (liblxmf_rn.a)
@_silgen_name("lxmf_init")
func lxmf_init(_ dbPath: UnsafePointer<CChar>?) -> Int32

@_silgen_name("lxmf_start")
func lxmf_start(
    _ identityHex: UnsafePointer<CChar>?,
    _ addressHex: UnsafePointer<CChar>?,
    _ mode: UInt32,
    _ announceIntervalMs: UInt64,
    _ bleMtuHint: UInt16,
    _ tcpInterfacesJson: UnsafePointer<CChar>?,
    _ displayName: UnsafePointer<CChar>?,
    _ isBeacon: UInt8
) -> Int32

@_silgen_name("lxmf_stop")
func lxmf_stop() -> Int32

@_silgen_name("lxmf_is_running")
func lxmf_is_running() -> Int32

@_silgen_name("lxmf_send")
func lxmf_send(
    _ destPtr: UnsafePointer<UInt8>?,
    _ bodyPtr: UnsafePointer<UInt8>?,
    _ bodyLen: Int,
    _ fieldsJson: UnsafePointer<CChar>?
) -> Int64

@_silgen_name("lxmf_broadcast")
func lxmf_broadcast(
    _ destsPtr: UnsafePointer<UInt8>?,
    _ destCount: Int,
    _ bodyPtr: UnsafePointer<UInt8>?,
    _ bodyLen: Int,
    _ fieldsJson: UnsafePointer<CChar>?
) -> Int64

@_silgen_name("lxmf_poll_events")
func lxmf_poll_events(
    _ timeoutMs: UInt64,
    _ outBuf: UnsafeMutablePointer<UInt8>?,
    _ outCapacity: Int
) -> Int32

@_silgen_name("lxmf_get_identity_hex")
func lxmf_get_identity_hex(
    _ outBuf: UnsafeMutablePointer<UInt8>?,
    _ outCapacity: Int
) -> Int32

@_silgen_name("lxmf_get_status")
func lxmf_get_status(
    _ outBuf: UnsafeMutablePointer<UInt8>?,
    _ outCapacity: Int
) -> Int32

@_silgen_name("lxmf_get_beacons")
func lxmf_get_beacons(
    _ outBuf: UnsafeMutablePointer<UInt8>?,
    _ outCapacity: Int
) -> Int32

@_silgen_name("lxmf_on_announce")
func lxmf_on_announce(
    _ destHashPtr: UnsafePointer<UInt8>?,
    _ appDataPtr: UnsafePointer<UInt8>?,
    _ appDataLen: Int
) -> Int32

@_silgen_name("lxmf_set_log_level")
func lxmf_set_log_level(_ level: UInt32) -> Int32

@_silgen_name("lxmf_abi_version")
func lxmf_abi_version() -> UInt32

@_silgen_name("lxmf_fetch_messages")
func lxmf_fetch_messages(
    _ limit: UInt32,
    _ outBuf: UnsafeMutablePointer<UInt8>?,
    _ outCapacity: Int
) -> Int32

// --- BLE Interface FFI ---

@_silgen_name("lxmf_ble_receive")
func lxmf_ble_receive(
    _ peerAddr: UnsafePointer<UInt8>?,
    _ data: UnsafePointer<UInt8>?,
    _ dataLen: Int
) -> Int32

@_silgen_name("lxmf_ble_poll_tx")
func lxmf_ble_poll_tx(
    _ outPeer: UnsafeMutablePointer<UInt8>?,
    _ outData: UnsafeMutablePointer<UInt8>?,
    _ outCapacity: Int
) -> Int32

@_silgen_name("lxmf_ble_connected")
func lxmf_ble_connected(
    _ peerAddr: UnsafePointer<UInt8>?
) -> Int32

@_silgen_name("lxmf_ble_disconnected")
func lxmf_ble_disconnected(
    _ peerAddr: UnsafePointer<UInt8>?
) -> Int32

@_silgen_name("lxmf_ble_peer_count")
func lxmf_ble_peer_count() -> Int32

@_silgen_name("lxmf_ble_mtu_negotiated")
func lxmf_ble_mtu_negotiated(
    _ peerAddr: UnsafePointer<UInt8>?,
    _ writeLimit: UInt32
) -> Int32

// --- NUS Interface FFI (RNode BLE via Nordic UART Service) ---

@_silgen_name("lxmf_nus_receive")
func lxmf_nus_receive(
    _ data: UnsafePointer<UInt8>?,
    _ dataLen: Int
) -> Int32

@_silgen_name("lxmf_nus_poll_tx")
func lxmf_nus_poll_tx(
    _ outData: UnsafeMutablePointer<UInt8>?,
    _ outCapacity: Int
) -> Int32

// --- Beacon RPC FFI ---

@_silgen_name("lxmf_beacon_rpc")
func lxmf_beacon_rpc(
    _ destHashHex: UnsafePointer<CChar>?,
    _ method: UnsafePointer<CChar>?,
    _ paramsJson: UnsafePointer<CChar>?
) -> Int64

@_silgen_name("lxmf_partial_sign_execute_payment")
func lxmf_partial_sign_execute_payment(
    _ payerKey: UnsafePointer<UInt8>?,
    _ nonceBh: UnsafePointer<UInt8>?,
    _ accountsJson: UnsafePointer<CChar>?,
    _ paramsJson: UnsafePointer<CChar>?,
    _ outBuf: UnsafeMutablePointer<UInt8>?,
    _ outCap: Int32
) -> Int32

@_silgen_name("lxmf_extract_nonce_blockhash")
func lxmf_extract_nonce_blockhash(
    _ accountDataB64: UnsafePointer<CChar>?,
    _ outBuf: UnsafeMutablePointer<UInt8>?,
    _ outCap: Int32
) -> Int32

@_silgen_name("lxmf_set_program_id")
func lxmf_set_program_id(_ programIdHex: UnsafePointer<CChar>?) -> Int32

@_silgen_name("lxmf_get_program_id")
func lxmf_get_program_id(
    _ outBuf: UnsafeMutablePointer<UInt8>?,
    _ outCap: Int
) -> Int32

@_silgen_name("lxmf_beacon_set_keypair")
func lxmf_beacon_set_keypair(
    _ keyBytes: UnsafePointer<UInt8>?,
    _ len: Int32
) -> Int32

@_silgen_name("lxmf_beacon_set_solana_rpc_url")
func lxmf_beacon_set_solana_rpc_url(
    _ url: UnsafePointer<CChar>?
) -> Int32

@_silgen_name("lxmf_create_group")
func lxmf_create_group(
    _ name: UnsafePointer<CChar>?,
    _ keyHex: UnsafePointer<CChar>?,
    _ outAddrBuf: UnsafeMutablePointer<UInt8>?,
    _ outAddrLen: Int
) -> Int32

@_silgen_name("lxmf_join_group")
func lxmf_join_group(
    _ addrHex: UnsafePointer<CChar>?,
    _ keyHex: UnsafePointer<CChar>?
) -> Int32

@_silgen_name("lxmf_leave_group")
func lxmf_leave_group(_ addrHex: UnsafePointer<CChar>?) -> Int32

@_silgen_name("lxmf_send_group")
func lxmf_send_group(
    _ addrHex: UnsafePointer<CChar>?,
    _ bodyPtr: UnsafePointer<UInt8>?,
    _ bodyLen: Int,
    _ fieldsJson: UnsafePointer<CChar>?
) -> Int64


public class LxmfModule: Module {
    // Shared JSON buffer for FFI calls (64KB)
    private var jsonBuf = [UInt8](repeating: 0, count: 65536)

    // Adaptive poll state
    private var isPolling = false
    private var emptyRxCount = 0
    private var currentRxInterval: TimeInterval = 0.016
    private var emptyTxCount = 0
    private var currentTxInterval: TimeInterval = 0.02

    // BLE manager for phone-to-phone mesh
    private lazy var bleManager: BLEManager = {
        let mgr = BLEManager()
        mgr.onReadyToSend = { [weak self] in DispatchQueue.main.async { self?.drainOutgoing() } }
        return mgr
    }()

    public func definition() -> ModuleDefinition {
        Name("LxmfModule")

        // --- Events emitted to JavaScript ---
        Events(
            "onPacketReceived",
            "onTxReceived",
            "onBeaconDiscovered",
            "onMessageReceived",
            "onAnnounceReceived",
            "onStatusChanged",
            "onRpcResponse",
            "onMessageQueued",
            "onMessageDelivered",
            "onMessageFailed",
            "onLog",
            "onError",
            "onOutgoingPacket"
        )

        // --- Lifecycle ---

        Function("init") { (dbPath: String?) -> Bool in
            let result: Int32
            if let path = dbPath {
                result = path.withCString { lxmf_init($0) }
            } else {
                result = lxmf_init(nil)
            }
            return result == 0
        }

        AsyncFunction("start") { (
            identityHex: String,
            lxmfAddressHex: String,
            mode: Int,
            announceIntervalMs: Double,
            bleMtuHint: Int,
            tcpInterfaces: [[String: Any]],
            displayName: String,
            isBeacon: Bool
        ) -> Bool in
            // Serialize TCP interfaces to JSON (matches Android pattern)
            let interfacesJson: String
            if let data = try? JSONSerialization.data(withJSONObject: tcpInterfaces),
               let str = String(data: data, encoding: .utf8) {
                interfacesJson = str
            } else {
                interfacesJson = "[]"
            }

            let result = identityHex.withCString { idPtr in
                lxmfAddressHex.withCString { addrPtr in
                    interfacesJson.withCString { ifacesPtr in
                        displayName.withCString { namePtr in
                            lxmf_start(
                                idPtr, addrPtr,
                                UInt32(mode), UInt64(announceIntervalMs),
                                UInt16(bleMtuHint), ifacesPtr, namePtr,
                                isBeacon ? 1 : 0
                            )
                        }
                    }
                }
            }

            if result == 0 {
                self.startPolling()
                self.bleManager.start()
            }

            return result == 0
        }

        AsyncFunction("stop") { () -> Bool in
            self.stopPolling()
            self.bleManager.stop()
            return lxmf_stop() == 0
        }

        Function("isRunning") { () -> Bool in
            return lxmf_is_running() != 0
        }

        // --- Messaging ---

        AsyncFunction("send") { (destHex: String, bodyBase64: String, fieldsJson: String?) -> Double in
            guard let destBytes = Self.hexToBytes(destHex),
                  destBytes.count == 16,
                  let bodyData = Data(base64Encoded: bodyBase64) else {
                return -1
            }

            let opId: Int64
            if let json = fieldsJson {
                opId = destBytes.withUnsafeBufferPointer { destBuf in
                    [UInt8](bodyData).withUnsafeBufferPointer { bodyBuf in
                        json.withCString { jsonPtr in
                            lxmf_send(destBuf.baseAddress, bodyBuf.baseAddress, bodyData.count, jsonPtr)
                        }
                    }
                }
            } else {
                opId = destBytes.withUnsafeBufferPointer { destBuf in
                    [UInt8](bodyData).withUnsafeBufferPointer { bodyBuf in
                        lxmf_send(destBuf.baseAddress, bodyBuf.baseAddress, bodyData.count, nil)
                    }
                }
            }
            return Double(opId)
        }

        AsyncFunction("broadcast") { (destsHex: [String], bodyBase64: String, fieldsJson: String?) -> Double in
            guard let bodyData = Data(base64Encoded: bodyBase64) else { return -1 }

            var flatDests = [UInt8]()
            for hex in destsHex {
                guard let bytes = Self.hexToBytes(hex), bytes.count == 16 else { return -1 }
                flatDests.append(contentsOf: bytes)
            }

            let opId: Int64
            if let json = fieldsJson {
                opId = flatDests.withUnsafeBufferPointer { destBuf in
                    [UInt8](bodyData).withUnsafeBufferPointer { bodyBuf in
                        json.withCString { jsonPtr in
                            lxmf_broadcast(destBuf.baseAddress, destsHex.count, bodyBuf.baseAddress, bodyData.count, jsonPtr)
                        }
                    }
                }
            } else {
                opId = flatDests.withUnsafeBufferPointer { destBuf in
                    [UInt8](bodyData).withUnsafeBufferPointer { bodyBuf in
                        lxmf_broadcast(destBuf.baseAddress, destsHex.count, bodyBuf.baseAddress, bodyData.count, nil)
                    }
                }
            }
            return Double(opId)
        }

        // --- Identity ---

        Function("getIdentityHex") { () -> String? in
            // 128 hex chars (full private key) — small, dedicated buffer to avoid
            // sharing with the larger status JSON buffer.
            var buf = [UInt8](repeating: 0, count: 256)
            let len = buf.withUnsafeMutableBufferPointer { ptr in
                lxmf_get_identity_hex(ptr.baseAddress, ptr.count)
            }
            guard len > 0 else { return nil }
            return String(bytes: buf[0..<Int(len)], encoding: .utf8)
        }

        // --- Status & Beacons ---

        Function("getStatus") { () -> String? in
            return self.callJsonFfi { buf, cap in lxmf_get_status(buf, cap) }
        }

        Function("getBeacons") { () -> String? in
            return self.callJsonFfi { buf, cap in lxmf_get_beacons(buf, cap) }
        }

        Function("fetchMessages") { (limit: Int) -> String? in
            return self.callJsonFfi { buf, cap in lxmf_fetch_messages(UInt32(limit), buf, cap) }
        }

        // --- Configuration ---

        Function("setLogLevel") { (level: Int) -> Bool in
            return lxmf_set_log_level(UInt32(level)) == 0
        }

        Function("abiVersion") { () -> Int in
            return Int(lxmf_abi_version())
        }

        // --- BLE interface control ---

        Function("startBLE") { () -> Void in
            self.bleManager.start()
        }

        Function("stopBLE") { () -> Void in
            self.bleManager.stop()
        }

        Function("blePeerCount") { () -> Int in
            return Int(lxmf_ble_peer_count())
        }

        Function("bleUnpairedRNodeCount") { () -> Int in
            return self.bleManager.discoveredUnpairedRNodes.count
        }

        // --- Beacon RPC ---

        AsyncFunction("beaconRpc") { (destHashHex: String, method: String, paramsJson: String?) -> Double in
            let id = destHashHex.withCString { destPtr in
                method.withCString { methodPtr in
                    if let p = paramsJson {
                        return p.withCString { paramsPtr in
                            lxmf_beacon_rpc(destPtr, methodPtr, paramsPtr)
                        }
                    }
                    return lxmf_beacon_rpc(destPtr, methodPtr, nil)
                }
            }
            return Double(id)
        }

        // --- RNode pairing (NUS) ---

        Function("getNusUnpairedRNodes") { () -> String in
            return self.bleManager.unpairedRNodesJson()
        }

        // On iOS, "pairing" = connect (CoreBluetooth handles encryption/bonding transparently).
        // The identifier is a CoreBluetooth UUID string, not a MAC (iOS hides MACs since iOS 13).
        Function("pairNusRNode") { (identifier: String) -> Bool in
            return self.bleManager.connectRNode(identifier)
        }

        // --- Solana tx building ---

        Function("partialSignExecutePayment") { (payerKeyHex: String, nonceBlockhashHex: String,
                                                  accountsJson: String, paramsJson: String) -> String? in
            guard payerKeyHex.count == 64 else { return nil }
            guard nonceBlockhashHex.count == 64 else { return nil }
            var payerKey = [UInt8]()
            var idx = payerKeyHex.startIndex
            while idx < payerKeyHex.endIndex {
                let next = payerKeyHex.index(idx, offsetBy: 2)
                guard let byte = UInt8(payerKeyHex[idx..<next], radix: 16) else {
                    for i in 0..<payerKey.count { payerKey[i] = 0 }
                    return nil
                }
                payerKey.append(byte)
                idx = next
            }
            var nonceKey = [UInt8]()
            var idx2 = nonceBlockhashHex.startIndex
            while idx2 < nonceBlockhashHex.endIndex {
                let next2 = nonceBlockhashHex.index(idx2, offsetBy: 2)
                guard let byte = UInt8(nonceBlockhashHex[idx2..<next2], radix: 16) else {
                    for i in 0..<payerKey.count { payerKey[i] = 0 }
                    return nil
                }
                nonceKey.append(byte)
                idx2 = next2
            }
            var outBuf = [UInt8](repeating: 0, count: 1024)
            let written = payerKey.withUnsafeBufferPointer { pkPtr in
                nonceKey.withUnsafeBufferPointer { nhPtr in
                    accountsJson.withCString { accts in
                        paramsJson.withCString { prms in
                            outBuf.withUnsafeMutableBufferPointer { outPtr in
                                lxmf_partial_sign_execute_payment(
                                    pkPtr.baseAddress, nhPtr.baseAddress,
                                    accts, prms,
                                    outPtr.baseAddress, Int32(outBuf.count))
                            }
                        }
                    }
                }
            }
            for i in 0..<payerKey.count { payerKey[i] = 0 }
            guard written > 0 else { return nil }
            return String(bytes: outBuf[0..<Int(written)], encoding: .utf8)
        }

        Function("extractNonceBlockhash") { (accountDataB64: String) -> String? in
            var outBuf = [UInt8](repeating: 0, count: 64)
            let written = accountDataB64.withCString { dataPtr in
                outBuf.withUnsafeMutableBufferPointer { outPtr in
                    lxmf_extract_nonce_blockhash(dataPtr, outPtr.baseAddress, Int32(outBuf.count))
                }
            }
            guard written == 64 else { return nil }
            return String(bytes: outBuf, encoding: .utf8)
        }

        // --- Beacon configuration ---

        Function("setProgramId") { (programIdHex: String) -> Bool in
            return programIdHex.withCString { lxmf_set_program_id($0) == 0 }
        }

        Function("getProgramId") { () -> String? in
            var out = [UInt8](repeating: 0, count: 64)
            let n = out.withUnsafeMutableBufferPointer { lxmf_get_program_id($0.baseAddress, 64) }
            guard n == 64 else { return nil }
            return String(bytes: out, encoding: .utf8)
        }

        Function("setBeaconKeypair") { (keyHex: String) -> Bool in
            guard keyHex.count == 64 || keyHex.count == 128 else { return false }
            var bytes = [UInt8]()
            var idx = keyHex.startIndex
            while idx < keyHex.endIndex {
                let next = keyHex.index(idx, offsetBy: 2)
                guard let byte = UInt8(keyHex[idx..<next], radix: 16) else {
                    for i in 0..<bytes.count { bytes[i] = 0 }
                    return false
                }
                bytes.append(byte)
                idx = next
            }
            let ok = bytes.withUnsafeBufferPointer { ptr in
                lxmf_beacon_set_keypair(ptr.baseAddress, Int32(bytes.count)) == 0
            }
            for i in 0..<bytes.count { bytes[i] = 0 }
            return ok
        }

        Function("setBeaconSolanaRpc") { (url: String) -> Bool in
            return url.withCString { lxmf_beacon_set_solana_rpc_url($0) == 0 }
        }

        // --- Group Chat ---

        Function("createGroup") { (name: String, keyHex: String) -> String in
            var addrBuf = [UInt8](repeating: 0, count: 33)
            let rc = name.withCString { namePtr in
                keyHex.withCString { keyPtr in
                    lxmf_create_group(namePtr, keyPtr, &addrBuf, 33)
                }
            }
            guard rc == 0 else { throw NSError(domain: "LxmfGroup", code: -1) }
            return String(bytes: addrBuf.prefix(while: { $0 != 0 }), encoding: .utf8) ?? ""
        }

        Function("joinGroup") { (addrHex: String, keyHex: String) -> Bool in
            let rc = addrHex.withCString { addrPtr in
                keyHex.withCString { keyPtr in
                    lxmf_join_group(addrPtr, keyPtr)
                }
            }
            return rc == 0
        }

        Function("leaveGroup") { (addrHex: String) -> Bool in
            let rc = addrHex.withCString { lxmf_leave_group($0) }
            return rc == 0
        }

        AsyncFunction("sendGroup") { (addrHex: String, bodyBase64: String, fieldsJson: String?) -> Double in
            guard let bodyData = Data(base64Encoded: bodyBase64) else {
                throw NSError(domain: "LxmfGroup", code: -2, userInfo: [NSLocalizedDescriptionKey: "invalid base64 body"])
            }
            let seq = addrHex.withCString { addrPtr in
                bodyData.withUnsafeBytes { bodyBuf -> Int64 in
                    guard let bodyPtr = bodyBuf.baseAddress?.assumingMemoryBound(to: UInt8.self) else { return -1 }
                    if let fields = fieldsJson {
                        return fields.withCString { fieldsPtr in
                            lxmf_send_group(addrPtr, bodyPtr, bodyData.count, fieldsPtr)
                        }
                    }
                    return lxmf_send_group(addrPtr, bodyPtr, bodyData.count, nil)
                }
            }
            if seq < 0 { throw NSError(domain: "LxmfGroup", code: -3) }
            return Double(seq)
        }
    }

    // MARK: - Polling

    private func startPolling() {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            self.isPolling = true
            self.emptyRxCount = 0
            self.currentRxInterval = 0.016
            self.emptyTxCount = 0
            self.currentTxInterval = 0.02
            self.scheduleRxPoll()
            self.scheduleTxDrain()
        }
    }

    private func stopPolling() {
        isPolling = false
    }

    private func scheduleRxPoll() {
        DispatchQueue.main.asyncAfter(deadline: .now() + currentRxInterval) { [weak self] in
            guard let self = self, self.isPolling else { return }
            let hadEvents = self.drainEvents()
            if hadEvents {
                self.emptyRxCount = 0
                self.currentRxInterval = 0.016
            } else if self.emptyRxCount >= 5 {
                self.currentRxInterval = min(self.currentRxInterval * 2, 0.5)
            } else {
                self.emptyRxCount += 1
            }
            self.scheduleRxPoll()
        }
    }

    private func scheduleTxDrain() {
        DispatchQueue.main.asyncAfter(deadline: .now() + currentTxInterval) { [weak self] in
            guard let self = self, self.isPolling else { return }
            let hadFrames = self.drainOutgoing()
            if hadFrames {
                self.emptyTxCount = 0
                self.currentTxInterval = 0.02
            } else if self.emptyTxCount >= 5 {
                self.currentTxInterval = min(self.currentTxInterval * 2, 0.5)
            } else {
                self.emptyTxCount += 1
            }
            self.scheduleTxDrain()
        }
    }

    @discardableResult
    private func drainEvents() -> Bool {
        let len = jsonBuf.withUnsafeMutableBufferPointer { buf in
            lxmf_poll_events(0, buf.baseAddress, buf.count)
        }

        guard len > 0 else { return false }

        let jsonData = Data(jsonBuf[0..<Int(len)])
        guard let events = try? JSONSerialization.jsonObject(with: jsonData) as? [[String: Any]] else { return false }

        for event in events {
            guard let type_ = event["type"] as? String else { continue }

            switch type_ {
            case "statusChanged":
                sendEvent("onStatusChanged", event)
            case "packetReceived":
                sendEvent("onPacketReceived", event)
            case "txReceived":
                sendEvent("onTxReceived", event)
            case "beaconDiscovered":
                sendEvent("onBeaconDiscovered", event)
            case "messageReceived":
                sendEvent("onMessageReceived", event)
            case "announceReceived":
                sendEvent("onAnnounceReceived", event)
            case "rpcResponse":
                sendEvent("onRpcResponse", event)
            case "messageQueued":
                sendEvent("onMessageQueued", event)
            case "messageDelivered":
                sendEvent("onMessageDelivered", event)
            case "messageFailed":
                sendEvent("onMessageFailed", event)
            case "log":
                sendEvent("onLog", event)
            case "error":
                sendEvent("onError", event)
            default:
                break
            }
        }
        return !events.isEmpty
    }

    @discardableResult
    private func drainOutgoing() -> Bool {
        var sentAny = false

        // --- Mesh BLE: poll for peer-addressed frames ---
        var peerAddr = [UInt8](repeating: 0, count: 6)
        var dataBuf = [UInt8](repeating: 0, count: 512)

        for _ in 0..<8 {
            let len = peerAddr.withUnsafeMutableBufferPointer { peerBuf in
                dataBuf.withUnsafeMutableBufferPointer { dataBuf in
                    lxmf_ble_poll_tx(peerBuf.baseAddress, dataBuf.baseAddress, dataBuf.count)
                }
            }
            guard len > 0 else { break }

            let frameData = Data(dataBuf[0..<Int(len)])
            let addr = Data(peerAddr)
            sentAny = true
            // Stop draining if CoreBluetooth buffer is full — onReadyToSend re-triggers us.
            guard bleManager.sendToPeerAddr(addr, data: frameData) else { break }
        }

        // --- NUS: poll for KISS-framed RNode data ---
        var nusBuf = [UInt8](repeating: 0, count: 1024)
        for _ in 0..<8 {
            let len = nusBuf.withUnsafeMutableBufferPointer { buf in
                lxmf_nus_poll_tx(buf.baseAddress, buf.count)
            }
            guard len > 0 else { break }

            let kissData = Data(nusBuf[0..<Int(len)])
            bleManager.sendToNus(kissData)
            sentAny = true
        }

        return sentAny
    }

    // MARK: - Helpers

    private func callJsonFfi(_ fn_: (UnsafeMutablePointer<UInt8>?, Int) -> Int32) -> String? {
        let len = jsonBuf.withUnsafeMutableBufferPointer { buf in
            fn_(buf.baseAddress, buf.count)
        }
        guard len > 0 else { return nil }
        return String(bytes: jsonBuf[0..<Int(len)], encoding: .utf8)
    }

    static func hexToBytes(_ hex: String) -> [UInt8]? {
        let chars = Array(hex)
        guard chars.count % 2 == 0 else { return nil }
        var bytes = [UInt8]()
        bytes.reserveCapacity(chars.count / 2)
        for i in stride(from: 0, to: chars.count, by: 2) {
            guard let byte = UInt8(String(chars[i...i+1]), radix: 16) else { return nil }
            bytes.append(byte)
        }
        return bytes
    }
}
