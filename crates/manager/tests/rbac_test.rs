use manager::rbac::verify_cli_admin_token;
use shared::auth::{Role, TokenManager};
use shared::error::AppError;
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmn";

async fn test_pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    let sql = r#"
        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            display_name TEXT NOT NULL DEFAULT '',
            is_platform_admin INTEGER NOT NULL DEFAULT 0,
            is_super_admin INTEGER NOT NULL DEFAULT 0,
            is_active INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS api_tokens (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            name TEXT NOT NULL,
            token_hash TEXT NOT NULL,
            token_prefix TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            last_used_at TEXT,
            revoked_at TEXT
        );
        CREATE TABLE IF NOT EXISTS teams (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS team_members (
            team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
            user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (team_id, user_id)
        );
    "#;
    for stmt in sql.split(';') {
        let trimmed = stmt.trim();
        if !trimmed.is_empty() {
            sqlx::query(trimmed)
                .execute(&pool)
                .await
                .expect("apply schema");
        }
    }
    pool
}

async fn insert_user(
    pool: &sqlx::SqlitePool,
    id: &str,
    username: &str,
    is_platform_admin: i64,
    is_active: i64,
) {
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, is_platform_admin, is_active)
         VALUES (?, ?, 'hash', ?, ?)",
    )
    .bind(id)
    .bind(username)
    .bind(is_platform_admin)
    .bind(is_active)
    .execute(pool)
    .await
    .expect("insert user");
}

fn mint_token(role: Role, sub: &str) -> String {
    TokenManager::new(JWT_SECRET)
        .create_token(sub, role, 1)
        .expect("mint token")
}

#[tokio::test]
async fn accepts_platform_admin_token() {
    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    insert_user(&pool, &admin_id, "admin", 1, 1).await;
    let token = mint_token(Role::Admin, &admin_id);

    let user = verify_cli_admin_token(&pool, JWT_SECRET, &token)
        .await
        .expect("admin token accepted");
    assert!(user.is_platform_admin);
    assert_eq!(user.id, admin_id);
}

#[tokio::test]
async fn accepts_platform_admin_user_role_token() {
    // Login issues a User-role JWT for a user that happens to be platform admin.
    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    insert_user(&pool, &admin_id, "boss", 1, 1).await;
    let token = mint_token(Role::User, &admin_id);

    let user = verify_cli_admin_token(&pool, JWT_SECRET, &token)
        .await
        .expect("user-role token of platform admin accepted");
    assert!(user.is_platform_admin);
}

#[tokio::test]
async fn rejects_non_admin_user() {
    let pool = test_pool().await;
    let user_id = Uuid::new_v4().to_string();
    insert_user(&pool, &user_id, "pleb", 0, 1).await;
    let token = mint_token(Role::User, &user_id);

    let err = verify_cli_admin_token(&pool, JWT_SECRET, &token)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Unauthorized(_)), "{err:?}");
}

#[tokio::test]
async fn rejects_runner_role_token() {
    let pool = test_pool().await;
    let token = mint_token(Role::Runner, "runner-1");
    let err = verify_cli_admin_token(&pool, JWT_SECRET, &token)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::AuthFailed(_)), "{err:?}");
}

#[tokio::test]
async fn rejects_garbage_token() {
    let pool = test_pool().await;
    let err = verify_cli_admin_token(&pool, JWT_SECRET, "not-a-jwt")
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::AuthFailed(_)), "{err:?}");
}

#[tokio::test]
async fn rejects_token_signed_with_different_secret() {
    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    insert_user(&pool, &admin_id, "admin", 1, 1).await;
    let token = TokenManager::new("a-different-secret-abcdefghijklmnop")
        .create_token(&admin_id, Role::Admin, 1)
        .expect("mint token");

    let err = verify_cli_admin_token(&pool, JWT_SECRET, &token)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::AuthFailed(_)), "{err:?}");
}

#[tokio::test]
async fn rejects_disabled_user() {
    let pool = test_pool().await;
    let user_id = Uuid::new_v4().to_string();
    insert_user(&pool, &user_id, "disabled", 1, 0).await;
    let token = mint_token(Role::Admin, &user_id);

    let err = verify_cli_admin_token(&pool, JWT_SECRET, &token)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::AuthFailed(_)), "{err:?}");
}

#[tokio::test]
async fn load_auth_user_exposes_super_admin_flag() {
    use manager::rbac::load_auth_user;

    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, is_platform_admin, is_super_admin, is_active)
         VALUES (?, 'boss', 'hash', 1, 1, 1)",
    )
    .bind(&admin_id)
    .execute(&pool)
    .await
    .expect("insert super admin");

    let user = load_auth_user(&pool, &admin_id).await.expect("load user");
    assert!(user.is_platform_admin);
    assert!(user.is_super_admin);
}

#[tokio::test]
async fn load_auth_user_defaults_super_admin_false() {
    use manager::rbac::load_auth_user;

    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    insert_user(&pool, &admin_id, "regular", 1, 1).await;

    let user = load_auth_user(&pool, &admin_id).await.expect("load user");
    assert!(user.is_platform_admin);
    assert!(!user.is_super_admin);
}

#[tokio::test]
async fn accepts_admin_api_token() {
    use shared::api_token;

    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    insert_user(&pool, &admin_id, "admin", 1, 1).await;

    let raw = api_token::generate_token();
    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix)
         VALUES ('tok-1', ?, 'ci', ?, 'buz_abc…')",
    )
    .bind(&admin_id)
    .bind(api_token::hash_token(&raw))
    .execute(&pool)
    .await
    .expect("insert api token");

    let user = verify_cli_admin_token(&pool, JWT_SECRET, &raw)
        .await
        .expect("admin api token accepted");
    assert!(user.is_platform_admin);
    assert_eq!(user.id, admin_id);
}

#[tokio::test]
async fn rejects_api_token_of_non_admin_user() {
    use shared::api_token;

    let pool = test_pool().await;
    let user_id = Uuid::new_v4().to_string();
    insert_user(&pool, &user_id, "pleb", 0, 1).await;

    let raw = api_token::generate_token();
    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix)
         VALUES ('tok-1', ?, 'ci', ?, 'buz_abc…')",
    )
    .bind(&user_id)
    .bind(api_token::hash_token(&raw))
    .execute(&pool)
    .await
    .expect("insert api token");

    let err = verify_cli_admin_token(&pool, JWT_SECRET, &raw)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Unauthorized(_)), "{err:?}");
}

#[tokio::test]
async fn rejects_revoked_api_token() {
    use shared::api_token;

    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    insert_user(&pool, &admin_id, "admin", 1, 1).await;

    let raw = api_token::generate_token();
    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, revoked_at)
         VALUES ('tok-1', ?, 'ci', ?, 'buz_abc…', datetime('now'))",
    )
    .bind(&admin_id)
    .bind(api_token::hash_token(&raw))
    .execute(&pool)
    .await
    .expect("insert revoked api token");

    let err = verify_cli_admin_token(&pool, JWT_SECRET, &raw)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::AuthFailed(_)), "{err:?}");
}
