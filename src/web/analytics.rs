use super::*;

#[derive(Deserialize)]
pub(super) struct AnalyticsQuery {
    pub(super) period: Option<String>,
}

pub(super) struct AnalyticsMetrics {
    /// One HTML line per denomination; no cross-asset sum is ever rendered.
    revenue_lines: String,
    invoice_total: i64,
    invoice_paid: i64,
    repeat_total: i64,
    repeat_buyers: i64,
    abandoned: i64,
    recovered: i64,
    customer_count: i64,
    aov_text: String,
    abandoned_rate: f64,
    top_rows: String,
    top_labels_json: String,
    top_values_json: String,
}

fn analytics_seller_ids(state: &WebState, session: &Session) -> Vec<i64> {
    if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id.into_iter().collect()
    } else {
        // Bot scoping (web-bot-scoping N1): a global owner with a selected
        // bot aggregates exactly that bot's sellers. An empty scope keeps the
        // existing empty fallback (aggregate seller 0 → zeros) rather than
        // widening back to every seller.
        let mut ids = match bot_scope_sellers(state, session) {
            Some(set) => set,
            None => state.db.list_sellers(),
        };
        if ids.is_empty() {
            ids.push(0);
        }
        ids
    }
}

pub(super) struct AnalyticsAgg {
    revenue_by_asset: std::collections::BTreeMap<String, f64>,
    invoice_total: i64,
    invoice_paid: i64,
    repeat_total: i64,
    repeat_buyers: i64,
    abandoned: i64,
    recovered: i64,
    customer_count: i64,
    aov_single: Option<f64>,
    top_products: Vec<(i64, i64)>,
}

fn analytics_aggregate(state: &WebState, seller_ids: &[i64]) -> AnalyticsAgg {
    let mut revenue_by_asset: std::collections::BTreeMap<String, f64> =
        std::collections::BTreeMap::new();
    let mut invoice_total: i64 = 0;
    let mut invoice_paid: i64 = 0;
    let mut repeat_total: i64 = 0;
    let mut repeat_buyers: i64 = 0;
    let mut abandoned: i64 = 0;
    let mut recovered: i64 = 0;
    let mut customer_count: i64 = 0;
    let mut top_products: Vec<(i64, i64)> = Vec::new();
    let mut aov_single: Option<f64> = None;

    if seller_ids.len() == 1 {
        let sid = seller_ids[0];
        for (asset, amount) in state.db.analytics_revenue_by_asset(sid) {
            revenue_by_asset.insert(asset, amount);
        }
        let (t, p) = state.db.analytics_invoice_counts(sid);
        invoice_total = t;
        invoice_paid = p;
        aov_single = state.db.analytics_aov(sid).ok();
        top_products.extend(state.db.analytics_top_products(sid, 10));
        let (tot, rep) = state.db.analytics_repeat_buyers(sid);
        repeat_total = tot;
        repeat_buyers = rep;
        let (ab, rec) = state.db.analytics_abandoned_cart_rate(sid);
        abandoned = ab;
        recovered = rec;
        customer_count = state.db.analytics_customer_count(sid);
    } else {
        for sid in seller_ids {
            for (asset, amount) in state.db.analytics_revenue_by_asset(*sid) {
                *revenue_by_asset.entry(asset).or_insert(0.0) += amount;
            }
            let (t, p) = state.db.analytics_invoice_counts(*sid);
            invoice_total += t;
            invoice_paid += p;
            top_products.extend(state.db.analytics_top_products(*sid, 10));
            let (tot, rep) = state.db.analytics_repeat_buyers(*sid);
            repeat_total += tot;
            repeat_buyers += rep;
            let (ab, rec) = state.db.analytics_abandoned_cart_rate(*sid);
            abandoned += ab;
            recovered += rec;
            customer_count += state.db.analytics_customer_count(*sid);
        }
    }

    AnalyticsAgg {
        revenue_by_asset,
        invoice_total,
        invoice_paid,
        repeat_total,
        repeat_buyers,
        abandoned,
        recovered,
        customer_count,
        aov_single,
        top_products,
    }
}

