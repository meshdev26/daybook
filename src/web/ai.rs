use super::*;

#[tracing::instrument(skip_all)]
pub(super) async fn ai_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "AI", "AI");
    }
    Html(html_page(
        &state,
        Some(&session),
        "AI",
        &ai_body(&state, None),
    ))
    .into_response()
}

fn ai_unconfigured_alert(state: &WebState) -> String {
    // Unconfigured alert
    if !state.cfg.ai_enabled {
        r#"<div class="alert alert-info" role="status" aria-live="polite">AI is currently disabled. Configure a provider and key, then toggle Enable AI features.</div>"#.to_string()
    } else {
        String::new()
    }
}

struct AiFormFields {
    key_badge: String,
    key_placeholder: &'static str,
    key_help: &'static str,
    base_url_value: String,
    model_value: String,
    budget_value: String,
    enabled_checked: &'static str,
    selected_openai: &'static str,
    selected_anthropic: &'static str,
    selected_custom: &'static str,
    selected_ollama: &'static str,
    selected_openrouter: &'static str,
    selected_style_openai: &'static str,
    selected_style_anthropic: &'static str,
    base_disabled: &'static str,
}

fn selected_if(value: &str, target: &str) -> &'static str {
    if value == target { "selected" } else { "" }
}

fn disabled_if(value: &str, target: &str) -> &'static str {
    if value == target { "disabled" } else { "" }
}

fn ai_form_fields(state: &WebState) -> AiFormFields {
    // API key badge. No aria-label on purpose: the badge sits inside the
    // field's <label>, so its text already joins the input's accessible name
    // ("API key not set") — an aria-label here would read it twice.
    let key_badge = if state
        .cfg
        .ai_api_key
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        r#"<span class="badge badge-warning">not set</span>"#.to_string()
    } else {
        r#"<span class="badge badge-success">configured</span>"#.to_string()
    };
    let key_placeholder = if state
        .cfg
        .ai_api_key
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        "sk-… / your provider key"
    } else {
        "leave blank to keep the current key"
    };
    let key_help = if state
        .cfg
        .ai_api_key
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        "Write-only. Stored in .env with 0600 permissions and never rendered back."
    } else {
        "leave blank to keep the current key. Stored in .env with 0600 permissions and never rendered back."
    };

    let provider_value = &state.cfg.ai_provider;
    // Defensively strip any userinfo from a pre-existing .env value so it
    // cannot leak into the rendered input (B1).
    let base_url_value = esc(&strip_url_userinfo(&state.cfg.ai_base_url));
    let model_value = esc(&state.cfg.ai_model);
    let budget_value = state
        .cfg
        .ai_daily_token_budget
        .map(|v| v.to_string())
        .unwrap_or_default();
    let budget_value = esc(&budget_value);
    let enabled_checked = if state.cfg.ai_enabled { "checked" } else { "" };

    let selected_openai = selected_if(provider_value, "openai");
    let selected_anthropic = selected_if(provider_value, "anthropic");
    let selected_custom = selected_if(provider_value, "custom");
    let selected_ollama = selected_if(provider_value, "ollama");
    let selected_openrouter = selected_if(provider_value, "openrouter");

    let api_style_value = &state.cfg.ai_api_style;
    let selected_style_openai = selected_if(api_style_value, "openai");
    let selected_style_anthropic = selected_if(api_style_value, "anthropic");

    // base url disabled for anthropic initial
    let base_disabled = disabled_if(provider_value, "anthropic");

    AiFormFields {
        key_badge,
        key_placeholder,
        key_help,
        base_url_value,
        model_value,
        budget_value,
        enabled_checked,
        selected_openai,
        selected_anthropic,
        selected_custom,
        selected_ollama,
        selected_openrouter,
        selected_style_openai,
        selected_style_anthropic,
        base_disabled,
    }
}

