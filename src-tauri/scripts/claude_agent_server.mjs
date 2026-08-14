#!/usr/bin/env node
// CaPilot Claude Agent SDK sidecar (architecture §8.1).
//
// Bridges the Node-side Claude Agent SDK (`@anthropic-ai/claude-agent-sdk`) to
// the same NDJSON JSON-RPC 2.0 wire schema the Codex Direct adapter already
// speaks, so the Rust `direct/claude.rs` adapter reuses the exact Codex session
// machinery (`direct/codex.rs`) — the provider is interchangeable at the
// contract-test level, no Provider-ID branching.
//
// Wire schema (mirrors `codex app-server --listen stdio://`):
//
//   Client → server (requests with `id`):
//     initialize {}                     → {}
//     thread/start {cwd}                → {thread:{id}}
//     thread/resume {threadId, cwd}     → {thread:{id}}
//     model/list {}                     → {data:[ModelDefinition]}
//     thread/settings/update {threadId, model} → {}
//     turn/start {threadId, clientUserMessageId, input} → {} (ack; events follow)
//     turn/interrupt {threadId, turnId} → {}
//     thread/unsubscribe {threadId}     → {}
//
//   Server → client notifications:
//     turn/started {turn:{id}}
//     turn/completed {turn:{id,status[,error]}}
//     item/started {item:{id,type,...}}
//     item/completed {item:{id,type,status[,aggregatedOutput]}}
//     item/agentMessage/delta {itemId, delta}
//     item/reasoning/textDelta {itemId, delta}
//     thread/tokenUsage/updated {tokenUsage:{total:{totalTokens},modelContextWindow}}
//
//   Server → client requests (permissions; client answers {decision}):
//     item/commandExecution/requestApproval {command, approvalId, availableDecisions}
//       decision ∈ {accept, acceptForSession, decline, cancel}
//
// A thread maps 1:1 to a Claude session. The first `turn/start` lazily spawns
// the SDK `query()` and binds the real `session_id` (from `system/init`) to the
// thread; the binding is persisted to ~/.capilot/claude-sidecar-sessions.json so
// a daemon restart can still resume a thread by its stable id.
//
// SDK resolution: sidecar-local node_modules, `CLAUDE_SDK_PATH`, then standard
// ancestor-node_modules resolution.

import { createInterface } from 'node:readline';
import { randomUUID } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';
import os from 'node:os';

// ── Model catalog (surfaced to the model selector; `model/list`) ──

const MODELS = [
  { id: 'claude-opus-5', displayName: 'Claude Opus 5', isDefault: false, supportedReasoningEfforts: [] },
  { id: 'claude-sonnet-5', displayName: 'Claude Sonnet 5', isDefault: true, supportedReasoningEfforts: [] },
  { id: 'claude-haiku-4-5', displayName: 'Claude Haiku 4.5', isDefault: false, supportedReasoningEfforts: [] },
];

const CONTEXT_WINDOW = 200_000;

// ── SDK loading (lazy: catalog / handshake never touch the SDK) ──

let sdkPromise;
function getSdk() {
  if (!sdkPromise) sdkPromise = loadSdk();
  return sdkPromise;
}

async function loadSdk() {
  const scriptDir = path.dirname(fileURLToPath(import.meta.url));
  const candidates = [
    path.join(scriptDir, 'node_modules', '@anthropic-ai', 'claude-agent-sdk'),
    path.join(scriptDir, '..', 'node_modules', '@anthropic-ai', 'claude-agent-sdk'),
    path.join(scriptDir, '..', '..', 'node_modules', '@anthropic-ai', 'claude-agent-sdk'),
    process.env.CLAUDE_SDK_PATH,
  ].filter(Boolean);
  for (const dir of candidates) {
    const sdkFile = path.join(dir, 'sdk.mjs');
    if (existsSync(sdkFile)) {
      return await import(pathToFileURL(sdkFile).href);
    }
  }
  return import('@anthropic-ai/claude-agent-sdk');
}

// ── Thread → Claude-session persistence (survives daemon restarts) ──

function mappingFile() {
  return path.join(os.homedir(), '.capilot', 'claude-sidecar-sessions.json');
}

let mappings = {};
try {
  mappings = JSON.parse(readFileSync(mappingFile(), 'utf8'));
} catch {
  mappings = {};
}

