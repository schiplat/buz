use runner::client::image_prefetch::{
    candidates, is_public_library_digest, ImagePrefetch, MAX_CONCURRENT_PULLS,
};
use runner::config::Config;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn public_library_digest_parser_accepts_only_immutable_official_images() {
    let digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    for reference in [
        format!("alpine@{digest}"),
        format!("docker.io/library/alpine@{digest}"),
        format!("docker.io/library/my.image_name-1@{digest}"),
    ] {
        assert!(is_public_library_digest(&reference), "{reference}");
    }

    for reference in [
        "alpine:3.20".to_string(),
        "ubuntu".to_string(),
        format!("alpine:3.20@{digest}"),
        format!("docker.io/user/alpine@{digest}"),
        format!("registry.example/alpine@{digest}"),
        format!("localhost:5000/alpine@{digest}"),
        format!("docker.io/library/alpine@sha512:{}", "a".repeat(128)),
        format!("alpine@sha256:{}", "a".repeat(63)),
        format!(" alpine@{digest}"),
        format!("alpine@{digest} "),
        format!("alpine@{digest}@{digest}"),
    ] {
        assert!(
            !is_public_library_digest(&reference),
            "unexpectedly accepted {reference}"
        );
    }
}

#[test]
fn candidates_filter_engine_policy_and_deduplicate() {
    let mut config = Config::default();
    config.engine.docker_enabled = true;
    let digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let image = format!("alpine@{digest}");
    let tasks = vec![
        json!({"exec_type":"docker", "image":image}),
        json!({"exec_type":"docker", "image":image}),
        json!({"exec_type":"shell", "image":image}),
        json!({"exec_type":"docker", "image":"alpine:3.20"}),
        json!({"exec_type":"docker", "image":image, "privileged":true}),
    ];
    assert_eq!(candidates(&tasks, &config), vec![image]);

    config.engine.docker_enabled = false;
    assert!(candidates(&tasks, &config).is_empty());
}

#[test]
fn no_candidates_means_no_anonymous_config_or_pull_tasks() {
    let config = Config::default();
    let tasks = vec![json!({"exec_type":"docker", "image":"alpine:3.20"})];
    assert!(ImagePrefetch::start(&tasks, &config, "job-1").is_none());
}

#[tokio::test]
async fn prefetch_is_bounded_uses_empty_config_and_cleans_up() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let seen_configs = Arc::new(Mutex::new(Vec::new()));
    let images: Vec<String> = (0..8).map(|i| format!("image-{i}")).collect();
    let active_for_pull = Arc::clone(&active);
    let max_for_pull = Arc::clone(&max_active);
    let configs_for_pull = Arc::clone(&seen_configs);

    let mut prefetch = ImagePrefetch::start_with_candidates(
        images.clone(),
        "job-test",
        move |_image, config_dir| {
            let active = Arc::clone(&active_for_pull);
            let max_active = Arc::clone(&max_for_pull);
            let configs = Arc::clone(&configs_for_pull);
            async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(current, Ordering::SeqCst);
                assert!(std::path::Path::new(&config_dir).is_dir());
                assert_eq!(
                    std::fs::read_dir(&config_dir).unwrap().count(),
                    0,
                    "anonymous config starts empty"
                );
                configs.lock().unwrap().push(config_dir);
                tokio::time::sleep(Duration::from_millis(20)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }
        },
    )
    .expect("eligible candidates create prefetch manager");

    for image in &images {
        assert!(prefetch.wait_for(image).await.unwrap().is_ok());
    }
    assert!(max_active.load(Ordering::SeqCst) <= MAX_CONCURRENT_PULLS);
    assert_eq!(seen_configs.lock().unwrap().len(), images.len());
    let config_dir = seen_configs.lock().unwrap()[0].clone();
    prefetch.abort_and_drain().await;
    drop(prefetch);
    assert!(!std::path::Path::new(&config_dir).exists());
}

#[tokio::test]
async fn abort_and_drain_stops_pending_pulls_before_config_cleanup() {
    struct ActiveGuard(Arc<AtomicUsize>);
    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    let active = Arc::new(AtomicUsize::new(0));
    let seen_config = Arc::new(Mutex::new(None::<String>));
    let active_for_pull = Arc::clone(&active);
    let config_for_pull = Arc::clone(&seen_config);
    let candidates = (0..5).map(|i| format!("image-{i}")).collect();
    let mut prefetch =
        ImagePrefetch::start_with_candidates(candidates, "job-cancel", move |_image, config| {
            let active = Arc::clone(&active_for_pull);
            let seen = Arc::clone(&config_for_pull);
            async move {
                active.fetch_add(1, Ordering::SeqCst);
                let _guard = ActiveGuard(active);
                *seen.lock().unwrap() = Some(config);
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(())
            }
        })
        .expect("prefetch manager");

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(active.load(Ordering::SeqCst) > 0);
    let config_dir = seen_config.lock().unwrap().clone().unwrap();
    prefetch.abort_and_drain().await;
    assert_eq!(active.load(Ordering::SeqCst), 0, "all pull futures drained");
    drop(prefetch);
    assert!(
        !std::path::Path::new(&config_dir).exists(),
        "anonymous config is removed only after pulls stop"
    );
}

#[tokio::test]
async fn prefetch_failure_is_returned_for_warning_only_handling() {
    let mut prefetch = ImagePrefetch::start_with_candidates(
        vec!["alpine@sha256:deadbeef".into()],
        "job-failure",
        |_image, _config| async { anyhow::bail!("registry unavailable") },
    )
    .expect("prefetch manager");
    let result = prefetch.wait_for("alpine@sha256:deadbeef").await;
    assert!(result.expect("candidate result").is_err());
    prefetch.abort_and_drain().await;
}
