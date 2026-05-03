import { useCallback, useState } from 'react';
import {
  FlatList,
  Modal,
  Pressable,
  StyleSheet,
  Text,
  TextInput,
  View,
} from 'react-native';
import { useRouter } from 'expo-router';
import { useLxmfContext, type Contact, type Group } from '@/context/LxmfContext';

// ── Helpers ───────────────────────────────────────────────────────────────────

function shortHex(v: string): string {
  if (!v || v.length <= 12) return v || '—';
  return `${v.slice(0, 6)}…${v.slice(-6)}`;
}

function relTime(unix: number): string {
  const diff = Math.floor(Date.now() / 1000) - unix;
  if (diff < 60) return 'now';
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}

// ── List item types ───────────────────────────────────────────────────────────

type ListItem =
  | { kind: 'group'; data: Group }
  | { kind: 'contact'; data: Contact };

// ── Group row ─────────────────────────────────────────────────────────────────

function GroupRow({ group, onPress }: Readonly<{ group: Group; onPress: () => void }>) {
  return (
    <Pressable
      style={({ pressed }) => [S.row, pressed && S.rowPressed]}
      onPress={onPress}>
      <View style={[S.avatar, S.avatarGroup]}>
        <Text style={[S.avatarText, S.avatarTextGroup]}>#</Text>
      </View>
      <View style={S.rowBody}>
        <View style={S.rowTop}>
          <Text style={S.rowName} numberOfLines={1}>{group.name}</Text>
          <Text style={S.rowGroupBadge}>GROUP</Text>
        </View>
        <Text style={S.rowPreview} numberOfLines={1}>{shortHex(group.addrHex)}</Text>
      </View>
    </Pressable>
  );
}

// ── Contact row ───────────────────────────────────────────────────────────────

function ContactRow({ contact, onPress }: Readonly<{ contact: Contact; onPress: () => void }>) {
  const label = contact.name || shortHex(contact.address);
  return (
    <Pressable
      style={({ pressed }) => [S.row, pressed && S.rowPressed]}
      onPress={onPress}>
      <View style={S.avatar}>
        <Text style={S.avatarText}>{label.slice(0, 2).toUpperCase()}</Text>
      </View>
      <View style={S.rowBody}>
        <View style={S.rowTop}>
          <Text style={S.rowName} numberOfLines={1}>{label}</Text>
          <Text style={S.rowTime}>{relTime(contact.lastSeen)}</Text>
        </View>
        <View style={S.rowBottom}>
          <Text style={S.rowPreview} numberOfLines={1}>
            {contact.lastMessage || 'No messages yet'}
          </Text>
          {contact.unread > 0 && (
            <View style={S.badge}>
              <Text style={S.badgeText}>{contact.unread > 99 ? '99+' : contact.unread}</Text>
            </View>
          )}
        </View>
      </View>
    </Pressable>
  );
}

// ── Group create/join modal ───────────────────────────────────────────────────

type GroupModalProps = {
  visible: boolean;
  onClose: () => void;
  onCreated: (addrHex: string) => void;
  onJoined: (addrHex: string) => void;
  createGroup: (name: string) => { addrHex: string; keyHex: string };
  joinGroup: (addrHex: string, keyHex: string) => boolean;
};

