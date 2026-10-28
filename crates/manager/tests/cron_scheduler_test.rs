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

    let rows = fetch_active_cron_pipelines(&pool).await.expect("fetch");
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
