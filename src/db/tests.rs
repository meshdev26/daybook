use super::*;
use proptest::prelude::*;
use std::fs;
#[test]
fn db_basic() {
    let path = "/tmp/opencode/test_agent2.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    assert_eq!(db.seller_channel(1).as_deref(), Some("@chan"));
    db.add_plan(1, "P", "1", "USDT", 7, Some("@chan"));
    let plans = db.list_plans(Some(1));
    assert_eq!(plans.len(), 1);
    let pid = db.get_plan(1, "P").unwrap().0;
    let code = db.create_code(1, Some(pid), Some(7), 1, None, 12);
    assert_eq!(code.len(), 12);
    let (_id, sid, p, _vd, _mu, uses, _exp) = db.get_code(&code).unwrap();
    assert_eq!(sid, 1);
    assert_eq!(p, Some(pid));
    assert_eq!(uses, 0);
    let res = db.redeem_code(&code, 42);
    assert!(res.is_ok());
    let code2 = db.get_code(&code).unwrap();
    assert_eq!(code2.5, 1);
}

/// `known_channel` (identity allowlist): exact-name lookup over the
/// channels table, case-insensitive per the column collation, returning
/// the canonical stored name; `None` for unknown or empty names.
#[test]
fn known_channel_scopes_to_configured_channels() {
    let path = "/tmp/opencode/test_known_channel.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    assert_eq!(db.known_channel("@chan"), None);
    db.add_channel("@Chan", 0).unwrap();
    assert_eq!(db.known_channel("@Chan").as_deref(), Some("@Chan"));
    // NOCASE: the canonical (stored) name comes back for any casing.
    assert_eq!(db.known_channel("@chan").as_deref(), Some("@Chan"));
    assert_eq!(db.known_channel("@CHAN").as_deref(), Some("@Chan"));
    // Unknown and empty names never resolve.
    assert_eq!(db.known_channel("@other"), None);
    assert_eq!(db.known_channel(""), None);
    // Removed channels stop being known.
    db.remove_channel("@Chan", 0).unwrap();
    assert_eq!(db.known_channel("@Chan"), None);
    let _ = fs::remove_file(path);
}

/// Phase 2: the per-channel sender binding setter round-trips, matches
/// case-insensitively, and refuses values outside the 'bot' | 'user'
/// contract (and unknown channels).
#[test]
fn channel_sender_identity_setter_round_trips() {
    let path = "/tmp/opencode/test_sender_identity_setter.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    assert_eq!(db.get_channel_sender_identity("@chan"), None);
    db.add_channel("@Chan", 0).unwrap();
    // Fresh rows read as the fail-safe default 'bot'.
    assert_eq!(
        db.get_channel_sender_identity("@chan").as_deref(),
        Some("bot")
    );
    assert!(db.set_channel_sender_identity("@chan", "user"));
    // NOCASE on write and read back, like the getter.
    assert_eq!(
        db.get_channel_sender_identity("@CHAN").as_deref(),
        Some("user")
    );
    assert!(db.set_channel_sender_identity("@CHAN", "bot"));
    assert_eq!(
        db.get_channel_sender_identity("@chan").as_deref(),
        Some("bot")
    );
    // Invalid identity values never land; the prior binding survives.
    for bad in ["admin", "", "USER", "user "] {
        assert!(!db.set_channel_sender_identity("@chan", bad), "{bad}");
    }
    assert_eq!(
        db.get_channel_sender_identity("@chan").as_deref(),
        Some("bot")
    );
    // Unknown channel: nothing affected.
    assert!(!db.set_channel_sender_identity("@missing", "user"));
    let _ = fs::remove_file(path);
}

/// Phase 2: the posts.sender audit column — migrated in with DEFAULT
/// 'bot' (legacy-style rows read back as bot), written explicitly by
/// log_post, and returned by list_posts at field position 7.
#[test]
fn posts_carry_sender_and_default_to_bot() {
    let path = "/tmp/opencode/test_posts_sender.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.log_post(0, "@chan", 1, "text", Some("by user"), "user")
        .unwrap();
    db.log_post(0, "@chan", 2, "text", Some("by bot"), "bot")
        .unwrap();
    // Legacy-style row, written without the sender column (a pre-Phase-2
    // writer against the migrated table): the DEFAULT applies.
    {
        let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
                "INSERT INTO posts(seller_id,chat_id,message_id,kind,content,created_at) VALUES(0,'@chan',3,'text','legacy','2026-01-01T00:00:00+00:00')",
                [],
            )
            .unwrap();
    }
    let rows = db.list_posts(None, 10);
    assert_eq!(rows.len(), 3);
    // list_posts orders by id DESC: the legacy row was inserted last, so
    // it comes first; find rows by message_id for a robust read of the
    // sender column (field position 7).
    let sender_of = |mid: i32| {
        rows.iter()
            .find(|r| r.3 == mid)
            .map(|r| r.7.clone())
            .unwrap_or_default()
    };
    assert_eq!(sender_of(1), "user");
    assert_eq!(sender_of(2), "bot");
    assert_eq!(
        sender_of(3),
        "bot",
        "migration DEFAULT must fill legacy rows"
    );
    // The scoped (per-seller) read carries the column too.
    db.log_post(7, "@chan", 4, "text", Some("seller user"), "user")
        .unwrap();
    let scoped = db.list_posts(Some(7), 10);
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].7, "user");
    let _ = fs::remove_file(path);
}

/// H1 ownership lookup: get_post_seller resolves the seller_id written by
/// log_post for a given row, and returns None for an unknown id so the
/// console mutation handlers deny by default.
#[test]
fn get_post_seller_round_trips_and_is_none_for_unknown_id() {
    let path = "/tmp/opencode/test_post_seller.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.log_post(42, "@chan", 9, "text", Some("hello"), "bot")
        .unwrap();
    // log_post returns (), so read the id back the way the console does.
    let id = db.list_posts(Some(42), 1)[0].0;
    assert_eq!(db.get_post_seller(id), Some(42));
    // A newer row from another seller must not shift the answer.
    db.log_post(7, "@chan", 10, "text", Some("other"), "user")
        .unwrap();
    assert_eq!(db.get_post_seller(id), Some(42));
    // Unknown id: None (callers deny by default).
    assert_eq!(db.get_post_seller(999_999), None);
    let _ = fs::remove_file(path);
}

// ---- v1.4 bookings / setup links / MCP tokens / per-asset revenue ----

fn booking_db(tag: &str) -> (Db, String) {
    let path = format!("/tmp/opencode/test_bookings_{tag}.db");
    let _ = fs::remove_file(&path);
    let db = Db::new(&path).unwrap();
    (db, path)
}

fn service_with_slot(db: &Db, seller: i64, name: &str, starts_at: &str) -> (i64, i64) {
    let svc = db
        .create_service(seller, name, "10", "USDT", 60, None)
        .expect("service created");
    let slot = db.create_slot(svc, seller, starts_at, 1);
    (svc, slot)
}

fn rfc_in_minutes(minutes: i64) -> String {
    (Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339()
}

/// Point a booking and its slot's hold stamp into the past — exactly the
/// values claim time wrote together — so the next expire_held_bookings()
/// pass transitions the booking and frees the slot like real clock time
/// would (tests must not wait out BOOKING_HOLD_MINUTES).
fn force_expiry(path: &str, booking_id: i64) {
    let past = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "UPDATE bookings SET expires_at=?1 WHERE id=?2",
        params![past, booking_id],
    )
    .unwrap();
    conn.execute(
            "UPDATE slots SET held_until=(SELECT expires_at FROM bookings WHERE id=?1) WHERE id=(SELECT slot_id FROM bookings WHERE id=?1)",
            params![booking_id],
        )
        .unwrap();
}

#[test]
fn test_booking_claim_is_atomic() {
    let (db, _path) = booking_db("claim");
    let (svc, slot) = service_with_slot(&db, 11, "consult", &rfc_in_minutes(120));
    let mut won = Vec::new();
    for u in 1..=8i64 {
        if let Ok(Some(id)) = db.create_booking(svc, slot, 100 + u) {
            won.push(id);
        }
    }
    assert_eq!(won.len(), 1, "exactly one buyer wins the slot");

    let slots = db.list_slots(svc);
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].4, "held");
    assert_eq!(db.list_bookings(Some(11)).len(), 1);
}

#[test]
fn test_booking_claim_rejects_missing_past_and_nonsense() {
    let (db, _path) = booking_db("reject");
    let (svc, slot) = service_with_slot(&db, 11, "consult", &rfc_in_minutes(60));
    // unknown slot id
    assert_eq!(db.create_booking(svc, 999_999, 1).unwrap(), None);
    // past slot
    let (svc_past, slot_past) = service_with_slot(&db, 11, "past", &rfc_in_minutes(-60));
    assert_eq!(db.create_booking(svc_past, slot_past, 1).unwrap(), None);
    // unparseable stored time — the db refuses nonsense
    let (svc_bad, slot_bad) = service_with_slot(&db, 11, "bad", "not-a-timestamp");
    assert_eq!(
        db.create_booking(svc_bad, slot_bad, 1).unwrap_err(),
        "slot time is invalid"
    );
    // service/slot pair mismatch: the slot belongs to svc, not svc_past
    assert_eq!(db.create_booking(svc_past, slot, 1).unwrap(), None);
    // nothing mutated by any rejected claim
    assert_eq!(db.list_bookings(Some(11)).len(), 0);
    assert_eq!(db.list_slots(svc)[0].4, "available");
}

#[test]
fn test_booking_expires_at_is_min_of_hold_and_start() {
    let (db, _path) = booking_db("expiry_window");
    // slot sooner than the hold window: expires_at == slot start (D3)
    let (svc, slot) = service_with_slot(&db, 11, "soon", &rfc_in_minutes(10));
    assert!(db.create_booking(svc, slot, 1).unwrap().is_some());
    let row = &db.list_bookings_for_user(1, 10)[0];
    let exp = DateTime::parse_from_rfc3339(row.7.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    let start = DateTime::parse_from_rfc3339(&rfc_in_minutes(10))
        .unwrap()
        .with_timezone(&Utc);
    assert!((exp - start).num_seconds().abs() < 5);
    // slot later than the hold window: expires_at ≈ now + BOOKING_HOLD_MINUTES
    let (svc2, slot2) = service_with_slot(&db, 11, "far", &rfc_in_minutes(240));
    assert!(db.create_booking(svc2, slot2, 2).unwrap().is_some());
    let row2 = &db.list_bookings_for_user(2, 10)[0];
    let exp2 = DateTime::parse_from_rfc3339(row2.7.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    let expected = Utc::now() + chrono::Duration::minutes(BOOKING_HOLD_MINUTES);
    assert!((exp2 - expected).num_seconds().abs() < 10);
}

#[test]
fn test_list_bookings_filtered_scope_status_limit() {
    let (db, _path) = booking_db("filtered");
    let (svc1, slot1) = service_with_slot(&db, 11, "alpha", &rfc_in_minutes(60));
    let (svc2, slot2) = service_with_slot(&db, 22, "beta", &rfc_in_minutes(120));
    let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
    let b2 = db.create_booking(svc2, slot2, 200).unwrap().unwrap();
    assert!(db.set_booking_status(b2, Some(22), "confirmed"));

    // scope: seller 11 sees only their own rows
    let s1 = db.list_bookings_filtered(Some(11), None, 100);
    assert_eq!(s1.len(), 1);
    assert_eq!(s1[0].0, b1);
    assert_eq!(s1[0].1.len(), 8, "public booking token present");
    assert_eq!(s1[0].2, 100);
    assert_eq!(s1[0].3, "alpha");
    assert!(s1[0].4.is_some(), "slot starts_at joined");
    assert_eq!(s1[0].6, "held");
    assert!(s1[0].7.is_some(), "expires_at joined");
    assert_eq!(s1[0].8, Some(11));

    // status filter at SQL level
    let held = db.list_bookings_filtered(None, Some("held"), 100);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].0, b1);
    let confirmed = db.list_bookings_filtered(Some(22), Some("confirmed"), 100);
    assert_eq!(confirmed.len(), 1);
    assert_eq!(confirmed[0].0, b2);
    assert_eq!(
        db.list_bookings_filtered(None, Some("cancelled"), 100)
            .len(),
        0
    );

    // limit caps the rows; ordering is slot start DESC (beta starts later)
    assert_eq!(db.list_bookings_filtered(None, None, 1).len(), 1);
    let all = db.list_bookings_filtered(None, None, 100);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].0, b2);
    assert_eq!(all[1].0, b1);
}

#[test]
fn test_list_bookings_for_user() {
    let (db, _path) = booking_db("for_user");
    let (svc1, slot1) = service_with_slot(&db, 11, "alpha", &rfc_in_minutes(60));
    let (svc2, slot2) = service_with_slot(&db, 22, "beta", &rfc_in_minutes(90));
    let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
    let b2 = db.create_booking(svc2, slot2, 100).unwrap().unwrap();
    let rows = db.list_bookings_for_user(100, 10);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, b2, "newest first");
    assert_eq!(rows[1].0, b1);
    assert!(rows.iter().all(|r| r.2 == 100));
    assert_eq!(db.list_bookings_for_user(999, 10).len(), 0);
    assert_eq!(db.list_bookings_for_user(100, 1).len(), 1);
}

#[test]
fn test_cancel_booking_scoped_rules() {
    let (db, path) = booking_db("cancel");
    // held + future slot: the owning seller cancels, slot freed
    let (svc1, slot1) = service_with_slot(&db, 11, "a", &rfc_in_minutes(60));
    let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
    assert!(
        !db.cancel_booking_scoped(b1, Some(99)),
        "wrong seller scope"
    );
    assert!(db.cancel_booking_scoped(b1, Some(11)));
    assert_eq!(db.list_slots(svc1)[0].4, "available");
    assert_eq!(db.list_bookings(Some(11))[0].4, "cancelled");
    assert!(!db.cancel_booking_scoped(b1, Some(11)), "terminal now");

    // admin (None) may cancel another seller's booking
    let (svc2, slot2) = service_with_slot(&db, 22, "b", &rfc_in_minutes(60));
    let b2 = db.create_booking(svc2, slot2, 200).unwrap().unwrap();
    assert!(db.cancel_booking_scoped(b2, None));
    assert_eq!(db.list_slots(svc2)[0].4, "available");

    // confirmed booking on a future slot: cancel frees the 'booked' slot
    let (svc3, slot3) = service_with_slot(&db, 33, "c", &rfc_in_minutes(60));
    let b3 = db.create_booking(svc3, slot3, 300).unwrap().unwrap();
    assert!(db.set_booking_status(b3, Some(33), "confirmed"));
    assert_eq!(db.list_slots(svc3)[0].4, "booked");
    assert!(db.cancel_booking_scoped(b3, Some(33)));
    assert_eq!(db.list_slots(svc3)[0].4, "available");

    // confirmed booking whose slot already started: cancelled, but the
    // slot is NOT freed back to available
    let (svc4, slot4) = service_with_slot(&db, 44, "d", &rfc_in_minutes(60));
    let b4 = db.create_booking(svc4, slot4, 400).unwrap().unwrap();
    assert!(db.set_booking_status(b4, Some(44), "confirmed"));
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE slots SET starts_at=?1 WHERE id=?2",
            params![rfc_in_minutes(-30), slot4],
        )
        .unwrap();
    }
    assert!(db.cancel_booking_scoped(b4, Some(44)));
    assert_eq!(db.list_slots(svc4)[0].4, "booked", "started slot stays");
}

#[test]
fn test_set_booking_status_confirm_decline_and_resurrection() {
    let (db, path) = booking_db("status");
    // held → confirmed books the future slot
    let (svc1, slot1) = service_with_slot(&db, 11, "a", &rfc_in_minutes(60));
    let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
    assert!(
        !db.set_booking_status(b1, Some(99), "confirmed"),
        "wrong seller"
    );
    assert!(!db.set_booking_status(b1, Some(11), "pending"), "allowlist");
    assert!(db.set_booking_status(b1, Some(11), "confirmed"));
    assert_eq!(db.list_slots(svc1)[0].4, "booked");
    assert_eq!(db.list_bookings(Some(11))[0].4, "confirmed");
    assert!(!db.set_booking_status(b1, Some(11), "declined"), "terminal");
    assert!(
        !db.set_booking_status(b1, Some(11), "confirmed"),
        "terminal"
    );

    // confirm refuses a past slot; decline still frees it
    let (svc2, slot2) = service_with_slot(&db, 22, "b", &rfc_in_minutes(60));
    let b2 = db.create_booking(svc2, slot2, 200).unwrap().unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE slots SET starts_at=?1 WHERE id=?2",
            params![rfc_in_minutes(-5), slot2],
        )
        .unwrap();
    }
    assert!(!db.set_booking_status(b2, Some(22), "confirmed"));
    assert_eq!(db.list_bookings(Some(22))[0].4, "held", "unchanged");
    assert_eq!(db.list_slots(svc2)[0].4, "held", "slot untouched");
    assert!(db.set_booking_status(b2, Some(22), "declined"));
    assert_eq!(db.list_bookings(Some(22))[0].4, "declined");
    assert_eq!(db.list_slots(svc2)[0].4, "available");

    // D3 resurrection: expired hold whose slot was freed → confirm re-books
    let (svc3, slot3) = service_with_slot(&db, 33, "c", &rfc_in_minutes(120));
    let b3 = db.create_booking(svc3, slot3, 300).unwrap().unwrap();
    force_expiry(&path, b3);
    assert_eq!(db.expire_held_bookings().len(), 1);
    assert_eq!(db.list_slots(svc3)[0].4, "available");
    assert!(db.set_booking_status(b3, Some(33), "confirmed"));
    assert_eq!(db.list_slots(svc3)[0].4, "booked");
    assert_eq!(db.list_bookings(Some(33))[0].4, "confirmed");

    // resurrection loses when another booking re-claimed the slot
    let (svc4, slot4) = service_with_slot(&db, 44, "d", &rfc_in_minutes(120));
    let b4 = db.create_booking(svc4, slot4, 400).unwrap().unwrap();
    force_expiry(&path, b4);
    db.expire_held_bookings();
    assert!(db.create_booking(svc4, slot4, 401).unwrap().is_some());
    assert!(
        !db.set_booking_status(b4, Some(44), "confirmed"),
        "slot taken"
    );
    let rows = db.list_bookings_filtered(Some(44), None, 100);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().find(|r| r.2 == 400).unwrap().6,
        "expired",
        "loser untouched"
    );
    assert_eq!(
        rows.iter().find(|r| r.2 == 401).unwrap().6,
        "held",
        "winner intact"
    );
}

#[test]
fn test_expire_held_bookings_returns_each_row_once() {
    let (db, path) = booking_db("sweep");
    let (svc1, slot1) = service_with_slot(&db, 11, "a", &rfc_in_minutes(120));
    let (svc2, slot2) = service_with_slot(&db, 22, "b", &rfc_in_minutes(240));
    let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
    let _b2 = db.create_booking(svc2, slot2, 200).unwrap().unwrap();
    force_expiry(&path, b1);
    let rows = db.expire_held_bookings();
    // (booking id, buyer, service name, seller id) — the seller id is the
    // bot-attribution field the sweeper needs to DM from the right bot.
    assert_eq!(rows, vec![(b1, 100, "a".to_string(), 11)]);
    // exactly-once: the second pass returns nothing
    assert!(db.expire_held_bookings().is_empty());
    // the untouched booking keeps its hold; b1's slot was freed
    assert_eq!(db.list_bookings(Some(22))[0].4, "held");
    assert_eq!(db.list_slots(svc2)[0].4, "held");
    assert_eq!(db.list_slots(svc1)[0].4, "available");
}

#[test]
fn test_setup_link_single_use_purpose_and_ttl() {
    let (db, path) = booking_db("setup");
    let raw = db.create_setup_link(42);
    assert_eq!(raw.len(), 48);
    // single use: exactly one consume wins
    assert_eq!(db.consume_pending_token(&raw, SETUP_LINK_PURPOSE), Some(42));
    assert_eq!(db.consume_pending_token(&raw, SETUP_LINK_PURPOSE), None);
    assert_eq!(db.consume_pending_token("nope", SETUP_LINK_PURPOSE), None);
    // purpose separation: an MFA verify token is not a setup link
    let verify_raw = crate::util::gen_token(48);
    let expires = (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    db.create_pending_mfa(
        sha256_hex(&verify_raw),
        42,
        "verify",
        None::<String>,
        expires,
    )
    .unwrap();
    assert_eq!(
        db.consume_pending_token(&verify_raw, SETUP_LINK_PURPOSE),
        None
    );
    assert_eq!(db.consume_pending_token(&verify_raw, "verify"), Some(42));
    // TTL: an expired setup link dies with the same generic None
    let raw2 = db.create_setup_link(43);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE web_pending_mfa SET expires_at=?1 WHERE account_id=43",
            params![(Utc::now() - chrono::Duration::minutes(1)).to_rfc3339()],
        )
        .unwrap();
    }
    assert_eq!(db.consume_pending_token(&raw2, SETUP_LINK_PURPOSE), None);
}

#[test]
fn test_mcp_token_lifecycle_and_masking() {
    let (db, path) = booking_db("mcp");
    let (id, raw) = db.create_mcp_token(7, Some("ci"));
    assert_eq!(raw.len(), 48);
    // the hash-lookup path mcp::lookup_mcp_token already consumes
    let row = db
        .get_mcp_token_by_hash(&sha256_hex(&raw))
        .expect("hash finds the token");
    assert_eq!(row.0, id);
    assert_eq!(row.1, 7);
    assert_eq!(row.2.as_deref(), Some("ci"));
    assert_eq!(row.3, 0, "not revoked");

    // masked list: prefix + name only — never the raw token or the hash
    let list = db.list_mcp_tokens(7);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, id);
    assert_eq!(list[0].1.as_deref(), Some("ci"));
    let prefix = list[0].2.as_deref().expect("prefix stored");
    assert_eq!(prefix.len(), 8);
    assert!(raw.starts_with(prefix), "prefix is a prefix, not the token");
    assert_eq!(list[0].4, 0);
    assert!(
        db.list_mcp_tokens(8).is_empty(),
        "other sellers see nothing"
    );

    // scoped revoke: wrong seller loses, owner and admin win, idempotent
    assert!(!db.revoke_mcp_token(id, Some(8)));
    assert!(db.revoke_mcp_token(id, Some(7)));
    assert_eq!(db.get_mcp_token_by_hash(&sha256_hex(&raw)).unwrap().3, 1);
    assert!(db.revoke_mcp_token(id, Some(7)), "idempotent");
    assert!(!db.revoke_mcp_token(999_999, None), "unknown id");
    let (id2, raw2) = db.create_mcp_token(7, None);
    assert!(
        db.revoke_mcp_token(id2, None),
        "admin scope acts on any row"
    );
    assert_eq!(db.get_mcp_token_by_hash(&sha256_hex(&raw2)).unwrap().3, 1);

    // touch stamps last_used_at (best-effort bookkeeping)
    db.touch_mcp_token(id);
    {
        let conn = Connection::open(&path).unwrap();
        let used: Option<String> = conn
            .query_row(
                "SELECT last_used_at FROM mcp_tokens WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(used.is_some());
    }
}