function GroupModal({ visible, onClose, onCreated, onJoined, createGroup, joinGroup }: Readonly<GroupModalProps>) {
  const [tab, setTab] = useState<'create' | 'join'>('create');
  const [createName, setCreateName] = useState('');
  const [joinAddr, setJoinAddr] = useState('');
  const [joinKey, setJoinKey] = useState('');
  const [err, setErr] = useState('');
  const [created, setCreated] = useState<{ addrHex: string; keyHex: string; name: string } | null>(null);

  const reset = () => {
    setCreateName('');
    setJoinAddr('');
    setJoinKey('');
    setErr('');
    setCreated(null);
    setTab('create');
  };

  const handleClose = () => { reset(); onClose(); };

  const handleCreate = () => {
    const name = createName.trim();
    if (!name) { setErr('Enter a group name.'); return; }
    try {
      const { addrHex, keyHex } = createGroup(name);
      setCreated({ addrHex, keyHex, name });
      setErr('');
    } catch (e: any) {
      setErr(e?.message ?? 'Failed to create group.');
    }
  };

  const handleJoin = () => {
    const addr = joinAddr.trim().toLowerCase();
    const key = joinKey.trim().toLowerCase();
    if (!/^[0-9a-f]{32}$/.test(addr)) { setErr('Address must be 32 hex chars.'); return; }
    if (!/^[0-9a-f]{64}$/.test(key)) { setErr('Key must be 64 hex chars (32 bytes).'); return; }
    const ok = joinGroup(addr, key);
    if (ok) { onJoined(addr); handleClose(); }
    else { setErr('Failed to join group.'); }
  };

  const handleDone = () => {
    if (created) onCreated(created.addrHex);
    handleClose();
  };

  return (
    <Modal visible={visible} transparent animationType="fade" onRequestClose={handleClose}>
      <Pressable style={S.overlay} onPress={handleClose}>
        <Pressable style={S.modal}>
          <Text style={S.modalTitle}>Group Channel</Text>

          {/* Tabs */}
          <View style={S.tabs}>
            <Pressable style={[S.tab, tab === 'create' && S.tabActive]} onPress={() => { setTab('create'); setErr(''); }}>
              <Text style={[S.tabText, tab === 'create' && S.tabTextActive]}>Create</Text>
            </Pressable>
            <Pressable style={[S.tab, tab === 'join' && S.tabActive]} onPress={() => { setTab('join'); setErr(''); }}>
              <Text style={[S.tabText, tab === 'join' && S.tabTextActive]}>Join</Text>
            </Pressable>
          </View>

          {tab === 'create' && !created && (
            <>
              <Text style={S.modalHint}>Choose a channel name. Everyone with the same name + key can communicate.</Text>
              <TextInput
                style={S.modalInput}
                placeholder="e.g. team-alpha"
                placeholderTextColor="#4a6070"
                value={createName}
                onChangeText={t => { setCreateName(t); setErr(''); }}
                autoFocus
                autoCapitalize="none"
                autoCorrect={false}
              />
              {err ? <Text style={S.modalError}>{err}</Text> : null}
              <View style={S.modalBtns}>
                <Pressable style={[S.modalBtn, S.modalBtnCancel]} onPress={handleClose}>
                  <Text style={S.modalBtnText}>Cancel</Text>
                </Pressable>
                <Pressable style={[S.modalBtn, S.modalBtnOk]} onPress={handleCreate}>
                  <Text style={S.modalBtnText}>Create</Text>
                </Pressable>
              </View>
            </>
          )}

          {tab === 'create' && created && (
            <>
              <Text style={S.modalHint}>Group created. Share address + key with members — both are required to join.</Text>
              <View style={S.inviteBox}>
                <Text style={S.inviteLabel}>Name</Text>
                <Text selectable style={S.inviteValue}>{created.name}</Text>
                <Text style={S.inviteLabel}>Address (32 hex)</Text>
                <Text selectable style={S.inviteValue}>{created.addrHex}</Text>
                <Text style={S.inviteLabel}>Key (64 hex)</Text>
                <Text selectable style={S.inviteValue}>{created.keyHex}</Text>
              </View>
              <View style={S.modalBtns}>
                <Pressable style={[S.modalBtn, S.modalBtnCancel]} onPress={handleDone}>
                  <Text style={S.modalBtnText}>Open Chat</Text>
                </Pressable>
              </View>
            </>
          )}

          {tab === 'join' && (
            <>
              <Text style={S.modalHint}>Enter the group address and shared key from an invite.</Text>
              <TextInput
                style={S.modalInput}
                placeholder="Group address (32 hex chars)"
                placeholderTextColor="#4a6070"
                value={joinAddr}
                onChangeText={t => { setJoinAddr(t); setErr(''); }}
                autoFocus
                autoCapitalize="none"
                autoCorrect={false}
              />
              <TextInput
                style={S.modalInput}
                placeholder="Key (64 hex chars)"
                placeholderTextColor="#4a6070"
                value={joinKey}
                onChangeText={t => { setJoinKey(t); setErr(''); }}
                autoCapitalize="none"
                autoCorrect={false}
              />
              {err ? <Text style={S.modalError}>{err}</Text> : null}
              <View style={S.modalBtns}>
                <Pressable style={[S.modalBtn, S.modalBtnCancel]} onPress={handleClose}>
                  <Text style={S.modalBtnText}>Cancel</Text>
                </Pressable>
                <Pressable style={[S.modalBtn, S.modalBtnOk]} onPress={handleJoin}>
                  <Text style={S.modalBtnText}>Join</Text>
                </Pressable>
              </View>
            </>
          )}
        </Pressable>
      </Pressable>
    </Modal>
  );
}

