// Timeout middleware tests (L8). The production ceiling is 30s; the
// tests exercise the real `run_with_timeout` logic with a short limit so
// nothing sleeps 30 seconds. No sleep route exists in production code —
// these live only inside this test module.
use super::*;
use axum::body::Body;
use axum::routing::get;
use tower::ServiceExt;

#[tracing::instrument(skip_all)]
async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(500)).await;
    "done"
}
#[tracing::instrument(skip_all)]
async fn fast() -> &'static str {
    "ok"
}

fn app(limit: Duration) -> Router {
    Router::new()
        .route("/slow", get(slow))
        .route("/fast", get(fast))
        .layer(axum::middleware::from_fn(move |req, next| async move {
            run_with_timeout(limit, req, next).await
        }))
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn timeout_returns_503_envelope() {
    let resp = app(Duration::from_millis(50))
        .oneshot(
            axum::http::Request::builder()
                .uri("/slow")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "timeout");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn fast_handlers_pass_through_untimed() {
    let resp = app(Duration::from_millis(2000))
        .oneshot(
            axum::http::Request::builder()
                .uri("/fast")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
