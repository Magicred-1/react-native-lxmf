import { useCallback, useEffect, useState } from 'react';
import {
  PermissionsAndroid,
  Platform,
  Pressable,
  ScrollView,
  StyleSheet,
  Switch,
  Text,
  TextInput,
  View,
} from 'react-native';
import * as SecureStore from 'expo-secure-store';
import { LxmfModule, LxmfNodeMode } from '@magicred-1/react-native-lxmf';
import { useLxmfContext } from '@/context/LxmfContext';

const BEACON_KEYPAIR_KEY   = 'lxmf_beacon_keypair_hex';
const BEACON_RPC_KEY       = 'lxmf_beacon_rpc_url';
const BEACON_PROGRAM_ID_KEY = 'lxmf_beacon_program_id';

function shortHex(v: string): string {
  if (!v || v.length <= 12) return v || '—';
  return `${v.slice(0, 6)}…${v.slice(-6)}`;
}

const BASE58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

function base58ToHex(s: string): string | null {
  try {
    let n = 0n;
    for (const c of s) {
      const i = BASE58.indexOf(c);
      if (i < 0) return null;
      n = n * 58n + BigInt(i);
    }
    return n.toString(16).padStart(64, '0');
  } catch { return null; }
}

function normalizeProgramId(v: string): string {
  const t = v.trim();
  if (t.length === 64 && /^[0-9a-fA-F]+$/.test(t)) return t;
  return base58ToHex(t) ?? t;
}

function generateSeedHex(): string {
  const buf = new Uint8Array(32);
  const cryptoApi = globalThis.crypto ?? (globalThis as Record<string, any>).msCrypto;
  if (cryptoApi?.getRandomValues) {
    cryptoApi.getRandomValues(buf);
  } else {
    for (let i = 0; i < buf.length; i++) buf[i] = Math.trunc(Math.random() * 256);
  }
  return Array.from(buf, b => b.toString(16).padStart(2, '0')).join('');
}

type TransportTab = 'ble' | 'tcp' | 'both';

// ── Stat row ──────────────────────────────────────────────────────────────────

function Row({ label, value }: Readonly<{ label: string; value: string }>) {
  return (
    <View style={S.statRow}>
      <Text style={S.statLabel}>{label}</Text>
      <Text selectable style={S.statValue}>{value}</Text>
    </View>
  );
}

// ── Main screen ───────────────────────────────────────────────────────────────

