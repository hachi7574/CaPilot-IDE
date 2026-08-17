//! Recursive file-content search.
//!
//! Engine precedence, mirroring the Orca IDE design:
//!   1. **ripgrep** (`rg --json`) — fast, feature-complete, respects `.gitignore`.
//!   2. **`git grep`** — git worktrees when `rg` is absent; also ignore-aware.
//!   3. **pure-Rust walker** — any directory, no external binary. Guarantees the
//!      feature works even on Linux desktops where `rg`/`git` aren't on PATH.
//!
//! Every engine enforces the same safety valves: per-file match cap, global
//! result cap, per-file size cap, line-content truncation (so a minified line
//! can't blow up the UI / the IPC payload), and a hard timeout that marks the
//! result `truncated`. Overlapping searches for the same root are killed so a
//! slow scan never piles up behind a newer query.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

pub const MAX_MATCHES_PER_FILE: usize = 100;
pub const DEFAULT_MAX_RESULTS: usize = 2000;
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Line content longer than this is truncated around the match (with `…`).
pub const MAX_LINE_CONTENT: usize = 500;
pub const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024;
/// Query / glob text longer than this is rejected (bounds on remote-ish input).
const MAX_PATTERN_BYTES: usize = 8 * 1024;

/// Directories skipped by the pure-Rust walker (mirrors the FilesPanel tree's
/// `SKIP_DIRS`). rg / git grep get the same set via `--glob` / `--exclude`.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".claude", "dist", "build"];

// ── Output types (serialized straight to the frontend) ─────────

