use sha2::Digest;
use teloxide::types::{ChatId, Recipient};

pub fn to_chat_id(s: &str) -> Recipient {
    let s = s.trim();
    if let Ok(id) = s.parse::<i64>() {
        Recipient::Id(ChatId(id))
    } else {
        let name = s.trim_start_matches('@');
        Recipient::ChannelUsername(if s.starts_with('@') {
            s.to_string()
        } else {
            format!("@{}", name)
        })
    }
}

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key = key.to_vec();
    if key.len() > BLOCK {
        let h = sha2::Sha256::digest(&key);
        key = h.to_vec();
    }
    if key.len() < BLOCK {
        key.resize(BLOCK, 0);
    }
    let mut ipad = vec![0u8; BLOCK];
    let mut opad = vec![0u8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] = key[i] ^ 0x36;
        opad[i] = key[i] ^ 0x5c;
    }
    ipad.extend_from_slice(msg);
    let inner = sha2::Sha256::digest(&ipad);
    opad.extend_from_slice(&inner);
    let outer = sha2::Sha256::digest(&opad);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}
/// Word list for human-friendly recovery codes (crypto-wallet style). Kept
/// small and lowercase so codes are easy to write down and type. Each code
/// is 4 words joined by hyphens (~32 bits of entropy) which is ample for
/// one-time-use, bcrypt-hashed recovery codes.
const RECOVERY_WORDS: &[&str] = &[
    "able", "acid", "aged", "also", "area", "army", "away", "baby", "back", "ball", "band", "bank",
    "base", "bath", "bean", "bear", "beat", "been", "bell", "belt", "best", "bill", "bird", "bite",
    "blue", "boat", "body", "bone", "book", "boot", "born", "boss", "both", "bowl", "bulk", "burn",
    "bush", "busy", "cake", "call", "calm", "came", "camp", "card", "care", "cart", "case", "cash",
    "cast", "cell", "chat", "chip", "city", "club", "coal", "coat", "code", "cold", "come", "cook",
    "cool", "copy", "core", "cost", "crew", "crop", "dark", "data", "date", "dawn", "days", "dead",
    "deal", "dear", "debt", "deep", "desk", "dial", "diet", "disk", "does", "done", "door", "dose",
    "down", "draw", "drop", "drum", "dual", "duck", "duly", "dust", "duty", "each", "earn", "ease",
    "east", "easy", "edge", "else", "emit", "epic", "even", "ever", "evil", "exam", "exit", "face",
    "fact", "fail", "fair", "fall", "farm", "fast", "fate", "fear", "feed", "feel", "feet", "fell",
    "felt", "file", "fill", "film", "find", "fine", "fire", "firm", "fish", "five", "flag", "flat",
    "flow", "food", "foot", "ford", "form", "fort", "four", "free", "from", "fuel", "full", "fund",
    "gain", "game", "gate", "gave", "gear", "gene", "gift", "girl", "give", "glad", "glow", "goal",
    "goes", "gold", "gone", "good", "grab", "gray", "grew", "grid", "grip", "grow", "gulf", "hair",
    "half", "hall", "hand", "hang", "hard", "harm", "hate", "have", "head", "heal", "heap", "hear",
    "heat", "held", "help", "here", "hero", "high", "hill", "hire", "hold", "hole", "holy", "home",
    "hope", "host", "hour", "huge", "hunt", "hurt", "idea", "idle", "inch", "into", "iron", "item",
    "join", "jump", "just", "keep", "kept", "kick", "kill", "kind", "king", "knee", "knew", "know",
    "lack", "lady", "laid", "lake", "land", "lane", "last", "late", "lead", "leaf", "left", "lend",
    "less", "life", "lift", "like", "limb", "line", "link", "list", "live", "load", "loan", "lock",
    "logo", "long", "look", "loop", "lose", "loss", "lost", "loud", "love", "luck", "made", "mail",
    "main", "make", "male", "mall", "many", "mark", "mask", "mass", "mate", "math", "meal", "mean",
    "meat", "meet", "menu", "mere", "mile", "milk", "mill", "mind", "mine", "miss", "mode", "mood",
    "moon", "more", "most", "move", "much", "must", "name", "navy", "near", "neck", "need", "next",
];