export default function NetworkScreen() {
  const {
    isRunning, isNativeAvailable, status, error, identityHydrated, displayName,
    start, stop, getStatus,
    setBeaconKeypair, setBeaconSolanaRpc, setProgramId,
    beacons, beaconRpcWait, beaconBroadcastRpc,
    requestUnsignedTx, submitSignedTx, cosignAndSubmit,
  } = useLxmfContext();

  const [tab, setTab] = useState<TransportTab>('both');
  const [tcpHost, setTcpHost] = useState('192.168.1.135');
  const [tcpPort, setTcpPort] = useState('4243');
  const [localName, setLocalName] = useState(displayName);
  const [isBeacon, setIsBeacon] = useState(false);
  const [beaconKeyHex, setBeaconKeyHex] = useState('');
  const [beaconRpcUrl, setBeaconRpcUrl] = useState('https://api.devnet.solana.com');
  const [msg, setMsg] = useState('');
  const [bleCount, setBleCount] = useState(0);
  const [unpairedCount, setUnpairedCount] = useState(0);
  const [programIdHex, setProgramIdHex] = useState('');
  const [selectedBeacon, setSelectedBeacon] = useState<string | null>(null);
  const [rpcLoading, setRpcLoading] = useState(false);
  const [rpcResult, setRpcResult] = useState<string | null>(null);

  // Payment demo state
  const [payerKeyHex, setPayerKeyHex] = useState('');
  const [accountsJson, setAccountsJson] = useState(JSON.stringify({
    payer:          '0'.repeat(64),
    payerAta:       '0'.repeat(64),
    recipient:      '0'.repeat(64),
    recipientAta:   '0'.repeat(64),
    broadcasterAta: '0'.repeat(64),
    mint:           '0'.repeat(64),
  }, null, 2));
  const [paramsJson, setParamsJson] = useState(JSON.stringify({
    compOffset:       0,
    amount:           1_000_000,
    encryptedAmount:  '0'.repeat(64),
    nonce:            '0',
    encryptionPubKey: '0'.repeat(64),
  }, null, 2));
  const [preparedTxB64, setPreparedTxB64] = useState<string | null>(null);
  const [preparedBeaconHash, setPreparedBeaconHash] = useState<string | null>(null);
  const [paymentLoading, setPaymentLoading] = useState(false);
  const [paymentResult, setPaymentResult] = useState<string | null>(null);

  // Poll BLE peer count while running
  useEffect(() => {
    if (!isRunning) { setBleCount(0); setUnpairedCount(0); return; }
    const tick = () => {
      try { setBleCount(LxmfModule.blePeerCount()); } catch {}
      try { setUnpairedCount(LxmfModule.bleUnpairedRNodeCount()); } catch {}
    };
    tick();
    const id = setInterval(tick, 2000);
    return () => clearInterval(id);
  }, [isRunning]);

  // Auto-refresh status
  useEffect(() => {
    if (!isRunning) return;
    const id = setInterval(getStatus, 5000);
    return () => clearInterval(id);
  }, [isRunning, getStatus]);

  // Load persisted beacon config on mount
  useEffect(() => {
    SecureStore.getItemAsync(BEACON_KEYPAIR_KEY).then(v => { if (v) setBeaconKeyHex(v); }).catch(() => {});
    SecureStore.getItemAsync(BEACON_RPC_KEY).then(v => { if (v) setBeaconRpcUrl(v); }).catch(() => {});
    SecureStore.getItemAsync(BEACON_PROGRAM_ID_KEY).then(v => { if (v) setProgramIdHex(normalizeProgramId(v)); }).catch(() => {});
  }, []);

  // Persist beacon keypair when it changes (only in beacon mode)
  useEffect(() => {
    if (!isBeacon || !beaconKeyHex) return;
    SecureStore.setItemAsync(BEACON_KEYPAIR_KEY, beaconKeyHex).catch(() => {});
  }, [beaconKeyHex, isBeacon]);

  // Persist beacon RPC URL when it changes (only in beacon mode)
  useEffect(() => {
    if (!isBeacon) return;
    SecureStore.setItemAsync(BEACON_RPC_KEY, beaconRpcUrl).catch(() => {});
  }, [beaconRpcUrl, isBeacon]);

  // Persist program ID when it changes (only in beacon mode)
  useEffect(() => {
    if (!isBeacon || !programIdHex) return;
    SecureStore.setItemAsync(BEACON_PROGRAM_ID_KEY, programIdHex).catch(() => {});
  }, [programIdHex, isBeacon]);

  const requestBlePerms = async (): Promise<boolean> => {
    if (Platform.OS !== 'android') return true;
    const perms = Platform.Version >= 31
      ? [PermissionsAndroid.PERMISSIONS.BLUETOOTH_SCAN, PermissionsAndroid.PERMISSIONS.BLUETOOTH_ADVERTISE, PermissionsAndroid.PERMISSIONS.BLUETOOTH_CONNECT]
      : [PermissionsAndroid.PERMISSIONS.ACCESS_FINE_LOCATION];
    const res = await PermissionsAndroid.requestMultiple(perms);
    if (Object.values(res).some(r => r !== PermissionsAndroid.RESULTS.GRANTED)) {
      setMsg('BLE permissions denied.');
      return false;
    }
    return true;
  };

  const onStart = useCallback(async () => {
    setMsg('');
    const name = localName.trim() || 'lxmf-mobile';

    if (isBeacon) {
      if (beaconKeyHex.length !== 64) { setMsg('Beacon keypair must be 64 hex chars (32-byte seed).'); return; }
      const normalizedProgramId = normalizeProgramId(programIdHex);
      if (normalizedProgramId.length !== 64) { setMsg('Program ID must be 64 hex chars or a base58 Solana address.'); return; }
      setBeaconKeypair(beaconKeyHex);
      setBeaconSolanaRpc(beaconRpcUrl.trim());
      setProgramId(normalizedProgramId);
    }

    if (tab === 'ble') {
      if (!await requestBlePerms()) return;
      const ok = await start({ mode: LxmfNodeMode.BleOnly, displayName: name, isBeacon });
      if (!ok) setMsg('Failed to start BLE node.');
    } else {
      const host = tcpHost.trim();
      const port = Number(tcpPort);
      if (!host) { setMsg('Host required.'); return; }
      if (!Number.isInteger(port) || port < 1 || port > 65535) { setMsg('Port must be 1–65535.'); return; }
      if (tab === 'both' && !await requestBlePerms()) return;
      const mode = tab === 'tcp' ? LxmfNodeMode.Reticulum : LxmfNodeMode.ReticulumAndBle;
      const ok = await start({ mode, tcpInterfaces: [{ host, port }], displayName: name, isBeacon });
      if (!ok) setMsg('Failed to start node.');
    }
  }, [tab, tcpHost, tcpPort, localName, isBeacon, beaconKeyHex, beaconRpcUrl, programIdHex, start, setBeaconKeypair, setBeaconSolanaRpc, setProgramId]);

  const onStop = useCallback(async () => {
    await stop();
    setMsg('');
  }, [stop]);

  const modeLabel = (m: number) => ['BLE', 'TCP', 'TCP Server', 'Reticulum', 'Reticulum+BLE'][m] ?? String(m);

  const hasTcp = tab !== 'ble';

  return (
    <ScrollView style={S.root} contentContainerStyle={S.scroll}>
      <View style={S.header}>
        <Text style={S.headerTitle}>Network</Text>
      </View>

      {/* Error banner */}
      {error ? <View style={S.errorBanner}><Text style={S.errorText}>{error}</Text></View> : null}

      {/* ── Transport card ───────────────────────────────────────────── */}
      <View style={S.card}>
        <Text style={S.cardTitle}>Transport</Text>

        {/* Segment control */}
        <View style={S.segment}>
          {(['ble', 'tcp', 'both'] as TransportTab[]).map(t => (
            <Pressable
              key={t}
              style={[S.segBtn, tab === t && S.segBtnActive]}
              onPress={() => { setTab(t); setMsg(''); }}
              disabled={isRunning}>
              <Text style={[S.segText, tab === t && S.segTextActive]}>
                {t === 'ble' ? 'BLE' : t === 'tcp' ? 'TCP' : 'TCP+BLE'}
              </Text>
            </Pressable>
          ))}
        </View>

        {/* TCP fields */}
        {hasTcp && (
          <>
            <TextInput style={S.input} placeholder="Host" placeholderTextColor="#4a6070"
              value={tcpHost} onChangeText={setTcpHost} autoCapitalize="none" autoCorrect={false} editable={!isRunning} />
            <TextInput style={S.input} placeholder="Port" placeholderTextColor="#4a6070"
              value={tcpPort} onChangeText={setTcpPort} keyboardType="number-pad" editable={!isRunning} />
          </>
        )}

        {/* Display name */}
        <TextInput style={S.input} placeholder="Display name" placeholderTextColor="#4a6070"
          value={localName} onChangeText={setLocalName} autoCapitalize="none" autoCorrect={false} editable={!isRunning} />

        {/* Beacon toggle */}
        <View style={S.switchRow}>
          <Text style={S.switchLabel}>Beacon mode</Text>
          <Switch
            value={isBeacon} onValueChange={setIsBeacon} disabled={isRunning}
            trackColor={{ false: '#1e3040', true: '#1a7fc1' }}
            thumbColor={isBeacon ? '#4fb3e8' : '#4a6070'}
          />
        </View>

        {/* Beacon server config — shown only when beacon mode is on */}
        {isBeacon && (
          <>
            <View style={S.inputRow}>
              <TextInput
                style={[S.input, { flex: 1 }]}
                placeholder="Beacon keypair (64 hex chars)"
                placeholderTextColor="#4a6070"
                value={beaconKeyHex}
                onChangeText={setBeaconKeyHex}
                autoCapitalize="none"
                autoCorrect={false}
                secureTextEntry
                editable={!isRunning}
              />
              <Pressable
                style={({ pressed }) => [S.genBtn, isRunning && S.btnDisabled, pressed && S.btnPressed]}
                disabled={isRunning}
                onPress={() => setBeaconKeyHex(generateSeedHex())}>
                <Text style={S.genBtnText}>Generate</Text>
              </Pressable>
            </View>
            <TextInput
              style={S.input}
              placeholder="Program ID (hex or base58)"
              placeholderTextColor="#4a6070"
              value={programIdHex}
              onChangeText={(v) => {
                const t = v.trim();
                if ((t.length === 43 || t.length === 44) && /^[1-9A-HJ-NP-Za-km-z]+$/.test(t)) {
                  const hex = base58ToHex(t);
                  if (hex) { setProgramIdHex(hex); return; }
                }
                setProgramIdHex(t);
              }}
              autoCapitalize="none"
              autoCorrect={false}
              editable={!isRunning}
            />
            <TextInput
              style={S.input}
              placeholder="Solana RPC URL"
              placeholderTextColor="#4a6070"
              value={beaconRpcUrl}
              onChangeText={setBeaconRpcUrl}
              autoCapitalize="none"
              autoCorrect={false}
              keyboardType="url"
              editable={!isRunning}
            />
          </>
        )}

        {msg ? <Text style={S.warn}>{msg}</Text> : null}

        {unpairedCount > 0 && (
          <Text style={S.warn}>
            {unpairedCount} unpaired RNode{unpairedCount > 1 ? 's' : ''} nearby — pair in Settings &gt; Bluetooth first.
          </Text>
        )}

        <View style={S.btnRow}>
          <Pressable
            style={({ pressed }) => [S.btn, (!isNativeAvailable || isRunning || !identityHydrated) && S.btnDisabled, pressed && S.btnPressed]}
            onPress={onStart}
            disabled={!isNativeAvailable || isRunning || !identityHydrated}>
            <Text style={S.btnText}>Start</Text>
          </Pressable>
          <Pressable
            style={({ pressed }) => [S.btn, S.btnDanger, !isRunning && S.btnDisabled, pressed && S.btnPressed]}
            onPress={onStop}
            disabled={!isRunning}>
            <Text style={S.btnText}>Stop</Text>
          </Pressable>
        </View>
      </View>

      {/* ── Node Status card ─────────────────────────────────────────── */}
      <View style={S.card}>
        <View style={S.cardTitleRow}>
          <Text style={S.cardTitle}>Node Status</Text>
          <Pressable style={({ pressed }) => [S.refreshBtn, pressed && { opacity: 0.7 }]} onPress={getStatus}>
            <Text style={S.refreshText}>↻ Refresh</Text>
          </Pressable>
        </View>

        <Row label="State" value={isRunning ? '● Running' : '○ Stopped'} />
        <Row label="Mode" value={status ? modeLabel(status.mode) : '—'} />
        <Row label="Address" value={status?.addressHex ? shortHex(status.addressHex) : '—'} />
        <Row label="BLE peers" value={String(bleCount)} />
        <Row label="Pending outbound" value={String(status?.pendingOutbound ?? 0)} />
        <Row label="Messages sent" value={String(status?.outboundSent ?? 0)} />
        <Row label="Messages received" value={String(status?.lxmfMessagesReceived ?? 0)} />
        <Row label="Announces received" value={String(status?.announcesReceived ?? 0)} />
        <Row label="Inbound accepted" value={String(status?.inboundAccepted ?? 0)} />
      </View>

      {/* ── Beacon list ──────────────────────────────────────────────── */}
      {isRunning && (
        <View style={S.card}>
          <Text style={S.cardTitle}>Beacons ({beacons.length})</Text>
          {beacons.length === 0
            ? <Text style={S.emptyHint}>No beacons discovered yet — waiting for announces…</Text>
            : beacons.map(b => (
                <Pressable
                  key={b.destHash}
                  style={[S.beaconRow, selectedBeacon === b.destHash && S.beaconRowSelected]}
                  onPress={() => setSelectedBeacon(prev => prev === b.destHash ? null : b.destHash)}>
                  <View style={{ flex: 1 }}>
                    <Text style={S.beaconHash}>{shortHex(b.destHash)}</Text>
                    <Text style={S.beaconMeta}>{b.state} · {Math.round((Date.now() / 1000 - b.lastAnnounce) / 60)}m ago</Text>
                  </View>
                  {selectedBeacon === b.destHash && <Text style={S.beaconCheck}>✓</Text>}
                </Pressable>
              ))
          }
        </View>
      )}

      {/* ── RPC demo panel ───────────────────────────────────────────── */}
      {isRunning && beacons.length > 0 && (
        <View style={S.card}>
          <Text style={S.cardTitle}>Beacon RPC</Text>
          {!selectedBeacon && (
            <Text style={S.emptyHint}>Select a beacon above to use targeted ping, or use broadcast.</Text>
          )}
          <View style={S.btnRow}>
            <Pressable
              style={({ pressed }) => [S.btn, (!selectedBeacon || rpcLoading) && S.btnDisabled, pressed && S.btnPressed]}
              disabled={!selectedBeacon || rpcLoading}
              onPress={async () => {
                if (!selectedBeacon) return;
                setRpcLoading(true);
                setRpcResult(null);
                try {
                  const res = await beaconRpcWait(selectedBeacon, 'getLatestBlockhash', [{ commitment: 'confirmed' }]);
                  const parsed = JSON.parse(res.resultJson);
                  const blockhash = parsed?.result?.value?.blockhash ?? parsed?.result ?? res.resultJson;
                  setRpcResult(`blockhash: ${blockhash}\nvia: ${shortHex(selectedBeacon)}`);
                } catch (e: any) {
                  setRpcResult(`error: ${e?.message ?? String(e)}`);
                } finally {
                  setRpcLoading(false);
                }
              }}>
              <Text style={S.btnText}>{rpcLoading ? '…' : 'Ping (targeted)'}</Text>
            </Pressable>
            <Pressable
              style={({ pressed }) => [S.btn, rpcLoading && S.btnDisabled, pressed && S.btnPressed]}
              disabled={rpcLoading}
              onPress={async () => {
                setRpcLoading(true);
                setRpcResult(null);
                try {
                  const res = await beaconBroadcastRpc('getLatestBlockhash', [{ commitment: 'confirmed' }]);
                  const parsed = JSON.parse(res.resultJson);
                  const blockhash = parsed?.result?.value?.blockhash ?? parsed?.result ?? res.resultJson;
                  setRpcResult(`blockhash: ${blockhash}\nvia: ${shortHex(res.beaconHash)}`);
                } catch (e: any) {
                  setRpcResult(`error: ${e?.message ?? String(e)}`);
                } finally {
                  setRpcLoading(false);
                }
              }}>
              <Text style={S.btnText}>{rpcLoading ? '…' : 'Ping (broadcast)'}</Text>
            </Pressable>
          </View>
          {rpcResult !== null && (
            <ScrollView style={S.rpcResultBox} nestedScrollEnabled>
              <Text style={S.rpcResultText}>{rpcResult}</Text>
            </ScrollView>
          )}
        </View>
      )}

      {/* ── Payment cosign demo ──────────────────────────────────────── */}
      {isRunning && beacons.length > 0 && (
        <View style={S.card}>
          <Text style={S.cardTitle}>Payment (Cosign Demo)</Text>
          <Text style={S.emptyHint}>
            Non-MWA path — private key never leaves device. For MWA use Prepare → wallet sign → Submit.
          </Text>

          <TextInput
            style={S.input}
            placeholder="Payer private key (64 hex chars)"
            placeholderTextColor="#4a6070"
            value={payerKeyHex}
            onChangeText={setPayerKeyHex}
            autoCapitalize="none"
            autoCorrect={false}
            secureTextEntry
          />

          <Text style={S.fieldLabel}>Accounts JSON</Text>
          <TextInput
            style={[S.input, S.textArea]}
            placeholder="{ payer, payerAta, recipient, recipientAta, broadcasterAta, mint }"
            placeholderTextColor="#4a6070"
            value={accountsJson}
            onChangeText={setAccountsJson}
            autoCapitalize="none"
            autoCorrect={false}
            multiline
          />

          <Text style={S.fieldLabel}>Params JSON</Text>
          <TextInput
            style={[S.input, S.textArea]}
            placeholder="{ compOffset, amount, encryptedAmount, nonce, encryptionPubKey }"
            placeholderTextColor="#4a6070"
            value={paramsJson}
            onChangeText={setParamsJson}
            autoCapitalize="none"
            autoCorrect={false}
            multiline
          />

          <View style={S.btnRow}>
            {/* Round 1 only — useful for MWA: get the unsigned tx, sign externally */}
            <Pressable
              style={({ pressed }) => [S.btn, paymentLoading && S.btnDisabled, pressed && S.btnPressed]}
              disabled={paymentLoading}
              onPress={async () => {
                setPaymentLoading(true);
                setPaymentResult(null);
                setPreparedTxB64(null);
                setPreparedBeaconHash(null);
                try {
                  const accounts = JSON.parse(accountsJson);
                  const params   = JSON.parse(paramsJson);
                  const { unsignedTxB64, beaconHash } = await requestUnsignedTx(accounts, params);
                  setPreparedTxB64(unsignedTxB64);
                  setPreparedBeaconHash(beaconHash);
                  setPaymentResult(`prepared\nbeacon: ${shortHex(beaconHash)}\ntxB64: ${unsignedTxB64.slice(0, 40)}…`);
                } catch (e: any) {
                  setPaymentResult(`error: ${e?.message ?? String(e)}`);
                } finally {
                  setPaymentLoading(false);
                }
              }}>
              <Text style={S.btnText}>{paymentLoading ? '…' : 'Prepare'}</Text>
            </Pressable>

            {/* Submit round 2 with an already-prepared tx (MWA: sign externally first) */}
            <Pressable
              style={({ pressed }) => [S.btn, (!preparedTxB64 || !preparedBeaconHash || paymentLoading) && S.btnDisabled, pressed && S.btnPressed]}
              disabled={!preparedTxB64 || !preparedBeaconHash || paymentLoading}
              onPress={async () => {
                if (!preparedTxB64 || !preparedBeaconHash) return;
                setPaymentLoading(true);
                setPaymentResult(null);
                try {
                  const signed = await submitSignedTx(preparedBeaconHash, preparedTxB64);
                  setPaymentResult(`submitted\ntxSig: ${shortHex(signed.txSig)}\nbeacon: ${shortHex(signed.beaconHash)}`);
                  setPreparedTxB64(null);
                  setPreparedBeaconHash(null);
                } catch (e: any) {
                  setPaymentResult(`error: ${e?.message ?? String(e)}`);
                } finally {
                  setPaymentLoading(false);
                }
              }}>
              <Text style={S.btnText}>{paymentLoading ? '…' : 'Submit'}</Text>
            </Pressable>
          </View>

          {/* One-shot non-MWA: prepare + local sign + submit in one call */}
          <Pressable
            style={({ pressed }) => [S.btn, (payerKeyHex.length !== 64 || paymentLoading) && S.btnDisabled, pressed && S.btnPressed]}
            disabled={payerKeyHex.length !== 64 || paymentLoading}
            onPress={async () => {
              setPaymentLoading(true);
              setPaymentResult(null);
              setPreparedTxB64(null);
              setPreparedBeaconHash(null);
              try {
                const accounts = JSON.parse(accountsJson);
                const params   = JSON.parse(paramsJson);
                const { txSig, beaconHash } = await cosignAndSubmit(payerKeyHex, accounts, params);
                setPaymentResult(`confirmed\ntxSig: ${shortHex(txSig)}\nbeacon: ${shortHex(beaconHash)}`);
              } catch (e: any) {
                setPaymentResult(`error: ${e?.message ?? String(e)}`);
              } finally {
                setPaymentLoading(false);
              }
            }}>
            <Text style={S.btnText}>{paymentLoading ? '…' : 'Sign & Submit (direct key)'}</Text>
          </Pressable>

          {paymentResult !== null && (
            <ScrollView style={S.rpcResultBox} nestedScrollEnabled>
              <Text style={S.rpcResultText}>{paymentResult}</Text>
            </ScrollView>
          )}
        </View>
      )}
    </ScrollView>
  );
}