pub(super) fn analytics_metrics(state: &WebState, session: &Session) -> AnalyticsMetrics {
    let seller_ids = analytics_seller_ids(state, session);
    let agg = analytics_aggregate(state, &seller_ids);

    // Money is rendered per asset. A single float is shown only when the
    // whole scope uses one denomination; otherwise both total and AOV carry
    // a clearly labeled marker, never a meaningless cross-asset number.
    let mixed = agg.revenue_by_asset.len() > 1;
    let revenue_sum: f64 = agg.revenue_by_asset.values().sum();
    let mut revenue_lines = String::new();
    for (asset, amount) in &agg.revenue_by_asset {
        revenue_lines.push_str(&format!(
            r#"<div class="stat"><span class="stat-label">Revenue ({asset})</span><span class="stat-value">{amount:.2}</span></div>"#
        ));
    }
    if revenue_lines.is_empty() {
        revenue_lines = r#"<div class="stat"><span class="stat-label">Revenue</span><span class="stat-value">0.00</span></div>"#.to_string();
    }
    let aov_text = if mixed {
        "mixed assets".to_string()
    } else {
        let value = agg.aov_single.unwrap_or_else(|| {
            if agg.invoice_paid > 0 {
                revenue_sum / agg.invoice_paid as f64
            } else {
                0.0
            }
        });
        format!("{value:.2}")
    };

    let mut top_map: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
    for (pid, cnt) in &agg.top_products {
        *top_map.entry(*pid).or_insert(0) += *cnt;
    }
    let mut top_sorted: Vec<(i64, i64)> = top_map.into_iter().collect();
    top_sorted.sort_by_key(|x| std::cmp::Reverse(x.1));
    top_sorted.truncate(10);

    let abandoned_rate = if agg.abandoned + agg.recovered > 0 {
        (agg.abandoned as f64 / (agg.abandoned + agg.recovered) as f64) * 100.0
    } else {
        0.0
    };

    let top_labels: Vec<String> = top_sorted
        .iter()
        .map(|(pid, _)| format!("Product {}", pid))
        .collect();
    let top_values: Vec<i64> = top_sorted.iter().map(|(_, c)| *c).collect();
    let top_labels_json = serde_json::to_string(&top_labels).unwrap_or_else(|_| "[]".to_string());
    let top_values_json = serde_json::to_string(&top_values).unwrap_or_else(|_| "[]".to_string());

    let mut top_rows = String::new();
    for (pid, cnt) in &top_sorted {
        top_rows.push_str(&format!(
            "<tr><td>Product {}</td><td class=\"num\">{}</td></tr>",
            pid, cnt
        ));
    }
    if top_rows.is_empty() {
        top_rows =
            r#"<tr><td colspan="2" class="empty">No product sales yet.</td></tr>"#.to_string();
    }

    AnalyticsMetrics {
        revenue_lines,
        invoice_total: agg.invoice_total,
        invoice_paid: agg.invoice_paid,
        repeat_total: agg.repeat_total,
        repeat_buyers: agg.repeat_buyers,
        abandoned: agg.abandoned,
        recovered: agg.recovered,
        customer_count: agg.customer_count,
        aov_text,
        abandoned_rate,
        top_rows,
        top_labels_json,
        top_values_json,
    }
}

