// Navigation IA redesign tests (IA_REDESIGN_SPEC §2/§10/§11).
use super::*;
use axum::http::StatusCode;
use tower::ServiceExt;

const EXPECTED_GROUPS: &[&str] = &[
    "Overview",
    "Offerings",
    "Sales",
    "Fulfill",
    "Communicate",
    "Workspace",
];

// (group, id, label, path, role-set tag)
// role tags: "all", "owner", "seller_admin_owner"
const EXPECTED: &[(&str, &str, &str, &str, &str)] = &[
    ("Overview", "dashboard", "Dashboard", "dashboard", "all"),
    ("Overview", "analytics", "Analytics", "analytics", "all"),
    ("Overview", "monitor", "Monitor", "monitor", "owner"),
    ("Offerings", "products", "Products", "products", "all"),
    ("Offerings", "plans", "Plans", "plans", "all"),
    ("Offerings", "channels", "Channels", "channels", "all"),
    ("Offerings", "bots", "Bots", "bots", "owner"),
    ("Sales", "orders", "Orders", "orders", "all"),
    ("Sales", "customers", "Customers", "customers", "all"),
    ("Sales", "codes", "Codes", "codes", "all"),
    ("Sales", "promos", "Promos", "promos", "all"),
    ("Sales", "referrals", "Referrals", "referrals", "all"),
    ("Sales", "cart", "My cart", "cart", "all"),
    (
        "Sales",
        "carts",
        "Abandoned carts",
        "carts",
        "seller_admin_owner",
    ),
    ("Fulfill", "subs", "Subscriptions", "subs", "all"),
    ("Fulfill", "bookings", "Bookings", "bookings", "all"),
    ("Fulfill", "tickets", "Tickets", "tickets", "all"),
    ("Communicate", "posts", "Posts", "posts", "all"),
    ("Communicate", "broadcast", "Broadcast", "broadcast", "all"),
    ("Workspace", "identity", "Identity", "identity", "owner"),
    ("Workspace", "ai", "AI", "ai", "owner"),
    ("Workspace", "crypto", "Crypto", "crypto", "owner"),
    (
        "Workspace",
        "finance",
        "Finance",
        "finance",
        "seller_admin_owner",
    ),
    ("Workspace", "admins", "Admins", "admins", "owner"),
    ("Workspace", "sellers", "Sellers", "sellers", "owner"),
    ("Workspace", "webusers", "Web users", "webusers", "owner"),
    (
        "Workspace",
        "apikeys",
        "API keys",
        "keys",
        "seller_admin_owner",
    ),
];

#[test]
fn nav_table_shape() {
    assert_eq!(NAV_ITEMS.len(), 27);
    // unique ids
    let mut ids: Vec<&str> = NAV_ITEMS.iter().map(|i| i.id).collect();
    ids.sort();
    let mut uniq = ids.clone();
    uniq.dedup();
    assert_eq!(ids.len(), uniq.len(), "ids must be unique");
    // group order
    let mut groups: Vec<&str> = Vec::new();
    for item in NAV_ITEMS {
        if !groups.contains(&item.group) {
            groups.push(item.group);
        }
    }
    assert_eq!(groups, EXPECTED_GROUPS);
    // rows match spec
    assert_eq!(NAV_ITEMS.len(), EXPECTED.len());
    for (item, exp) in NAV_ITEMS.iter().zip(EXPECTED.iter()) {
        assert_eq!(item.group, exp.0, "group mismatch for {}", item.id);
        assert_eq!(item.id, exp.1, "id mismatch");
        assert_eq!(item.label, exp.2, "label mismatch for {}", item.id);
        assert_eq!(item.path, exp.3, "path mismatch for {}", item.id);
        let tag = match item.roles {
            NavRole::All => "all",
            NavRole::Owner => "owner",
            NavRole::SellerAdminOwner => "seller_admin_owner",
        };
        assert_eq!(tag, exp.4, "role mismatch for {}", item.id);
    }
}

#[test]
fn active_nav_id_mapping() {
    assert_eq!(active_nav_id("Dashboard"), Some("dashboard"));
    assert_eq!(active_nav_id("Cart"), Some("cart"));
    assert_eq!(active_nav_id("Carts"), Some("carts"));
    assert_eq!(active_nav_id("API keys"), Some("apikeys"));
    assert_eq!(active_nav_id("Bots"), Some("bots"));
    assert_eq!(active_nav_id("Add bot"), Some("bots"));
    assert_eq!(active_nav_id("Reviews"), Some("referrals"));
    assert_eq!(active_nav_id("Restarting"), None);
    // Settings is owner-only and lives in Utilities, but still resolves to a
    // stable id so its active state is not title/label driven.
    assert_eq!(active_nav_id("Settings"), Some("settings"));
}