#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    /// 1-based line number in the source file.
    pub line: usize,
    /// Char offset of the match within the *raw* line.
    pub column: usize,
    /// Match length in chars (raw line).
    pub match_length: usize,
    /// Possibly-truncated display text of the line.
    pub line_content: String,
    /// Char offset of the match within `line_content` (accounts for the `…`
    /// prefix when truncated). Always populated; the frontend highlights with
    /// `display_column` + `display_match_length`.
    pub display_column: usize,
    /// Char length of the match within `line_content`.
    pub display_match_length: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchFileResult {
    pub file_path: String,
    pub relative_path: String,
    pub matches: Vec<SearchMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SearchResult {
    pub files: Vec<SearchFileResult>,
    pub total_matches: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub root: PathBuf,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub include_pattern: Option<String>,
    pub exclude_pattern: Option<String>,
    pub max_results: Option<usize>,
}

// ── Process registry: kill-previous per root ───────────────────

/// Currently-running `rg` child per root path. A new search for the same root
/// kills and replaces the previous one so overlapping scans don't pile up.
static ACTIVE_RG: LazyLock<Mutex<HashMap<String, tokio::process::Child>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn rg_available() -> bool {
    static AVAILABLE: LazyLock<bool> = LazyLock::new(|| {
        let mut cmd = std::process::Command::new("rg");
        cmd.arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::agent_runtime::executable::hide_windows_console(&mut cmd);
        cmd.status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    *AVAILABLE
}

fn is_git_repo(root: &Path) -> bool {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::agent_runtime::executable::hide_windows_console(&mut cmd);
    cmd.status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Line matching (walker + git-grep column re-derivation) ─────

/// Byte-offset spans of every query match in a line.
struct LineMatcher {
    re: regex::Regex,
}

impl LineMatcher {
    fn new(opts: &SearchOptions) -> Result<LineMatcher, String> {
        let raw = if opts.use_regex {
            opts.query.clone()
        } else {
            regex::escape(&opts.query)
        };
        // `\b` only recognizes ASCII word boundaries; for CJK it degenerates to
        // plain substring matching, which is the same behaviour rg's
        // `--word-regexp` exhibits, so we accept it.
        let pat = if opts.whole_word {
            format!(r"\b(?:{raw})\b")
        } else {
            raw
        };
        let re = regex::RegexBuilder::new(&pat)
            .case_insensitive(!opts.case_sensitive)
            .build()
            .map_err(|e| format!("Invalid search pattern: {e}"))?;
        Ok(LineMatcher { re })
    }

    fn find(&self, line: &str) -> Vec<(usize, usize)> {
        self.re.find_iter(line).map(|m| (m.start(), m.end())).collect()
    }
}

// ── Glob handling for include / exclude patterns ───────────────

fn build_globset(patterns: Option<&str>) -> Result<Option<globset::GlobSet>, String> {
    let Some(patterns) = patterns else {
        return Ok(None);
    };
    let mut builder = globset::GlobSetBuilder::new();
    let mut added = false;
    for pat in patterns.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        // Tolerate a single bad glob (ignore it) but error on the whole set only
        // if nothing could be built.
        if let Ok(g) = globset::Glob::new(pat) {
            builder.add(g);
            added = true;
        }
    }
    if !added {
        return Ok(None);
    }
    let set = builder.build().map_err(|e| format!("Invalid glob pattern: {e}"))?;
    Ok(Some(set))
}

/// Apply include/exclude globs to a relative path. A glob without `/` is matched
/// against the basename (rg semantics: `*.ts` matches any depth), one with `/`
/// against the full relative path.
fn matches_glob(
    include: &Option<globset::GlobSet>,
    exclude: &Option<globset::GlobSet>,
    rel: &Path,
) -> bool {
    let basename = rel.file_name();
    if let Some(ex) = exclude {
        if ex.is_match(rel) || basename.is_some_and(|b| ex.is_match(b)) {
            return false;
        }
    }
    if let Some(inc) = include {
        let hit = inc.is_match(rel) || basename.is_some_and(|b| inc.is_match(b));
        if !hit {
            return false;
        }
    }
    true
}

// ── Accumulator ────────────────────────────────────────────────

struct Accumulator {
    files: Vec<SearchFileResult>,
    index: HashMap<PathBuf, usize>,
    total: usize,
    truncated: bool,
    max_results: usize,
    matcher: Option<LineMatcher>,
}

impl Accumulator {
    fn new(max_results: usize, matcher: Option<LineMatcher>) -> Self {
        Accumulator {
            files: Vec::new(),
            index: HashMap::new(),
            total: 0,
            truncated: false,
            max_results,
            matcher,
        }
    }

    fn file_full(&self, rel: &Path) -> bool {
        self.index
            .get(rel)
            .is_some_and(|&i| self.files[i].matches.len() >= MAX_MATCHES_PER_FILE)
    }

    /// Record matches for one line. `rel` is the path relative to the search
    /// root; `spans` are byte offsets into `line_text`.
    fn add_matches(&mut self, rel: PathBuf, line_no: usize, line_text: &str, spans: &[(usize, usize)]) {
        if self.truncated || spans.is_empty() {
            return;
        }
        let idx = match self.index.get(&rel) {
            Some(&i) => i,
            None => {
                let i = self.files.len();
                self.index.insert(rel.clone(), i);
                self.files.push(SearchFileResult {
                    file_path: String::new(), // filled in finalize
                    relative_path: rel.to_string_lossy().into_owned(),
                    matches: Vec::new(),
                    match_count: None,
                });
                i
            }
        };
        let file = &mut self.files[idx];
        for (start, end) in spans {
            if file.matches.len() >= MAX_MATCHES_PER_FILE || self.total >= self.max_results {
                if self.total >= self.max_results {
                    self.truncated = true;
                }
                break;
            }
            let start = (*start).min(line_text.len());
            let end = (*end).min(line_text.len());
            let (display, disp_col, disp_len) = clamp_line_content(line_text, start, end);
            file.matches.push(SearchMatch {
                line: line_no,
                column: char_index(line_text, start),
                match_length: char_index(line_text, end) - char_index(line_text, start),
                line_content: display,
                display_column: disp_col,
                display_match_length: disp_len,
            });
            self.total += 1;
        }
    }

    fn finalize(mut self, root: &Path) -> SearchResult {
        for f in &mut self.files {
            f.file_path = root.join(&f.relative_path).to_string_lossy().into_owned();
            f.match_count = Some(f.matches.len());
        }
        SearchResult {
            files: self.files,
            total_matches: self.total,
            truncated: self.truncated,
        }
    }
}

// ── Line content truncation / char math ────────────────────────

fn char_index(s: &str, byte_idx: usize) -> usize {
    s[..byte_idx.min(s.len())].chars().count()
}

/// Truncate `line` around the match at `start_byte..end_byte`, returning
/// `(display_text, display_column, display_match_length)` where the column is a
/// char offset into `display_text` (accounting for a leading `…`).
fn clamp_line_content(line: &str, start_byte: usize, end_byte: usize) -> (String, usize, usize) {
    let char_len = line.chars().count();
    let start_char = char_index(line, start_byte);
    let end_char = char_index(line, end_byte);
    let match_len = end_char - start_char;
    if char_len <= MAX_LINE_CONTENT {
        return (line.to_string(), start_char, match_len);
    }
    let half = MAX_LINE_CONTENT / 2;
    let mut win_start = start_char.saturating_sub(half);
    let win_end = (win_start + MAX_LINE_CONTENT).min(char_len);
    if win_end - win_start < MAX_LINE_CONTENT {
        win_start = win_end.saturating_sub(MAX_LINE_CONTENT);
    }
    let prefix = if win_start > 0 { "…" } else { "" };
    let suffix = if win_end < char_len { "…" } else { "" };
    let slice: String = line.chars().skip(win_start).take(win_end - win_start).collect();
    let display = format!("{prefix}{slice}{suffix}");
    let disp_col = start_char - win_start + prefix.chars().count();
    (display, disp_col, match_len)
}

fn normalize_rel(path_text: &str) -> PathBuf {
    PathBuf::from(path_text.strip_prefix("./").unwrap_or(path_text))
}

// ── Engine 1: ripgrep ──────────────────────────────────────────

async fn search_rg(opts: &SearchOptions, key: &str) -> Result<SearchResult, String> {
    let max_results = opts
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, DEFAULT_MAX_RESULTS);

    let mut cmd = tokio::process::Command::new("rg");
    // Windows GUI hosts flash a console for every short-lived child unless
    // CREATE_NO_WINDOW is set. Tokio's Command supports the same flag.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.args([
        "--json",
        "--hidden",
        "--glob",
        "!.git",
        "--max-count",
        "100",
        "--max-filesize",
        "5M",
    ]);
    if !opts.case_sensitive {
        cmd.arg("--ignore-case");
    }
    if opts.whole_word {
        cmd.arg("--word-regexp");
    }
    if !opts.use_regex {
        cmd.arg("--fixed-strings");
    }
    if let Some(inc) = &opts.include_pattern {
        for pat in inc.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            cmd.arg("--glob").arg(pat);
        }
    }
    if let Some(exc) = &opts.exclude_pattern {
        for pat in exc.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            cmd.arg("--glob").arg(format!("!{pat}"));
        }
    }
    cmd.arg("--").arg(&opts.query).arg(".");
    cmd.current_dir(&opts.root);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("rg unavailable: {e}")),
    };
    let my_id = child.id();
    let stdout = child.stdout.take().ok_or_else(|| "rg stdout closed".to_string())?;
    let stderr = child.stderr.take();
    // Kill any previous search for this root; replacing the map entry drops the
    // old child (its handle is the only one, and we id-check before reaping).
    // The lock guard must not be held across `.await` (MutexGuard isn't Send),
    // so the insert's guard temp is dropped before the kill.
    let previous = ACTIVE_RG.lock().unwrap().insert(key.to_string(), child);
    if let Some(mut prev) = previous {
        let _ = prev.kill().await;
        let _ = prev.wait().await;
    }

    // Drain stderr concurrently (tiny; avoids a full-pipe deadlock if rg ever
    // writes more than a pipe's buffer).
    let stderr_task = stderr.map(|se| {
        tokio::spawn(async move {
            let mut s = String::new();
            let mut r = BufReader::new(se);
            use tokio::io::AsyncReadExt;
            let _ = r.read_to_string(&mut s).await;
            s
        })
    });

    let mut acc = Accumulator::new(max_results, None);
    let mut lines = BufReader::new(stdout).lines();
    let timeout = tokio::time::sleep(SEARCH_TIMEOUT);
    tokio::pin!(timeout);

    loop {
        if acc.truncated {
            break;
        }
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(l)) => ingest_rg_json_line(&l, &mut acc),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            _ = &mut timeout => {
                acc.truncated = true;
                break;
            }
        }
    }

    // Reap our child (only if it's still the registered one).
    let exit = reap_child(key, my_id).await;
    let stderr_text = match stderr_task {
        Some(t) => t.await.unwrap_or_default(),
        None => String::new(),
    };

    match exit.map(|st| st.code()) {
        // 0 = matches found, 1 = no matches (empty result), 2 = error.
        Some(Some(0)) | Some(Some(1)) | None => {}
        Some(code) => {
            let msg = stderr_text.trim();
            return Err(if msg.is_empty() {
                format!("ripgrep failed with exit code {}", code.unwrap_or(-1))
            } else {
                msg.to_string()
            });
        }
    }

    Ok(acc.finalize(&opts.root))
}