#[test]
fn test_mcp_audit_carries_seller_id() {
    let (db, path) = booking_db("audit");
    db.mcp_audit("list_plans", "seller", Some(9), true, None);
    db.mcp_audit("owner_tool", "owner", None, false, Some("boom"));
    let conn = Connection::open(&path).unwrap();
    let mut stmt = conn
        .prepare("SELECT role, seller_id, ok FROM mcp_audit ORDER BY id")
        .unwrap();
    let rows: Vec<(String, Option<i64>, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], ("seller".to_string(), Some(9), 1));
    assert_eq!(rows[1], ("owner".to_string(), None, 0));
}

#[test]
fn test_revenue_by_asset_per_asset_lines() {
    let (db, _path) = booking_db("revasset");
    db.save_invoice("i1", 1, "50", "USDT", None, "paid", None, None, None, None);
    db.save_invoice("i2", 1, "20", "USDT", None, "paid", None, None, None, None);
    db.save_invoice("i3", 1, "0.5", "BTC", None, "paid", None, None, None, None);
    // unpaid invoices stay out of the per-asset lines
    db.save_invoice(
        "i4", 1, "100", "USDT", None, "active", None, None, None, None,
    );
    let rows = db.revenue_by_asset();
    assert_eq!(
        rows,
        vec![("BTC".to_string(), 0.5), ("USDT".to_string(), 70.0)]
    );
    // A cross-asset sum (70.5) is never returned: mixed assets produce an
    // error so callers must render the per-asset lines above instead.
    assert_eq!(db.revenue_total(None), Err(MixedAssets));
}

#[test]
fn mixed_asset_totals_are_not_single_floats() {
    let (db, _path) = booking_db("mixedtotals");
    db.save_invoice("b1", 1, "0.5", "BTC", None, "paid", None, None, None, None);
    db.save_invoice("u1", 1, "50", "USDT", None, "paid", None, None, None, None);
    // All three entry points refuse to produce a cross-asset number.
    assert_eq!(db.revenue_total(None), Err(MixedAssets));
    assert_eq!(db.revenue_total(Some(1)), Err(MixedAssets));
    assert_eq!(db.analytics_revenue_total(1), Err(MixedAssets));
    assert_eq!(db.analytics_aov(1), Err(MixedAssets));
    // The per-asset sources still carry the correct denominated amounts.
    assert_eq!(
        db.revenue_by_asset(),
        vec![("BTC".to_string(), 0.5), ("USDT".to_string(), 50.0)]
    );
    assert_eq!(
        db.analytics_revenue_by_asset(1),
        vec![("BTC".to_string(), 0.5), ("USDT".to_string(), 50.0)]
    );
    // Single-asset scopes keep their exact total.
    db.save_invoice("u2", 2, "7", "USDT", None, "paid", None, None, None, None);
    assert_eq!(db.revenue_total(Some(2)), Ok(7.0));
}

#[test]
fn secret_config_encrypts_at_rest_and_reads_back() {
    let (db, path) = booking_db("secretcfg");
    let mk = crate::config::parse_master_key(&"ab".repeat(32)).unwrap();
    db.set_secret_config_with(Some(&mk), "crypto_token", "token-123")
        .unwrap();
    // Raw SQLite value is an encrypted envelope, never the token.
    let conn = rusqlite::Connection::open(&path).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT value FROM config WHERE key='crypto_token'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(raw.starts_with(crate::secrets::SECRET_PREFIX));
    assert!(!raw.contains("token-123"));
    assert_eq!(
        db.get_secret_config_with(&mk, "crypto_token").unwrap(),
        Some("token-123".to_string())
    );
    // Legacy readers of the same key transparently see plaintext.
    assert_eq!(
        db.get_config_with_key("crypto_token", Some(&mk)).as_deref(),
        Some("token-123")
    );
    // Non-secret keys keep their verbatim behavior.
    db.set_config("channel", "@plain");
    assert_eq!(db.get_config("channel").as_deref(), Some("@plain"));
}

#[test]
fn secret_config_without_master_key_refuses_write() {
    let (db, _) = booking_db("nokey");
    // No key resolved: the setter refuses before touching the database.
    assert_eq!(
        db.set_secret_config_with(None, "crypto_token", "token-789"),
        Err(crate::secrets::SecretError::MissingMasterKey)
    );
    assert_eq!(db.get_secret_config("crypto_token").unwrap(), None);
    // The public entry point uses the same rule when MASTER_KEY is unset.
    if std::env::var_os("MASTER_KEY").is_none() {
        assert_eq!(
            db.try_set_secret_config("crypto_token", "token-789"),
            Err(crate::secrets::SecretError::MissingMasterKey)
        );
        assert_eq!(db.get_secret_config("crypto_token").unwrap(), None);
    }
    // Sensitive-key allowlist is enforced as well.
    assert_eq!(
        db.set_secret_config_with(None, "channel", "@x"),
        Err(crate::secrets::SecretError::NotSensitiveKey)
    );
}

#[test]
fn secret_config_tamper_and_legacy_values() {
    let (db, path) = booking_db("secrettamper");
    let mk = crate::config::parse_master_key(&"cd".repeat(32)).unwrap();
    db.set_secret_config_with(Some(&mk), "crypto_token", "token-456")
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT value FROM config WHERE key='crypto_token'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut chars: Vec<char> = raw.chars().collect();
    let i = chars.len() - 1;
    chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
    let tampered: String = chars.into_iter().collect();
    conn.execute(
        "UPDATE config SET value=?1 WHERE key='crypto_token'",
        rusqlite::params![tampered],
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        db.get_secret_config_with(&mk, "crypto_token"),
        Err(crate::secrets::SecretError::Decrypt)
    );
    // A legacy plaintext value is still readable without any key.
    db.set_config("crypto_token", "legacy-token");
    assert_eq!(
        db.get_secret_config("crypto_token").unwrap().as_deref(),
        Some("legacy-token")
    );
}

#[tokio::test]
async fn test_web_account_create_verify() {
    let path = "/tmp/opencode/test_web_account_create_verify.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.create_web_account("alice", "secret", "admin", None)
        .await
        .unwrap();
    // correct password
    let r = db.verify_web_password("alice", "secret").await;
    assert!(r.is_some());
    let (id, role, seller) = r.unwrap();
    assert!(id > 0);
    assert_eq!(role, "admin");
    assert!(seller.is_none());
    // wrong password
    assert!(db.verify_web_password("alice", "wrong").await.is_none());
    // duplicate username errors
    let dup = db.create_web_account("alice", "x", "user", None).await;
    assert!(dup.is_err());
}

#[tokio::test]
async fn test_web_account_update_password() {
    let path = "/tmp/opencode/test_web_account_update_password.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.create_web_account("bob", "oldpw", "user", None)
        .await
        .unwrap();
    assert!(db.verify_web_password("bob", "oldpw").await.is_some());
    db.update_web_password("bob", "newpw").await.unwrap();
    // old fails
    assert!(db.verify_web_password("bob", "oldpw").await.is_none());
    // new succeeds
    assert!(db.verify_web_password("bob", "newpw").await.is_some());
}

/// The creation boundary: a seller-tier account with no seller binding is the
/// fail-open state (None was read as "unscoped / every tenant"), so minting one
/// must be impossible. Every other (role, binding) pair stays valid.
#[tokio::test]
async fn create_web_account_refuses_scopeless_seller() {
    let path = "/tmp/opencode/test_create_scopeless_seller.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();

    let err = db
        .create_web_account("ghost-seller", "pw", "seller", None)
        .await
        .expect_err("a seller with no binding must be refused");
    assert!(err.contains("seller binding"), "{err}");
    assert!(
        db.get_web_account_by_username("ghost-seller").is_none(),
        "the refused account must not be persisted"
    );

    // A bound seller, an owner, an admin and a buyer are all still creatable.
    db.create_web_account("bound-seller", "pw", "seller", Some(42))
        .await
        .expect("a bound seller is legitimate");
    db.create_web_account("boss", "pw", "owner", None)
        .await
        .expect("the owner is platform-level");
    db.create_web_account("ops", "pw", "admin", None)
        .await
        .expect("an admin may be platform-level");
    db.create_web_account("shopper", "pw", "user", None)
        .await
        .expect("a buyer holds no tenant grant");

    // The legacy escape hatch is exactly what it says: it can still write the
    // refused state, which is how the old rows are modelled in tests.
    db.insert_legacy_web_account("legacy-ghost", "seller", None);
    assert!(db.get_web_account_by_username("legacy-ghost").is_some());
}

/// `0` is a real namespace (the operator's), not "no row". A seller bound to
/// it would inherit the platform view, and negative ids are never issued, so
/// neither binding may be minted. Refusal must leave no account row behind.
#[tokio::test]
async fn create_web_account_refuses_platform_namespace_seller() {
    let path = "/tmp/opencode/test_create_platform_namespace_seller.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();

    for (username, seller_id) in [("bad-zero", 0i64), ("bad-negative", -1i64)] {
        let err = db
            .create_web_account(username, "pw", "seller", Some(seller_id))
            .await
            .expect_err("a seller bound to the platform namespace must be refused");
        assert!(err.contains("seller binding"), "{err}");
        assert!(
            db.get_web_account_by_username(username).is_none(),
            "the refused account must not be persisted"
        );
    }

    // Real positive seller bindings and the non-seller tiers are unaffected.
    db.create_web_account("bound-seller", "pw", "seller", Some(42))
        .await
        .expect("a real positive seller binding is legitimate");
    db.create_web_account("boss", "pw", "owner", None)
        .await
        .expect("the owner is platform-level");
    db.create_web_account("ops", "pw", "admin", Some(0))
        .await
        .expect("an admin may use the platform namespace");
    db.create_web_account("shopper", "pw", "user", Some(0))
        .await
        .expect("a buyer holds no tenant grant");
}

#[tokio::test]
async fn test_web_session_lifecycle() {
    let path = "/tmp/opencode/test_web_session_lifecycle.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.create_web_account("carol", "pw", "seller", Some(42))
        .await
        .unwrap();
    let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("carol").unwrap();
    // create session (long-lived)
    let token = db.create_web_session(acc_id, "seller", Some(42), 7);
    assert!(!token.is_empty());
    // get returns account/role/seller
    let got = db.get_web_session(&token);
    assert!(got.is_some());
    let (a, role, seller, _selected) = got.unwrap();
    assert_eq!(a, acc_id);
    assert_eq!(role, "seller");
    assert_eq!(seller, Some(42));
    // delete removes
    db.delete_web_session(&token);
    assert!(db.get_web_session(&token).is_none());
    // expired: days=0
    let token2 = db.create_web_session(acc_id, "seller", Some(42), 0);
    assert!(db.get_web_session(&token2).is_none());
}

#[tokio::test]
async fn test_list_web_accounts() {
    let path = "/tmp/opencode/test_list_web_accounts.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.create_web_account("u1", "p1", "owner", None)
        .await
        .unwrap();
    db.create_web_account("u2", "p2", "admin", None)
        .await
        .unwrap();
    let list = db.list_web_accounts();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0], ("u1".to_string(), "owner".to_string()));
    assert_eq!(list[1], ("u2".to_string(), "admin".to_string()));
}

#[test]
fn test_seller_upsert_and_channel() {
    let path = "/tmp/opencode/test_seller_upsert_and_channel.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    let s = db.get_seller(1).unwrap();
    assert_eq!(s.0, 1);
    assert_eq!(s.1, Some("alice".to_string()));
    assert_eq!(s.2, Some("@chan".to_string()));
    assert_eq!(db.seller_channel(1).as_deref(), Some("@chan"));
    db.set_seller_channel(1, "@chan2");
    assert_eq!(db.seller_channel(1).as_deref(), Some("@chan2"));
    assert!(db.list_sellers().contains(&1));
}

#[test]
fn test_plan_crud_and_scoping() {
    let path = "/tmp/opencode/test_plan_crud_and_scoping.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.add_plan(1, "P", "1", "USDT", 7, Some("@chan"));
    let scoped = db.list_plans(Some(1));
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].0, "P");
    let global = db.list_plans(None);
    assert_eq!(global.len(), 1);
    let got = db.get_plan(1, "P").unwrap();
    assert_eq!(got.3, "P");
    db.remove_plan(1, "P");
    assert_eq!(db.list_plans(Some(1)).len(), 0);
}

#[test]
fn test_subscription_create_and_user_has_active() {
    let path = "/tmp/opencode/test_subscription_create_and_user_has_active.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    let id = db.create_subscription(1, 42, None, "@chan", 7, None, None);
    assert!(id > 0);
    assert!(db.user_has_active_sub(1, 42, "@chan"));
    assert!(!db.user_has_active_sub(1, 999, "@chan"));
    assert!(!db.user_has_active_sub(2, 42, "@chan"));
}

#[test]
fn test_activation_code_create_redeem() {
    let path = "/tmp/opencode/test_activation_code_create_redeem.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    let code = db.create_code(1, None, Some(7), 1, None, 12);
    assert_eq!(code.len(), 12);
    let got = db.get_code(&code).unwrap();
    assert_eq!(got.1, 1);
    assert_eq!(got.5, 0);
    let res = db.redeem_code(&code, 42);
    assert!(res.is_ok());
    let got2 = db.get_code(&code).unwrap();
    assert_eq!(got2.5, 1);
    // second redeem fails
    let res2 = db.redeem_code(&code, 42);
    assert!(res2.is_err());
}

/// Bot scoping for the console: `list_codes_for_sellers` returns exactly
/// the codes of the requested sellers (never the others), in the same
/// newest-first order and column shape as `list_codes`, honours the limit,
/// and short-circuits an empty slice (no `IN ()` SQL).
#[test]
fn test_list_codes_for_sellers_filters_and_limits() {
    let path = "/tmp/opencode/test_list_codes_for_sellers.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@a"));
    db.upsert_seller(2, Some("bob"), Some("@b"));
    db.upsert_seller(3, Some("carol"), Some("@c"));
    let c1 = db.create_code(1, None, Some(7), 1, None, 12);
    let c2 = db.create_code(1, None, Some(30), 5, None, 12);
    let c3 = db.create_code(2, None, Some(7), 2, None, 12);
    let _c4 = db.create_code(3, None, Some(7), 1, None, 12);

    // Empty scope never issues SQL and never widens to "all sellers".
    assert!(db.list_codes_for_sellers(&[], 10).is_empty());

    // One seller only, with the seller column (element 5) intact.
    let only_two = db.list_codes_for_sellers(&[2], 10);
    assert_eq!(only_two.len(), 1);
    assert_eq!(only_two[0].0, c3);
    assert_eq!(only_two[0].5, 2);

    // Two sellers: identical to the unscoped list filtered by seller.
    let expected: Vec<CodeListRow> = db
        .list_codes(None, 10)
        .into_iter()
        .filter(|r| r.5 == 1 || r.5 == 2)
        .collect();
    assert_eq!(expected.len(), 3);
    let both = db.list_codes_for_sellers(&[1, 2], 10);
    assert_eq!(both, expected, "same rows, same order as list_codes");
    // newest first (list_codes' ORDER BY id DESC); seller 3 never leaks in.
    let ordered: Vec<&str> = both.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(ordered, vec![c3.as_str(), c2.as_str(), c1.as_str()]);
    assert!(both.iter().all(|r| r.5 != 3));

    // Limit applies after the seller filter.
    let limited = db.list_codes_for_sellers(&[1, 2], 2);
    assert_eq!(limited, expected[..2].to_vec());
    assert_eq!(limited.len(), 2);
    let _ = fs::remove_file(path);
}

#[test]
fn test_invoice_save_and_pending() {
    let path = "/tmp/opencode/test_invoice_save_and_pending.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.save_invoice(
        "inv1",
        1,
        "10",
        "USDT",
        None,
        "active",
        None,
        Some("P"),
        Some(42),
        None,
    );
    let inv = db.get_invoice("inv1").unwrap();
    assert_eq!(inv.0, "10");
    assert_eq!(inv.1, "USDT");
    assert_eq!(inv.2, "active");
    let pending = db.pending_invoices();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, "inv1");
}

#[test]
fn test_invoice_claim_finalize_and_exists() {
    let path = "/tmp/opencode/test_invoice_claim_finalize_and_exists.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.save_invoice(
        "inv_claim",
        1,
        "10",
        "USDT",
        None,
        "active",
        None,
        Some("P"),
        Some(42),
        None,
    );
    // claim succeeds exactly once (atomic handoff between granters)
    assert!(db.claim_invoice_for_grant("inv_claim"));
    assert!(!db.claim_invoice_for_grant("inv_claim"));
    // a FRESH claim is never re-queued: the live claimer owns the row
    assert!(!db.pending_invoices().iter().any(|r| r.0 == "inv_claim"));
    // ... but a claim wedged past WEDGED_GRANT_STALE_MINUTES (claimer
    // crashed between claim and finalize) re-queues for re-verification,
    // which is safe because both grant branches are existence-guarded.
    {
        let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
        let stale = (Utc::now()
            - chrono::Duration::try_minutes(WEDGED_GRANT_STALE_MINUTES + 1).unwrap_or_default())
        .to_rfc3339();
        let _ = conn.execute(
            "UPDATE invoices SET updated_at=?1 WHERE invoice_id='inv_claim'",
            params![stale],
        );
    }
    assert!(db.pending_invoices().iter().any(|r| r.0 == "inv_claim"));
    // release_invoice_claim unwinds ONLY a 'granting' row, exactly once
    assert!(db.release_invoice_claim("inv_claim"));
    assert!(!db.release_invoice_claim("inv_claim"));
    assert_eq!(db.get_invoice("inv_claim").unwrap().2, "active");
    assert!(db.pending_invoices().iter().any(|r| r.0 == "inv_claim"));
    // re-claim, then the normal grant + finalize flow drops it for good
    assert!(db.claim_invoice_for_grant("inv_claim"));
    // no subscription yet
    assert!(!db.subscription_exists_for_invoice("inv_claim"));
    db.create_subscription(1, 42, None, "@chan", 7, Some("inv_claim"), None);
    assert!(db.subscription_exists_for_invoice("inv_claim"));
    // finalize flips to paid and drops it out of the pending queue
    db.finalize_invoice_paid("inv_claim");
    assert_eq!(db.get_invoice("inv_claim").unwrap().2, "paid");
    assert!(!db.pending_invoices().iter().any(|r| r.0 == "inv_claim"));
    // a paid invoice can no longer be claimed, nor released
    assert!(!db.claim_invoice_for_grant("inv_claim"));
    assert!(!db.release_invoice_claim("inv_claim"));
}

#[test]
fn test_finalize_invoice_paid_never_overwrites_refunded() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("race.db").to_str().unwrap()).unwrap();
    db.save_invoice(
        "inv-refund-race",
        1,
        "10",
        "USDT",
        None,
        "active",
        None,
        Some("P"),
        Some(42),
        None,
    );
    assert!(db.claim_invoice_for_grant("inv-refund-race")); // active -> granting
    db.update_invoice_status("inv-refund-race", "refunded"); // refund settles mid-grant
    db.finalize_invoice_paid("inv-refund-race"); // must be a no-op
    assert_eq!(db.get_invoice("inv-refund-race").unwrap().2, "refunded");
    assert_eq!(db.analytics_invoice_counts(1).1, 0); // no paid/revenue miscount
    assert!(!db.claim_invoice_for_grant("inv-refund-race")); // no new grant possible
    assert!(!db.subscription_exists_for_invoice("inv-refund-race"));
}

#[test]
fn test_seller_has_channel() {
    let path = "/tmp/opencode/test_seller_has_channel.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    // unknown seller: no channel
    assert!(!db.seller_has_channel(1));
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    assert!(db.seller_has_channel(1));
    // an empty-string channel reads as NO channel (matches the
    // seller_channel(..).unwrap_or_default().is_empty() callers do)
    db.upsert_seller(2, Some("bob"), Some(""));
    assert!(!db.seller_has_channel(2));
    assert!(db.seller_channel(2).is_some());
}

#[test]
fn test_pending_invoices_includes_products_only() {
    let path = "/tmp/opencode/test_pending_invoices_includes_products_only.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    // Product invoice (plan_id NULL, product_id set): must be queued.
    db.save_invoice(
        "inv_prod",
        1,
        "5",
        "USDT",
        None,
        "active",
        None,
        None,
        Some(9),
        Some(3),
    );
    // Plan invoice: queued as before.
    db.save_invoice(
        "inv_plan",
        1,
        "5",
        "USDT",
        None,
        "active",
        None,
        Some("gold"),
        Some(9),
        None,
    );
    // Neither plan nor product: nothing grantable, never queued.
    db.save_invoice(
        "inv_bare",
        1,
        "5",
        "USDT",
        None,
        "active",
        None,
        None,
        Some(9),
        None,
    );
    let pending = db.pending_invoices();
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().any(|r| r.0 == "inv_prod"));
    assert!(pending.iter().any(|r| r.0 == "inv_plan"));
    assert!(!pending.iter().any(|r| r.0 == "inv_bare"));
}