// ── Main screen ───────────────────────────────────────────────────────────────

export default function ConversationsScreen() {
  const { contacts, groups, upsertContact, createGroup, joinGroup, isRunning } = useLxmfContext();
  const router = useRouter();
  const [showNew, setShowNew] = useState(false);
  const [showGroup, setShowGroup] = useState(false);
  const [newAddr, setNewAddr] = useState('');
  const [addrError, setAddrError] = useState('');

  const openThread = useCallback((address: string) => {
    router.push(`/conversation/${address}`);
  }, [router]);

  const addContact = useCallback(() => {
    const addr = newAddr.trim().toLowerCase();
    if (!/^[0-9a-f]{32}$/.test(addr)) {
      setAddrError('Must be 32 hex characters.');
      return;
    }
    upsertContact(addr);
    setShowNew(false);
    setNewAddr('');
    setAddrError('');
    openThread(addr);
  }, [newAddr, upsertContact, openThread]);

  // Unified list: groups first, then contacts
  const listData: ListItem[] = [
    ...groups.map(g => ({ kind: 'group' as const, data: g })),
    ...contacts.map(c => ({ kind: 'contact' as const, data: c })),
  ];

  const renderItem = useCallback(({ item }: { item: ListItem }) => {
    if (item.kind === 'group') {
      return <GroupRow group={item.data} onPress={() => openThread(item.data.addrHex)} />;
    }
    return <ContactRow contact={item.data} onPress={() => openThread(item.data.address)} />;
  }, [openThread]);

  const keyExtractor = useCallback((item: ListItem) => {
    return item.kind === 'group' ? `g:${item.data.addrHex}` : `c:${item.data.address}`;
  }, []);

  return (
    <View style={S.root}>
      <View style={S.header}>
        <Text style={S.headerTitle}>Messages</Text>
        {!isRunning && (
          <Text style={S.headerHint}>Start node in Network tab to receive messages.</Text>
        )}
      </View>

      {listData.length === 0 ? (
        <View style={S.empty}>
          <Text style={S.emptyTitle}>No contacts yet</Text>
          <Text style={S.emptyBody}>
            Peer announces appear here automatically.{'\n'}
            Tap + to message a known address or create a group.
          </Text>
        </View>
      ) : (
        <FlatList
          data={listData}
          keyExtractor={keyExtractor}
          renderItem={renderItem}
          contentContainerStyle={S.list}
          ItemSeparatorComponent={Separator}
        />
      )}

      {/* FAB row */}
      <View style={S.fabRow}>
        <Pressable style={({ pressed }) => [S.fab, S.fabGroup, pressed && S.fabPressed]} onPress={() => setShowGroup(true)}>
          <Text style={S.fabText}>#</Text>
        </Pressable>
        <Pressable style={({ pressed }) => [S.fab, pressed && S.fabPressed]} onPress={() => setShowNew(true)}>
          <Text style={S.fabText}>+</Text>
        </Pressable>
      </View>

      {/* New direct message modal */}
      <Modal visible={showNew} transparent animationType="fade" onRequestClose={() => setShowNew(false)}>
        <Pressable style={S.overlay} onPress={() => setShowNew(false)}>
          <Pressable style={S.modal}>
            <Text style={S.modalTitle}>New Conversation</Text>
            <Text style={S.modalHint}>Enter 32-character LXMF address (hex)</Text>
            <TextInput
              style={S.modalInput}
              placeholder="aabbccdd…"
              placeholderTextColor="#4a6070"
              value={newAddr}
              onChangeText={t => { setNewAddr(t); setAddrError(''); }}
              autoCapitalize="none"
              autoCorrect={false}
              autoFocus
            />
            {addrError ? <Text style={S.modalError}>{addrError}</Text> : null}
            <View style={S.modalBtns}>
              <Pressable style={[S.modalBtn, S.modalBtnCancel]} onPress={() => { setShowNew(false); setNewAddr(''); setAddrError(''); }}>
                <Text style={S.modalBtnText}>Cancel</Text>
              </Pressable>
              <Pressable style={[S.modalBtn, S.modalBtnOk]} onPress={addContact}>
                <Text style={S.modalBtnText}>Open</Text>
              </Pressable>
            </View>
          </Pressable>
        </Pressable>
      </Modal>

      {/* Group create/join modal */}
      <GroupModal
        visible={showGroup}
        onClose={() => setShowGroup(false)}
        createGroup={createGroup}
        joinGroup={joinGroup}
        onCreated={(addrHex) => { setShowGroup(false); openThread(addrHex); }}
        onJoined={(addrHex) => { setShowGroup(false); openThread(addrHex); }}
      />
    </View>
  );
}

