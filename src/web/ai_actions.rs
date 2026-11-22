use super::*;

#[derive(Deserialize)]
pub(super) struct AiForm {
    pub(super) ai_enabled: Option<String>,
    pub(super) ai_provider: Option<String>,
    pub(super) ai_api_style: Option<String>,
    pub(super) ai_base_url: Option<String>,
    pub(super) ai_api_key: Option<String>,
    pub(super) ai_model: Option<String>,
    pub(super) ai_daily_token_budget: Option<String>,
}

fn validate_ai_save(state: &WebState, f: &AiForm, enabled: bool) -> Result<String, String> {
    // 1. provider
    let provider = if let Some(p) = non_blank(&f.ai_provider) {
        if !matches!(
            p,
            "openai" | "anthropic" | "ollama" | "openrouter" | "custom"
        ) {
            return Err(
                "Provider must be openai, anthropic, ollama, openrouter, or custom.".to_string(),
            );
        }
        p.to_string()
    } else {
        state.cfg.ai_provider.clone()
    };

    // 1b. api style
    if let Some(s) = non_blank(&f.ai_api_style)
        && !matches!(s, "openai" | "anthropic")
    {
        return Err("API style must be openai or anthropic.".to_string());
    }

    // 2. base URL presence/parse (validate whenever submitted, regardless of
    //    provider — N3), plus required-for-openai/custom.
    if let Some(b) = non_blank(&f.ai_base_url) {
        if b.len() > 512 {
            return Err("Base URL must be 512 characters or fewer.".to_string());
        }
        match reqwest::Url::parse(b) {
            Ok(u)
                if (u.scheme() == "http" || u.scheme() == "https")
                    && u.username().is_empty()
                    && u.password().is_none() => {}
            _ => {
                return Err("Base URL must be a valid http(s) URL.".to_string());
            }
        }
    } else if matches!(provider.as_str(), "openai" | "custom")
        && state.cfg.ai_base_url.trim().is_empty()
    {
        return Err("Base URL is required for this provider.".to_string());
    }

    // 3. if enabling: API key available
    if enabled {
        let key_available = non_blank(&f.ai_api_key).is_some()
            || state
                .cfg
                .ai_api_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some();
        if !key_available {
            return Err("Add an API key before enabling AI.".to_string());
        }
    }

    // 4. model
    if let Some(m) = non_blank(&f.ai_model)
        && (m.chars().count() > 128 || m.contains('\n') || m.contains('\r'))
    {
        return Err("Model must be 1-128 characters with no newline.".to_string());
    }

    // 5. budget
    if let Some(b) = non_blank(&f.ai_daily_token_budget) {
        match b.parse::<i64>() {
            Ok(v) if v >= 0 => {}
            _ => return Err("Budget must be an integer >= 0.".to_string()),
        }
    }

    // 6. api key
    if let Some(k) = non_blank(&f.ai_api_key)
        && (k.chars().count() < 8 || k.chars().count() > 4096 || k.chars().any(char::is_whitespace))
    {
        return Err("API key must be 8-4096 characters with no whitespace.".to_string());
    }

    Ok(provider)
}

#[tracing::instrument(skip_all)]
pub(super) async fn ai_save(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<AiForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "AI", "AI");
    }

    let enabled = f.ai_enabled.is_some();

    if let Err(msg) = validate_ai_save(&state, &f, enabled) {
        return ai_error(&state, Some(&session), &msg);
    }

    // Build updates
    let mut updates: Vec<(String, String)> = Vec::new();
    updates.push((
        "AI_ENABLED".to_string(),
        if enabled {
            "1".to_string()
        } else {
            "0".to_string()
        },
    ));
    if let Some(p) = non_blank(&f.ai_provider) {
        updates.push(("AI_PROVIDER".to_string(), p.to_string()));
    }
    if let Some(s) = non_blank(&f.ai_api_style) {
        updates.push(("AI_API_STYLE".to_string(), s.to_string()));
    }
    if let Some(b) = non_blank(&f.ai_base_url) {
        updates.push(("AI_BASE_URL".to_string(), b.to_string()));
    }
    if let Some(k) = non_blank(&f.ai_api_key) {
        updates.push(("AI_API_KEY".to_string(), k.to_string()));
    }
    if let Some(m) = non_blank(&f.ai_model) {
        updates.push(("AI_MODEL".to_string(), m.to_string()));
    }
    if let Some(b) = non_blank(&f.ai_daily_token_budget) {
        updates.push(("AI_DAILY_TOKEN_BUDGET".to_string(), b.to_string()));
    }

    match crate::config::write_env_file(&updates) {
        Ok(_) => ai_response(
            &state,
            Some(&session),
            r#"<div class="alert alert-success" role="status" aria-live="polite">Saved. AI settings apply on restart.</div>"#
                .to_string(),
        ),
        Err(e) => ai_error(
            &state,
            Some(&session),
            &format!("Failed to write .env: {}", esc(&e.to_string())),
        ),
    }
}

pub(super) fn ai_response(
    state: &WebState,
    session: Option<&Session>,
    banner_html: String,
) -> Response {
    Html(html_page(
        state,
        session,
        "AI",
        &ai_body(state, Some(&banner_html)),
    ))
    .into_response()
}