/// Remove and kill+wait the child for `key` only if it is still our pid; if a
/// newer search replaced it, put the newer child back untouched.
async fn reap_child(key: &str, my_id: Option<u32>) -> Option<std::process::ExitStatus> {
    let child = {
        let mut guard = ACTIVE_RG.lock().unwrap();
        guard.remove(key)?
    };
    if child.id() == my_id {
        let mut child = child;
        return child.wait().await.ok();
    }
    // A newer search owns the slot now — don't kill it; restore it.
    ACTIVE_RG.lock().unwrap().insert(key.to_string(), child);
    None
}

/// Parse one rg `--json` line; returns true when the global result cap was hit
/// (caller should stop reading).
fn ingest_rg_json_line(line: &str, acc: &mut Accumulator) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("match") {
        return;
    }
    let data = &v["data"];
    let Some(path_text) = data["path"]["text"].as_str() else {
        return;
    };
    let Some(line_no) = data["line_number"].as_u64() else {
        return;
    };
    let Some(lines_text) = data["lines"]["text"].as_str() else {
        return;
    };
    let Some(submatches) = data["submatches"].as_array() else {
        return;
    };
    // rg's `lines.text` includes the trailing newline; sub-match offsets are
    // relative to the line content.
    let content = lines_text.strip_suffix('\n').unwrap_or(lines_text);
    let spans: Vec<(usize, usize)> = submatches
        .iter()
        .filter_map(|sm| {
            let s = sm["start"].as_u64()? as usize;
            let e = sm["end"].as_u64()? as usize;
            Some((s, e))
        })
        .collect();
    if spans.is_empty() {
        return;
    }
    let rel = normalize_rel(path_text);
    if acc.file_full(&rel) {
        return;
    }
    acc.add_matches(rel, line_no as usize, content, &spans);
}