function Separator() {
  return <View style={S.separator} />;
}

// ── Styles ────────────────────────────────────────────────────────────────────

const C = {
  bg: '#0c1218',
  surface: '#131d26',
  border: '#1e3040',
  accent: '#1a7fc1',
  accentBright: '#4fb3e8',
  group: '#1a8c6a',
  groupBright: '#3edba8',
  text: '#d8ecf8',
  textDim: '#7a9db5',
  warn: '#f0a500',
};

const S = StyleSheet.create({
  root: { flex: 1, backgroundColor: C.bg },

  header: {
    paddingHorizontal: 16,
    paddingTop: 56,
    paddingBottom: 12,
    backgroundColor: C.surface,
    borderBottomWidth: 1,
    borderBottomColor: C.border,
  },
  headerTitle: { color: C.text, fontSize: 28, fontWeight: '700' },
  headerHint: { color: C.warn, fontSize: 12, marginTop: 4 },

  list: { paddingBottom: 100 },
  separator: { height: 1, backgroundColor: C.border, marginLeft: 72 },

  row: { flexDirection: 'row', alignItems: 'center', paddingHorizontal: 16, paddingVertical: 12, backgroundColor: C.surface },
  rowPressed: { backgroundColor: '#17232e' },

  avatar: {
    width: 44,
    height: 44,
    borderRadius: 22,
    backgroundColor: '#0d3550',
    borderWidth: 1,
    borderColor: C.accentBright,
    alignItems: 'center',
    justifyContent: 'center',
    marginRight: 12,
  },
  avatarGroup: {
    backgroundColor: '#0d3328',
    borderColor: C.groupBright,
  },
  avatarText: { color: C.accentBright, fontSize: 14, fontWeight: '700' },
  avatarTextGroup: { color: C.groupBright, fontSize: 20 },

  rowBody: { flex: 1 },
  rowTop: { flexDirection: 'row', justifyContent: 'space-between', alignItems: 'center', marginBottom: 3 },
  rowName: { color: C.text, fontSize: 15, fontWeight: '600', flex: 1, marginRight: 8 },
  rowTime: { color: C.textDim, fontSize: 12 },
  rowGroupBadge: { color: C.groupBright, fontSize: 10, fontWeight: '700', letterSpacing: 0.5 },

  rowBottom: { flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between' },
  rowPreview: { color: C.textDim, fontSize: 13, flex: 1, marginRight: 8 },

  badge: {
    backgroundColor: C.accent,
    borderRadius: 10,
    minWidth: 20,
    height: 20,
    alignItems: 'center',
    justifyContent: 'center',
    paddingHorizontal: 5,
  },
  badgeText: { color: '#fff', fontSize: 11, fontWeight: '700' },

  empty: { flex: 1, alignItems: 'center', justifyContent: 'center', paddingHorizontal: 40 },
  emptyTitle: { color: C.text, fontSize: 20, fontWeight: '600', marginBottom: 10 },
  emptyBody: { color: C.textDim, fontSize: 14, textAlign: 'center', lineHeight: 22 },

  fabRow: {
    position: 'absolute',
    right: 20,
    bottom: 24,
    flexDirection: 'row',
    gap: 12,
  },
  fab: {
    width: 54,
    height: 54,
    borderRadius: 27,
    backgroundColor: C.accent,
    alignItems: 'center',
    justifyContent: 'center',
    shadowColor: '#000',
    shadowOffset: { width: 0, height: 3 },
    shadowOpacity: 0.4,
    shadowRadius: 6,
    elevation: 6,
  },
  fabGroup: { backgroundColor: C.group },
  fabPressed: { opacity: 0.8 },
  fabText: { color: '#fff', fontSize: 24, lineHeight: 28, fontWeight: '400' },

  overlay: { flex: 1, backgroundColor: 'rgba(0,0,0,0.7)', justifyContent: 'center', alignItems: 'center', padding: 24 },
  modal: { width: '100%', backgroundColor: C.surface, borderRadius: 16, borderWidth: 1, borderColor: C.border, padding: 20, gap: 12 },
  modalTitle: { color: C.text, fontSize: 18, fontWeight: '700' },
  modalHint: { color: C.textDim, fontSize: 13 },
  modalInput: {
    borderWidth: 1, borderColor: '#2a4050', backgroundColor: '#0b1820', color: C.text,
    borderRadius: 10, paddingHorizontal: 12, paddingVertical: 10,
    fontFamily: 'monospace', fontSize: 13,
  },
  modalError: { color: '#ff7070', fontSize: 12 },
  modalBtns: { flexDirection: 'row', gap: 10, marginTop: 4 },
  modalBtn: { flex: 1, borderRadius: 10, paddingVertical: 11, alignItems: 'center' },
  modalBtnCancel: { backgroundColor: '#1a2e40' },
  modalBtnOk: { backgroundColor: C.accent },
  modalBtnText: { color: '#e8f6ff', fontSize: 14, fontWeight: '600' },

  tabs: { flexDirection: 'row', borderRadius: 8, overflow: 'hidden', borderWidth: 1, borderColor: C.border },
  tab: { flex: 1, paddingVertical: 8, alignItems: 'center', backgroundColor: '#0b1820' },
  tabActive: { backgroundColor: C.accent },
  tabText: { color: C.textDim, fontSize: 14, fontWeight: '600' },
  tabTextActive: { color: '#fff' },

  inviteBox: {
    backgroundColor: '#0b1820',
    borderRadius: 10,
    borderWidth: 1,
    borderColor: C.border,
    padding: 12,
    gap: 4,
  },
  inviteLabel: { color: C.textDim, fontSize: 11, fontWeight: '600', letterSpacing: 0.5, textTransform: 'uppercase' },
  inviteValue: { color: C.text, fontSize: 12, fontFamily: 'monospace', marginBottom: 6 },
});
