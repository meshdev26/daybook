#![no_main]
//! Fuzz target for the security-relevant pure functions. One combined target
//! (first byte selects the function) keeps the sanitizer build count at one.
//!
//!   cargo fuzz run fuzz_core            # ASan, release
//!   make fuzz FUZZ_SECS=60              # bounded campaign via the Makefile

use libfuzzer_sys::fuzz_target;

/// Inverse of `esc`. Order matters: `&amp;` must decode LAST — decoding it
/// first would fabricate new entity sequences (e.g. esc("&#39;") is
/// "&amp;#39;"; restoring & early lets the &#39; pass corrupt it).
fn decode_entities(e: &str) -> String {
    e.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fuzz_target!(|data: &[u8]| {
    let Some(&tag) = data.first() else { return };
    let rest = &data[1..];
    match tag % 8 {
        // esc(): any UTF-8 input must produce metachar-free output that
        // exactly round-trips through entity decoding.
        0 => {
            if let Ok(s) = std::str::from_utf8(rest) {
                let e = daybook::web::esc(s);
                for c in ['<', '>', '"', '\''] {
                    assert!(!e.contains(c), "raw {c} survived escaping: {e:?}");
                }
                assert_eq!(decode_entities(&e), s);
            }
        }
        // HMAC-SHA256 + ct_eq: correct digest self-verifies, a mutated digest
        // never verifies, for arbitrary key/message splits of the input.
        1 => {
            if rest.len() >= 3 {
                let third = rest.len() / 3;
                let (k, m) = (&rest[..third], &rest[third..2 * third]);
                let mac = daybook::util::hmac_sha256(k, m);
                let hex: String = mac.iter().map(|b| format!("{:02x}", b)).collect();
                assert_eq!(hex.len(), 64);
                assert!(daybook::util::ct_eq(&hex, &hex));
                let mut bad = hex.clone().into_bytes();
                bad[0] ^= 1;
                let bad = String::from_utf8(bad).expect("hex mutation kept utf8");
                assert!(!daybook::util::ct_eq(&hex, &bad));
                // determinism on recompute
                let again = daybook::util::hmac_sha256(k, m);
                assert_eq!(mac, again);
            }
        }
        // extract_invoice_items: arbitrary bytes parsed as JSON must never
        // panic, in every wrapping shape.
        2 => {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(rest) {
                let _ = daybook::api::extract_invoice_items(&v);
                let _ = daybook::api::extract_invoice_items(&serde_json::json!({
                    "items": v.clone()
                }));
                let _ = daybook::api::extract_invoice_items(&serde_json::json!({
                    "result": { "items": v }
                }));
            }
        }
        // gen_token: any requested length yields exactly that many
        // alphanumeric characters.
        3 => {
            let n = rest.first().copied().unwrap_or(0) as usize;
            let t = daybook::util::gen_token(n);
            assert_eq!(t.len(), n);
            assert!(
                t.bytes().all(|b| b.is_ascii_alphanumeric()),
                "invalid charset: {t}"
            );
        }
        // ct_eq: reflexivity for arbitrary strings, and inequality detected
        // between a string and its arbitrary prefix.
        4 => {
            if let Ok(a) = std::str::from_utf8(rest) {
                let half = &rest[..rest.len() / 2];
                if let Ok(b) = std::str::from_utf8(half) {
                    assert!(daybook::util::ct_eq(a, a));
                    if a != b {
                        assert!(!daybook::util::ct_eq(a, b));
                    }
                }
            }
        }
        // channel_key: canonicalization is total. Leading '@' is stripped,
        // output is ASCII-lowercase, and a second pass is a no-op. An '@' in
        // the middle of a name is not a prefix marker and stays untouched.
        5 => {
            if let Ok(s) = std::str::from_utf8(rest) {
                let k = daybook::handlers::channel_access::channel_key(s);
                assert!(!k.starts_with('@'), "leading @ survived: {k:?}");
                assert_eq!(k, k.to_ascii_lowercase(), "not lowercase: {k:?}");
                assert_eq!(
                    daybook::handlers::channel_access::channel_key(&k),
                    k,
                    "not idempotent: {k:?}"
                );
            }
        }
        // Role parsing degrades to User for every spelling but the exact four
        // lowercase role strings, and only "owner" reaches the Owner tier.
        6 => {
            if let Ok(s) = std::str::from_utf8(rest) {
                use daybook::roles::Role;
                let r = Role::from_role_str(s);
                let expected = match s {
                    "owner" => Role::Owner,
                    "admin" => Role::Admin,
                    "seller" => Role::Seller,
                    "user" => Role::User,
                    _ => Role::User,
                };
                assert_eq!(r, expected, "role mismatch for {s:?}");
                assert_eq!(r.has_level(Role::Owner), s == "owner", "owner gate: {s:?}");
            }
        }
        // to_chat_id: the produced target never carries whitespace/control
        // characters, and a username form has exactly one leading '@'.
        7 => {
            if let Ok(s) = std::str::from_utf8(rest) {
                let target = daybook::util::to_chat_id(s).to_string();
                assert!(
                    !target.chars().any(|c| c.is_whitespace() || c.is_control()),
                    "invalid target chars: {target:?}"
                );
                if target.starts_with('@') {
                    assert!(!target.starts_with("@@"), "doubled prefix: {target:?}");
                }
            }
        }
        _ => unreachable!(),
    }
});
