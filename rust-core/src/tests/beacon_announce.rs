use crate::node::build_app_data;

const PREFIX: &[u8] = b"anonmesh::beacon::v1\0";

// ── helpers ───────────────────────────────────────────────────────────────────

/// Decode msgpack fixarray(2) [fixstr, float64] → (name_bytes, stamp_cost_bits)
fn parse_non_beacon(data: &[u8]) -> (&[u8], u64) {
    assert_eq!(data[0], 0x92, "fixarray(2)");
    let len = (data[1] & 0x1f) as usize;
    assert!(data[1] >= 0xa0 && data[1] <= 0xbf, "fixstr tag");
    let name = &data[2..2 + len];
    assert_eq!(data[2 + len], 0xcb, "float64 tag");
    let bits = u64::from_be_bytes(data[2 + len + 1..2 + len + 9].try_into().unwrap());
    (name, bits)
}

// ── non-beacon mode — msgpack [name, stamp_cost:0.0] ─────────────────────────

#[test]
fn non_beacon_returns_msgpack_with_name() {
    let d = build_app_data("Alice", false);
    let (name, bits) = parse_non_beacon(&d);
    assert_eq!(name, b"Alice");
    assert_eq!(bits, 0u64, "stamp_cost must be 0.0");
}

#[test]
fn non_beacon_empty_name_defaults_to_lxmf_mobile() {
    let d = build_app_data("", false);
    let (name, _) = parse_non_beacon(&d);
    assert_eq!(name, b"lxmf-mobile");
}

#[test]
fn non_beacon_name_truncated_to_31_bytes() {
    let long = "a".repeat(50);
    let d = build_app_data(&long, false);
    let (name, _) = parse_non_beacon(&d);
    assert_eq!(name.len(), 31);
    assert_eq!(name, b"a".repeat(31).as_slice());
}

#[test]
fn non_beacon_msgpack_total_length() {
    // fixarray(1) + fixstr_tag(1) + name(N) + float64_tag(1) + f64(8) = N + 11
    let d = build_app_data("hi", false);
    assert_eq!(d.len(), 2 + 11, "2-byte name → 13 bytes total");
}

#[test]
fn non_beacon_multibyte_utf8_truncates_at_boundary() {
    // each 'é' is 2 bytes; 16 × 'é' = 32 bytes — must truncate to 30 (15 × 'é')
    let name: String = "é".repeat(16);
    let d = build_app_data(&name, false);
    let (bytes, _) = parse_non_beacon(&d);
    assert!(bytes.len() <= 31, "must fit in fixstr");
    // all retained bytes must be valid UTF-8
    assert!(std::str::from_utf8(bytes).is_ok());
}

// ── beacon mode — raw prefix + name ──────────────────────────────────────────

#[test]
fn beacon_starts_with_prefix() {
    let d = build_app_data("Alice", true);
    assert!(d.starts_with(PREFIX), "must start with beacon prefix");
}

#[test]
fn beacon_contains_display_name_after_prefix() {
    let d = build_app_data("Bob", true);
    assert_eq!(&d[PREFIX.len()..], b"Bob");
}

#[test]
fn beacon_empty_name_defaults_to_lxmf_mobile() {
    let d = build_app_data("", true);
    assert!(d.starts_with(PREFIX));
    assert_eq!(&d[PREFIX.len()..], b"lxmf-mobile");
}

#[test]
fn beacon_name_truncated_to_31_bytes() {
    let long = "x".repeat(50);
    let d = build_app_data(&long, true);
    assert!(d.starts_with(PREFIX));
    assert_eq!(&d[PREFIX.len()..], b"x".repeat(31).as_slice());
}

#[test]
fn beacon_prefix_contains_null_separator() {
    assert_eq!(PREFIX[PREFIX.len() - 1], 0, "prefix ends with \\0 separator");
}

#[test]
fn beacon_total_length_is_prefix_plus_name() {
    let name = "MyNode";
    let d = build_app_data(name, true);
    assert_eq!(d.len(), PREFIX.len() + name.len());
}

// ── CLI startswith compatibility ──────────────────────────────────────────────

#[test]
fn cli_can_detect_beacon_via_startswith() {
    let d = build_app_data("SomeNode", true);
    assert!(d.starts_with(b"anonmesh::beacon::v1"));
}

#[test]
fn non_beacon_fails_cli_detection() {
    let d = build_app_data("SomeNode", false);
    assert!(!d.starts_with(b"anonmesh::beacon::v1"));
}

// ── Sideband compat — first byte must be fixarray(2) = 0x92 ──────────────────

#[test]
fn non_beacon_first_byte_is_fixarray2() {
    let d = build_app_data("test", false);
    assert_eq!(d[0], 0x92, "Sideband expects msgpack fixarray(2)");
}

// ── display-name decode (announce app_data → clean UTF-8) ─────────────────────

#[test]
fn decode_name_from_our_non_beacon_msgpack() {
    use crate::node::decode_display_name;
    let d = build_app_data("Alice", false); // msgpack [fixstr name, f64]
    assert_eq!(decode_display_name(&d), "Alice");
}

#[test]
fn decode_name_from_beacon_prefix() {
    use crate::node::decode_display_name;
    let d = build_app_data("RelayNode", true); // anonmesh::beacon::v1\0RelayNode
    assert_eq!(decode_display_name(&d), "RelayNode");
}

#[test]
fn decode_name_from_sideband_style_bin_array() {
    use crate::node::decode_display_name;
    // [ bin8 "Bob", nil ] — Sideband / LXMF-rs form
    let app_data = [0x92u8, 0xc4, 0x03, b'B', b'o', b'b', 0xc0];
    assert_eq!(decode_display_name(&app_data), "Bob");
}

#[test]
fn non_beacon_stamp_cost_is_zero() {
    let d = build_app_data("test", false);
    let (_, bits) = parse_non_beacon(&d);
    assert_eq!(f64::from_bits(bits), 0.0);
}
