// Lazy require — safe in Node.js config-plugin context (no React Native runtime)
let requireOptionalNativeModule: (<T>(name: string) => T | null) = () => null;
try {
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  requireOptionalNativeModule = require('expo-modules-core').requireOptionalNativeModule;
} catch {
  // Node.js / config-plugin evaluation context — native modules not available
}

export type NativeModuleType = {
  // Lifecycle
  init(dbPath?: string | null): boolean;
  start(
    identityHex: string,
    lxmfAddressHex: string,
    mode: number,
    announceIntervalMs: number,
    bleMtuHint: number,
    tcpInterfaces: { host: string; port: number }[],
    displayName: string,
    isBeacon: boolean
  ): Promise<boolean>;
  stop(): Promise<boolean>;
  isRunning(): boolean;

  // Messaging
  send(destHex: string, bodyBase64: string, fieldsJson?: string | null): Promise<number>;
  broadcast(destsHex: string[], bodyBase64: string, fieldsJson?: string | null): Promise<number>;

  // Identity (returns full 128-char private key hex for persistence; null if no node)
  getIdentityHex(): string | null;

  // Status & State
  getStatus(): string | null;
  getBeacons(): string | null;
  fetchMessages(limit: number): string | null;

  // Beacon RPC — queue a JSON-RPC 2.0 call to a beacon; response arrives as onRpcResponse event.
  beaconRpc(destHashHex: string, method: string, paramsJson?: string | null): Promise<number>;

  // Solana tx building — client-side partial sign
  partialSignExecutePayment(payerKeyHex: string, nonceBlockhashHex: string, accountsJson: string, paramsJson: string): string | null;
  extractNonceBlockhash(accountDataB64: string): string | null;

  // Program ID — set once at init (deployment-specific: devnet vs mainnet)
  setProgramId(programIdHex: string): boolean;
  getProgramId(): string | null;

  // Beacon configuration (beacon server side)
  setBeaconKeypair(keyHex: string): boolean;
  setBeaconSolanaRpc(url: string): boolean;

  // Configuration
  setLogLevel(level: number): boolean;
  abiVersion(): number;

  // BLE Control
  startBLE(): boolean;
  stopBLE(): boolean;
  blePeerCount(): number;
  bleUnpairedRNodeCount(): number;

  // RNode pairing — NUS/KISS BLE path
  getNusUnpairedRNodes(): string;
  pairNusRNode(mac: string): boolean;

  // Group chat — Reticulum GROUP destination (shared AES key)
  createGroup(name: string, keyHex: string): string;
  joinGroup(addrHex: string, keyHex: string): boolean;
  leaveGroup(addrHex: string): boolean;
  sendGroup(addrHex: string, bodyBase64: string, fieldsJson?: string): Promise<number>;
}

const MISSING_NATIVE_MESSAGE =
  "Cannot find native module 'LxmfModule'. Use an Expo development build (not Expo Go) and rebuild native apps after local module changes.";

const LxmfModuleNative = requireOptionalNativeModule<NativeModuleType>('LxmfModule');

export const isLxmfNativeAvailable = !!LxmfModuleNative;

const throwMissingNative = (): never => {
  throw new Error(MISSING_NATIVE_MESSAGE);
};

const missingNativeShim: NativeModuleType = {
  init: () => throwMissingNative(),
  start: async () => throwMissingNative(),
  stop: async () => throwMissingNative(),
  isRunning: () => false,
  send: async () => throwMissingNative(),
  broadcast: async () => throwMissingNative(),
  getIdentityHex: () => throwMissingNative(),
  getStatus: () => throwMissingNative(),
  getBeacons: () => throwMissingNative(),
  fetchMessages: () => throwMissingNative(),
  setLogLevel: () => throwMissingNative(),
  abiVersion: () => throwMissingNative(),
  startBLE: () => throwMissingNative(),
  stopBLE: () => throwMissingNative(),
  blePeerCount: () => throwMissingNative(),
  bleUnpairedRNodeCount: () => throwMissingNative(),
  beaconRpc: async () => throwMissingNative(),
  partialSignExecutePayment: () => throwMissingNative(),
  extractNonceBlockhash: () => throwMissingNative(),
  setProgramId: () => throwMissingNative(),
  getProgramId: () => throwMissingNative(),
  setBeaconKeypair: () => throwMissingNative(),
  setBeaconSolanaRpc: () => throwMissingNative(),
  getNusUnpairedRNodes: () => throwMissingNative(),
  pairNusRNode: () => throwMissingNative(),
  createGroup: () => throwMissingNative(),
  joinGroup: () => throwMissingNative(),
  leaveGroup: () => throwMissingNative(),
  sendGroup: async () => throwMissingNative(),
} as NativeModuleType;

export const LxmfModule = LxmfModuleNative ?? missingNativeShim;

/**
 * The raw native module instance, or null when unavailable.
 * In Expo SDK 50+, NativeModule extends the C++ EventEmitter — call addListener() on it directly.
 * Do NOT use NativeEventEmitter from react-native; it does not wire up to Expo module events.
 */
export { LxmfModuleNative };
