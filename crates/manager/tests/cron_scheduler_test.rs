use manager::services::cron_scheduler::fetch_active_cron_pipelines;
use manager::services::cron_scheduler::normalize_cron;
use std::str::FromStr;

#[test]
fn normalize_5_to_7_field() {
    assert_eq!(normalize_cron("0 2 * * *"), "0 0 2 * * * *");
    assert_eq!(normalize_cron("*/5 * * * *"), "0 */5 * * * * *");
    assert_eq!(normalize_cron("30 4 1,15 * *"), "0 30 4 1,15 * * *");
}

#[test]
fn normalize_6_to_7_field() {
    // sec + 5 fields → append year wildcard
    assert_eq!(normalize_cron("0 0 2 * * *"), "0 0 2 * * * *");
    assert_eq!(normalize_cron("15 */10 * * * *"), "15 */10 * * * * *");
}

#[test]
fn normalize_7_field_passthrough() {
    assert_eq!(normalize_cron("30 0 2 * * * *"), "30 0 2 * * * *");
}

#[test]
fn parse_5_field_cron() {
    let expr = "0 2 * * *";
    let normalized = normalize_cron(expr);
    let schedule = cron::Schedule::from_str(&normalized);
    assert!(
        schedule.is_ok(),
        "normalized 5-field cron should parse: {normalized}"
    );
}

#[test]
fn parse_6_field_cron() {
    let expr = "0 0 2 * * *";
    let normalized = normalize_cron(expr);
    let schedule = cron::Schedule::from_str(&normalized);
    assert!(
        schedule.is_ok(),
        "normalized 6-field cron should parse: {normalized}"
    );
}

#[test]
fn parse_7_field_cron() {
    let expr = "30 0 2 * * * *";
    let schedule = cron::Schedule::from_str(expr);
    assert!(schedule.is_ok(), "valid 7-field cron should parse");
}

#[test]
fn reject_invalid_cron() {
    let expr = "invalid string";
    let normalized = normalize_cron(expr);
    // best-effort passthrough — Schedule::from_str must still fail
    assert!(cron::Schedule::from_str(&normalized).is_err());
}

#[test]
fn daily_cron_upcoming() {
    let expr = "0 2 * * *";
    let normalized = normalize_cron(expr);
    let schedule = cron::Schedule::from_str(&normalized).unwrap();
    let mut upcoming = schedule.upcoming(chrono::Utc);
    assert!(
        upcoming.next().is_some(),
        "should have an upcoming fire time"
    );
}

#[test]
fn every_minute_cron_has_upcoming() {
    let normalized = normalize_cron("* * * * *");
    let schedule = cron::Schedule::from_str(&normalized).unwrap();
    let next = schedule.upcoming(chrono::Utc).next();
    assert!(next.is_some());
}

/// The scheduler only considers active cron pipelines with an expression, and
/// passes each pipeline's `cron_environment` through as the fixed target.
#[tokio::test]
async fn fetch_active_cron_pipelines_returns_configured_environment() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    manager::db::run_migrations(&pool, "test-secret-0123456789abcdef")
        .await
        .expect("migrate db");

    let seed = |id: &str, expr: Option<&str>, env: &str, active: i64| {
        let pool = pool.clone();
        let id = id.to_string();
        let expr = expr.map(str::to_string);
        let env = env.to_string();
        async move {
            sqlx::query(
                "INSERT INTO pipelines (id, name, trigger_type, cron_expr, cron_environment, is_active)
                 VALUES (?, ?, 'cron', ?, ?, ?)",
            )
            .bind(&id)
            .bind(&id)
            .bind(&expr)
            .bind(&env)
            .bind(active)
            .execute(&pool)
            .await
            .expect("seed pipeline");
        }
    };
    seed("cron-nightly", Some("0 3 * * *"), "nightly", 1).await;
    seed("cron-missing-env", Some("0 4 * * *"), "", 1).await;
    seed("cron-inactive", Some("0 5 * * *"), "nightly", 0).await;
    seed("cron-no-expr", None, "nightly", 1).await;
    seed("cron-manual", None, "", 1).await;

    let rows = fetch_active_cron_pipelines(&manager::store::exec::DbPool::from_sqlite(&pool))
        .await
        .expect("fetch");
    let mut found: Vec<(String, String)> = rows
        .into_iter()
        .map(|(id, _name, _expr, env)| (id, env))
        .collect();
    found.sort();

    assert_eq!(
        found,
        vec![
            ("cron-missing-env".to_string(), String::new()),
            ("cron-nightly".to_string(), "nightly".to_string()),
        ],
        "only active pipelines with an expression are scheduled; \
         cron_environment is passed through verbatim (empty stays empty and \
         is rejected downstream)"
    );
}