fn ai_form(state: &WebState) -> String {
    let f = ai_form_fields(state);
    let form = format!(
        r#"<article class="panel"><h3>AI provider</h3>
<form method="post" action="{}">
<label>Enable AI features <input type="checkbox" name="ai_enabled" value="1" {} aria-describedby="ai-enabled-help"><span class="help" id="ai-enabled-help">Turns on AI-assisted replies and templates. Off means no AI calls are made, even with a key saved.</span></label>
<label>Provider <select name="ai_provider" id="ai-provider" aria-describedby="ai-provider-help">
<option value="openai" {}>openai</option>
<option value="anthropic" {}>anthropic</option>
<option value="ollama" {}>ollama (local)</option>
<option value="openrouter" {}>openrouter</option>
<option value="custom" {}>custom</option>
</select><span class="help" id="ai-provider-help">OpenAI, Anthropic, Ollama (local), OpenRouter, or any OpenAI-compatible endpoint. Ollama/vLLM/LM Studio use OpenAI-compatible /v1/chat/completions; Anthropic uses /v1/messages.</span></label>
<label>API style <select name="ai_api_style" id="ai-api-style" aria-describedby="ai-api-style-help">
<option value="openai" {}>openai (chat/completions)</option>
<option value="anthropic" {}>anthropic (messages)</option>
</select><span class="help" id="ai-api-style-help">Use openai for /v1/chat/completions, anthropic for /v1/messages.</span></label>
<label>Base URL <input type="text" name="ai_base_url" inputmode="url" value="{}" {} aria-describedby="ai-baseurl-help" maxlength="512"><span class="help" id="ai-baseurl-help">OpenAI-compatible API base URL ending in /v1 (e.g. https://api.openai.com/v1, http://localhost:11434/v1, https://openrouter.ai/api/v1). Anthropic ignores this field.</span></label>
<label for="ai-api-key">API key {}</label>
<span class="secret-field"><input type="password" id="ai-api-key" name="ai_api_key" autocomplete="new-password" placeholder="{}" aria-describedby="ai-apikey-help">{toggle}</span><span class="help" id="ai-apikey-help">{}</span>
<label>Model <input type="text" name="ai_model" value="{}" aria-describedby="ai-model-help" maxlength="128"><span class="help" id="ai-model-help">Model name as the provider expects it (e.g. gpt-4o-mini, claude-3-5-sonnet-20241022).</span></label>
<label>Daily token budget (global) <input type="number" name="ai_daily_token_budget" min="0" step="1" inputmode="numeric" value="{}" aria-describedby="ai-budget-help"><span class="help" id="ai-budget-help">Maximum tokens per day across all sellers and buyers. Blank = keep current. The service treats an unset budget as unlimited. Enforced by the AI service.</span></label>
<div class="form-row">
<div><button type="submit">Save AI settings</button></div>
<div><button type="button" id="ai-test-btn" class="btn btn-secondary">Test connection</button></div>
</div>
<div id="ai-test-status" role="status" aria-live="polite" aria-atomic="true" class="help"></div>
<noscript><p class="help">JavaScript is required to test the connection.</p></noscript>
</form>
<p class="help">Values are written to .env with 0600 permissions. AI settings apply on restart; Test connection uses the values as typed.</p>
<script>
(function(){{
  var sel=document.getElementById('ai-provider');
  var base=document.querySelector('input[name="ai_base_url"]');
  var defaults={{'openai':'https://api.openai.com/v1','ollama':'http://localhost:11434/v1','openrouter':'https://openrouter.ai/api/v1'}};
  function sync(){{ if(!base) return; base.disabled = (sel && sel.value==='anthropic'); if(sel && defaults[sel.value] && !base.value){{ base.value=defaults[sel.value]; }} }}
  if(sel){{ sel.addEventListener('change',sync); sync(); }}
  var btn=document.getElementById('ai-test-btn');
  var status=document.getElementById('ai-test-status');
  function esc(s){{ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;'); }}
  if(btn){{
    btn.addEventListener('click', async function(){{
      if(btn.disabled) return;
      btn.disabled=true; btn.setAttribute('aria-busy','true'); btn.textContent='Testing…';
      if(status){{ status.setAttribute('role','status'); status.textContent='Testing connection…'; }}
      var form=btn.closest('form');
      var fd=new FormData(form);
      try{{
        var r=await fetch('{}',{{method:'POST',body:fd,credentials:'same-origin'}});
        var j=await r.json();
        if(j.ok){{ if(status){{ status.setAttribute('role','status'); status.textContent='Connected in '+j.latency_ms+' ms.'; }} }}
        else {{ if(status){{ status.setAttribute('role','alert'); status.textContent='Connection failed: '+(j.error||'Unknown error'); }} }}
      }}catch(e){{ if(status){{ status.setAttribute('role','alert'); status.textContent='Connection failed: Network error.'; }} }}
      btn.disabled=false; btn.removeAttribute('aria-busy'); btn.textContent='Test connection';
    }});
  }}
}})();
</script>
</article>"#,
        state.url("/ai/save"),
        f.enabled_checked,
        f.selected_openai,
        f.selected_anthropic,
        f.selected_ollama,
        f.selected_openrouter,
        f.selected_custom,
        f.selected_style_openai,
        f.selected_style_anthropic,
        f.base_url_value,
        f.base_disabled,
        f.key_badge,
        f.key_placeholder,
        f.key_help,
        f.model_value,
        f.budget_value,
        state.url("/ai/test"),
        toggle = SECRET_TOGGLE,
    );
    form
}

fn ai_usage_dashboard(state: &WebState) -> String {
    // Usage dashboard
    let today_tokens = state.db.ai_usage_today_tokens_global();
    let summary = state.db.ai_usage_summary_global(7);
    let week_tokens: i64 = summary.iter().map(|r| r.1).sum();
    let week_requests: i64 = summary.iter().map(|r| r.2).sum();
    let week_cost = state.db.ai_usage_cost_total_global(7);
    let errors = state.db.ai_usage_recent_errors_global(10);

    // daily table
    let daily_table = if summary.is_empty() {
        r#"<p class="empty">No AI usage yet. Usage appears here after the first AI-assisted reply.</p>"#.to_string()
    } else {
        let rows = summary
            .iter()
            .map(|(d, t, c)| {
                format!(
                    "<tr><td class=\"mono\" title=\"{}\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                    esc(d),
                    esc(d),
                    t,
                    c
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!(
            "<div class=\"table-wrap\" tabindex=\"0\" role=\"region\" aria-label=\"Daily AI usage\"><table><caption class=\"sr-only\">Daily AI usage for the last 7 days</caption><thead><tr><th>Date</th><th class=\"num\">Tokens</th><th class=\"num\">Requests</th></tr></thead><tbody>{}</tbody></table></div>",
            rows
        )
    };

    // recent errors
    let errors_html = if errors.is_empty() {
        r#"<p class="empty">No recent errors. AI calls are succeeding.</p>"#.to_string()
    } else {
        let items = errors
            .iter()
            .map(|(ts, prov, model, err, _status)| {
                let raw = err.clone().unwrap_or_default();
                // Render-time redaction (N4): never trust stored error text.
                let key = state.cfg.ai_api_key.clone().unwrap_or_default();
                let base = state.cfg.ai_base_url.clone();
                let msg = redact_ai_error(&raw, &key, &base);
                let short = if msg.chars().count() > 160 {
                    let s: String = msg.chars().take(160).collect();
                    format!("{}…", s)
                } else {
                    msg.clone()
                };
                format!(
                    "<li><span class=\"mono\">{}</span> <span class=\"mono\">{}/{}</span> <span title=\"{}\">{}</span><details><summary>Show more</summary><p>{}</p></details></li>",
                    esc(ts),
                    esc(prov),
                    esc(model),
                    esc(&msg),
                    esc(&short),
                    esc(&msg)
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!("<ul>{}</ul>", items)
    };

    let dashboard = format!(
        r#"<section aria-labelledby="ai-usage-heading"><h2 id="ai-usage-heading">Usage &amp; cost</h2>
<div class="grid-stats">
<article class="panel"><div class="stat"><span class="stat-label">Today's tokens</span><span class="stat-value">{}</span></div></article>
<article class="panel"><div class="stat"><span class="stat-label">7-day tokens</span><span class="stat-value">{}</span></div></article>
<article class="panel"><div class="stat"><span class="stat-label">7-day requests</span><span class="stat-value">{}</span></div></article>
<article class="panel"><div class="stat"><span class="stat-label">7-day estimated cost</span><span class="stat-value">${:.2}</span></div><p class="help" title="Estimated from token counts and rough per-model pricing; not a bill.">estimated cost, last 7 days</p></article>
</div>
{}
<article class="panel"><h3>Recent errors</h3>{}</article>
<p class="help">Refresh the page to update usage.</p>
</section>"#,
        today_tokens, week_tokens, week_requests, week_cost, daily_table, errors_html,
    );
    dashboard
}

fn ai_templates_panel(state: &WebState) -> String {
    // Templates (read-only). Owners have no seller_id, so the REST list
    // (`ai_templates_list` in api.rs) resolves their scope to 0 — the global
    // bucket — and this panel reads the exact same rows. Editing lives in the
    // bot (/ai) and the REST API, deliberately not here.
    let templates = state.db.list_ai_templates(0);
    let templates_html = if templates.is_empty() {
        r#"<p class="empty">No templates yet.</p>"#.to_string()
    } else {
        let items = templates
            .iter()
            .map(|(name, description)| {
                format!(
                    "<li><span class=\"mono\">{}</span> <span>{}</span></li>",
                    esc(name),
                    esc(description.as_deref().unwrap_or("—"))
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!("<ul>{}</ul>", items)
    };
    let templates_panel = format!(
        r#"<section class="panel" aria-labelledby="ai-templates-heading"><h3 id="ai-templates-heading">Templates</h3>
{}
<p class="help">Manage templates via the bot (/ai) or REST /api/v1/ai/templates.</p>
</section>"#,
        templates_html
    );
    templates_panel
}

pub(super) fn ai_body(state: &WebState, banner: Option<&str>) -> String {
    let banner = banner.unwrap_or_default();
    let unconfigured = ai_unconfigured_alert(state);
    let form = ai_form(state);
    let dashboard = ai_usage_dashboard(state);
    let templates_panel = ai_templates_panel(state);
    format!(
        r#"{banner}{unconfigured}<h1>AI <span class="sub">provider credentials, usage and templates</span></h1>
{form}
{dashboard}
{templates_panel}
<p class="help">Values are written to .env with 0600 permissions. AI settings apply on restart; Test connection uses the values as typed.</p>"#,
        banner = banner,
        unconfigured = unconfigured,
        form = form,
        dashboard = dashboard,
        templates_panel = templates_panel,
    )
}

/// Strip userinfo (and nothing else) from a URL-ish string for safe rendering.
pub(super) fn strip_url_userinfo(s: &str) -> String {
    match reqwest::Url::parse(s) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => s.to_string(),
    }
}

pub(super) fn redact_ai_error(msg: &str, key: &str, base_url: &str) -> String {
    // 1. exact key replacement (covers key-in-body and key-in-URL when the
    //    full key string appears).
    let mut s = if key.trim().is_empty() {
        msg.to_string()
    } else {
        msg.replace(key, "[redacted]")
    };

    // 2. Parse any URL embedded in the (already key-redacted) text and strip
    //    userinfo + query + fragment, so secrets in query strings or userinfo
    //    cannot survive. Full request URLs (scheme://host/path) are retained
    //    in redacted form rather than only the configured base_url.
    s = redact_urls_in_text(&s);

    // 3. Also scrub the configured base_url if it still appears verbatim
    //    (e.g. without a scheme) — best effort.
    if !base_url.trim().is_empty() {
        let scrubbed = strip_url_userinfo(base_url);
        s = s.replace(base_url, &scrubbed);
    }

    s
}

/// Walk `input` and redact userinfo/query/fragment from any embedded URL.
pub(super) fn redact_urls_in_text(input: &str) -> String {
    pub(super) const SCHEMES: [&str; 2] = ["https://", "http://"];
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        // Find the next scheme occurrence at or after i.
        let next = SCHEMES
            .iter()
            .filter_map(|sch| input[i..].find(sch).map(|off| (off, *sch)))
            .min_by_key(|(off, _)| *off);
        match next {
            Some((off, _)) => {
                let abs = i + off;
                // Copy the non-URL prefix verbatim.
                out.push_str(&input[i..abs]);
                // Determine URL end: stop at whitespace or a delimiter char.
                let mut j = abs;
                while j < input.len() {
                    let c = input.as_bytes()[j];
                    if c.is_ascii_whitespace()
                        || matches!(
                            c,
                            b'"' | b'\'' | b'<' | b'>' | b',' | b';' | b')' | b']' | b'}'
                        )
                    {
                        break;
                    }
                    j += 1;
                }
                let candidate = &input[abs..j];
                if let Ok(mut u) = reqwest::Url::parse(candidate) {
                    let _ = u.set_username("");
                    let _ = u.set_password(None);
                    u.set_query(None);
                    u.set_fragment(None);
                    out.push_str(u.as_str());
                } else {
                    out.push_str(candidate);
                }
                i = j;
            }
            None => {
                out.push_str(&input[i..]);
                break;
            }
        }
    }
    out
}

pub(super) fn categorize_ai_error(msg: &str) -> String {
    let lower = msg.to_lowercase();
    if lower.contains("401") || lower.contains("unauthorized") || lower.contains("authentication") {
        return "Authentication failed (check the API key).".to_string();
    }
    if lower.contains("not found") || lower.contains("does not exist") {
        // only model-ish; default to model not found if mentions model
        if lower.contains("model") || lower.contains("404") {
            return "Model not found.".to_string();
        }
    }
    if lower.contains("timed out") {
        return "Timed out after 10s.".to_string();
    }
    if lower.contains("error sending request")
        || lower.contains("connect")
        || lower.contains("network")
        || lower.contains("dns")
    {
        return "Network error.".to_string();
    }
    // fallback: redacted provider message (truncate)
    let trimmed = msg.trim();
    if trimmed.is_empty() {
        "Provider error.".to_string()
    } else {
        trimmed.chars().take(200).collect()
    }
}
