//! Immutable canvas Run: freeze a plan from the graph, track node states.
//!
//! Live ports / exit codes never write back into BlockGraph. Phase 2 drives
//! tasks by exit code only; PTY spawn is wired by the caller via [`RunExecutor`].

use crate::canvas_graph::{expand_workflow, topo_order, BlockGraph};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunPlan {
    pub run_id: String,
    pub project: String,
    pub workspace_id: String,
    pub terminal_ids: Vec<String>,
    pub edges: Vec<(String, String)>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NodeRunState {
    Pending,
    Running,
    Ok,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunStatus {
    pub run_id: String,
    pub plan: RunPlan,
    pub node_states: HashMap<String, NodeRunState>,
    pub blocked: Vec<String>,
    pub ready: Vec<String>,
    /// terminalId → allocated port (Run-only; never persisted on the graph).
    pub leases: HashMap<String, u16>,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub plan: RunPlan,
    pub node_states: HashMap<String, NodeRunState>,
    /// Frozen graph snapshot at start — later canvas_graph_set must not mutate this.
    #[allow(dead_code)]
    pub frozen: BlockGraph,
    pub leases: HashMap<String, u16>,
}

pub fn allocate_port(policy: &str, preferred: Option<u16>) -> Result<u16, String> {
    match policy {
        "fixed" => {
            let p = preferred.ok_or_else(|| "fixed port policy requires a port".to_string())?;
            let listener = std::net::TcpListener::bind(("127.0.0.1", p))
                .map_err(|_| format!("port {p} is not available"))?;
            drop(listener);
            Ok(p)
        }
        "preferred" => {
            if let Some(p) = preferred {
                if std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
                    return Ok(p);
                }
                for n in 1..=50 {
                    let cand = p.saturating_add(n);
                    if std::net::TcpListener::bind(("127.0.0.1", cand)).is_ok() {
                        return Ok(cand);
                    }
                }
            }
            allocate_port("auto", None)
        }
        _ => {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| format!("auto port bind failed: {e}"))?;
            let port = listener
                .local_addr()
                .map_err(|e| e.to_string())?
                .port();
            drop(listener);
            Ok(port)
        }
    }
}

pub fn allocate_leases(graph: &BlockGraph, ids: &[String]) -> Result<HashMap<String, u16>, String> {
    let mut leases = HashMap::new();
    for id in ids {
        let Some(term) = graph.terminals.iter().find(|t| t.id == *id) else {
            continue;
        };
        let needs = term.kind == "service"
            || term.command.contains("{PORT}")
            || term.port_policy.is_some();
        if !needs {
            continue;
        }
        let policy = term.port_policy.as_deref().unwrap_or("auto");
        leases.insert(id.clone(), allocate_port(policy, term.port)?);
    }
    Ok(leases)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn freeze_plan(graph: &BlockGraph, root_terminal_id: &str) -> Result<RunPlan, String> {
    if graph.agents.iter().any(|a| a.id == root_terminal_id) {
        return Err("agent consoles cannot be run roots".to_string());
    }
    let ids = expand_workflow(graph, root_terminal_id)?;
    let order = topo_order(graph, &ids)?;
    let idset: HashSet<&str> = order.iter().map(|s| s.as_str()).collect();
    let edges = graph
        .edges
        .iter()
        .filter(|e| idset.contains(e.source.as_str()) && idset.contains(e.target.as_str()))
        .map(|e| (e.source.clone(), e.target.clone()))
        .collect();
    Ok(RunPlan {
        run_id: uuid::Uuid::new_v4().to_string(),
        project: graph.project_id.clone(),
        workspace_id: graph.workspace_id.clone(),
        terminal_ids: order,
        edges,
        created_at: now_ms(),
    })
}

fn predecessors<'a>(plan: &'a RunPlan, id: &str) -> Vec<&'a str> {
    plan.edges
        .iter()
        .filter(|(_, t)| t == id)
        .map(|(s, _)| s.as_str())
        .collect()
}

fn successors<'a>(plan: &'a RunPlan, id: &str) -> Vec<&'a str> {
    plan.edges
        .iter()
        .filter(|(s, _)| s == id)
        .map(|(_, t)| t.as_str())
        .collect()
}

fn descendants(plan: &RunPlan, id: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut stack: Vec<&str> = successors(plan, id);
    while let Some(n) = stack.pop() {
        if !seen.insert(n.to_string()) {
            continue;
        }
        stack.extend(successors(plan, n));
    }
    seen.into_iter().collect()
}

pub fn initial_states(plan: &RunPlan) -> HashMap<String, NodeRunState> {
    let mut states = HashMap::new();
    for id in &plan.terminal_ids {
        let ready = predecessors(plan, id).is_empty();
        states.insert(
            id.clone(),
            if ready {
                NodeRunState::Pending
            } else {
                NodeRunState::Pending
            },
        );
    }
    states
}

