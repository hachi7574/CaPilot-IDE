//! Lightweight development CLI for CaPilot orchestration.
//!
//! This binary only translates CLI arguments to the Dispatcher's JSON wire
//! request, sends one line over local IPC, and prints the response. Task and
//! Worker business rules remain in `orchestration::dispatcher`.

use serde::Serialize;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const HELP: &str = "Usage:
  capilot status
  capilot dispatch --worker <name> [--title <title>] --prompt <prompt>
  capilot dispatch <worker> <prompt...>
  capilot report <task-id> <succeeded|failed> <summary...>";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Dispatch {
        worker: String,
        title: Option<String>,
        prompt: String,
    },
    Status,
    Report {
        task_id: String,
        reporter_agent_id: String,
        status: String,
        result: Option<String>,
        error: Option<String>,
    },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_request",
            message: message.into(),
        }
    }

    fn ipc(message: impl Into<String>) -> Self {
        Self {
            code: "ipc_error",
            message: message.into(),
        }
    }

    fn to_json(&self) -> String {
        serde_json::json!({
            "ok": false,
            "code": self.code,
            "message": self.message,
        })
        .to_string()
    }
}

#[derive(Debug)]
enum ParsedCommand {
    Help,
    Request(Request),
}

fn utf8_args(args: impl IntoIterator<Item = OsString>) -> Result<Vec<String>, CliError> {
    args.into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|_| CliError::invalid("capilot arguments must be valid UTF-8"))
        })
        .collect()
}

fn parse_args(args: &[String], reporter_agent_id: Option<&str>) -> Result<ParsedCommand, CliError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(ParsedCommand::Help);
    };
    match command {
        "-h" | "--help" | "help" => Ok(ParsedCommand::Help),
        "status" => {
            if args.len() != 1 {
                return Err(CliError::invalid("status does not accept arguments"));
            }
            Ok(ParsedCommand::Request(Request::Status))
        }
        "ping" => {
            if args.len() != 1 {
                return Err(CliError::invalid("ping does not accept arguments"));
            }
            Ok(ParsedCommand::Request(Request::Ping))
        }
        "dispatch" => parse_dispatch(&args[1..]).map(ParsedCommand::Request),
        "report" => parse_report(&args[1..], reporter_agent_id).map(ParsedCommand::Request),
        other => Err(CliError::invalid(format!("unknown command: {other}"))),
    }
}

fn parse_dispatch(args: &[String]) -> Result<Request, CliError> {
    if args.iter().any(|arg| arg.starts_with("--")) {
        let mut worker = None;
        let mut title = None;
        let mut prompt = None;
        let mut index = 0;
        while index < args.len() {
            let flag = args[index].as_str();
            let value = args
                .get(index + 1)
                .ok_or_else(|| CliError::invalid(format!("missing value for {flag}")))?
                .clone();
            match flag {
                "--worker" if worker.is_none() => worker = Some(value),
                "--title" if title.is_none() => title = Some(value),
                "--prompt" if prompt.is_none() => prompt = Some(value),
                "--worker" | "--title" | "--prompt" => {
                    return Err(CliError::invalid(format!("duplicate option: {flag}")));
                }
                _ => {
                    return Err(CliError::invalid(format!(
                        "unknown dispatch option: {flag}"
                    )))
                }
            }
            index += 2;
        }
        let worker = required_nonempty(worker, "dispatch requires --worker")?;
        let prompt = required_nonempty(prompt, "dispatch requires --prompt")?;
        let title = title.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        return Ok(Request::Dispatch {
            worker,
            title,
            prompt,
        });
    }

    if args.len() < 2 {
        return Err(CliError::invalid(
            "usage: capilot dispatch <worker> <prompt...>",
        ));
    }
    let worker = args[0].trim().to_string();
    let prompt = args[1..].join(" ").trim().to_string();
    if worker.is_empty() || prompt.is_empty() {
        return Err(CliError::invalid("dispatch requires a worker and prompt"));
    }
    Ok(Request::Dispatch {
        worker,
        title: None,
        prompt,
    })
}

fn required_nonempty(value: Option<String>, message: &str) -> Result<String, CliError> {
    let value = value.unwrap_or_default().trim().to_string();
    if value.is_empty() {
        Err(CliError::invalid(message))
    } else {
        Ok(value)
    }
}

fn parse_report(args: &[String], reporter_agent_id: Option<&str>) -> Result<Request, CliError> {
    if args.len() < 3 {
        return Err(CliError::invalid(
            "usage: capilot report <task-id> <succeeded|failed> <summary...>",
        ));
    }
    let task_id = args[0].trim().to_string();
    let status = args[1].trim();
    if !matches!(status, "succeeded" | "failed") {
        return Err(CliError::invalid(
            "report status must be `succeeded` or `failed`",
        ));
    }
    let content = args[2..].join(" ").trim().to_string();
    if task_id.is_empty() || content.is_empty() {
        return Err(CliError::invalid("report requires a task id and result"));
    }
    let reporter_agent_id = reporter_agent_id
        .map(str::trim)
        .filter(|identity| !identity.is_empty())
        .ok_or_else(|| {
            CliError::invalid("CAPILOT_AGENT_ID is missing; report must run inside a CaPilot Agent")
        })?
        .to_string();
    Ok(Request::Report {
        task_id,
        reporter_agent_id,
        status: status.to_string(),
        result: (status == "succeeded").then_some(content.clone()),
        error: (status == "failed").then_some(content),
    })
}