#[test]
fn test_product_add_get_list_and_collisions() {
    let path = "/tmp/opencode/test_product_add_get_list_and_collisions.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.upsert_seller(2, Some("bob"), Some("@chan"));
    // invalid kind rejected
    assert!(
        db.add_product(1, "bad", "5", "USDT", "service", None, None)
            .is_err()
    );
    let id = db
        .add_product(
            1,
            "ebook",
            "5",
            "USDT",
            "digital_link",
            Some("https://example.com/dl"),
            Some(10),
        )
        .unwrap();
    assert!(id > 0);
    let p = db.get_product(1, "ebook").unwrap();
    assert_eq!(p.0, id);
    assert_eq!(p.1, 1);
    assert_eq!(p.2, "ebook");
    assert_eq!(p.3, "5");
    assert_eq!(p.4, "USDT");
    assert_eq!(p.5, "digital_link");
    assert_eq!(p.6.as_deref(), Some("https://example.com/dl"));
    assert_eq!(p.7, Some(10));
    assert_eq!(p.8, 1);
    // same name, same seller: product-vs-product collision
    assert!(
        db.add_product(1, "ebook", "9", "TON", "digital_file", None, None)
            .is_err()
    );
    // same name, other seller: fine
    assert!(
        db.add_product(2, "ebook", "5", "USDT", "digital_link", None, None)
            .is_ok()
    );
    // product-vs-plan collision, scoped to the same seller only
    db.add_plan(1, "gold", "10", "USDT", 30, Some("@chan"));
    assert!(
        db.add_product(1, "gold", "5", "USDT", "digital_link", None, None)
            .is_err()
    );
    assert!(
        db.add_product(2, "gold", "5", "USDT", "digital_link", None, None)
            .is_ok()
    );
    // unscoped listing is the active-only shop view
    assert_eq!(db.list_products(None).len(), 3);
    db.set_product_active(1, "ebook", false);
    assert_eq!(db.list_products(None).len(), 2);
    assert!(db.list_products(None).iter().all(|r| r.8 == 1));
    // scoped listing is the seller's management view: retired rows stay
    assert_eq!(db.list_products(Some(1)).len(), 1);
    assert_eq!(db.list_products(Some(1))[0].8, 0);
    // scoped /buy name resolution (active only): the live 'ebook' belongs
    // to seller 2 (seller 1's copy is retired), the plan 'gold' to
    // seller 1 — each namespace resolves only its own rows.
    assert_eq!(db.find_products_by_name(Some(2), "ebook").len(), 1);
    assert_eq!(db.find_products_by_name(Some(2), "ebook")[0].1, 2);
    assert_eq!(db.find_products_by_name(Some(1), "ebook").len(), 0);
    assert_eq!(db.find_plans_by_name(Some(1), "gold").len(), 1);
    assert_eq!(db.find_plans_by_name(Some(1), "gold")[0].6, 1);
    // unscoped is the cross-seller shop view (the caller filters
    // namespaces: seller_id 0 is admin/owner-only, see commands.rs)
    assert_eq!(db.find_products_by_name(None, "ebook").len(), 1);
    // a retired product's name slot can be reused (upsert revives it)
    let id2 = db
        .add_product(1, "ebook", "7", "TON", "physical", None, Some(3))
        .unwrap();
    let p2 = db.get_product(1, "ebook").unwrap();
    assert_eq!(p2.0, id2);
    assert_eq!(p2.3, "7");
    assert_eq!(p2.8, 1);
    assert_eq!(db.list_products(None).len(), 3);
}

#[test]
fn test_product_grant_once_and_stock_exhaustion() {
    let path = "/tmp/opencode/test_product_grant_once_and_stock.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    let pid = db
        .add_product(
            1,
            "course",
            "5",
            "USDT",
            "digital_link",
            Some("link"),
            Some(2),
        )
        .unwrap();
    // first grant succeeds and decrements stock
    assert!(db.grant_product(1, 42, pid, "inv_a").unwrap());
    assert_eq!(db.get_product(1, "course").unwrap().7, Some(1));
    assert!(db.product_grant_exists("inv_a"));
    // same invoice again: idempotent no-op, stock untouched
    assert!(!db.grant_product(1, 42, pid, "inv_a").unwrap());
    assert_eq!(db.get_product(1, "course").unwrap().7, Some(1));
    // second unit sells, stock hits zero
    assert!(db.grant_product(1, 43, pid, "inv_b").unwrap());
    assert_eq!(db.get_product(1, "course").unwrap().7, Some(0));
    // exhausted: no grant row, no further decrement
    assert_eq!(
        db.grant_product(1, 44, pid, "inv_c").unwrap_err(),
        "out of stock"
    );
    assert!(!db.product_grant_exists("inv_c"));
    assert_eq!(db.get_product(1, "course").unwrap().7, Some(0));
    // NULL stock = unlimited, never exhausts
    let uid = db
        .add_product(
            1,
            "bonus",
            "1",
            "TON",
            "digital_file",
            Some("file_id"),
            None,
        )
        .unwrap();
    for i in 0..5 {
        assert!(
            db.grant_product(1, 50 + i, uid, &format!("inv_u{}", i))
                .unwrap()
        );
    }
    assert_eq!(db.get_product(1, "bonus").unwrap().7, None);
}

#[test]
fn test_order_create_list_status_and_scoping() {
    let path = "/tmp/opencode/test_order_create_list_status.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    let mug = db
        .add_product(1, "mug", "9", "USDT", "physical", None, None)
        .unwrap();
    let tee = db
        .add_product(2, "tee", "7", "USDT", "physical", None, None)
        .unwrap();
    // create -> 8-char order id, pending, no tracking
    let oid = db.create_order(1, mug, 42, "inv_o1").unwrap();
    assert_eq!(oid.len(), 8);
    db.create_order(2, tee, 43, "inv_o2").unwrap();
    // one order per purchase (UNIQUE invoice_id)
    assert!(db.create_order(1, mug, 42, "inv_o1").is_err());
    // list: all vs seller-scoped
    assert_eq!(db.list_orders(None).len(), 2);
    let own = db.list_orders(Some(1));
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].0, oid);
    assert_eq!(own[0].1, 1);
    assert_eq!(own[0].2, 42);
    assert_eq!(own[0].3, "pending");
    assert_eq!(own[0].4, None);
    assert!(!own[0].5.is_empty());
    // status transitions
    assert!(db.set_order_status(1, &oid, "shipped"));
    assert_eq!(db.list_orders(Some(1))[0].3, "shipped");
    assert!(db.set_order_status(1, &oid, "delivered"));
    assert_eq!(db.list_orders(Some(1))[0].3, "delivered");
    assert!(db.set_order_status(1, &oid, "refunded"));
    assert!(db.set_order_status(1, &oid, "cancelled"));
    assert!(db.set_order_status(1, &oid, "pending"));
    // invalid status rejected, state untouched
    assert!(!db.set_order_status(1, &oid, "destroyed"));
    assert!(!db.set_order_status(1, &oid, ""));
    assert_eq!(db.list_orders(Some(1))[0].3, "pending");
    // seller scoping: seller 2 cannot flip seller 1's order
    assert!(!db.set_order_status(2, &oid, "shipped"));
    assert_eq!(db.list_orders(Some(1))[0].3, "pending");
    // unknown order id
    assert!(!db.set_order_status(1, "nope1234", "shipped"));
    // tracking, same seller scoping
    db.set_order_tracking(1, &oid, "TRACK123");
    assert_eq!(db.list_orders(Some(1))[0].4.as_deref(), Some("TRACK123"));
    assert_eq!(db.list_orders(Some(2))[0].4, None);
}

#[test]
fn test_invoice_product_roundtrip() {
    let path = "/tmp/opencode/test_invoice_product_roundtrip.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    let pid = db
        .add_product(
            1,
            "guide",
            "2",
            "TON",
            "digital_file",
            Some("FILEID123"),
            None,
        )
        .unwrap();
    db.save_invoice(
        "inv_p",
        1,
        "2",
        "TON",
        None,
        "active",
        None,
        None,
        Some(42),
        Some(pid),
    );
    assert_eq!(db.invoice_product("inv_p"), Some(pid));
    let (dp, kind, name, delivery) = db.invoice_product_delivery("inv_p").unwrap();
    assert_eq!(dp, pid);
    assert_eq!(kind, "digital_file");
    assert_eq!(name, "guide");
    assert_eq!(delivery.as_deref(), Some("FILEID123"));
    // plan invoices (product_id NULL) take the existing plan path
    db.save_invoice(
        "inv_plan",
        1,
        "10",
        "USDT",
        None,
        "active",
        None,
        Some("gold"),
        Some(42),
        None,
    );
    assert_eq!(db.invoice_product("inv_plan"), None);
    assert!(db.invoice_product_delivery("inv_plan").is_none());
    assert!(db.invoice_product("missing").is_none());
    assert!(db.invoice_product_delivery("missing").is_none());
}

#[test]
fn test_subscription_status_and_by_invoice() {
    let path = "/tmp/opencode/test_subscription_status_and_by_invoice.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    let id = db.create_subscription(1, 42, None, "@chan", 7, Some("inv_s"), None);
    assert_eq!(
        db.subscription_by_invoice("inv_s"),
        Some((id, "@chan".to_string()))
    );
    assert!(db.subscription_by_invoice("nope").is_none());
    db.set_subscription_status(id, "refunded");
    assert_eq!(db.list_subscriptions(Some(1), 10)[0].4, "refunded");
}

#[test]
fn test_list_subscriptions_filtered() {
    let path = "/tmp/opencode/test_list_subscriptions_filtered.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    let id1 = db.create_subscription(1, 42, None, "@chan", 7, Some("inv_a"), None);
    let id2 = db.create_subscription(1, 43, None, "@chan", 7, Some("inv_b"), None);
    db.set_subscription_status(id2, "expired");
    // status filter
    let active = db.list_subscriptions_filtered(Some(1), Some("active"), 10);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].0, id1);
    let expired = db.list_subscriptions_filtered(Some(1), Some("expired"), 10);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].0, id2);
    // no status filter returns all
    let all = db.list_subscriptions_filtered(Some(1), None, 10);
    assert_eq!(all.len(), 2);
    // global scope
    let global = db.list_subscriptions_filtered(None, None, 10);
    assert_eq!(global.len(), 2);
    // order by id desc
    assert!(global[0].0 > global[1].0);
    // limit
    let limited = db.list_subscriptions_filtered(Some(1), None, 1);
    assert_eq!(limited.len(), 1);
    let _ = id1;
}

/// Bot scoping for the console: `list_subscriptions_for_sellers` returns
/// exactly the rows of the requested sellers (never the others), in the
/// same newest-first order and row shape as `list_subscriptions`, honours
/// the limit, and short-circuits an empty slice (no `IN ()` SQL).
#[test]
fn test_list_subscriptions_for_sellers_filters_and_limits() {
    let path = "/tmp/opencode/test_list_subscriptions_for_sellers.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@a"));
    db.upsert_seller(2, Some("bob"), Some("@b"));
    db.upsert_seller(3, Some("carol"), Some("@c"));
    let s1 = db.create_subscription(1, 42, None, "@a", 7, Some("inv_s1"), None);
    let s2 = db.create_subscription(1, 43, None, "@a", 7, Some("inv_s2"), None);
    let s3 = db.create_subscription(2, 44, None, "@b", 7, Some("inv_s3"), None);
    let s4 = db.create_subscription(3, 45, None, "@c", 7, Some("inv_s4"), None);
    assert!(s1 > 0 && s4 > 0);

    // Empty scope never issues SQL and never widens to "all sellers".
    assert!(db.list_subscriptions_for_sellers(&[], 10).is_empty());

    // One seller only.
    let only_two = db.list_subscriptions_for_sellers(&[2], 10);
    assert_eq!(only_two.len(), 1);
    assert_eq!(only_two[0].0, s3);
    assert_eq!(only_two[0].1, 2);

    // Two sellers: identical to the unscoped list filtered by seller.
    let expected: Vec<(i64, i64, i64, String, String, String)> = db
        .list_subscriptions(None, 10)
        .into_iter()
        .filter(|r| r.1 == 1 || r.1 == 2)
        .collect();
    assert_eq!(expected.len(), 3);
    let both = db.list_subscriptions_for_sellers(&[1, 2], 10);
    assert_eq!(
        both, expected,
        "same rows, same order as list_subscriptions"
    );
    // newest first, and seller 3 never leaks in.
    assert!(both[0].0 > both[1].0 && both[1].0 > both[2].0);
    assert!(both.iter().all(|r| r.1 != 3));

    // Limit applies after the seller filter.
    let limited = db.list_subscriptions_for_sellers(&[1, 2], 2);
    assert_eq!(limited, expected[..2].to_vec());
    let _ = s2;
    let _ = fs::remove_file(path);
}

#[test]
fn test_refund_and_payout_state_transitions() {
    let path = "/tmp/opencode/test_refund_and_payout_state_transitions.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.upsert_seller(2, Some("bob"), Some("@chan"));
    db.save_invoice(
        "inv_r1",
        1,
        "10",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    // refund starts pending; unknown user falls back to the invoice buyer
    let rid = db.create_refund(1, Some("inv_r1"), None, "10", "USDT", Some("damaged"));
    assert!(rid > 0);
    let refunds = db.list_refunds(Some(1));
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0].0, rid);
    assert_eq!(refunds[0].1, 1);
    assert_eq!(refunds[0].2.as_deref(), Some("inv_r1"));
    assert_eq!(refunds[0].3, Some(42));
    assert_eq!(refunds[0].4, "10");
    assert_eq!(refunds[0].5, "USDT");
    assert_eq!(refunds[0].6.as_deref(), Some("damaged"));
    assert_eq!(refunds[0].7, "pending");
    // pending -> approved -> executed
    db.set_refund_status(rid, "approved");
    assert_eq!(db.list_refunds(Some(1))[0].7, "approved");
    db.set_refund_status(rid, "executed");
    assert_eq!(db.get_refund(rid.to_string()).unwrap().7, "executed");
    // explicit user + scoped listing
    let rid2 = db.create_refund(2, None, Some(7), "3", "TON", None);
    assert!(rid2 > 0);
    assert_eq!(db.list_refunds(Some(2))[0].3, Some(7));
    assert_eq!(db.list_refunds(None).len(), 2);
    // payout pending -> approved -> paid
    let pid = db.create_payout(1, "5", "USDT", Some("@alice"));
    assert!(pid > 0);
    assert_eq!(db.list_payouts(Some(1))[0].5, "pending");
    db.set_payout_status(pid, "approved");
    assert_eq!(db.list_payouts(Some(1))[0].5, "approved");
    db.set_payout_status(pid, "paid");
    assert_eq!(db.get_payout(pid.to_string()).unwrap().5, "paid");
    assert_eq!(db.list_payouts(Some(2)).len(), 0);
}

/// CAS execution claims on payouts: begin only wins from 'approved',
/// exactly one concurrent claimer wins, finish only applies from
/// 'executing', and invalid finish statuses are rejected with no write.
#[test]
fn test_payout_execution_cas_transitions() {
    let path = "/tmp/opencode/test_payout_execution_cas_transitions.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));

    // begin-from-pending is refused; begin-from-approved wins
    let pid = db.create_payout(1, "5", "USDT", Some("@alice"));
    assert_eq!(
        db.get_payout_status(pid.to_string()).as_deref(),
        Some("pending")
    );
    assert!(!db.begin_payout_execution(pid.to_string()));
    db.set_payout_status(pid, "approved");
    assert!(db.begin_payout_execution(pid.to_string()));
    assert_eq!(
        db.get_payout_status(pid.to_string()).as_deref(),
        Some("executing")
    );

    // double-begin loses: the row is no longer 'approved'
    let pid2 = db.create_payout(1, "7", "USDT", Some("@alice"));
    db.set_payout_status(pid2, "approved");
    assert!(db.begin_payout_execution(pid2.to_string()));
    assert!(!db.begin_payout_execution(pid2.to_string()));

    // finish applies only from 'executing'
    assert_eq!(db.finish_payout_execution(pid.to_string(), "paid"), 1);
    assert_eq!(
        db.get_payout_status(pid.to_string()).as_deref(),
        Some("paid")
    );
    // finish-from-wrong-status (already 'paid') changes nothing
    assert_eq!(db.finish_payout_execution(pid.to_string(), "approved"), 0);
    assert_eq!(db.finish_payout_execution(pid.to_string(), "paid"), 0);

    // revert path: 'executing' -> 'approved' is legal, then re-executable
    assert_eq!(db.finish_payout_execution(pid2.to_string(), "approved"), 1);
    assert_eq!(
        db.get_payout_status(pid2.to_string()).as_deref(),
        Some("approved")
    );
    assert!(db.begin_payout_execution(pid2.to_string()));
    assert_eq!(db.finish_payout_execution(pid2.to_string(), "paid"), 1);

    // invalid status args are rejected with no DB write (row stays
    // 'executing'); matching is case-sensitive by design
    let pid3 = db.create_payout(1, "9", "USDT", Some("@alice"));
    db.set_payout_status(pid3, "approved");
    assert!(db.begin_payout_execution(pid3.to_string()));
    for bad in ["failed", "processed", "pending", "executing", "PAID", ""] {
        assert_eq!(db.finish_payout_execution(pid3.to_string(), bad), 0);
    }
    assert_eq!(
        db.get_payout_status(pid3.to_string()).as_deref(),
        Some("executing")
    );
}

/// CAS execution claims on refunds: same contract as payouts, with
/// "executed" as the terminal outcome.
#[test]
fn test_refund_execution_cas_transitions() {
    let path = "/tmp/opencode/test_refund_execution_cas_transitions.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));

    // begin-from-pending is refused; begin-from-approved wins
    let rid = db.create_refund(1, None, Some(42), "10", "USDT", Some("damaged"));
    assert!(!db.begin_refund_execution(rid.to_string()));
    db.set_refund_status(rid, "approved");
    assert!(db.begin_refund_execution(rid.to_string()));
    assert_eq!(db.get_refund(rid.to_string()).unwrap().7, "executing");

    // double-begin loses: the row is no longer 'approved'
    let rid2 = db.create_refund(1, None, Some(43), "3", "TON", None);
    db.set_refund_status(rid2, "approved");
    assert!(db.begin_refund_execution(rid2.to_string()));
    assert!(!db.begin_refund_execution(rid2.to_string()));

    // finish applies only from 'executing'
    assert_eq!(db.finish_refund_execution(rid.to_string(), "executed"), 1);
    assert_eq!(db.get_refund(rid.to_string()).unwrap().7, "executed");
    // finish-from-wrong-status (already 'executed') changes nothing
    assert_eq!(db.finish_refund_execution(rid.to_string(), "approved"), 0);
    assert_eq!(db.finish_refund_execution(rid.to_string(), "executed"), 0);

    // revert path: 'executing' -> 'approved' is legal, then re-executable
    assert_eq!(db.finish_refund_execution(rid2.to_string(), "approved"), 1);
    assert_eq!(db.get_refund(rid2.to_string()).unwrap().7, "approved");
    assert!(db.begin_refund_execution(rid2.to_string()));
    assert_eq!(db.finish_refund_execution(rid2.to_string(), "executed"), 1);

    // invalid status args are rejected with no DB write (row stays
    // 'executing'); matching is case-sensitive by design
    let rid3 = db.create_refund(1, None, Some(44), "1", "USDT", None);
    db.set_refund_status(rid3, "approved");
    assert!(db.begin_refund_execution(rid3.to_string()));
    for bad in ["paid", "failed", "processed", "pending", "EXECUTED", ""] {
        assert_eq!(db.finish_refund_execution(rid3.to_string(), bad), 0);
    }
    assert_eq!(db.get_refund(rid3.to_string()).unwrap().7, "executing");
}

proptest! {
    /// Model check of the invoice claim state machine: for any sequence of
    /// claim / finalize / reset operations, the database's observable state
    /// always matches the reference model (active → granting → paid, reset
    /// re-activates) and a claim succeeds only when the model is 'active'.
    #[test]
    fn prop_claim_state_machine(ops in proptest::collection::vec(0u8..3, 1..40)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claim.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.upsert_seller(1, Some("s"), Some("@chan"));
        db.save_invoice(
            "inv",
            1,
            "10",
            "USDT",
            None,
            "active",
            None,
            Some("P"),
            Some(42),
            None,
        );
        let mut model = 0u8; // 0=active, 1=granting, 2=paid
        for op in ops {
            match op {
                0 => {
                    let won = db.claim_invoice_for_grant("inv");
                    prop_assert_eq!(won, model == 0);
                    if model == 0 {
                        model = 1;
                    }
                }
                1 => {
                    db.finalize_invoice_paid("inv");
                    model = 2;
                }
                _ => {
                    db.update_invoice_status("inv", "active");
                    model = 0;
                }
            }
            let status = db.get_invoice("inv").unwrap().2.clone();
            let want = match model {
                0 => "active",
                1 => "granting",
                _ => "paid",
            };
            prop_assert_eq!(status, want);
        }
    }

    /// Any i64 plan duration survives create_subscription without panicking.
    #[test]
    fn prop_subscription_arbitrary_days(
        seller in 1i64..1000,
        user in 1i64..1000,
        days in any::<i64>(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.upsert_seller(seller, Some("s"), Some("@chan"));
        let id = db.create_subscription(seller, user, None, "@chan", days, None, None);
        prop_assert!(id > 0);
    }
}

proptest! {
    /// Product grant lifecycle: no matter how many grant attempts race in,
    /// an invoice's product is granted at most once (double-grant guard),
    /// and exhaustion is reported rather than overselling.
    #[test]
    fn prop_product_grant_exactly_once(
        attempts in 1u8..8,
        stock in proptest::option::of(0i64..3),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pg.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.upsert_seller(7, Some("s"), Some("@chan"));
        let pid = db
            .add_product(7, "Ebook", "5", "USDT", "digital_link", Some("https://x/dl"), stock)
            .unwrap();
        db.save_invoice(
            "invP",
            7,
            "5",
            "USDT",
            None,
            "active",
            None,
            None,
            Some(9),
            None,
        );
        let mut granted = 0;
        for _ in 0..attempts {
            match db.grant_product(7, 9, pid, "invP") {
                Ok(true) => granted += 1,
                Ok(false) => {}
                Err(_) => break, // out of stock
            }
        }
        prop_assert!(granted <= 1);
        prop_assert_eq!(db.product_grant_exists("invP"), granted == 1);
    }
}

#[test]
fn test_subscription_does_not_panic_on_huge_days() {
    let path = "/tmp/opencode/test_subscription_does_not_panic_on_huge_days.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    let id = db.create_subscription(1, 42, None, "@chan", i64::MAX, None, None);
    assert!(id > 0);
    // extreme negative duration must not panic either
    let id2 = db.create_subscription(1, 43, None, "@chan", -i64::MAX, None, None);
    assert!(id2 > 0);
    let subs = db.list_subscriptions(Some(1), 10);
    assert_eq!(subs.len(), 2);
}

#[tokio::test]
async fn test_one_time_credential_rotation() {
    let path = "/tmp/opencode/test_one_time_credential_rotation.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.create_web_account("rot", "pw", "admin", None)
        .await
        .unwrap();
    let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("rot").unwrap();
    let token = db.create_web_session(acc_id, "admin", None, 7);
    assert!(db.get_web_session(&token).is_some());
    // Simulate a pre-CSPRNG database: clear the rotation marker.
    {
        let conn = db
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM config WHERE key='migration_rotate_tokens_v1'",
            [],
        )
        .unwrap();
    }
    // Re-opening rotates legacy credentials exactly once.
    let db2 = Db::new(path).unwrap();
    assert!(db2.get_web_session(&token).is_none());
    // Sessions created after the rotation survive later restarts.
    let token2 = db2.create_web_session(acc_id, "admin", None, 7);
    assert!(db2.get_web_session(&token2).is_some());
    let db3 = Db::new(path).unwrap();
    assert!(db3.get_web_session(&token2).is_some());
}

