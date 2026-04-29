# Getting Started: Running the Example App

## Quick Start (5 minutes)

### 1. Prerequisites

```bash
# Node.js 18+ with npm
node --version    # Should be v18+
npm --version     # Should be v9+

# Xcode (iOS) or Android Studio (Android)
# For physical device: just need Expo Go app
```

> **Contributors only:** If modifying Rust, rebuild first:
> `cd rust-core && cargo build --release --target aarch64-linux-android`
> Then `npm run rust:android` from `expo-module/` to copy `.so` files.

### 2. Install Dependencies

```bash
cd expo-module/example
npm install
```

### 3. Run on Device

```bash
npm start
```

Follow the prompts:
- Press `i` for iOS simulator
- Press `a` for Android emulator  
- Press `w` for web (UI only, no BLE)
- Scan QR with **Expo Go** app on physical device

---

## Step-by-Step Guide

### iOS Physical Device

1. **Install Expo Go** on your iPhone from App Store
2. **In terminal:**
   ```bash
   cd expo-module/example
   npm start
   ```
3. **Scan the QR code** with your iPhone camera
4. Tap the notification to open in Expo Go
5. App loads ✅

### Android Physical Device

1. **Install Expo Go** on your Android from Play Store
2. **In terminal:**
   ```bash
   cd expo-module/example
   npm start
   ```
3. **Open Expo Go** → Tap QR scanner icon → scan terminal QR code
4. App loads ✅

### iOS Simulator

```bash
cd expo-module/example
npm start
# Press 'i'
```

⚠️ **Note:** iOS simulator has limited BLE; better to use physical device.

### Android Emulator

```bash
cd expo-module/example
npm start
# Press 'a'
```

⚠️ **Note:** Android emulator generally doesn't support BLE; physical device recommended.

---

## App Tour

### Home Screen 🏠

1. **Status Card**: Shows if node is running
2. **Generate Identity**: Auto-generates random 64-char hex (32 bytes)
3. **Generate Address**: Auto-generates 32-char hex (16 bytes)
4. **Start Node**: Initializes LXMF node with your identity
5. Once running, shows:
   - ✅ Epoch (uptime)
   - ✅ Announces received
   - ✅ Messages received

### Beacons Screen 📡

1. Tap **"Beacons"** button (only enabled when node running)
2. Shows count of discovered beacons
3. List of peers with:
   - Destination hash (first 16 chars shown)
   - Connection state (Connected/Discovered/Disconnected)
   - Latency if available
4. Auto-refreshes every 2 seconds
5. Pull to refresh manually

### Messages Screen 💬

1. Tap **"Messages"** button (only enabled when node running)
2. **Send Message** form:
   - Enter peer's address (32 hex chars)
   - Type message text
   - Tap "Send"
3. **Recent Messages** list:
   - Shows last 50 messages
   - Indicates inbound 📥 vs outbound 📤
   - Shows timestamp
   - Shows sender hash

---

## Testing Two-Device Communication

### Setup

- **Device A**: iPhone, identity = `AAA...`
- **Device B**: Android phone, identity = `BBB...`

### Steps

1. Start app on Device A
   - Generate identity/address
   - Tap "Start Node"
   - Wait for "🟢 Running"

2. Start app on Device B
   - Generate identity/address
   - Tap "Start Node"
   - Wait for "🟢 Running"

3. On Device A, tap "Beacons"
   - Should see Device B's beacon in list
   - Shows "Connected" or "Discovered"

4. On Device B, tap "Beacons"
   - Should see Device A's beacon in list

5. Send message from Device A to Device B:
   - Copy Device B's address from beacons
   - Go to Messages
   - Paste address in "Destination" field
   - Type message
   - Tap "Send"

6. Check Device B Messages screen
   - Should show inbound message
   - Shows Device A's hash as sender

---

## Troubleshooting

### "Cannot find module @lxmf/react-native"

```bash
# From expo-module/example directory:
npm install ..
npm start
```

### Rust Library Not Found

```bash
# Check Android .so files:
ls ../expo-module/android/src/main/jniLibs/arm64-v8a/liblxmf_rn.so

# If missing, rebuild:
cd ../rust-core
cargo build --release --target aarch64-linux-android
cd ../expo-module
npm run rust:android

# Check iOS XCFramework:
ls ../expo-module/ios/RustCore/liblxmf_rn.xcframework
# If missing, build on macOS:
npm run rust:ios
```