/// Generate a human-friendly recovery code (4 random words joined by hyphens).
pub fn gen_recovery_code() -> String {
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf).expect("OS entropy unavailable");
    let mut out = String::new();
    for i in 0..4usize {
        let idx = u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]) as usize % RECOVERY_WORDS.len();
        if i > 0 {
            out.push('-');
        }
        out.push_str(RECOVERY_WORDS[idx]);
    }
    out
}

pub fn gen_token(len: usize) -> String {
    // CSPRNG-backed: tokens must be unguessable (sessions, API keys).
    // On OS entropy failure there is no safe fallback -- treat it as an
    // unrecoverable startup-class failure.
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).expect("OS entropy unavailable");
    let mut out = String::with_capacity(len);
    for b in buf {
        out.push(CHARS[(b as usize) % CHARS.len()] as char);
    }
    out
}

/// Constant-time string equality: XOR-folds over the bytes of both strings
/// with no early exit, so the comparison time does not leak where the first
/// mismatch occurs. Used for webhook signature checks. The padding byte for
/// the shorter input is 0x01 (not 0x00) so a NUL-containing string can never
/// compare equal to a shorter one; length itself is public, so the explicit
/// length check is not a timing leak.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let n = a.len().max(b.len());
    let mut diff: u8 = 0;
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(1);
        let y = b.get(i).copied().unwrap_or(1);
        diff |= x ^ y;
    }
    if a.len() != b.len() {
        diff |= 1;
    }
    diff == 0
}