#[test]
fn test_duplicate_invoice_grants_do_not_break_startup() {
    let path = "/tmp/opencode/test_duplicate_invoice_grants.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.create_subscription(1, 42, None, "@c", 7, Some("dup"), None);
    db.create_subscription(1, 43, None, "@c", 7, Some("dup"), None);
    // Re-open: the unique index cannot be built over duplicates, but
    // startup must continue.
    let db2 = Db::new(path).unwrap();
    assert!(db2.subscription_exists_for_invoice("dup"));
}

#[test]
fn test_admin_add_remove() {
    let path = "/tmp/opencode/test_admin_add_remove.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    let _owner_id = 999i64;
    assert!(!db.is_admin(5, None));
    db.add_admin(5, Some("admin5"));
    assert!(db.is_admin(5, None));
    assert!(db.list_admins().contains(&5));
    db.remove_admin(5);
    assert!(!db.is_admin(5, None));
    assert!(!db.list_admins().contains(&5));
}

/// `set_admin_bot_id` scopes a freshly added admin (NULL → global) to an
/// existing bot, clears it back to NULL, rejects unknown bot ids without
/// touching the stored scope, and reports "no rows changed" for unknown
/// users. Write-only concern: `is_admin`/`get_admin_bot_id` stay as-is.
#[test]
fn test_set_admin_bot_id_scopes_and_clears() {
    let path = "/tmp/opencode/test_set_admin_bot_id.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    let bot_a = db.create_bot("bot-a", 100, None, "");
    let bot_b = db.create_bot("bot-b", 101, None, "");
    assert!(bot_a > 0 && bot_b > 0);
    db.add_admin(7, Some("admin7"));

    // A freshly added admin is global (bot_id NULL).
    assert_eq!(db.get_admin_bot_id(7), None);
    assert!(db.is_admin(7, None));

    // Assign an existing bot: the row now reports it.
    assert!(db.set_admin_bot_id(7, Some(bot_b)));
    assert_eq!(db.get_admin_bot_id(7), Some(bot_b));

    // Re-assign to another existing bot.
    assert!(db.set_admin_bot_id(7, Some(bot_a)));
    assert_eq!(db.get_admin_bot_id(7), Some(bot_a));

    // Unknown bot id: rejected, prior assignment untouched.
    assert!(!db.set_admin_bot_id(7, Some(bot_a + 999)));
    assert_eq!(db.get_admin_bot_id(7), Some(bot_a));

    // Clear back to NULL = global admin; the admins row itself survives.
    assert!(db.set_admin_bot_id(7, None));
    assert_eq!(db.get_admin_bot_id(7), None);
    assert!(db.is_admin(7, None));
    assert!(db.list_admins().contains(&7));

    // Unknown user: nothing to update, for both Some and None.
    assert!(!db.set_admin_bot_id(4242, Some(bot_a)));
    assert!(!db.set_admin_bot_id(4242, None));
    assert_eq!(db.get_admin_bot_id(4242), None);
    let _ = fs::remove_file(path);
}

#[test]
fn test_expired_subscriptions() {
    let path = "/tmp/opencode/test_expired_subscriptions.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    let id = db.create_subscription(1, 42, None, "@chan", -1, None, None);
    let expired = db.expired_subscriptions();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].0, id);
    db.mark_subscription_expired(id);
    let subs = db.list_subscriptions(Some(1), 10);
    assert_eq!(subs[0].4, "expired");
}

/// The scoped expire must touch only the requested seller's active-but-expired
/// rows; a foreign seller's rows and the caller's still-valid rows stay active.
#[test]
fn test_expire_subscriptions_for_seller_is_scoped() {
    let path = "/tmp/opencode/test_expire_subscriptions_for_seller.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.upsert_seller(2, Some("bob"), Some("@chan"));
    let own_expired = db.create_subscription(1, 42, None, "@chan", -1, None, None);
    let own_valid = db.create_subscription(1, 43, None, "@chan", 7, None, None);
    let foreign_expired = db.create_subscription(2, 44, None, "@chan", -1, None, None);
    let foreign_valid = db.create_subscription(2, 45, None, "@chan", 7, None, None);

    assert_eq!(db.expire_subscriptions_for_seller(1), 1);
    let status = |id: i64| {
        db.list_subscriptions(None, 1000)
            .into_iter()
            .find(|s| s.0 == id)
            .unwrap()
            .4
    };
    assert_eq!(status(own_expired), "expired");
    assert_eq!(status(own_valid), "active");
    assert_eq!(status(foreign_expired), "active");
    assert_eq!(status(foreign_valid), "active");

    // Idempotent: a second pass finds nothing left for this seller.
    assert_eq!(db.expire_subscriptions_for_seller(1), 0);
    // The global view still sees only the other seller's expired row.
    let global = db.expired_subscriptions();
    assert_eq!(global.len(), 1);
    assert_eq!(global[0].0, foreign_expired);
    let _ = fs::remove_file(path);
}

#[test]
fn test_ai_template_upsert_get_list_delete() {
    let path = "/tmp/opencode/test_ai_template_upsert_get_list_delete.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.upsert_ai_template(
        1,
        "t1",
        Some("desc"),
        Some("sys"),
        "user prompt",
        Some("vars"),
    );
    let got = db.get_ai_template(1, "t1").unwrap();
    assert_eq!(got.0, "t1");
    assert_eq!(got.1, Some("desc".to_string()));
    assert_eq!(got.3, "user prompt");
    let list = db.list_ai_templates(1);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "t1");
    // upsert update
    db.upsert_ai_template(1, "t1", Some("desc2"), Some("sys2"), "new prompt", None);
    let got2 = db.get_ai_template(1, "t1").unwrap();
    assert_eq!(got2.1, Some("desc2".to_string()));
    assert_eq!(got2.3, "new prompt");
    db.delete_ai_template(1, "t1");
    assert!(db.get_ai_template(1, "t1").is_none());
    assert!(db.list_ai_templates(1).is_empty());
}

#[test]
fn test_ai_usage_record_and_summary() {
    let path = "/tmp/opencode/test_ai_usage_record_and_summary.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.record_ai_usage(
        1,
        Some(42),
        "openai",
        "gpt",
        Some("t1"),
        10,
        5,
        15,
        0.1,
        "ok",
        None,
    );
    assert_eq!(db.ai_usage_today_tokens(1), 15);
    let summary = db.ai_usage_summary(1, 7);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].1, 15);
    assert_eq!(summary[0].2, 1);
}

#[test]
fn test_ai_usage_global_and_recent_errors() {
    let path = "/tmp/opencode/test_ai_usage_global_and_recent_errors.db";
    let _ = fs::remove_file(path);
    let db = Db::new(path).unwrap();
    db.upsert_seller(1, Some("alice"), Some("@chan"));
    db.record_ai_usage(
        1,
        Some(42),
        "openai",
        "gpt",
        Some("t1"),
        10,
        5,
        15,
        0.1,
        "ok",
        None,
    );
    db.record_ai_usage(
        1,
        Some(42),
        "openai",
        "gpt",
        Some("t1"),
        1,
        1,
        2,
        0.01,
        "error",
        Some("boom"),
    );
    assert_eq!(db.ai_usage_today_tokens_global(), 17);
    let gsummary = db.ai_usage_summary_global(7);
    assert_eq!(gsummary.len(), 1);
    assert_eq!(gsummary[0].1, 17);
    assert_eq!(gsummary[0].2, 2);
    let cost = db.ai_usage_cost_total_global(7);
    assert!((cost - 0.11).abs() < 1e-9);
    // global recent errors
    let errs = db.ai_usage_recent_errors_global(10);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].4, "error");
    assert_eq!(errs[0].3, Some("boom".to_string()));
    // seller scoped
    let serrs = db.ai_usage_recent_errors(1, 10);
    assert_eq!(serrs.len(), 1);
    assert_eq!(serrs[0].1, "openai");
    // no errors for other seller
    assert!(db.ai_usage_recent_errors(2, 10).is_empty());
}

#[test]
fn test_scheduled_post_create_and_list_due() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sched.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let id = db.create_scheduled_post(1, "chat", "content", "text", &past);
    assert!(id > 0);
    let due = db.list_due_scheduled();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].0, id);
    db.mark_scheduled_posted(id, 999).unwrap();
    let due2 = db.list_due_scheduled();
    assert!(due2.is_empty());
}

#[test]
fn test_scheduled_post_attempts_cap_terminal() {
    // Bounded retries (2026-10-25 review): every send failure bumps the
    // counter; at the cap the post leaves the due queue with status
    // 'failed' instead of retrying forever. A concurrent 'posted' win is
    // never overwritten by the terminal mark.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sched_cap.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let id = db.create_scheduled_post(1, "chat", "content", "text", &past);
    for expected in 1..=10 {
        assert_eq!(db.bump_scheduled_attempts(id), expected);
    }
    // Not terminal yet: still due.
    assert_eq!(db.list_due_scheduled().len(), 1);
    db.mark_scheduled_failed(id);
    assert!(db.list_due_scheduled().is_empty());
    let all = db.list_scheduled_posts(Some(1));
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].6, "failed");
    // The terminal mark is scoped to 'pending': a posted row stays.
    let id2 = db.create_scheduled_post(1, "chat", "content", "text", &past);
    db.mark_scheduled_posted(id2, 42).unwrap();
    db.mark_scheduled_failed(id2);
    let all2 = db.list_scheduled_posts(Some(1));
    assert_eq!(
        all2.iter().find(|r| r.0 == id2).map(|r| r.6.as_str()),
        Some("posted")
    );
}

#[test]
fn test_scheduled_post_cancel_scoped() {
    // /cancelpost semantics: only the owning seller (or admin/owner with no
    // scope) cancels; scoped to 'pending', so posted/failed rows are never
    // overwritten; a cancelled row leaves the due queue and a second cancel
    // is the same refusal; unknown ids read as the same refusal (no oracle).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sched_cancel.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let mine = db.create_scheduled_post(1, "chat", "a", "text", &past);
    let other = db.create_scheduled_post(2, "chat", "b", "text", &past);

    // Wrong scope: another seller's row is not cancelled.
    assert!(!db.cancel_scheduled_post(mine, Some(2)));
    assert!(!db.cancel_scheduled_post(other, Some(1)));
    assert_eq!(db.list_due_scheduled().len(), 2, "refusals store nothing");

    // Right scope: the owner cancels their own pending row.
    assert!(db.cancel_scheduled_post(mine, Some(1)));
    assert!(db.list_due_scheduled().iter().all(|r| r.0 != mine));
    assert_eq!(db.list_due_scheduled().len(), 1);
    let all = db.list_scheduled_posts(Some(1));
    assert_eq!(
        all.iter().find(|r| r.0 == mine).map(|r| r.6.as_str()),
        Some("cancelled")
    );

    // Already cancelled and unknown ids: the same refusal.
    assert!(!db.cancel_scheduled_post(mine, Some(1)));
    assert!(!db.cancel_scheduled_post(9999, None));

    // Admin/owner (no scope) cancels any pending row — never a posted one.
    assert!(db.cancel_scheduled_post(other, None));
    let posted = db.create_scheduled_post(2, "chat", "c", "text", &past);
    db.mark_scheduled_posted(posted, 1).unwrap();
    assert!(!db.cancel_scheduled_post(posted, None));
    assert_eq!(
        db.list_scheduled_posts(Some(2))
            .iter()
            .find(|r| r.0 == posted)
            .map(|r| r.6.as_str()),
        Some("posted")
    );
}

#[test]
fn test_pending_scheduled_posts_scoping() {
    // /scheduled listing: pending rows only (cancelled/posted/failed never
    // appear), seller-scoped when a scope is given, all for admin/owner,
    // soonest first.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sched_pending.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let soon = chrono::Utc::now().to_rfc3339();
    let later = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let s1_late = db.create_scheduled_post(1, "chat", "late", "text", &later);
    let s1_soon = db.create_scheduled_post(1, "chat", "soon", "text", &soon);
    let s2 = db.create_scheduled_post(2, "chat", "other", "text", &soon);
    // Non-pending rows of seller 1 must not come back.
    assert!(db.cancel_scheduled_post(s1_late, Some(1)));
    let s1_done = db.create_scheduled_post(1, "chat", "done", "text", &soon);
    db.mark_scheduled_posted(s1_done, 5).unwrap();

    let scoped = db.list_pending_scheduled_posts(Some(1));
    assert_eq!(
        scoped.iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![s1_soon]
    );
    assert_eq!(scoped[0].1, 1, "rows stay seller-tagged");
    let everyone = db.list_pending_scheduled_posts(None);
    assert_eq!(
        everyone.iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![s1_soon, s2],
        "same scheduled_at ties break by id"
    );
}

#[test]
fn test_scheduled_post_unparseable_detected() {
    // The A6 poison sweep: pending rows whose scheduled_at is not RFC3339 are
    // reported with their raw stored string — both the never-due kind
    // ("tomorrow" sorts above every RFC3339 timestamp) and the always-due
    // kind ("01/02/2026" sorts below). Parseable rows never appear, and a
    // cancelled poison row stops being reported (pending only).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sched_poison.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let good = db.create_scheduled_post(1, "chat", "ok", "text", &past);
    let late_poison = db.create_scheduled_post(1, "chat", "never", "text", "tomorrow");
    let early_poison = db.create_scheduled_post(1, "chat", "early", "text", "01/02/2026");
    let mut poison = db.list_pending_scheduled_unparseable();
    poison.sort();
    assert_eq!(poison.len(), 2, "parseable rows are not poison: {poison:?}");
    assert_eq!(poison[0].0, late_poison);
    assert_eq!(poison[0].1, "tomorrow");
    assert_eq!(poison[1].0, early_poison);
    assert_eq!(poison[1].1, "01/02/2026");
    let _ = good;
    // Pending-only: once cancelled, a poison row stops being reported.
    assert!(db.cancel_scheduled_post(early_poison, Some(1)));
    let poison2 = db.list_pending_scheduled_unparseable();
    assert_eq!(poison2, vec![(late_poison, "tomorrow".to_string())]);
}

#[test]
fn test_invoice_attempts_bump_and_terminal_state() {
    // The verification-attempts counter advances only via
    // bump_invoice_attempts (the "not settled yet" release path never
    // touches it), and the terminal 'failed' status drops the invoice
    // from pending_invoices() — the verifier's queue.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inv_cap.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.save_invoice(
        "inv1",
        1,
        "5",
        "USDT",
        None,
        "active",
        None,
        Some("gold"),
        Some(7),
        None,
    );
    assert_eq!(db.pending_invoices().len(), 1);
    for expected in 1..=10 {
        assert_eq!(db.bump_invoice_attempts("inv1"), expected);
    }
    // Counter column is stored, not just returned.
    let raw: i64 = {
        let conn = Connection::open(&path).unwrap();
        conn.query_row(
            "SELECT attempts FROM invoices WHERE invoice_id='inv1'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(raw, 10);
    // Terminal-failed leaves the queue (verifier + release paths use the
    // same status vocabulary the rest of the codebase writes).
    db.update_invoice_status("inv1", "failed");
    assert!(db.pending_invoices().is_empty());
    // And an unrelated invoice keeps its own counter at zero.
    db.save_invoice(
        "inv2",
        1,
        "5",
        "USDT",
        None,
        "active",
        None,
        Some("gold"),
        Some(7),
        None,
    );
    assert_eq!(db.bump_invoice_attempts("inv2"), 1);
}

#[test]
fn test_wal_checkpoint_best_effort() {
    // checkpoint() must never panic — including when WAL is unavailable
    // (in-memory dbs) or the file was never written — and must leave the
    // database usable afterwards.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wal.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.set_config("k", "v");
    db.checkpoint();
    assert_eq!(db.get_config("k").as_deref(), Some("v"));
    db.checkpoint();
    // On a WAL-mode file the TRUNCATE checkpoint empties the -wal log.
    let wal = dir.path().join("wal.db-wal");
    if wal.exists() {
        assert_eq!(wal.metadata().unwrap().len(), 0);
    }
}

#[test]
fn test_plan_namespace_cross_seller_isolation() {
    // (seller_id, name) namespace: two sellers may both define "pro" and
    // each resolves only their own row; a name unique to another seller
    // is invisible from the wrong namespace. seller_id 0 is the
    // admin/owner global namespace.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ns.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.add_plan(1, "pro", "10", "USDT", 30, None);
    db.add_plan(2, "pro", "20", "USDT", 30, None);
    let p1 = db.find_plans_by_name(Some(1), "pro");
    let p2 = db.find_plans_by_name(Some(2), "pro");
    assert_eq!(p1.len(), 1);
    assert_eq!(p1[0].1, "10");
    assert_eq!(p1[0].6, 1);
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].1, "20");
    assert_eq!(p2[0].6, 2);
    // a name unique to seller 2 is not resolvable from seller 1
    db.add_plan(2, "solo", "5", "TON", 7, None);
    assert!(db.find_plans_by_name(Some(1), "solo").is_empty());
    // the global namespace (0) is its own namespace, not a wildcard
    db.add_plan(0, "legacy", "3", "USDT", 30, None);
    assert_eq!(db.find_plans_by_name(Some(0), "legacy").len(), 1);
    assert!(db.find_plans_by_name(Some(1), "legacy").is_empty());
    // cross-seller active view returns all namespaces (caller filters)
    assert_eq!(db.find_plans_by_name(None, "pro").len(), 2);
    // the (seller_id, name) uniqueness holds per seller: re-adding the
    // same name to one seller upserts (INSERT OR REPLACE), it does not
    // create a second row or collide with the other seller's plan.
    db.add_plan(1, "pro", "11", "USDT", 30, None);
    assert_eq!(db.find_plans_by_name(Some(1), "pro").len(), 1);
    assert_eq!(db.find_plans_by_name(Some(1), "pro")[0].1, "11");
}

#[test]
fn test_plan_namespace_dedupe_migration() {
    // Legacy database: plans WITHOUT the seller_id column and WITHOUT the
    // UNIQUE(seller_id, name) constraint (both arrive via migration), so
    // duplicate same-name rows can exist. Db::new must dedupe them
    // (keeping the LOWEST id), re-point subscriptions/activation codes,
    // and create the unique index — while a fresh database must NOT get
    // a redundant manual index next to the schema-level constraint.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
                "CREATE TABLE plans (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, price TEXT NOT NULL, asset TEXT NOT NULL, duration_days INTEGER NOT NULL, channel_id TEXT, active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL);
                 CREATE TABLE subscriptions (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL, plan_id INTEGER, channel_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'active', starts_at TEXT NOT NULL, expires_at TEXT NOT NULL, invoice_id TEXT, code_id INTEGER, created_at TEXT NOT NULL);
                 CREATE TABLE activation_codes (id INTEGER PRIMARY KEY AUTOINCREMENT, code TEXT NOT NULL UNIQUE, plan_id INTEGER, value_days INTEGER, max_uses INTEGER NOT NULL DEFAULT 1, uses INTEGER NOT NULL DEFAULT 0, expires_at TEXT, created_at TEXT NOT NULL);
                 INSERT INTO plans(name,price,asset,duration_days,created_at) VALUES('gold','10','USDT',30,'2024-01-01');
                 INSERT INTO plans(name,price,asset,duration_days,created_at) VALUES('gold','20','USDT',30,'2024-01-02');
                 INSERT INTO plans(name,price,asset,duration_days,created_at) VALUES('gold','30','USDT',30,'2024-01-03');
                 INSERT INTO subscriptions(user_id,plan_id,channel_id,starts_at,expires_at,created_at) VALUES(7,3,'@c','2024-01-01','2024-02-01','2024-01-01');
                 INSERT INTO activation_codes(code,plan_id,created_at) VALUES('CODE1',2,'2024-01-01');",
            )
            .unwrap();
    }
    let db = Db::new(path.to_str().unwrap()).unwrap();
    // All three legacy rows carried seller_id 0; the lowest id (price
    // '10') survived.
    let kept = db.get_plan(0, "gold").expect("kept lowest-id row");
    assert_eq!(kept.0, 1);
    assert_eq!(kept.1, "10");
    assert_eq!(db.list_plans(Some(0)).len(), 1);
    // References were re-pointed to the kept row, not orphaned.
    {
        let conn = Connection::open(&path).unwrap();
        let sub_plan: i64 = conn
            .query_row(
                "SELECT plan_id FROM subscriptions WHERE user_id=7",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sub_plan, 1);
        let code_plan: i64 = conn
            .query_row(
                "SELECT plan_id FROM activation_codes WHERE code='CODE1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(code_plan, 1);
        // The legacy table got the unique index (it has no table-level
        // constraint for sqlite to rely on).
        let idx: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list('plans') WHERE name='idx_plans_seller_name'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(idx, 1);
        // The index actually enforces the namespace now.
        let dup = conn.execute(
                "INSERT INTO plans(seller_id,name,price,asset,duration_days,active,created_at) VALUES(0,'gold','99','USDT',30,1,'2024-01-04')",
                [],
            );
        assert!(dup.is_err(), "unique index must reject a duplicate");
    }
    // A FRESH database relies on the schema-level UNIQUE(seller_id, name)
    // constraint and must not carry the redundant manual index.
    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh = Db::new(fresh_dir.path().join("fresh.db").to_str().unwrap()).unwrap();
    drop(fresh);
    {
        let conn = Connection::open(fresh_dir.path().join("fresh.db")).unwrap();
        let idx: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list('plans') WHERE name='idx_plans_seller_name'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(
            idx, 0,
            "fresh db uses the schema constraint, not a dup index"
        );
        let constraint: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list('plans') WHERE \"unique\"=1 AND origin='u'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(constraint, 1);
    }
}

