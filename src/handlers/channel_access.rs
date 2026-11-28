//! Single source of truth for channel-target authorization (security F2).
//!
//! The REST API and the server-rendered console address the same `channels`
//! rows and both can trigger a send through the bot bound to the target. Keeping
//! this policy in one module stops the two call sites from drifting: the API
//! delegates from `api_may_mutate_channel`, and the console's
//! `web::may_mutate_channel` is expected to delegate here too.
//!
//! No new privileges: owner/admin keep the global tier, a seller may address
//! only a row they own or their configured default channel, and every other
//! case fails closed.

use crate::{db::Db, roles::Role};

/// Normalize a channel target for comparison: trimmed, no leading `@`, ASCII
/// lowercased. Stored names may be handles or numeric ids; the value sent to
/// Telegram is never rewritten. A second pass must be a no-op, so stripping
/// the prefix cannot stop while whitespace that trim() would remove is still
/// hanging off the result.
pub fn channel_key(channel: &str) -> String {
    channel
        .trim()
        .trim_start_matches('@')
        .trim()
        .to_ascii_lowercase()
}

/// Exact stored form first, then the normalized key.
fn owns_channel_row(db: &Db, seller_id: i64, channel: &str) -> bool {
    db.get_channel_seller_id(channel.trim())
        .or_else(|| db.get_channel_seller_id(&channel_key(channel)))
        == Some(seller_id)
}

/// Tenant gate for channel-addressed mutations (post scheduling, channel
/// metadata, post row ops). Owner/admin are the global privileged tier; a
/// seller owns a `channels` row or their own `sellers.default_channel`;
/// everything else (profileless session, unknown or foreign channel) is
/// refused. Callers MUST run this before any Bot API call or DB row write.
pub fn may_mutate_channel(
    db: &Db,
    session_role: &str,
    session_seller_id: Option<i64>,
    account_id: i64,
    selected_bot_id: Option<i64>,
    channel: &str,
) -> bool {
    // These two fields are part of the shared contract so both surfaces can
    // adopt the same signature before any future bot-scoped tightening.
    // Today's policy is role + seller-scope only; consulting them here would
    // narrow the owner/admin global tier and change existing behavior.
    let _ = (account_id, selected_bot_id);
    if Role::from_role_str(session_role).has_level(Role::Admin) {
        return true;
    }
    let key = channel_key(channel);
    if key.is_empty() {
        return false;
    }
    let Some(sid) = session_seller_id else {
        return false;
    };
    if owns_channel_row(db, sid, channel) {
        return true;
    }
    db.seller_channel(sid)
        .map(|own| channel_key(&own))
        .is_some_and(|own| !own.is_empty() && own == key)
}

/// Composer-target variant for immediate post creation: the console dropdown
/// offers sellers the global namespace plus their own rows, so a global row is
/// allowed in addition to the [`may_mutate_channel`] cases. A row owned by
/// another seller is still refused.
pub fn may_post_to_channel(
    db: &Db,
    session_role: &str,
    session_seller_id: Option<i64>,
    account_id: i64,
    selected_bot_id: Option<i64>,
    channel: &str,
) -> bool {
    if may_mutate_channel(
        db,
        session_role,
        session_seller_id,
        account_id,
        selected_bot_id,
        channel,
    ) {
        return true;
    }
    let key = channel_key(channel);
    // The extra global-row allowance is a seller privilege. Without a tenant
    // binding, a buyer or unknown-role principal would otherwise deliver a
    // message through the operator's bot and only fail afterwards when the row
    // is attributed. Operator tiers already passed `may_mutate_channel`.
    if !Role::from_role_str(session_role).has_level(Role::Seller) || session_seller_id.is_none() {
        return false;
    }
    !key.is_empty() && owns_channel_row(db, 0, channel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Canonicalization is total and fail-closed: a target always loses every
    // leading '@' and is ASCII-lowercased, and applying it twice changes
    // nothing. An input made only of whitespace and '@' canonicalizes to the
    // empty (refused) key. An '@' in the middle of a name is not a prefix
    // marker and is deliberately preserved, so the property checks the
    // leading form rather than banning the character outright.
    proptest! {
        #[test]
        fn prop_channel_key_is_total_and_fail_closed(s in ".*") {
            let k = channel_key(&s);
            prop_assert!(!k.starts_with('@'), "leading @ survived: {:?}", k);
            prop_assert!(k == k.to_ascii_lowercase(), "not lowercase: {:?}", k);
            prop_assert!(channel_key(&k) == k, "not idempotent: {:?}", k);
            prop_assert_eq!(
                k.is_empty(),
                s.trim().trim_start_matches('@').is_empty(),
                "empty target mismatch: {:?} -> {:?}", s, k
            );
        }
    }

    #[test]
    fn channel_gate_matches_console_policy() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::new(dir.path().join("test.db").to_str().unwrap()).unwrap();
        db.upsert_seller(42, Some("seller42"), Some("@mine"));
        db.add_channel("@mine", 42).unwrap();
        db.add_channel("@victim", 99).unwrap();
        db.add_channel("@global", 0).unwrap();

        // Owner/admin keep the global tier.
        assert!(may_mutate_channel(&db, "owner", None, 1, None, "@victim"));
        assert!(may_mutate_channel(&db, "admin", None, 2, None, "@victim"));
        // A seller may address their own row, their default in any spelling,
        // but not another tenant's row, a global row, an empty target, or a
        // session without a seller profile.
        for target in ["@mine", "mine", " @MINE "] {
            assert!(may_mutate_channel(&db, "seller", Some(42), 3, None, target));
        }
        for target in ["@victim", "@global", "@ghost", ""] {
            assert!(!may_mutate_channel(
                &db,
                "seller",
                Some(42),
                3,
                None,
                target
            ));
        }
        assert!(!may_mutate_channel(&db, "seller", None, 3, None, "@mine"));
        assert!(!may_mutate_channel(&db, "user", None, 4, None, "@mine"));

        // The composer variant additionally admits the global namespace, never
        // another seller's row.
        assert!(may_post_to_channel(
            &db,
            "seller",
            Some(42),
            3,
            None,
            "@global"
        ));
        assert!(may_post_to_channel(
            &db,
            "seller",
            Some(42),
            3,
            None,
            "@mine"
        ));
        assert!(!may_post_to_channel(
            &db,
            "seller",
            Some(42),
            3,
            None,
            "@victim"
        ));
        // A tenantless principal must not reach the global row either: the
        // send happens before the caller can attribute the post to a tenant.
        assert!(!may_post_to_channel(&db, "user", None, 4, None, "@global"));
        assert!(!may_post_to_channel(
            &db, "seller", None, 4, None, "@global"
        ));
        assert!(!may_post_to_channel(
            &db, "unknown", None, 4, None, "@global"
        ));
    }
}