fn socket_path() -> Result<PathBuf, CliError> {
    if let Some(path) = std::env::var_os("CAPILOT_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| CliError::ipc("HOME is not set"))?;
    let pointer = home.join(".capilot").join("socket");
    if pointer.is_file() {
        let path = std::fs::read_to_string(&pointer).map_err(|error| {
            CliError::ipc(format!("cannot read {}: {error}", pointer.display()))
        })?;
        let path = path.trim();
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    Ok(std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".capilot").join("run"))
        .join("capilot-orchestrator.sock"))
}

#[cfg(unix)]
fn send_request(path: &Path, request: &Request) -> Result<String, CliError> {
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(path).map_err(|error| {
        CliError::ipc(format!(
            "orchestrator is not available at {}: {error}",
            path.display()
        ))
    })?;
    let timeout = Some(Duration::from_secs(5));
    stream
        .set_read_timeout(timeout)
        .map_err(|error| CliError::ipc(error.to_string()))?;
    stream
        .set_write_timeout(timeout)
        .map_err(|error| CliError::ipc(error.to_string()))?;
    let mut wire = serde_json::to_vec(request)
        .map_err(|error| CliError::invalid(format!("cannot serialize request: {error}")))?;
    wire.push(b'\n');
    stream
        .write_all(&wire)
        .map_err(|error| CliError::ipc(format!("cannot write request: {error}")))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| CliError::ipc(format!("cannot finish request: {error}")))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| CliError::ipc(format!("cannot read response: {error}")))?;
    if response.trim().is_empty() {
        return Err(CliError::ipc("orchestrator returned an empty response"));
    }
    Ok(response.trim_end().to_string())
}

#[cfg(not(unix))]
fn send_request(_path: &Path, _request: &Request) -> Result<String, CliError> {
    Err(CliError::ipc(
        "this development CLI currently requires Unix socket support",
    ))
}

fn run() -> Result<(), CliError> {
    let args = utf8_args(std::env::args_os().skip(1))?;
    let reporter_agent_id = std::env::var("CAPILOT_AGENT_ID").ok();
    match parse_args(&args, reporter_agent_id.as_deref())? {
        ParsedCommand::Help => {
            println!("{HELP}");
            Ok(())
        }
        ParsedCommand::Request(request) => {
            let response = send_request(&socket_path()?, &request)?;
            println!("{response}");
            Ok(())
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{}", error.to_json());
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn structured_dispatch_preserves_chinese_quotes_spaces_and_multiline_prompt() {
        let ParsedCommand::Request(request) = parse_args(
            &strings(&[
                "dispatch",
                "--worker",
                "阿比西尼亚",
                "--title",
                "检查 \"README\"",
                "--prompt",
                "第一行 有空格\n第二行 \"有引号\"",
            ]),
            None,
        )
        .unwrap() else {
            panic!("expected request");
        };
        assert_eq!(
            request,
            Request::Dispatch {
                worker: "阿比西尼亚".to_string(),
                title: Some("检查 \"README\"".to_string()),
                prompt: "第一行 有空格\n第二行 \"有引号\"".to_string(),
            }
        );
    }

    #[test]
    fn legacy_dispatch_remains_compatible() {
        let ParsedCommand::Request(request) = parse_args(
            &strings(&["dispatch", "阿比西尼亚", "检查", "README"]),
            None,
        )
        .unwrap() else {
            panic!("expected request");
        };
        assert_eq!(
            request,
            Request::Dispatch {
                worker: "阿比西尼亚".to_string(),
                title: None,
                prompt: "检查 README".to_string(),
            }
        );
    }

    #[test]
    fn report_contains_task_identity_status_and_result() {
        let ParsedCommand::Request(request) = parse_args(
            &strings(&["report", "task_123", "succeeded", "第一行\n第二行 \"完成\""]),
            Some("agent-a1"),
        )
        .unwrap() else {
            panic!("expected request");
        };
        assert_eq!(
            request,
            Request::Report {
                task_id: "task_123".to_string(),
                reporter_agent_id: "agent-a1".to_string(),
                status: "succeeded".to_string(),
                result: Some("第一行\n第二行 \"完成\"".to_string()),
                error: None,
            }
        );
    }

    #[test]
    fn report_rejects_invalid_status_and_missing_agent_identity() {
        let invalid = parse_args(
            &strings(&["report", "task_123", "cancelled", "不应接受"]),
            Some("agent-a1"),
        )
        .unwrap_err();
        assert!(invalid.message.contains("succeeded"));

        let missing = parse_args(
            &strings(&["report", "task_123", "failed", "测试失败"]),
            None,
        )
        .unwrap_err();
        assert!(missing.message.contains("CAPILOT_AGENT_ID"));
    }

    #[cfg(unix)]
    #[test]
    fn rust_cli_transport_exchanges_real_json_over_unix_socket() {
        use std::io::{BufRead, BufReader};
        use std::os::unix::net::UnixListener;

        let root = std::env::temp_dir().join(format!(
            "capilot-rust-cli-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("dispatcher.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            stream
                .write_all(b"{\"ok\":true,\"task_id\":\"task_test\"}\n")
                .unwrap();
            request
        });
        let request = Request::Dispatch {
            worker: "阿比西尼亚".to_string(),
            title: Some("检查 README".to_string()),
            prompt: "第一行\n第二行".to_string(),
        };
        let response = send_request(&path, &request).unwrap();
        let received = server.join().unwrap();
        assert_eq!(response, r#"{"ok":true,"task_id":"task_test"}"#);
        assert_eq!(received["worker"], "阿比西尼亚");
        assert_eq!(received["prompt"], "第一行\n第二行");
        let _ = std::fs::remove_dir_all(root);
    }
}
