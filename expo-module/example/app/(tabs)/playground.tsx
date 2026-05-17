import { useState } from 'react';
import {
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
} from 'react-native';
import {
  type ExecutePaymentAccounts,
  type ExecutePaymentParams,
} from '@magicred-1/react-native-lxmf';
import { useLxmfContext } from '@/context/LxmfContext';

// ── RPC presets ───────────────────────────────────────────────────────────────

const PRESETS = [
  {
    label: 'getBlockhash',
    method: 'getLatestBlockhash',
    params: '[{"commitment":"confirmed"}]',
  },
  {
    label: 'prepare',
    method: 'prepareTransaction',
    params: '[]',
  },
  {
    label: 'cosign',
    method: 'cosignTransaction',
    params: '["<unsigned-tx-base64>"]',
  },
] as const;

// ── Account field list ────────────────────────────────────────────────────────

const ACCOUNT_FIELDS: Array<[keyof ExecutePaymentAccounts, string]> = [
  ['payer',          'Payer pubkey'],
  ['payerAta',       'Payer ATA'],
  ['recipient',      'Recipient pubkey'],
  ['recipientAta',   'Recipient ATA'],
  ['broadcasterAta', 'Broadcaster ATA'],
  ['mint',           'Mint'],
];

const EMPTY_HEX = '0'.repeat(64);

// ── Screen ────────────────────────────────────────────────────────────────────