### "Failed to start LXMF node" on iOS

- Ensure `ios/RustCore/liblxmf_rn.xcframework` is built and present
- Run `npm run rust:ios` from `expo-module/` to build it
- Rebuild: `npx expo run:ios`

### "System.loadLibrary failed" on Android

```bash
# Rebuild Rust for Android:
cd ../rust-core
cargo build --release --target aarch64-linux-android
cd ../expo-module
npm run rust:android

# Clean gradle cache:
cd android
./gradlew clean

# Rebuild example:
cd ../../expo-module/example
npx expo run:android
```

### No Beacons Discovered

- ✅ Check both devices have Bluetooth enabled
- ✅ Check both nodes show "🟢 Running"
- ✅ Check devices are within BLE range (~10 meters)
- ✅ Try pulling to refresh beacons list
- ✅ On Android, check Location permission granted
- ✅ Try stopping & starting nodes again

### App Crashes on Start

- Check console for errors: `expo logs`
- Rebuild: `npm start` → `r` to reload
- Try clearing Expo cache: `expo cache --purge`

---

## Project Structure

```
lxmf_react_native_rust/
├── rust-core/                    # Rust FFI + rns-embedded-ffi
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── ffi.rs               # C exports
│       ├── node.rs              # Rust wrapper
│       ├── beacon.rs
│       ├── store.rs
│       ├── jni_bridge.rs
│       └── framing.rs
│   └── target/release/          # OUTPUT: liblxmf_rn.a, .so
│
├── expo-module/                 # Native modules + TypeScript
│   ├── package.json
│   ├── src/
│   │   ├── LxmfModule.ts       # Native wrapper
│   │   ├── useLxmf.ts          # React hook
│   │   └── index.ts
│   ├── ios/
│   │   ├── LxmfModule.swift    # Swift Expo module
│   │   └── BLEManager.swift    # BLE dual-role
│   ├── android/
│   │   └── src/main/kotlin/
│   │       └── expo/modules/lxmf/LxmfModule.kt
│   └── LxmfReactNative.podspec
│
└── expo-module/example/                 # Runnable example
    ├── package.json
    ├── app.json                # Expo config
    ├── app/
    │   ├── _layout.tsx         # Navigation
    │   ├── index.tsx           # Home screen
    │   ├── beacons.tsx         # Beacons screen
    │   └── messages.tsx        # Messages screen
    └── README.md
```

---

## Architecture Recap

```
User (You)
    ↓
React Native App (TypeScript)
    ↓
useLxmf() Hook
    ↓
LxmfModule (Swift/Kotlin)
    ↓
C FFI / JNI Bridge
    ↓
Rust (node.rs)
    ↓
rns-embedded-ffi
    ↓
BLE Mesh Network
```

---

## Next Steps

### For Development

1. **Modify screens**: Edit files in `app/`
2. **Add more features**: Extend `useLxmf` hook
3. **Test state management**: Add Redux/Zustand
4. **Add persistence**: Store identity/beacons to disk

### For Production

1. **Build for distribution**: 
   ```bash
   eas build --platform ios    # iOS
   eas build --platform android # Android
   ```
2. **Submit to stores**: App Store / Play Store
3. **Handle permissions**: Request Bluetooth, Location
4. **Add onboarding**: Guide users through setup

---

## Quick Reference

| Task | Command |
|------|---------|
| Start dev server | `npm start` |
| Run on iOS simulator | `npm start` → `i` |
| Run on Android emulator | `npm start` → `a` |
| Run on web (UI only) | `npm start` → `w` |
| Rebuild Rust (Android) | `cd ../rust-core && cargo build --release --target aarch64-linux-android` |
| Clear cache | `npx expo start --clear` |
| View logs | `npx expo logs` |
| Reload app | Press `r` in terminal |

---

## Support

- **App Issues**: Check `expo-module/example/README.md`
- **Module Issues**: Check `expo-module/` docs
- **Rust Issues**: Check `rust-core/` docs
- **LXMF-rs**: See [FreeTAKTeam/LXMF-rs](https://github.com/FreeTAKTeam/LXMF-rs)

## Contributors
- [Magicred-1](https://github.com/Magicred-1)
- [FwazB](https://github.com/FwazB)
- [FreeTAKTeam](https://github.com/FreeTAKTeam)

---

**Happy meshing! 🚀**
