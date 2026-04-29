package expo.modules.lxmf

import android.os.Handler
import android.os.Looper
import android.util.Log
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import org.json.JSONArray

private const val TAG = "LxmfModule"
private const val POLL_INTERVAL_MS = 500L

class LxmfModule : Module() {
  private val pollHandler = Handler(Looper.getMainLooper())
  private val pollRunnable = object : Runnable {
    override fun run() {
      drainAndEmitEvents()
      pollHandler.postDelayed(this, POLL_INTERVAL_MS)
    }
  }

  // BleManager is created lazily when the app context is available
  private var bleManager: BleManager? = null

  override fun definition() = ModuleDefinition {
    Name("LxmfModule")

    Events(
      "onPacketReceived",
      "onTxReceived",
      "onBeaconDiscovered",
      "onMessageReceived",
      "onAnnounceReceived",
      "onStatusChanged",
      "onLog",
      "onError",
      "onOutgoingPacket"
    )

    OnCreate {
      if (isNativeLibraryLoaded()) {
        pollHandler.postDelayed(pollRunnable, POLL_INTERVAL_MS)
      } else {
        Log.w(TAG, "Skipping event polling because liblxmf_rn is not loaded")
      }
      // Create BleManager with whatever Android context is available in this lifecycle phase
      val ctx = appContext.reactContext?.applicationContext
        ?: appContext.currentActivity?.applicationContext
      if (ctx != null) {
        bleManager = BleManager(ctx, this@LxmfModule)
      }
    }

    OnDestroy {
      pollHandler.removeCallbacks(pollRunnable)
      bleManager?.stop()
      bleManager = null
    }

    // Lifecycle
    Function("init") { dbPath: String? ->
      val rc = nativeInit(dbPath)
      if (rc != 0) throw RuntimeException("nativeInit returned $rc")
      true
    }

    AsyncFunction("start") { identityHex: String, lxmfAddressHex: String, mode: Int,
                              announceIntervalMs: Double, bleMtuHint: Int,
                              tcpInterfaces: List<Map<String, Any>>, displayName: String,
                              isBeacon: Boolean ->
      Log.d(TAG, "start() mode=$mode interfaces=$tcpInterfaces name=$displayName beacon=$isBeacon")
      val interfacesJson = org.json.JSONArray(tcpInterfaces.map { iface ->
        org.json.JSONObject().apply {
          put("host", iface["host"] ?: "")
          put("port", iface["port"] ?: 0)
        }
      }).toString()
      val rc = nativeStart(identityHex, lxmfAddressHex, mode, announceIntervalMs.toLong(),
                  bleMtuHint.toShort(), interfacesJson, displayName, isBeacon)
      if (rc != 0) throw RuntimeException("nativeStart returned $rc")
      true
    }

    AsyncFunction("stop") {
      val rc = nativeStop()
      if (rc != 0) throw RuntimeException("nativeStop returned $rc")
      true
    }

    Function("isRunning") {
      nativeIsRunning()
    }

    // Messaging
    AsyncFunction("send") { destHex: String, bodyBase64: String ->
      nativeSend(destHex, bodyBase64).toDouble()
    }

    AsyncFunction("broadcast") { destsHex: List<String>, bodyBase64: String ->
      val destsJson = org.json.JSONArray(destsHex).toString()
      nativeBroadcast(destsJson, bodyBase64).toDouble()
    }

    // Identity
    Function("getIdentityHex") {
      nativeGetIdentityHex()
    }

    // Status & State
    Function("getStatus") {
      nativeGetStatus()
    }

    Function("getBeacons") {
      nativeGetBeacons()
    }

    Function("fetchMessages") { limit: Int ->
      nativeFetchMessages(limit)
    }

    // Configuration
    Function("setLogLevel") { level: Int ->
      nativeSetLogLevel(level) == 0
    }

    Function("abiVersion") {
      nativeAbiVersion()
    }

    // BLE Control — starts/stops BLE scan+advertise+GATT bridge to Rust
    Function("startBLE") {
      Log.d(TAG, "startBLE()")
      bleManager?.start()
      true
    }

    Function("stopBLE") {
      Log.d(TAG, "stopBLE()")
      bleManager?.stop()
      true
    }

    Function("blePeerCount") {
      nativeBlePeerCount()
    }
  }

