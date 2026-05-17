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

export interface ExecutePaymentAccounts {
  payer: string;           // 64-char hex (32-byte pubkey)
  payerAta: string;
  recipient: string;
  recipientAta: string;
  broadcasterAta: string;
  mint: string;
  // broadcaster and programId are beacon-side — not per-call arguments
}

/** Well-known Solana program addresses as 64-char lowercase hex (no base58). */
export const SOLANA_PUBKEYS = {
  systemProgram:           '0000000000000000000000000000000000000000000000000000000000000000',
  tokenProgram:            '06ddf6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a9',
  associatedTokenProgram:  '8c97258f4e2489f1bb3d1029148e0d830b5a1399daff1084048e7bd8dbe9f859',
  sysvarRent:              '06a7d51718c774c928566398691d5eb68b5eb8a39b4b6d5c73555b2100000000',
  sysvarRecentBlockhashes: '06a7d51713acca52218cc94c3d4af17f58daee089ba1fd44e3dbd98a00000000',
} as const;

export interface ExecutePaymentParams {
  compOffset: number;       // u64 — safe for typical values
  amount: number;           // u64
  encryptedAmount: string;  // 64-char hex → [u8; 32]
  nonce: string;            // decimal string → u128
  encryptionPubKey: string; // 64-char hex → [u8; 32]
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