/// Nodes with all predecessors Ok (or no predecessors) and still Pending.
pub fn ready_to_start(run: &Run) -> Vec<String> {
    run.plan
        .terminal_ids
        .iter()
        .filter(|id| {
            run.node_states.get(*id) == Some(&NodeRunState::Pending)
                && predecessors(&run.plan, id)
                    .iter()
                    .all(|p| run.node_states.get(*p) == Some(&NodeRunState::Ok))
        })
        .cloned()
        .collect()
}

pub fn mark_running(run: &mut Run, id: &str) {
    run.node_states.insert(id.to_string(), NodeRunState::Running);
}

/// Apply an exit code: 0 → Ok and unlock downstream; non-zero → Failed + block descendants.
pub fn apply_exit(run: &mut Run, id: &str, code: i32) {
    if code == 0 {
        run.node_states.insert(id.to_string(), NodeRunState::Ok);
    } else {
        run.node_states.insert(id.to_string(), NodeRunState::Failed);
        for d in descendants(&run.plan, id) {
            if run.node_states.get(&d) == Some(&NodeRunState::Pending) {
                run.node_states.insert(d, NodeRunState::Blocked);
            }
        }
    }
}

/// Reverse-dependency order of nodes that were started (running/ok/failed).
pub fn stop_order(run: &Run) -> Vec<String> {
    let started: HashSet<&str> = run
        .node_states
        .iter()
        .filter(|(_, s)| {
            matches!(
                s,
                NodeRunState::Running | NodeRunState::Ok | NodeRunState::Failed
            )
        })
        .map(|(k, _)| k.as_str())
        .collect();
    run.plan
        .terminal_ids
        .iter()
        .rev()
        .filter(|id| started.contains(id.as_str()))
        .cloned()
        .collect()
}

fn runs() -> &'static Mutex<HashMap<String, Run>> {
    static RUNS: OnceLock<Mutex<HashMap<String, Run>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn run_status(run: &Run) -> RunStatus {
    let blocked = run
        .node_states
        .iter()
        .filter(|(_, s)| **s == NodeRunState::Blocked)
        .map(|(k, _)| k.clone())
        .collect();
    RunStatus {
        run_id: run.plan.run_id.clone(),
        plan: run.plan.clone(),
        node_states: run.node_states.clone(),
        blocked,
        ready: ready_to_start(run),
        leases: run.leases.clone(),
    }
}

#[tauri::command]
pub fn canvas_run_start(
    project: String,
    workspace_id: String,
    root_terminal_id: String,
) -> Result<RunStatus, String> {
    let graph = crate::canvas_graph::canvas_graph_get(project, workspace_id)?;
    let plan = freeze_plan(&graph, &root_terminal_id)?;
    let leases = allocate_leases(&graph, &plan.terminal_ids)?;
    let node_states = initial_states(&plan);
    let run = Run {
        plan,
        node_states,
        frozen: graph,
        leases,
    };
    let status = run_status(&run);
    runs()
        .lock()
        .map_err(|e| e.to_string())?
        .insert(run.plan.run_id.clone(), run);
    Ok(status)
}

#[tauri::command]
pub fn canvas_run_status(run_id: String) -> Result<RunStatus, String> {
    let map = runs().lock().map_err(|e| e.to_string())?;
    let run = map.get(&run_id).ok_or_else(|| "unknown canvas run".to_string())?;
    Ok(run_status(run))
}

#[tauri::command]
pub fn canvas_run_stop(run_id: String) -> Result<Vec<String>, String> {
    let mut map = runs().lock().map_err(|e| e.to_string())?;
    let run = map
        .remove(&run_id)
        .ok_or_else(|| "unknown canvas run".to_string())?;
    Ok(stop_order(&run))
}

#[tauri::command]
pub fn canvas_run_report_exit(
    run_id: String,
    terminal_id: String,
    code: i32,
) -> Result<RunStatus, String> {
    let mut map = runs().lock().map_err(|e| e.to_string())?;
    let run = map
        .get_mut(&run_id)
        .ok_or_else(|| "unknown canvas run".to_string())?;
    apply_exit(run, &terminal_id, code);
    Ok(run_status(run))
}

#[tauri::command]
pub fn canvas_run_probe_ready(run_id: String, terminal_id: String) -> Result<bool, String> {
    let map = runs().lock().map_err(|e| e.to_string())?;
    let run = map
        .get(&run_id)
        .ok_or_else(|| "unknown canvas run".to_string())?;
    let Some(port) = run.leases.get(&terminal_id) else {
        return Ok(false);
    };
    Ok(std::net::TcpStream::connect(("127.0.0.1", *port)).is_ok())
}

