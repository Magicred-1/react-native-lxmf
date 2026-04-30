import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  PermissionsAndroid,
  Platform,
  Pressable,
  ScrollView,
  Share,
  StyleSheet,
  Switch,
  Text,
  TextInput,
  View,
} from 'react-native';
import * as SecureStore from 'expo-secure-store';
import { LxmfModule, LxmfNodeMode, type LxmfEvent, type LxmfMessageEvent, useLxmf } from '@magicred-1/react-native-lxmf';

// Persisted identity blob schema (versioned). Stored in expo-secure-store under
// IDENTITY_KEY — encrypted at rest on iOS (Keychain) and Android (Keystore-backed).
// Schema version bumps allow forward-compatible migrations if the FFI changes.
const IDENTITY_KEY = 'lxmf.identity.v1';
const IDENTITY_SCHEMA_VERSION = 1;
type StoredIdentity = {
  version: number;
  identity_hex: string;   // 128 hex chars (private key)
  address_hex: string;    // 32 hex chars (LXMF address)
  created_at: string;     // ISO8601
};

function isValidIdentity(blob: unknown): blob is StoredIdentity {
  if (!blob || typeof blob !== 'object') return false;
  const b = blob as Record<string, unknown>;
  return (
    typeof b.version === 'number' &&
    typeof b.identity_hex === 'string' && /^[0-9a-fA-F]{128}$/.test(b.identity_hex) &&
    typeof b.address_hex === 'string' && /^[0-9a-fA-F]{32}$/.test(b.address_hex) &&
    typeof b.created_at === 'string'
  );
}

// ── Helpers ──────────────────────────────────────────────────────────────────

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

function bytesToBase64(bytes: Uint8Array): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i] ?? 0, b1 = bytes[i + 1] ?? 0, b2 = bytes[i + 2] ?? 0;
    const t = (b0 << 16) | (b1 << 8) | b2;
    out += B64[(t >> 18) & 0x3f];
    out += B64[(t >> 12) & 0x3f];
    out += i + 1 < bytes.length ? B64[(t >> 6) & 0x3f] : '=';
    out += i + 2 < bytes.length ? B64[t & 0x3f] : '=';
  }
  return out;
}

function utf8ToBase64(s: string): string {
  if (typeof globalThis.btoa === 'function') return globalThis.btoa(s);
  if (typeof TextEncoder !== 'undefined') return bytesToBase64(new TextEncoder().encode(s));
  return s;
}

function shortHex(v: string): string {
  if (!v) return '—';
  return v.length <= 12 ? v : `${v.slice(0, 6)}…${v.slice(-6)}`;
}

function ts(e: LxmfEvent): number | null {
  const r = e.timestamp ?? e.ts ?? e.time ?? e.epoch;
  if (typeof r === 'number' && Number.isFinite(r)) return r;
  if (typeof r === 'string') { const n = Number(r); return Number.isFinite(n) ? n : null; }
  return null;
}

function fmtTime(e: LxmfEvent): string {
  const t = ts(e);
  if (!t) return 'now';
  return new Date(t > 10_000_000_000 ? t : t * 1000)
    .toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function base64ToUtf8(b64: string): string {
  if (!b64) return '';
  try {
    const binary = globalThis.atob(b64);
    const bytes = Uint8Array.from(binary, c => c.codePointAt(0) ?? 0);
    return new TextDecoder('utf-8', { fatal: false }).decode(bytes);
  } catch {
    return '';
  }
}

function evtSummary(e: LxmfEvent): string {
  if (e.type === 'announceReceived') {
    const from = shortHex(String(e.destHash ?? e.address ?? e.source ?? '?'));
    const hops = e.hops ?? e.hopCount;
    return hops === undefined ? `Announce ${from}` : `Announce ${from} (${hops} hop)`;
  }
  if (e.type === 'messageReceived') return `Msg from ${shortHex(String(e.source ?? e.from ?? '?'))}`;
  if (e.type === 'log') return String(e.message ?? e.msg ?? 'log');
  if (e.type === 'error') return String(e.message ?? 'error');
  return e.type;
}

function evtKey(e: LxmfEvent, prefix = ''): string {
  const t = ts(e) ?? 'na';
  const m = String(e.id ?? e.receipt ?? e.destHash ?? e.source ?? e.message ?? 'ev');
  return `${prefix}${e.type}-${t}-${m}`;
}

async function copyToClipboard(text: string) {
  try {
    await Share.share({ message: text });
  } catch {}
}

// ── Accordion ─────────────────────────────────────────────────────────────────

function Accordion({
  title,
  badge,
  defaultOpen = false,
  children,
}: Readonly<{
  title: string;
  badge?: string | number;
  defaultOpen?: boolean;
  children: React.ReactNode;
}>) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <View style={S.accordion}>
      <Pressable
        style={({ pressed }) => [S.accordionHeader, pressed && S.accordionHeaderPressed]}
        onPress={() => setOpen(o => !o)}>
        <Text style={S.accordionChevron}>{open ? '▾' : '▸'}</Text>
        <Text style={S.accordionTitle}>{title}</Text>
        {badge === undefined ? null : (
          <View style={S.accordionBadge}>
            <Text style={S.accordionBadgeText}>{badge}</Text>
          </View>
        )}
      </Pressable>
      {open ? <View style={S.accordionBody}>{children}</View> : null}
    </View>
  );
}