function persistMapping(tid, sessionId) {
  if (!tid || !sessionId || mappings[tid] === sessionId) return;
  mappings[tid] = sessionId;
  try {
    mkdirSync(path.dirname(mappingFile()), { recursive: true });
    writeFileSync(mappingFile(), JSON.stringify(mappings, null, 2));
  } catch (e) {
    // Best-effort; an unwritable mapping file degrades resume only.
    console.error(`[claude-sidecar] persist mapping failed: ${e.message}`);
  }
}

// ── Wire I/O ─────────────────────────────────────────────────────

function writeLine(obj) {
  try {
    process.stdout.write(JSON.stringify(obj) + '\n');
  } catch (e) {
    // EPIPE — the client went away; exit so the parent reaps us.
    process.exit(0);
  }
}
const notify = (method, params) => writeLine({ jsonrpc: '2.0', method, params });
const respond = (id, result) => writeLine({ jsonrpc: '2.0', id, result });
const respondError = (id, code, message) =>
  writeLine({ jsonrpc: '2.0', id, error: { code, message } });

// ── Threads ──────────────────────────────────────────────────────

const threads = new Map(); // threadId -> ThreadState

function newThread(cwd) {
  return {
    id: null,
    cwd,
    model: null,
    claudeSessionId: null,
    active: false,
    activeQuery: null,
    turnId: null,
    interruptRequested: false,
    textLen: new Map(),
    thinkLen: new Map(),
    textItems: new Map(),
    thinkItems: new Map(),
    textCounter: 0,
    thinkCounter: 0,
    toolMeta: new Map(),
  };
}

// ── Permission round-trips (SDK canUseTool ↔ client approval) ──

let nextRpcId = 1;
const pendingApprovals = new Map(); // rpcId -> {thread, resolve, suggestions}

function requestApproval(thread, toolName, input, extra) {
  return new Promise((resolve) => {
    if (thread.interruptRequested) {
      resolve({ behavior: 'deny' });
      return;
    }
    const rpcId = nextRpcId++;
    pendingApprovals.set(rpcId, {
      thread,
      resolve,
      suggestions: (extra && extra.suggestions) || [],
    });
    const command =
      toolName === 'Bash'
        ? (input && input.command) || ''
        : `${toolName} ${JSON.stringify(input || {})}`;
    writeLine({
      jsonrpc: '2.0',
      id: rpcId,
      method: 'item/commandExecution/requestApproval',
      params: {
        command,
        approvalId: (extra && extra.toolUseID) || String(rpcId),
        availableDecisions: [],
      },
    });
  });
}

function resolveAllApprovals(thread) {
  for (const [rpcId, entry] of [...pendingApprovals]) {
    if (entry.thread === thread) {
      pendingApprovals.delete(rpcId);
      entry.resolve({ behavior: 'deny' });
    }
  }
}

// ── SDK event → wire notification mapping ────────────────────────

function handleAssistant(thread, msg) {
  const mid = msg.message && msg.message.id;
  if (!mid) return;
  const blocks = msg.message.content;
  if (!Array.isArray(blocks)) return;
  for (const block of blocks) {
    if (!block || typeof block !== 'object') continue;
    if (block.type === 'text' && typeof block.text === 'string' && block.text.length > 0) {
      const key = `t:${mid}`;
      const prev = thread.textLen.get(key) || 0;
      const full = block.text;
      if (full.length > prev) {
        let itemId = thread.textItems.get(key);
        if (!itemId) {
          itemId = `${mid}:a${thread.textCounter++}`;
          thread.textItems.set(key, itemId);
        }
        thread.textLen.set(key, full.length);
        notify('item/agentMessage/delta', { itemId, delta: full.slice(prev) });
      }
    } else if (
      block.type === 'thinking' &&
      typeof block.thinking === 'string' &&
      block.thinking.length > 0
    ) {
      const key = `r:${mid}`;
      const prev = thread.thinkLen.get(key) || 0;
      const full = block.thinking;
      if (full.length > prev) {
        let itemId = thread.thinkItems.get(key);
        if (!itemId) {
          itemId = `${mid}:r${thread.thinkCounter++}`;
          thread.thinkItems.set(key, itemId);
        }
        thread.thinkLen.set(key, full.length);
        notify('item/reasoning/textDelta', { itemId, delta: full.slice(prev) });
      }
    } else if (block.type === 'tool_use' && block.id && block.name) {
      handleToolUse(thread, block);
    }
  }
}

