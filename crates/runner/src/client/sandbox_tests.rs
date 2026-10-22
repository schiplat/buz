use super::{Request, Response};
use shared::protocol::RunnerExecutionMode;

#[test]
fn supervisor_wire_requests_include_mode_and_scoped_job_data() {
    let request = Request::CreateJob {
        job_id: "job-1",
        mode: RunnerExecutionMode::Container,
        workspace: "/workspace/job-1",
        image: "registry.example/job@sha256:deadbeef",
        guest_image: "",
        firecracker_runtime: "",
    };
    let value = serde_json::to_value(request).unwrap();
    assert_eq!(value["op"], "create_job");
    assert_eq!(value["mode"], "container");
    assert_eq!(value["workspace"], "/workspace/job-1");
}

#[test]
fn supervisor_response_defaults_optional_fields() {
    let response: Response = serde_json::from_str(r#"{"ok":true,"handle":"h1"}"#).unwrap();
    assert!(response.ok);
    assert_eq!(response.handle, "h1");
    assert_eq!(response.content, "");
    assert_eq!(response.error, "");
    assert!(response.capabilities.is_none());
}

#[test]
fn capability_probe_request_uses_machine_local_pinned_runtime_config() {
    let request = Request::ProbeCapabilities {
        container_image: "job@sha256:abc",
        vm_image: "guest@sha256:def",
        firecracker_runtime: "firecracker",
    };
    let value = serde_json::to_value(request).unwrap();
    assert_eq!(value["op"], "probe_capabilities");
    assert_eq!(value["container_image"], "job@sha256:abc");
    assert_eq!(value["firecracker_runtime"], "firecracker");
}
