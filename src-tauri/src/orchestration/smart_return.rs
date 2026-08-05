//! 智能返回（smart return）分级规则引擎 (DevPlan §5.4)
//!
//! 规则：失败→完整输出+错误摘要；≤600 字符→完整；600~3000→概述+关键文件；
//! >3000→标题级。只影响 master 向用户的汇报粒度，worker 终端原始输出永不失。

/// Level of detail returned to the master / user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnLevel {
    /// Failure or short output — full content.
    Full,
    /// Medium output — summary + key files.
    Summary,
    /// Long output — title-level only.
    Title,
}

pub const FULL_MAX_CHARS: usize = 600;
pub const SUMMARY_MAX_CHARS: usize = 3000;

/// Classify output length + failure into a return level.
pub fn classify(output: &str, is_failure: bool) -> ReturnLevel {
    if is_failure {
        return ReturnLevel::Full;
    }
    let len = output.chars().count();
    if len <= FULL_MAX_CHARS {
        ReturnLevel::Full
    } else if len <= SUMMARY_MAX_CHARS {
        ReturnLevel::Summary
    } else {
        ReturnLevel::Title
    }
}

/// Produce the text presented for `output` according to smart-return rules.
pub fn summarize(output: &str) -> String {
    match classify(output, false) {
        ReturnLevel::Full => output.trim().to_string(),
        ReturnLevel::Summary => summarize_medium(output),
        ReturnLevel::Title => summarize_title(output),
    }
}

/// Failure presentation: full output is preserved in the worker terminal; the
/// report carries an error excerpt.
pub fn failure_report(output: &str) -> String {
    let excerpt = extract_error_lines(output);
    format!("[失败]\n{}", excerpt)
}

/// Extract likely error lines for the failure report.
fn extract_error_lines(output: &str) -> String {
    let mut out = Vec::new();
    let mut shown = 0;
    for line in output.lines() {
        let low = line.to_lowercase();
        if low.contains("error") || low.contains("failed") || low.contains("panic") {
            out.push(line.trim().to_string());
            shown += 1;
            if shown >= 8 {
                break;
            }
        }
    }
    if out.is_empty() {
        output.lines().take(5).map(|l| l.trim().to_string()).collect::<Vec<_>>().join("\n")
    } else {
        out.join("\n")
    }
}

fn summarize_medium(output: &str) -> String {
    let head: String = output.chars().take(SUMMARY_MAX_CHARS).collect();
    let lines: Vec<&str> = head.lines().collect();

    let mut key_files: Vec<&str> = Vec::new();
    for line in &lines {
        let t = line.trim();
        let is_file = t.starts_with("src/")
            || t.starts_with("ui/")
            || t.starts_with("lib/")
            || t.starts_with("tests/")
            || t.contains(".rs:")
            || t.contains(".tsx:")
            || t.contains(".ts:");
        if is_file {
            key_files.push(t);
        }
        if key_files.len() >= 5 {
            break;
        }
    }

    let mut s = String::new();
    s.push_str("[概述]\n");
    let summary_lines: Vec<&str> = lines.iter().take(12).map(|l| *l).collect();
    s.push_str(&summary_lines.join("\n"));
    if !key_files.is_empty() {
        s.push_str("\n[关键文件]\n");
        s.push_str(&key_files.join("\n"));
    }
    s
}

fn summarize_title(output: &str) -> String {
    for line in output.lines() {
        let t = line.trim();
        if !t.is_empty() {
            return format!("[标题] {}", t);
        }
    }
    "[标题] (无输出)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_is_full() {
        assert_eq!(classify("hello", false), ReturnLevel::Full);
    }

    #[test]
    fn long_output_is_title() {
        let long = "x".repeat(5000);
        assert_eq!(classify(&long, false), ReturnLevel::Title);
    }

    #[test]
    fn failure_is_full() {
        assert_eq!(classify("", true), ReturnLevel::Full);
    }
}
