import { useCallback, useEffect, useState } from 'react';
import { isLxmfNativeAvailable, LxmfModule, LxmfModuleNative } from './LxmfModule';

export interface LxmfNodeStatus {
  running: boolean;
  mode: number;
  identityHex: string;
  addressHex: string;
  lifecycle: number;
  epoch: number;
  pendingOutbound: number;
  outboundSent: number;
  inboundAccepted: number;
  announcesReceived: number;
  lxmfMessagesReceived: number;
  blePeerCount: number;
}

export interface Beacon {
  destHash: string;
  state: string;
  lastAnnounce: number;
  reconnectAttempts: number;
}

export interface LxmfMessageEvent {
  type: 'messageReceived';
  source: string;
  title: string;       // base64
  body: string;        // base64
  timestamp: number;
  image?: { mimeType: string; data: string };  // data = base64
  files?: { name: string; data: string }[];    // data = base64
}

export interface RpcResponseEvent {
  id: number;
  method: string;
  resultJson: string;
  isError: boolean;
}

export interface LxmfEvent {
  type: 'statusChanged' | 'packetReceived' | 'txReceived' | 'beaconDiscovered' | 'messageReceived' | 'announceReceived' | 'messageQueued' | 'messageDelivered' | 'messageFailed' | 'log' | 'error' | 'rpcResponse';
  [key: string]: any;
}

/** Node transport mode */
export enum LxmfNodeMode {
  /** BLE-only mesh (default) */
  BleOnly = 0,
  /** Connect via FFI's internal TCP (non-standard framing) */
  TcpClient = 1,
  /** Listen via FFI's internal TCP (non-standard framing) */
  TcpServer = 2,
  /** Connect to standard Reticulum daemon (rnsd) via HDLC-framed TCP */
  Reticulum = 3,
  /** TCP/Reticulum + BLE simultaneously on the same transport instance */
  ReticulumAndBle = 4,
}

export interface TcpInterface {
  host: string;
  port: number;
}

/** Media attachments to include in an LXMF message.
 *  Encoded as LXMF standard fields: FIELD_IMAGE (0x06) and FIELD_FILE_ATTACHMENTS (0x05).
 *  Compatible with Sideband and other LXMF clients.
 */
export interface LxmfMedia {
  /** Inline image: LXMF FIELD_IMAGE — rendered by receiving clients. data = base64 string. */
  image?: { mimeType: string; data: string };
  /** File attachments: LXMF FIELD_FILE_ATTACHMENTS — list of named blobs. data = base64 string. */
  files?: { name: string; data: string }[];
}

export interface UseLxmfOptions {
  autoStart?: boolean;
  identityHex?: string;
  lxmfAddressHex?: string;
  dbPath?: string;
  logLevel?: number;
  /** Transport mode — BLE or Reticulum TCP. Default: BleOnly */
  mode?: LxmfNodeMode;
  /** One or more TCP interfaces to connect to (required for Reticulum mode). */
  tcpInterfaces?: TcpInterface[];
  /** Announce interval in ms. Default: 60000 for BLE modes, 5000 for TCP-only. Rust enforces 60s minimum for BLE. */
  announceIntervalMs?: number;
  /** BLE MTU hint. Default: 255 */
  bleMtuHint?: number;
  /** Display name broadcast in LXMF announces. Default: "lxmf-mobile" */
  displayName?: string;
  /** Advertise this node as an anonmesh beacon (app_data = "anonmesh::beacon::v1\0<name>"). Default: false */
  isBeacon?: boolean;
}

function parseJson<T>(value: string | null, fallback: T): T {
  if (!value) return fallback;
  try {
    return JSON.parse(value) as T;
  } catch {
    return fallback;
  }
}

