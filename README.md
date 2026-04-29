# @magicred-1/react-native-lxmf

LXMF mesh networking for React Native + Expo, powered by a Rust core.

Built by [anonme.sh](https://anonme.sh)

---

## What it is

Expo module wrapping a Rust implementation of [LXMF](https://github.com/markqvist/LXMF) over [Reticulum](https://reticulum.network). Runs on Android and iOS. Supports BLE mesh, TCP transport and RNode, and peer-to-peer encrypted messaging, no internet required.

## Install

```bash
npm install @magicred-1/react-native-lxmf
```

Requires a [custom dev client](https://docs.expo.dev/develop/development-builds/introduction/) — not compatible with Expo Go.

## Usage

```ts
import { useLxmf, LxmfNodeMode } from '@magicred-1/react-native-lxmf';

const { start, stop, send, status, beacons, events } = useLxmf({
  identityHex: 'new',
  lxmfAddressHex: 'new',
  mode: LxmfNodeMode.BleOnly,
});
```

## Modes

| Mode | Value | Description |
|------|-------|-------------|
| `BleOnly` | 0 | BLE mesh only |
| `TcpClient` | 1 | TCP client to remote node |
| `TcpServer` | 2 | TCP server |
| `Reticulum` | 3 | Full Reticulum stack via local `rnsd` |
| `ReticulumAndBle` | 4 | Full Reticulum stack + BLE mesh |

## Expo Plugin

Add to `app.json` to auto-configure BLE permissions:

```json
{
  "expo": {
    "plugins": ["@magicred-1/react-native-lxmf"]
  }
}
```

## Stack

```
React Native (TypeScript)
    ↓  useLxmf() hook
Expo Module (Swift / Kotlin)
    ↓  C FFI / JNI
Rust  —  rns-transport + LXMF
    ↓
BLE / TCP mesh / RNode (LoRa support via external hardware)
```

## Repo

[github.com/magicred-1/react-native-lxmf](https://github.com/magicred-1/react-native-lxmf)

---

[anonme.sh](https://anonme.sh)
