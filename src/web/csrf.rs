//! Per-form CSRF enforcement for the operator console.
//!
//! Two halves, both centralized so no form or handler can be missed:
//!
//! * [`csrf_middleware`] validates a submitted `csrf_token` against the
//!   session's token (derived in `crate::auth::try_session`) on **every** POST
//!   that reaches the web router, before the handler is dispatched. Requests
//!   without a session (the login/claim/setup/MFA doors) validate against the
//!   anonymous token; bearer-authenticated machine clients are exempt.
//! * [`inject_csrf_fields`] adds the hidden field to every POST form in the
//!   final rendered HTML of `html_page` / `html_page_bare_panel`.
//!
//! This router does not host `/api/v1/**`, MCP or the webhook — those are
//! merged in `main.rs` after `web::router()` returns, so the layer never sees
//! them. `SameSite=Strict` on the session cookie stays in place as defense in
//! depth.

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Method, StatusCode, header};

/// Largest form body we will buffer to look for the token. Console forms are
/// tiny; anything larger is refused rather than buffered without bound.
const MAX_FORM_BYTES: usize = 1 << 20;

/// True only for a `Bearer` credential. Those callers are machine clients
/// (REST sessions / MCP) and are exempt from cookie-form CSRF.
fn is_bearer(req: &Request) -> bool {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer "))
}

/// Pull `csrf_token` out of a urlencoded body. The token is hex, so it is
/// never percent-encoded; still require a field boundary (start or `&`) so a
/// value like `not_csrf_token=` cannot match.
fn extract_submitted(body: &[u8]) -> Option<&str> {
    const KEY: &[u8] = b"csrf_token=";
    let mut offset = 0;
    while offset + KEY.len() <= body.len() {
        let at_boundary = offset == 0 || body[offset - 1] == b'&';
        if at_boundary && &body[offset..offset + KEY.len()] == KEY {
            let rest = &body[offset + KEY.len()..];
            let end = rest
                .iter()
                .position(|b| matches!(b, b'&' | b'\r' | b'\n'))
                .unwrap_or(rest.len());
            return std::str::from_utf8(&rest[..end]).ok();
        }
        offset += 1;
    }
    None
}

fn forbidden(state: &WebState) -> Response {
    (
        StatusCode::FORBIDDEN,
        Html(format!(
            r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Forbidden — Daybook</title><link rel="stylesheet" href="{base}/assets/app.css"></head><body><main class="door" id="main"><div class="panel"><h1>Forbidden</h1><p>The form token was missing or invalid. No change was made.</p><p><a class="btn btn-secondary" href="{base}/login">Back to sign in</a></p></div></main></body></html>"#,
            base = esc(&state.base_path),
        )),
    )
        .into_response()
}

/// Router-level CSRF gate. Runs before any state-changing handler, so a
/// rejected request has no side effects.
#[tracing::instrument(skip_all)]
pub(super) async fn csrf_middleware(
    State(state): State<WebState>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() != Method::POST || is_bearer(&req) {
        return next.run(req).await;
    }
    let expected = session_from_headers(&state, req.headers())
        .map(|s| s.csrf_token)
        .unwrap_or_else(anonymous_csrf_token);
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, MAX_FORM_BYTES).await {
        Ok(b) => b,
        Err(_) => return forbidden(&state),
    };
    let ok = extract_submitted(&bytes).is_some_and(|t| ct_eq(t, &expected));
    if !ok {
        return forbidden(&state);
    }
    // Hand the untouched body back to axum's Form extractors.
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

/// Open the tag after `<form` up to its closing `>`, skipping quoted attribute
/// values so a `>` inside e.g. an `onsubmit` string cannot end the tag early.
fn form_tag_end(html: &str, start: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut i = start + 5;
    let mut quote = 0u8;
    while i < bytes.len() {
        let b = bytes[i];
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'\'' || b == b'"' {
            quote = b;
        } else if b == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_post_form(tag: &str) -> bool {
    let t = tag.to_ascii_lowercase();
    t.contains(r#"method="post""#)
        || t.contains("method='post'")
        || t.contains("method=post ")
        || t.trim_end().ends_with("method=post")
}

/// Insert `<input type="hidden" name="csrf_token" value="…">` immediately
/// after the opening tag of every POST form in a rendered page. Idempotent:
/// a form that already carries the field is left alone. GET forms are never
/// touched, and an empty token skips the pass (used by render-only tests that
/// build a `Session` without a token).
pub(super) fn inject_csrf_fields(html: &str, token: &str) -> String {
    if token.is_empty() {
        return html.to_string();
    }
    let field = format!(
        r#"<input type="hidden" name="csrf_token" value="{}">"#,
        esc(token)
    );
    let mut out = String::with_capacity(html.len() + 128);
    let mut pos = 0;
    while let Some(rel) = html[pos..].find("<form") {
        let start = pos + rel;
        let boundary_ok = html
            .as_bytes()
            .get(start + 5)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>'));
        let Some(end) = form_tag_end(html, start) else {
            break;
        };
        let tag = &html[start..=end];
        if boundary_ok && is_post_form(tag) && !tag.contains("csrf_token") {
            out.push_str(&html[pos..=end]);
            out.push_str(&field);
            pos = end + 1;
        } else {
            out.push_str(&html[pos..=end]);
            pos = end + 1;
        }
    }
    out.push_str(&html[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::inject_csrf_fields;

    #[test]
    fn injects_only_into_post_forms() {
        let html = r#"<form method="get" action="/a"><input name="q"></form><form method="post" action="/b" onsubmit="x('a>b')"><button>Go</button></form>"#;
        let out = inject_csrf_fields(html, "TOKEN");
        assert_eq!(
            out.matches(r#"name="csrf_token" value="TOKEN""#).count(),
            1,
            "{out}"
        );
        assert!(out.contains(r#"<form method="get" action="/a"><input name="q"></form>"#));
        assert!(out.contains(
            r#"onsubmit="x('a>b')"><input type="hidden" name="csrf_token" value="TOKEN">"#
        ));
    }

    #[test]
    fn empty_token_is_a_no_op() {
        let html = r#"<form method="post" action="/b"></form>"#;
        assert_eq!(inject_csrf_fields(html, ""), html);
    }
}
