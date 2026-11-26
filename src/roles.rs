use crate::db::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    User = 0,
    Seller = 1,
    Admin = 2,
    Owner = 3,
}

impl Role {
    /// Parse the role string stored in sessions / accounts into an ordered
    /// level. Unknown (or legacy) strings intentionally degrade to `User`,
    /// the least-privileged level — never to a privileged one.
    pub fn from_role_str(s: &str) -> Role {
        match s {
            "owner" => Role::Owner,
            "admin" => Role::Admin,
            "seller" => Role::Seller,
            "user" => Role::User,
            _ => Role::User,
        }
    }

    /// Ordered gate check: does this role meet or exceed `required`?
    pub fn has_level(&self, required: Role) -> bool {
        *self >= required
    }
}

/// Compatibility wrapper for call sites that used the old name-string check.
/// Admins and owners share the admin tier; everything else (including an
/// unknown role string, which parses to `User`) is rejected.
pub fn is_admin_role(role: &str) -> bool {
    Role::from_role_str(role).has_level(Role::Admin)
}

/// True when a (role, seller binding) pair may carry tenant authority.
///
/// The one state that may not is the seller tier with no binding: it has no
/// tenant to act as, and treating it as "unscoped" is what used to widen it to
/// the platform view. Every other tier is usable -- owner/admin are legitimately
/// global, and `user` is a buyer account whose authority is limited elsewhere.
///
/// This is the session-load gate (`api.rs::seller_scope` is the per-request
/// view). Keeping it a pure function makes the policy testable without a DB and
/// usable at both the mint and the load boundary.
pub fn session_is_usable(role: &str, seller_id: Option<i64>) -> bool {
    !(Role::from_role_str(role) == Role::Seller && seller_id.is_none())
}

/// The tenant scope a request is allowed to act on, resolved once from the
/// session at the auth boundary.
///
/// The previous representation was a bare `Option<i64>` where `None` meant
/// "unscoped, so show every tenant". That conflated two very different states:
/// the operator's legitimate global view, and a `seller` account whose
/// `seller_id` column was NULL. The second one therefore *resolved to* the
/// first — a seller with no binding silently inherited cross-tenant read and
/// write authority (`web_accounts.seller_id` is nullable and the console's
/// create form defaults to the empty "platform-level" option).
///
/// `Denied` makes the missing-scope state unrepresentable as authority: it
/// is not a flavour of `None`, it is a refusal, and every consumer must handle
/// it before touching data. `Role::tenant_scope` is the only constructor, so
/// the mapping is auditable in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantScope {
    /// The platform operator view: the owner, or any admin account.
    Global,
    /// Exactly one seller's rows.
    Seller(i64),
    /// No tenant authority. Callers must refuse the request, never widen.
    Denied,
}

impl TenantScope {
    /// Sentinel passed to `Db` as a seller id for [`TenantScope::Denied`].
    ///
    /// Seller ids are Telegram user ids (always positive) plus the platform
    /// namespace `0`, so `-1` can never own a row. This is defence in depth for
    /// a call site that forgets [`TenantScope::is_denied`]: the query still
    /// returns no seller-owned rows instead of widening to every tenant.
    ///
    /// It is NOT a complete denial on its own. The `Db` layer has two shapes
    /// that a sentinel cannot neutralise, so a gate must refuse `Denied` before
    /// calling a scoped helper:
    ///
    /// * Visibility predicates that *include* the platform namespace, e.g.
    ///   `list_channels` is `WHERE seller_id = 0 OR seller_id = ?1`, so `-1`
    ///   still yields the global rows.
    /// * Ownership predicates of the form `(?2 IS NULL OR seller_id = ?2)`,
    ///   where only `None` skips the check; `-1` participates in it normally
    ///   and therefore fails closed, which is the safe direction.
    ///
    /// Negative ids are never issued to a seller and no `Db` path treats one as
    /// privileged, so `-1` can only under-select, never over-select.
    pub const DENIED_ID: i64 = -1;

    /// The `Option<i64>` the `Db` layer takes, where `None` means "all rows".
    ///
    /// `Denied` maps to [`Self::DENIED_ID`], never `None` — the whole point of
    /// this type is that "no scope" must not be spellable as "global scope".
    pub fn as_opt(self) -> Option<i64> {
        match self {
            TenantScope::Global => None,
            TenantScope::Seller(id) => Some(id),
            TenantScope::Denied => Some(Self::DENIED_ID),
        }
    }

    /// True when the request must be refused before any data access.
    pub fn is_denied(self) -> bool {
        matches!(self, TenantScope::Denied)
    }
}