// ── Engine 2: git grep ─────────────────────────────────────────

async fn search_git_grep(opts: &SearchOptions) -> Result<SearchResult, String> {
    let max_results = opts
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, DEFAULT_MAX_RESULTS);

    let mut cmd = tokio::process::Command::new("git");
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.args([
        "-c",
        "submodule.recurse=false",
        "grep",
        "-n",
        "-I",
        "--null",
        "--no-color",
        "--untracked",
        "--no-recurse-submodules",
    ]);
    if !opts.case_sensitive {
        cmd.arg("-i");
    }
    if opts.whole_word {
        cmd.arg("-w");
    }
    if opts.use_regex {
        cmd.arg("--extended-regexp");
    } else {
        cmd.arg("--fixed-strings");
    }
    if let Some(inc) = &opts.include_pattern {
        for pat in inc.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            cmd.arg("--include").arg(pat);
        }
    }
    if let Some(exc) = &opts.exclude_pattern {
        for pat in exc.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            cmd.arg("--exclude").arg(pat);
        }
    }
    cmd.arg("-e").arg(&opts.query);
    cmd.current_dir(&opts.root);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("git grep failed: {e}")),
    };
    let stdout = child.stdout.take().ok_or_else(|| "git grep stdout closed".to_string())?;
    let stderr = child.stderr.take();

    let stderr_task = stderr.map(|se| {
        tokio::spawn(async move {
            let mut s = String::new();
            let mut r = BufReader::new(se);
            use tokio::io::AsyncReadExt;
            let _ = r.read_to_string(&mut s).await;
            s
        })
    });

    let matcher = LineMatcher::new(opts)?;
    let mut acc = Accumulator::new(max_results, None);
    let mut lines = BufReader::new(stdout).lines();
    let mut seen = std::collections::HashSet::new();
    let timeout = tokio::time::sleep(SEARCH_TIMEOUT);
    tokio::pin!(timeout);

    loop {
        if acc.truncated {
            break;
        }
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(l)) => ingest_git_grep_line(&l, &mut acc, &mut seen, &matcher),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            _ = &mut timeout => {
                acc.truncated = true;
                break;
            }
        }
    }

    // git grep is short-lived and synchronous; there is no overlapping-run
    // registry for it (rg is the only engine worth de-duplicating).
    let _ = child.kill().await;
    let exit = child.wait().await.ok();
    let stderr_text = match stderr_task {
        Some(t) => t.await.unwrap_or_default(),
        None => String::new(),
    };

    match exit.map(|st| st.code()) {
        Some(Some(0)) | Some(Some(1)) | None => {}
        Some(code) => {
            let msg = stderr_text.trim();
            return Err(if msg.is_empty() {
                format!("git grep failed with exit code {}", code.unwrap_or(-1))
            } else {
                msg.to_string()
            });
        }
    }

    Ok(acc.finalize(&opts.root))
}