export default function PlaygroundScreen() {
  const {
    isRunning,
    beacons,
    beaconRpcWait,
    beaconBroadcastRpc,
    requestUnsignedTx,
    submitSignedTx,
    cosignAndSubmit,
  } = useLxmfContext();

  const noBeacons = !isRunning || beacons.length === 0;

  // ── RPC console ─────────────────────────────────────────────────────────────

  const [rpcTarget, setRpcTarget] = useState('__broadcast__');
  const [rpcMethod, setRpcMethod] = useState('getLatestBlockhash');
  const [rpcParams, setRpcParams] = useState('[{"commitment":"confirmed"}]');
  const [rpcLoading, setRpcLoading] = useState(false);
  const [rpcHistory, setRpcHistory] = useState<
    Array<{ method: string; ok: boolean; body: string }>
  >([]);

  const pushHistory = (method: string, ok: boolean, body: string) =>
    setRpcHistory(prev => [{ method, ok, body }, ...prev].slice(0, 8));

  const sendRpc = async () => {
    setRpcLoading(true);
    try {
      let parsed: unknown;
      try { parsed = JSON.parse(rpcParams); } catch { parsed = []; }

      if (rpcTarget === '__broadcast__') {
        const res = await beaconBroadcastRpc(rpcMethod, parsed);
        pushHistory(rpcMethod, true, res.resultJson);
      } else {
        const res = await beaconRpcWait(rpcTarget, rpcMethod, parsed);
        pushHistory(rpcMethod, !res.isError, res.resultJson);
      }
    } catch (e: any) {
      pushHistory(rpcMethod, false, e?.message ?? String(e));
    } finally {
      setRpcLoading(false);
    }
  };

  // ── Payment builder ──────────────────────────────────────────────────────────

  const [payerKey, setPayerKey] = useState('');
  const [accounts, setAccounts] = useState<ExecutePaymentAccounts>({
    payer:          EMPTY_HEX,
    payerAta:       EMPTY_HEX,
    recipient:      EMPTY_HEX,
    recipientAta:   EMPTY_HEX,
    broadcasterAta: EMPTY_HEX,
    mint:           EMPTY_HEX,
  });
  const [amount, setAmount] = useState('1000000');
  const [compOffset, setCompOffset] = useState('0');
  const [encryptedAmount, setEncryptedAmount] = useState(EMPTY_HEX);
  const [nonce, setNonce] = useState('0');
  const [encryptionPubKey, setEncryptionPubKey] = useState(EMPTY_HEX);

  const [preparedTxB64, setPreparedTxB64] = useState<string | null>(null);
  const [preparedBeaconHash, setPreparedBeaconHash] = useState<string | null>(null);
  const [payLoading, setPayLoading] = useState(false);
  const [payResult, setPayResult] = useState<string | null>(null);

  const makeParams = (): ExecutePaymentParams => ({
    compOffset: Number(compOffset),
    amount:     Number(amount),
    encryptedAmount,
    nonce,
    encryptionPubKey,
  });

  const onPrepare = async () => {
    setPayLoading(true);
    setPayResult(null);
    setPreparedTxB64(null);
    setPreparedBeaconHash(null);
    try {
      const { unsignedTxB64, beaconHash } = await requestUnsignedTx(accounts, makeParams());
      setPreparedTxB64(unsignedTxB64);
      setPreparedBeaconHash(beaconHash);
      setPayResult(
        `prepared ✓\nbeacon: ${beaconHash.slice(0, 12)}…\ntxB64: ${unsignedTxB64.slice(0, 48)}…`,
      );
    } catch (e: any) {
      setPayResult(`error: ${e?.message ?? String(e)}`);
    } finally {
      setPayLoading(false);
    }
  };

  const onSubmit = async () => {
    if (!preparedTxB64 || !preparedBeaconHash) return;
    setPayLoading(true);
    setPayResult(null);
    try {
      const res = await submitSignedTx(preparedBeaconHash, preparedTxB64);
      setPayResult(`submitted ✓\ntxSig: ${res.txSig}\nbeacon: ${res.beaconHash.slice(0, 12)}…`);
      setPreparedTxB64(null);
      setPreparedBeaconHash(null);
    } catch (e: any) {
      setPayResult(`error: ${e?.message ?? String(e)}`);
    } finally {
      setPayLoading(false);
    }
  };

  const onSignAndSubmit = async () => {
    setPayLoading(true);
    setPayResult(null);
    setPreparedTxB64(null);
    setPreparedBeaconHash(null);
    try {
      const { txSig, beaconHash } = await cosignAndSubmit(payerKey, accounts, makeParams());
      setPayResult(`confirmed ✓\ntxSig: ${txSig}\nbeacon: ${beaconHash.slice(0, 12)}…`);
    } catch (e: any) {
      setPayResult(`error: ${e?.message ?? String(e)}`);
    } finally {
      setPayLoading(false);
    }
  };

  // ── Render ───────────────────────────────────────────────────────────────────

  return (
    <ScrollView style={S.root} contentContainerStyle={S.scroll}>
      <View style={S.header}>
        <Text style={S.headerTitle}>Playground</Text>
      </View>

      {noBeacons && (
        <View style={S.infoBox}>
          <Text style={S.infoText}>Start the node and wait for beacons (Network tab).</Text>
        </View>
      )}

      {/* ── RPC Console ─────────────────────────────────────────────────── */}
      <View style={S.card}>
        <Text style={S.cardTitle}>RPC Console</Text>

        <Text style={S.label}>Target</Text>
        <ScrollView horizontal showsHorizontalScrollIndicator={false}>
          <View style={S.chipRow}>
            <Pressable
              style={[S.chip, rpcTarget === '__broadcast__' && S.chipActive]}
              onPress={() => setRpcTarget('__broadcast__')}>
              <Text style={[S.chipText, rpcTarget === '__broadcast__' && S.chipTextActive]}>
                Broadcast
              </Text>
            </Pressable>
            {beacons.map(b => (
              <Pressable
                key={b.destHash}
                style={[S.chip, rpcTarget === b.destHash && S.chipActive]}
                onPress={() => setRpcTarget(b.destHash)}>
                <Text style={[S.chipText, rpcTarget === b.destHash && S.chipTextActive]}>
                  {b.destHash.slice(0, 10)}…
                </Text>
              </Pressable>
            ))}
          </View>
        </ScrollView>

        <Text style={S.label}>Presets</Text>
        <View style={S.chipRow}>
          {PRESETS.map(p => (
            <Pressable
              key={p.label}
              style={({ pressed }) => [S.preset, pressed && { opacity: 0.7 }]}
              onPress={() => { setRpcMethod(p.method); setRpcParams(p.params); }}>
              <Text style={S.presetText}>{p.label}</Text>
            </Pressable>
          ))}
        </View>

        <Text style={S.label}>Method</Text>
        <TextInput
          style={S.input}
          value={rpcMethod}
          onChangeText={setRpcMethod}
          autoCapitalize="none"
          autoCorrect={false}
          placeholderTextColor="#4a6070"
        />

        <Text style={S.label}>Params (JSON array)</Text>
        <TextInput
          style={[S.input, S.textArea]}
          value={rpcParams}
          onChangeText={setRpcParams}
          autoCapitalize="none"
          autoCorrect={false}
          multiline
          placeholderTextColor="#4a6070"
        />

        <Pressable
          style={({ pressed }) => [
            S.btn,
            (noBeacons || rpcLoading) && S.btnDisabled,
            pressed && S.btnPressed,
          ]}
          disabled={noBeacons || rpcLoading}
          onPress={sendRpc}>
          <Text style={S.btnText}>{rpcLoading ? '…' : 'Send'}</Text>
        </Pressable>

        {rpcHistory.length > 0 && (
          <View style={S.historyBox}>
            {rpcHistory.map((h, i) => (
              <View key={i} style={S.historyItem}>
                <Text style={[S.historyMethod, h.ok ? S.ok : S.err]}>{h.method}</Text>
                <Text style={S.historyBody} numberOfLines={4}>{h.body}</Text>
              </View>
            ))}
          </View>
        )}
      </View>

      {/* ── Payment Builder ─────────────────────────────────────────────── */}
      <View style={S.card}>
        <Text style={S.cardTitle}>Payment Builder</Text>
        <Text style={S.hint}>
          Beacon builds unsigned tx (slot 1 empty). Client signs slot 0 here (direct key) or via MWA:
          Prepare → sign externally → Submit.
        </Text>

        <Text style={S.label}>Payer private key (64-hex seed)</Text>
        <TextInput
          style={S.input}
          value={payerKey}
          onChangeText={setPayerKey}
          placeholder="32-byte seed as hex"
          placeholderTextColor="#4a6070"
          autoCapitalize="none"
          autoCorrect={false}
          secureTextEntry
        />

        <Text style={S.sectionLabel}>Accounts</Text>
        {ACCOUNT_FIELDS.map(([field, label]) => (
          <View key={field} style={S.fieldRow}>
            <Text style={S.fieldLabel}>{label}</Text>
            <TextInput
              style={[S.input, S.fieldInput]}
              value={accounts[field]}
              onChangeText={v => setAccounts(prev => ({ ...prev, [field]: v }))}
              placeholder={EMPTY_HEX.slice(0, 16) + '…'}
              placeholderTextColor="#4a6070"
              autoCapitalize="none"
              autoCorrect={false}
            />
          </View>
        ))}

        <Text style={S.sectionLabel}>Params</Text>
        <View style={S.fieldRow}>
          <Text style={S.fieldLabel}>Amount (lamports)</Text>
          <TextInput style={[S.input, S.fieldInput]} value={amount} onChangeText={setAmount}
            keyboardType="number-pad" placeholderTextColor="#4a6070" />
        </View>
        <View style={S.fieldRow}>
          <Text style={S.fieldLabel}>compOffset</Text>
          <TextInput style={[S.input, S.fieldInput]} value={compOffset} onChangeText={setCompOffset}
            keyboardType="number-pad" placeholderTextColor="#4a6070" />
        </View>
        <View style={S.fieldRow}>
          <Text style={S.fieldLabel}>encryptedAmount (hex)</Text>
          <TextInput style={[S.input, S.fieldInput]} value={encryptedAmount}
            onChangeText={setEncryptedAmount} autoCapitalize="none" autoCorrect={false}
            placeholderTextColor="#4a6070" />
        </View>
        <View style={S.fieldRow}>
          <Text style={S.fieldLabel}>nonce (u128)</Text>
          <TextInput style={[S.input, S.fieldInput]} value={nonce} onChangeText={setNonce}
            keyboardType="number-pad" placeholderTextColor="#4a6070" />
        </View>
        <View style={S.fieldRow}>
          <Text style={S.fieldLabel}>encryptionPubKey (hex)</Text>
          <TextInput style={[S.input, S.fieldInput]} value={encryptionPubKey}
            onChangeText={setEncryptionPubKey} autoCapitalize="none" autoCorrect={false}
            placeholderTextColor="#4a6070" />
        </View>

        {/* MWA split */}
        <View style={S.actionRow}>
          <Pressable
            style={({ pressed }) => [
              S.btn,
              (noBeacons || payLoading) && S.btnDisabled,
              pressed && S.btnPressed,
            ]}
            disabled={noBeacons || payLoading}
            onPress={onPrepare}>
            <Text style={S.btnText}>{payLoading ? '…' : 'Prepare'}</Text>
          </Pressable>
          <Pressable
            style={({ pressed }) => [
              S.btn,
              (!preparedTxB64 || payLoading) && S.btnDisabled,
              pressed && S.btnPressed,
            ]}
            disabled={!preparedTxB64 || payLoading}
            onPress={onSubmit}>
            <Text style={S.btnText}>{payLoading ? '…' : 'Submit'}</Text>
          </Pressable>
        </View>

        {/* Direct key path */}
        <Pressable
          style={({ pressed }) => [
            S.btn,
            S.btnGreen,
            (payerKey.length !== 64 || noBeacons || payLoading) && S.btnDisabled,
            pressed && S.btnPressed,
          ]}
          disabled={payerKey.length !== 64 || noBeacons || payLoading}
          onPress={onSignAndSubmit}>
          <Text style={S.btnText}>{payLoading ? '…' : 'Sign & Submit (direct key)'}</Text>
        </Pressable>

        {payResult !== null && (
          <ScrollView style={S.resultBox} nestedScrollEnabled>
            <Text style={[S.resultText, payResult.startsWith('error') ? S.err : S.ok]}>
              {payResult}
            </Text>
          </ScrollView>
        )}
      </View>
    </ScrollView>
  );
}

