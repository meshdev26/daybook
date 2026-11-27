//! Provider seams for external principals (future payment/delivery adapters).
//!
//! The tenant core in [`crate::roles`] is generic: [`Role::tenant_scope`] maps a
//! `(Role, Option<i64>)` pair into [`TenantScope`] without knowing where the
//! principal came from. Identity, however, is hard-wired to Telegram via
//! [`crate::roles::role_of`]. This module adds a resolution seam around that
//! logic without changing any existing call site: the default provider
//! delegates to `role_of`, so today's behavior is preserved exactly.

use crate::db::Db;
use crate::roles::{Role, TenantScope, role_of};

/// Resolve an external principal into the role + seller binding the tenant
/// core already understands.
///
/// `db` is passed in instead of stored so a provider stays stateless and
/// `Send + Sync`. The binding is always server-derived, never a request field.
/// `None` for a seller binding means "no tenant" and becomes
/// [`TenantScope::Denied`] via [`tenant_scope_for`], never a global view.
pub trait PrincipalProvider: Send + Sync {
    fn resolve(&self, db: &Db, principal_id: i64, owner_id: i64) -> (Role, Option<i64>);
}

/// The default provider: today's Telegram-only resolution, unchanged.
pub struct TelegramPrincipalProvider;

impl PrincipalProvider for TelegramPrincipalProvider {
    fn resolve(&self, db: &Db, principal_id: i64, owner_id: i64) -> (Role, Option<i64>) {
        // Delegate the decision itself to `role_of` so owner/admin/seller/user
        // precedence stays identical to the pre-seam behavior.
        let role = role_of(db, principal_id, owner_id);
        // Only the seller tier carries a tenant binding; owner/admin are
        // global and user holds no grant, so both keep `None`.
        let seller_id = match role {
            Role::Seller => db.get_seller(principal_id).map(|(id, _, _)| id),
            _ => None,
        };
        (role, seller_id)
    }
}

/// Convert a resolved principal into the existing tenant scope.
///
/// This is the seam future payment/delivery adapters will use: they resolve
/// their own principal to `(Role, Option<i64>)` and hand it here, so the
/// fail-closed mapping stays single-sourced in `Role::tenant_scope`.
pub fn tenant_scope_for(principal: (Role, Option<i64>)) -> TenantScope {
    let (role, seller_id) = principal;
    role.tenant_scope(seller_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const OWNER_ID: i64 = 999;

    #[test]
    fn test_telegram_provider_owner() {
        let path = "/tmp/opencode/test_adapters_owner.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let provider = TelegramPrincipalProvider;
        let principal = provider.resolve(&db, OWNER_ID, OWNER_ID);
        assert_eq!(principal, (Role::Owner, None));
        assert_eq!(principal.0, role_of(&db, OWNER_ID, OWNER_ID));
        assert_eq!(tenant_scope_for(principal), TenantScope::Global);
    }

    #[test]
    fn test_telegram_provider_admin() {
        let path = "/tmp/opencode/test_adapters_admin.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.add_admin(100, Some("admin1"));
        let provider = TelegramPrincipalProvider;
        let principal = provider.resolve(&db, 100, OWNER_ID);
        assert_eq!(principal, (Role::Admin, None));
        assert_eq!(principal.0, role_of(&db, 100, OWNER_ID));
        assert_eq!(tenant_scope_for(principal), TenantScope::Global);
    }

    #[test]
    fn test_telegram_provider_seller() {
        let path = "/tmp/opencode/test_adapters_seller.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(200, Some("seller1"), None);
        let provider = TelegramPrincipalProvider;
        let principal = provider.resolve(&db, 200, OWNER_ID);
        assert_eq!(principal, (Role::Seller, Some(200)));
        assert_eq!(principal.0, role_of(&db, 200, OWNER_ID));
        assert_eq!(tenant_scope_for(principal), TenantScope::Seller(200));
    }

    #[test]
    fn test_telegram_provider_user() {
        let path = "/tmp/opencode/test_adapters_user.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let provider = TelegramPrincipalProvider;
        let principal = provider.resolve(&db, 300, OWNER_ID);
        assert_eq!(principal, (Role::User, None));
        assert_eq!(principal.0, role_of(&db, 300, OWNER_ID));
        assert_eq!(tenant_scope_for(principal), TenantScope::Denied);
    }

    #[test]
    fn test_telegram_provider_unknown() {
        let path = "/tmp/opencode/test_adapters_unknown.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let provider = TelegramPrincipalProvider;
        let principal = provider.resolve(&db, 404404, OWNER_ID);
        assert_eq!(principal, (Role::User, None));
        assert_eq!(principal.0, role_of(&db, 404404, OWNER_ID));
        assert_eq!(tenant_scope_for(principal), TenantScope::Denied);
    }

    #[test]
    fn test_telegram_provider_matches_role_of() {
        let path = "/tmp/opencode/test_adapters_equivalent.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.add_admin(100, Some("admin1"));
        db.upsert_seller(200, Some("seller1"), None);
        // Admin wins over seller when both rows exist for one principal.
        db.add_admin(400, Some("dual"));
        db.upsert_seller(400, Some("dual"), None);
        let provider = TelegramPrincipalProvider;
        for id in [OWNER_ID, 100, 200, 300, 404404, 400] {
            let principal = provider.resolve(&db, id, OWNER_ID);
            assert_eq!(
                principal.0,
                role_of(&db, id, OWNER_ID),
                "provider role for {id} must match role_of"
            );
        }
        // The admin+seller principal keeps the admin's unbound shape.
        assert_eq!(provider.resolve(&db, 400, OWNER_ID), (Role::Admin, None));
        // The seller binding comes straight from the server-side seller row.
        assert_eq!(db.get_seller(200).map(|row| row.0), Some(200));
        assert_eq!(provider.resolve(&db, 200, OWNER_ID).1, Some(200));
    }

    #[test]
    fn test_tenant_scope_for_seller_without_binding_is_denied() {
        assert_eq!(tenant_scope_for((Role::Seller, None)), TenantScope::Denied);
        assert_eq!(
            tenant_scope_for((Role::Seller, None)).as_opt(),
            Some(TenantScope::DENIED_ID)
        );
    }

    #[test]
    fn test_tenant_scope_for_delegates_to_role_tenant_scope() {
        for (principal, expected) in [
            ((Role::Owner, None), TenantScope::Global),
            ((Role::Owner, Some(7)), TenantScope::Global),
            ((Role::Admin, None), TenantScope::Global),
            ((Role::Admin, Some(7)), TenantScope::Global),
            ((Role::Seller, Some(7)), TenantScope::Seller(7)),
            ((Role::Seller, None), TenantScope::Denied),
            ((Role::User, None), TenantScope::Denied),
            ((Role::User, Some(7)), TenantScope::Denied),
        ] {
            assert_eq!(tenant_scope_for(principal), expected);
            assert_eq!(
                tenant_scope_for(principal),
                principal.0.tenant_scope(principal.1),
                "tenant_scope_for must delegate to Role::tenant_scope"
            );
        }
    }
}