export function useLxmf(options: UseLxmfOptions = {}) {
  const [status, setStatus] = useState<LxmfNodeStatus | null>(null);
  const [beacons, setBeacons] = useState<Beacon[]>([]);
  const [events, setEvents] = useState<LxmfEvent[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);

  const pushEvent = useCallback((type: LxmfEvent['type'], payload: Record<string, any>) => {
    const event = { ...payload, type } as LxmfEvent;
    setEvents((prev: LxmfEvent[]) => [event, ...prev].slice(0, 200));
    return event;
  }, []);

  const syncStatus = useCallback(() => {
    const parsed = parseJson<LxmfNodeStatus | null>(LxmfModule.getStatus(), null);
    setStatus(parsed);
    if (parsed && typeof parsed.running === 'boolean') {
      setRunning(parsed.running);
    }
    return parsed;
  }, []);

  useEffect(() => {
    if (!isLxmfNativeAvailable) {
      setError(
        "Cannot find native module 'LxmfModule'. Run this app in an Expo development build (not Expo Go)."
      );
      return;
    }

    try {
      const initialized = LxmfModule.init(options.dbPath || null);
      if (!initialized) {
        setError('Failed to initialize LXMF module');
        return;
      }

      const alreadyRunning = LxmfModule.isRunning();
      setRunning(alreadyRunning);
      if (alreadyRunning) {
        syncStatus();
      }
      setError(null);
    } catch (e: any) {
      setError(e?.message ?? 'Initialization failed');
    }
  }, [options.dbPath, syncStatus]);

  useEffect(() => {
    if (!isLxmfNativeAvailable || !LxmfModuleNative) {
      return;
    }

    const mod = LxmfModuleNative as any;

    const subscriptions = [
      mod.addListener('onStatusChanged', (event: Record<string, any>) => {
        pushEvent('statusChanged', event);
        if (typeof event.running === 'boolean') {
          setRunning(event.running);
        }
        syncStatus();
      }),
      mod.addListener('onPacketReceived', (event: Record<string, any>) => {
        pushEvent('packetReceived', event);
      }),
      mod.addListener('onTxReceived', (event: Record<string, any>) => {
        pushEvent('txReceived', event);
      }),
      mod.addListener('onBeaconDiscovered', (event: Record<string, any>) => {
        pushEvent('beaconDiscovered', event);
        const latestBeacons = parseJson<Beacon[]>(LxmfModule.getBeacons(), []);
        setBeacons(latestBeacons);
      }),
      mod.addListener('onMessageReceived', (event: Record<string, any>) => {
        pushEvent('messageReceived', event);
      }),
      mod.addListener('onAnnounceReceived', (event: Record<string, any>) => {
        pushEvent('announceReceived', event);
      }),
      mod.addListener('onLog', (event: Record<string, any>) => {
        pushEvent('log', event);
        if (typeof options.logLevel === 'number' && options.logLevel >= Number(event.level ?? 0)) {
          console.log(`[LXMF] ${String(event.message)}`);
        }
      }),
      mod.addListener('onError', (event: Record<string, any>) => {
        pushEvent('error', event);
        setError(`${String(event.code)}: ${String(event.message)}`);
      }),
      mod.addListener('onRpcResponse', (event: Record<string, any>) => {
        pushEvent('rpcResponse', event);
      }),
      mod.addListener('onMessageQueued', (event: Record<string, any>) => {
        pushEvent('messageQueued', event);
      }),
      mod.addListener('onMessageDelivered', (event: Record<string, any>) => {
        pushEvent('messageDelivered', event);
      }),
      mod.addListener('onMessageFailed', (event: Record<string, any>) => {
        pushEvent('messageFailed', event);
        setError(`Message ${String(event.seq)} failed: ${String(event.reason ?? 'unknown')}`);
      }),
    ];

    return () => {
      subscriptions.forEach((sub: { remove: () => void }) => sub.remove());
    };
  }, [options.logLevel, pushEvent, syncStatus]);

  const start = useCallback(
    async (overrides?: {
      identityHex?: string;
      lxmfAddressHex?: string;
      mode?: LxmfNodeMode;
      tcpInterfaces?: TcpInterface[];
      displayName?: string;
      isBeacon?: boolean;
    }) => {
      try {
        if (!isLxmfNativeAvailable) {
          setError(
            "Cannot find native module 'LxmfModule'. Run this app in an Expo development build (not Expo Go)."
          );
          return false;
        }

        const resolvedIdentityHex = overrides?.identityHex ?? options.identityHex;
        const resolvedLxmfAddressHex = overrides?.lxmfAddressHex ?? options.lxmfAddressHex;
        if (!resolvedIdentityHex || !resolvedLxmfAddressHex) {
          setError('Missing identity or LXMF address. Pass them to start() or UseLxmfOptions.');
          return false;
        }

        const mode = overrides?.mode ?? options.mode ?? LxmfNodeMode.BleOnly;
        const tcpInterfaces = overrides?.tcpInterfaces ?? options.tcpInterfaces ?? [];
        const BLE_MODES = [LxmfNodeMode.BleOnly, LxmfNodeMode.ReticulumAndBle];
        const defaultAnnounceMs = BLE_MODES.includes(mode) ? 60_000 : 5_000;
        const announceMs = options.announceIntervalMs ?? defaultAnnounceMs;
        const bleMtu = options.bleMtuHint ?? 255;
        const displayName = overrides?.displayName ?? options.displayName ?? '';
        const isBeacon = overrides?.isBeacon ?? options.isBeacon ?? false;

        if (mode !== LxmfNodeMode.BleOnly && tcpInterfaces.length === 0) {
          setError(`Mode ${mode} requires at least one TCP interface.`);
          return false;
        }

        await LxmfModule.start(
          resolvedIdentityHex,
          resolvedLxmfAddressHex,
          mode,
          announceMs,
          bleMtu,
          tcpInterfaces,
          displayName,
          isBeacon,
        );
        setRunning(true);
        syncStatus();
        setError(null);
        return true;
      } catch (e: any) {
        setError(e?.message ?? 'Failed to start');
        return false;
      }
    },
    [
      options.identityHex,
      options.lxmfAddressHex,
      options.mode,
      options.tcpInterfaces,
      options.announceIntervalMs,
      options.bleMtuHint,
      options.displayName,
      options.isBeacon,
      syncStatus,
    ]
  );

  useEffect(() => {
    if (!options.autoStart || running) return;
    if (!options.identityHex || !options.lxmfAddressHex) return;
    start().catch(() => {
      // start() already sets error state on failure
    });
  }, [options.autoStart, options.identityHex, options.lxmfAddressHex, running, start]);

  const stop = useCallback(async () => {
    try {
      await LxmfModule.stop();
      setRunning(false);
      setStatus(null);
      setError(null);
    } catch (e: any) {
      setError(e?.message ?? 'Failed to stop');
    }
  }, []);

  const send = useCallback(async (destHex: string, bodyBase64: string, media?: LxmfMedia) => {
    try {
      const fieldsJson = media ? JSON.stringify(media) : null;
      return await LxmfModule.send(destHex, bodyBase64, fieldsJson);
    } catch (e: any) {
      setError(e.message);
      return -1;
    }
  }, []);

  const broadcast = useCallback(async (destsHex: string[], bodyBase64: string, media?: LxmfMedia) => {
    try {
      const fieldsJson = media ? JSON.stringify(media) : null;
      return await LxmfModule.broadcast(destsHex, bodyBase64, fieldsJson);
    } catch (e: any) {
      setError(e.message);
      return -1;
    }
  }, []);

  const getStatus = useCallback(() => {
    try {
      return syncStatus();
    } catch (e: any) {
      setError(`Failed to parse status payload: ${e?.message ?? 'unknown error'}`);
      return null;
    }
  }, [syncStatus]);

  const getBeacons = useCallback(() => {
    try {
      const parsed = parseJson<Beacon[]>(LxmfModule.getBeacons(), []);
      setBeacons(parsed);
      return parsed;
    } catch (e: any) {
      setError(`Failed to parse beacon payload: ${e?.message ?? 'unknown error'}`);
      return [];
    }
  }, []);

  const fetchMessages = useCallback((limit: number = 50) => {
    try {
      return parseJson<any[]>(LxmfModule.fetchMessages(limit), []);
    } catch (e: any) {
      setError(`Failed to parse message payload: ${e?.message ?? 'unknown error'}`);
      return [];
    }
  }, []);

  const setLogLevel = useCallback((level: number) => {
    return LxmfModule.setLogLevel(level);
  }, []);

  /**
   * Returns the full 128-char private identity hex for persistence, or null
   * if no node is initialized. Persist to encrypted storage (e.g. expo-secure-store)
   * and pass back via UseLxmfOptions.identityHex on next mount to reuse the identity.
   */
  const getIdentityHex = useCallback((): string | null => {
    try {
      return LxmfModule.getIdentityHex();
    } catch {
      return null;
    }
  }, []);

  const startBLE = useCallback(() => {
    LxmfModule.startBLE();
  }, []);

  const stopBLE = useCallback(() => {
    LxmfModule.stopBLE();
  }, []);

  const bleUnpairedRNodeCount = useCallback(() => {
    return LxmfModule.bleUnpairedRNodeCount();
  }, []);

  /** List of RNodes visible in scan but not yet OS-paired. */
  const getNusUnpairedRNodes = useCallback((): { mac: string; name: string }[] => {
    try {
      return parseJson<{ mac: string; name: string }[]>(LxmfModule.getNusUnpairedRNodes(), []);
    } catch {
      return [];
    }
  }, []);

  /**
   * Initiate OS Bluetooth pairing with an unpaired RNode (mac = "AA:BB:CC:DD:EE:FF").
   * Shows system pairing dialog. Auto-connects on bond completion via bondReceiver.
   */
  const pairNusRNode = useCallback((mac: string): boolean => {
    return LxmfModule.pairNusRNode(mac);
  }, []);

  /**
   * Queue a JSON-RPC 2.0 call to a beacon.
   * Returns correlation id; the response arrives as an `rpcResponse` event.
   * `params` is any JSON-serializable value (usually an array).
   */
  const beaconRpc = useCallback(async (
    destHashHex: string,
    method: string,
    params?: unknown,
  ): Promise<number> => {
    try {
      const paramsJson = params === undefined ? null : JSON.stringify(params);
      return await LxmfModule.beaconRpc(destHashHex, method, paramsJson);
    } catch (e: any) {
      setError(e?.message ?? 'beaconRpc failed');
      return -1;
    }
  }, []);

  return {
    // State
    status,
    beacons,
    events,
    error,
    isRunning: running,
    isNativeAvailable: isLxmfNativeAvailable,

    // Methods
    start,
    stop,
    send,
    broadcast,
    getStatus,
    getBeacons,
    fetchMessages,
    getIdentityHex,
    setLogLevel,
    startBLE,
    stopBLE,
    bleUnpairedRNodeCount,
    getNusUnpairedRNodes,
    pairNusRNode,
    beaconRpc,
  };
}