#[test]
fn test_channels_add_dedupe_and_scoping() {
    // The channels store: seller_id 0 is the global namespace; names are
    // unique console-wide and case-insensitive (a Telegram @username
    // names one channel no matter who references it).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("channels.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert!(db.add_channel("@global", 0).is_ok());
    assert!(db.add_channel("@seller1", 1).is_ok());
    // Dedupe: same channel, different case or namespace, is refused with
    // an honest message — not a raw SQLite constraint dump. (The message
    // names the caller's spelling, so assert the refusal class, not an
    // exact string across cases.)
    let dup = db.add_channel("@GLOBAL", 0).unwrap_err();
    assert!(dup.contains("already exists"), "got: {dup}");
    assert!(
        db.add_channel("@global", 1)
            .unwrap_err()
            .contains("already exists"),
        "dedupe is console-wide across namespaces"
    );
    assert_eq!(db.list_channels(None).len(), 2, "duplicates must not land");
    // Empty names are rejected.
    assert!(db.add_channel("   ", 0).is_err());
    // Scoping: the owner view (None) sees everything; a seller sees the
    // global namespace plus their own rows, never another seller's.
    assert_eq!(db.list_channels(None).len(), 2);
    let s1 = db.list_channels(Some(1));
    assert_eq!(s1.len(), 2);
    assert!(s1.iter().any(|c| c.channel == "@global"));
    assert!(s1.iter().any(|c| c.channel == "@seller1"));
    db.add_channel("@seller2", 2).unwrap();
    let s2 = db.list_channels(Some(2));
    assert_eq!(s2.len(), 2);
    assert!(
        s2.iter().all(|c| c.channel != "@seller1"),
        "other sellers invisible"
    );
    // Row shape: named fields, global namespace, not a default.
    let row = db
        .list_channels(None)
        .into_iter()
        .find(|c| c.channel == "@global")
        .unwrap();
    assert_eq!(row.seller_id, 0);
    assert!(!row.is_default);
    assert!(!row.created_at.is_empty());
}

#[test]
fn test_channels_remove_scoping() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rm.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.add_channel("@global", 0).unwrap();
    db.add_channel("@own", 1).unwrap();
    db.add_channel("@other", 2).unwrap();
    // A seller cannot remove the global row or another seller's row.
    assert!(db.remove_channel("@global", 1).is_err());
    assert!(db.remove_channel("@other", 1).is_err());
    assert!(
        db.remove_channel("@OTHER", 1).is_err(),
        "case-insensitive name, still scoped"
    );
    assert_eq!(
        db.list_channels(None).len(),
        3,
        "nothing deleted by cross-scope attempts"
    );
    // A seller removes their own row.
    assert!(db.remove_channel("@own", 1).is_ok());
    assert!(
        db.remove_channel("@own", 1).is_err(),
        "second remove is not-found"
    );
    // The owner (0) removes global rows only.
    assert!(db.remove_channel("@global", 0).is_ok());
    assert!(
        db.remove_channel("@other", 0).is_err(),
        "owner does not reach seller rows here"
    );
    assert_eq!(
        db.list_channels(None).len(),
        1,
        "only @other (seller 2) survives"
    );
    // Empty names are refused, not silently ignored.
    assert!(db.remove_channel("", 0).is_err());
}

#[test]
fn test_channels_rename_scoping_default_preserved_and_unique() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rename.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.add_channel("@global", 0).unwrap();
    db.set_default_channel("@global").unwrap();
    db.add_channel("@dup", 0).unwrap();
    db.add_channel("@own", 1).unwrap();
    db.add_channel("@other", 2).unwrap();

    // Cross-scope / unknown rows are refused.
    assert!(db.rename_channel(1, "@global", "@g2", false).is_err());
    assert!(db.rename_channel(1, "@other", "@o2", false).is_err());
    assert!(db.rename_channel(0, "@own", "@o2", true).is_err());
    assert!(db.rename_channel(0, "@missing", "@x", true).is_err());

    // Colliding with an existing channel (case-insensitive) is refused.
    let err = db.rename_channel(0, "@global", "@DUP", true).unwrap_err();
    assert!(err.contains("already exists"), "got: {err}");
    assert_eq!(
        db.list_channels(None)
            .into_iter()
            .filter(|c| c.channel.eq_ignore_ascii_case("@global"))
            .count(),
        1,
        "collision must not mutate"
    );

    // Renaming the default global channel preserves the default flag and name.
    assert!(db.rename_channel(0, "@global", "@renamed", true).is_ok());
    assert_eq!(db.default_channel().as_deref(), Some("@renamed"));
    let row = db
        .list_channels(None)
        .into_iter()
        .find(|c| c.channel == "@renamed")
        .unwrap();
    assert!(row.is_default, "is_default must be preserved");
    assert_eq!(row.seller_id, 0);

    // Seller-scoped rename works and is case-insensitive on the old name.
    assert!(db.rename_channel(1, "@OWN", "@owned2", false).is_ok());
    assert!(
        db.list_channels(Some(1))
            .iter()
            .any(|c| c.channel == "@owned2")
    );
    assert!(
        !db.list_channels(Some(1))
            .iter()
            .any(|c| c.channel == "@own")
    );

    // Empty names are refused.
    assert!(db.rename_channel(0, "@renamed", "", true).is_err());
    assert!(db.rename_channel(0, "", "@x", true).is_err());

    // Renaming is scoped by seller_id even when is_global differs.
    assert!(db.rename_channel(0, "@renamed", "@final", false).is_ok());
    assert!(db.list_channels(None).iter().any(|c| c.channel == "@final"));
}

#[test]
fn test_channels_default_switches_exactly_one_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    // Missing channels are upserted as new global defaults.
    db.set_default_channel("@a").unwrap();
    assert_eq!(db.default_channel().as_deref(), Some("@a"));
    db.add_channel("@b", 0).unwrap();
    // Switching: exactly one row carries the flag afterwards.
    db.set_default_channel("@b").unwrap();
    assert_eq!(db.default_channel().as_deref(), Some("@b"));
    let defaults = db
        .list_channels(None)
        .into_iter()
        .filter(|c| c.is_default)
        .count();
    assert_eq!(defaults, 1, "switching must clear every other is_default");
    // A seller-owned channel cannot become the global default.
    db.add_channel("@seller", 5).unwrap();
    let err = db.set_default_channel("@seller").unwrap_err();
    assert!(err.contains("belongs to a seller"), "got: {err}");
    assert_eq!(
        db.default_channel().as_deref(),
        Some("@b"),
        "refusal must not clobber"
    );
    // Re-flipping back to a global row works, still exactly one default.
    db.set_default_channel("@a").unwrap();
    assert_eq!(db.default_channel().as_deref(), Some("@a"));
    assert_eq!(
        db.list_channels(None)
            .into_iter()
            .filter(|c| c.is_default)
            .count(),
        1
    );
    // Empty names are rejected.
    assert!(db.set_default_channel("  ").is_err());
}

#[test]
fn test_default_channel_legacy_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-fallback.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    // Nothing anywhere: None.
    assert!(db.default_channel().is_none());
    // Legacy installs: the free-text `channel` config key is honored.
    db.set_config("channel", "@legacy");
    assert_eq!(db.default_channel().as_deref(), Some("@legacy"));
    // An empty legacy value is never resolved as a channel.
    db.set_config("channel", "");
    assert!(
        db.default_channel().is_none(),
        "empty legacy value is not a channel"
    );
    db.set_config("channel", "@legacy");
    // A table default wins over the legacy key.
    db.set_default_channel("@table").unwrap();
    assert_eq!(db.default_channel().as_deref(), Some("@table"));
    // Seller-owned rows never resolve as the global default, flag or not.
    db.add_channel("@sellerch", 7).unwrap();
    db.set_default_channel("@sellerch").unwrap_err();
    assert_eq!(db.default_channel().as_deref(), Some("@table"));
}

#[test]
fn test_channels_migration_seeds_from_legacy_config() {
    // Legacy database: only the config table with the `channel` key. The
    // one-time migration seeds the channels table with that value as the
    // single default global row.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
                "CREATE TABLE config(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO config(key,value,updated_at) VALUES('channel','@old','2024-01-01');",
            )
            .unwrap();
    }
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let rows = db.list_channels(None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].channel, "@old");
    assert_eq!(rows[0].seller_id, 0, "seed lands in the global namespace");
    assert!(rows[0].is_default, "seeded row is the default");
    assert_eq!(db.default_channel().as_deref(), Some("@old"));
    // Reopen (second startup): the config marker prevents a re-seed.
    drop(db);
    let db2 = Db::new(path.to_str().unwrap()).unwrap();
    assert_eq!(db2.list_channels(None).len(), 1);
    // Removing the channel afterwards must not resurrect it on restart.
    assert!(db2.remove_channel("@old", 0).is_ok());
    drop(db2);
    let db3 = Db::new(path.to_str().unwrap()).unwrap();
    assert!(db3.list_channels(None).is_empty());
    // A legacy db with an empty `channel` value seeds nothing.
    let empty_dir = tempfile::tempdir().unwrap();
    {
        let conn = Connection::open(empty_dir.path().join("e.db")).unwrap();
        conn.execute_batch(
                "CREATE TABLE config(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO config(key,value,updated_at) VALUES('channel','','2024-01-01');",
            )
            .unwrap();
    }
    let empty = Db::new(empty_dir.path().join("e.db").to_str().unwrap()).unwrap();
    assert!(empty.list_channels(None).is_empty());
    assert!(empty.default_channel().is_none());
    // A fresh database (no legacy key at all) seeds nothing either.
    let fresh = Db::new(dir.path().join("fresh.db").to_str().unwrap()).unwrap();
    assert!(fresh.list_channels(None).is_empty());
}

#[test]
fn test_claim_code_stable_while_unclaimed() {
    // Fresh install (no owner account, setup not completed): a claim code
    // is generated, stored, and served — and repeated startups (same Db
    // and a reopened Db) keep the SAME code while it stays unclaimed.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claim.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert!(!db.owner_account_exists());
    let first = db.ensure_claim_code().expect("code must be generated");
    assert_eq!(first.len(), 12, "gen_token(12) shape");
    assert_eq!(db.claim_code().as_deref(), Some(first.as_str()));
    assert_eq!(
        db.ensure_claim_code().as_deref(),
        Some(first.as_str()),
        "not regenerated within one run"
    );
    drop(db);
    let db2 = Db::new(path.to_str().unwrap()).unwrap();
    assert_eq!(
        db2.ensure_claim_code().as_deref(),
        Some(first.as_str()),
        "not regenerated across startups"
    );
}

#[tokio::test]
async fn test_claim_code_cleared_when_owner_exists() {
    // Once an owner account exists, the stored code is stale: startup
    // maintenance clears it and claim mode can never return.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claim-owner.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let code = db.ensure_claim_code().expect("pre-claim code exists");
    db.create_web_account("owner", "password123", "owner", None)
        .await
        .expect("owner account created");
    assert!(db.owner_account_exists());
    assert!(
        db.ensure_claim_code().is_none(),
        "no code printed once claimed"
    );
    assert!(
        db.claim_code().is_none(),
        "stale code cleared from the store"
    );
    assert_ne!(db.claim_code().as_deref(), Some(code.as_str()));
    // Repeated startups stay clean (no resurrection, no churn).
    drop(db);
    let db2 = Db::new(path.to_str().unwrap()).unwrap();
    assert!(db2.ensure_claim_code().is_none());
}

// ---- core coverage: config / web accounts / totp / api keys / mcp approvals / audit / plans / channels ----

#[test]
fn test_config_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cfg.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert!(db.get_config("foo").is_none());
    db.set_config("foo", "bar");
    assert_eq!(db.get_config("foo").as_deref(), Some("bar"));
    db.set_config("foo", "baz");
    assert_eq!(db.get_config("foo").as_deref(), Some("baz"));
}

#[test]
fn test_config_missing_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cfg2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert!(db.get_config("nope").is_none());
}

#[tokio::test]
async fn test_web_account_get_by_username_and_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wa.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_web_account("alice", "secret", "admin", Some(7))
        .await
        .unwrap();
    let (id, username, _hash, role, seller) = db.get_web_account_by_username("alice").unwrap();
    assert!(id > 0);
    assert_eq!(username, "alice");
    assert_eq!(role, "admin");
    assert_eq!(seller, Some(7));
    let by_id = db.get_web_account_by_id(id).unwrap();
    assert_eq!(by_id.0, id);
    assert_eq!(by_id.1, "alice");
    assert!(db.get_web_account_by_username("nope").is_none());
    assert!(db.get_web_account_by_id(9999).is_none());
}

#[tokio::test]
async fn test_web_account_verify_password_unknown_user() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vwp.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_web_account("alice", "secret", "user", None)
        .await
        .unwrap();
    assert!(db.verify_web_password("alice", "secret").await.is_some());
    assert!(db.verify_web_password("alice", "nope").await.is_none());
    assert!(db.verify_web_password("ghost", "secret").await.is_none());
}

#[tokio::test]
async fn test_totp_set_get_enable_disable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("totp.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_web_account("alice", "pw", "admin", None)
        .await
        .unwrap();
    let (id, _u, _h, _r, _s) = db.get_web_account_by_username("alice").unwrap();

    db.set_totp_secret(id, Some("SECRET")).unwrap();
    let (sec, enabled, last) = db.get_totp_state(id);
    assert_eq!(sec.as_deref(), Some("SECRET"));
    assert!(!enabled);
    assert!(last.is_none());

    db.enable_totp(id, "SECRET2").unwrap();
    let (sec, enabled, _) = db.get_totp_state(id);
    assert_eq!(sec.as_deref(), Some("SECRET2"));
    assert!(enabled);

    db.disable_totp(id).unwrap();
    let (sec, enabled, last) = db.get_totp_state(id);
    assert!(sec.is_none());
    assert!(!enabled);
    assert!(last.is_none());
}

#[tokio::test]
async fn test_totp_set_secret_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("totpnone.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_web_account("alice", "pw", "admin", None)
        .await
        .unwrap();
    let (id, _u, _h, _r, _s) = db.get_web_account_by_username("alice").unwrap();
    db.set_totp_secret(id, Some("X")).unwrap();
    db.set_totp_secret::<String>(id, None).unwrap();
    let (sec, enabled, _) = db.get_totp_state(id);
    assert!(sec.is_none());
    assert!(!enabled);
}

#[test]
fn test_api_key_create_get_list_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("apikey.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let raw = db.create_api_key(1, Some("k1"));
    assert_eq!(raw.len(), 48);
    let hash = sha256_hex(&raw);
    let row = db.get_api_key_by_hash(&hash).unwrap();
    assert_eq!(row.1, 1);
    assert_eq!(row.2.as_deref(), Some("k1"));
    assert_eq!(row.5, 0);
    let list = db.list_api_keys(1);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].2.as_deref(), Some("k1"));
    assert!(db.list_api_keys(2).is_empty());
    db.revoke_api_key(row.0);
    let row2 = db.get_api_key_by_hash(&hash).unwrap();
    assert_eq!(row2.5, 1);
    db.revoke_api_key(&raw);
}

#[test]
fn test_mcp_approval_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcpapp.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let id = db.mcp_create_approval("tool", "{\"a\":1}", &expires);
    assert!(id > 0);
    let row = db.mcp_get_approval(id).unwrap();
    assert_eq!(row.1, "tool");
    assert_eq!(row.2, "{\"a\":1}");
    assert_eq!(row.3, "pending");
    let pending = db.mcp_list_approvals(Some("pending"), 10);
    assert_eq!(pending.len(), 1);
    assert!(db.mcp_decide_approval(id, "approved"));
    assert!(!db.mcp_decide_approval(id, "denied"));
    let row2 = db.mcp_get_approval(id).unwrap();
    assert_eq!(row2.3, "approved");
    assert!(row2.6.is_some());
}

#[test]
fn test_mcp_approval_deny_and_expire() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcpapp2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let expires = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let id = db.mcp_create_approval("tool2", "{}", &expires);
    let pending = db.mcp_list_approvals(Some("pending"), 10);
    assert!(!pending.iter().any(|r| r.0 == id));
    let row = db.mcp_get_approval(id).unwrap();
    assert_eq!(row.3, "expired");
    assert!(!db.mcp_decide_approval(id, "approved"));
}

#[test]
fn test_mcp_approval_list_all() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcpall.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    db.mcp_create_approval("t1", "{}", &expires);
    db.mcp_create_approval("t2", "{}", &expires);
    let all = db.mcp_list_approvals(None, 10);
    assert_eq!(all.len(), 2);
}

#[test]
fn test_audit_log_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.log_action(1, "admin", "create", "plan", "42", Some("{\"x\":1}"));
    db.log_action(2, "user", "delete", "plan", "7", None);
    let rows = db.list_audit_log(10);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, 2);
    assert_eq!(rows[0].3, "delete");
    assert_eq!(rows[1].1, 1);
    assert_eq!(rows[1].3, "create");
}

#[test]
fn test_audit_log_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auditlimit.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    for i in 0..5 {
        db.log_action(i, "r", "a", "t", "1", None);
    }
    assert_eq!(db.list_audit_log(2).len(), 2);
    assert_eq!(db.list_audit_log(100).len(), 5);
}

#[test]
fn test_plan_add_get_list() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plan2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.add_plan(1, "P", "10", "USDT", 7, Some("@chan"));
    let plans = db.list_plans(Some(1));
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].0, "P");
    let got = db.get_plan(1, "P").unwrap();
    assert!(got.0 > 0);
    assert_eq!(got.3, "P");
    assert_eq!(got.5, Some("@chan".to_string()));
}

#[test]
fn test_channels_add_list_set_default_default_channel() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chan.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.add_channel("@c1", 0).unwrap();
    db.add_channel("@c2", 0).unwrap();
    let list = db.list_channels(None);
    assert_eq!(list.len(), 2);
    db.set_default_channel("@c2").unwrap();
    assert_eq!(db.default_channel().as_deref(), Some("@c2"));
    let scoped = db.list_channels(Some(5));
    assert_eq!(scoped.len(), 2);
}

#[test]
fn list_channels_denied_scope_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chan-denied.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.add_channel("@global", 0).unwrap();
    db.add_channel("@own", 5).unwrap();

    assert_eq!(db.list_channels(None).len(), 2);
    assert_eq!(db.list_channels(Some(5)).len(), 2);
    assert!(
        db.list_channels(Some(crate::roles::TenantScope::DENIED_ID))
            .is_empty(),
        "the denied sentinel must not widen to the global namespace"
    );
}

#[test]
fn test_channel_default_fallback_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chanfb.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.set_config("channel", "@legacy");
    assert_eq!(db.default_channel().as_deref(), Some("@legacy"));
}

proptest! {
    #[test]
    fn prop_plan_create_list(seller_id in 1i64..101i64, name in "[a-z]{3,8}") {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.add_plan(seller_id, &name, "1", "USDT", 7, None);
        let plans = db.list_plans(Some(seller_id));
        prop_assert!(!plans.is_empty());
    }

    #[test]
    fn prop_code_uniqueness(_n in 0u8..1u8) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("code.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.add_plan(1, "P", "1", "USDT", 7, None);
        let pid = db.get_plan(1, "P").unwrap().0;
        let mut codes = std::collections::HashSet::new();
        for _ in 0..10 {
            let c = db.create_code(1, Some(pid), Some(7), 1, None, 12);
            codes.insert(c);
        }
        prop_assert_eq!(codes.len(), 10);
    }

    #[test]
    fn prop_subscription_roundtrip(seller_id in 1i64..101i64, user_id in 1i64..1001i64) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        let id = db.create_subscription(seller_id, user_id, None, "@chan", 7, None, None);
        prop_assert!(id > 0);
        prop_assert!(db.user_has_active_sub(seller_id, user_id, "@chan"));
    }
}

#[test]
fn promo_create_get_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("promo.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_promo_code(
        1,
        "SAVE10",
        "percent",
        10.0,
        Some("USDT"),
        Some(5),
        0.0,
        "all",
        None,
        None,
    )
    .unwrap();
    let p = db.get_promo_code(1, "SAVE10").unwrap();
    assert_eq!(p.0, "SAVE10");
    assert_eq!(p.2, "percent");
    assert!((p.3 - 10.0).abs() < f64::EPSILON);
    assert!(db.revoke_promo_code(1, "SAVE10"));
    let p2 = db.get_promo_code(1, "SAVE10").unwrap();
    assert_eq!(p2.11, 0); // active=0
}

#[test]
fn promo_seller_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("promo2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_promo_code(1, "CODE", "fixed", 5.0, None, None, 0.0, "all", None, None)
        .unwrap();
    assert!(db.get_promo_code(2, "CODE").is_none());
    assert!(db.list_promo_codes(2).is_empty());
}

#[test]
fn promo_redeem_expired_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("promo3.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let past = "2020-01-01T00:00:00Z".to_string();
    db.create_promo_code(
        1,
        "OLD",
        "percent",
        10.0,
        None,
        None,
        0.0,
        "all",
        None,
        Some(&past),
    )
    .unwrap();
    let r = db.redeem_promo_code(1, "OLD", 100, None, 50.0);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("expired"));
}

#[test]
fn promo_redeem_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("promo4.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_promo_code(
        1,
        "ONCE",
        "fixed",
        2.0,
        None,
        Some(1),
        0.0,
        "all",
        None,
        None,
    )
    .unwrap();
    assert!(db.redeem_promo_code(1, "ONCE", 100, None, 10.0).is_ok());
    let r = db.redeem_promo_code(1, "ONCE", 101, None, 10.0);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("cap"));
}

#[test]
fn promo_redeem_discount_math() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("promo5.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_promo_code(
        1, "PCT", "percent", 20.0, None, None, 0.0, "all", None, None,
    )
    .unwrap();
    let d = db.redeem_promo_code(1, "PCT", 100, None, 50.0).unwrap();
    assert!((d - 10.0).abs() < 1e-9);
    db.create_promo_code(1, "FIX", "fixed", 7.0, None, None, 0.0, "all", None, None)
        .unwrap();
    let d2 = db.redeem_promo_code(1, "FIX", 100, None, 50.0).unwrap();
    assert!((d2 - 7.0).abs() < 1e-9);
}

#[test]
fn cart_add_total_clear() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cart.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.cart_add_item(42, 1, "product", 7, 2).unwrap();
    db.cart_add_item(42, 1, "product", 7, 1).unwrap(); // increments
    let items = db.cart_get_items(42, 1);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].3, 3); // qty 3
    assert_eq!(db.cart_count(42, 1), 3);
    db.cart_clear(42, 1);
    assert!(db.cart_get_items(42, 1).is_empty());
}

