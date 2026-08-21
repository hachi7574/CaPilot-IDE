//! BlockGraph JSON persistence for the workspace canvas.
//!
//! Path: `<data_root>/workspaces/<project>/canvas/<workspaceHash>/graph.json`
//!
//! - `terminals[].agentId` is a reference into `sessions.db`, not ownership.
//! - Shell sessions project as terminals; coding-agent sessions as `agents[]`.
//! - `agents[].id` must never appear as an edge endpoint.
//! - Missing file → empty graph, no write. User drag is what persists.

use crate::persistence::{custom_project_root, path_is_within, project_dir};
use crate::sanitize_project;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasVec {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasSize {
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasViewport {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasTerminal {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub command: String,
    pub kind: String,
    pub agent_id: Option<String>,
    pub position: CanvasVec,
    pub size: CanvasSize,
    #[serde(default)]
    pub port_policy: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub ready_pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasAgentLayout {
    pub id: String,
    pub position: CanvasVec,
    pub size: CanvasSize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasEdge {
    pub id: String,
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanvasCombination {
    pub id: String,
    pub member_terminal_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BlockGraph {
    pub version: u32,
    pub project_id: String,
    pub workspace_id: String,
    pub viewport: CanvasViewport,
    pub terminals: Vec<CanvasTerminal>,
    pub edges: Vec<CanvasEdge>,
    pub combinations: Vec<CanvasCombination>,
    pub agents: Vec<CanvasAgentLayout>,
    #[serde(default)]
    pub agents_hidden: Vec<String>,
}

impl BlockGraph {
    pub fn empty(project: &str, workspace_id: &str) -> Self {
        Self {
            version: 1,
            project_id: project.to_string(),
            workspace_id: workspace_id.to_string(),
            viewport: CanvasViewport {
                x: 0.0,
                y: 0.0,
                zoom: 1.0,
            },
            terminals: Vec::new(),
            edges: Vec::new(),
            combinations: Vec::new(),
            agents: Vec::new(),
            agents_hidden: Vec::new(),
        }
    }
}

/// FNV-1a 64-bit — stable across rustc versions, filesystem-safe hex.
fn workspace_hash(workspace_id: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in workspace_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn has_parent_dir(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

fn resolve_existing_or_parent(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("workspace id cannot be empty".to_string());
    }
    if path.exists() {
        return path
            .canonicalize()
            .map_err(|e| format!("Invalid workspace path: {e}"));
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let Some(parent) = parent else {
        return Err("workspace path does not exist".to_string());
    };
    let parent = if parent.exists() {
        parent
            .canonicalize()
            .map_err(|e| format!("Invalid workspace path: {e}"))?
    } else {
        parent.to_path_buf()
    };
    let name = path.file_name().ok_or_else(|| "Invalid workspace path".to_string())?;
    Ok(parent.join(name))
}

fn allowed_workspace_roots(project: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let dir = project_dir(project);
    roots.push(dir.canonicalize().unwrap_or(dir));
    if let Some(custom) = custom_project_root(project) {
        roots.push(custom.canonicalize().unwrap_or(custom));
    }
    #[cfg(test)]
    {
        roots.push(std::env::temp_dir());
    }
    roots
}

pub fn validate_workspace(project: &str, workspace_id: &str) -> Result<PathBuf, String> {
    if workspace_id.is_empty() {
        return Err("workspace id cannot be empty".to_string());
    }
    let path = Path::new(workspace_id);
    if has_parent_dir(path) {
        return Err("Invalid workspace path".to_string());
    }
    let resolved = resolve_existing_or_parent(path)?;
    let allowed = allowed_workspace_roots(project);
    if !allowed.iter().any(|root| path_is_within(&resolved, root)) {
        return Err("workspace path is outside the project".to_string());
    }
    Ok(resolved)
}

pub fn graph_path(project: &str, workspace_id: &str) -> Result<PathBuf, String> {
    sanitize_project(project)?;
    validate_workspace(project, workspace_id)?;
    Ok(project_dir(project)
        .join("canvas")
        .join(workspace_hash(workspace_id))
        .join("graph.json"))
}

pub fn validate_graph(graph: &BlockGraph) -> Result<(), String> {
    if graph.version != 1 {
        return Err(format!("unsupported canvas graph version {}", graph.version));
    }
    let term_ids: HashSet<&str> = graph.terminals.iter().map(|t| t.id.as_str()).collect();
    let agent_ids: HashSet<&str> = graph.agents.iter().map(|a| a.id.as_str()).collect();
    for edge in &graph.edges {
        if edge.source == edge.target {
            return Err("canvas edge cannot connect a terminal to itself".to_string());
        }
        if agent_ids.contains(edge.source.as_str()) || agent_ids.contains(edge.target.as_str()) {
            return Err("agent consoles cannot be edge endpoints".to_string());
        }
        if !term_ids.contains(edge.source.as_str()) || !term_ids.contains(edge.target.as_str()) {
            return Err("canvas edge must connect existing terminals".to_string());
        }
    }
    let mut owned: HashSet<&str> = HashSet::new();
    for comb in &graph.combinations {
        for id in &comb.member_terminal_ids {
            if agent_ids.contains(id.as_str()) {
                return Err("agent consoles cannot join combinations".to_string());
            }
            if !term_ids.contains(id.as_str()) {
                return Err("combination member must be a terminal".to_string());
            }
            if !owned.insert(id.as_str()) {
                return Err("a terminal cannot belong to two combinations".to_string());
            }
        }
    }
    if let Err(cycle) = topo_order(graph, &graph.terminals.iter().map(|t| t.id.clone()).collect::<Vec<_>>()) {
        return Err(cycle);
    }
    Ok(())
}

/// Undirected reachable terminals from `terminal_id` (a full workflow).
pub fn expand_workflow(graph: &BlockGraph, terminal_id: &str) -> Result<Vec<String>, String> {
    if graph.agents.iter().any(|a| a.id == terminal_id) {
        return Err("agent consoles are not workflow members".to_string());
    }
    if !graph.terminals.iter().any(|t| t.id == terminal_id) {
        return Err(format!("unknown terminal {terminal_id}"));
    }
    let mut adj: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for t in &graph.terminals {
        adj.entry(t.id.as_str()).or_default();
    }
    for e in &graph.edges {
        adj.entry(e.source.as_str()).or_default().push(e.target.as_str());
        adj.entry(e.target.as_str()).or_default().push(e.source.as_str());
    }
    let mut seen = HashSet::new();
    let mut stack = vec![terminal_id];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(n) = adj.get(id) {
            stack.extend(n.iter().copied());
        }
    }
    Ok(graph
        .terminals
        .iter()
        .filter(|t| seen.contains(t.id.as_str()))
        .map(|t| t.id.clone())
        .collect())
}

/// Kahn topological order over `ids` using directed edges. Cycle → Err.
pub fn topo_order(graph: &BlockGraph, ids: &[String]) -> Result<Vec<String>, String> {
    let set: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let mut indeg: std::collections::HashMap<&str, usize> =
        ids.iter().map(|s| (s.as_str(), 0usize)).collect();
    let mut down: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for e in &graph.edges {
        if set.contains(e.source.as_str()) && set.contains(e.target.as_str()) {
            *indeg.entry(e.target.as_str()).or_default() += 1;
            down.entry(e.source.as_str())
                .or_default()
                .push(e.target.as_str());
        }
    }
    let mut ready: Vec<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();
    ready.sort_unstable();
    let mut out = Vec::new();
    while let Some(id) = ready.pop() {
        out.push(id.to_string());
        if let Some(children) = down.get(id) {
            for child in children {
                if let Some(d) = indeg.get_mut(child) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        ready.push(child);
                        ready.sort_unstable();
                    }
                }
            }
        }
    }
    if out.len() != ids.len() {
        return Err("cycle involving canvas terminals".to_string());
    }
    Ok(out)
}

fn new_edge_id(graph: &BlockGraph) -> String {
    format!("edge_{}", graph.edges.len() + 1)
}

#[tauri::command]
pub fn canvas_graph_connect(
    project: String,
    workspace_id: String,
    source: String,
    target: String,
) -> Result<BlockGraph, String> {
    let path = graph_path(&project, &workspace_id)?;
    let mut graph = read_graph_at(&path, &project, &workspace_id)?;
    graph.edges.push(CanvasEdge {
        id: new_edge_id(&graph),
        source,
        target,
    });
    write_graph_at(&path, &graph)?;
    Ok(graph)
}

fn read_graph_at(path: &Path, project: &str, workspace_id: &str) -> Result<BlockGraph, String> {
    if !path.exists() {
        return Ok(BlockGraph::empty(project, workspace_id));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("Failed to read canvas graph: {e}"))?;
    let graph: BlockGraph =
        serde_json::from_str(&raw).map_err(|e| format!("Invalid canvas graph: {e}"))?;
    if graph.version != 1 {
        return Err(format!("unsupported canvas graph version {}", graph.version));
    }
    Ok(graph)
}

fn write_graph_at(path: &Path, graph: &BlockGraph) -> Result<(), String> {
    validate_graph(graph)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create canvas graph directory: {e}"))?;
    }
    let bytes =
        serde_json::to_vec_pretty(graph).map_err(|e| format!("Failed to serialize canvas graph: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("Failed to write canvas graph: {e}"))?;
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Failed to commit canvas graph: {e}")
    })?;
    Ok(())
}

#[tauri::command]
pub fn canvas_graph_get(project: String, workspace_id: String) -> Result<BlockGraph, String> {
    let path = graph_path(&project, &workspace_id)?;
    read_graph_at(&path, &project, &workspace_id)
}

#[tauri::command]
pub fn canvas_graph_set(
    project: String,
    workspace_id: String,
    graph: BlockGraph,
) -> Result<(), String> {
    let path = graph_path(&project, &workspace_id)?;
    write_graph_at(&path, &graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn unique_home() -> PathBuf {
        std::env::temp_dir().join(format!(
            "capilot-canvas-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CAPILOT_HOME").ok();
        std::env::set_var("CAPILOT_HOME", home);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("CAPILOT_HOME", v),
            None => std::env::remove_var("CAPILOT_HOME"),
        }
        out
    }

    fn sample_graph(project: &str, workspace: &str) -> BlockGraph {
        let mut g = BlockGraph::empty(project, workspace);
        g.terminals.push(CanvasTerminal {
            id: "term_a".into(),
            name: "dev".into(),
            cwd: workspace.into(),
            command: String::new(),
            kind: "task".into(),
            agent_id: Some("agent-1".into()),
            position: CanvasVec { x: 80.0, y: 80.0 },
            size: CanvasSize { w: 240.0, h: 88.0 },
            port_policy: None,
            port: None,
            ready_pattern: None,
        });
        g.agents.push(CanvasAgentLayout {
            id: "claude-1".into(),
            position: CanvasVec { x: 400.0, y: 80.0 },
            size: CanvasSize { w: 240.0, h: 88.0 },
        });
        g
    }

    #[test]
    fn get_missing_file_returns_empty_without_creating() {
        let home = unique_home();
        std::fs::create_dir_all(&home).unwrap();
        let ws = home.join("proj-root");
        std::fs::create_dir_all(&ws).unwrap();
        let ws_str = ws.to_string_lossy().into_owned();
        let result = with_home(&home, || canvas_graph_get("p1".into(), ws_str.clone()));
        let graph = result.unwrap();
        assert_eq!(graph.version, 1);
        assert!(graph.terminals.is_empty());
        let path = with_home(&home, || graph_path("p1", &ws_str)).unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn set_then_get_roundtrips_position() {
        let home = unique_home();
        std::fs::create_dir_all(&home).unwrap();
        let ws = home.join("proj-root");
        std::fs::create_dir_all(&ws).unwrap();
        let ws_str = ws.to_string_lossy().into_owned();
        let mut graph = sample_graph("p1", &ws_str);
        graph.terminals[0].position = CanvasVec { x: 120.0, y: 40.0 };
        with_home(&home, || {
            canvas_graph_set("p1".into(), ws_str.clone(), graph.clone())
        })
        .unwrap();
        let loaded = with_home(&home, || canvas_graph_get("p1".into(), ws_str.clone())).unwrap();
        assert_eq!(loaded.terminals[0].position, CanvasVec { x: 120.0, y: 40.0 });
        assert_eq!(loaded.agents[0].id, "claude-1");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rejects_parent_dir_project() {
        let err = graph_path("../escape", "/tmp/x").unwrap_err();
        assert!(err.to_lowercase().contains("invalid") || err.contains("Project"));
    }

    #[test]
    fn rejects_parent_dir_workspace() {
        let home = unique_home();
        std::fs::create_dir_all(&home).unwrap();
        let err = with_home(&home, || graph_path("p1", "/tmp/foo/../escape"));
        assert!(err.is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn self_loop_leaves_file_unchanged() {
        let home = unique_home();
        std::fs::create_dir_all(&home).unwrap();
        let ws = home.join("proj-root");
        std::fs::create_dir_all(&ws).unwrap();
        let ws_str = ws.to_string_lossy().into_owned();
        let good = sample_graph("p1", &ws_str);
        with_home(&home, || {
            canvas_graph_set("p1".into(), ws_str.clone(), good.clone())
        })
        .unwrap();
        let mut bad = good.clone();
        bad.edges.push(CanvasEdge {
            id: "e1".into(),
            source: "term_a".into(),
            target: "term_a".into(),
        });
        let err = with_home(&home, || {
            canvas_graph_set("p1".into(), ws_str.clone(), bad)
        });
        assert!(err.is_err());
        let loaded = with_home(&home, || canvas_graph_get("p1".into(), ws_str.clone())).unwrap();
        assert!(loaded.edges.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn agent_as_edge_endpoint_rejected() {
        let home = unique_home();
        std::fs::create_dir_all(&home).unwrap();
        let ws = home.join("proj-root");
        std::fs::create_dir_all(&ws).unwrap();
        let ws_str = ws.to_string_lossy().into_owned();
        let good = sample_graph("p1", &ws_str);
        with_home(&home, || {
            canvas_graph_set("p1".into(), ws_str.clone(), good.clone())
        })
        .unwrap();
        let mut bad = good.clone();
        bad.edges.push(CanvasEdge {
            id: "e1".into(),
            source: "claude-1".into(),
            target: "term_a".into(),
        });
        let err = with_home(&home, || {
            canvas_graph_set("p1".into(), ws_str.clone(), bad)
        });
        assert!(err.unwrap_err().contains("agent"));
        let loaded = with_home(&home, || canvas_graph_get("p1".into(), ws_str.clone())).unwrap();
        assert!(loaded.edges.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn version_2_on_disk_is_error() {
        let home = unique_home();
        std::fs::create_dir_all(&home).unwrap();
        let ws = home.join("proj-root");
        std::fs::create_dir_all(&ws).unwrap();
        let ws_str = ws.to_string_lossy().into_owned();
        let path = with_home(&home, || graph_path("p1", &ws_str)).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version":2,"projectId":"p1","workspaceId":"x","viewport":{"x":0,"y":0,"zoom":1},"terminals":[],"edges":[],"combinations":[],"agents":[]}"#,
        )
        .unwrap();
        let err = with_home(&home, || canvas_graph_get("p1".into(), ws_str.clone()));
        assert!(err.unwrap_err().contains("version"));
        let _ = std::fs::remove_dir_all(&home);
    }

    fn chain_graph() -> BlockGraph {
        let mut g = BlockGraph::empty("p", "/tmp/x");
        for (id, x) in [("term_a", 0.0), ("term_b", 1.0), ("term_c", 2.0), ("term_d", 3.0)] {
            g.terminals.push(CanvasTerminal {
                id: id.into(),
                name: id.into(),
                cwd: "/tmp/x".into(),
                command: String::new(),
                kind: "task".into(),
                agent_id: None,
                position: CanvasVec { x, y: 0.0 },
                size: CanvasSize { w: 240.0, h: 88.0 },
                port_policy: None,
                port: None,
                ready_pattern: None,
            });
        }
        g.edges.push(CanvasEdge { id: "e1".into(), source: "term_a".into(), target: "term_b".into() });
        g.edges.push(CanvasEdge { id: "e2".into(), source: "term_b".into(), target: "term_c".into() });
        g
    }

    #[test]
    fn expand_from_middle_covers_component() {
        let g = chain_graph();
        let mut ids = expand_workflow(&g, "term_b").unwrap();
        ids.sort();
        assert_eq!(ids, vec!["term_a", "term_b", "term_c"]);
    }

    #[test]
    fn expand_isolated_is_singleton() {
        let g = chain_graph();
        assert_eq!(expand_workflow(&g, "term_d").unwrap(), vec!["term_d"]);
    }

    #[test]
    fn expand_rejects_agent_id() {
        let mut g = chain_graph();
        g.agents.push(CanvasAgentLayout {
            id: "claude-1".into(),
            position: CanvasVec { x: 0.0, y: 0.0 },
            size: CanvasSize { w: 1.0, h: 1.0 },
        });
        assert!(expand_workflow(&g, "claude-1").is_err());
    }

    #[test]
    fn cycle_is_rejected() {
        let mut g = chain_graph();
        g.edges.push(CanvasEdge { id: "e3".into(), source: "term_c".into(), target: "term_a".into() });
        let err = validate_graph(&g).unwrap_err();
        assert!(err.contains("cycle"));
    }

    #[test]
    fn topo_orders_dependencies() {
        let g = chain_graph();
        let ids = vec!["term_a".into(), "term_b".into(), "term_c".into()];
        let order = topo_order(&g, &ids).unwrap();
        let a = order.iter().position(|x| x == "term_a").unwrap();
        let b = order.iter().position(|x| x == "term_b").unwrap();
        let c = order.iter().position(|x| x == "term_c").unwrap();
        assert!(a < b && b < c);
    }

    #[test]
    fn combination_cannot_share_terminal() {
        let mut g = chain_graph();
        g.combinations.push(CanvasCombination {
            id: "c1".into(),
            member_terminal_ids: vec!["term_a".into()],
        });
        g.combinations.push(CanvasCombination {
            id: "c2".into(),
            member_terminal_ids: vec!["term_a".into()],
        });
        assert!(validate_graph(&g).unwrap_err().contains("two combinations"));
    }
}
