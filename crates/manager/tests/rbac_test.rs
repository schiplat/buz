mod common;

use manager::rbac::verify_cli_admin_token;
use manager::store::exec;
use shared::auth::{Role, TokenManager};
use shared::error::AppError;
use uuid::Uuid;

const JWT_SECRET: &str = "test-secret-0123456789-abcdefghijklmn";

async fn test_pool() -> exec::DbPool {
    let pool = common::test_db(4).await;
    // The migrated schema replaces the hand-built mini-DDL.
    pool
}

async fn insert_user(
    pool: &exec::DbPool,
    id: &str,
    username: &str,
    is_platform_admin: i64,
    is_active: i64,
) {
    exec::execute(
        pool,
        "INSERT INTO users (id, username, password_hash, is_platform_admin, is_active)
         VALUES (?, ?, 'hash', ?, ?)",
        &[
            id.into(),
            username.into(),
            is_platform_admin.into(),
            is_active.into(),
        ],
    )
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

    let user = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &token)
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

    let user = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &token)
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

    let err = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &token)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
}

#[tokio::test]
async fn rejects_runner_role_token() {
    let pool = test_pool().await;
    let token = mint_token(Role::Runner, "runner-1");
    let err = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &token)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::AuthFailed(_)), "{err:?}");
}

#[tokio::test]
async fn rejects_garbage_token() {
    let pool = test_pool().await;
    let err = verify_cli_admin_token(&pool.clone(), JWT_SECRET, "not-a-jwt")
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

    let err = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &token)
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

    let err = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &token)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::AuthFailed(_)), "{err:?}");
}

#[tokio::test]
async fn load_auth_user_exposes_super_admin_flag() {
    use manager::rbac::load_auth_user;

    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    exec::execute(&pool,
        "INSERT INTO users (id, username, password_hash, is_platform_admin, is_super_admin, is_active)
         VALUES (?, 'boss', 'hash', 1, 1, 1)",
        &[admin_id.as_str().into()],
    )
    .await
    .expect("insert super admin");

    let user = load_auth_user(&pool.clone(), &admin_id)
        .await
        .expect("load user");
    assert!(user.is_platform_admin);
    assert!(user.is_super_admin);
}

#[tokio::test]
async fn load_auth_user_defaults_super_admin_false() {
    use manager::rbac::load_auth_user;

    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    insert_user(&pool, &admin_id, "regular", 1, 1).await;

    let user = load_auth_user(&pool.clone(), &admin_id)
        .await
        .expect("load user");
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
    exec::execute(
        &pool,
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix)
         VALUES ('tok-1', ?, 'ci', ?, 'buz_abc…')",
        &[admin_id.as_str().into(), api_token::hash_token(&raw).into()],
    )
    .await
    .expect("insert api token");

    let user = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &raw)
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
    exec::execute(
        &pool,
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix)
         VALUES ('tok-1', ?, 'ci', ?, 'buz_abc…')",
        &[user_id.into(), api_token::hash_token(&raw).into()],
    )
    .await
    .expect("insert api token");

    let err = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &raw)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
}

#[tokio::test]
async fn rejects_revoked_api_token() {
    use shared::api_token;

    let pool = test_pool().await;
    let admin_id = Uuid::new_v4().to_string();
    insert_user(&pool, &admin_id, "admin", 1, 1).await;

    let raw = api_token::generate_token();
    exec::execute(
        &pool,
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, revoked_at)
         VALUES ('tok-1', ?, 'ci', ?, 'buz_abc…', datetime('now'))",
        &[admin_id.as_str().into(), api_token::hash_token(&raw).into()],
    )
    .await
    .expect("insert revoked api token");

    let err = verify_cli_admin_token(&pool.clone(), JWT_SECRET, &raw)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::AuthFailed(_)), "{err:?}");
}
