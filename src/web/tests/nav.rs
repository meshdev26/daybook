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

/// Admin sits above Seller in the ordered Role ladder: it keeps the
/// seller-tier rows (including the Blocked words capability row) but still
/// never sees owner-only rows. The check is by level, not by role string.
#[tokio::test]
async fn nav_role_visibility_admin() {
    let (state, _dir) = make_state().await;
    state
        .db
        .create_web_account("admin", "secret", "admin", None)
        .await
        .unwrap();
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("admin").unwrap();
    let sess = Session {
        account_id: acc,
        role: "admin".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let body = rail_nav(&state, "Dashboard", Some(&sess));
    // Admin outranks Seller: same non-owner tier, including Blocked words.
    for p in ["finance", "keys", "carts", "blocked-words"] {
        assert!(
            body.contains(&format!(r#"href="/{}""#, p)),
            "admin missing {} in:\n{}",
            p,
            body
        );
    }
    // Owner-only rows stay hidden from admin.
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
            "admin should not see {} in:\n{}",
            p,
            body
        );
    }
}

/// The nav visibility predicate consumes Role levels directly, so unknown
/// session strings degrade to User (least privilege) and cannot unlock rows.
#[test]
fn nav_visibility_uses_ordered_role_levels() {
    assert!(NavRole::Owner.visible(Role::Owner));
    assert!(!NavRole::Owner.visible(Role::Admin));
    assert!(!NavRole::Owner.visible(Role::Seller));
    assert!(!NavRole::Owner.visible(Role::User));
    for role in [Role::Seller, Role::Admin, Role::Owner] {
        assert!(NavRole::SellerAdminOwner.visible(role));
    }
    assert!(!NavRole::SellerAdminOwner.visible(Role::User));
    assert!(NavRole::All.visible(Role::User));
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
        assert!(!item.roles.visible(Role::User), "{}", item.id);
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

/// B1 (WCAG 2.2 AA 2.1.1 / 2.4.3 / 4.1.2 / 1.3.1): at 375px the rail is
/// flattened and the group toggles are hidden. A non-current group must stay
/// navigable — APP_JS forces every panel open on mobile, so no visible link is
/// `inert` or `aria-hidden` and every `<a href>` stays keyboard-focusable.
#[tokio::test]
async fn mobile_375px_nav_links_are_never_inert_or_hidden() {
    let (state, _dir) = make_state().await;
    let (acc, _u, _h, _r, _s) = state.db.get_web_account_by_username("owner").unwrap();
    // A seller at the drawer breakpoint: the B1 report's exact scenario.
    let sess = Session {
        account_id: acc,
        role: "seller".to_string(),
        seller_id: Some(41),
        selected_bot_id: None,
    };
    let nav = rail_nav(&state, "Dashboard", Some(&sess));

    // The CSS that flattens every group at 375px must still ship, including a
    // stale-class fallback for the no-JS / pre-JS paint.
    let app = router(state.clone());
    let css = body_string(
        app.clone()
            .oneshot(get_req("/assets/app.css"))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        css.contains(".nav-group-toggle { display: none; }"),
        "{css}"
    );
    assert!(
        css.contains(".nav-group, .nav-group-panel { display: contents; }"),
        "{css}"
    );
    assert!(
        css.contains(
            ".nav-group.is-collapsed .nav-group-panel { max-height: none; overflow: visible; }"
        ),
        "{css}"
    );

    // APP_JS: force open on mobile, re-apply when the breakpoint is crossed,
    // and refuse a phantom collapse from an invisible toggle.
    let js = super::super::html::APP_JS;
    assert!(js.contains("if (isMobile()) open = true;"), "{js}");
    assert!(
        js.contains("mq.addEventListener('change', applyStored)"),
        "{js}"
    );
    assert!(js.contains("if (isMobile()) return;"), "{js}");

    // DOM-equivalent assertion on the markup the mobile JS enhances: no panel
    // or link carries inert / aria-hidden="true", and every panel link is a
    // native, focusable <a href> (no tabindex="-1").
    assert!(!nav.contains("inert"), "{nav}");
    let panels: Vec<&str> = nav
        .split("<div class=\"nav-group-panel\"")
        .skip(1)
        .map(|chunk| chunk.split("</div>").next().unwrap_or(""))
        .collect();
    assert!(panels.len() >= 2, "expected several nav groups: {nav}");
    let mut links = 0usize;
    for panel in panels {
        assert!(!panel.contains("inert"), "{panel}");
        assert!(!panel.contains("aria-hidden=\"true\""), "{panel}");
        for anchor in panel.split("<a ").skip(1) {
            let tag = anchor.split('>').next().unwrap_or("");
            assert!(tag.contains("href="), "nav link without href: {tag}");
            assert!(
                !tag.contains("tabindex=\"-1\""),
                "unfocusable nav link: {tag}"
            );
            links += 1;
        }
    }
    assert!(links >= 5, "expected seller nav links, got {links}: {nav}");
}

/// Scroll memory (full-navigation console): the 5th APP_JS IIFE saves and
/// restores the scroll offset per URL in sessionStorage, but leaves back/
/// forward native and never touches focus management or scrollRestoration.
#[tokio::test]
async fn nav_scroll_memory_js_and_shell_markup_unchanged() {
    let js = super::super::html::APP_JS;
    for needle in [
        "tg.scroll.v1",
        "sessionStorage",
        "location.pathname + location.search",
        "pagehide",
        "beforeunload",
        "performance.getEntriesByType('navigation')",
        "back_forward",
        "requestAnimationFrame",
        // Window restore is now an instant scrollTo (the rail has its own
        // memory test below); the old `window.scrollTo(0, y)` form is gone.
        "window.scrollTo({ top: y, behavior: 'instant' })",
    ] {
        assert!(js.contains(needle), "missing `{needle}` in APP_JS");
    }
    // NOTE: the mandated snippet mentions `scrollRestoration` in a comment,
    // so a literal `!js.contains("scrollRestoration")` cannot pass. Assert the
    // actual invariant instead: the property is never read or assigned, so
    // back/forward scroll stays native.
    assert!(
        !js.contains("history.scrollRestoration")
            && !js.contains("scrollRestoration =")
            && !js.contains("scrollRestoration=")
            && js.matches("scrollRestoration").count() == 1,
        "back/forward scroll must stay native (no scrollRestoration access)"
    );
    // The existing focus-on-load / skip-link behavior is untouched.
    assert!(js.contains("[data-focus-on-load]"), "{js}");

    let (state, _dir) = make_state().await;
    let body = html_page(&state, None, "Dashboard", "<p>x</p>");
    assert!(
        body.contains(r#"<a class="skip-link" href='#main'>"#),
        "{body}"
    );
    assert!(body.contains(r#"<main class="main" id="main">"#), "{body}");
    // No tabindex was added to the skip link or the main landmark.
    assert!(
        !body.contains(r#"class="skip-link" href='#main' tabindex"#),
        "{body}"
    );
    assert!(
        !body.contains(r#"<main class="main" id="main" tabindex"#),
        "{body}"
    );
}

/// Rail-scroll CSS contract (screenshot fix): on desktop the rail is its own
/// scroll container — never the page — and it carries a stable scrollbar
/// gutter so the nav does not jump horizontally when it starts to overflow.
/// At the drawer breakpoint the same rail must hand scrolling back to the
/// document (`overflow-y: visible`), or a wrapping header becomes a second,
/// unreachable scroll area.
#[tokio::test]
async fn rail_is_a_scoped_desktop_scroll_container() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(get_req("/assets/app.css"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let css = body_string(resp).await;

    let start = css.find(".rail {").expect("the base .rail rule is served");
    let end = css[start..]
        .find('}')
        .map(|i| start + i)
        .unwrap_or(css.len());
    let base = &css[start..end];
    for needle in [
        "overflow-y: auto",
        "overscroll-behavior-y: contain",
        "scrollbar-gutter: stable",
    ] {
        assert!(base.contains(needle), "missing `{needle}` in .rail: {base}");
    }

    // Whitespace-tolerant media-query assertions: the contract is the
    // desktop-only `flex-shrink:0` pin and the mobile `overflow-y:visible`
    // override inside a `@media (max-width: 900px)` block.
    let compact: String = css.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        compact.contains("@media(min-width:901px){.rail>*{flex-shrink:0;}}"),
        "desktop-only rail child pin missing: {css}"
    );
    let mobile_override = css
        .match_indices("@media (max-width: 900px) {")
        .any(|(i, _)| {
            let mq = &css[i..];
            let end = mq.find("\n}").unwrap_or(mq.len());
            let mq = &mq[..end];
            mq.split(".rail {").skip(1).any(|chunk| {
                chunk
                    .split('}')
                    .next()
                    .unwrap_or("")
                    .contains("overflow-y: visible")
            })
        });
    assert!(
        mobile_override,
        "mobile .rail must restore document scrolling: {css}"
    );
}

/// The sixth APP_JS IIFE remembers the rail's scrollTop per desktop session
/// across full navigations, but never touches history.scrollRestoration and
/// never forces a scroll on the mobile rail. The window scroll restoration
/// (existing `tg.scroll.v1` IIFE) must be `instant`, not smooth, so a reload
/// does not animate from the top.
#[tokio::test]
async fn rail_scroll_memory_is_desktop_only_and_native_history_is_untouched() {
    let js = super::super::html::APP_JS;
    for needle in [
        "tg.rail.scroll.v1",
        "sessionStorage",
        "scrollTop",
        "matchMedia('(min-width: 901px)')",
        "pagehide",
        "beforeunload",
    ] {
        assert!(js.contains(needle), "missing `{needle}` in APP_JS");
    }
    let compact: String = js.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        compact.contains("behavior:'instant'"),
        "scroll restore must be instant: {js}"
    );
    assert!(
        compact.contains("scrollTo({top:y,behavior:'instant'})"),
        "window restore must use instant scrollTo: {js}"
    );

    let rail = js
        .split("var KEY = 'tg.rail.scroll.v1';")
        .nth(1)
        .expect("rail scroll IIFE exists");
    assert!(
        rail.contains("scrollTop"),
        "rail offset must be saved: {rail}"
    );
    assert!(
        rail.contains("sessionStorage"),
        "rail offset must be session-scoped, not shared across tabs: {rail}"
    );
    assert!(
        rail.contains("getItem") && rail.contains("setItem"),
        "rail offset must round-trip through Web Storage: {rail}"
    );
    // `< 901px` returns early on save and restore: the flat mobile header is
    // never pinned to a stale desktop offset.
    assert!(
        rail.contains("isDesktop()"),
        "rail memory must be gated on desktop: {rail}"
    );
    assert!(
        !js.contains("history.scrollRestoration")
            && !js.contains("scrollRestoration =")
            && !js.contains("scrollRestoration="),
        "back/forward scroll must stay native (no scrollRestoration access)"
    );
}

/// Regression: an <optgroup> has no `.options` property (only <select> does).
/// Iterating `g.options` threw inside the top-level product-filter IIFE and
/// aborted the whole deferred script, silently preventing the later theme,
/// per-URL window-scroll and rail-scroll IIFEs from ever running — which is why
/// sidebar scroll memory never worked. Pin the valid option collection and the
/// presence of all six top-level IIFEs so a future crash cannot drop them.
#[test]
fn app_js_optgroup_iteration_does_not_abort_later_iifes() {
    let js = super::super::html::APP_JS;

    // The optgroup loop must enumerate real <option> elements, never the
    // nonexistent `.options` collection on HTMLOptGroupElement.
    assert!(
        js.contains("g.querySelectorAll('option')") || js.contains("g.children"),
        "optgroup loop must use querySelectorAll('option') or g.children: {js}"
    );
    assert!(
        !js.contains("forEach.call(g.options"),
        "optgroup loop must never iterate g.options (optgroups have no such property): {js}"
    );

    // All six APP_JS IIFEs must ship. Losing any of the later ones was the
    // silent consequence of the top-level throw above.
    for marker in [
        ".secret-toggle",       // IIFE 1: secret reveal + focus-on-load
        "tg.sidebar.groups.v1", // IIFE 2: sidebar group open/closed state
        "scope-type",           // IIFE 3: product/type facet (the fixed loop)
        "theme-toggle",         // IIFE 4: mirror resolved theme onto controls
        "tg.scroll.v1",         // IIFE 5: per-URL window scroll memory
        "tg.rail.scroll.v1",    // IIFE 6: desktop rail scroll memory
    ] {
        assert!(js.contains(marker), "missing APP_JS IIFE marker `{marker}`");
    }
    // Six top-level IIFEs (a nested navigation-type probe is indented). New
    // IIFEs are fine; dropping one is not.
    let top_level = js
        .lines()
        .filter(|line| line.starts_with("(function () {"))
        .count();
    assert!(
        top_level >= 6,
        "expected all six APP_JS IIFEs, got {top_level}"
    );
}