#[tokio::test]
async fn nav_role_visibility_owner() {
    let (state, _dir) = make_state().await;
    // Workspace items (AI/Crypto/Admins/Sellers/Web users) are pref-gated;
    // enable them so "owner sees all items" is exercised.
    state.db.set_config(
        "sidebar_items",
        r#"{"ai":true,"crypto":true,"admins":true,"sellers":true,"webusers":true}"#,
    );
    // build a session struct manually
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("owner").unwrap();
    let sess = Session {
        account_id: acc,
        role: "owner".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let body = rail_nav(&state, "Dashboard", Some(&sess));
    // owner sees all incl. Settings in utilities
    for p in [
        "/bots",
        "/identity",
        "/ai",
        "/crypto",
        "/finance",
        "/admins",
        "/sellers",
        "/webusers",
        "/keys",
        "/carts",
        "/settings",
    ] {
        // paths are rendered without leading slash in href? href="{base}/bots"
        assert!(
            body.contains(&format!(r#"href="/{}""#, p.trim_start_matches('/'))),
            "owner missing {} in:\n{}",
            p,
            body
        );
    }
}

#[tokio::test]
async fn nav_role_visibility_seller() {
    let (state, _dir) = make_state().await;
    state
        .db
        .create_web_account("seller", "secret", "seller", Some(42))
        .await
        .unwrap();
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("seller").unwrap();
    let sess = Session {
        account_id: acc,
        role: "seller".to_string(),
        seller_id: Some(42),
        selected_bot_id: None,
    };
    let body = rail_nav(&state, "Dashboard", Some(&sess));
    // seller sees finance, api keys, abandoned carts
    for p in ["finance", "keys", "carts"] {
        assert!(
            body.contains(&format!(r#"href="/{}""#, p)),
            "seller missing {} in:\n{}",
            p,
            body
        );
    }
    // seller does not see owner-only
    for p in [
        "bots", "identity", "ai", "crypto", "admins", "sellers", "webusers", "settings",
    ] {
        assert!(
            !body.contains(&format!(r#"href="/{}""#, p)),
            "seller should not see {} in:\n{}",
            p,
            body
        );
    }
}

#[tokio::test]
async fn nav_role_visibility_buyer() {
    let (state, _dir) = make_state().await;
    state
        .db
        .create_web_account("buyer", "secret", "user", None)
        .await
        .unwrap();
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("buyer").unwrap();
    let sess = Session {
        account_id: acc,
        role: "user".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let body = rail_nav(&state, "Dashboard", Some(&sess));
    let expected = [
        "dashboard",
        "channels",
        "plans",
        "products",
        "codes",
        "promos",
        "referrals",
        "orders",
        "customers",
        "cart",
        "subs",
        "bookings",
        "tickets",
        "analytics",
        "posts",
        "broadcast",
    ];
    for p in expected {
        assert!(
            body.contains(&format!(r#"href="/{}""#, p)),
            "buyer missing {} in:\n{}",
            p,
            body
        );
    }
    let forbidden = [
        "monitor", "bots", "identity", "ai", "crypto", "finance", "admins", "sellers", "webusers",
        "keys", "carts", "settings",
    ];
    for p in forbidden {
        assert!(
            !body.contains(&format!(r#"href="/{}""#, p)),
            "buyer should not see {} in:\n{}",
            p,
            body
        );
    }
}

#[tokio::test]
async fn nav_markup_basics() {
    let (state, _dir) = make_state().await;
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("owner").unwrap();
    let sess = Session {
        account_id: acc,
        role: "owner".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let body = rail_nav(&state, "Dashboard", Some(&sess));
    // skip link is in html_page, not rail; check aria attrs
    assert!(body.contains("aria-expanded"), "{body}");
    assert!(
        body.contains("aria-controls=\"nav-panel-overview\""),
        "{body}"
    );
    assert!(body.contains("data-nav-group=\"overview\""), "{body}");
    assert!(body.contains("data-current=\"true\""), "{body}");
    assert!(body.contains("aria-current=\"page\""), "{body}");
    // footer div gone is checked at page level
    let app_body = html_page(&state, Some(&sess), "Dashboard", "<p>x</p>");
    assert!(app_body.contains("skip-link"), "{app_body}");
    assert!(!app_body.contains(r#"<div class="foot">"#), "{app_body}");
}

#[tokio::test]
async fn nav_page_smoke() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/dashboard", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("skip-link"), "{body}");
}