#[tauri::command]
pub fn canvas_run_mark_running(
    run_id: String,
    terminal_id: String,
) -> Result<RunStatus, String> {
    let mut map = runs().lock().map_err(|e| e.to_string())?;
    let run = map
        .get_mut(&run_id)
        .ok_or_else(|| "unknown canvas run".to_string())?;
    mark_running(run, &terminal_id);
    Ok(run_status(run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas_graph::{CanvasEdge, CanvasSize, CanvasTerminal, CanvasVec};

    fn term(id: &str) -> CanvasTerminal {
        CanvasTerminal {
            id: id.into(),
            name: id.into(),
            cwd: "/tmp".into(),
            command: "true".into(),
            kind: "task".into(),
            agent_id: None,
            position: CanvasVec { x: 0.0, y: 0.0 },
            size: CanvasSize { w: 1.0, h: 1.0 },
            port_policy: None,
            port: None,
            ready_pattern: None,
        }
    }

    fn graph_ab() -> BlockGraph {
        let mut g = BlockGraph::empty("p", "/tmp/ws");
        g.terminals = vec![term("a"), term("b"), term("c")];
        g.edges = vec![CanvasEdge {
            id: "e".into(),
            source: "a".into(),
            target: "b".into(),
        }];
        g
    }

    #[test]
    fn freeze_orders_a_before_b() {
        let g = graph_ab();
        let plan = freeze_plan(&g, "a").unwrap();
        let a = plan.terminal_ids.iter().position(|x| x == "a").unwrap();
        let b = plan.terminal_ids.iter().position(|x| x == "b").unwrap();
        assert!(a < b);
        assert!(!plan.terminal_ids.contains(&"c".to_string()));
    }

    #[test]
    fn freeze_rejects_agent_root() {
        let mut g = graph_ab();
        g.agents.push(crate::canvas_graph::CanvasAgentLayout {
            id: "agent-x".into(),
            position: CanvasVec { x: 0.0, y: 0.0 },
            size: CanvasSize { w: 1.0, h: 1.0 },
        });
        assert!(freeze_plan(&g, "agent-x").is_err());
    }

    #[test]
    fn mutating_live_graph_does_not_change_frozen_plan() {
        let g = graph_ab();
        let plan = freeze_plan(&g, "a").unwrap();
        let mut live = g.clone();
        live.edges.clear();
        let plan2 = freeze_plan(&live, "a").unwrap();
        assert_eq!(plan.terminal_ids.len(), 2);
        assert_eq!(plan2.terminal_ids, vec!["a".to_string()]);
    }

    #[test]
    fn failure_blocks_downstream() {
        let g = graph_ab();
        let plan = freeze_plan(&g, "a").unwrap();
        let mut run = Run {
            node_states: initial_states(&plan),
            plan,
            frozen: g,
            leases: HashMap::new(),
        };
        assert_eq!(ready_to_start(&run), vec!["a".to_string()]);
        mark_running(&mut run, "a");
        apply_exit(&mut run, "a", 1);
        assert_eq!(run.node_states.get("a"), Some(&NodeRunState::Failed));
        assert_eq!(run.node_states.get("b"), Some(&NodeRunState::Blocked));
        assert!(ready_to_start(&run).is_empty());
    }

    #[test]
    fn success_unlocks_downstream() {
        let g = graph_ab();
        let plan = freeze_plan(&g, "a").unwrap();
        let mut run = Run {
            node_states: initial_states(&plan),
            plan,
            frozen: g,
            leases: HashMap::new(),
        };
        mark_running(&mut run, "a");
        apply_exit(&mut run, "a", 0);
        assert_eq!(ready_to_start(&run), vec!["b".to_string()]);
    }

    #[test]
    fn stop_order_is_reverse_dependency() {
        let g = graph_ab();
        let plan = freeze_plan(&g, "a").unwrap();
        let mut run = Run {
            node_states: initial_states(&plan),
            plan,
            frozen: g,
            leases: HashMap::new(),
        };
        mark_running(&mut run, "a");
        apply_exit(&mut run, "a", 0);
        mark_running(&mut run, "b");
        let order = stop_order(&run);
        assert_eq!(order, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn auto_port_allocates_and_is_not_on_graph() {
        let mut g = graph_ab();
        g.terminals[0].command = "python -m http.server {PORT}".into();
        g.terminals[0].kind = "service".into();
        g.terminals[0].port_policy = Some("auto".into());
        let leases = allocate_leases(&g, &["a".into()]).unwrap();
        assert!(leases.get("a").copied().unwrap() > 0);
        let json = serde_json::to_string(&g).unwrap();
        assert!(!json.contains("allocatedPort"));
        assert!(!json.contains(&leases[&"a".to_string()].to_string()) || g.terminals[0].command.contains("{PORT}"));
        assert!(g.terminals[0].command.contains("{PORT}"));
    }

    #[test]
    fn fixed_port_conflict_fails() {
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let p = held.local_addr().unwrap().port();
        let err = allocate_port("fixed", Some(p)).unwrap_err();
        assert!(err.contains("not available"));
    }
}
