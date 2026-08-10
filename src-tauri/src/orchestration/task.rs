//! Persisted orchestration task types.
//!
//! Phase 1 deliberately keeps the lifecycle small: one task is queued, starts
//! once, and then reaches exactly one terminal state. Retries create a new task
//! instead of moving a terminal task back to `running`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// State transitions supported by the Phase 1 persistence API.
    ///
    /// A queued task can fail before dispatch or be cancelled. Once a task is
    /// terminal it is immutable; a retry must receive a new task id.
    pub const fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, next),
            (Self::Queued, Self::Running)
                | (Self::Queued, Self::Failed)
                | (Self::Queued, Self::Cancelled)
                | (Self::Running, Self::Succeeded)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Cancelled)
        )
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTaskStatusError(pub String);

impl fmt::Display for ParseTaskStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown task status: {}", self.0)
    }
}

impl std::error::Error for ParseTaskStatusError {}

impl FromStr for TaskStatus {
    type Err = ParseTaskStatusError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(ParseTaskStatusError(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub project_id: String,
    pub master_agent_id: String,
    pub worker_agent_id: String,
    /// Snapshot of the worker's visible name when the task was created.
    pub worker_display_name: String,
    pub title: String,
    pub prompt: String,
    pub status: TaskStatus,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub result: Option<String>,
    pub error: Option<String>,
    /// Reserved for structured results such as changed files, commits and test
    /// reports. Phase 1 permits this to remain null.
    pub artifact: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDispatchRequest {
    pub worker_reference: String,
    pub title: Option<String>,
    pub prompt: String,
}

/// Foundation type for task-aware worker reports in Step 3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskReportRequest {
    pub task_id: String,
    pub reporter_agent_id: String,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    pub artifact: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_roundtrips_as_snake_case() {
        for status in [
            TaskStatus::Queued,
            TaskStatus::Running,
            TaskStatus::Succeeded,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            assert_eq!(TaskStatus::from_str(status.as_str()).unwrap(), status);
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{status}\"")
            );
        }
        assert!(TaskStatus::from_str("unknown").is_err());
    }

    #[test]
    fn terminal_task_statuses_cannot_transition() {
        for terminal in [
            TaskStatus::Succeeded,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            assert!(terminal.is_terminal());
            for next in [
                TaskStatus::Queued,
                TaskStatus::Running,
                TaskStatus::Succeeded,
                TaskStatus::Failed,
                TaskStatus::Cancelled,
            ] {
                assert!(!terminal.can_transition_to(next));
            }
        }
    }

    #[test]
    fn active_task_statuses_allow_only_phase_one_transitions() {
        assert!(TaskStatus::Queued.can_transition_to(TaskStatus::Running));
        assert!(TaskStatus::Queued.can_transition_to(TaskStatus::Failed));
        assert!(TaskStatus::Queued.can_transition_to(TaskStatus::Cancelled));
        assert!(!TaskStatus::Queued.can_transition_to(TaskStatus::Succeeded));

        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Succeeded));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Failed));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Cancelled));
        assert!(!TaskStatus::Running.can_transition_to(TaskStatus::Queued));
    }
}