#[test]
fn cart_update_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cart2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.cart_add_item(42, 1, "plan", 3, 1).unwrap();
    assert!(db.cart_update_qty(42, 1, "plan", 3, 5));
    let items = db.cart_get_items(42, 1);
    assert_eq!(items[0].3, 5);
    assert!(db.cart_remove_item(42, 1, "plan", 3));
    assert!(db.cart_get_items(42, 1).is_empty());
}

#[test]
fn cart_seller_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cart3.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.cart_add_item(42, 1, "product", 7, 1).unwrap();
    assert!(db.cart_get_items(42, 2).is_empty());
}

#[test]
fn referral_code_reuse_and_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ref.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let c1 = db.create_referral_code(1, 100);
    let c2 = db.create_referral_code(1, 100);
    assert_eq!(c1, c2);
    assert_eq!(db.get_referrer_by_code(1, &c1), Some(100));
}

#[test]
fn referral_self_blocked_and_unique_referee() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ref2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert!(db.create_referral(1, 5, 5, "X").is_err());
    db.create_referral(1, 5, 6, "X").unwrap();
    // duplicate referee rejected
    assert!(db.create_referral(1, 7, 6, "Y").is_err());
}

#[test]
fn referral_reward_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ref3.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_referral(1, 5, 6, "X").unwrap();
    assert!(db.mark_referral_rewarded(1, 6, 10.0, "credit"));
    assert!(!db.mark_referral_rewarded(1, 6, 10.0, "credit"));
    let ls = db.list_referrals(1, Some(5));
    assert_eq!(ls.len(), 1);
    assert_eq!(ls[0].4, "rewarded");
}

#[test]
fn bonus_credit_debit_balance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bonus.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert_eq!(db.bonus_balance(1, 42), 0);
    db.bonus_credit(1, 42, 1000, "signup");
    assert_eq!(db.bonus_balance(1, 42), 1000);
    let newbal = db.bonus_debit(1, 42, 300, "redeem").unwrap();
    assert_eq!(newbal, 700);
    assert!(db.bonus_debit(1, 42, 10000, "x").is_err());
}

#[test]
fn bonus_seller_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bonus2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.bonus_credit(1, 42, 500, "x");
    assert_eq!(db.bonus_balance(2, 42), 0);
}

#[test]
fn customer_upsert_and_purchase() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cust.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.upsert_customer(1, 42);
    assert!(db.get_customer(1, 42).is_some());
    db.record_customer_purchase(1, 42, 10.0);
    let c = db.get_customer(1, 42).unwrap();
    assert!((c.6 - 10.0).abs() < 1e-9);
    assert_eq!(c.7, 1);
    db.update_customer_notes(1, 42, "vip");
    db.update_customer_tags(1, 42, "gold");
    let c2 = db.get_customer(1, 42).unwrap();
    assert_eq!(c2.5.as_deref(), Some("vip"));
    assert_eq!(c2.4.as_deref(), Some("gold"));
}

#[test]
fn customer_seller_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cust2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.upsert_customer(1, 42);
    assert!(db.get_customer(2, 42).is_none());
    assert!(db.list_customers(2, None, None).is_empty());
}

#[test]
fn customer_filter_by_status() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cust3.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.upsert_customer(1, 1);
    db.upsert_customer(1, 2);
    db.set_customer_status(1, 2, "blocked");
    let active = db.list_customers(1, None, Some("active"));
    assert_eq!(active.len(), 1);
    let blocked = db.list_customers(1, None, Some("blocked"));
    assert_eq!(blocked.len(), 1);
}

#[test]
fn ticket_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tkt.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let id = db.create_ticket(1, 42, "help");
    assert!(id > 0);
    let t = db.get_ticket(id).unwrap();
    assert_eq!(t.4, "open");
    db.add_ticket_message(id, "buyer", 42, "hi");
    db.add_ticket_message(id, "seller", 7, "hello");
    let msgs = db.list_ticket_messages(id);
    assert_eq!(msgs.len(), 2);
    assert!(db.set_ticket_status(id, "resolved"));
    let t2 = db.get_ticket(id).unwrap();
    assert_eq!(t2.4, "resolved");
}

#[test]
fn ticket_seller_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tkt2.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let id = db.create_ticket(1, 42, "help");
    let ts = db.list_tickets(Some(2), None, None);
    assert!(ts.is_empty());
    let ts2 = db.list_tickets(Some(1), None, None);
    assert_eq!(ts2.len(), 1);
    let _ = id;
}

#[test]
fn cart_abandonment_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ab.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.mark_cart_abandoned(42, 1);
    let lst = db.list_abandoned_carts(0);
    assert_eq!(lst.len(), 1);
    db.mark_cart_reminded(42, 1);
    let lst2 = db.list_abandoned_carts(0);
    assert_eq!(lst2[0].3, 1);
    db.mark_cart_recovered(42, 1);
    let lst3 = db.list_abandoned_carts(0);
    assert!(lst3.is_empty());
}

#[test]
fn reorder_schedule_and_due() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("re.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.set_reorder_schedule(42, 1, 7, 30);
    let due = db.list_due_reorders();
    // next reminder is 30 days out, not due now
    assert!(due.is_empty() || due.iter().all(|r| r.1 == 42));
    assert!(db.disable_reorder(42, 1, 7));
}

#[test]
fn review_rating_range_and_avg() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rev.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert!(db.create_review(42, 1, 7, 6, None).is_err());
    let id = db.create_review(42, 1, 7, 5, Some("great")).unwrap();
    assert!(id > 0);
    // duplicate rejected
    assert!(db.create_review(42, 1, 7, 4, None).is_err());
    db.approve_review(id);
    let avg = db.avg_rating(1, 7).unwrap();
    assert!((avg - 5.0).abs() < 1e-9);
    let revs = db.list_reviews(1, 7, true);
    assert_eq!(revs.len(), 1);
}

#[test]
fn analytics_empty_seller() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("an.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert_eq!(db.analytics_revenue_total(1), Ok(0.0));
    assert_eq!(db.analytics_invoice_counts(1), (0, 0));
    assert_eq!(db.analytics_aov(1), Ok(0.0));
    assert_eq!(db.analytics_repeat_buyers(1), (0, 0));
    assert_eq!(db.analytics_abandoned_cart_rate(1), (0, 0));
    assert_eq!(db.analytics_customer_count(1), 0);
}

#[test]
fn compliance_export_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("comp.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.upsert_customer(1, 42);
    db.record_customer_purchase(1, 42, 5.0);
    db.bonus_credit(1, 42, 100, "x");
    let json = db.export_buyer_data(1, 42);
    assert!(json.contains("\"user_id\": 42"));
    assert!(json.contains("bonus_balance_minor"));
    let n = db.delete_buyer_data(1, 42);
    assert!(n >= 1);
    assert!(db.get_customer(1, 42).is_none());
    assert_eq!(db.bonus_balance(1, 42), 0);
}

#[test]
fn update_bot_settings_changes_owner_channel_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bot_settings.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let bot_id = db.create_bot("bot-key-1", 100, None, "");
    assert!(bot_id > 0);

    // Update all three fields at once (owner, default channel, enabled).
    assert!(db.update_bot_settings(bot_id, 200, Some(7), false));
    let bot = db.get_bot(bot_id).expect("bot exists");
    assert_eq!(bot.owner_user_id, 200);
    assert_eq!(bot.default_channel_id, Some(7));
    assert!(!bot.enabled);

    // Back to enabled with the channel cleared.
    assert!(db.update_bot_settings(bot_id, 201, None, true));
    let bot = db.get_bot(bot_id).expect("bot exists");
    assert_eq!(bot.owner_user_id, 201);
    assert_eq!(bot.default_channel_id, None);
    assert!(bot.enabled);

    // Unknown bot id must report no rows updated.
    assert!(!db.update_bot_settings(999_999, 1, Some(1), true));
}

/// The v1.7 `bots.name` migration covers legacy databases: a pre-migration
/// bots table (no `name` column) gains the column on `Db::new`, existing
/// rows read back with the DEFAULT '' (show the key), and the setter works
/// on the migrated row.
#[test]
fn bots_name_migration_covers_legacy_databases() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bots_legacy.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE bots (id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL UNIQUE, username TEXT, owner_user_id INTEGER NOT NULL, default_channel_id INTEGER, enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bots(key, username, owner_user_id, created_at) VALUES('proxy26_bot', '@proxy26_bot', 100, '2026-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
    }
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let bot = db.get_bot_by_key("proxy26_bot").expect("legacy row kept");
    assert_eq!(bot.name, "", "DEFAULT '' fills existing rows: show the key");

    // The migrated column is writable through the setter.
    db.set_bot_name(bot.id, "Proxy 26").unwrap();
    assert_eq!(db.get_bot(bot.id).unwrap().name, "Proxy 26");
}

/// `backfill_bots_at_startup` carries each `BotConfig.name` (whitespace
/// trimmed) into the freshly created row, for every configured bot.
#[test]
fn backfill_bots_at_startup_carries_config_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bots_backfill.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let bots = vec![
        crate::config::BotConfig {
            key: "proxy26_bot".to_string(),
            name: "  Proxy 26  ".to_string(),
            token: String::new(),
            owner_id: 100,
            default_channel: None,
            token_env: None,
            bot_type: crate::config::DEFAULT_BOT_TYPE.to_string(),
        },
        crate::config::BotConfig {
            key: "shop_bot".to_string(),
            name: String::new(),
            token: String::new(),
            owner_id: 101,
            default_channel: None,
            token_env: None,
            bot_type: crate::config::DEFAULT_BOT_TYPE.to_string(),
        },
    ];
    db.backfill_bots_at_startup(&bots);
    assert_eq!(
        db.get_bot_by_key("proxy26_bot").unwrap().name,
        "Proxy 26",
        "config name carried in, trimmed"
    );
    assert_eq!(
        db.get_bot_by_key("shop_bot").unwrap().name,
        "",
        "empty config name stays empty (show the key)"
    );
}

/// The multibot-foundation `bot_type` migration is additive: a legacy
/// database with no such column gains it on `Db::new`, existing rows read
/// back as 'shop_bot' via the NOT NULL DEFAULT (no destructive rewrite),
/// and the new column is writable afterwards. Reopening is idempotent.
#[test]
fn bots_bot_type_migration_backfills_legacy_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bots_legacy_type.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE bots (id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL UNIQUE, username TEXT, owner_user_id INTEGER NOT NULL, default_channel_id INTEGER, enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, name TEXT NOT NULL DEFAULT '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bots(key, owner_user_id, created_at) VALUES('legacy_a', 100, '2026-01-01T00:00:00+00:00'), ('legacy_b', 101, '2026-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
    }
    let db = Db::new(path.to_str().unwrap()).unwrap();
    for key in ["legacy_a", "legacy_b"] {
        assert_eq!(
            db.get_bot_by_key(key).expect("legacy row kept").bot_type,
            crate::config::DEFAULT_BOT_TYPE,
            "NOT NULL DEFAULT backfills existing rows"
        );
    }
    drop(db);
    let db = Db::new(path.to_str().unwrap()).unwrap();
    assert_eq!(
        db.list_bots_of_type("shop_bot").len(),
        2,
        "idempotent reopen"
    );

    let id = db.get_bot_by_key("legacy_a").unwrap().id;
    db.set_bot_type(id, "movie_bot").unwrap();
    assert_eq!(db.get_bot(id).unwrap().bot_type, "movie_bot");
    assert_eq!(db.list_bots_of_type("movie_bot").len(), 1);
    assert_eq!(db.list_bots_of_type("shop_bot").len(), 1);
}

/// Typed listing is parameterized (a quote cannot widen the query), returns
/// an empty vec for unknown/disabled types, and the enabled-only variant
/// mirrors `list_bots_enabled`. `create_bot` still defaults to `shop_bot`.
#[test]
fn bot_type_typed_listing_is_parameterized_and_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bot_type_list.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let shop = db.create_bot("shop-1", 100, None, "");
    let movie = db.create_bot_with_type("movie-1", 101, None, "", "movie_bot");
    let movie2 = db.create_bot_with_type("movie-2", 102, None, "", "movie_bot");
    assert!(shop > 0 && movie > 0 && movie2 > 0);

    assert_eq!(db.get_bot(shop).unwrap().bot_type, "shop_bot");
    assert_eq!(db.get_bot_by_key("movie-1").unwrap().bot_type, "movie_bot");
    assert!(db.list_bots().iter().any(|b| b.bot_type == "movie_bot"));

    assert_eq!(db.list_bots_of_type("movie_bot").len(), 2);
    assert_eq!(db.list_bots_of_type("shop_bot").len(), 1);
    assert!(db.list_bots_of_type("reply_bot").is_empty());
    assert!(db.list_bots_of_type("movie_bot' OR 1=1 --").is_empty());

    db.set_bot_enabled(movie2, false);
    assert_eq!(db.list_bots_of_type_enabled("movie_bot").len(), 1);
    assert_eq!(db.list_bots_of_type("movie_bot").len(), 2);

    // Empty/whitespace is normalized to the default; unknown id refused.
    db.set_bot_type(movie, "  ").unwrap();
    assert_eq!(db.get_bot(movie).unwrap().bot_type, "shop_bot");
    assert!(db.set_bot_type(999_999, "movie_bot").is_err());
}

/// The seller-scoped listing uses the same COALESCE(bot_id, primary) rule as
/// `sellers_for_bot`, binds both parameters, and never leaks another
/// seller's typed bots.
#[test]
fn bot_type_listing_for_seller_is_seller_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bot_type_seller.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    db.create_bot("primary", 1, None, "");
    let movie = db.create_bot_with_type("movie", 2, None, "", "movie_bot");
    db.upsert_seller(10, Some("s10"), None); // NULL bot_id -> primary
    db.upsert_seller_bot_id(11, Some(movie));
    db.upsert_seller(12, Some("s12"), None); // NULL bot_id -> primary

    assert_eq!(db.list_bots_of_type_for_seller(10, "shop_bot").len(), 1);
    assert!(db.list_bots_of_type_for_seller(10, "movie_bot").is_empty());
    assert_eq!(db.list_bots_of_type_for_seller(11, "movie_bot").len(), 1);
    assert!(db.list_bots_of_type_for_seller(11, "shop_bot").is_empty());
    assert!(db.list_bots_of_type_for_seller(999, "shop_bot").is_empty());
    assert!(
        db.list_bots_of_type_for_seller(10, "movie_bot' OR 1=1 --")
            .is_empty()
    );
}

/// A keyed primary must win over a lower id so NULL-bot_id sellers do not
/// drift to an unrelated bot when more bots are added.
#[test]
fn sellers_without_bot_id_follow_keyed_primary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keyed_primary_seller.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let older = db.create_bot("alpha", 1, None, "");
    let primary = db.create_bot("primary", 2, None, "");
    assert!(primary > older);
    db.upsert_seller(10, Some("s10"), None);

    assert_eq!(db.sellers_for_bot(primary), vec![10]);
    assert!(db.sellers_for_bot(older).is_empty());
    let listed = db.list_bots_of_type_for_seller(10, "shop_bot");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, primary);

    let fallback_path = dir.path().join("no_keyed_primary_seller.db");
    let fallback_db = Db::new(fallback_path.to_str().unwrap()).unwrap();
    let only = fallback_db.create_bot("solo", 1, None, "");
    fallback_db.upsert_seller(20, Some("s20"), None);
    assert_eq!(fallback_db.sellers_for_bot(only), vec![20]);
}

/// Startup backfill carries each configured `BotConfig.bot_type` into the
/// freshly created row, not just its name.
#[test]
fn backfill_bots_at_startup_carries_config_bot_type() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bots_backfill_type.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let bots = vec![
        crate::config::BotConfig {
            key: "movie-bot".to_string(),
            name: "Movie".to_string(),
            token: String::new(),
            owner_id: 100,
            default_channel: None,
            token_env: None,
            bot_type: "movie_bot".to_string(),
        },
        crate::config::BotConfig {
            key: "shop-bot".to_string(),
            name: String::new(),
            token: String::new(),
            owner_id: 101,
            default_channel: None,
            token_env: None,
            bot_type: crate::config::DEFAULT_BOT_TYPE.to_string(),
        },
    ];
    db.backfill_bots_at_startup(&bots);
    assert_eq!(
        db.get_bot_by_key("movie-bot").unwrap().bot_type,
        "movie_bot"
    );
    assert_eq!(db.get_bot_by_key("shop-bot").unwrap().bot_type, "shop_bot");
}

/// `set_bot_name` persists the trimmed name, allows the empty string (show
/// the key) and refuses unknown bot ids. No length cap is enforced at the
/// Db layer — sibling bot setters validate no string bounds — so an
/// over-long name is stored as-is (trimmed); the 64-char console bound is
/// an input-validation concern above this layer.
#[test]
fn set_bot_name_trims_persists_and_allows_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bot_set_name.db");
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let bot_id = db.create_bot("bot-key-1", 100, None, "");
    assert!(bot_id > 0);

    // Surrounding whitespace is trimmed; the trimmed value persists.
    db.set_bot_name(bot_id, "  My Bot  ").unwrap();
    assert_eq!(db.get_bot(bot_id).unwrap().name, "My Bot");

    // Empty (or whitespace-only) is allowed: clear back to "show the key".
    db.set_bot_name(bot_id, "   ").unwrap();
    assert_eq!(db.get_bot(bot_id).unwrap().name, "");

    // No length cap at this layer: an 80-char name persists unchanged.
    let long = "x".repeat(80);
    db.set_bot_name(bot_id, &long).unwrap();
    assert_eq!(db.get_bot(bot_id).unwrap().name, long);

    // Unknown bot id: honest Err, nothing to update.
    assert!(db.set_bot_name(999_999, "Ghost").is_err());
}

#[cfg(test)]
mod moderation_tests {
    use super::*;

    fn test_db(name: &str) -> Db {
        let dir = tempfile::tempdir().unwrap();
        // Keep the directory alive for the test (Db::new opens the file by
        // path); `keep` is the non-deprecated form of the old `into_path`.
        let path = dir.keep().join(name);
        Db::new(path.to_str().unwrap()).unwrap()
    }

    #[test]
    fn moderation_seller_isolation() {
        let db = test_db("mod_seller.db");
        let (a, b) = (11_i64, 22_i64);

        assert!(db.add_blocked_word(a, "spam", "hide", true));
        assert!(db.add_allow_word(a, "legit"));
        // Seller B sees none of seller A's rows.
        assert!(db.list_blocked_words(b).is_empty());
        assert!(db.get_blocked_word(b, "spam").is_none());
        assert!(db.list_allow_words(b).is_empty());
        // Seller B cannot mutate or delete A's rows by name/id.
        assert!(!db.set_blocked_word_mode(b, "spam", "warn"));
        assert!(!db.set_blocked_word_enabled(b, "spam", false));
        assert!(!db.remove_blocked_word(b, "spam"));
        assert!(!db.remove_allow_word(b, "legit"));
        assert_eq!(db.get_blocked_word(a, "spam").unwrap().mode, "hide");

        // Appeals and decisions are seller-scoped too.
        let appeal = db
            .create_moderation_appeal(a, None, None, "opaque:1", Some("please review"))
            .unwrap();
        db.log_moderation_decision(
            a,
            None,
            Some("spam"),
            "hide",
            Some("matched"),
            Some("opaque:1"),
        )
        .unwrap();
        assert!(db.get_moderation_appeal(b, appeal).is_none());
        assert_eq!(db.get_moderation_appeal(a, appeal).unwrap().seller_id, a);
        assert!(!db.set_moderation_appeal_status(b, appeal, "approved"));
        assert!(db.set_moderation_appeal_status(a, appeal, "approved"));
        assert_eq!(
            db.get_moderation_appeal(a, appeal).unwrap().status,
            "approved"
        );
        assert!(db.list_moderation_appeals(b, None).is_empty());
        assert_eq!(db.list_moderation_appeals(a, None).len(), 1);
        assert!(db.list_moderation_decisions(b, 10).is_empty());
        assert_eq!(db.list_moderation_decisions(a, 10).len(), 1);
    }

    #[test]
    fn moderation_bot_override_scoped_by_both_ids() {
        let db = test_db("mod_bot_scope.db");
        let (a, b) = (101_i64, 202_i64);
        let bot_a1 = db.create_bot("mod-bot-a1", a, None, "");
        let bot_a2 = db.create_bot("mod-bot-a2", a, None, "");
        let bot_b1 = db.create_bot("mod-bot-b1", b, None, "");

        // Inheritance is the default for every scope.
        assert!(db.bot_override_inherits(a, bot_a1));
        // Adding a custom term activates custom rules for (a, bot_a1) ONLY.
        assert!(db.add_bot_override_term(a, bot_a1, "spoiler", "hide"));
        assert!(!db.bot_override_inherits(a, bot_a1));
        assert!(db.bot_override_inherits(a, bot_a2));
        assert!(db.bot_override_inherits(b, bot_b1));
        assert_eq!(db.list_bot_override_terms(a, bot_a1).len(), 1);
        assert!(db.list_bot_override_terms(a, bot_a2).is_empty());
        assert!(db.list_bot_override_terms(b, bot_b1).is_empty());

        // Same bot id under a different seller, and a different bot under the
        // same seller, are disjoint namespaces for reads AND writes.
        assert!(!db.remove_bot_override_term(a, bot_a2, "spoiler"));
        assert!(!db.remove_bot_override_term(b, bot_b1, "spoiler"));
        assert!(db.get_bot_override_term(a, bot_a1, "spoiler").is_some());
        let pa = db.resolve_effective_policy(a, Some(bot_a1));
        let pb = db.resolve_effective_policy(a, Some(bot_a2));
        assert!(pa.bot_override_active);
        assert!(!pb.bot_override_active);
        assert_eq!(pa.terms.len(), 1);
        assert!(pb.terms.is_empty());

        // Restoring inheritance is non-destructive: terms stay stored but are
        // ignored while inheriting.
        assert!(db.set_bot_override_inherits(a, bot_a1, true));
        assert!(db.bot_override_inherits(a, bot_a1));
        assert_eq!(db.list_bot_override_terms(a, bot_a1).len(), 1);
        assert!(
            db.resolve_effective_policy(a, Some(bot_a1))
                .terms
                .is_empty()
        );

        // Removal is scoped by both ids.
        assert!(db.remove_bot_override_term(a, bot_a1, "spoiler"));
        assert!(db.list_bot_override_terms(a, bot_a1).is_empty());
    }

