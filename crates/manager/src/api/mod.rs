use axum::{
    routing::{delete, get, patch, post, put},
    Router,
};

use crate::state::AppState;

mod auth;
mod pipeline_yaml_files;
mod pipelines;
pub mod runners;
// Re-export helper functions for YAML scanner and trigger logic
pub(crate) use pipelines::{
    do_trigger, insert_env_vars, insert_pipeline_secret_bindings, insert_pipeline_var_bindings,
    insert_steps,
};
mod api_tokens;
mod audit_logs;
mod environments;
mod git_hooks;
mod git_webhook;
mod github_sync;
mod hosts;
mod jobs;
mod logs;
mod secrets;
mod teams;
mod trigger_events;
mod users;
mod variables;
pub(crate) mod version;
mod webhook;

pub fn router(state: AppState) -> Router {
    Router::new()
        // Auth (public)
        .route("/auth/login", post(auth::login))
        .route("/auth/setup-status", get(auth::setup_status))
        .route("/auth/setup", post(auth::setup))
        .route("/auth/verify", get(auth::verify))
        .route("/auth/me", get(auth::me))
        .route("/auth/change-password", post(auth::change_password))
        // Users (platform admin)
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", put(users::update).delete(users::delete))
        // Teams
        .route("/teams", get(teams::list).post(teams::create))
        .route(
            "/teams/{id}",
            get(teams::get_one).put(teams::update).delete(teams::delete),
        )
        .route(
            "/teams/{id}/members",
            get(teams::list_members).put(teams::upsert_member),
        )
        .route(
            "/teams/{id}/members/{user_id}",
            delete(teams::remove_member),
        )
        // Runner endpoints
        .route("/runners/register", post(runners::register))
        .route("/runners/heartbeat", post(runners::heartbeat))
        .route("/runners/poll", get(runners::poll))
        // Pipelines / jobs
        .route("/pipelines", get(pipelines::list).post(pipelines::create))
        .route("/pipelines/validate", post(pipelines::validate_yaml))
        // File-backed YAML authoring (scan dir is authority; DB is projection)
        .route(
            "/pipelines/yaml-files",
            get(pipeline_yaml_files::list_files).post(pipeline_yaml_files::create_file),
        )
        .route(
            "/pipelines/yaml-files/{filename}",
            get(pipeline_yaml_files::get_file)
                .put(pipeline_yaml_files::put_file)
                .delete(pipeline_yaml_files::delete_file),
        )
        .route(
            "/pipelines/{id}/yaml-source",
            get(pipeline_yaml_files::get_pipeline_source),
        )
        .route(
            "/pipelines/{id}",
            get(pipelines::get_one)
                .put(pipelines::update)
                .delete(pipelines::delete),
        )
        .route(
            "/pipelines/{id}/toggle-active",
            patch(pipelines::toggle_active),
        )
        .route("/pipelines/{id}/trigger", post(pipelines::trigger))
        .route("/pipelines/{id}/trigger-events", get(trigger_events::list))
        .route(
            "/pipelines/{id}/git-hooks",
            get(git_hooks::list_hooks).post(git_hooks::create_hook),
        )
        .route(
            "/pipelines/{id}/git-hooks/{hook_id}",
            delete(git_hooks::delete_hook),
        )
        .route("/jobs", get(jobs::list))
        .route("/jobs/stats/trend", get(jobs::stats_trend))
        .route("/jobs/recent", get(jobs::recent))
        .route("/jobs/{id}", get(jobs::get_one).delete(jobs::delete))
        .route("/jobs/{id}/cancel", post(jobs::cancel))
        .route("/jobs/{id}/approve", post(jobs::approve))
        .route("/jobs/{id}/reject", post(jobs::reject))
        .route("/jobs/{id}/status-check", get(jobs::status_check))
        .route("/jobs/{id}/claim", post(jobs::claim))
        .route("/jobs/{id}/status", patch(jobs::update_status))
        .route("/jobs/{id}/logs", post(logs::upload).get(logs::get_logs))
        .route("/jobs/{id}/logs/stream", get(logs::stream_logs))
        .route("/pipelines/{id}/stream", get(logs::pipeline_stream))
        // Hosts / secrets / variables
        .route("/hosts", get(hosts::list).post(hosts::create))
        .route("/hosts/test", post(hosts::test_connection))
        .route(
            "/hosts/{id}",
            get(hosts::get_one).put(hosts::update).delete(hosts::delete),
        )
        .route("/secrets", get(secrets::list).post(secrets::create))
        .route(
            "/secrets/{id}",
            get(secrets::get_one)
                .put(secrets::update)
                .delete(secrets::delete),
        )
        .route("/variables", get(variables::list).post(variables::create))
        .route(
            "/variables/{id}",
            get(variables::get_one)
                .put(variables::update)
                .delete(variables::delete),
        )
        .route(
            "/environments",
            get(environments::list).post(environments::create),
        )
        .route(
            "/environments/{id}",
            get(environments::get_one)
                .put(environments::update)
                .delete(environments::delete),
        )
        .route(
            "/environments/{id}/overrides",
            get(environments::list_overrides).put(environments::upsert_override),
        )
        .route(
            "/environments/{id}/overrides/{var_id}",
            delete(environments::delete_override),
        )
        .route(
            "/environments/{id}/secret-overrides",
            get(environments::list_secret_overrides).put(environments::upsert_secret_override),
        )
        .route(
            "/environments/{id}/secret-overrides/{secret_id}",
            delete(environments::delete_secret_override),
        )
        // Audit logs (platform admin; super admin sees all, others only their own)
        .route("/audit-logs", get(audit_logs::list))
        // GitHub Actions sync (platform admin; secret binds use SecretManage)
        .route(
            "/github-sync/connections",
            get(github_sync::list_connections).post(github_sync::create_connection),
        )
        .route(
            "/github-sync/connections/{id}",
            put(github_sync::update_connection).delete(github_sync::delete_connection),
        )
        .route("/github-sync/repos/fetch", post(github_sync::fetch_repos))
        .route(
            "/github-sync/repos",
            get(github_sync::list_repos).post(github_sync::create_repo),
        )
        .route(
            "/github-sync/repos/{id}",
            put(github_sync::update_repo).delete(github_sync::delete_repo),
        )
        .route("/github-sync/test", post(github_sync::test_connection))
        .route(
            "/secrets/{id}/github-binds",
            get(github_sync::list_secret_binds).post(github_sync::add_secret_bind),
        )
        .route(
            "/secrets/{id}/github-binds/push",
            post(github_sync::push_secret_binds),
        )
        .route(
            "/secrets/{id}/github-binds/{bind_id}",
            patch(github_sync::patch_secret_bind).delete(github_sync::delete_secret_bind),
        )
        .route(
            "/variables/{id}/github-binds",
            get(github_sync::list_variable_binds).post(github_sync::add_variable_bind),
        )
        .route(
            "/variables/{id}/github-binds/push",
            post(github_sync::push_variable_binds),
        )
        .route(
            "/variables/{id}/github-binds/{bind_id}",
            patch(github_sync::patch_variable_bind).delete(github_sync::delete_variable_bind),
        )
        // API tokens (platform admin; super admin sees all)
        .route(
            "/api-tokens",
            get(api_tokens::list).post(api_tokens::create),
        )
        .route("/api-tokens/{id}", delete(api_tokens::revoke))
        // Runner management (platform admin)
        .route(
            "/runners",
            get(runners::list_runners).post(runners::create_runner),
        )
        .route(
            "/runners/{id}",
            get(runners::get_runner)
                .put(runners::update_runner)
                .delete(runners::delete_runner),
        )
        .route("/runners/{id}/freeze", patch(runners::toggle_freeze))
        .route("/runners/{id}/restart", post(runners::restart))
        .route(
            "/runners/{id}/register-token/reissue",
            post(runners::reissue_register_token),
        )
        .route("/runners/{id}/events", get(runners::get_events))
        .route(
            "/hooks/git/{provider}/{hook_key}",
            post(git_webhook::handle),
        )
        .route("/hooks/{path}", post(webhook::handle))
        .with_state(state)
}

