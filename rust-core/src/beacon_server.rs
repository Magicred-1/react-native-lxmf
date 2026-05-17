//! Beacon-side JSON-RPC 2.0 server for the anon0mesh co-sign protocol.
//!
//! When this node runs as a beacon (`is_beacon = true`), inbound link data that
//! looks like a JSON-RPC *request* (has a "method" field) is routed here instead
//! of being treated as a client-side response.
//!
//! Two categories of method are handled:
//!   1. `cosignTransaction` — sign slot 1 of the partial tx and submit to Solana
//!   2. Everything else   — forward to the configured Solana JSON-RPC URL (proxy)

use base64::Engine as _;
use ed25519_dalek::Signer;

use crate::beacon::{compress_payload, decompress_payload, RpcRequest};

// ── Public config ─────────────────────────────────────────────────────────────

/// Runtime configuration for a beacon node.
/// Stored in `LxmfNode` behind `Arc<Mutex>` so FFI can update it after init.
#[derive(Default)]
pub struct BeaconConfig {
    /// ed25519 signing keypair for co-signing Solana transactions (slot 1 / broadcaster).
    pub keypair: Option<ed25519_dalek::SigningKey>,
    /// Solana JSON-RPC endpoint URL (e.g. "https://api.mainnet-beta.solana.com").
    pub solana_rpc_url: Option<String>,
    /// ble_revshare Anchor program address — deployment-specific (devnet vs mainnet).
    /// Set once via `lxmf_set_program_id`; read by `lxmf_partial_sign_execute_payment`.
    pub program_id: Option<[u8; 32]>,
}

// ── Request detection ─────────────────────────────────────────────────────────

/// Returns true if `data` (possibly compressed) looks like a JSON-RPC *request*.
///
/// Both requests and responses are JSON/zlib, but only requests carry a "method" key.
/// This distinguishes beacon-side (incoming request) from client-side (incoming response).
pub fn is_rpc_request(data: &[u8]) -> bool {
    let raw = match decompress_payload(data) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let v: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    v.get("method").is_some()
}

// ── Top-level dispatcher ──────────────────────────────────────────────────────

/// Parse and handle an incoming JSON-RPC request. Returns compressed response bytes.
///
/// `keypair` and `program_id` are optional: proxy methods (`getLatestBlockhash`, etc.)
/// only require `solana_rpc_url`. `cosignTransaction` and `prepareTransaction` return a
/// JSON-RPC error if the beacon keypair or program_id are not configured.
pub async fn handle_rpc_request(
    data: &[u8],
    keypair: Option<&ed25519_dalek::SigningKey>,
    solana_rpc_url: &str,
    program_id: Option<[u8; 32]>,
) -> Vec<u8> {
    let raw = match decompress_payload(data) {
        Ok(r) => r,
        Err(e) => return error_response(0, -32700, &format!("decompress failed: {e}")),
    };
    let req: RpcRequest = match serde_json::from_slice(&raw) {
        Ok(r) => r,
        Err(e) => return error_response(0, -32700, &format!("parse failed: {e}")),
    };

    let id = req.id;
    match req.method.as_str() {
        "cosignTransaction" => match (keypair, program_id) {
            (Some(kp), Some(pid)) => cosign_and_submit(id, &req.params, kp, solana_rpc_url, pid).await,
            _ => error_response(id, -32002, "cosignTransaction requires beacon keypair and program_id"),
        },
        "prepareTransaction" => match (keypair, program_id) {
            (Some(kp), Some(pid)) => prepare_transaction(id, &req.params, kp, solana_rpc_url, pid).await,
            _ => error_response(id, -32002, "prepareTransaction requires beacon keypair and program_id"),
        },
        method => proxy_solana(id, method, req.params, solana_rpc_url).await,
    }
}

// ── cosignTransaction ─────────────────────────────────────────────────────────