// ── Tiny components ──────────────────────────────────────────────────────────

function Btn({
  label, onPress, disabled, danger, small,
}: Readonly<{ label: string; onPress: () => void; disabled?: boolean; danger?: boolean; small?: boolean }>) {
  return (
    <Pressable
      style={({ pressed }) => [
        S.btn, danger && S.btnDanger, disabled && S.btnDisabled, small && S.btnSmall,
        pressed && !disabled && S.btnPressed,
      ]}
      onPress={onPress}
      disabled={disabled}>
      <Text style={[S.btnText, small && S.btnTextSmall]}>{label}</Text>
    </Pressable>
  );
}

function Row({ label, value, onCopy }: Readonly<{ label: string; value: string; onCopy?: () => void }>) {
  return (
    <View style={S.statRow}>
      <Text style={S.statLabel}>{label}</Text>
      <View style={S.statValueRow}>
        <Text selectable style={S.statValue}>{value}</Text>
        {onCopy ? (
          <Pressable onPress={onCopy} style={S.copyBtn}>
            <Text style={S.copyBtnText}>⎘</Text>
          </Pressable>
        ) : null}
      </View>
    </View>
  );
}

function Pill({ label, active }: Readonly<{ label: string; active: boolean }>) {
  return (
    <View style={[S.pill, active && S.pillActive]}>
      <Text style={[S.pillText, active && S.pillTextActive]}>{label}</Text>
    </View>
  );
}

// ── Main screen ──────────────────────────────────────────────────────────────