  private fun drainAndEmitEvents() {
    if (!isNativeLibraryLoaded()) return

    val json = try {
      nativePollEvents()
    } catch (e: UnsatisfiedLinkError) {
      Log.e(TAG, "nativePollEvents unavailable: ${e.message}")
      pollHandler.removeCallbacks(pollRunnable)
      return
    } ?: return

    try {
      val arr = JSONArray(json)
      for (i in 0 until arr.length()) {
        val obj = arr.getJSONObject(i)
        val type = obj.optString("type")
        val eventName = when (type) {
          "statusChanged"    -> "onStatusChanged"
          "announceReceived" -> "onAnnounceReceived"
          "messageReceived"  -> "onMessageReceived"
          "packetReceived"   -> "onPacketReceived"
          "txReceived"       -> "onTxReceived"
          "beaconDiscovered" -> "onBeaconDiscovered"
          "log"              -> "onLog"
          "error"            -> "onError"
          else               -> null
        } ?: continue

        val params = mutableMapOf<String, Any?>()
        val keys = obj.keys()
        while (keys.hasNext()) {
          val key = keys.next()
          if (key != "type") params[key] = obj.get(key)
        }
        sendEvent(eventName, params)
      }
    } catch (e: Exception) {
      Log.e(TAG, "drainAndEmitEvents parse error: ${e.message}")
    }
  }

  // Native JNI method declarations — types must match Rust JNI signatures exactly
  private external fun nativeInit(dbPath: String?): Int
  private external fun nativeStart(
    identityHex: String,
    lxmfAddressHex: String,
    mode: Int,
    announceIntervalMs: Long,
    bleMtuHint: Short,
    tcpInterfacesJson: String,
    displayName: String,
    isBeacon: Boolean
  ): Int
  private external fun nativeStop(): Int
  private external fun nativeIsRunning(): Boolean
  private external fun nativePollEvents(): String?
  private external fun nativeSend(destHex: String, bodyBase64: String): Long
  private external fun nativeBroadcast(destsJson: String, bodyBase64: String): Long
  private external fun nativeGetIdentityHex(): String?
  private external fun nativeGetStatus(): String?
  private external fun nativeGetBeacons(): String?
  private external fun nativeFetchMessages(limit: Int): String?
  private external fun nativeSetLogLevel(level: Int): Int
  private external fun nativeAbiVersion(): Int

  // BLE JNI — called by BleManager (same package)
  // Must NOT be `internal` — Kotlin mangles internal function names in JVM bytecode,
  // which breaks JNI symbol resolution (produces e.g. nativeBlePollTx$lxmf_react_native_debug).
  external fun nativeBleReceive(peerAddr: ByteArray, data: ByteArray)
  external fun nativeBlePollTx(): String?
  external fun nativeBleConnected(peerAddr: ByteArray)
  external fun nativeBleDisconnected(peerAddr: ByteArray)
  external fun nativeBlePeerCount(): Int
  external fun nativeOnMtuNegotiated(peerAddr: ByteArray, attMtu: Int)

  companion object {
    @Volatile
    private var nativeLibraryLoaded = false

    fun isNativeLibraryLoaded(): Boolean = nativeLibraryLoaded

    init {
      try {
        System.loadLibrary("lxmf_rn")
        nativeLibraryLoaded = true
        Log.i(TAG, "liblxmf_rn loaded successfully")
      } catch (e: UnsatisfiedLinkError) {
        nativeLibraryLoaded = false
        Log.e(TAG, "Failed to load liblxmf_rn: ${e.message}")
      }
    }
  }
}
