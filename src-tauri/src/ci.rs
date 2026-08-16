//! CI build status for the current version (developer tool).
//!
//! Polls the GitHub Actions API for the workflow run(s) triggered by the app's
//! current version tag and reports per-job progress. Public repo → anonymous
//! API access (rate-limited to 60 req/h, well above a manual poll cadence).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Repo whose Actions runs are watched (mirrors the updater endpoint owner).
const REPO_OWNER: &str = "hachi7574";
const REPO_NAME: &str = "CaPilot-IDE";

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// A single job inside a workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiJob {
    /// Job name as shown in the Actions UI (e.g. "build (linux, ubuntu-22.04)").
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    /// Job id, used by the frontend as a stable key.
    pub id: i64,
}

/// One workflow run (a Release pipeline execution for a tag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiRun {
    pub id: i64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    /// 0..1 overall progress across the run's jobs.
    pub progress: f64,
    pub jobs: Vec<CiJob>,
    /// `display_title`, e.g. the commit message or "feat: v0.1.2 …".
    pub title: String,
}

/// Computed status for one tag's latest matching run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiStatus {
    pub tag: String,
    /// None when no run has been found for the tag yet (nothing pushed).
    pub run: Option<CiRun>,
    /// Non-empty when the API query itself failed.
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct RunsResponse {
    workflow_runs: Vec<RunLite>,
}

#[derive(Deserialize)]
struct RunLite {
    id: i64,
    name: String,
    status: String,
    conclusion: Option<String>,
    head_branch: Option<String>,
    display_title: Option<String>,
}

#[derive(Deserialize)]
struct JobsResponse {
    jobs: Vec<JobLite>,
}

#[derive(Deserialize)]
struct JobLite {
    id: i64,
    name: String,
    status: String,
    conclusion: Option<String>,
}

/// Build a reqwest client with the ring TLS provider (same pattern as usage.rs;
/// tauri-plugin-updater installs it lazily, install here so we work standalone).
fn client() -> Result<reqwest::Client, String> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(format!("capilot-ide/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败: {e}"))
}

/// Fetch the latest Release-pipeline run for `tag` and its job breakdown.
pub async fn fetch_ci_status(tag: &str) -> Result<CiStatus, String> {
    let client = client()?;

    // 1. Find the run triggered by the tag push.
    let runs_url = format!(
        "https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/actions/runs?event=push&per_page=30"
    );
    let resp = client
        .get(&runs_url)
        .send()
        .await
        .map_err(|e| format!("查询 Actions 列表失败: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status();
        return Err(format!("GitHub API 返回 {code}"));
    }
    let runs: RunsResponse = resp
        .json()
        .await
        .map_err(|e| format!("解析 Actions 列表失败: {e}"))?;

    // Tag-triggered push runs report the tag as head_branch; fall back to the
    // display title (some payloads carry the tag in the title instead).
    let run = runs.workflow_runs.into_iter().find(|r| {
        r.head_branch.as_deref() == Some(tag)
            || r.display_title.as_deref().map(|t| t.contains(tag)).unwrap_or(false)
    });

    let run = match run {
        None => {
            return Ok(CiStatus {
                tag: tag.to_string(),
                run: None,
                error: None,
            });
        }
        Some(r) => r,
    };

    // 2. Fetch the run's jobs for per-step granularity.
    let jobs_url = format!(
        "https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/actions/runs/{}/jobs",
        run.id
    );
    let resp = client
        .get(&jobs_url)
        .send()
        .await
        .map_err(|e| format!("查询 Job 列表失败: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status();
        return Err(format!("GitHub API 返回 {code}"));
    }
    let jobs: JobsResponse = resp
        .json()
        .await
        .map_err(|e| format!("解析 Job 列表失败: {e}"))?;

    let jobs: Vec<CiJob> = jobs
        .jobs
        .into_iter()
        .map(|j| CiJob {
            name: j.name,
            status: j.status,
            conclusion: j.conclusion,
            id: j.id,
        })
        .collect();

    // 3. Progress: completed jobs count fully, in-progress count half.
    let progress = if jobs.is_empty() {
        0.0
    } else {
        let done = jobs
            .iter()
            .filter(|j| j.status == "completed")
            .count() as f64;
        let running = jobs.iter().filter(|j| j.status == "in_progress").count() as f64;
        (done + running * 0.5) / jobs.len() as f64
    };

    Ok(CiStatus {
        tag: tag.to_string(),
        run: Some(CiRun {
            id: run.id,
            name: run.name,
            status: run.status,
            conclusion: run.conclusion,
            progress,
            jobs,
            title: run.display_title.unwrap_or_default(),
        }),
        error: None,
    })
}
