package expo.modules.lxmf

import android.bluetooth.*
import android.bluetooth.le.*
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

private const val NUS_TAG = "LxmfNus"

// NUS GATT UUIDs — must match nus_iface.rs constants exactly
val NUS_SERVICE_UUID: UUID = UUID.fromString("6e400001-b5a3-f393-e0a9-e50e24dcca9e")
val NUS_TX_CHAR_UUID: UUID = UUID.fromString("6e400002-b5a3-f393-e0a9-e50e24dcca9e") // phone writes TO RNode
val NUS_RX_CHAR_UUID: UUID = UUID.fromString("6e400003-b5a3-f393-e0a9-e50e24dcca9e") // phone receives FROM RNode

/**
 * NusManager — Android BLE client for RNode hardware (Heltec V3 etc.) via Nordic UART Service.
 *
 * Mirrors iOS BLEManager NUS logic:
 *   - Scans for NUS service UUID
 *   - If device is OS-paired (BOND_BONDED): connects and sets up KISS pipe to Rust NusInterface
 *   - If not paired: tracks as discoveredUnpaired (bleUnpairedRNodeCount returns this count)
 *   - RX notifications → nativeNusReceive() → Rust NusInterface (KISS deframing + transport)
 *   - TX polling → nativeNusPollTx() → write chunked to RNode's NUS TX characteristic
 *
 * Separate from BleManager (mesh peers) — two different scan filters, two GATT roles.
 */
