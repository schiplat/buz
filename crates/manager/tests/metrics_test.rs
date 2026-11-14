//! `GET /metrics` — the Prometheus scrape target. Serves aggregate counts
//! only (no team/pipeline/runner/job identifiers), in exposition format,
//! unauthenticated like `/health`. The route is mounted bare at root by
//! `main`, so these tests build the same two-line mini-router.

use axum::{routing::get, Router};
use manager::api;
use manager::services::log_broadcast::LogBroadcaster;
use manager::state::AppState;
use tower::ServiceExt;

mod common;

async fn metrics_app() -> Router {
    let pool = common::migrated_pool(4).await;
    let state = AppState {
        pool: manager::store::exec::DbPool::from_sqlite(&pool),
        config: common::test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    Router::new()
        .route("/metrics", get(api::metrics::metrics))
        .with_state(state)
}

/// GET /metrics returns Prometheus exposition format: HELP/TYPE headers,
/// zero-filled job series, build info, and uptime.
#[tokio::test]
async fn metrics_scrape_shape_and_zero_fill() {
    let app = metrics_app().await;
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/metrics")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "prometheus exposition must be text/plain, got {content_type}"
    );

    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();

    for line in [
        "# TYPE buz_jobs gauge",
        // Zero-filled so dashboards keep their series when a status is empty.
        "buz_jobs{status=\"pending\"} 0",
        "buz_jobs{status=\"queued\"} 0",
        "buz_jobs{status=\"running\"} 0",
        "buz_jobs{status=\"waiting_approval\"} 0",
        "buz_jobs{status=\"success\"} 0",
        "# TYPE buz_runners gauge",
        "buz_runners{state=\"total\"} 0",
        "# TYPE buz_pipelines gauge",
        "buz_pipelines{state=\"active\"} 0",
        "# TYPE buz_build_info gauge",
        "buz_process_uptime_seconds 0",
    ] {
        assert!(
            body.contains(line),
            "metrics body missing {line:?}\n--- body ---\n{body}"
        );
    }
}

/// Counts track real rows, and no identifier (team/pipeline/runner names)
/// ever leaks into the payload.
#[tokio::test]
async fn metrics_counts_rows_and_leaks_no_identifiers() {
    let pool = common::migrated_pool(4).await;
    sqlx::query("INSERT INTO pipelines (id, name) VALUES ('p-metrics', 'secret-name')")
        .execute(&pool)
        .await
        .unwrap();
    for (id, status) in [("j1", "pending"), ("j2", "pending"), ("j3", "success")] {
        sqlx::query(
            "INSERT INTO jobs (id, pipeline_id, status, trigger_by, snapshot)
             VALUES (?, 'p-metrics', ?, 'someone', '{}')",
        )
        .bind(id)
        .bind(status)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO runners (id, name, register_token, labels, status, max_concurrent_jobs, is_frozen)
         VALUES ('r-frozen', 'quiet-runner', 'tok', '[]', 'online', 1, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = AppState {
        pool: manager::store::exec::DbPool::from_sqlite(&pool),
        config: common::test_config(),
        log_broadcast: LogBroadcaster::new(),
    };
    let app = Router::new()
        .route("/metrics", get(api::metrics::metrics))
        .with_state(state);

    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/metrics")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();

    assert!(
        body.contains("buz_jobs{status=\"pending\"} 2"),
        "pending count\n{body}"
    );
    assert!(
        body.contains("buz_jobs{status=\"success\"} 1"),
        "success count\n{body}"
    );
    assert!(
        body.contains("buz_runners{state=\"total\"} 1"),
        "runner total\n{body}"
    );
    assert!(
        body.contains("buz_runners{state=\"frozen\"} 1"),
        "frozen runner\n{body}"
    );
    assert!(
        body.contains("buz_pipelines{state=\"total\"} 1"),
        "pipeline total\n{body}"
    );
    assert!(
        body.contains("buz_pipelines{state=\"active\"} 1"),
        "active pipeline\n{body}"
    );

    for identifier in ["secret-name", "quiet-runner", "someone", "j1", "p-metrics"] {
        assert!(
            !body.contains(identifier),
            "metrics must never contain identifiers, found {identifier:?}\n{body}"
        );
    }
}