function handleToolUse(thread, block) {
  const { id, name, input } = block;
  if (name === 'Bash') {
    thread.toolMeta.set(id, { kind: 'commandExecution' });
    notify('item/started', {
      item: {
        id,
        type: 'commandExecution',
        command: (input && input.command) || '',
        cwd: (input && input.cwd) || thread.cwd,
      },
    });
  } else if (name === 'Write') {
    thread.toolMeta.set(id, { kind: 'fileChange' });
    notify('item/started', {
      item: {
        id,
        type: 'fileChange',
        status: 'inProgress',
        changes: [{ kind: 'edit', filePath: (input && input.file_path) || '' }],
      },
    });
  } else {
    // Read/Glob/Grep/TodoWrite/WebFetch/WebSearch/Task/… — render as a command
    // item so the timeline still shows what the agent did.
    thread.toolMeta.set(id, { kind: 'commandExecution' });
    notify('item/started', {
      item: {
        id,
        type: 'commandExecution',
        command: `${name} ${JSON.stringify(input || {})}`,
        cwd: thread.cwd,
      },
    });
  }
}

function handleUserMessage(thread, msg) {
  const content = msg.message && msg.message.content;
  if (!Array.isArray(content)) return;
  for (const c of content) {
    if (c && c.type === 'tool_result') {
      const tid = c.tool_use_id;
      const meta = thread.toolMeta.get(tid);
      if (!meta) continue;
      const text = toolResultText(c.content);
      if (meta.kind === 'commandExecution') {
        notify('item/completed', {
          item: { id: tid, type: 'commandExecution', status: 'completed', aggregatedOutput: text },
        });
      } else {
        notify('item/completed', {
          item: { id: tid, type: 'fileChange', status: 'completed' },
        });
      }
      thread.toolMeta.delete(tid);
    }
  }
}

function toolResultText(content) {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content
    .map((x) => {
      if (typeof x === 'string') return x;
      if (x && typeof x.text === 'string') return x.text;
      return JSON.stringify(x);
    })
    .filter(Boolean)
    .join('\n');
}

function finalizeTurn(thread, status, errorMessage) {
  const turnId = thread.turnId;
  if (!turnId) return;
  thread.turnId = null;
  const u = thread.lastUsage || {};
  const total = (u.input_tokens || 0) + (u.output_tokens || 0);
  if (total > 0) {
    notify('thread/tokenUsage/updated', {
      tokenUsage: { total: { totalTokens: total }, modelContextWindow: CONTEXT_WINDOW },
    });
  }
  const turn = { id: turnId, status };
  if (errorMessage) turn.error = { message: errorMessage };
  notify('turn/completed', { turn });
}

// ── Turn execution ───────────────────────────────────────────────

async function runTurn(thread, promptText) {
  try {
    const sdk = await getSdk();
    const options = {
      cwd: thread.cwd,
      permissionMode: 'default',
      canUseTool: (toolName, input, extra) => requestApproval(thread, toolName, input, extra),
    };
    if (thread.model) options.model = thread.model;
    if (thread.claudeSessionId) options.resume = thread.claudeSessionId;
    thread.active = true;
    const q = sdk.query({ prompt: promptText, options });
    thread.activeQuery = q;
    for await (const msg of q) {
      if (msg.type === 'system') {
        if (!thread.claudeSessionId && msg.session_id) {
          thread.claudeSessionId = msg.session_id;
          persistMapping(thread.id, msg.session_id);
        }
      } else if (msg.type === 'assistant') {
        handleAssistant(thread, msg);
      } else if (msg.type === 'user') {
        handleUserMessage(thread, msg);
      } else if (msg.type === 'result') {
        thread.lastUsage = msg.usage || {};
        const status = thread.interruptRequested
          ? 'interrupted'
          : msg.is_error || (msg.subtype && msg.subtype.startsWith('error'))
            ? 'failed'
            : 'completed';
        finalizeTurn(
          thread,
          status,
          status === 'failed' ? (msg.result || 'claude turn failed') : undefined,
        );
        break;
      }
    }
  } catch (e) {
    finalizeTurn(thread, 'failed', (e && e.message) || String(e));
  } finally {
    thread.active = false;
    thread.activeQuery = null;
    resolveAllApprovals(thread);
  }
}