/// Co-sign a partial Solana transaction and submit it via `sendRawTransaction`.
///
/// Protocol (matches `partial_sign_execute_payment` in solana_tx.rs):
///   Wire format: [compact_u16: num_sigs] [sig_0: 64B] [sig_1: 64B (zeros)] [message...]
///   Beacon signs `message` bytes and fills sig_1.
///
/// Returns compressed JSON-RPC response: `{"result": "tx_sig_b58"}` or an error.
async fn cosign_and_submit(
    request_id: u32,
    params: &serde_json::Value,
    keypair: &ed25519_dalek::SigningKey,
    solana_rpc_url: &str,
    program_id: [u8; 32],
) -> Vec<u8> {
    // params is a JSON array; element 0 is the base64 partial tx
    let partial_tx_b64 = match params.get(0).and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response(request_id, -32602, "cosignTransaction: missing params[0]"),
    };

    let mut tx_bytes = match base64::engine::general_purpose::STANDARD.decode(partial_tx_b64) {
        Ok(b) => b,
        Err(e) => return error_response(request_id, -32602, &format!("base64 decode: {e}")),
    };

    // Parse compact_u16 signature count
    let (num_sigs, consumed) = match read_compact_u16(&tx_bytes) {
        Some(v) => v,
        None => return error_response(request_id, -32602, "tx: compact_u16 parse failed"),
    };
    if num_sigs < 2 {
        return error_response(request_id, -32602, "tx: expected at least 2 signers");
    }

    let sig1_start = consumed + 64;      // slot 1 starts after sig_0
    let sig1_end   = sig1_start + 64;
    let msg_start  = consumed + (num_sigs as usize) * 64;

    if tx_bytes.len() < msg_start {
        return error_response(request_id, -32602, "tx: too short for declared sig count");
    }

    // Sign the Solana message bytes (everything after the signature array)
    let message_bytes = tx_bytes[msg_start..].to_vec();

    // Verify payer's slot 0 signature before cosigning — rejects crafted/invalid txs
    let payer_pubkey = match extract_account_key(&message_bytes, 0) {
        Some(k) => k,
        None => return error_response(request_id, -32602, "tx: cannot extract payer pubkey"),
    };
    if !crate::solana_tx::verify_slot_signature(&tx_bytes, &payer_pubkey, 0) {
        return error_response(request_id, -32600, "tx: payer signature (slot 0) is invalid");
    }

    if !verify_execute_payment_instruction(&message_bytes, &program_id) {
        return error_response(request_id, -32602,
            "cosignTransaction: not a valid execute_payment for the configured program");
    }

    let sig: ed25519_dalek::Signature = keypair.sign(&message_bytes);
    tx_bytes[sig1_start..sig1_end].copy_from_slice(&sig.to_bytes());

    let full_tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);

    // Submit to Solana
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendRawTransaction",
        "params": [full_tx_b64, {"encoding": "base64", "skipPreflight": true}]
    });

    let client = match build_http_client() {
        Ok(c) => c,
        Err(e) => return error_response(request_id, -32000, &format!("http client: {e}")),
    };

    let resp_json = match post_json(&client, solana_rpc_url, &body).await {
        Ok(v) => v,
        Err(e) => return error_response(request_id, -32000, &format!("sendRawTransaction: {e}")),
    };

    // Relay Solana's result/error back to the client, preserving our request_id
    rewrap_response(request_id, resp_json)
}

// ── prepareTransaction ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct PrepareAccountsJson {
    payer: String,
    #[serde(rename = "payerAta")]      payer_ata: String,
    recipient: String,
    #[serde(rename = "recipientAta")]  recipient_ata: String,
    #[serde(rename = "broadcasterAta")] broadcaster_ata: String,
    mint: String,
}

#[derive(serde::Deserialize)]
struct PrepareParamsJson {
    #[serde(rename = "compOffset")]       comp_offset: u64,
    amount: u64,
    #[serde(rename = "encryptedAmount")]  encrypted_amount: String,
    nonce: String,
    #[serde(rename = "encryptionPubKey")] encryption_pub_key: String,
}

#[derive(serde::Deserialize)]
struct PrepareTransactionRequest {
    accounts: PrepareAccountsJson,
    params: PrepareParamsJson,
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    hex::decode(s).ok()?.try_into().ok()
}