export default function HomeScreen() {
  // Transport state
  const [tcpHost, setTcpHost] = useState('192.168.1.135');
  const [tcpPort, setTcpPort] = useState('4243');
  const [displayName, setDisplayName] = useState('lxmf-mobile');
  const [isBeacon, setIsBeacon] = useState(false);
  const [bleActive, setBleActive] = useState(false);
  const [tcpActive, setTcpActive] = useState(false);
  const [transportMsg, setTransportMsg] = useState('');

  // Send state
  const [dest, setDest] = useState('');
  const [msgText, setMsgText] = useState('Hello from LXMF');
  const [sendResult, setSendResult] = useState('');

  const [unpairedRNodes, setUnpairedRNodes] = useState(0);
  const [liveBleCount, setLiveBleCount] = useState(0);

  // Identity hydration: read once from secure store on mount. Until hydrated,
  // we pass 'new' so Rust generates a fresh identity (which we'll then persist
  // after start succeeds, see effect below).
  const [storedIdentity, setStoredIdentity] = useState<StoredIdentity | null>(null);
  const [identityHydrated, setIdentityHydrated] = useState(false);
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const raw = await SecureStore.getItemAsync(IDENTITY_KEY);
        // TODO(sentinel): remove debug logs before merge — lengths/booleans only, no key material
        console.log('[persist] hydrate', { hasRaw: !!raw, rawLen: raw?.length ?? 0 });
        if (cancelled) return;
        if (raw) {
          const parsed = JSON.parse(raw);
          const valid = isValidIdentity(parsed);
          console.log('[persist] parsed', { valid, hasIdHex: typeof parsed?.identity_hex === 'string', hasAddrHex: typeof parsed?.address_hex === 'string' });
          if (valid) {
            setStoredIdentity(parsed);
          }
        }
      } catch (e: any) {
        console.log('[persist] hydrate FAIL', e?.message ?? 'unknown');
        // Corrupt blob or storage error — fall through; we'll generate fresh.
      } finally {
        if (!cancelled) setIdentityHydrated(true);
      }
    })();
    return () => { cancelled = true; };
  }, []);

  const {
    isNativeAvailable, isRunning, status, error, events, beacons,
    start, stop, send, getStatus, getIdentityHex,
    startBLE, stopBLE, bleUnpairedRNodeCount,
  } = useLxmf({
    identityHex: storedIdentity?.identity_hex ?? 'new',
    lxmfAddressHex: storedIdentity?.address_hex ?? 'new',
    logLevel: 3,
  });

  // Persist identity after node starts successfully (only if not already stored,
  // or if the running identity differs — defensive against schema migrations).
  useEffect(() => {
    if (!isRunning) return;
    const idHex = getIdentityHex();
    const addrHex = status?.addressHex;
    // TODO(sentinel): remove debug logs before merge — lengths/booleans only, no key material
    console.log('[persist] save check', {
      idHexLen: idHex?.length ?? 0,
      addrHexLen: addrHex?.length ?? 0,
      alreadyStoredSame: storedIdentity?.identity_hex === idHex && storedIdentity?.address_hex === addrHex,
    });
    if (!idHex || idHex.length !== 128) return;
    if (!addrHex || !/^[0-9a-fA-F]{32}$/.test(addrHex)) return;
    if (storedIdentity?.identity_hex === idHex && storedIdentity?.address_hex === addrHex) return;

    const blob: StoredIdentity = {
      version: IDENTITY_SCHEMA_VERSION,
      identity_hex: idHex,
      address_hex: addrHex,
      created_at: new Date().toISOString(),
    };
    SecureStore.setItemAsync(IDENTITY_KEY, JSON.stringify(blob))
      .then(() => {
        console.log('[persist] save OK');
        setStoredIdentity(blob);
      })
      .catch((e: any) => {
        console.log('[persist] save FAIL', e?.message ?? 'unknown');
        /* persistence failure is non-fatal for the running session */
      });
  }, [isRunning, status?.addressHex, storedIdentity, getIdentityHex]);

  // ── Derived ───────────────────────────────────────────────────────────────

  const counts = useMemo(() => {
    let announces = 0, logs = 0, messages = 0, errors = 0;
    for (const e of events) {
      if (e.type === 'announceReceived') announces++;
      if (e.type === 'log') logs++;
      if (e.type === 'messageReceived') messages++;
      if (e.type === 'error') errors++;
    }
    return { announces, logs, messages, errors };
  }, [events]);

  const announceEvts = useMemo(() => events.filter(e => e.type === 'announceReceived').slice(0, 20), [events]);
  const msgEvts = useMemo(() => events.filter(e => e.type === 'messageReceived').slice(0, 20), [events]);
  const logEvts = useMemo(() => events.filter(e => e.type === 'log').slice(0, 100), [events]);

  // Deduped peer identity hashes from all announce events (any interface)
  const knownPeerHashes = useMemo(() => {
    const map = new Map<string, { hash: string; name: string; lastSeen: string }>();
    for (const e of events) {
      if (e.type !== 'announceReceived') continue;
      const hash = String(e.destHash ?? e.address ?? '');
      if (!hash) continue;
      if (!map.has(hash)) {
        map.set(hash, { hash, name: e.appData ? String(e.appData) : '', lastSeen: fmtTime(e) });
      }
    }
    return Array.from(map.values());
  }, [events]);
  const allEvts = useMemo(() => events.slice(0, 30), [events]);

  // ── Actions ───────────────────────────────────────────────────────────────

  const onStartTcp = useCallback(async () => {
    setTransportMsg('');
    const host = tcpHost.trim();
    const port = Number(tcpPort);
    if (!host) { setTransportMsg('Host required.'); return; }
    if (!Number.isInteger(port) || port < 1 || port > 65535) { setTransportMsg('Port 1–65535.'); return; }
    const ok = await start({
      mode: LxmfNodeMode.ReticulumAndBle,
      tcpInterfaces: [{ host, port }],
      displayName: displayName.trim() || 'lxmf-mobile',
      isBeacon,
    });
    if (ok) {
      setTcpActive(true);
      startBLE();
      setBleActive(true);
    }
  }, [tcpHost, tcpPort, displayName, isBeacon, start, startBLE]);

  const onStopTcp = useCallback(async () => {
    stopBLE();
    await stop();
    setTcpActive(false);
    setBleActive(false);
  }, [stop, stopBLE]);

  const onStartBle = useCallback(async () => {
    setTransportMsg('');
    if (Platform.OS === 'android') {
      const perms = Platform.Version >= 31
        ? [
            PermissionsAndroid.PERMISSIONS.BLUETOOTH_SCAN,
            PermissionsAndroid.PERMISSIONS.BLUETOOTH_ADVERTISE,
            PermissionsAndroid.PERMISSIONS.BLUETOOTH_CONNECT,
          ]
        : [PermissionsAndroid.PERMISSIONS.ACCESS_FINE_LOCATION];
      const results = await PermissionsAndroid.requestMultiple(perms);
      const denied = Object.values(results).some(r => r !== PermissionsAndroid.RESULTS.GRANTED);
      if (denied) {
        setTransportMsg('BLE permissions denied.');
        return;
      }
    }
    // If node already running (started via TCP), just enable BLE hardware.
    // Otherwise start mode 4 (TCP+BLE) using the configured TCP host/port.
    if (!isRunning) {
      const host = tcpHost.trim();
      const port = Number(tcpPort);
      if (!host) { setTransportMsg('Host required for TCP+BLE mode.'); return; }
      if (!Number.isInteger(port) || port < 1 || port > 65535) { setTransportMsg('Port 1–65535.'); return; }
      const ok = await start({
        mode: LxmfNodeMode.ReticulumAndBle,
        tcpInterfaces: [{ host, port }],
        displayName: displayName.trim() || 'lxmf-mobile',
        isBeacon,
      });
      if (!ok) {
        setTransportMsg('Failed to start node.');
        return;
      }
      setTcpActive(true);
    }
    startBLE();
    setBleActive(true);
  }, [isRunning, tcpHost, tcpPort, isBeacon, start, startBLE, displayName]);

  const onStopBle = useCallback(async () => {
    stopBLE();
    setBleActive(false);
    setUnpairedRNodes(0);
    if (isRunning) {
      await stop();
    }
  }, [stopBLE, stop, isRunning]);

  // Poll for unpaired RNodes while BLE is active
  useEffect(() => {
    if (!bleActive) return;
    const id = setInterval(() => {
      try { setUnpairedRNodes(bleUnpairedRNodeCount()); } catch {}
    }, 2000);
    return () => clearInterval(id);
  }, [bleActive, bleUnpairedRNodeCount]);

  // Live BLE peer count — poll every second
  useEffect(() => {
    if (!bleActive) { setLiveBleCount(0); return; }
    const tick = () => { try { setLiveBleCount(LxmfModule.blePeerCount()); } catch {} };
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, [bleActive]);

  const onSend = useCallback(async () => {
    const d = dest.trim().toLowerCase();
    if (!/^[0-9a-f]{32}$/.test(d)) { setSendResult('Dest = 32 hex chars.'); return; }
    const r = await send(d, utf8ToBase64(msgText));
    setSendResult(r >= 0 ? `Receipt #${r}` : 'Send failed.');
  }, [dest, msgText, send]);

  const copyIdentity = useCallback(() => {
    if (status?.identityHex) copyToClipboard(status.identityHex);
  }, [status?.identityHex]);

  const copyAddress = useCallback(() => {
    if (status?.addressHex) copyToClipboard(status.addressHex);
  }, [status?.addressHex]);

  // ── Render ────────────────────────────────────────────────────────────────

  return (
    <ScrollView contentContainerStyle={S.scroll} contentInsetAdjustmentBehavior="automatic">

      {/* Header */}
      <View style={S.header}>
        <Text style={S.headerTitle}>LXMF Console</Text>
        <View style={S.headerPills}>
          <Pill label="BLE" active={bleActive} />
          <Pill label="TCP" active={tcpActive} />
          <Pill label={isRunning ? 'Running' : 'Stopped'} active={isRunning} />
        </View>
      </View>

      {/* Error banner */}
      {error ? (
        <View style={S.errorBanner}>
          <Text style={S.errorBannerText}>{error}</Text>
        </View>
      ) : null}

      {/* ── Node Status ─────────────────────────────────────────────────── */}
      <Accordion title="Node Status" defaultOpen>
        <Row label="Native module" value={isNativeAvailable ? 'Loaded ✓' : 'Missing ✗'} />
        <Row label="State" value={isRunning ? 'Running' : 'Stopped'} />
        <Row
          label="Identity"
          value={status?.identityHex ? shortHex(status.identityHex) : '—'}
          onCopy={status?.identityHex ? copyIdentity : undefined}
        />
        <Row
          label="Address"
          value={status?.addressHex ? shortHex(status.addressHex) : '—'}
          onCopy={status?.addressHex ? copyAddress : undefined}
        />
        <Row label="Announces" value={String(status?.announcesReceived ?? 0)} />
        <Row label="Messages" value={String(status?.lxmfMessagesReceived ?? 0)} />
        <Row label="Outbound sent" value={String(status?.outboundSent ?? 0)} />
        <Row label="Inbound accepted" value={String(status?.inboundAccepted ?? 0)} />
        <View style={S.btnRow}>
          <Btn label="Refresh" onPress={getStatus} small />
        </View>
      </Accordion>

      {/* ── TCP / Reticulum ──────────────────────────────────────────────── */}
      <Accordion title="TCP / Reticulum" defaultOpen>
        <Text style={S.hint}>Connect to rnsd daemon. BLE can run simultaneously.</Text>
        <TextInput
          style={S.input}
          placeholder="Host (e.g. 192.168.1.10)"
          placeholderTextColor="#607080"
          value={tcpHost}
          onChangeText={setTcpHost}
          autoCapitalize="none"
          autoCorrect={false}
        />
        <TextInput
          style={S.input}
          placeholder="Port (default 4242)"
          placeholderTextColor="#607080"
          value={tcpPort}
          onChangeText={setTcpPort}
          keyboardType="number-pad"
        />
        <TextInput
          style={S.input}
          placeholder="Display name (e.g. lxmf-mobile)"
          placeholderTextColor="#607080"
          value={displayName}
          onChangeText={setDisplayName}
          autoCapitalize="none"
          autoCorrect={false}
        />
        <View style={S.switchRow}>
          <Text style={S.switchLabel}>Beacon mode</Text>
          <Switch
            value={isBeacon}
            onValueChange={setIsBeacon}
            disabled={isRunning}
            trackColor={{ false: C.border, true: C.accent }}
            thumbColor={isBeacon ? C.accentBright : C.textDim}
          />
        </View>
        {transportMsg ? <Text style={S.warn}>{transportMsg}</Text> : null}
        <View style={S.btnRow}>
          <Btn label="Start TCP" onPress={onStartTcp} disabled={!isNativeAvailable || isRunning || !identityHydrated} />
          <Btn label="Stop TCP" onPress={onStopTcp} disabled={!isRunning} danger />
        </View>
      </Accordion>

      {/* ── BLE Mesh ─────────────────────────────────────────────────────── */}
      <Accordion title="BLE Mesh" defaultOpen>
        <Text style={S.hint}>Pair RNodes in iOS Settings &gt; Bluetooth first, then start BLE.</Text>
        <Row label="BLE active" value={bleActive ? 'Yes' : 'No'} />
        <Row label="Connected peers" value={String(liveBleCount)} />
        {unpairedRNodes > 0 && (
          <Text style={S.warn}>
            Found {unpairedRNodes} unpaired RNode{unpairedRNodes > 1 ? 's' : ''}. Open Settings &gt; Bluetooth, pair the device, then restart BLE.
          </Text>
        )}
        <View style={S.btnRow}>
          <Btn label="Start BLE" onPress={onStartBle} disabled={bleActive || !identityHydrated} />
          <Btn label="Stop BLE" onPress={onStopBle} disabled={!bleActive} danger />
        </View>
      </Accordion>

      {/* ── BLE Peers ────────────────────────────────────────────────────── */}
      <Accordion title="BLE Peers" badge={liveBleCount} defaultOpen>
        <Text style={S.hint}>
          Live BLE connections: {liveBleCount}. LXMF identity hashes appear after peer announces.
        </Text>
        {knownPeerHashes.length === 0 ? (
          <Text style={S.muted}>No peer announces received yet.</Text>
        ) : (
          knownPeerHashes.map((p) => (
            <View key={p.hash} style={S.itemCard}>
              {p.name ? <Text style={S.itemTitle}>{p.name}</Text> : null}
              <Text selectable style={S.itemBody}>{p.hash}</Text>
              <Text style={S.itemMeta}>last seen: {p.lastSeen}</Text>
              <View style={S.announceActions}>
                <Pressable style={S.copyBtn} onPress={() => copyToClipboard(p.hash)}>
                  <Text style={S.copyBtnText}>⎘</Text>
                </Pressable>
                <Pressable style={S.sendToBtn} onPress={() => { setDest(p.hash); setSendResult(''); }}>
                  <Text style={S.sendToBtnText}>→ Send</Text>
                </Pressable>
              </View>
            </View>
          ))
        )}
      </Accordion>

      {/* ── Announces ────────────────────────────────────────────────────── */}
      <Accordion title="Announces" badge={counts.announces} defaultOpen>
        {announceEvts.length === 0 ? (
          <Text style={S.muted}>No announces yet.</Text>
        ) : (
          announceEvts.map((e: LxmfEvent, i: number) => {
            const hash = String(e.destHash ?? e.address ?? '');
            const name = e.appData ? String(e.appData) : '';
            return (
              <View key={`${evtKey(e, 'ann-')}-${i}`} style={S.itemCard}>
                <View style={S.announceHeader}>
                  <View style={S.announceInfo}>
                    {name ? <Text style={S.itemTitle}>{name}</Text> : null}
                    <Text selectable style={S.itemBody}>{shortHex(hash)}</Text>
                    <Text style={S.itemMeta}>{fmtTime(e)}{e.hops !== undefined ? ` · ${e.hops} hop` : ''}</Text>
                  </View>
                  <View style={S.announceActions}>
                    <Pressable
                      style={S.copyBtn}
                      onPress={() => copyToClipboard(hash)}>
                      <Text style={S.copyBtnText}>⎘</Text>
                    </Pressable>
                    <Pressable
                      style={S.sendToBtn}
                      onPress={() => { setDest(hash); setSendResult(''); }}>
                      <Text style={S.sendToBtnText}>→ Send</Text>
                    </Pressable>
                  </View>
                </View>
              </View>
            );
          })
        )}
      </Accordion>

      {/* ── Send Message ─────────────────────────────────────────────────── */}
      <Accordion title="Send Message" defaultOpen>
        {dest ? (
          <Text style={S.destFilled}>→ {shortHex(dest)}</Text>
        ) : (
          <Text style={S.hint}>{'Tap "→ Send" on an announce above to fill the destination.'}</Text>
        )}
        <TextInput
          style={S.input}
          placeholder="Destination (32 hex chars)"
          placeholderTextColor="#607080"
          value={dest}
          onChangeText={setDest}
          autoCapitalize="none"
          autoCorrect={false}
        />
        <TextInput
          style={S.input}
          placeholder="Message text"
          placeholderTextColor="#607080"
          value={msgText}
          onChangeText={setMsgText}
        />
        <View style={S.btnRow}>
          <Btn label="Send" onPress={onSend} disabled={!isRunning} />
        </View>
        {sendResult ? <Text style={S.feedback}>{sendResult}</Text> : null}
      </Accordion>

      {/* ── Messages ─────────────────────────────────────────────────────── */}
      <Accordion title="Messages" badge={counts.messages} defaultOpen>
        {msgEvts.length === 0 ? (
          <Text style={S.muted}>No messages yet.</Text>
        ) : (
          msgEvts.map((e, i) => {
            const msg = e as unknown as LxmfMessageEvent;
            const bodyText = base64ToUtf8(msg.body ?? '');
            const titleText = msg.title ? base64ToUtf8(msg.title) : '';
            const sender = msg.source ?? '';
            return (
              <View key={`${evtKey(e, 'msg-')}-${i}`} style={S.itemCard}>
                <View style={S.announceHeader}>
                  <View style={S.announceInfo}>
                    <Text selectable style={S.itemTitle}>From: {shortHex(sender)}</Text>
                    {titleText ? <Text selectable style={S.msgTitle}>{titleText}</Text> : null}
                    {bodyText ? <Text selectable style={S.itemBody}>{bodyText}</Text> : null}
                    {msg.image ? (
                      <Text style={S.mediaBadge}>[img: {msg.image.mimeType}]</Text>
                    ) : null}
                    {msg.files?.length ? (
                      <Text style={S.mediaBadge}>[{msg.files.length} file{msg.files.length > 1 ? 's' : ''}]</Text>
                    ) : null}
                    <Text style={S.itemMeta}>{fmtTime(e)}</Text>
                  </View>
                  {sender ? (
                    <View style={S.announceActions}>
                      <Pressable style={S.copyBtn} onPress={() => copyToClipboard(sender)}>
                        <Text style={S.copyBtnText}>⎘</Text>
                      </Pressable>
                      <Pressable style={S.sendToBtn} onPress={() => { setDest(sender); setSendResult(''); }}>
                        <Text style={S.sendToBtnText}>↩ Reply</Text>
                      </Pressable>
                    </View>
                  ) : null}
                </View>
              </View>
            );
          })
        )}
      </Accordion>

      {/* ── Event Log ────────────────────────────────────────────────────── */}
      <Accordion title="Event Log" badge={allEvts.length} defaultOpen={false}>
        {allEvts.length === 0 ? (
          <Text style={S.muted}>No events yet.</Text>
        ) : (
          allEvts.map((e, i) => (
            <View key={`${evtKey(e, 'el-')}-${i}`} style={S.logRow}>
              <Text style={S.logTag}>{e.type}</Text>
              <Text selectable style={S.logText} numberOfLines={2}>{evtSummary(e)}</Text>
              <Text style={S.logTime}>{fmtTime(e)}</Text>
            </View>
          ))
        )}
      </Accordion>

      {/* ── Debug Logs ───────────────────────────────────────────────────── */}
      <Accordion title="Debug Logs" badge={counts.logs} defaultOpen>
        {logEvts.length === 0 ? (
          <Text style={S.muted}>No logs yet.</Text>
        ) : (
          logEvts.map((e, i) => (
            <View key={`${evtKey(e, 'lg-')}-${i}`} style={S.logRow}>
              <Text style={S.logTime}>{fmtTime(e)}</Text>
              <Text selectable style={S.logLine}>{String(e.message ?? e.msg ?? evtSummary(e))}</Text>
            </View>
          ))
        )}
      </Accordion>

    </ScrollView>
  );
}