fn analytics_period_options(period: &str) -> String {
    ["7d", "30d", "90d", "all"]
        .iter()
        .map(|p| {
            let sel = if *p == period { " selected" } else { "" };
            format!(r#"<option value="{}"{}>{}</option>"#, p, sel, p)
        })
        .collect::<String>()
}

/// The Overview metric cards (B11.2): the `.stat` shape in `.grid-stats`, the
/// same idiom the subs/bookings count cards use.
fn analytics_overview(m: &AnalyticsMetrics) -> String {
    format!(
        r#"<section aria-labelledby="overview-heading"><h2 id="overview-heading">Overview</h2>
<div class="grid-stats">
{revenue_lines}
<div class="panel"><div class="stat"><span class="stat-label">Invoice counts</span><span class="stat-value">{invoice_paid}/{invoice_total}</span></div></div>
<div class="panel"><div class="stat"><span class="stat-label">AOV</span><span class="stat-value">{aov_text}</span></div></div>
<div class="panel"><div class="stat"><span class="stat-label">Customer count</span><span class="stat-value">{customer_count}</span></div></div>
<div class="panel"><div class="stat"><span class="stat-label">Repeat buyers</span><span class="stat-value">{repeat_buyers}/{repeat_total}</span></div></div>
<div class="panel"><div class="stat"><span class="stat-label">Abandoned cart rate</span><span class="stat-value">{abandoned_rate:.1}% ({abandoned}/{recovered})</span></div></div>
</div></section>"#,
        revenue_lines = m.revenue_lines,
        invoice_paid = m.invoice_paid,
        invoice_total = m.invoice_total,
        aov_text = m.aov_text,
        customer_count = m.customer_count,
        repeat_buyers = m.repeat_buyers,
        repeat_total = m.repeat_total,
        abandoned_rate = m.abandoned_rate,
        abandoned = m.abandoned,
        recovered = m.recovered,
    )
}

/// Top-products table + the Chart.js canvas. Chart colors are read from the
/// token set at render time (B11.4a) so the bars follow the theme instead of
/// Chart.js's default blue; the canvas carries role="img" + an aria-label
/// naming the table that holds the same data (C2.7).
fn analytics_chart_section(m: &AnalyticsMetrics, export_url: &str) -> String {
    format!(
        r#"<section aria-labelledby="top-products-heading"><h2 id="top-products-heading">Top products</h2>
<article class="panel"><div class="table-wrap" tabindex="0" role="region" aria-label="Top products"><table aria-label="Top products"><thead><tr><th scope="col">Product</th><th scope="col" class="num">Sales</th></tr></thead><tbody>{top_rows}</tbody></table></div>
<canvas id="topProductsChart" width="400" height="200" role="img" aria-label="Bar chart of sales by product; the table above lists the same data"></canvas>
<p class="help"><a href="{export_url}">Export CSV</a></p></article></section>
<script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
<script>
(function() {{
  var ctx = document.getElementById('topProductsChart');
  if (ctx && window.Chart) {{
    var cs = getComputedStyle(document.documentElement);
    var ink2 = cs.getPropertyValue('--ink-2').trim();
    var line = cs.getPropertyValue('--line').trim();
    var brand = cs.getPropertyValue('--brand').trim();
    new Chart(ctx, {{ type: 'bar',
      data: {{ labels: {top_labels_json}, datasets: [{{ label: 'Sales', data: {top_values_json},
               backgroundColor: brand, borderRadius: 3 }}] }},
      options: {{ plugins: {{ legend: {{ labels: {{ color: ink2 }} }} }},
        scales: {{ x: {{ ticks: {{ color: ink2 }}, grid: {{ color: line }} }},
                  y: {{ ticks: {{ color: ink2, precision: 0 }}, grid: {{ color: line }} }} }} }} }});
  }}
}})();
</script>"#,
        top_rows = m.top_rows,
        top_labels_json = m.top_labels_json,
        top_values_json = m.top_values_json,
        export_url = export_url,
    )
}

fn analytics_body(state: &WebState, period: &str, metrics: &AnalyticsMetrics) -> String {
    format!(
        r#"<h1>Analytics <span class="sub">business overview</span></h1>
<form method="get" action="{analytics_url}" class="inline-form">
<label>Period <select name="period">{period_options}</select></label>
<button type="submit">Apply</button>
</form>
{overview}
{chart_section}"#,
        analytics_url = state.url("/analytics"),
        period_options = analytics_period_options(period),
        overview = analytics_overview(metrics),
        chart_section = analytics_chart_section(metrics, &state.url("/analytics/export")),
    )
}

#[tracing::instrument(skip_all)]
pub(super) async fn analytics_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<AnalyticsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.tenant_scope().is_none() {
        return forbidden_page(&state, "Analytics", "Analytics").into_response();
    }

    let period = q.period.clone().unwrap_or_else(|| "30d".to_string());
    let period = match period.as_str() {
        "7d" | "30d" | "90d" | "all" => period,
        _ => "30d".to_string(),
    };

    let metrics = analytics_metrics(&state, &session);

    Html(html_page(
        &state,
        Some(&session),
        "Analytics",
        &analytics_body(&state, &period, &metrics),
    ))
    .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn analytics_export(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.tenant_scope().is_none() {
        return forbidden_page(&state, "Analytics", "Analytics").into_response();
    }
    let seller_ids: Vec<i64> = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id.into_iter().collect()
    } else {
        let mut ids = state.db.list_sellers();
        if ids.is_empty() {
            ids.push(0);
        }
        ids
    };

    let mut revenue_by_asset: std::collections::BTreeMap<String, f64> =
        std::collections::BTreeMap::new();
    let mut invoice_total: i64 = 0;
    let mut invoice_paid: i64 = 0;
    let mut repeat_total: i64 = 0;
    let mut repeat_buyers: i64 = 0;
    let mut abandoned: i64 = 0;
    let mut recovered: i64 = 0;
    let mut customer_count: i64 = 0;
    for sid in &seller_ids {
        for (asset, amount) in state.db.analytics_revenue_by_asset(*sid) {
            *revenue_by_asset.entry(asset).or_insert(0.0) += amount;
        }
        let (t, p) = state.db.analytics_invoice_counts(*sid);
        invoice_total += t;
        invoice_paid += p;
        let (tot, rep) = state.db.analytics_repeat_buyers(*sid);
        repeat_total += tot;
        repeat_buyers += rep;
        let (ab, rec) = state.db.analytics_abandoned_cart_rate(*sid);
        abandoned += ab;
        recovered += rec;
        customer_count += state.db.analytics_customer_count(*sid);
    }
    let mixed = revenue_by_asset.len() > 1;
    let revenue_sum: f64 = revenue_by_asset.values().sum();
    let aov_text = if mixed {
        "mixed assets".to_string()
    } else if invoice_paid > 0 {
        format!("{:.2}", revenue_sum / invoice_paid as f64)
    } else {
        "0.00".to_string()
    };
    let abandoned_rate = if abandoned + recovered > 0 {
        (abandoned as f64 / (abandoned + recovered) as f64) * 100.0
    } else {
        0.0
    };

    let mut csv = String::from("metric,value\n");
    for (asset, amount) in &revenue_by_asset {
        csv.push_str(&format!("revenue_{asset},{amount:.2}\n"));
    }
    csv.push_str(&format!(
        "revenue_total,{}\n",
        if mixed {
            "mixed assets".to_string()
        } else {
            format!("{revenue_sum:.2}")
        }
    ));
    csv.push_str(&format!("invoice_total,{}\n", invoice_total));
    csv.push_str(&format!("invoice_paid,{}\n", invoice_paid));
    csv.push_str(&format!("aov,{aov_text}\n"));
    csv.push_str(&format!("customer_count,{}\n", customer_count));
    csv.push_str(&format!("repeat_buyers,{}\n", repeat_buyers));
    csv.push_str(&format!("repeat_buyer_total,{}\n", repeat_total));
    csv.push_str(&format!("abandoned_carts,{}\n", abandoned));
    csv.push_str(&format!("recovered_carts,{}\n", recovered));
    csv.push_str(&format!("abandoned_cart_rate,{:.2}\n", abandoned_rate));

    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"analytics.csv\"",
            ),
        ],
        csv,
    )
        .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn usage_export(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<UsageExportQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.tenant_scope().is_none() {
        return forbidden_page(&state, "Usage", "Usage").into_response();
    }
    let is_owner = Role::from_role_str(&session.role).has_level(Role::Owner);
    let seller_scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let reveal = q.reveal.as_deref() == Some("1")
        || q.include_user_ids.as_deref() == Some("true")
        || q.include_user_ids.as_deref() == Some("1");
    let allow_reveal = is_owner && reveal;

    let rows = state.db.list_ai_usage(seller_scope, 100000);

    let mut csv = String::from(
        "id,seller_id,user_id,provider,model,template,prompt_tokens,completion_tokens,total_tokens,estimated_cost,status,created_at\n",
    );
    for r in rows {
        let user_val = match r.2 {
            Some(uid) if allow_reveal => uid.to_string(),
            Some(uid) => mask_user_id(uid),
            None => String::new(),
        };
        let template = r.5.clone().unwrap_or_default();
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{:.6},{},{},{}\n",
            r.0,
            r.1,
            user_val,
            esc_csv(&r.3),
            esc_csv(&r.4),
            esc_csv(&template),
            r.6,
            r.7,
            r.8,
            r.9,
            esc_csv(&r.10),
            esc_csv(r.11.as_deref().unwrap_or("")),
            esc_csv(&r.12),
        ));
    }
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"ai_usage.csv\"",
            ),
        ],
        csv,
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct UsageExportQuery {
    #[serde(default)]
    pub(super) reveal: Option<String>,
    #[serde(default)]
    pub(super) include_user_ids: Option<String>,
}

pub(super) fn mask_user_id(id: i64) -> String {
    format!("***{:03}", id.rem_euclid(1000))
}

pub(super) fn esc_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
