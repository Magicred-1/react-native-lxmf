//! lxmf_react_native_rust — React Native bridge for Reticulum/LXMF mesh networking
//!
//! Architecture:
//!   TypeScript (useLxmf hook) → Expo Modules (Kotlin) → JNI → this crate → rns-transport
//!
//! Mode 0: BLE mesh via rns-embedded-ffi
//! Mode 3: Standard Reticulum TCP via rns-transport (full protocol, real identity, announces)

pub mod node;
pub mod beacon;
pub mod solana_tx;
pub mod ble_iface;
pub mod nus_iface;
pub mod ffi;
pub mod framing;
pub mod log_bridge;
pub mod store;

#[cfg(target_os = "android")]
pub mod jni_bridge;

#[cfg(test)]
mod tests;