/// Build an unsigned execute_payment tx and return it base64-encoded.
/// Beacon fills its own pubkey as `broadcaster`; client signs payer slot 0 before cosigning.
async fn prepare_transaction(
    id: u32,
    params: &serde_json::Value,
    keypair: &ed25519_dalek::SigningKey,
    solana_rpc_url: &str,
    program_id: [u8; 32],
) -> Vec<u8> {
    let arg = match params.get(0).and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response(id, -32602, "prepareTransaction: missing params[0]"),
    };
    let req: PrepareTransactionRequest = match serde_json::from_str(arg) {
        Ok(r) => r,
        Err(e) => return error_response(id, -32602, &format!("prepareTransaction: invalid params: {e}")),
    };

    let blockhash = match fetch_latest_blockhash(solana_rpc_url).await {
        Ok(b) => b,
        Err(e) => return error_response(id, -32603, &format!("getLatestBlockhash failed: {e}")),
    };

    let broadcaster = keypair.verifying_key().to_bytes(); // public bytes only

    let payer = match hex32(&req.accounts.payer) { Some(b) => b, None => return error_response(id, -32602, "bad payer") };
    let accounts = crate::solana_tx::PlainExecutePaymentAccounts {
        payer,
        payer_ata:      match hex32(&req.accounts.payer_ata)      { Some(b) => b, None => return error_response(id, -32602, "bad payerAta") },
        recipient:      match hex32(&req.accounts.recipient)      { Some(b) => b, None => return error_response(id, -32602, "bad recipient") },
        recipient_ata:  match hex32(&req.accounts.recipient_ata)  { Some(b) => b, None => return error_response(id, -32602, "bad recipientAta") },
        broadcaster_ata:match hex32(&req.accounts.broadcaster_ata){ Some(b) => b, None => return error_response(id, -32602, "bad broadcasterAta") },
        mint:           match hex32(&req.accounts.mint)           { Some(b) => b, None => return error_response(id, -32602, "bad mint") },
        program_id,
    };

    let enc_amt = match hex32(&req.params.encrypted_amount) { Some(b) => b, None => return error_response(id, -32602, "bad encryptedAmount") };
    let nonce_u128: u128 = match req.params.nonce.parse() { Ok(n) => n, Err(_) => return error_response(id, -32602, "bad nonce") };
    let ep_params = crate::solana_tx::ExecutePaymentParams {
        comp_offset: req.params.comp_offset,
        amount: req.params.amount,
        encrypted_amount: enc_amt,
        nonce: nonce_u128,
        encryption_pub_key: match hex32(&req.params.encryption_pub_key) { Some(b) => b, None => return error_response(id, -32602, "bad encryptionPubKey") },
    };

    let tx_bytes = crate::solana_tx::build_unsigned_execute_payment(
        &payer, &broadcaster, blockhash, &accounts, &ep_params,
    );
    let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);
    let resp = serde_json::json!({
        "jsonrpc": "2.0", "id": id,
        "result": { "unsignedTxB64": tx_b64 }
    });
    compress_payload(resp.to_string().as_bytes())
}

async fn fetch_latest_blockhash(solana_rpc_url: &str) -> Result<[u8; 32], String> {
    let client = build_http_client().map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "getLatestBlockhash",
        "params": [{"commitment": "confirmed"}]
    });
    let resp = post_json(&client, solana_rpc_url, &body).await?;
    let b58 = resp["result"]["value"]["blockhash"]
        .as_str()
        .ok_or_else(|| "getLatestBlockhash: no blockhash field".to_string())?;
    let bytes = bs58::decode(b58).into_vec().map_err(|e| format!("bs58 decode: {e}"))?;
    bytes.try_into().map_err(|_| "blockhash not 32 bytes".to_string())
}

// ── Solana RPC proxy ──────────────────────────────────────────────────────────

/// Forward any other Solana JSON-RPC method straight to the configured RPC URL.
/// Preserves the client's `request_id` in the response.
async fn proxy_solana(
    request_id: u32,
    method: &str,
    params: serde_json::Value,
    solana_rpc_url: &str,
) -> Vec<u8> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": params,
    });

    let client = match build_http_client() {
        Ok(c) => c,
        Err(e) => return error_response(request_id, -32000, &format!("http client: {e}")),
    };

    let resp_json = match post_json(&client, solana_rpc_url, &body).await {
        Ok(v) => v,
        Err(e) => return error_response(request_id, -32000, &format!("proxy {method}: {e}")),
    };

    rewrap_response(request_id, resp_json)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Verify the Solana legacy message's first instruction is an `execute_payment` call
