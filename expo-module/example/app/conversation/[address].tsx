import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  FlatList,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  StyleSheet,
  Text,
  TextInput,
  View,
} from 'react-native';
import { useLocalSearchParams, useRouter } from 'expo-router';
import { useLxmfContext, type StoredMessage } from '@/context/LxmfContext';

// ── Helpers ───────────────────────────────────────────────────────────────────

function b64decode(b64: string): string {
  if (!b64) return '';
  try {
    const bin = globalThis.atob(b64);
    const bytes = Uint8Array.from(bin, c => c.codePointAt(0) ?? 0);
    return new TextDecoder('utf-8', { fatal: false }).decode(bytes);
  } catch {
    return '';
  }
}

function b64encode(s: string): string {
  if (typeof globalThis.btoa === 'function') {
    try {
      return globalThis.btoa(
        Array.from(new TextEncoder().encode(s), b => String.fromCodePoint(b)).join(''),
      );
    } catch {}
  }
  return s;
}

function fmtTime(unix: number): string {
  const d = new Date(unix > 10_000_000_000 ? unix : unix * 1000);
  return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function shortHex(v: string): string {
  if (!v || v.length <= 12) return v || '—';
  return `${v.slice(0, 6)}…${v.slice(-6)}`;
}

// ── Types ─────────────────────────────────────────────────────────────────────

type BubbleMsg = {
  key: string;
  outbound: boolean;
  title: string;
  body: string;
  timestamp: number;
  acked: boolean;
  image?: { mimeType: string; data: string };
  files?: { name: string; data: string }[];
};

// ── Bubble ────────────────────────────────────────────────────────────────────

function Bubble({ msg }: Readonly<{ msg: BubbleMsg }>) {
  const body = b64decode(msg.body);
  const title = b64decode(msg.title);
  return (
    <View style={[S.bubbleWrap, msg.outbound ? S.bubbleRight : S.bubbleLeft]}>
      <View style={[S.bubble, msg.outbound ? S.bubbleOut : S.bubbleIn]}>
        {title ? <Text style={S.bubbleTitle}>{title}</Text> : null}
        {body ? <Text selectable style={S.bubbleBody}>{body}</Text> : null}
        {msg.image ? <Text style={S.mediaBadge}>[Image: {msg.image.mimeType}]</Text> : null}
        {msg.files?.length ? <Text style={S.mediaBadge}>[{msg.files.length} file{msg.files.length > 1 ? 's' : ''}]</Text> : null}
        <View style={S.bubbleMeta}>
          <Text style={S.bubbleTime}>{fmtTime(msg.timestamp)}</Text>
          {msg.outbound && msg.acked && <Text style={S.ackMark}> ✓</Text>}
        </View>
      </View>
    </View>
  );
}

// ── Main screen ───────────────────────────────────────────────────────────────

export default function ConversationScreen() {
  const { address } = useLocalSearchParams<{ address: string }>();
  const router = useRouter();
  const { events, send, fetchMessages, markRead, contacts, upsertContact, groups, isGroup, shareGroupInvite } = useLxmfContext();

  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);
  const [sendErr, setSendErr] = useState('');
  const listRef = useRef<FlatList>(null);

  const isGroupThread = isGroup(address ?? '');
  const group = isGroupThread ? groups.find(g => g.addrHex === address) : undefined;
  const contact = !isGroupThread ? contacts.find(c => c.address === address) : undefined;
  const peerName = group?.name ?? contact?.name ?? shortHex(address ?? '');

  // Mark thread as read on open
  useEffect(() => {
    if (address) markRead(address);
  }, [address, markRead]);

  // Load SQLite history + merge with live events
  const [sqlMsgs, setSqlMsgs] = useState<StoredMessage[]>([]);

  const loadHistory = useCallback(() => {
    if (!address) return;
    const all = fetchMessages(100);
    const forThread = all.filter(m =>
      m.source === address || m.dest === address
    );
    setSqlMsgs(forThread);
  }, [address, fetchMessages]);

  useEffect(() => { loadHistory(); }, [loadHistory]);

  // Refresh when new message arrives for this thread
  useEffect(() => {
    const latest = events[0];
    if (!latest || latest.type !== 'messageReceived') return;
    const forThisThread = isGroupThread
      ? latest.groupDest === address
      : latest.source === address && !latest.groupDest;
    if (forThisThread) {
      loadHistory();
      markRead(address ?? '');
    }
  }, [events, address, isGroupThread, loadHistory, markRead]);

  // Build merged, sorted, deduped bubble list
  const bubbles = useMemo((): BubbleMsg[] => {
    const sqlKeys = new Set(sqlMsgs.map(m => String(m.id)));
    const liveExtra: BubbleMsg[] = events
      .filter(e => e.type === 'messageReceived' && (
        isGroupThread ? e.groupDest === address : e.source === address && !e.groupDest
      ))
      .filter(e => !sqlKeys.has(String(e.id)))
      .map(e => ({
        key: `live-${e.source}-${e.timestamp ?? Date.now()}`,
        outbound: false,
        title: String(e.title ?? ''),
        body: String(e.body ?? ''),
        timestamp: typeof e.timestamp === 'number' ? e.timestamp : Math.floor(Date.now() / 1000),
        acked: false,
        image: e.image,
        files: e.files,
      }));

    const fromSql: BubbleMsg[] = sqlMsgs.map(m => ({
      key: `sql-${m.id}`,
      outbound: m.outbound,
      title: m.title ?? '',
      body: m.body ?? '',
      timestamp: m.timestamp,
      acked: m.acked,
      image: m.image,
      files: m.files,
    }));

    return [...fromSql, ...liveExtra].sort((a, b) => a.timestamp - b.timestamp);
  }, [sqlMsgs, events, address]);

  // Scroll to bottom when new bubble arrives
  useEffect(() => {
    if (bubbles.length > 0) {
      setTimeout(() => listRef.current?.scrollToEnd({ animated: true }), 100);
    }
  }, [bubbles.length]);

  const onSend = useCallback(async () => {
    const trimmed = text.trim();
    if (!trimmed || !address) return;
    setSending(true);
    setSendErr('');
    // send() in context auto-routes: group address → sendGroup, peer address → send
    const r = await send(address, trimmed);
    setSending(false);
    if (r >= 0) {
      setText('');
      if (!isGroupThread) upsertContact(address, { lastMessage: trimmed });
      loadHistory();
    } else {
      setSendErr('Send failed — message queued for retry.');
    }
  }, [text, address, send, isGroupThread, upsertContact, loadHistory]);

  const onShare = useCallback(() => {
    if (address) shareGroupInvite(address);
  }, [address, shareGroupInvite]);

  const renderBubble = useCallback(({ item }: { item: BubbleMsg }) => (
    <Bubble msg={item} />
  ), []);

  const keyExtractor = useCallback((item: BubbleMsg) => item.key, []);

  return (
    <KeyboardAvoidingView
      style={S.root}
      behavior={Platform.OS === 'ios' ? 'padding' : 'height'}
      keyboardVerticalOffset={0}>

      {/* Header */}
      <View style={S.header}>
        <Pressable style={({ pressed }) => [S.backBtn, pressed && { opacity: 0.7 }]} onPress={() => router.back()}>
          <Text style={S.backBtnText}>‹</Text>
        </Pressable>
        <View style={S.headerCenter}>
          <View style={S.headerNameRow}>
            {isGroupThread && <Text style={S.groupHash}>#</Text>}
            <Text style={S.headerName} numberOfLines={1}>{peerName}</Text>
          </View>
          <Text selectable style={S.headerAddr}>{shortHex(address ?? '')}</Text>
        </View>
        {isGroupThread && (
          <Pressable style={({ pressed }) => [S.shareBtn, pressed && { opacity: 0.7 }]} onPress={onShare}>
            <Text style={S.shareBtnText}>Share</Text>
          </Pressable>
        )}
      </View>

      {/* Message list */}
      <FlatList
        ref={listRef}
        data={bubbles}
        keyExtractor={keyExtractor}
        renderItem={renderBubble}
        contentContainerStyle={S.list}
        ListEmptyComponent={<Text style={S.empty}>No messages yet. Send the first one.</Text>}
        onContentSizeChange={() => listRef.current?.scrollToEnd({ animated: false })}
      />

      {/* Compose bar */}
      <View style={S.composeWrap}>
        {sendErr ? <Text style={S.sendErr}>{sendErr}</Text> : null}
        <View style={S.compose}>
          <TextInput
            style={S.composeInput}
            placeholder={isGroupThread ? `Message #${peerName}…` : 'Message…'}
            placeholderTextColor="#4a6070"
            value={text}
            onChangeText={setText}
            multiline
            maxLength={2000}
          />
          <Pressable
            style={({ pressed }) => [S.sendBtn, isGroupThread && S.sendBtnGroup, (!text.trim() || sending) && S.sendBtnDisabled, pressed && { opacity: 0.75 }]}
            onPress={onSend}
            disabled={!text.trim() || sending}>
            <Text style={S.sendBtnText}>{sending ? '…' : '↑'}</Text>
          </Pressable>
        </View>
      </View>
    </KeyboardAvoidingView>
  );
}

// ── Styles ────────────────────────────────────────────────────────────────────

const S = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#0c1218' },

  header: {
    flexDirection: 'row',
    alignItems: 'center',
    paddingTop: 50,
    paddingBottom: 12,
    paddingHorizontal: 12,
    backgroundColor: '#131d26',
    borderBottomWidth: 1,
    borderBottomColor: '#1e3040',
    gap: 10,
  },
  backBtn: { paddingHorizontal: 8, paddingVertical: 4 },
  backBtnText: { color: '#1a7fc1', fontSize: 30, lineHeight: 34 },
  headerCenter: { flex: 1 },
  headerNameRow: { flexDirection: 'row', alignItems: 'center', gap: 4 },
  groupHash: { color: '#3edba8', fontSize: 18, fontWeight: '700', lineHeight: 22 },
  headerName: { color: '#d8ecf8', fontSize: 16, fontWeight: '700', flex: 1 },
  headerAddr: { color: '#4a6070', fontSize: 11, fontFamily: 'monospace' },
  shareBtn: {
    paddingHorizontal: 10,
    paddingVertical: 6,
    borderRadius: 8,
    backgroundColor: '#1a3328',
    borderWidth: 1,
    borderColor: '#3edba8',
  },
  shareBtnText: { color: '#3edba8', fontSize: 12, fontWeight: '600' },

  list: { paddingHorizontal: 12, paddingVertical: 12, gap: 6, flexGrow: 1 },

  empty: { color: '#4a6070', fontSize: 14, textAlign: 'center', marginTop: 60 },

  bubbleWrap: { flexDirection: 'row', marginVertical: 2 },
  bubbleLeft: { justifyContent: 'flex-start' },
  bubbleRight: { justifyContent: 'flex-end' },

  bubble: {
    maxWidth: '78%',
    borderRadius: 16,
    paddingHorizontal: 12,
    paddingVertical: 8,
    gap: 3,
  },
  bubbleIn: {
    backgroundColor: '#1a2a38',
    borderWidth: 1,
    borderColor: '#1e3040',
    borderBottomLeftRadius: 4,
  },
  bubbleOut: {
    backgroundColor: '#1a7fc1',
    borderBottomRightRadius: 4,
  },

  bubbleTitle: { color: '#d8ecf8', fontSize: 13, fontWeight: '600', fontStyle: 'italic' },
  bubbleBody: { color: '#d8ecf8', fontSize: 14, lineHeight: 20 },
  mediaBadge: { color: '#4fb3e8', fontSize: 11, fontFamily: 'monospace' },

  bubbleMeta: { flexDirection: 'row', alignItems: 'center', justifyContent: 'flex-end', marginTop: 2 },
  bubbleTime: { color: 'rgba(216,236,248,0.55)', fontSize: 10 },
  ackMark: { color: 'rgba(216,236,248,0.75)', fontSize: 10 },

  composeWrap: {
    borderTopWidth: 1,
    borderTopColor: '#1e3040',
    backgroundColor: '#0e1923',
    paddingHorizontal: 12,
    paddingVertical: 10,
    paddingBottom: Platform.OS === 'ios' ? 28 : 10,
    gap: 6,
  },
  sendErr: { color: '#f0a500', fontSize: 12, fontFamily: 'monospace' },
  compose: { flexDirection: 'row', alignItems: 'flex-end', gap: 10 },
  composeInput: {
    flex: 1,
    borderWidth: 1,
    borderColor: '#2a4050',
    backgroundColor: '#0b1820',
    color: '#d8ecf8',
    borderRadius: 20,
    paddingHorizontal: 14,
    paddingVertical: 10,
    fontSize: 15,
    maxHeight: 120,
  },
  sendBtn: {
    width: 42,
    height: 42,
    borderRadius: 21,
    backgroundColor: '#1a7fc1',
    alignItems: 'center',
    justifyContent: 'center',
  },
  sendBtnGroup: { backgroundColor: '#1a8c6a' },
  sendBtnDisabled: { opacity: 0.35 },
  sendBtnText: { color: '#fff', fontSize: 20, fontWeight: '700' },
});
