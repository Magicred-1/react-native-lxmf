import React, { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import * as SecureStore from 'expo-secure-store';
import { Share } from 'react-native';
import { documentDirectory, readAsStringAsync, writeAsStringAsync } from 'expo-file-system/legacy';
import {
  useLxmf,
  LxmfNodeMode,
  type LxmfEvent,
  type LxmfNodeStatus,
} from '@magicred-1/react-native-lxmf';

const DB_PATH = documentDirectory
  ? documentDirectory.replace('file://', '') + 'lxmf.db'
  : undefined;

// Contacts are not sensitive — stored as a plain file to avoid SecureStore 2048B limit
const CONTACTS_FILE = documentDirectory ? documentDirectory + 'lxmf_contacts.json' : null;

async function readContactsFile(): Promise<string | null> {
  if (!CONTACTS_FILE) return null;
  try { return await readAsStringAsync(CONTACTS_FILE); } catch { return null; }
}

async function writeContactsFile(json: string): Promise<void> {
  if (!CONTACTS_FILE) return;
  try { await writeAsStringAsync(CONTACTS_FILE, json); } catch {}
}

const IDENTITY_KEY = 'lxmf.identity.v1';
const DISPLAY_NAME_KEY = 'lxmf.displayName';
const GROUPS_KEY = 'lxmf.groups.v1';

// ── Types ─────────────────────────────────────────────────────────────────────

export type StoredIdentity = {
  version: number;
  identity_hex: string;
  address_hex: string;
  created_at: string;
};

export type Contact = {
  address: string;
  name: string;
  lastSeen: number;
  lastMessage: string;
  unread: number;
};

export type StoredMessage = {
  id: number;
  source: string;
  dest: string;
  title: string;
  body: string;
  outbound: boolean;
  timestamp: number;
  acked: boolean;
  image?: { mimeType: string; data: string };
  files?: { name: string; data: string }[];
};

export type Group = {
  addrHex: string;
  name: string;
  keyHex: string;
};

// ── Context ───────────────────────────────────────────────────────────────────

export type LxmfContextValue = {
  // Node state
  isNativeAvailable: boolean;
  isRunning: boolean;
  status: LxmfNodeStatus | null;
  error: string | null;
  events: LxmfEvent[];
  // Node control
  start: (overrides?: {
    identityHex?: string;
    lxmfAddressHex?: string;
    mode?: LxmfNodeMode;
    tcpInterfaces?: { host: string; port: number }[];
    displayName?: string;
  }) => Promise<boolean>;
  stop: () => Promise<void>;
  getStatus: () => LxmfNodeStatus | null;
  setLogLevel: (level: number) => void;
  // Messaging — auto-routes DM vs group based on dest address
  send: (dest: string, body: string) => Promise<number>;
  fetchMessages: (limit?: number) => StoredMessage[];
  // Identity
  identity: StoredIdentity | null;
  identityHydrated: boolean;
  clearIdentity: () => Promise<void>;
  // Display name
  displayName: string;
  setDisplayName: (name: string) => void;
  // Contacts
  contacts: Contact[];
  upsertContact: (address: string, opts?: { name?: string; lastMessage?: string }) => void;
  markRead: (address: string) => void;
  // Groups
  groups: Group[];
  createGroup: (name: string) => { addrHex: string; keyHex: string };
  joinGroup: (addrHex: string, keyHex: string) => boolean;
  leaveGroup: (addrHex: string) => void;
  isGroup: (addrHex: string) => boolean;
  shareGroupInvite: (addrHex: string) => Promise<void>;
};

const LxmfContext = createContext<LxmfContextValue | null>(null);

export function useLxmfContext(): LxmfContextValue {
  const ctx = useContext(LxmfContext);
  if (!ctx) throw new Error('useLxmfContext must be used within LxmfProvider');
  return ctx;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function isValidIdentity(b: unknown): b is StoredIdentity {
  if (!b || typeof b !== 'object') return false;
  const v = b as Record<string, unknown>;
  return (
    typeof v.version === 'number' &&
    typeof v.identity_hex === 'string' && /^[0-9a-fA-F]{128}$/.test(v.identity_hex) &&
    typeof v.address_hex === 'string' && /^[0-9a-fA-F]{32}$/.test(v.address_hex) &&
    typeof v.created_at === 'string'
  );
}

function tryJson<T>(raw: string | null, fallback: T): T {
  if (!raw) return fallback;
  try { return JSON.parse(raw) as T; } catch { return fallback; }
}

function b64preview(b64: string, maxLen = 60): string {
  try {
    const bytes = Uint8Array.from(globalThis.atob(b64), c => c.codePointAt(0) ?? 0);
    const text = new TextDecoder('utf-8', { fatal: false }).decode(bytes);
    return text.length > maxLen ? text.slice(0, maxLen) + '…' : text;
  } catch {
    return '';
  }
}

function sortedContacts(map: Record<string, Contact>): Contact[] {
  return Object.values(map).sort((a, b) => b.lastSeen - a.lastSeen);
}

function generateKeyHex(): string {
  const buf = new Uint8Array(16); // Rust group_encrypt uses AES-128: 16-byte key
  const cryptoApi = globalThis.crypto ?? (globalThis as Record<string, any>).msCrypto;
  if (cryptoApi?.getRandomValues) {
    cryptoApi.getRandomValues(buf);
  } else {
    for (let i = 0; i < buf.length; i++) {
      buf[i] = Math.trunc(Math.random() * 256);
    }
  }
  return Array.from(buf, b => b.toString(16).padStart(2, '0')).join('');
}

// ── Provider ──────────────────────────────────────────────────────────────────

export function LxmfProvider({ children }: { readonly children: React.ReactNode }) {
  const [identity, setIdentity] = useState<StoredIdentity | null>(null);
  const [identityHydrated, setIdentityHydrated] = useState(false);
  const [displayName, setDisplayNameState] = useState('lxmf-mobile');
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [groups, setGroups] = useState<Group[]>([]);
  const contactMapRef = useRef<Record<string, Contact>>({});
  const groupMapRef = useRef<Record<string, Group>>({});
  const lastEventRef = useRef<LxmfEvent | null>(null);

  const lxmf = useLxmf({
    identityHex: identity?.identity_hex ?? 'new',
    lxmfAddressHex: identity?.address_hex ?? 'new',
    dbPath: DB_PATH,
    logLevel: 3,
  });

  // Load persisted state on mount
  useEffect(() => {
    (async () => {
      try {
        const raw = await SecureStore.getItemAsync(IDENTITY_KEY);
        const parsed = tryJson<unknown>(raw, null);
        if (isValidIdentity(parsed)) setIdentity(parsed);
      } catch {}
      try {
        const raw = await readContactsFile();
        const map = tryJson<Record<string, Contact>>(raw, {});
        contactMapRef.current = map;
        setContacts(sortedContacts(map));
      } catch {}
      try {
        const name = await SecureStore.getItemAsync(DISPLAY_NAME_KEY);
        if (name) setDisplayNameState(name);
      } catch {}
      try {
        const raw = await SecureStore.getItemAsync(GROUPS_KEY);
        const list = tryJson<Group[]>(raw, []);
        const map: Record<string, Group> = {};
        for (const g of list) map[g.addrHex] = g;
        groupMapRef.current = map;
        setGroups(list);
        // Re-register with Rust — in-memory registry clears on process restart
        for (const g of list) {
          try { lxmf.joinGroup(g.addrHex, g.keyHex); } catch {}
        }
      } catch {}
      setIdentityHydrated(true);
    })();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Persist identity whenever the running node reports one
  const identityHexRef = useRef<string | null>(null);
  useEffect(() => {
    if (!lxmf.isRunning) return;
    const idHex = lxmf.getIdentityHex();
    const addrHex = lxmf.status?.addressHex;
    if (!idHex || idHex.length !== 128) return;
    if (!addrHex || !/^[0-9a-fA-F]{32}$/.test(addrHex)) return;
    if (identityHexRef.current === idHex) return;
    identityHexRef.current = idHex;
    const blob: StoredIdentity = {
      version: 1,
      identity_hex: idHex,
      address_hex: addrHex,
      created_at: identity?.created_at ?? new Date().toISOString(),
    };
    SecureStore.setItemAsync(IDENTITY_KEY, JSON.stringify(blob))
      .then(() => setIdentity(blob))
      .catch(() => {});
  }, [lxmf.isRunning, lxmf.status?.addressHex]);

  const persistContacts = useCallback((map: Record<string, Contact>) => {
    setContacts(sortedContacts(map));
    writeContactsFile(JSON.stringify(map));
  }, []);

  const persistGroups = useCallback((list: Group[]) => {
    setGroups(list);
    SecureStore.setItemAsync(GROUPS_KEY, JSON.stringify(list)).catch(() => {});
  }, []);

  const upsertContact = useCallback((address: string, opts?: { name?: string; lastMessage?: string }) => {
    const map = { ...contactMapRef.current };
    const prev = map[address];
    map[address] = {
      address,
      name: opts?.name ?? prev?.name ?? '',
      lastSeen: Math.floor(Date.now() / 1000),
      lastMessage: opts?.lastMessage ?? prev?.lastMessage ?? '',
      unread: (prev?.unread ?? 0) + (opts?.lastMessage !== undefined ? 1 : 0),
    };
    contactMapRef.current = map;
    persistContacts(map);
  }, [persistContacts]);

  const markRead = useCallback((address: string) => {
    const map = { ...contactMapRef.current };
    if (map[address]) {
      map[address] = { ...map[address], unread: 0 };
      contactMapRef.current = map;
      persistContacts(map);
    }
  }, [persistContacts]);

  // Update contacts from incoming events
  useEffect(() => {
    for (const event of lxmf.events) {
      if (event === lastEventRef.current) break;

      if (event.type === 'announceReceived') {
        const addr = String(event.destHash ?? event.address ?? '');
        if (addr.length === 32) {
          upsertContact(addr, { name: event.appData ? String(event.appData) : undefined });
        }
      } else if (event.type === 'messageReceived') {
        const addr = String(event.source ?? '');
        if (!event.groupDest && addr.length === 32 && !groupMapRef.current[addr]) {
          upsertContact(addr, { lastMessage: event.body ? b64preview(String(event.body)) : '' });
        }
      }
    }
    if (lxmf.events.length > 0) lastEventRef.current = lxmf.events[0];
  }, [lxmf.events, upsertContact]);

  const setDisplayName = useCallback((name: string) => {
    setDisplayNameState(name);
    SecureStore.setItemAsync(DISPLAY_NAME_KEY, name).catch(() => {});
  }, []);

  const clearIdentity = useCallback(async () => {
    await SecureStore.deleteItemAsync(IDENTITY_KEY).catch(() => {});
    identityHexRef.current = null;
    setIdentity(null);
  }, []);

  const fetchMessages = useCallback((limit = 50): StoredMessage[] => {
    try {
      return lxmf.fetchMessages(limit) as StoredMessage[];
    } catch {
      return [];
    }
  }, [lxmf.fetchMessages]);

  // Auto-routes to DM or group send based on destination address
  const lxmfSend = lxmf.send;
  const lxmfSendGroup = lxmf.sendGroup;
  const send = useCallback(async (dest: string, body: string): Promise<number> => {
    const bytes = new TextEncoder().encode(body);
    const b64 = btoa(String.fromCodePoint(...bytes));
    if (groupMapRef.current[dest]) {
      return lxmfSendGroup(dest, b64);
    }
    return lxmfSend(dest, b64);
  }, [lxmfSend, lxmfSendGroup]);

  // ── Group operations ──────────────────────────────────────────────────────

  const createGroup = useCallback((name: string): { addrHex: string; keyHex: string } => {
    const keyHex = generateKeyHex();
    const addrHex = lxmf.createGroup(name, keyHex);
    const group: Group = { addrHex, name, keyHex };
    const newMap = { ...groupMapRef.current, [addrHex]: group };
    groupMapRef.current = newMap;
    persistGroups(Object.values(newMap));
    return { addrHex, keyHex };
  }, [lxmf.createGroup, persistGroups]);

  const joinGroup = useCallback((addrHex: string, keyHex: string): boolean => {
    const ok = lxmf.joinGroup(addrHex, keyHex);
    if (ok) {
      const group: Group = { addrHex, name: addrHex.slice(0, 8), keyHex };
      const newMap = { ...groupMapRef.current, [addrHex]: group };
      groupMapRef.current = newMap;
      persistGroups(Object.values(newMap));
    }
    return ok;
  }, [lxmf.joinGroup, persistGroups]);

  const leaveGroup = useCallback((addrHex: string) => {
    lxmf.leaveGroup(addrHex);
    const newMap = { ...groupMapRef.current };
    delete newMap[addrHex];
    groupMapRef.current = newMap;
    persistGroups(Object.values(newMap));
  }, [lxmf.leaveGroup, persistGroups]);

  const isGroup = useCallback((addrHex: string): boolean => {
    return !!groupMapRef.current[addrHex];
  }, []);

  const shareGroupInvite = useCallback(async (addrHex: string) => {
    const g = groupMapRef.current[addrHex];
    if (!g) return;
    const invite = JSON.stringify({ groupAddr: g.addrHex, keyHex: g.keyHex, name: g.name });
    await Share.share({
      message: `Join my LXMF group "${g.name}"\n\nAddr: ${g.addrHex}\nKey: ${g.keyHex}\n\nInvite payload: ${invite}`,
      title: `LXMF Group Invite — ${g.name}`,
    });
  }, []);

  const value = useMemo<LxmfContextValue>(() => ({
    isNativeAvailable: lxmf.isNativeAvailable,
    isRunning: lxmf.isRunning,
    status: lxmf.status,
    error: lxmf.error,
    events: lxmf.events,
    start: lxmf.start,
    stop: lxmf.stop,
    getStatus: lxmf.getStatus,
    setLogLevel: lxmf.setLogLevel,
    send,
    fetchMessages,
    identity,
    identityHydrated,
    clearIdentity,
    displayName,
    setDisplayName,
    contacts,
    upsertContact,
    markRead,
    groups,
    createGroup,
    joinGroup,
    leaveGroup,
    isGroup,
    shareGroupInvite,
  }), [
    lxmf.isNativeAvailable, lxmf.isRunning, lxmf.status, lxmf.error, lxmf.events,
    lxmf.start, lxmf.stop, lxmf.getStatus, lxmf.setLogLevel,
    send, fetchMessages, identity, identityHydrated, clearIdentity,
    displayName, setDisplayName, contacts, upsertContact, markRead,
    groups, createGroup, joinGroup, leaveGroup, isGroup, shareGroupInvite,
  ]);

  return (
    <LxmfContext.Provider value={value}>
      {children}
    </LxmfContext.Provider>
  );
}
