use serde::{Deserialize, Serialize};

/// Team membership role (within a single team).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Owner,
    Maintainer,
    Developer,
    Viewer,
}

impl TeamRole {
    pub fn as_str(self) -> &'static str {
        match self {
            TeamRole::Owner => "owner",
            TeamRole::Maintainer => "maintainer",
            TeamRole::Developer => "developer",
            TeamRole::Viewer => "viewer",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(TeamRole::Owner),
            "maintainer" => Some(TeamRole::Maintainer),
            "developer" => Some(TeamRole::Developer),
            "viewer" => Some(TeamRole::Viewer),
            _ => None,
        }
    }

    /// Rank for comparisons (higher = more privileged).
    pub fn rank(self) -> u8 {
        match self {
            TeamRole::Viewer => 1,
            TeamRole::Developer => 2,
            TeamRole::Maintainer => 3,
            TeamRole::Owner => 4,
        }
    }

    pub fn at_least(self, required: TeamRole) -> bool {
        self.rank() >= required.rank()
    }
}

/// Fine-grained permission checked against platform admin or team role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perm {
    /// View pipelines, jobs, logs, secret metadata, hosts metadata
    TeamView,
    /// Trigger a pipeline
    PipelineTrigger,
    /// Create / edit pipeline and its variables
    PipelineWrite,
    /// Delete pipeline
    PipelineDelete,
    /// Create / update / delete secrets and bind to pipelines
    SecretManage,
    /// Create / update / delete team-scoped hosts
    HostManage,
    /// Reference hosts in SSH steps
    HostUse,
    /// Manage team membership
    TeamManageMembers,
    /// Delete the team
    TeamDelete,
}

impl Perm {
    /// Minimum team role required (platform admin bypasses this).
    pub fn min_role(self) -> TeamRole {
        match self {
            Perm::TeamView => TeamRole::Viewer,
            Perm::PipelineTrigger | Perm::PipelineWrite | Perm::HostUse => TeamRole::Developer,
            Perm::PipelineDelete | Perm::SecretManage | Perm::HostManage => TeamRole::Maintainer,
            Perm::TeamManageMembers | Perm::TeamDelete => TeamRole::Owner,
        }
    }
}

#[cfg(test)]
#[path = "rbac_tests.rs"]
mod tests;
