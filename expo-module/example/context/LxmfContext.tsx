import React, { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import * as SecureStore from 'expo-secure-store';
import { documentDirectory } from 'expo-file-system/legacy';
import {
  useLxmf,
  LxmfNodeMode,
  type LxmfEvent,
  type LxmfNodeStatus,
} from '@magicred-1/react-native-lxmf';

const DB_PATH = documentDirectory
  ? documentDirectory.replace('file://', '') + 'lxmf.db'
  : undefined;

const IDENTITY_KEY = 'lxmf.identity.v1';
const CONTACTS_KEY = 'lxmf.contacts.v1';
const DISPLAY_NAME_KEY = 'lxmf.displayName';

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
  // Messaging
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

// ── Provider ──────────────────────────────────────────────────────────────────

export function LxmfProvider({ children }: { readonly children: React.ReactNode }) {
  const [identity, setIdentity] = useState<StoredIdentity | null>(null);
  const [identityHydrated, setIdentityHydrated] = useState(false);
  const [displayName, setDisplayNameState] = useState('lxmf-mobile');
  const [contacts, setContacts] = useState<Contact[]>([]);
  const contactMapRef = useRef<Record<string, Contact>>({});
  // Tracks the most recent processed event so the effect only processes new ones.
  const lastEventRef = useRef<LxmfEvent | null>(null);

  // Load persisted state on mount
  useEffect(() => {
    (async () => {
      try {
        const raw = await SecureStore.getItemAsync(IDENTITY_KEY);
        const parsed = tryJson<unknown>(raw, null);
        if (isValidIdentity(parsed)) setIdentity(parsed);
      } catch {}
      try {
        const raw = await SecureStore.getItemAsync(CONTACTS_KEY);
        const map = tryJson<Record<string, Contact>>(raw, {});
        contactMapRef.current = map;
        setContacts(sortedContacts(map));
      } catch {}
      try {
        const name = await SecureStore.getItemAsync(DISPLAY_NAME_KEY);
        if (name) setDisplayNameState(name);
      } catch {}
      setIdentityHydrated(true);
    })();
  }, []);

  const lxmf = useLxmf({
    identityHex: identity?.identity_hex ?? 'new',
    lxmfAddressHex: identity?.address_hex ?? 'new',
    dbPath: DB_PATH,
    logLevel: 3,
  });

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
    SecureStore.setItemAsync(CONTACTS_KEY, JSON.stringify(map)).catch(() => {});
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

  // Update contacts from incoming events. Iterates all events newer than the
  // last-seen one so batch deliveries (multiple events per poll) aren't missed.
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
        if (addr.length === 32) {
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

  // lxmf.send expects body as base64 (JNI decodes it to raw bytes before LXMF framing).
  // This wrapper accepts plain UTF-8 text and encodes it so callers don't need to know the wire format.
  const lxmfSend = lxmf.send;
  const send = useCallback(async (dest: string, body: string): Promise<number> => {
    const bytes = new TextEncoder().encode(body);
    const b64 = btoa(String.fromCodePoint(...bytes));
    return lxmfSend(dest, b64);
  }, [lxmfSend]);

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
  }), [
    lxmf.isNativeAvailable, lxmf.isRunning, lxmf.status, lxmf.error, lxmf.events,
    lxmf.start, lxmf.stop, lxmf.getStatus, lxmf.setLogLevel,
    send, fetchMessages, identity, identityHydrated, clearIdentity,
    displayName, setDisplayName, contacts, upsertContact, markRead,
  ]);

  return (
    <LxmfContext.Provider value={value}>
      {children}
    </LxmfContext.Provider>
  );
}
