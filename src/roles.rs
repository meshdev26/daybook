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