impl Role {
    /// Resolve the tenant scope for this role + its server-side seller binding.
    ///
    /// The binding is always server-derived (the session/account row), never a
    /// request field, so a caller cannot widen this by supplying an id.
    /// Ordering: the operator tier is global, the seller tier *requires* a
    /// binding, and the buyer tier holds no grant at all. The historic defect
    /// was the last edge of the seller arm -- `seller` without a binding fell
    /// through to the global view instead of being refused, so it is `Denied`
    /// here. Note the console (`web::moderation::seller_scope`) narrows a
    /// *bound* admin to its own seller; this REST resolver keeps every admin
    /// global, which is the pre-existing behaviour on this surface and is left
    /// unchanged rather than silently re-scoped.
    pub fn tenant_scope(self, seller_id: Option<i64>) -> TenantScope {
        match (self, seller_id) {
            // The seller tier *is* a tenant grant: without a binding there is
            // no tenant to act as, and widening to global is the bug.
            (Role::Seller, None) => TenantScope::Denied,
            (Role::Seller, Some(id)) => TenantScope::Seller(id),
            // The operator tier keeps its legitimate platform-wide view.
            (Role::Owner, _) | (Role::Admin, _) => TenantScope::Global,
            // The buyer tier is not a tenant principal. It holds no seller
            // grant, so resolving it to `Global` is a privilege inversion: an
            // unprivileged `user` reads wider than a bound `seller`. Nothing
            // legitimately depends on that widening here, because `seller_scope`
            // is private to this REST surface (`src/api.rs`): the buyer
            // storefront is `/api/v1/shop` (unauthenticated, passes `None`
            // explicitly) and the buyer console pages live in `web/` with their
            // own resolver. Redemption (`/codes/{code}/redeem`) and claiming
            // (`/invoices/{id}/claim`) derive scope from the code/invoice, not
            // from this call. Unknown role strings parse to `User`, so they
            // fail closed here as well.
            (Role::User, _) => TenantScope::Denied,
        }
    }
}

pub fn role_of(db: &Db, user_id: i64, owner_id: i64) -> Role {
    if user_id == owner_id {
        return Role::Owner;
    }
    if db.is_admin(user_id, None) {
        return Role::Admin;
    }
    if db.get_seller(user_id).is_some() {
        return Role::Seller;
    }
    Role::User
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::fs;

    #[test]
    fn test_role_of() {
        let path = "/tmp/opencode/test_role_of.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let owner_id = 999i64;
        // owner
        assert_eq!(role_of(&db, owner_id, owner_id), Role::Owner);
        // admin
        db.add_admin(100, Some("admin1"));
        assert_eq!(role_of(&db, 100, owner_id), Role::Admin);
        // seller
        db.upsert_seller(200, Some("seller1"), None);
        assert_eq!(role_of(&db, 200, owner_id), Role::Seller);
        // user
        assert_eq!(role_of(&db, 300, owner_id), Role::User);
    }

    #[test]
    fn test_role_owner() {
        let path = "/tmp/opencode/test_role_owner.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let owner_id = 777i64;
        assert_eq!(role_of(&db, owner_id, owner_id), Role::Owner);
    }

    #[test]
    fn test_role_admin() {
        let path = "/tmp/opencode/test_role_admin.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let owner_id = 999i64;
        db.add_admin(101, Some("admin1"));
        assert_eq!(role_of(&db, 101, owner_id), Role::Admin);
    }

    #[test]
    fn test_role_seller() {
        let path = "/tmp/opencode/test_role_seller.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let owner_id = 999i64;
        db.upsert_seller(201, Some("seller1"), None);
        assert_eq!(role_of(&db, 201, owner_id), Role::Seller);
    }

    #[test]
    fn test_role_user() {
        let path = "/tmp/opencode/test_role_user.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let owner_id = 999i64;
        assert_eq!(role_of(&db, 301, owner_id), Role::User);
    }

    #[test]
    fn test_role_ordering() {
        assert!(Role::Owner > Role::Admin);
        assert!(Role::Admin > Role::Seller);
        assert!(Role::Seller > Role::User);
    }

    #[test]
    fn test_from_role_str_mapping() {
        assert_eq!(Role::from_role_str("owner"), Role::Owner);
        assert_eq!(Role::from_role_str("admin"), Role::Admin);
        assert_eq!(Role::from_role_str("seller"), Role::Seller);
        assert_eq!(Role::from_role_str("user"), Role::User);
    }

    #[test]
    fn test_from_role_str_unknown_defaults_to_user() {
        for bad in ["", "nobody", "OWNER", "Admin", "admin ", " webuser"] {
            assert_eq!(
                Role::from_role_str(bad),
                Role::User,
                "unknown role {bad:?} must degrade to User"
            );
        }
    }

    #[test]
    fn test_has_level_ordering() {
        // Every role satisfies its own level and every level below it.
        assert!(Role::User.has_level(Role::User));
        assert!(Role::Seller.has_level(Role::User));
        assert!(Role::Seller.has_level(Role::Seller));
        assert!(Role::Admin.has_level(Role::Seller));
        assert!(Role::Owner.has_level(Role::Admin));
        assert!(Role::Owner.has_level(Role::Owner));
        // Nothing satisfies a level above it.
        assert!(!Role::User.has_level(Role::Seller));
        assert!(!Role::Seller.has_level(Role::Admin));
        assert!(!Role::Admin.has_level(Role::Owner));
        assert!(!Role::User.has_level(Role::Owner));
    }

    #[test]
    fn test_admin_passes_admin_gate_but_fails_owner_gate() {
        let admin = Role::from_role_str("admin");
        assert!(
            admin.has_level(Role::Admin),
            "admin must pass an Admin-level gate"
        );
        assert!(
            !admin.has_level(Role::Owner),
            "admin must fail an Owner-level gate"
        );
    }

    #[test]
    fn test_is_admin_role_compat_wrapper() {
        assert!(is_admin_role("owner"));
        assert!(is_admin_role("admin"));
        assert!(!is_admin_role("seller"));
        assert!(!is_admin_role("user"));
        assert!(!is_admin_role(""));
        assert!(!is_admin_role("bogus"));
    }
}