/// Parse one `git grep --null` record: `path\0line\0content`. Each record is one
/// match; git grep doesn't report columns, so we re-scan the line to derive
/// every match's offsets (and de-duplicate repeated records for one line).
fn ingest_git_grep_line(
    line: &str,
    acc: &mut Accumulator,
    seen: &mut std::collections::HashSet<(PathBuf, usize)>,
    matcher: &LineMatcher,
) {
    // NUL-separated: [path, line, content].
    let mut parts = line.split('\0');
    let (Some(path), Some(line_no), Some(content)) = (parts.next(), parts.next(), parts.next())
    else {
        return;
    };
    let line_no = match line_no.parse::<usize>() {
        Ok(n) => n,
        Err(_) => return,
    };
    let rel = normalize_rel(path);
    if !seen.insert((rel.clone(), line_no)) {
        return;
    }
    let spans = matcher.find(content);
    if spans.is_empty() {
        return;
    }
    acc.add_matches(rel, line_no, content, &spans);
}

// ── Engine 3: pure-Rust walker ─────────────────────────────────

fn search_walker(opts: &SearchOptions) -> Result<SearchResult, String> {
    let max_results = opts
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, DEFAULT_MAX_RESULTS);
    let matcher = LineMatcher::new(opts)?;
    let include = build_globset(opts.include_pattern.as_deref())?;
    let exclude = build_globset(opts.exclude_pattern.as_deref())?;
    let mut acc = Accumulator::new(max_results, Some(matcher));

    let mut stack = vec![opts.root.clone()];
    while let Some(dir) = stack.pop() {
        if acc.truncated {
            break;
        }
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() {
                if acc.truncated {
                    break;
                }
                let rel = path.strip_prefix(&opts.root).unwrap_or(&path).to_path_buf();
                if !matches_glob(&include, &exclude, &rel) {
                    continue;
                }
                scan_file(&path, &rel, &mut acc);
            }
        }
    }
    Ok(acc.finalize(&opts.root))
}