/// Base32 (RFC 4648) encode, no spaces, with `=` padding.
pub fn base32_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut buffer: u64 = 0;
    let mut bits_left: usize = 0;
    for &byte in input {
        buffer = (buffer << 8) | byte as u64;
        bits_left += 8;
        while bits_left >= 5 {
            bits_left -= 5;
            let idx = ((buffer >> bits_left) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits_left > 0 {
        buffer <<= 5 - bits_left;
        let idx = (buffer & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    while !out.is_empty() && !out.len().is_multiple_of(8) {
        out.push('=');
    }
    out
}

/// Base32 (RFC 4648) decode. Case-insensitive; ignores whitespace and padding.
pub fn base32_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut bits: u64 = 0;
    let mut bits_left: usize = 0;
    let mut out: Vec<u8> = Vec::new();
    for c in input.chars() {
        if c == '=' || c.is_whitespace() {
            continue;
        }
        let val = match c.to_ascii_uppercase() {
            'A'..='Z' => c.to_ascii_uppercase() as u8 - b'A',
            '2'..='7' => c.to_ascii_uppercase() as u8 - b'2' + 26,
            _ => return Err(format!("invalid base32 character: {}", c)),
        };
        bits = (bits << 5) | val as u64;
        bits_left += 5;
        if bits_left >= 8 {
            bits_left -= 8;
            out.push(((bits >> bits_left) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// TOTP code generation using HMAC-SHA256 (RFC 6238 / RFC 4226 truncation).
pub fn totp_generate<Ts, Td, Tstep>(
    secret_base32: &str,
    timestamp: Ts,
    digits: Td,
    step: Tstep,
) -> String
where
    Ts: TryInto<u64>,
    Ts::Error: std::fmt::Debug,
    Td: TryInto<usize>,
    Td::Error: std::fmt::Debug,
    Tstep: TryInto<u64>,
    Tstep::Error: std::fmt::Debug,
{
    let ts: u64 = timestamp.try_into().unwrap_or(0);
    let digits: usize = digits.try_into().unwrap_or(6);
    let step: u64 = step.try_into().unwrap_or(30);
    let key = base32_decode(secret_base32).unwrap_or_default();
    let counter = ts.checked_div(step).unwrap_or(0);
    let mut msg = [0u8; 8];
    for i in 0..8usize {
        msg[7 - i] = ((counter >> (i * 8)) & 0xff) as u8;
    }
    let mac = hmac_sha256(&key, &msg);
    let offset = (mac[mac.len() - 1] & 0x0f) as usize;
    let mut code = ((mac[offset] as u32 & 0x7f) << 24)
        | ((mac[offset + 1] as u32) << 16)
        | ((mac[offset + 2] as u32) << 8)
        | (mac[offset + 3] as u32);
    let modulus = 10u32.pow(digits as u32);
    code %= modulus;
    format!("{:0width$}", code, width = digits)
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Any requested length yields exactly that many ASCII-alnum chars.
        #[test]
        fn token_shape(len in 0usize..256) {
            let t = gen_token(len);
            prop_assert_eq!(t.len(), len);
            prop_assert!(t.bytes().all(|b| b.is_ascii_alphanumeric()));
        }

        /// Two draws must differ (collision odds at 59^16+ are nil).
        #[test]
        fn token_two_draws_differ(len in 16usize..64) {
            prop_assert_ne!(gen_token(len), gen_token(len));
        }

        /// HMAC is deterministic and flips on any single input-bit change.
        #[test]
        fn hmac_deterministic_and_sensitive(
            key in proptest::collection::vec(any::<u8>(), 0..80),
            msg in proptest::collection::vec(any::<u8>(), 0..200),
            bit in 0usize..2048,
        ) {
            let a = hmac_sha256(&key, &msg);
            prop_assert_eq!(a, hmac_sha256(&key, &msg));
            let mut m2 = msg.clone();
            if m2.is_empty() {
                m2.push(1);
            } else {
                let byte = bit / 8 % m2.len();
                m2[byte] ^= 1 << (bit % 8);
            }
            prop_assert_ne!(a, hmac_sha256(&key, &m2));
            prop_assert_ne!(a, [0u8; 32]);
        }

        /// ct_eq: reflexivity always, and distinct strings never equal.
        #[test]
        fn ct_eq_matches_equality(a in ".*", b in ".*") {
            prop_assert!(ct_eq(&a, &a));
            if a != b {
                prop_assert!(!ct_eq(&a, &b));
            }
        }
    }

    /// RFC 4231 test case 1 for HMAC-SHA256 (official vector).
    #[test]
    fn hmac_rfc4231_case1() {
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            mac.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    /// RFC 4231 test case 2 (key = "Jefe", data = the what-do-ya-know line).
    #[test]
    fn hmac_rfc4231_case2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            mac.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gen_token() {
        let t = gen_token(48);
        assert_eq!(t.len(), 48);
        const CHARS: &str = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
        for c in t.chars() {
            assert!(CHARS.contains(c), "invalid char: {}", c);
        }
        // different lengths
        assert_eq!(gen_token(0).len(), 0);
        assert_eq!(gen_token(10).len(), 10);
    }

    #[test]
    fn test_gen_token_charset() {
        let t = gen_token(128);
        const CHARS: &str = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
        for c in t.chars() {
            assert!(CHARS.contains(c), "invalid char: {}", c);
        }
    }

    #[test]
    fn test_gen_token_unique() {
        let a = gen_token(32);
        let b = gen_token(32);
        assert_ne!(a, b);
    }

    #[test]
    fn test_to_chat_id() {
        use teloxide::types::{ChatId, Recipient};
        // numeric
        assert_eq!(to_chat_id("  12345 "), Recipient::Id(ChatId(12345)));
        // @username
        assert_eq!(
            to_chat_id("@mychannel"),
            Recipient::ChannelUsername("@mychannel".to_string())
        );
    }
}