// ── Styles ────────────────────────────────────────────────────────────────────

const S = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#0c1218' },
  scroll: { paddingBottom: 48, gap: 12 },

  header: {
    paddingHorizontal: 16, paddingTop: 56, paddingBottom: 14,
    backgroundColor: '#131d26', borderBottomWidth: 1, borderBottomColor: '#1e3040',
  },
  headerTitle: { color: '#d8ecf8', fontSize: 28, fontWeight: '700' },

  infoBox: {
    backgroundColor: '#0d2030', borderRadius: 10, borderWidth: 1, borderColor: '#1e3040',
    padding: 12, marginHorizontal: 14,
  },
  infoText: { color: '#7a9db5', fontSize: 13 },

  card: {
    backgroundColor: '#131d26', borderRadius: 14, borderWidth: 1,
    borderColor: '#1e3040', padding: 16, gap: 10, marginHorizontal: 14,
  },
  cardTitle: { color: '#d8ecf8', fontSize: 16, fontWeight: '700' },
  hint: { color: '#7a9db5', fontSize: 12, lineHeight: 18 },

  label: { color: '#7a9db5', fontSize: 11, marginBottom: -4 },
  sectionLabel: { color: '#4fb3e8', fontSize: 12, fontWeight: '700', marginTop: 4 },

  input: {
    borderWidth: 1, borderColor: '#2a4050', backgroundColor: '#0b1820', color: '#d8ecf8',
    borderRadius: 10, paddingHorizontal: 12, paddingVertical: 10,
    fontFamily: 'monospace', fontSize: 12,
  },
  textArea: { minHeight: 72, textAlignVertical: 'top' },

  chipRow: { flexDirection: 'row', flexWrap: 'wrap', gap: 6 },
  chip: {
    paddingHorizontal: 12, paddingVertical: 6, borderRadius: 20,
    backgroundColor: '#0e1923', borderWidth: 1, borderColor: '#1e3040',
  },
  chipActive: { borderColor: '#1a7fc1', backgroundColor: '#0d3550' },
  chipText: { color: '#4a6070', fontSize: 12, fontWeight: '600' },
  chipTextActive: { color: '#4fb3e8' },

  preset: {
    paddingHorizontal: 10, paddingVertical: 5, borderRadius: 8,
    backgroundColor: '#0d2030', borderWidth: 1, borderColor: '#1a7fc1',
  },
  presetText: { color: '#4fb3e8', fontSize: 12, fontWeight: '600' },

  fieldRow: { gap: 4 },
  fieldLabel: { color: '#7a9db5', fontSize: 11 },
  fieldInput: { fontSize: 11 },

  actionRow: { flexDirection: 'row', gap: 8 },

  btn: {
    flex: 1, borderRadius: 10, paddingVertical: 11,
    alignItems: 'center', backgroundColor: '#1a7fc1',
  },
  btnGreen: { backgroundColor: '#1a7040' },
  btnDisabled: { opacity: 0.35 },
  btnPressed: { opacity: 0.75 },
  btnText: { color: '#e8f6ff', fontSize: 14, fontWeight: '600' },

  historyBox: { gap: 6, borderTopWidth: 1, borderTopColor: '#1e3040', paddingTop: 8 },
  historyItem: { gap: 2 },
  historyMethod: { fontSize: 11, fontWeight: '700', fontFamily: 'monospace' },
  historyBody: {
    fontSize: 11, fontFamily: 'monospace', color: '#7a9db5',
    backgroundColor: '#080f16', borderRadius: 6, padding: 6,
  },

  resultBox: { maxHeight: 140, backgroundColor: '#080f16', borderRadius: 8, padding: 8 },
  resultText: { fontSize: 12, fontFamily: 'monospace' },

  ok:  { color: '#7affb2' },
  err: { color: '#ff7a7a' },
});