/// targeting the configured `program_id`.
///
/// Checks both:
///   1. The instruction's program account key matches `program_id`
///   2. The instruction data begins with the Anchor discriminator for `execute_payment`
///
/// Returns false on any parse failure, program mismatch, or discriminator mismatch.
fn verify_execute_payment_instruction(message_bytes: &[u8], program_id: &[u8; 32]) -> bool {
    // Solana legacy message wire format:
    //   [3 bytes]        header (num_required_sigs, num_readonly_signed, num_readonly_unsigned)
    //   [compact_u16]    account_keys count (N)
    //   [N × 32 bytes]   account keys
    //   [32 bytes]       recent_blockhash
    //   [compact_u16]    instruction count
    //   --- first instruction ---
    //   [1 byte]         program_id_index  → index into account_keys
    //   [compact_u16]    account_indices count (M)
    //   [M × 1 byte]     account indices
    //   [compact_u16]    data length
    //   [data_len bytes] instruction data (first 8 = Anchor discriminator)

    if message_bytes.len() < 3 { return false; }
    let mut cur = 3usize; // skip 3-byte message header

    let (account_count, n) = match read_compact_u16(&message_bytes[cur..]) {
        Some(v) => v, None => return false,
    };
    cur += n;
    let account_bytes = (account_count as usize).saturating_mul(32);
    if cur + account_bytes + 32 > message_bytes.len() { return false; }
    let account_keys = &message_bytes[cur..cur + account_bytes];
    cur += account_bytes + 32; // skip account keys + recent blockhash

    let (ix_count, n) = match read_compact_u16(&message_bytes[cur..]) {
        Some(v) => v, None => return false,
    };
    if ix_count == 0 { return false; }
    cur += n;

    // First instruction: program_id_index (1 byte)
    if cur >= message_bytes.len() { return false; }
    let pid_idx = message_bytes[cur] as usize;
    cur += 1;

    // Verify the program account key matches the stored program_id
    let key_start = pid_idx.saturating_mul(32);
    if key_start + 32 > account_keys.len() { return false; }
    if &account_keys[key_start..key_start + 32] != program_id.as_slice() { return false; }

    // Account indices
    let (acct_idx_count, n) = match read_compact_u16(&message_bytes[cur..]) {
        Some(v) => v, None => return false,
    };
    cur += n;
    if cur + acct_idx_count as usize > message_bytes.len() { return false; }
    cur += acct_idx_count as usize;

    // Instruction data discriminator check
    let (data_len, n) = match read_compact_u16(&message_bytes[cur..]) {
        Some(v) => v, None => return false,
    };
    cur += n;

    if data_len < 8 || cur + 8 > message_bytes.len() { return false; }

    let expected = crate::solana_tx::anchor_discriminator("execute_payment");
    message_bytes[cur..cur + 8] == expected
}

/// Extract the Nth 32-byte account key from a Solana legacy message body.
/// `message_bytes` starts at the 3-byte header (not the tx-level sig array).
fn extract_account_key(message_bytes: &[u8], index: usize) -> Option<[u8; 32]> {
    if message_bytes.len() < 3 { return None; }
    let (count, n) = read_compact_u16(&message_bytes[3..])?;
    if index >= count as usize { return None; }
    let key_start = 3 + n + index * 32;
    if key_start + 32 > message_bytes.len() { return None; }
    let mut key = [0u8; 32];
    key.copy_from_slice(&message_bytes[key_start..key_start + 32]);
    Some(key)
}

/// Parse a Solana compact_u16 from the start of `data`.
/// Returns `(value, bytes_consumed)` or `None` on malformed input.
fn read_compact_u16(data: &[u8]) -> Option<(u16, usize)> {
    let mut result = 0u16;
    let mut shift = 0u16;
    for (i, &b) in data.iter().enumerate().take(3) {
        result |= ((b & 0x7f) as u16) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return Some((result, i + 1));
        }
    }
    None
}

fn build_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    client
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| e.to_string())
}

/// Re-wrap a Solana HTTP response, fixing the `id` field to match the client's request_id.
/// Compresses the result using the beacon protocol's zlib encoding.
fn rewrap_response(request_id: u32, mut resp: serde_json::Value) -> Vec<u8> {
    resp["id"] = serde_json::json!(request_id);
    let bytes = serde_json::to_vec(&resp).unwrap_or_default();
    compress_payload(&bytes)
}

