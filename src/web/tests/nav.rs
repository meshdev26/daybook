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
        "/blocked-words",
        "/content-policy",
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
    // seller sees finance, api keys, abandoned carts and the Blocked words
    // capability page, but never the owner-only global policy.
    for p in ["finance", "keys", "carts", "blocked-words"] {
        assert!(
            body.contains(&format!(r#"href="/{}""#, p)),
            "seller missing {} in:\n{}",
            p,
            body
        );
    }
    // seller does not see owner-only
    for p in [
        "bots",
        "content-policy",
        "identity",
        "ai",
        "crypto",
        "admins",
        "sellers",
        "webusers",
        "settings",
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
        "monitor",
        "bots",
        "blocked-words",
        "content-policy",
        "identity",
        "ai",
        "crypto",
        "finance",
        "admins",
        "sellers",
        "webusers",
        "keys",
        "carts",
        "settings",
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

// ---- multibot foundation: product scope + gated nav (Step 3) ------------

/// The default build keeps the shipped 27 rows and every path byte-identical:
/// no existing item is tagged with a capability or a product type, so both
/// new filters are no-ops here. A capability/page may only be hidden when a
/// builder opts out of that feature or selects a narrower product type.
#[test]
fn default_build_nav_rows_are_untagged_and_paths_unchanged() {
    assert_eq!(NAV_ITEMS.len(), 27);
    for item in NAV_ITEMS {
        assert_eq!(
            item.feature, None,
            "{} must stay in the default build",
            item.id
        );
        assert_eq!(
            item.product_types, None,
            "{} must not be type-filtered",
            item.id
        );
    }
    // Capability flags resolve from Cargo features; unknown never enables.
    assert!(capability_enabled("shop_bot")); // default feature
    assert!(!capability_enabled("movie_bot")); // not compiled by default
    assert!(!capability_enabled("not_a_capability"));
    assert_eq!(product_type_label("movie_bot"), "Movie bots");
    assert_eq!(product_type_label("raw_slug"), "Other products");
}

/// A nav item whose capability was not compiled is absent. This exercises
/// the exact filter `rail_nav` applies; a `--no-default-features --features
/// movie_bot` build makes the mirror case (a shop-gated item absent) true.
#[test]
fn feature_gated_item_absent_when_not_compiled() {
    let movie_only = NavItem {
        id: "movie-catalog",
        label: "Movie catalog",
        path: "movie-catalog",
        group: "Offerings",
        roles: NavRole::Owner,
        titles: &["Movie catalog"],
        pref: None,
        feature: Some("movie_bot"),
        product_types: Some(&["movie_bot"]),
    };
    // Default build: movie_bot is not compiled -> the item is filtered out.
    assert!(!movie_only.feature.is_none_or(capability_enabled));
    // Runtime type filter is a view filter only: the selected movie product
    // keeps the item; an unscoped owner ("All products") also keeps it.
    assert!(nav_item_type_ok(&movie_only, Some("movie_bot")));
    assert!(nav_item_type_ok(&movie_only, None));
    assert!(!nav_item_type_ok(&movie_only, Some("shop_bot")));
    // An untagged item is never removed by either filter.
    let core = &NAV_ITEMS[0];
    assert!(core.feature.is_none_or(capability_enabled));
    assert!(nav_item_type_ok(core, Some("shop_bot")));
}

#[tokio::test]
async fn product_scope_owner_has_type_facet_and_optgroups() {
    let (state, _dir) = make_state().await;
    state.db.create_bot("cinema", 1, None, "");
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("owner").unwrap();
    let sess = Session {
        account_id: acc,
        role: "owner".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let body = html_page(&state, Some(&sess), "Dashboard", "<p>x</p>");
    assert!(
        body.contains(r#"class="bot-scope product-scope""#),
        "{body}"
    );
    assert!(
        body.contains(r#"<label for="scope-type">Product type</label>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<label for="scope-product">Product</label>"#),
        "{body}"
    );
    assert!(body.contains(r#"<select id="scope-type""#), "{body}");
    assert!(
        body.contains(r#"<option value="">All types</option>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<option value="">All products</option>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<option value="shop_bot">Shop bots</option>"#),
        "{body}"
    );
    assert!(body.contains(r#"<optgroup label="Shop bots">"#), "{body}");
    assert!(body.contains(r#"data-type="shop_bot""#), "{body}");
    assert!(body.contains(">Switch</button>"), "{body}");
    // The old label/facet must be gone, and the facet is client-side only:
    // no `name` on the type select and no forged seller field in the form.
    assert!(!body.contains("All bots (primary)"), "{body}");
    assert!(!body.contains(r#"name="seller_id""#), "{body}");
    assert!(!body.contains(r#"name="product_type""#), "{body}");
    let type_tag = body
        .split(r#"<select id="scope-type">"#)
        .nth(1)
        .and_then(|rest| rest.split("</select>").next())
        .unwrap_or("");
    assert!(!type_tag.contains("name="), "{type_tag}");
}

#[tokio::test]
async fn product_scope_seller_pinned_buyer_absent() {
    let (state, _dir) = make_state().await;
    state
        .db
        .create_web_account("seller", "secret", "seller", Some(42))
        .await
        .unwrap();
    state
        .db
        .create_web_account("buyer", "secret", "user", None)
        .await
        .unwrap();
    let (sacc, _u, _h, _r, _s) = state.db.get_web_account_by_username("seller").unwrap();
    let seller = Session {
        account_id: sacc,
        role: "seller".to_string(),
        seller_id: Some(42),
        selected_bot_id: None,
    };
    let seller_body = html_page(&state, Some(&seller), "Dashboard", "<p>x</p>");
    assert!(
        seller_body.contains(r#"aria-label="Active product""#),
        "{seller_body}"
    );
    assert!(
        seller_body.contains(r#"<span class="help">Product</span>"#),
        "{seller_body}"
    );
    assert!(
        !seller_body.contains(r#"<select id="scope-product""#),
        "{seller_body}"
    );
    assert!(
        !seller_body.contains(r#"aria-label="Active bot""#),
        "{seller_body}"
    );

    let (bacc, _u, _h, _r, _s) = state.db.get_web_account_by_username("buyer").unwrap();
    let buyer = Session {
        account_id: bacc,
        role: "user".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let buyer_body = html_page(&state, Some(&buyer), "Dashboard", "<p>x</p>");
    assert!(!buyer_body.contains("bot-scope"), "{buyer_body}");
    assert!(!buyer_body.contains("scope-type"), "{buyer_body}");
}

#[tokio::test]
async fn scope_control_375px_stacked_markup_and_css() {
    let (state, _dir) = make_state().await;
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("owner").unwrap();
    let sess = Session {
        account_id: acc,
        role: "owner".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let body = html_page(&state, Some(&sess), "Dashboard", "<p>x</p>");
    // Two labeled selects + a full-width Switch: the markup a 375px stack
    // binds to. Ordering is Type label -> type select -> Product label ->
    // product select -> Switch, so no side-by-side controls can form.
    let type_label = body
        .find(r#"<label for="scope-type">Product type</label>"#)
        .unwrap();
    let type_sel = body.find(r#"<select id="scope-type""#).unwrap();
    let product_label = body
        .find(r#"<label for="scope-product">Product</label>"#)
        .unwrap();
    let product_sel = body.find(r#"<select id="scope-product""#).unwrap();
    let switch = body.find(">Switch</button>").unwrap();
    assert!(type_label < type_sel && type_sel < product_label);
    assert!(product_label < product_sel && product_sel < switch);

    let app = router(state);
    let resp = app.oneshot(get_req("/assets/app.css")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let css = body_string(resp).await;
    assert!(css.contains(".rail .product-scope"), "{css}");
    assert!(css.contains("flex-direction: column"), "{css}");
    assert!(css.contains("min-height: 44px"), "{css}");
}

// ---- Step 4.3: feature-gated moderation nav items -------------------------

/// The 27 shipped group rows are untouched; capability rows live in their own
/// table and are all tagged with `moderation` so they disappear in a build
/// that opts out of the capability. Buyers never see either one.
#[test]
fn moderation_nav_rows_are_gated_and_separate_from_core_table() {
    assert_eq!(NAV_ITEMS.len(), 27);
    assert!(NAV_ITEMS.iter().all(|item| item.feature.is_none()));
    assert_eq!(FEATURE_NAV_ITEMS.len(), 2);
    for item in FEATURE_NAV_ITEMS {
        assert_eq!(item.feature, Some("moderation"), "{}", item.id);
        assert!(!item.roles.visible("user"), "{}", item.id);
        // Compiled-in by default; a movie-only build filters it out.
        assert!(item.feature.is_none_or(capability_enabled));
        assert!(!item.feature.is_none_or(|f| f == "movie_bot"));
    }
    let ids: Vec<&str> = FEATURE_NAV_ITEMS.iter().map(|i| i.id).collect();
    assert_eq!(ids, vec!["blocked-words", "content-policy"]);
    let anchors: Vec<(&str, &str)> = FEATURE_NAV_AFTER
        .iter()
        .map(|(id, anchor)| (*id, *anchor))
        .collect();
    assert_eq!(
        anchors,
        vec![("blocked-words", "bots"), ("content-policy", "ai")]
    );
    assert_eq!(active_nav_id("Blocked words"), Some("blocked-words"));
    assert_eq!(active_nav_id("Content policy"), Some("content-policy"));
}

/// A capability row is injected after its anchor (Content policy beside AI),
/// and the active state is id-keyed with `aria-current="page"`.
#[tokio::test]
async fn moderation_nav_placement_and_active_state() {
    let (state, _dir) = make_state().await;
    state.db.set_config(
        "sidebar_items",
        r#"{"ai":true,"crypto":true,"admins":true,"sellers":true,"webusers":true}"#,
    );
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("owner").unwrap();
    let sess = Session {
        account_id: acc,
        role: "owner".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let body = rail_nav(&state, "Content policy", Some(&sess));
    let bots = body.find(">Bots</a>").unwrap();
    let blocked = body.find(">Blocked words</a>").unwrap();
    let products = body.find(">Products</a>").unwrap();
    assert!(products < bots && bots < blocked, "{body}");
    let ai = body.find(">AI</a>").unwrap();
    let policy = body.find(">Content policy</a>").unwrap();
    let crypto = body.find(">Crypto</a>").unwrap();
    assert!(ai < policy && policy < crypto, "{body}");
    assert!(body.contains(r#"href="/blocked-words""#), "{body}");
    let active = body
        .split(r#"href="/content-policy""#)
        .nth(1)
        .unwrap_or("")
        .split('>')
        .next()
        .unwrap_or("");
    assert!(active.contains("aria-current=\"page\""), "{active}");
    // No seventh group was introduced.
    assert_eq!(body.matches("data-nav-group=\"").count(), 6);
}

/// The 375px rules for moderation forms reuse the shipped token contract and
/// enforce ≥44px targets with a stacked layout at the drawer breakpoint.
#[tokio::test]
async fn moderation_forms_have_375px_targets_and_stacked_rules() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/assets/app.css")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let css = body_string(resp).await;
    assert!(css.contains(".mod-form"), "{css}");
    assert!(css.contains(".moderation-override"), "{css}");
    assert!(css.contains("min-height: 44px"), "{css}");
    assert!(css.contains(".mod-row-form"), "{css}");
}