#[cfg(test)]
#[path = "tests/api_token_tests.rs"]
mod api_token_integration_tests;

#[cfg(test)]
#[path = "tests/pipeline_env_tests.rs"]
mod pipeline_env_integration_tests;

#[cfg(test)]
#[path = "tests/debug_trigger_tests.rs"]
mod debug_trigger_integration_tests;

#[cfg(test)]
#[path = "tests/dispatch_log_mask_tests.rs"]
mod dispatch_log_mask_integration_tests;

#[cfg(test)]
#[path = "tests/verbose_trigger_tests.rs"]
mod verbose_trigger_integration_tests;

#[cfg(test)]
#[path = "tests/pipeline_yaml_files_tests.rs"]
mod pipeline_yaml_files_integration_tests;

#[cfg(test)]
#[path = "tests/runner_standalone_register_tests.rs"]
mod runner_standalone_register_integration_tests;

#[cfg(test)]
#[path = "tests/runner_restart_tests.rs"]
mod runner_restart_integration_tests;

#[cfg(test)]
#[path = "tests/github_sync_tests.rs"]
mod github_sync_integration_tests;

#[cfg(test)]
#[path = "tests/input_validation_trigger_tests.rs"]
mod input_validation_trigger_integration_tests;

#[cfg(test)]
#[path = "tests/environments_tests.rs"]
mod environments_integration_tests;

#[cfg(test)]
#[path = "tests/environment_approval_tests.rs"]
mod environment_approval_integration_tests;

#[cfg(test)]
#[path = "tests/jobs_stats_tests.rs"]
mod jobs_stats_integration_tests;
