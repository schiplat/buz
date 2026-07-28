use super::{Perm, TeamRole};

#[test]
fn team_role_rank_ordering() {
    assert!(TeamRole::Owner.rank() > TeamRole::Maintainer.rank());
    assert!(TeamRole::Maintainer.rank() > TeamRole::Developer.rank());
    assert!(TeamRole::Developer.rank() > TeamRole::Viewer.rank());
}

#[test]
fn developer_can_write_pipeline_not_secrets() {
    assert!(TeamRole::Developer.at_least(Perm::PipelineWrite.min_role()));
    assert!(TeamRole::Developer.at_least(Perm::PipelineTrigger.min_role()));
    assert!(!TeamRole::Developer.at_least(Perm::SecretManage.min_role()));
    assert!(!TeamRole::Developer.at_least(Perm::HostManage.min_role()));
}

#[test]
fn viewer_cannot_trigger() {
    assert!(TeamRole::Viewer.at_least(Perm::TeamView.min_role()));
    assert!(!TeamRole::Viewer.at_least(Perm::PipelineTrigger.min_role()));
}

#[test]
fn maintainer_can_manage_secrets_not_members() {
    assert!(TeamRole::Maintainer.at_least(Perm::SecretManage.min_role()));
    assert!(TeamRole::Maintainer.at_least(Perm::PipelineDelete.min_role()));
    assert!(!TeamRole::Maintainer.at_least(Perm::TeamManageMembers.min_role()));
}

#[test]
fn parse_roundtrip() {
    for role in [
        TeamRole::Owner,
        TeamRole::Maintainer,
        TeamRole::Developer,
        TeamRole::Viewer,
    ] {
        assert_eq!(TeamRole::parse(role.as_str()), Some(role));
    }
    assert_eq!(TeamRole::parse("admin"), None);
}
