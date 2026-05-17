import { Tabs } from 'expo-router';
import { HapticTab } from '@/components/haptic-tab';
import { IconSymbol } from '@/components/ui/icon-symbol';

const ACCENT = '#1a7fc1';
const BG = '#0c1218';
const BORDER = '#1e3040';
const DIM = '#4a6070';

const iconConversations = ({ color }: { color: string }) => <IconSymbol size={26} name="message.fill" color={color} />;
const iconNetwork       = ({ color }: { color: string }) => <IconSymbol size={26} name="wifi" color={color} />;
const iconSettings      = ({ color }: { color: string }) => <IconSymbol size={26} name="gearshape.fill" color={color} />;
const iconPlayground    = ({ color }: { color: string }) => <IconSymbol size={26} name="hammer.fill" color={color} />;

export default function TabLayout() {
  return (
    <Tabs
      screenOptions={{
        headerShown: false,
        tabBarButton: HapticTab,
        tabBarActiveTintColor: ACCENT,
        tabBarInactiveTintColor: DIM,
        tabBarStyle: {
          backgroundColor: BG,
          borderTopColor: BORDER,
          borderTopWidth: 1,
        },
        tabBarLabelStyle: { fontSize: 11, fontWeight: '600' },
      }}>
      {/* Hide the redirect shim from the tab bar */}
      <Tabs.Screen name="index" options={{ href: null }} />
      <Tabs.Screen
        name="conversations"
        options={{ title: 'Messages', tabBarIcon: iconConversations }}
      />
      <Tabs.Screen
        name="network"
        options={{ title: 'Network', tabBarIcon: iconNetwork }}
      />
      <Tabs.Screen
        name="settings"
        options={{ title: 'Settings', tabBarIcon: iconSettings }}
      />
      <Tabs.Screen
        name="playground"
        options={{ title: 'Playground', tabBarIcon: iconPlayground }}
      />
    </Tabs>
  );
}