/// Build a compressed JSON-RPC error response.
fn error_response(id: u32, code: i32, message: &str) -> Vec<u8> {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    });
    compress_payload(resp.to_string().as_bytes())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_u16_single_byte() {
        assert_eq!(read_compact_u16(&[0x02]), Some((2, 1)));
        assert_eq!(read_compact_u16(&[0x7f]), Some((127, 1)));
    }

    #[test]
    fn compact_u16_two_bytes() {
        assert_eq!(read_compact_u16(&[0x80, 0x01]), Some((128, 2)));
    }

    #[test]
    fn is_rpc_request_detects_method_field() {
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"getLatestBlockhash","params":[]}"#;
        assert!(is_rpc_request(req.as_bytes()));
    }

    #[test]
    fn is_rpc_request_rejects_response() {
        let resp = r#"{"jsonrpc":"2.0","id":1,"result":"5NG..."}"#;
        assert!(!is_rpc_request(resp.as_bytes()));
    }

    #[test]
    fn is_rpc_request_handles_compressed() {
        use crate::beacon::compress_payload;
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"getLatestBlockhash","params":[]}"#;
        let compressed = compress_payload(req.as_bytes());
        assert!(is_rpc_request(&compressed));
    }

    #[test]
    fn is_rpc_request_rejects_compressed_response() {
        use crate::beacon::compress_payload;
        let resp = r#"{"jsonrpc":"2.0","id":1,"result":"5NG..."}"#;
        let compressed = compress_payload(resp.as_bytes());
        assert!(!is_rpc_request(&compressed));
    }

    /// Build a minimal Solana legacy message with a single instruction whose data
    /// begins with the given 8-byte discriminator.
    fn make_message_with_discriminator(discriminator: [u8; 8]) -> Vec<u8> {
        let mut msg = Vec::new();
        // Header: 2 required sigs, 0 read-only signed, 1 read-only unsigned
        msg.extend_from_slice(&[2u8, 0u8, 1u8]);
        // Account count: 3 (compact_u16 = single byte 0x03)
        msg.push(0x03);
        // 3 × 32-byte account keys (zeroed)
        msg.extend_from_slice(&[0u8; 96]);
        // Recent blockhash (32 bytes, zeroed)
        msg.extend_from_slice(&[0u8; 32]);
        // Instruction count: 1
        msg.push(0x01);
        // First instruction: program_id_index = 2
        msg.push(2u8);
        // Account indices count: 2 (compact_u16)
        msg.push(0x02);
        // Account indices
        msg.extend_from_slice(&[0u8, 1u8]);
        // Data length: 8 (compact_u16)
        msg.push(0x08);
        // Discriminator
        msg.extend_from_slice(&discriminator);
        msg
    }

    // make_message_with_discriminator sets program_id_index = 2 and all account keys = [0u8; 32],
    // so the expected program_id for these tests is [0u8; 32].
    const ZERO_PROGRAM_ID: [u8; 32] = [0u8; 32];

    #[test]
    fn discriminator_check_accepts_execute_payment() {
        let disc = crate::solana_tx::anchor_discriminator("execute_payment");
        let msg = make_message_with_discriminator(disc);
        assert!(verify_execute_payment_instruction(&msg, &ZERO_PROGRAM_ID));
    }

    #[test]
    fn discriminator_check_rejects_other_instruction() {
        // Anchor discriminator for a different method — same program, wrong instruction
        let wrong_disc = [0u8; 8];
        let msg = make_message_with_discriminator(wrong_disc);
        assert!(!verify_execute_payment_instruction(&msg, &ZERO_PROGRAM_ID));
    }

    #[test]
    fn discriminator_check_rejects_truncated_message() {
        assert!(!verify_execute_payment_instruction(&[], &ZERO_PROGRAM_ID));
        assert!(!verify_execute_payment_instruction(&[0u8; 10], &ZERO_PROGRAM_ID));
    }

    #[test]
    fn discriminator_check_rejects_wrong_program_id() {
        let disc = crate::solana_tx::anchor_discriminator("execute_payment");
        let msg = make_message_with_discriminator(disc);
        // Correct discriminator but wrong program_id → must reject
        let wrong_program_id = [0xFFu8; 32];
        assert!(!verify_execute_payment_instruction(&msg, &wrong_program_id));
    }

    #[test]
    fn error_response_is_valid_json() {
        let bytes = error_response(7, -32000, "test error");
        // decompress and check
        let raw = decompress_payload(&bytes).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["error"]["code"], -32000);
    }

    // ── extract_account_key ───────────────────────────────────────────────────

    #[test]
    fn extract_account_key_returns_first_key() {
        // make_message_with_discriminator: header [2,0,1], count [3], 3×32B keys all zeros
        let disc = crate::solana_tx::anchor_discriminator("execute_payment");
        let msg = make_message_with_discriminator(disc);
        // All three account keys are [0u8; 32] — verify index 0 and 2 return them
        assert_eq!(extract_account_key(&msg, 0), Some([0u8; 32]));
        assert_eq!(extract_account_key(&msg, 2), Some([0u8; 32]));
    }

    #[test]
    fn extract_account_key_out_of_bounds_returns_none() {
        let disc = [0u8; 8];
        let msg = make_message_with_discriminator(disc); // 3 accounts
        assert_eq!(extract_account_key(&msg, 3), None); // index == count
        assert_eq!(extract_account_key(&msg, 99), None);
    }

    #[test]
    fn extract_account_key_returns_none_on_truncated_message() {
        assert_eq!(extract_account_key(&[], 0), None);
        assert_eq!(extract_account_key(&[0u8; 2], 0), None); // less than 3-byte header
    }

    // ── payer-sig gate (unit tests the verify_slot_signature logic that
    //    cosign_and_submit calls before co-signing) ──────────────────────────

    fn make_plain_tx_bytes() -> (Vec<u8>, ed25519_dalek::SigningKey) {
        use crate::solana_tx::*;
        let seed = [77u8; 32];
        let keypair = ed25519_dalek::SigningKey::from_bytes(&seed);
        let payer = keypair.verifying_key().to_bytes();
        let accounts = PlainExecutePaymentAccounts {
            payer,
            payer_ata:      [3u8; 32],
            recipient:      [4u8; 32],
            recipient_ata:  [5u8; 32],
            broadcaster_ata:[6u8; 32],
            mint:           [7u8; 32],
            program_id:     [8u8; 32],
        };
        let params = ExecutePaymentParams {
            comp_offset: 0, amount: 500_000,
            encrypted_amount: [0u8; 32], nonce: 1,
            encryption_pub_key: [0u8; 32],
        };
        let tx = build_unsigned_execute_payment(&payer, &[2u8; 32], [0u8; 32], &accounts, &params);
        (tx, keypair)
    }

    #[test]
    fn cosign_gate_rejects_unsigned_payer_slot() {
        use crate::solana_tx::verify_slot_signature;
        let (tx, keypair) = make_plain_tx_bytes();
        let payer_pubkey = keypair.verifying_key().to_bytes();
        // slot 0 is all zeros → must fail (this is what cosign_and_submit checks)
        assert!(!verify_slot_signature(&tx, &payer_pubkey, 0));
    }

    #[test]
    fn cosign_gate_accepts_signed_payer_slot() {
        use crate::solana_tx::{sign_tx_at_slot, verify_slot_signature};
        let (tx, keypair) = make_plain_tx_bytes();
        let payer_pubkey = keypair.verifying_key().to_bytes();
        let signed = sign_tx_at_slot(&tx, &keypair, 0);
        // now slot 0 carries a real sig
        assert!(verify_slot_signature(&signed, &payer_pubkey, 0));
        // slot 1 is still zeros → still fails
        let broadcaster = [2u8; 32];
        assert!(!verify_slot_signature(&signed, &broadcaster, 1));
    }

    #[test]
    fn extract_account_key_matches_payer_in_plain_tx() {
        use crate::solana_tx::*;
        let seed = [77u8; 32];
        let keypair = ed25519_dalek::SigningKey::from_bytes(&seed);
        let payer = keypair.verifying_key().to_bytes();
        let broadcaster = [2u8; 32];
        let accounts = PlainExecutePaymentAccounts {
            payer,
            payer_ata:      [3u8; 32],
            recipient:      [4u8; 32],
            recipient_ata:  [5u8; 32],
            broadcaster_ata:[6u8; 32],
            mint:           [7u8; 32],
            program_id:     [8u8; 32],
        };
        let params = ExecutePaymentParams {
            comp_offset: 0, amount: 0,
            encrypted_amount: [0u8; 32], nonce: 0,
            encryption_pub_key: [0u8; 32],
        };
        let tx = build_unsigned_execute_payment(&payer, &broadcaster, [0u8; 32], &accounts, &params);

        // message starts at byte 1 + 2*64 = 129 (compact_u16(2) + 2 zero sigs)
        let msg = &tx[129..];
        // account[0] should be the payer pubkey
        let extracted = extract_account_key(msg, 0).unwrap();
        assert_eq!(extracted, payer);
        // account[1] should be the broadcaster pubkey
        let extracted_broadcaster = extract_account_key(msg, 1).unwrap();
        assert_eq!(extracted_broadcaster, broadcaster);
    }
}