    #[test]
    fn moderation_precedence_stricter_only_allowlist_and_floor() {
        // Floor 'hide' with a baseline term: nothing may be weaker than hide,
        // and the allow-list may not delete a floor term.
        let floor_terms = vec![ModerationTermInput {
            term: "floorword".into(),
            mode: "hide".into(),
        }];
        let seller = vec![
            ModerationTermInput {
                term: "floorword".into(),
                mode: "warn".into(),
            },
            ModerationTermInput {
                term: "soft".into(),
                mode: "warn".into(),
            },
            ModerationTermInput {
                term: "mid".into(),
                mode: "review".into(),
            },
            ModerationTermInput {
                term: "allowed".into(),
                mode: "hide".into(),
            },
        ];
        let bot = vec![
            ModerationTermInput {
                term: "floorword".into(),
                mode: "warn".into(),
            },
            ModerationTermInput {
                term: "extra".into(),
                mode: "review".into(),
            },
        ];
        let allow = vec![
            "allowed".to_string(),
            "floorword".to_string(),
            "extra".to_string(),
        ];
        let effective = apply_moderation_precedence("hide", &floor_terms, &seller, &bot, &allow);
        let mode_of = |t: &str| {
            effective
                .iter()
                .find(|e| e.term == t)
                .map(|e| e.mode.as_str())
        };
        assert_eq!(mode_of("allowed"), None); // allow-list wins over seller
        assert_eq!(mode_of("extra"), None); // allow-list wins over bot
        assert_eq!(mode_of("floorword"), Some("hide")); // floor term survives allow
        assert_eq!(mode_of("soft"), Some("hide")); // seller term raised to floor
        assert_eq!(mode_of("mid"), Some("hide")); // seller term raised to floor
        assert_eq!(effective.len(), 3);
        assert!(effective.windows(2).all(|w| w[0].term < w[1].term)); // deterministic

        // Floor 'warn' does not force every term to hide: here the stricter-only
        // rule between seller and bot layers is exercised directly.
        let seller = vec![
            ModerationTermInput {
                term: "s1".into(),
                mode: "review".into(),
            },
            ModerationTermInput {
                term: "s2".into(),
                mode: "warn".into(),
            },
        ];
        let bot = vec![
            ModerationTermInput {
                term: "s1".into(),
                mode: "warn".into(),
            }, // weaker -> stays review
            ModerationTermInput {
                term: "s2".into(),
                mode: "hide".into(),
            }, // stricter -> raised to hide
            ModerationTermInput {
                term: "s3".into(),
                mode: "review".into(),
            },
        ];
        let effective = apply_moderation_precedence("warn", &[], &seller, &bot, &[]);
        let mode_of = |t: &str| {
            effective
                .iter()
                .find(|e| e.term == t)
                .map(|e| e.mode.as_str())
        };
        assert_eq!(mode_of("s1"), Some("review"));
        assert_eq!(mode_of("s2"), Some("hide"));
        assert_eq!(mode_of("s3"), Some("review"));
    }

    #[test]
    fn moderation_db_resolution_wiring() {
        let db = test_db("mod_resolve.db");
        let seller = 303_i64;
        let bot = db.create_bot("mod-resolve", seller, None, "");
        assert!(db.add_blocked_word(seller, "spam", "warn", true));
        assert!(db.add_blocked_word(seller, "disabled", "hide", false));
        assert!(db.add_allow_word(seller, "spam"));
        assert!(db.add_bot_override_term(seller, bot, "bot-only", "review"));
        // Adding a custom term auto-activates "Custom rules for this bot";
        // restore inheritance to exercise the inherited view first.
        assert!(db.set_bot_override_inherits(seller, bot, true));

        // While inheriting, bot terms are ignored; allow-list removes "spam";
        // the disabled word is skipped.
        let inherited = db.resolve_effective_policy(seller, Some(bot));
        assert!(!inherited.bot_override_active);
        assert!(inherited.terms.is_empty());

        // Activating the override brings in the bot's stricter local rule.
        assert!(db.set_bot_override_inherits(seller, bot, false));
        let active = db.resolve_effective_policy(seller, Some(bot));
        assert!(active.bot_override_active);
        assert_eq!(active.floor_mode, MODERATION_FLOOR_MODE);
        let mode_of = |t: &str| {
            active
                .terms
                .iter()
                .find(|e| e.term == t)
                .map(|e| e.mode.as_str())
        };
        assert_eq!(mode_of("bot-only"), Some("review"));
        assert_eq!(mode_of("spam"), None);
        assert_eq!(mode_of("disabled"), None);

        // No bot scope: never activates a bot override.
        let global = db.resolve_effective_policy(seller, None);
        assert!(!global.bot_override_active);
        assert!(global.terms.is_empty());
    }

    #[test]
    fn moderation_terms_are_data_not_sql() {
        let db = test_db("mod_injection.db");
        let payload = "x'; DROP TABLE moderation_blocked_words; --";
        assert!(db.add_blocked_word(1, payload, "hide", true));
        // Stored verbatim as data, not executed.
        assert_eq!(db.get_blocked_word(1, payload).unwrap().term, payload);
        assert_eq!(db.list_blocked_words(1).len(), 1);
        // Table still exists; a different seller's writes are unaffected.
        assert!(db.add_blocked_word(2, "normal", "warn", true));
        assert_eq!(db.list_blocked_words(2).len(), 1);
        // Invalid inputs are refused at the boundary.
        assert!(!db.add_blocked_word(1, "bad\0term", "warn", true));
        assert!(!db.add_blocked_word(1, "   ", "warn", true));
        assert!(!db.add_blocked_word(1, &"x".repeat(MODERATION_MAX_TERM_LEN + 1), "warn", true));
        assert!(!db.add_blocked_word(1, "bad", "DROP", true));
    }

    #[test]
    fn moderation_migration_is_additive_over_legacy_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod_legacy.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE config(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE bots(id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL UNIQUE, username TEXT, owner_user_id INTEGER NOT NULL, default_channel_id INTEGER, enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, name TEXT NOT NULL DEFAULT '');
                 INSERT INTO bots(key, owner_user_id, created_at) VALUES('legacy-bot', 42, '2026-01-01T00:00:00+00:00');
                 INSERT INTO config(key, value, updated_at) VALUES('channel', '@legacy', '2026-01-01T00:00:00+00:00');",
            )
            .unwrap();
        }
        let db = Db::new(path.to_str().unwrap()).unwrap();
        // Pre-existing data survives the additive migration.
        assert_eq!(db.get_config("channel").as_deref(), Some("@legacy"));
        assert!(db.get_bot_by_key("legacy-bot").is_some());
        assert_eq!(
            db.get_bot_by_key("legacy-bot").unwrap().bot_type,
            "shop_bot"
        );
        // New tables are present, empty, and usable.
        assert!(db.list_blocked_words(42).is_empty());
        assert!(db.list_allow_words(42).is_empty());
        assert!(db.list_bot_override_terms(42, 1).is_empty());
        // The global floor is seeded empty for backward compatibility, and a
        // legacy DB opens non-degraded (all moderation DDL succeeded).
        assert!(db.list_moderation_global_floor().is_empty());
        assert!(!db.resolve_effective_policy(42, None).enforcement_degraded);
        assert!(db.add_blocked_word(42, "after-migrate", "warn", true));
        drop(db);
        // Reopening is idempotent: no data loss, no duplicate-table error.
        let reopened = Db::new(path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.list_blocked_words(42).len(), 1);
    }

    #[test]
    fn moderation_decisions_and_appeals_store_no_message_content() {
        let db = test_db("mod_no_content.db");
        let seller = 404_i64;
        let decision = db
            .log_moderation_decision(
                seller,
                None,
                Some("badword"),
                "hide",
                Some("matched"),
                Some("opaque-ref-123"),
            )
            .unwrap();
        let appeal = db
            .create_moderation_appeal(
                seller,
                None,
                Some(decision),
                "opaque-ref-123",
                Some("false positive"),
            )
            .unwrap();

        // No content-like column exists on either audit/appeal table.
        {
            let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
            for table in ["moderation_decisions", "moderation_appeals"] {
                let cols: Vec<String> = conn
                    .prepare(&format!("PRAGMA table_info({table})"))
                    .unwrap()
                    .query_map([], |r| r.get::<_, String>(1))
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .collect();
                assert!(
                    !cols
                        .iter()
                        .any(|c| c == "content" || c == "text" || c == "message"),
                    "{table} has a content-like column: {cols:?}"
                );
            }
        }

        // Reason is capped; opaque reference is preserved verbatim.
        let long = "a".repeat(MODERATION_MAX_REASON_LEN + 50);
        let capped = db
            .create_moderation_appeal(seller, None, None, "opaque-2", Some(&long))
            .unwrap();
        assert_eq!(
            db.get_moderation_appeal(seller, capped)
                .unwrap()
                .reason
                .unwrap()
                .len(),
            MODERATION_MAX_REASON_LEN
        );
        assert_eq!(
            db.get_moderation_appeal(seller, appeal)
                .unwrap()
                .message_ref,
            "opaque-ref-123"
        );
    }

    fn mode_of<'a>(policy: &'a EffectiveModerationPolicy, term: &str) -> Option<&'a str> {
        policy
            .terms
            .iter()
            .find(|e| e.term == term)
            .map(|e| e.mode.as_str())
    }

    #[test]
    fn moderation_global_floor_is_persistent_owner_writable_and_non_overridable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("mod_global_floor.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        let (seller_a, seller_b) = (501_i64, 502_i64);

        // Fresh install: floor seeded empty, enforcement not degraded.
        assert!(db.list_moderation_global_floor().is_empty());
        assert!(
            !db.resolve_effective_policy(seller_a, None)
                .enforcement_degraded
        );

        // Owner writes a global floor term (no seller_id); invalid input is
        // refused at the boundary (parameterized data, never SQL).
        assert!(db.add_moderation_global_floor_term("globalban", "hide"));
        assert!(!db.add_moderation_global_floor_term("bad\0term", "hide"));
        assert!(!db.add_moderation_global_floor_term("banned", "DROP"));
        assert_eq!(db.list_moderation_global_floor().len(), 1);
        assert_eq!(db.list_moderation_global_floor()[0].mode, "hide");

        // A seller cannot weaken or allow-list away a floor term: the same term
        // at a looser seller mode plus a seller allow-list still resolves to
        // the floor's 'hide'.
        assert!(db.add_blocked_word(seller_a, "globalban", "warn", true));
        assert!(db.add_allow_word(seller_a, "globalban"));
        assert_eq!(
            mode_of(&db.resolve_effective_policy(seller_a, None), "globalban"),
            Some("hide")
        );
        // Floor is global: a second tenant inherits it, but stays isolated
        // from seller A's own rows.
        assert_eq!(
            mode_of(&db.resolve_effective_policy(seller_b, None), "globalban"),
            Some("hide")
        );
        assert!(db.list_blocked_words(seller_b).is_empty());
        assert!(db.list_allow_words(seller_b).is_empty());

        // Each floor term's own mode is honored, clamped to >= the global
        // minimum; re-moding is an upsert, not a second row.
        assert!(db.add_moderation_global_floor_term("reviewterm", "review"));
        assert_eq!(
            mode_of(&db.resolve_effective_policy(seller_a, None), "reviewterm"),
            Some("review")
        );
        assert!(db.add_moderation_global_floor_term("globalban", "review"));
        assert_eq!(db.list_moderation_global_floor().len(), 2);

        // Persistence across reopen.
        drop(db);
        let reopened = Db::new(path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.list_moderation_global_floor().len(), 2);
        assert_eq!(
            mode_of(
                &reopened.resolve_effective_policy(seller_a, None),
                "globalban"
            ),
            Some("review")
        );

        // Once the owner removes a term, ordinary seller precedence returns:
        // seller A's looser rule is now allow-listed away entirely.
        assert!(reopened.remove_moderation_global_floor_term("globalban"));
        assert!(!reopened.remove_moderation_global_floor_term("globalban"));
        assert_eq!(
            mode_of(
                &reopened.resolve_effective_policy(seller_a, None),
                "globalban"
            ),
            None
        );
    }

    #[test]
    fn moderation_floor_per_term_mode_honored_and_clamped() {
        let floor = vec![
            ModerationTermInput {
                term: "a".into(),
                mode: "hide".into(),
            },
            ModerationTermInput {
                term: "b".into(),
                mode: "warn".into(),
            },
        ];
        let seller = vec![
            ModerationTermInput {
                term: "a".into(),
                mode: "warn".into(),
            }, // cannot weaken the floor term
            ModerationTermInput {
                term: "b".into(),
                mode: "hide".into(),
            }, // may raise above its floor mode
        ];
        let allow = vec!["a".to_string(), "b".to_string()];
        let out = apply_moderation_precedence("warn", &floor, &seller, &[], &allow);
        let mode_of = |t: &str| out.iter().find(|e| e.term == t).map(|e| e.mode.as_str());
        assert_eq!(mode_of("a"), Some("hide"));
        assert_eq!(mode_of("b"), Some("hide"));
    }

    #[test]
    fn moderation_ddl_failure_is_logged_and_fails_closed() {
        // Force a moderation DDL statement to fail by pre-creating an object
        // with the same name as one of the indexes (CREATE INDEX IF NOT EXISTS
        // cannot succeed when a table of that name already exists).
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE idx_mod_blocked_seller(x INTEGER);")
            .unwrap();
        assert!(Db::migrate_moderation_tables(&conn).is_err());
        // The helper short-circuits: a later table was never created.
        let later_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='moderation_decisions')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!later_exists);

        // With readiness false, policy resolution surfaces the fail-closed
        // signal rather than silently returning an empty, permissive policy.
        let db = test_db("mod_ddl_closed.db");
        db.moderation_storage_ready.store(false, Ordering::Relaxed);
        assert!(db.add_moderation_global_floor_term("globalban", "hide"));
        let policy = db.resolve_effective_policy(9, None);
        assert!(policy.enforcement_degraded);
        assert_eq!(mode_of(&policy, "globalban"), Some("hide"));
    }

    #[test]
    fn moderation_floor_read_failure_falls_back_and_degrades() {
        let db = test_db("mod_floor_missing.db");
        {
            let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute_batch("DROP TABLE moderation_global_floor;")
                .unwrap();
        }
        // No panic; the compile-time default (empty) is used and the policy
        // is marked degraded so hooks can fail closed.
        let policy = db.resolve_effective_policy(7, None);
        assert!(policy.enforcement_degraded);
        assert!(policy.terms.is_empty());
        assert_eq!(policy.floor_mode, MODERATION_FLOOR_MODE);
    }

    #[test]
    fn moderation_appeals_duplicate_and_concurrent_inserts_are_idempotent() {
        use std::sync::Arc;
        use std::thread;

        let db = test_db("mod_appeals_dedup.db");
        let seller = 700_i64;

        // Sequential duplicate of the same tuple: exactly one row, the second
        // submission is a no-op and reports "no row created".
        let first = db
            .create_moderation_appeal(seller, None, None, "opaque:dup", Some("first"))
            .expect("first appeal inserts");
        assert!(
            db.create_moderation_appeal(seller, None, None, "opaque:dup", Some("second"))
                .is_none()
        );
        assert_eq!(db.list_moderation_appeals(seller, None).len(), 1);
        assert_eq!(
            db.get_moderation_appeal(seller, first)
                .unwrap()
                .reason
                .as_deref(),
            Some("first")
        );

        // Concurrent double-submission (bot-less tuple included): exactly one
        // of the racing writers wins, so no duplicate rows can be created.
        let shared = Arc::new(db);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let db = Arc::clone(&shared);
            handles.push(thread::spawn(move || {
                db.create_moderation_appeal(seller, None, None, "opaque:race", Some("race"))
            }));
        }
        let ids: Vec<Option<i64>> = handles
            .into_iter()
            .map(|h| h.join().expect("worker thread"))
            .collect();
        assert_eq!(ids.iter().filter(|id| id.is_some()).count(), 1);
        assert_eq!(shared.list_moderation_appeals(seller, None).len(), 2);

        // Distinct seller / ref / bot are still allowed — dedup is scoped to
        // the exact (seller_id, bot_id, message_ref) tuple.
        assert!(
            shared
                .create_moderation_appeal(seller + 1, None, None, "opaque:dup", None)
                .is_some()
        );
        assert!(
            shared
                .create_moderation_appeal(seller, None, None, "opaque:other", None)
                .is_some()
        );
        let bot = shared.create_bot("appeal-dedup-bot", seller, None, "");
        assert!(
            shared
                .create_moderation_appeal(seller, Some(bot), None, "opaque:dup", None)
                .is_some()
        );
        assert_eq!(shared.list_moderation_appeals(seller, None).len(), 4);
    }

    #[test]
    fn moderation_appeals_legacy_duplicates_are_cleaned_up_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("mod_appeals_legacy_dup.db");
        let path = path.to_str().unwrap().to_owned();
        let seller = 701_i64;
        let bot;
        {
            let db = Db::new(&path).unwrap();
            bot = db.create_bot("appeal-legacy-bot", seller, None, "");
            let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
            // Simulate a legacy DB: drop the unique index, then plant the
            // duplicate rows the old check-then-insert handler could create.
            conn.execute_batch("DROP INDEX IF EXISTS idx_mod_appeals_dedup;")
                .unwrap();
            for (id, reason, bot_id, reference) in [
                (1_i64, "original", None, "opaque:legacy"),
                (2_i64, "duplicate-1", None, "opaque:legacy"),
                (3_i64, "duplicate-2", None, "opaque:legacy"),
                (4_i64, "other-ref", None, "opaque:distinct"),
                (5_i64, "other-bot", Some(bot), "opaque:legacy"),
            ] {
                conn.execute(
                    "INSERT INTO moderation_appeals
                     (id,seller_id,bot_id,decision_id,message_ref,reason,status,created_at,updated_at)
                     VALUES(?1,?2,?3,NULL,?4,?5,'pending','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                    params![id, seller, bot_id, reference, reason],
                )
                .unwrap();
            }
        }
        // Reopening runs init_schema (dedup before the unique index) followed
        // by the idempotent migrate pass — the legacy DB opens with the
        // duplicates collapsed and every distinct tuple preserved.
        let reopened = Db::new(&path).unwrap();
        let rows = reopened.list_moderation_appeals(seller, None);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|r| r.message_ref == "opaque:legacy"
            && r.bot_id.is_none()
            && r.reason.as_deref() == Some("original")));
        assert!(
            rows.iter()
                .any(|r| r.message_ref == "opaque:distinct" && r.bot_id.is_none())
        );
        assert!(
            rows.iter()
                .any(|r| r.message_ref == "opaque:legacy" && r.bot_id == Some(bot))
        );

        let indexed: bool = {
            let conn = reopened.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_mod_appeals_dedup')",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(indexed);
        // The restored index blocks any further duplicate submission.
        assert!(
            reopened
                .create_moderation_appeal(seller, None, None, "opaque:legacy", Some("late"))
                .is_none()
        );
    }
}

// ---- Money Safety Spec: data-layer tests (decisions 1/3/4/5) ----

fn money_test_db(name: &str) -> (Db, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    let db = Db::new(path.to_str().unwrap()).unwrap();
    // Keep the tempdir alive for the test's lifetime by leaking it; these
    // are short-lived unit tests and the OS reclaims the directory.
    std::mem::forget(dir);
    (db, path)
}

fn refund_input<'a>(
    seller_id: i64,
    invoice_id: Option<&'a str>,
    amount: &'a str,
    asset: &'a str,
    idem_key: &'a str,
    created_by: i64,
    created_role: &'a str,
) -> RefundInput<'a> {
    RefundInput {
        seller_id,
        invoice_id,
        user_id: None,
        amount,
        asset,
        reason: Some("test refund"),
        idem_key,
        created_by,
        created_role,
    }
}

/// §1.1/§1.2: one in-flight refund per invoice; idem replay returns the
/// same row; a completed partial refund frees the in-flight slot for a
/// later, distinctly-keyed partial (no blanket UNIQUE(invoice_id)).
#[test]
fn money_refund_inflight_uniqueness_and_partial_after_completion() {
    let (db, _path) = money_test_db("refund_inflight.db");
    db.save_invoice(
        "inv-u1",
        1,
        "10",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let first = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-u1"),
            "4",
            "USDT",
            "key-1",
            7,
            "Seller",
        ))
        .unwrap();
    assert!(first.created_new);

    let replay = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-u1"),
            "4",
            "USDT",
            "key-1",
            7,
            "Seller",
        ))
        .unwrap();
    assert_eq!(replay.id, first.id);
    assert!(!replay.created_new);
    assert_eq!(db.list_refunds(Some(1)).len(), 1);

    let blocked = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-u1"),
            "1",
            "USDT",
            "key-2",
            7,
            "Seller",
        ))
        .unwrap_err();
    assert_eq!(blocked, RefundCreateError::InFlight);

    db.set_refund_status(first.id, "executed");
    let second = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-u1"),
            "3",
            "USDT",
            "key-2",
            7,
            "Seller",
        ))
        .unwrap();
    assert!(second.created_new);
    assert_ne!(second.id, first.id);
    assert_eq!(db.list_refunds(Some(1)).len(), 2);
}

/// §1.2 step 3/4: the cap counts every non-void row (including terminal
/// `executed` and the legacy `processed` alias); `void` is not counted.
#[test]
fn money_refund_cumulative_cap_across_states() {
    let (db, path) = money_test_db("refund_cap.db");
    db.save_invoice(
        "inv-cap",
        1,
        "10.00000001",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let r1 = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-cap"),
            "6",
            "USDT",
            "cap-1",
            7,
            "Seller",
        ))
        .unwrap();
    db.set_refund_status(r1.id, "executed");
    // 6 + 4.00000001 == the invoiced amount: allowed to the last minor unit.
    let r2 = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-cap"),
            "4.00000001",
            "USDT",
            "cap-2",
            7,
            "Seller",
        ))
        .unwrap();
    assert!(r2.created_new);
    // One more minor unit exceeds the cap.
    db.set_refund_status(r2.id, "executed");
    let err = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-cap"),
            "0.00000001",
            "USDT",
            "cap-3",
            7,
            "Seller",
        ))
        .unwrap_err();
    assert_eq!(err, RefundCreateError::CapExceeded);

    // A `void` row is excluded from the cumulative sum and the in-flight set.
    let voided = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-void"),
            "1",
            "USDT",
            "void-1",
            7,
            "Seller",
        ))
        .unwrap_err();
    assert_eq!(voided, RefundCreateError::InvoiceNotFound);
    db.save_invoice(
        "inv-void",
        1,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let r3 = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-void"),
            "5",
            "USDT",
            "void-1",
            7,
            "Seller",
        ))
        .unwrap();
    db.set_refund_status(r3.id, "void");
    let r4 = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-void"),
            "5",
            "USDT",
            "void-2",
            7,
            "Seller",
        ))
        .unwrap();
    assert!(
        r4.created_new,
        "void must not consume cap or in-flight slot"
    );

    // Legacy `processed` alias on a fresh row is still counted (read-only).
    db.save_invoice(
        "inv-processed",
        1,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO refunds(seller_id,invoice_id,user_id,amount,asset,status,idem_key,created_by,created_role,created_at,updated_at) VALUES(1,'inv-processed',42,'4','USDT','processed','legacy:900',-1,'unknown','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }
    let blocked = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-processed"),
            "2",
            "USDT",
            "cap-4",
            7,
            "Seller",
        ))
        .unwrap_err();
    assert_eq!(blocked, RefundCreateError::CapExceeded);
}