  /**
   * Call a specific beacon by destHash and await the response.
   * Resolves when the matching `onRpcResponse` event arrives, rejects on timeout.
   *
   * Listener is registered BEFORE beaconRpc() is called to eliminate the race where
   * a fast beacon responds before the JS event loop resumes after the await.
   * Events arriving before the id is known are buffered and replayed once id is set.
   */
  const beaconRpcWait = useCallback((
    destHashHex: string,
    method: string,
    params?: unknown,
    timeoutMs = 30_000,
  ): Promise<{ resultJson: string; isError: boolean }> => {
    const paramsJson = params === undefined ? null : JSON.stringify(params);

    return new Promise((resolve, reject) => {
      let id: number | undefined;
      let settled = false;
      const buffered: any[] = [];

      const settle = (event: any) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        sub?.remove();
        resolve({ resultJson: event.resultJson, isError: event.isError });
      };

      const sub = (LxmfModuleNative as any)?.addListener('onRpcResponse', (event: any) => {
        if (id === undefined) { buffered.push(event); return; }
        if (event.id === id) settle(event);
      });

      if (sub == null) { reject(new Error('Native module unavailable')); return; }

      const timer = setTimeout(() => {
        sub.remove();
        reject(new Error(`beaconRpc(${method}) timed out after ${timeoutMs}ms`));
      }, timeoutMs);

      LxmfModule.beaconRpc(destHashHex, method, paramsJson).then(rpcId => {
        if (rpcId < 0) {
          clearTimeout(timer); sub.remove();
          reject(new Error(`beaconRpc(${method}): send failed`));
          return;
        }
        id = rpcId;
        for (const evt of buffered) {
          if (evt.id === id) { settle(evt); return; }
        }
      }).catch(e => { clearTimeout(timer); sub.remove(); reject(e); });
    });
  }, []);

  /**
   * Send the same RPC call to ALL discovered beacons simultaneously.
   * Resolves with the first *successful* (non-error) response and the responding beacon's destHash.
   * Error responses are rejected so Promise.any skips them in favour of a succeeding beacon.
   */
  const beaconBroadcastRpc = useCallback(async (
    method: string,
    params?: unknown,
    timeoutMs = 30_000,
  ): Promise<{ resultJson: string; beaconHash: string }> => {
    const raw = LxmfModule.getBeacons();
    const list: Array<{ destHash: string }> = raw ? JSON.parse(raw) : [];
    if (list.length === 0) throw new Error('No beacons discovered yet');
    return Promise.any(
      list.map(b =>
        beaconRpcWait(b.destHash, method, params, timeoutMs)
          .then(res => {
            if (res.isError) {
              const parsed = JSON.parse(res.resultJson) as { message?: string };
              throw new Error(parsed.message ?? 'rpc error');
            }
            return { resultJson: res.resultJson, beaconHash: b.destHash };
          })
      )
    );
  }, [beaconRpcWait]);

  /**
   * Round 1 of the 2-step cosign protocol.
   * Broadcasts `prepareTransaction` to all discovered beacons; races for the first success.
   * Returns the unsigned tx bytes (base64) and the destHash of the responding beacon.
   *
   * MWA path: pass `unsignedTxB64` to your wallet adapter, then call `submitSignedTx`.
   */
  const requestUnsignedTx = useCallback(async (
    accounts: ExecutePaymentAccounts,
    params: ExecutePaymentParams,
    timeoutMs = 60_000,
  ): Promise<{ unsignedTxB64: string; beaconHash: string }> => {
    const { resultJson, beaconHash } = await beaconBroadcastRpc(
      'prepareTransaction',
      [JSON.stringify({ accounts, params })],
      timeoutMs,
    );
    const prepared = JSON.parse(resultJson) as { result?: { unsignedTxB64?: string } };
    const unsignedTxB64 = prepared.result?.unsignedTxB64;
    if (!unsignedTxB64) throw new Error('prepareTransaction: no unsignedTxB64 in response');
    return { unsignedTxB64, beaconHash };
  }, [beaconBroadcastRpc]);

  /**
   * Round 2 of the 2-step cosign protocol.
   * Sends the payer-signed tx to the specific beacon that built it.
   * Beacon verifies payer sig, cosigns slot 1, and submits to Solana.
   */
  const submitSignedTx = useCallback(async (
    beaconHash: string,
    partialTxB64: string,
    timeoutMs = 60_000,
  ): Promise<{ txSig: string; beaconHash: string }> => {
    const res = await beaconRpcWait(beaconHash, 'cosignTransaction', [partialTxB64], timeoutMs);
    const parsed = JSON.parse(res.resultJson) as { result?: string; message?: string };
    if (res.isError) throw new Error(parsed.message ?? 'cosign rejected');
    return { txSig: parsed.result ?? '', beaconHash };
  }, [beaconRpcWait]);

  /**
   * Full 2-step cosign flow for the non-MWA (direct private key) case.
   * Broadcasts `prepareTransaction`, signs payer slot 0 locally, then calls `cosignTransaction`
   * on the same beacon. Returns the confirmed Solana tx signature.
   *
   * For MWA: use `requestUnsignedTx` + your wallet adapter + `submitSignedTx` instead.
   */
  const cosignAndSubmit = useCallback(async (
    payerPrivKeyHex: string,
    accounts: ExecutePaymentAccounts,
    params: ExecutePaymentParams,
    timeoutMs = 60_000,
  ): Promise<{ txSig: string; beaconHash: string }> => {
    const { unsignedTxB64, beaconHash } = await requestUnsignedTx(accounts, params, timeoutMs);
    const partialTxB64 = LxmfModule.signTx(payerPrivKeyHex, unsignedTxB64);
    if (!partialTxB64) throw new Error('signTx: local signing failed');
    return submitSignedTx(beaconHash, partialTxB64, timeoutMs);
  }, [requestUnsignedTx, submitSignedTx]);

  const setProgramId = useCallback((programIdHex: string): boolean => {
    try { return LxmfModule.setProgramId(programIdHex); }
    catch (e: any) { setError(e?.message ?? 'setProgramId failed'); return false; }
  }, []);

  const getProgramId = useCallback((): string | null => {
    try { return LxmfModule.getProgramId(); }
    catch { return null; }
  }, []);

  const setBeaconKeypair = useCallback((keyHex: string): boolean => {
    try { return LxmfModule.setBeaconKeypair(keyHex); }
    catch (e: any) { setError(e?.message ?? 'setBeaconKeypair failed'); return false; }
  }, []);

  const setBeaconSolanaRpc = useCallback((url: string): boolean => {
    try { return LxmfModule.setBeaconSolanaRpc(url); }
    catch (e: any) { setError(e?.message ?? 'setBeaconSolanaRpc failed'); return false; }
  }, []);

  /** @deprecated Use cosignAndSubmit (plain tx) or requestUnsignedTx+submitSignedTx (MWA). */
  const partialSignExecutePayment = useCallback((
    payerKeyHex: string,
    nonceBlockhashHex: string,
    accountsJson: string,
    paramsJson: string,
  ): string | null => {
    try {
      return LxmfModule.partialSignExecutePayment(payerKeyHex, nonceBlockhashHex, accountsJson, paramsJson);
    } catch (e: any) { setError(e?.message ?? 'partialSignExecutePayment failed'); return null; }
  }, []);

  const extractNonceBlockhash = useCallback((accountDataB64: string): string | null => {
    try { return LxmfModule.extractNonceBlockhash(accountDataB64); }
    catch (e: any) { setError(e?.message ?? 'extractNonceBlockhash failed'); return null; }
  }, []);

  /** Create a group channel with a shared AES key. Returns the group address hex. */
  const createGroup = useCallback((name: string, keyHex: string): string => {
    try {
      return LxmfModule.createGroup(name, keyHex);
    } catch (e: any) {
      setError(e?.message ?? 'createGroup failed');
      return '';
    }
  }, []);

  /** Join an existing group channel by address and shared AES key. */
  const joinGroup = useCallback((addrHex: string, keyHex: string): boolean => {
    try {
      return LxmfModule.joinGroup(addrHex, keyHex);
    } catch (e: any) {
      setError(e?.message ?? 'joinGroup failed');
      return false;
    }
  }, []);

  /** Leave a group channel and forget its key. */
  const leaveGroup = useCallback((addrHex: string): boolean => {
    try {
      return LxmfModule.leaveGroup(addrHex);
    } catch (e: any) {
      setError(e?.message ?? 'leaveGroup failed');
      return false;
    }
  }, []);

  /** Send a message to a group channel. Returns sequence number or -1 on error. */
  const sendGroup = useCallback(async (addrHex: string, bodyBase64: string, media?: LxmfMedia): Promise<number> => {
    try {
      const fieldsJson = media ? JSON.stringify(media) : undefined;
      return await LxmfModule.sendGroup(addrHex, bodyBase64, fieldsJson);
    } catch (e: any) {
      setError(e?.message ?? 'sendGroup failed');
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
    beaconRpcWait,
    beaconBroadcastRpc,
    requestUnsignedTx,
    submitSignedTx,
    cosignAndSubmit,
    setProgramId,
    getProgramId,
    setBeaconKeypair,
    setBeaconSolanaRpc,
    partialSignExecutePayment,
    extractNonceBlockhash,
    createGroup,
    joinGroup,
    leaveGroup,
    sendGroup,
  };
}