// ── JSON-RPC dispatch ────────────────────────────────────────────

function handleRequest(id, method, params) {
  switch (method) {
    case 'initialize':
      respond(id, {});
      break;
    case 'thread/start': {
      const tid = randomUUID();
      const thread = newThread(params.cwd || process.cwd());
      thread.id = tid;
      threads.set(tid, thread);
      respond(id, { thread: { id: tid } });
      break;
    }
    case 'thread/resume': {
      const tid = params.threadId;
      let thread = threads.get(tid);
      if (!thread) {
        // A daemon restart loses in-memory state; recover the Claude session id
        // from the persisted mapping, or assume the thread id itself is one.
        thread = newThread(params.cwd || process.cwd());
        thread.id = tid;
        thread.claudeSessionId = mappings[tid] || tid;
        threads.set(tid, thread);
      } else if (params.cwd) {
        thread.cwd = params.cwd;
      }
      respond(id, { thread: { id: tid } });
      break;
    }
    case 'model/list':
      respond(id, { data: MODELS });
      break;
    case 'thread/settings/update': {
      const thread = threads.get(params.threadId);
      if (!thread) return respondError(id, -32602, `unknown thread: ${params.threadId}`);
      if (typeof params.model === 'string') thread.model = params.model;
      respond(id, {});
      break;
    }
    case 'turn/start': {
      const thread = threads.get(params.threadId);
      if (!thread) return respondError(id, -32602, `unknown thread: ${params.threadId}`);
      if (thread.active) return respondError(id, -32602, 'turn already active');
      const input = Array.isArray(params.input) ? params.input : [];
      const text = input
        .filter((c) => c && c.type === 'text' && typeof c.text === 'string')
        .map((c) => c.text)
        .join('\n');
      if (!text.trim()) return respondError(id, -32602, 'empty prompt');
      thread.turnId = randomUUID();
      thread.interruptRequested = false;
      thread.lastUsage = null;
      thread.textLen = new Map();
      thread.thinkLen = new Map();
      thread.textItems = new Map();
      thread.thinkItems = new Map();
      thread.textCounter = 0;
      thread.thinkCounter = 0;
      thread.toolMeta = new Map();
      notify('turn/started', { turn: { id: thread.turnId } });
      respond(id, {});
      runTurn(thread, text).catch(() => {});
      break;
    }
    case 'turn/interrupt': {
      const thread = threads.get(params.threadId);
      if (!thread) return respondError(id, -32602, `unknown thread: ${params.threadId}`);
      thread.interruptRequested = true;
      if (thread.activeQuery) {
        thread.activeQuery.interrupt().catch(() => {});
      }
      respond(id, {});
      break;
    }
    case 'thread/unsubscribe': {
      const thread = threads.get(params.threadId);
      if (thread) {
        if (thread.activeQuery) {
          thread.activeQuery.interrupt().catch(() => {});
        }
        resolveAllApprovals(thread);
        threads.delete(params.threadId);
      }
      respond(id, {});
      break;
    }
    default:
      respondError(id, -32601, `method not found: ${method}`);
  }
}

function handleResponse(id, result) {
  const entry = pendingApprovals.get(id);
  if (!entry) return;
  pendingApprovals.delete(id);
  const decision = result && result.decision;
  if (decision === 'accept') {
    entry.resolve({ behavior: 'allow' });
  } else if (decision === 'acceptForSession') {
    entry.resolve({ behavior: 'allow', updatedPermissions: entry.suggestions });
  } else {
    entry.resolve({ behavior: 'deny' });
  }
}

function handleNotification(method, _params) {
  // The CaPilot client only sends requests; ignore stray notifications.
  if (method === 'shutdown') process.exit(0);
}

// ── stdin loop ───────────────────────────────────────────────────

const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });
rl.on('line', (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  const id = msg.id;
  const method = msg.method;
  if (typeof id === 'number' && typeof method === 'string') {
    handleRequest(id, method, msg.params || {});
  } else if (typeof id === 'number') {
    handleResponse(id, msg.result);
  } else if (typeof method === 'string') {
    handleNotification(method, msg.params || {});
  }
});
rl.on('close', () => process.exit(0));