/// §1.1: asset is locked to the invoice, actor is persisted, and money
/// columns are immutable even from a raw writer.
#[test]
fn money_refund_asset_lock_actor_and_immutability() {
    let (db, path) = money_test_db("refund_immutable.db");
    db.save_invoice(
        "inv-asset",
        1,
        "10",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let mismatch = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-asset"),
            "5",
            "TON",
            "asset-1",
            7,
            "Seller",
        ))
        .unwrap_err();
    assert_eq!(mismatch, RefundCreateError::AssetMismatch);
    let created = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-asset"),
            "5",
            "USDT",
            "asset-2",
            77,
            "Owner",
        ))
        .unwrap();
    assert!(created.created_new);
    let row = db.get_refund(created.id).unwrap();
    assert_eq!(row.1, 1);
    assert_eq!(row.5, "USDT");

    {
        let conn = Connection::open(&path).unwrap();
        let idem: String = conn
            .query_row(
                "SELECT idem_key FROM refunds WHERE id=?1",
                [created.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idem, "asset-2");
        let actor: (i64, String) = conn
            .query_row(
                "SELECT created_by,created_role FROM refunds WHERE id=?1",
                [created.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(actor.0, 77);
        assert_eq!(actor.1, "Owner");
        assert!(
            conn.execute("UPDATE refunds SET amount='999' WHERE id=?1", [created.id])
                .is_err(),
            "money columns must be immutable"
        );
        assert!(
            conn.execute(
                "UPDATE refunds SET status='executed' WHERE id=?1",
                [created.id]
            )
            .is_ok(),
            "status is allowed to change"
        );
    }
}

/// F3: `refund_creator` reads the actor from the persisted refund row written
/// by `create_refund_guarded` (not the age-limited audit log), so separation
/// of duties survives on mature DBs; unknown ids resolve to `None`.
#[test]
fn money_refund_creator_returns_persisted_actor() {
    let (db, _path) = money_test_db("refund_creator.db");
    db.save_invoice(
        "inv-creator",
        1,
        "10",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let created = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-creator"),
            "3",
            "USDT",
            "creator-key",
            88,
            "Owner",
        ))
        .unwrap();
    assert!(created.created_new);
    assert_eq!(
        db.refund_creator(created.id),
        Some((88, "Owner".to_string()))
    );
    assert_eq!(db.refund_creator(created.id + 9999), None);
}

/// §1.4 + §2.7 legacy migration: columns backfilled idempotently, audit
/// actor recovered where present, sentinels otherwise, no row deletions.
#[test]
fn money_migration_legacy_is_additive_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy_money.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE invoices (id INTEGER PRIMARY KEY AUTOINCREMENT, invoice_id TEXT NOT NULL UNIQUE, seller_id INTEGER NOT NULL DEFAULT 0, amount TEXT NOT NULL, asset TEXT NOT NULL, description TEXT, status TEXT NOT NULL DEFAULT 'active', pay_url TEXT, plan_id TEXT, user_id INTEGER, attempts INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE refunds (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, invoice_id TEXT, user_id INTEGER, amount TEXT NOT NULL, asset TEXT NOT NULL, reason TEXT, status TEXT NOT NULL DEFAULT 'pending', created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE payouts (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, amount TEXT NOT NULL, asset TEXT NOT NULL, destination TEXT, status TEXT NOT NULL DEFAULT 'pending', created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT, actor_id INTEGER NOT NULL, actor_role TEXT NOT NULL, action TEXT NOT NULL, target_type TEXT NOT NULL, target_id TEXT, metadata TEXT, created_at TEXT NOT NULL);
             INSERT INTO invoices(invoice_id,seller_id,amount,asset,status,created_at,updated_at) VALUES('old-inv',3,'2.5','USDT','paid','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z');
             INSERT INTO refunds(seller_id,invoice_id,user_id,amount,asset,status,created_at,updated_at) VALUES(3,'old-inv',42,'2.5','USDT','executed','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z');
             INSERT INTO audit_log(actor_id,actor_role,action,target_type,target_id,metadata,created_at) VALUES(55,'Owner','refund_create','refund','1','{}','2020-01-01T00:00:00Z');",
        )
        .unwrap();
    }
    // First open migrates; a second open is byte-identical (idempotent).
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let reopened = Db::new(path.to_str().unwrap()).unwrap();
    for db in [&db, &reopened] {
        let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
        let (idem, actor, role): (String, i64, String) = conn
            .query_row(
                "SELECT idem_key,created_by,created_role FROM refunds WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(idem, "legacy:1");
        assert_eq!(actor, 55, "actor comes from the refund_create audit row");
        assert_eq!(role, "Owner");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM refunds", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "migration deletes no money rows");
    }
}

/// §1.4: legacy DBs with two in-flight refunds for one invoice fail closed
/// with the offending ids; after an audited void leaves one survivor the
/// same DB opens and the unique index is present.
#[test]
fn money_migration_fails_closed_on_duplicate_inflight() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy_duplicate.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE refunds (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, invoice_id TEXT, user_id INTEGER, amount TEXT NOT NULL, asset TEXT NOT NULL, reason TEXT, status TEXT NOT NULL DEFAULT 'pending', created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             INSERT INTO refunds(seller_id,invoice_id,user_id,amount,asset,status,created_at,updated_at) VALUES(1,'dup',42,'1','USDT','pending','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z');
             INSERT INTO refunds(seller_id,invoice_id,user_id,amount,asset,status,created_at,updated_at) VALUES(1,'dup',42,'1','USDT','approved','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z');",
        )
        .unwrap();
    }
    let err = match Db::new(path.to_str().unwrap()) {
        Ok(_) => panic!("legacy duplicate in-flight refunds must fail closed"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(msg.contains("money-safety migration refused"), "{msg}");
    assert!(msg.contains("dup"), "{msg}");
    // Audited void resolution leaves one survivor; no money row is deleted.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE refunds SET status='void' WHERE id=2", [])
            .unwrap();
    }
    let db = Db::new(path.to_str().unwrap()).unwrap();
    let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
    let indexed: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='uq_refunds_invoice_inflight')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(indexed);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM refunds", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "both historical rows survive resolution");
}

/// §3: paid_ungranted is non-terminal, stamped once, retryable back to
/// paid, and never becomes `failed` after payment. 72h rows with no
/// executed auto-refund block the seller.
#[test]
fn money_paid_ungranted_transitions_and_failed_guard() {
    let (db, path) = money_test_db("paid_ungranted.db");
    db.save_invoice(
        "inv-ung",
        9,
        "3",
        "USDT",
        None,
        "active",
        None,
        None,
        Some(42),
        None,
    );
    assert!(db.mark_invoice_paid_ungranted("inv-ung"));
    let stamp = db.invoice_paid_ungranted_at("inv-ung").unwrap();
    assert!(!stamp.is_empty());
    // The DB trigger refuses `failed` from a paid state (best-effort callers
    // see no write; a raw writer sees a hard error).
    db.update_invoice_status("inv-ung", "failed");
    assert_eq!(db.get_invoice("inv-ung").unwrap().2, "paid_ungranted");
    {
        let conn = Connection::open(&path).unwrap();
        assert!(
            conn.execute(
                "UPDATE invoices SET status='failed' WHERE invoice_id='inv-ung'",
                []
            )
            .is_err()
        );
    }

    // Injected clock boundaries.
    assert_eq!(db.paid_ungranted_before("2999-01-01T00:00:00Z").len(), 1);
    assert!(db.paid_ungranted_before("2000-01-01T00:00:00Z").is_empty());
    assert_eq!(
        db.unresolved_ungranted_invoices(9, "2999-01-01T00:00:00Z")
            .len(),
        1
    );

    // Successful retry returns to paid and clears the clock.
    assert!(db.finalize_invoice_grant_retry("inv-ung"));
    assert_eq!(db.get_invoice("inv-ung").unwrap().2, "paid");
    assert!(db.invoice_paid_ungranted_at("inv-ung").is_none());
    // A fresh ungranted event starts a fresh window.
    assert!(db.mark_invoice_paid_ungranted("inv-ung"));
    assert_ne!(db.invoice_paid_ungranted_at("inv-ung").unwrap(), stamp);

    // The auto-refund (documented idem key) clears the blocking liability
    // only once it is executed.
    let auto = db
        .create_refund_guarded(refund_input(
            9,
            Some("inv-ung"),
            "3",
            "USDT",
            "auto:ungranted:inv-ung",
            -1,
            "system",
        ))
        .unwrap();
    assert!(auto.created_new);
    assert_eq!(
        db.unresolved_ungranted_invoices(9, "2999-01-01T00:00:00Z")
            .len(),
        1
    );
    db.set_refund_status(auto.id, "executed");
    assert!(
        db.unresolved_ungranted_invoices(9, "2999-01-01T00:00:00Z")
            .is_empty()
    );
    // Other sellers are never blocked by this seller's liability.
    assert!(
        db.unresolved_ungranted_invoices(10, "2999-01-01T00:00:00Z")
            .is_empty()
    );
}

/// §4.1: spend_id is persisted before transfer, attempt_no is monotonic,
/// duplicate keys are hard errors, and ambiguous retries can look up the
/// same key.
#[test]
fn money_transfer_attempt_spend_id_persistence() {
    let (db, _path) = money_test_db("spend_id.db");
    let a1 = db
        .record_transfer_attempt("refund", 11, 3, "spend-aaa")
        .unwrap();
    let a2 = db
        .record_transfer_attempt("refund", 11, 3, "spend-bbb")
        .unwrap();
    assert!(a2 > a1);
    let first = db.get_transfer_attempt("spend-aaa").unwrap();
    let second = db.get_transfer_attempt("spend-bbb").unwrap();
    assert_eq!(first.attempt_no, 1);
    assert_eq!(second.attempt_no, 2);
    assert_eq!(second.row_type, "refund");
    assert_eq!(second.row_id, 11);
    assert_eq!(second.seller_id, 3);
    assert!(db.get_transfer_attempt("spend-nope").is_none());
    assert!(
        db.record_transfer_attempt("refund", 12, 3, "spend-aaa")
            .is_err()
    );
    assert!(
        db.record_transfer_attempt("bogus", 12, 3, "spend-ccc")
            .is_err()
    );
    assert!(db.record_transfer_attempt("payout", 12, 3, "").is_err());

    let latest = db.latest_transfer_attempt("refund", 11).unwrap();
    assert_eq!(latest.spend_id, "spend-bbb");
    assert!(db.latest_transfer_attempt("refund", 99).is_none());

    assert!(db.set_transfer_attempt_outcome("spend-aaa", "ambiguous"));
    let resolved = db.get_transfer_attempt("spend-aaa").unwrap();
    assert_eq!(resolved.outcome.as_deref(), Some("ambiguous"));
    assert!(resolved.resolved_at.is_some());
}

/// §5.1-§5.4: executing stamps a clock, the sweeper moves due rows to
/// needs_reconciliation, the three owner outcomes are append-only, and
/// the digest carries the persisted spend_id.
#[test]
fn money_reconciliation_state_transitions() {
    let (db, path) = money_test_db("reconcile.db");
    db.save_invoice(
        "inv-rec",
        4,
        "10",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let refund = db
        .create_refund_guarded(refund_input(
            4,
            Some("inv-rec"),
            "10",
            "USDT",
            "rec-1",
            7,
            "Seller",
        ))
        .unwrap();
    let payout = db.create_payout(4, "2", "USDT", Some("@dest"));
    db.set_refund_status(refund.id, "approved");
    db.set_payout_status(payout, "approved");
    assert!(db.begin_refund_execution(refund.id));
    assert!(db.begin_payout_execution(payout));
    {
        let conn = Connection::open(&path).unwrap();
        for (table, id) in [("refunds", refund.id), ("payouts", payout)] {
            let since: Option<String> = conn
                .query_row(
                    &format!("SELECT executing_since FROM {table} WHERE id=?1"),
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(since.is_some(), "CAS must stamp executing_since");
        }
    }

    // Nothing is due before the injected cutoff; everything is due after it.
    assert!(
        db.reconciliation_rows("2000-01-01T00:00:00Z", "2999-01-01T00:00:00Z")
            .is_empty()
    );
    assert!(
        db.record_transfer_attempt("refund", refund.id, 4, "rec-spend")
            .is_ok()
    );
    let rows = db.reconciliation_rows("2999-01-01T00:00:00Z", "2999-01-01T00:00:00Z");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|r| r.spend_id.as_deref() == Some("rec-spend"))
    );

    assert!(db.mark_needs_reconciliation("refund", refund.id, "2999-01-01T00:00:00Z"));
    assert!(!db.mark_needs_reconciliation("refund", refund.id, "2999-01-01T00:00:00Z"));
    assert_eq!(db.get_refund(refund.id).unwrap().7, "needs_reconciliation");

    // Reason is mandatory; already-final rows cannot be resolved.
    assert_eq!(
        db.resolve_reconciliation(
            "refund",
            refund.id,
            ReconciliationOutcome::Executed,
            99,
            "  "
        ),
        Err(ReconcileError::ReasonRequired)
    );
    assert_eq!(
        db.resolve_reconciliation(
            "payout",
            payout,
            ReconciliationOutcome::Executed,
            99,
            "receipt checked"
        ),
        Err(ReconcileError::NotDue)
    );
    db.resolve_reconciliation(
        "refund",
        refund.id,
        ReconciliationOutcome::Executed,
        99,
        "receipt checked",
    )
    .unwrap();
    let resolution = db.money_resolution("refund", refund.id).unwrap();
    assert_eq!(resolution.0, "executed");
    assert_eq!(resolution.1, "receipt checked");
    assert_eq!(resolution.2, 99);
    assert_eq!(
        db.resolve_reconciliation(
            "refund",
            refund.id,
            ReconciliationOutcome::Void,
            99,
            "second try"
        ),
        Err(ReconcileError::AlreadyResolved)
    );
    // Append-only is enforced by the trigger too, not just the method CAS.
    {
        let conn = Connection::open(&path).unwrap();
        assert!(
            conn.execute(
                "UPDATE refunds SET resolution='tampered' WHERE id=?1",
                [refund.id]
            )
            .is_err()
        );
    }

    // The other two outcomes map to `approved` and `void`.
    assert!(db.mark_needs_reconciliation("payout", payout, "2999-01-01T00:00:00Z"));
    db.resolve_reconciliation(
        "payout",
        payout,
        ReconciliationOutcome::RevertedToApproved,
        99,
        "transfer never landed",
    )
    .unwrap();
    assert_eq!(db.get_payout_status(payout).as_deref(), Some("approved"));
    assert!(
        db.resolve_reconciliation(
            "payout",
            payout,
            ReconciliationOutcome::Void,
            99,
            "too late"
        )
        .is_err()
    );
}

/// Every new money record is seller-scoped; listings and digest rows never
/// cross tenant boundaries.
#[test]
fn money_safety_seller_isolation() {
    let (db, _path) = money_test_db("scope.db");
    db.save_invoice(
        "inv-s1",
        1,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(11),
        None,
    );
    db.save_invoice(
        "inv-s2",
        2,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(22),
        None,
    );
    let r1 = db
        .create_refund_guarded(refund_input(
            1,
            Some("inv-s1"),
            "5",
            "USDT",
            "s1-key",
            1,
            "Seller",
        ))
        .unwrap();
    let r2 = db
        .create_refund_guarded(refund_input(
            2,
            Some("inv-s2"),
            "5",
            "USDT",
            "s2-key",
            2,
            "Seller",
        ))
        .unwrap();
    assert_eq!(db.list_refunds(Some(1)).len(), 1);
    assert_eq!(db.list_refunds(Some(2)).len(), 1);
    assert_eq!(db.list_refunds(Some(3)).len(), 0);
    assert_eq!(db.get_refund(r1.id).unwrap().1, 1);
    assert_eq!(db.get_refund(r2.id).unwrap().1, 2);

    db.record_transfer_attempt("refund", r1.id, 1, "s1-spend")
        .unwrap();
    db.record_transfer_attempt("refund", r2.id, 2, "s2-spend")
        .unwrap();
    assert_eq!(db.get_transfer_attempt("s1-spend").unwrap().seller_id, 1);
    assert_eq!(db.get_transfer_attempt("s2-spend").unwrap().seller_id, 2);
}

/// G3 (deferred decision 8 guard): no NEW REAL/DOUBLE/FLOAT money column,
/// and all §1/§4/§5 columns exist with the expected shapes.
#[test]
fn money_safety_schema_shape() {
    let (db, _path) = money_test_db("shape.db");
    let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
    for table in ["invoices", "refunds", "payouts", "transfer_attempts"] {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let info: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(1)?, r.get(2)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for (name, ty) in &info {
            let ty = ty.to_ascii_uppercase();
            assert!(
                !matches!(ty.as_str(), "REAL" | "DOUBLE" | "FLOAT"),
                "{table}.{name} is a forbidden floating money column"
            );
        }
        let names: Vec<&str> = info.iter().map(|(n, _)| n.as_str()).collect();
        match table {
            "refunds" => {
                for expected in [
                    "idem_key",
                    "created_by",
                    "created_role",
                    "executing_since",
                    "reconciliation_since",
                    "resolution",
                    "resolved_by",
                    "resolved_at",
                ] {
                    assert!(names.contains(&expected), "refunds.{expected} missing");
                }
            }
            "payouts" => {
                for expected in [
                    "executing_since",
                    "reconciliation_since",
                    "resolution",
                    "resolved_by",
                    "resolved_at",
                ] {
                    assert!(names.contains(&expected), "payouts.{expected} missing");
                }
            }
            "invoices" => assert!(names.contains(&"paid_ungranted_at")),
            "transfer_attempts" => {
                for expected in [
                    "spend_id",
                    "attempt_no",
                    "seller_id",
                    "outcome",
                    "resolved_at",
                ] {
                    assert!(
                        names.contains(&expected),
                        "transfer_attempts.{expected} missing"
                    );
                }
            }
            _ => {}
        }
    }
    let index_names: Vec<String> = conn
        .prepare("PRAGMA index_list(refunds)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert!(
        index_names
            .iter()
            .any(|n| n == "uq_refunds_invoice_inflight")
    );
    assert!(index_names.iter().any(|n| n == "uq_refunds_invoice_idem"));
}

#[test]
fn capability_catalogue_search_is_active_and_buyer_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("search.db").to_str().unwrap()).unwrap();
    db.add_product(
        1,
        "Night Shift",
        "5",
        "USDT",
        "digital_file",
        Some("payload"),
        None,
    )
    .unwrap();
    db.add_product(
        1,
        "Hidden Reel",
        "7",
        "USDT",
        "digital_link",
        Some("link"),
        None,
    )
    .unwrap();
    db.add_product(
        0,
        "Admin Only",
        "9",
        "USDT",
        "digital_link",
        Some("link"),
        None,
    )
    .unwrap();
    db.set_product_active(1, "Hidden Reel", false);

    let hits = db.search_products("night", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].2, "Night Shift");
    assert!(
        db.search_products("reel", 10).is_empty(),
        "inactive is hidden"
    );
    assert!(
        db.search_products("admin", 10).is_empty(),
        "the legacy seller 0 namespace is excluded by the search query"
    );
    assert!(!db.search_products("", 10).is_empty());
}

#[test]
fn capability_file_share_grants_are_scoped_and_revocable() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("shares.db").to_str().unwrap()).unwrap();
    let id = db.create_file_share(7, 3, "file-id", "clip", "desc", "TOKEN1");
    assert!(id > 0);
    let row = db.get_file_share(id).expect("row exists");
    assert_eq!(row.1, 7);
    assert_eq!(row.2, 3);
    assert_eq!(row.3, "file-id");
    assert_eq!(row.6, "TOKEN1");
    assert_eq!(db.get_file_share_by_token("TOKEN1").unwrap().0, id);

    assert_eq!(db.list_file_shares(Some(7), Some(3)).len(), 1);
    assert!(db.list_file_shares(Some(8), Some(3)).is_empty());
    assert!(db.list_file_shares_for_user(42).is_empty());

    assert!(db.grant_file_share(id, 42));
    assert!(!db.grant_file_share(id, 42), "grant is idempotent");
    assert!(db.file_share_grant_exists(id, 42));
    let granted = db.list_file_shares_for_user(42);
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0].0, id);
    assert!(db.revoke_file_share(id, 42));
    assert!(db.list_file_shares_for_user(42).is_empty());

    assert!(db.set_file_share_active(id, 7, false));
    assert!(
        !db.set_file_share_active(id, 8, false),
        "other seller cannot change it"
    );
    assert!(db.list_file_shares(Some(7), Some(3)).is_empty());
}

#[test]
fn capability_automation_rules_scope_and_active_filter() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("rules.db").to_str().unwrap()).unwrap();
    let id = db.create_automation_rule(7, 3, "hello", "hi there");
    db.create_automation_rule(8, 3, "hello", "other seller");
    db.create_automation_rule(7, 9, "hello", "other bot");

    let mine = db.list_automation_rules(Some(7), Some(3), true);
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].0, id);
    assert_eq!(mine[0].3, "hello");
    assert_eq!(db.list_automation_rules(None, Some(3), true).len(), 2);
    assert_eq!(db.list_automation_rules(Some(7), None, true).len(), 2);

    assert!(db.set_automation_rule_active(id, 7, false));
    assert!(!db.set_automation_rule_active(id, 8, false));
    assert!(db.list_automation_rules(Some(7), Some(3), true).is_empty());
    assert_eq!(db.list_automation_rules(Some(7), Some(3), false).len(), 1);
}