// ── Styles ────────────────────────────────────────────────────────────────────

const S = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#0c1218' },
  scroll: { paddingBottom: 40, gap: 12 },

  header: {
    paddingHorizontal: 16, paddingTop: 56, paddingBottom: 14,
    backgroundColor: '#131d26', borderBottomWidth: 1, borderBottomColor: '#1e3040',
  },
  headerTitle: { color: '#d8ecf8', fontSize: 28, fontWeight: '700' },

  errorBanner: { backgroundColor: '#3a1515', borderWidth: 1, borderColor: '#7a2020', padding: 10, marginHorizontal: 14, borderRadius: 10 },
  errorText: { color: '#ff9a9a', fontSize: 13 },

  card: {
    backgroundColor: '#131d26', borderRadius: 14, borderWidth: 1,
    borderColor: '#1e3040', padding: 16, gap: 10, marginHorizontal: 14,
  },
  cardTitle: { color: '#d8ecf8', fontSize: 16, fontWeight: '700' },
  cardTitleRow: { flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center' },
  refreshBtn: { paddingHorizontal: 10, paddingVertical: 4, borderRadius: 8, backgroundColor: '#0d3550', borderWidth: 1, borderColor: '#1e3040' },
  refreshText: { color: '#4fb3e8', fontSize: 12, fontWeight: '600' },

  segment: { flexDirection: 'row', borderRadius: 10, borderWidth: 1, borderColor: '#1e3040', overflow: 'hidden' },
  segBtn: { flex: 1, paddingVertical: 9, alignItems: 'center', backgroundColor: '#0e1923' },
  segBtnActive: { backgroundColor: '#0d3550' },
  segText: { color: '#4a6070', fontSize: 13, fontWeight: '600' },
  segTextActive: { color: '#4fb3e8' },

  input: {
    borderWidth: 1, borderColor: '#2a4050', backgroundColor: '#0b1820', color: '#d8ecf8',
    borderRadius: 10, paddingHorizontal: 12, paddingVertical: 10, fontFamily: 'monospace', fontSize: 13,
  },

  switchRow: { flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between', paddingVertical: 2 },
  switchLabel: { color: '#7a9db5', fontSize: 13 },

  warn: { color: '#f0a500', fontSize: 12, fontFamily: 'monospace' },

  btnRow: { flexDirection: 'row', gap: 8 },
  btn: { flex: 1, borderRadius: 10, paddingVertical: 11, alignItems: 'center', backgroundColor: '#1a7fc1' },
  btnDanger: { backgroundColor: '#c0392b' },
  btnDisabled: { opacity: 0.4 },
  btnPressed: { opacity: 0.75 },
  btnText: { color: '#e8f6ff', fontSize: 14, fontWeight: '600' },

  statRow: { flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center', paddingVertical: 2 },
  statLabel: { color: '#7a9db5', fontSize: 13 },
  statValue: { color: '#d8ecf8', fontSize: 13, fontFamily: 'monospace' },

  emptyHint: { color: '#4a6070', fontSize: 12, fontStyle: 'italic' },

  beaconRow: {
    flexDirection: 'row', alignItems: 'center', paddingVertical: 8,
    paddingHorizontal: 10, borderRadius: 8, borderWidth: 1,
    borderColor: '#1e3040', backgroundColor: '#0e1923',
  },
  beaconRowSelected: { borderColor: '#1a7fc1', backgroundColor: '#0d3550' },
  beaconHash: { color: '#d8ecf8', fontSize: 13, fontFamily: 'monospace' },
  beaconMeta: { color: '#4a6070', fontSize: 11, marginTop: 2 },
  beaconCheck: { color: '#4fb3e8', fontSize: 16, marginLeft: 8 },

  rpcResultBox: { maxHeight: 120, backgroundColor: '#080f16', borderRadius: 8, padding: 8, marginTop: 4 },
  rpcResultText: { color: '#7affb2', fontSize: 12, fontFamily: 'monospace' },

  fieldLabel: { color: '#7a9db5', fontSize: 11, marginBottom: -4 },
  textArea: { minHeight: 80, textAlignVertical: 'top' },

  inputRow: { flexDirection: 'row', gap: 8, alignItems: 'center' },
  genBtn: { paddingHorizontal: 12, paddingVertical: 10, borderRadius: 10, backgroundColor: '#0d3550', borderWidth: 1, borderColor: '#1a7fc1' },
  genBtnText: { color: '#4fb3e8', fontSize: 13, fontWeight: '600' },
});