pub(super) fn ai_error(state: &WebState, session: Option<&Session>, msg: &str) -> Response {
    ai_response(
        state,
        session,
        format!(
            r#"<div class="alert alert-error" role="alert">{}</div>"#,
            msg
        ),
    )
}

fn ai_test_inputs(
    state: &WebState,
    f: &AiForm,
) -> Result<(String, String, String, String), String> {
    let provider_name = non_blank(&f.ai_provider)
        .map(str::to_string)
        .unwrap_or_else(|| state.cfg.ai_provider.clone());
    if !matches!(
        provider_name.as_str(),
        "openai" | "anthropic" | "ollama" | "openrouter" | "custom"
    ) {
        return Err(
            "Provider must be openai, anthropic, ollama, openrouter, or custom.".to_string(),
        );
    }
    let api_style = non_blank(&f.ai_api_style)
        .map(str::to_string)
        .unwrap_or_else(|| state.cfg.ai_api_style.clone());
    if !matches!(api_style.as_str(), "openai" | "anthropic") {
        return Err("API style must be openai or anthropic.".to_string());
    }
    let base_url = non_blank(&f.ai_base_url)
        .map(str::to_string)
        .unwrap_or_else(|| state.cfg.ai_base_url.clone());

    // B1: validate base URL for providers that use it.
    if !matches!(api_style.as_str(), "anthropic") {
        if base_url.len() > 512 {
            return Err("Base URL must be 512 characters or fewer.".to_string());
        }
        match reqwest::Url::parse(&base_url) {
            Ok(u)
                if (u.scheme() == "http" || u.scheme() == "https")
                    && u.username().is_empty()
                    && u.password().is_none() => {}
            _ => {
                return Err("Base URL must be a valid http(s) URL.".to_string());
            }
        }
    }
    let key = non_blank(&f.ai_api_key).map(str::to_string).or_else(|| {
        state
            .cfg
            .ai_api_key
            .clone()
            .filter(|s| !s.trim().is_empty())
    });
    let model = non_blank(&f.ai_model)
        .map(str::to_string)
        .unwrap_or_else(|| state.cfg.ai_model.clone());

    let Some(key) = key else {
        return Err("No API key configured.".to_string());
    };

    Ok((api_style, base_url, key, model))
}

fn build_ai_test_provider(
    api_style: &str,
    base_url: String,
    key: String,
) -> Result<std::sync::Arc<dyn crate::ai::provider::AiProvider>, String> {
    match api_style {
        "anthropic" => crate::ai::anthropic::AnthropicProvider::new(base_url, key)
            .map(|p| std::sync::Arc::new(p) as std::sync::Arc<dyn crate::ai::provider::AiProvider>)
            .map_err(|e| e.to_string()),
        _ => crate::ai::openai::OpenAiProvider::new(base_url, key)
            .map(|p| std::sync::Arc::new(p) as std::sync::Arc<dyn crate::ai::provider::AiProvider>)
            .map_err(|e| e.to_string()),
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn ai_test(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<AiForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"ok": false, "latency_ms": 0, "error": "Unauthorized"})),
            )
                .into_response();
        }
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "latency_ms": 0, "error": "Forbidden"})),
        )
            .into_response();
    }

    // N1: serialize concurrent /ai/test calls.
    let _inflight_guard = state.ai_test_inflight.lock().await;

    // N2: validate inputs.
    let (api_style, base_url, key, model) = match ai_test_inputs(&state, &f) {
        Ok(v) => v,
        Err(msg) => {
            return (
                StatusCode::OK,
                Json(json!({"ok": false, "latency_ms": 0, "error": msg})),
            )
                .into_response();
        }
    };

    use crate::ai::provider::{AiProvider, GenerateRequest, Message};
    use std::sync::Arc;

    let provider: Arc<dyn AiProvider> =
        match build_ai_test_provider(api_style.as_str(), base_url.clone(), key.clone()) {
            Ok(p) => p,
            Err(e) => {
                return (
                    StatusCode::OK,
                    Json(json!({"ok": false, "latency_ms": 0, "error": e})),
                )
                    .into_response();
            }
        };

    let req = GenerateRequest {
        model,
        messages: vec![Message {
            role: "user".to_string(),
            content: "ping".to_string(),
        }],
        max_tokens: 5,
        temperature: 0.0,
    };

    let start = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(10), provider.generate(req)).await;
    let latency_ms = start.elapsed().as_millis() as u64;

    match outcome {
        Ok(Ok(_)) => (
            StatusCode::OK,
            Json(json!({"ok": true, "latency_ms": latency_ms, "error": null})),
        )
            .into_response(),
        Ok(Err(e)) => {
            let msg = e.to_string();
            let err = redact_ai_error(&msg, &key, &base_url);
            let category = categorize_ai_error(&err);
            (
                StatusCode::OK,
                Json(json!({"ok": false, "latency_ms": latency_ms, "error": category})),
            )
                .into_response()
        }
        Err(_) => (
            StatusCode::OK,
            Json(json!({"ok": false, "latency_ms": latency_ms, "error": "Timed out after 10s."})),
        )
            .into_response(),
    }
}