// ── Styles ───────────────────────────────────────────────────────────────────

const C = {
  bg: '#0c1218',
  surface: '#131d26',
  border: '#1e3040',
  accent: '#1a7fc1',
  accentBright: '#4fb3e8',
  danger: '#c0392b',
  text: '#d8ecf8',
  textDim: '#7a9db5',
  textMono: '#a8c8dc',
  green: '#2ecc71',
  warn: '#f0a500',
};

const S = StyleSheet.create({
  scroll: {
    paddingHorizontal: 14,
    paddingTop: 14,
    paddingBottom: 60,
    gap: 10,
    backgroundColor: C.bg,
  },

  // Header
  header: {
    backgroundColor: C.surface,
    borderRadius: 14,
    borderWidth: 1,
    borderColor: C.border,
    padding: 14,
    gap: 10,
  },
  headerTitle: { color: C.text, fontSize: 26, fontWeight: '700' },
  headerPills: { flexDirection: 'row', gap: 8 },

  pill: {
    borderRadius: 20,
    borderWidth: 1,
    borderColor: C.border,
    paddingHorizontal: 10,
    paddingVertical: 4,
    backgroundColor: '#0e1923',
  },
  pillActive: { borderColor: C.accentBright, backgroundColor: '#0d3550' },
  pillText: { color: C.textDim, fontSize: 12, fontWeight: '600' },
  pillTextActive: { color: C.accentBright },

  errorBanner: {
    backgroundColor: '#3a1515',
    borderRadius: 10,
    borderWidth: 1,
    borderColor: '#7a2020',
    padding: 10,
  },
  errorBannerText: { color: '#ff9a9a', fontSize: 13 },

  // Accordion
  accordion: {
    backgroundColor: C.surface,
    borderRadius: 14,
    borderWidth: 1,
    borderColor: C.border,
    overflow: 'hidden',
  },
  accordionHeader: {
    flexDirection: 'row',
    alignItems: 'center',
    paddingHorizontal: 14,
    paddingVertical: 13,
    gap: 8,
  },
  accordionHeaderPressed: { backgroundColor: '#17232e' },
  accordionChevron: { color: C.textDim, fontSize: 14, width: 14 },
  accordionTitle: { color: C.text, fontSize: 16, fontWeight: '600', flex: 1 },
  accordionBadge: {
    backgroundColor: '#0d3550',
    borderRadius: 10,
    paddingHorizontal: 7,
    paddingVertical: 2,
    minWidth: 24,
    alignItems: 'center',
  },
  accordionBadgeText: { color: C.accentBright, fontSize: 11, fontWeight: '700' },
  accordionBody: {
    paddingHorizontal: 14,
    paddingBottom: 14,
    gap: 8,
    borderTopWidth: 1,
    borderTopColor: C.border,
  },

  // Stat rows
  statRow: { flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center' },
  statLabel: { color: C.textDim, fontSize: 13 },
  statValueRow: { flexDirection: 'row', alignItems: 'center', gap: 6 },
  statValue: { color: C.text, fontSize: 13, fontFamily: 'monospace' },
  copyBtn: {
    paddingHorizontal: 6,
    paddingVertical: 2,
    borderRadius: 6,
    backgroundColor: '#0d3550',
    borderWidth: 1,
    borderColor: C.border,
  },
  copyBtnText: { color: C.accentBright, fontSize: 13 },

  hint: { color: C.textDim, fontSize: 12, marginBottom: 2 },

  // Input
  input: {
    borderWidth: 1,
    borderColor: '#2a4050',
    backgroundColor: '#0b1820',
    color: C.text,
    borderRadius: 10,
    paddingHorizontal: 10,
    paddingVertical: 10,
    fontFamily: 'monospace',
    fontSize: 13,
  },

  warn: { color: C.warn, fontSize: 12, fontFamily: 'monospace' },
  feedback: { color: C.green, fontSize: 13, fontFamily: 'monospace' },
  muted: { color: C.textDim, fontSize: 13 },

  // Buttons
  btnRow: { flexDirection: 'row', gap: 8, marginTop: 2 },
  btn: {
    flex: 1,
    borderRadius: 10,
    paddingVertical: 10,
    alignItems: 'center',
    backgroundColor: C.accent,
  },
  btnSmall: { paddingVertical: 7, flex: 0, paddingHorizontal: 16 },
  btnDanger: { backgroundColor: C.danger },
  btnDisabled: { opacity: 0.4 },
  btnPressed: { opacity: 0.78 },
  btnText: { color: '#e8f6ff', fontSize: 14, fontWeight: '600' },
  btnTextSmall: { fontSize: 12 },

  // Item cards (announces, messages, beacons)
  itemCard: {
    borderWidth: 1,
    borderColor: '#1f3348',
    borderRadius: 10,
    padding: 10,
    backgroundColor: '#0e1e2b',
    gap: 3,
  },
  itemTitle: { color: C.text, fontSize: 13, fontWeight: '600' },
  itemBody: { color: C.textMono, fontSize: 13, fontFamily: 'monospace' },
  itemMeta: { color: C.textDim, fontSize: 11, fontFamily: 'monospace' },

  // Log rows
  logRow: { flexDirection: 'row', alignItems: 'flex-start', gap: 6 },
  logTag: { color: C.accentBright, fontFamily: 'monospace', fontSize: 10, width: 100 },
  logText: { color: C.textMono, flex: 1, fontSize: 11, fontFamily: 'monospace' },
  logTime: { color: C.textDim, fontFamily: 'monospace', fontSize: 10 },
  logLine: { color: C.textMono, fontSize: 11, fontFamily: 'monospace' },

  // Announce card layout
  announceHeader: { flexDirection: 'row', alignItems: 'center', gap: 8 },
  announceInfo: { flex: 1, gap: 2 },
  announceActions: { flexDirection: 'row', gap: 6, alignItems: 'center' },

  // Send-to button on announce cards
  sendToBtn: {
    paddingHorizontal: 8,
    paddingVertical: 4,
    borderRadius: 6,
    backgroundColor: '#0d3550',
    borderWidth: 1,
    borderColor: C.accentBright,
  },
  sendToBtnText: { color: C.accentBright, fontSize: 12, fontWeight: '600' },

  // Destination pre-filled indicator
  destFilled: { color: C.accentBright, fontSize: 12, fontFamily: 'monospace' },

  // Beacon mode toggle row
  switchRow: { flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between', paddingVertical: 4 },
  switchLabel: { color: C.textDim, fontSize: 13 },

  // Message card extras
  msgTitle: { color: C.text, fontSize: 13, fontWeight: '600', fontStyle: 'italic' },
  mediaBadge: { color: C.accentBright, fontSize: 11, fontFamily: 'monospace', marginTop: 2 },
});