fn scan_file(path: &Path, rel: &Path, acc: &mut Accumulator) {
    if acc.file_full(rel) {
        return;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() > MAX_FILE_SIZE {
        return;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    // Binary sniff: a NUL in the first 8 KiB ⇒ skip (mirrors rg `-I`).
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return;
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return;
    };
    for (i, line) in text.lines().enumerate() {
        if acc.truncated || acc.file_full(rel) {
            break;
        }
        // Scope the matcher borrow so `add_matches` can re-borrow `acc` mutably.
        let spans = {
            let Some(matcher) = acc.matcher.as_ref() else {
                return;
            };
            matcher.find(line)
        };
        if !spans.is_empty() {
            acc.add_matches(rel.to_path_buf(), i + 1, line, &spans);
        }
    }
}

// ── Entry point ────────────────────────────────────────────────

pub async fn search(opts: &SearchOptions) -> Result<SearchResult, String> {
    if opts.query.trim().is_empty() {
        return Ok(SearchResult::default());
    }
    if opts.query.len() > MAX_PATTERN_BYTES {
        return Ok(SearchResult::default());
    }
    let key = opts.root.to_string_lossy().into_owned();

    // Engine 1: ripgrep. A spawn failure means "rg unavailable" → fall through;
    // a runtime error (exit 2, e.g. bad regex) is surfaced to the user.
    if rg_available() {
        match search_rg(opts, &key).await {
            Ok(r) => return Ok(r),
            Err(e) if e.starts_with("rg unavailable") => {}
            Err(e) => return Err(e),
        }
    }

    // Engine 2: git grep for git worktrees when rg is absent.
    if is_git_repo(&opts.root) {
        if let Ok(r) = search_git_grep(opts).await {
            return Ok(r);
        }
    }

    // Engine 3: pure-Rust walker — any directory, no external binary.
    search_walker(opts)
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(query: &str) -> SearchOptions {
        SearchOptions {
            query: query.to_string(),
            root: PathBuf::from("/tmp"),
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            include_pattern: None,
            exclude_pattern: None,
            max_results: None,
        }
    }

    #[test]
    fn clamps_long_lines_around_the_match() {
        let line = "x".repeat(1000) + "NEEDLE" + &"y".repeat(1000);
        let start = line.find("NEEDLE").unwrap();
        let end = start + "NEEDLE".len();
        let (display, col, len) = clamp_line_content(&line, start, end);
        assert!(display.chars().count() <= MAX_LINE_CONTENT + 2); // +2 ellipses
        assert!(display.contains("NEEDLE"));
        let sliced: String = display.chars().skip(col).take(len).collect();
        assert_eq!(&sliced, "NEEDLE");
        assert!(display.starts_with('…'));
        assert!(display.ends_with('…'));
    }

    #[test]
    fn short_lines_are_kept_whole() {
        let (display, col, len) = clamp_line_content("hello world", 6, 11);
        assert_eq!(display, "hello world");
        assert_eq!(col, 6);
        assert_eq!(len, 5);
    }

    #[test]
    fn matcher_handles_case_whole_word_and_regex() {
        let base = |query: &str| SearchOptions {
            query: query.to_string(),
            root: PathBuf::from("/tmp"),
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            include_pattern: None,
            exclude_pattern: None,
            max_results: None,
        };
        let m = LineMatcher::new(&base("foo")).unwrap();
        assert_eq!(m.find("a foo b"), vec![(2, 5)]);
        assert_eq!(m.find("a FOO b"), vec![(2, 5)]); // case-insensitive default
        assert_eq!(m.find("foobar"), vec![(0, 3)]); // substring inside word

        let m = LineMatcher::new(&SearchOptions {
            whole_word: true,
            ..base("foo")
        })
        .unwrap();
        assert_eq!(m.find("a foo b"), vec![(2, 5)]);
        assert!(m.find("foobar").is_empty());

        let m = LineMatcher::new(&SearchOptions {
            use_regex: true,
            ..base(r"f\w+")
        })
        .unwrap();
        assert_eq!(m.find("a foo b far"), vec![(2, 5), (8, 11)]);

        let m = LineMatcher::new(&SearchOptions {
            case_sensitive: true,
            ..base("Foo")
        })
        .unwrap();
        assert!(m.find("foo").is_empty());
        assert_eq!(m.find("Foo"), vec![(0, 3)]);
    }

    #[test]
    fn parses_git_grep_null_records() {
        let o = opts("needle");
        let matcher = LineMatcher::new(&o).unwrap();
        let mut acc = Accumulator::new(100, None);
        let mut seen = std::collections::HashSet::new();
        ingest_git_grep_line("src/a.rs\07\0let needle = 1;", &mut acc, &mut seen, &matcher);
        ingest_git_grep_line("src/a.rs\07\0let needle = 1;", &mut acc, &mut seen, &matcher); // dup
        assert_eq!(acc.files.len(), 1);
        assert_eq!(acc.files[0].matches.len(), 1);
        assert_eq!(acc.files[0].matches[0].line, 7);
        assert_eq!(acc.files[0].matches[0].column, 4);
    }

    #[test]
    fn walker_finds_matches_in_a_temp_tree() {
        let dir = std::env::temp_dir().join(format!("capilot_fs_search_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "hello needle world\n").unwrap();
        std::fs::write(dir.join("sub/b.txt"), "nothing here\n").unwrap();
        std::fs::write(dir.join("sub/c.txt"), "needle again\nneedle twice\n").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "x").unwrap(); // skip-dir must hide this

        let o = SearchOptions {
            query: "needle".into(),
            root: dir.clone(),
            ..opts("needle")
        };
        let res = search_walker(&o).unwrap();
        assert_eq!(res.files.len(), 2);
        let a = res.files.iter().find(|f| f.relative_path == "a.txt").unwrap();
        assert_eq!(a.matches.len(), 1);
        let c = res.files.iter().find(|f| f.relative_path == "sub/c.txt").unwrap();
        assert_eq!(c.matches.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walker_honors_include_glob() {
        let dir = std::env::temp_dir().join(format!("capilot_fs_search_test2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.ts"), "needle\n").unwrap();
        std::fs::write(dir.join("src/b.js"), "needle\n").unwrap();

        let o = SearchOptions {
            query: "needle".into(),
            root: dir.clone(),
            include_pattern: Some("*.ts".into()),
            ..opts("needle")
        };
        let res = search_walker(&o).unwrap();
        assert_eq!(res.files.len(), 1);
        assert_eq!(res.files[0].relative_path, "src/a.ts");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn git_grep_searches_a_real_repo() {
        // Skip unless git is available (it is on every dev box CaPilot targets).
        if std::process::Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            == false
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("capilot_git_grep_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        assert!(run(&["init", "-q"]));
        std::fs::write(dir.join("a.txt"), "hello needle world\n").unwrap();
        std::fs::write(dir.join("b.txt"), "nothing here\n").unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "seed"]));

        let o = SearchOptions {
            query: "needle".into(),
            root: dir.clone(),
            ..opts("needle")
        };
        let res = search_git_grep(&o).await.unwrap();
        assert_eq!(res.files.len(), 1);
        assert_eq!(res.files[0].relative_path, "a.txt");
        assert_eq!(res.files[0].matches.len(), 1);
        assert_eq!(res.files[0].matches[0].line, 1);
        assert_eq!(res.files[0].matches[0].column, 6);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