class NusManager(
    private val context: Context,
    private val module: LxmfModule,
) {
    private val bluetoothManager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
    private val adapter: BluetoothAdapter? get() = bluetoothManager?.adapter
    private val mainHandler = Handler(Looper.getMainLooper())

    // Active GATT connections to bonded RNodes, keyed by MAC
    private val connections = mutableMapOf<String, BluetoothGatt>()
    // NUS TX characteristics (phone writes TO RNode), keyed by MAC
    private val txChars = mutableMapOf<String, BluetoothGattCharacteristic>()
    // Negotiated write MTU per connection — used for TX chunking
    private val writeMtu = mutableMapOf<String, Int>()
    // Devices found in scan but not OS-paired — keyed by MAC for O(1) lookup
    private val discoveredUnpaired = mutableMapOf<String, BluetoothDevice>()
    // MACs currently attempting connection
    private val connecting = mutableSetOf<String>()

    private var scanner: BluetoothLeScanner? = null
    private var isScanning = false
    private var isRunning = false

    companion object {
        private const val SCAN_RESTART_DELAY_MS = 30_000L
        private const val RECONNECT_DELAY_MS = 3_000L
        private const val DEFAULT_WRITE_MTU = 20  // conservative BLE default
    }

    // ── Bond state receiver ───────────────────────────────────────────────────

    private val bondReceiver = object : BroadcastReceiver() {
        override fun onReceive(ctx: Context, intent: Intent) {
            if (intent.action != BluetoothDevice.ACTION_BOND_STATE_CHANGED) return
            val device: BluetoothDevice? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE, BluetoothDevice::class.java)
            } else {
                @Suppress("DEPRECATION")
                intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE)
            }
            val mac = device?.address ?: return
            val bondState = intent.getIntExtra(BluetoothDevice.EXTRA_BOND_STATE, BluetoothDevice.BOND_NONE)
            if (bondState == BluetoothDevice.BOND_BONDED) {
                Log.i(NUS_TAG, "NUS: $mac bonded — connecting")
                discoveredUnpaired.remove(mac)
                if (isRunning && mac !in connections && mac !in connecting) {
                    connecting.add(mac)
                    device.connectGatt(context, false, gattCallback, BluetoothDevice.TRANSPORT_LE)
                }
            }
        }
    }

    // ── Lifecycle ────────────────────────────────────────────────────────────

    fun start() {
        if (isRunning) return
        isRunning = true
        val filter = IntentFilter(BluetoothDevice.ACTION_BOND_STATE_CHANGED)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.registerReceiver(bondReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            context.registerReceiver(bondReceiver, filter)
        }
        startScanning()
        Log.i(NUS_TAG, "NusManager started")
    }

    fun stop() {
        isRunning = false
        try { context.unregisterReceiver(bondReceiver) } catch (_: Exception) {}
        stopScanning()
        connections.values.forEach { it.disconnect(); it.close() }
        connections.clear()
        txChars.clear()
        writeMtu.clear()
        connecting.clear()
        discoveredUnpaired.clear()
        Log.i(NUS_TAG, "NusManager stopped")
    }

    /** Number of RNodes visible in scan but not yet OS-paired. */
    fun unpairedRNodeCount(): Int = discoveredUnpaired.size

    /** JSON array of unpaired RNodes: [{"mac":"AA:BB:...","name":"RNode_1234"},...] */
    fun unpairedRNodesJson(): String {
        val arr = JSONArray()
        discoveredUnpaired.values.forEach { device ->
            arr.put(JSONObject().apply {
                put("mac", device.address)
                put("name", device.name ?: "")
            })
        }
        return arr.toString()
    }

    /**
     * Initiate OS pairing with an unpaired RNode. Shows system Bluetooth pairing dialog.
     * Returns true if bond initiation succeeded (or device is already bonded and connecting).
     * Requires BLUETOOTH_CONNECT permission (API 31+).
     */
    fun pairRNode(mac: String): Boolean {
        val device = discoveredUnpaired[mac]
            ?: try { adapter?.getRemoteDevice(mac) } catch (_: Exception) { null }
            ?: return false
        return if (device.bondState == BluetoothDevice.BOND_BONDED) {
            if (mac !in connections && mac !in connecting) {
                connecting.add(mac)
                device.connectGatt(context, false, gattCallback, BluetoothDevice.TRANSPORT_LE)
            }
            true
        } else {
            device.createBond()
        }
    }

    /** Number of fully connected and ready RNodes. */
    fun connectedRNodeCount(): Int = connections.size

    /**
     * Drain Rust NUS TX queue and write each frame to all connected RNodes.
     * Called from LxmfModule poll runnable at the same 80ms cadence as event polling.
     */
    fun pollTxAndWrite() {
        if (connections.isEmpty()) return
        repeat(8) {
            val data = module.nativeNusPollTx() ?: return
            for ((mac, char) in txChars) {
                val gatt = connections[mac] ?: return@repeat
                writeChunked(gatt, char, data, writeMtu[mac] ?: DEFAULT_WRITE_MTU)
            }
        }
    }

    private fun writeChunked(
        gatt: BluetoothGatt,
        char: BluetoothGattCharacteristic,
        data: ByteArray,
        mtu: Int,
    ) {
        // Android GATT is single-op — only one writeCharacteristic in flight at a time.
        // For NUS, packets are bounded by NusInterface.mtu (244 B) so KISS frames fit in
        // one ATT PDU after MTU negotiation (514 B). The loop is a safety net only.
        var offset = 0
        while (offset < data.size) {
            val end = minOf(offset + mtu, data.size)
            val chunk = data.copyOfRange(offset, end)
            val ok = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                gatt.writeCharacteristic(char, chunk, BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE) ==
                    BluetoothGatt.GATT_SUCCESS
            } else {
                @Suppress("DEPRECATION")
                char.value = chunk
                char.writeType = BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE
                @Suppress("DEPRECATION")
                gatt.writeCharacteristic(char)
            }
            if (!ok) {
                Log.w(NUS_TAG, "NUS TX write rejected at offset $offset — ${data.size - offset}B dropped")
                return
            }
            offset = end
        }
    }

    // ── Scanning ─────────────────────────────────────────────────────────────

    private fun startScanning() {
        if (isScanning) return
        scanner = adapter?.bluetoothLeScanner ?: return
        val filter = ScanFilter.Builder()
            .setServiceUuid(ParcelUuid(NUS_SERVICE_UUID))
            .build()
        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_BALANCED)
            .build()
        scanner?.startScan(listOf(filter), settings, scanCallback)
        isScanning = true
        Log.d(NUS_TAG, "NUS scan started")
    }

    private fun stopScanning() {
        if (isScanning) {
            scanner?.stopScan(scanCallback)
            isScanning = false
        }
    }

    private val scanCallback = object : ScanCallback() {
        override fun onScanResult(callbackType: Int, result: ScanResult) {
            val device = result.device ?: return
            val mac = device.address ?: return
            if (mac in connections || mac in connecting) return

            when (device.bondState) {
                BluetoothDevice.BOND_BONDED -> {
                    // OS-paired — safe to connect and use NUS pipe
                    discoveredUnpaired.remove(mac)
                    Log.i(NUS_TAG, "NUS: found bonded RNode $mac, connecting")
                    connecting.add(mac)
                    device.connectGatt(context, false, gattCallback, BluetoothDevice.TRANSPORT_LE)
                }
                else -> {
                    // Not paired — track for UI; call pairRNode(mac) to initiate pairing
                    if (discoveredUnpaired.put(mac, device) == null) {
                        Log.i(NUS_TAG, "NUS: found unpaired RNode $mac (${device.name ?: "?"}) — call pairRNode()")
                    }
                }
            }
        }

        override fun onScanFailed(errorCode: Int) {
            Log.e(NUS_TAG, "NUS scan failed: $errorCode")
            isScanning = false
            mainHandler.postDelayed({ startScanning() }, SCAN_RESTART_DELAY_MS)
        }
    }

    // ── GATT ─────────────────────────────────────────────────────────────────

    private val gattCallback = object : BluetoothGattCallback() {
        override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
            val mac = gatt.device.address
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> {
                    Log.i(NUS_TAG, "NUS GATT connected: $mac")
                    connections[mac] = gatt
                    connecting.remove(mac)
                    discoveredUnpaired.remove(mac)
                    // Negotiate large MTU before service discovery
                    gatt.requestMtu(517)
                }
                BluetoothProfile.STATE_DISCONNECTED -> {
                    if (mac !in connections && mac !in connecting) return
                    Log.i(NUS_TAG, "NUS GATT disconnected: $mac (status=$status)")
                    connections.remove(mac)
                    txChars.remove(mac)
                    writeMtu.remove(mac)
                    connecting.remove(mac)
                    gatt.close()
                    // Auto-reconnect bonded devices
                    if (isRunning && gatt.device.bondState == BluetoothDevice.BOND_BONDED) {
                        mainHandler.postDelayed({
                            if (isRunning && mac !in connections && mac !in connecting) {
                                Log.i(NUS_TAG, "NUS: reconnecting $mac")
                                connecting.add(mac)
                                gatt.device.connectGatt(
                                    context, false, this, BluetoothDevice.TRANSPORT_LE
                                )
                            }
                        }, RECONNECT_DELAY_MS)
                    }
                }
            }
        }

        override fun onMtuChanged(gatt: BluetoothGatt, mtu: Int, status: Int) {
            val mac = gatt.device.address
            val effective = if (status == BluetoothGatt.GATT_SUCCESS) mtu - 3 else DEFAULT_WRITE_MTU
            writeMtu[mac] = effective.coerceAtLeast(DEFAULT_WRITE_MTU)
            Log.i(NUS_TAG, "NUS MTU negotiated: $mac → ${effective}B write limit")
            gatt.discoverServices()
        }

        override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
            val mac = gatt.device.address
            if (status != BluetoothGatt.GATT_SUCCESS) {
                Log.w(NUS_TAG, "NUS service discovery failed on $mac: $status")
                gatt.disconnect()
                return
            }
            val service = gatt.getService(NUS_SERVICE_UUID)
            if (service == null) {
                Log.w(NUS_TAG, "NUS service not found on $mac — not an RNode?")
                gatt.disconnect()
                return
            }

            // NUS TX char — phone writes KISS frames TO the RNode
            val txChar = service.getCharacteristic(NUS_TX_CHAR_UUID)
            if (txChar != null) txChars[mac] = txChar

            // NUS RX char — subscribe for KISS frame notifications FROM the RNode
            val rxChar = service.getCharacteristic(NUS_RX_CHAR_UUID)
            if (rxChar != null) {
                gatt.setCharacteristicNotification(rxChar, true)
                val cccd = rxChar.getDescriptor(CCCD_UUID)
                if (cccd != null) {
                    @Suppress("DEPRECATION")
                    cccd.value = BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
                    @Suppress("DEPRECATION")
                    gatt.writeDescriptor(cccd)
                }
            }

            Log.i(NUS_TAG, "NUS RNode ready: $mac (tx=${txChar != null}, rx=${rxChar != null})")
        }

        override fun onDescriptorWrite(gatt: BluetoothGatt, descriptor: BluetoothGattDescriptor, status: Int) {
            val mac = gatt.device.address
            if (descriptor.uuid == CCCD_UUID) {
                if (status == BluetoothGatt.GATT_SUCCESS) {
                    Log.i(NUS_TAG, "NUS RX notifications enabled: $mac — RNode ready")
                } else {
                    Log.e(NUS_TAG, "NUS CCCD write failed ($status) on $mac — RX notifications NOT enabled, disconnecting")
                    gatt.disconnect()
                }
            }
        }

        // API < 33 compat override
        @Suppress("OVERRIDE_DEPRECATION")
        override fun onCharacteristicChanged(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
        ) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) return
            if (characteristic.uuid == NUS_RX_CHAR_UUID) {
                @Suppress("DEPRECATION")
                val data = characteristic.value ?: return
                Log.d(NUS_TAG, "NUS RX(compat) ${data.size}B from ${gatt.device.address}")
                module.nativeNusReceive(data)
            }
        }

        // API 33+ — value delivered directly, no stale characteristic.value
        @androidx.annotation.RequiresApi(Build.VERSION_CODES.TIRAMISU)
        override fun onCharacteristicChanged(
            gatt: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
            value: ByteArray,
        ) {
            if (characteristic.uuid == NUS_RX_CHAR_UUID) {
                Log.d(NUS_TAG, "NUS RX ${value.size}B from ${gatt.device.address}")
                module.nativeNusReceive(value)
            }
        }
    }
}