// ── claim_cron_fire: persisted, transactional fire decisions ────────────

use manager::services::cron_scheduler::claim_cron_fire;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

async fn claim_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    manager::db::run_migrations(&pool, "test-secret-0123456789-abcdefghijklmnop")
        .await
        .expect("migrate db");
    pool
}

async fn seed_cron_pipeline(pool: &SqlitePool, id: &str, env: &str) {
    sqlx::query(
        "INSERT INTO pipelines (id, name, trigger_type, cron_expr, cron_environment, is_active)
         VALUES (?, ?, 'cron', '* * * * *', ?, 1)",
    )
    .bind(id)
    .bind(id)
    .bind(env)
    .execute(pool)
    .await
    .expect("seed cron pipeline");
}

/// Every-second schedule: a fire point always exists inside any non-empty
/// window, so claim outcomes are deterministic regardless of test timing.
fn every_second() -> cron::Schedule {
    cron::Schedule::from_str("* * * * * *").expect("every-second schedule")
}

#[tokio::test]
async fn first_claim_fires_and_stamps() {
    let pool = claim_pool().await;
    seed_cron_pipeline(&pool, "p-cron", "default").await;

    let schedule = every_second();
    let fired = claim_cron_fire(
        &manager::store::exec::DbPool::from_sqlite(&pool),
        "p-cron",
        &schedule,
        30,
    )
    .await
    .expect("first claim");
    assert!(fired, "never-fired pipeline fires");

    let stamp: String =
        sqlx::query_scalar("SELECT cron_last_fired_at FROM pipelines WHERE id = 'p-cron'")
            .fetch_one(&pool)
            .await
            .expect("read stamp");
    assert!(!stamp.is_empty(), "cron_last_fired_at persisted");
}

#[tokio::test]
async fn second_claim_within_gap_is_refused() {
    let pool = claim_pool().await;
    seed_cron_pipeline(&pool, "p-cron", "default").await;
    let schedule = every_second();

    assert!(claim_cron_fire(
        &manager::store::exec::DbPool::from_sqlite(&pool),
        "p-cron",
        &schedule,
        30
    )
    .await
    .unwrap());
    // Leadership-flip simulation: a fresh claimer immediately after the fire
    // must see the persisted stamp and refuse — the old in-memory limiter
    // allowed a re-fire exactly here.
    assert!(
        !claim_cron_fire(
            &manager::store::exec::DbPool::from_sqlite(&pool),
            "p-cron",
            &schedule,
            30
        )
        .await
        .unwrap(),
        "persisted stamp blocks the immediate re-fire"
    );
}

#[tokio::test]
async fn gap_expiry_allows_the_next_window() {
    let pool = claim_pool().await;
    seed_cron_pipeline(&pool, "p-cron", "default").await;

    // Backdate the stamp past the 120s gap.
    sqlx::query("UPDATE pipelines SET cron_last_fired_at = datetime('now', '-130 seconds') WHERE id = 'p-cron'")
        .execute(&pool)
        .await
        .expect("backdate stamp");
    let fired = claim_cron_fire(
        &manager::store::exec::DbPool::from_sqlite(&pool),
        "p-cron",
        &every_second(),
        30,
    )
    .await
    .expect("claim after gap");
    assert!(fired, "gap expired → fires again");
}

#[tokio::test]
async fn empty_window_never_fires() {
    let pool = claim_pool().await;
    seed_cron_pipeline(&pool, "p-cron", "default").await;

    // check_interval = 0 makes the window empty (next_fire > now && <= now),
    // so no schedule can fire — the window bound is what gates, not luck.
    let fired = claim_cron_fire(
        &manager::store::exec::DbPool::from_sqlite(&pool),
        "p-cron",
        &every_second(),
        0,
    )
    .await
    .expect("claim with empty window");
    assert!(!fired);

    // Garbage stamp (unparseable) behaves like "never fired".
    sqlx::query("UPDATE pipelines SET cron_last_fired_at = 'not-a-date' WHERE id = 'p-cron'")
        .execute(&pool)
        .await
        .expect("corrupt stamp");
    let fired = claim_cron_fire(
        &manager::store::exec::DbPool::from_sqlite(&pool),
        "p-cron",
        &every_second(),
        30,
    )
    .await
    .expect("claim with garbage stamp");
    assert!(fired, "unparseable stamp falls back to never-fired");
}
