// A real stdio MCP client for the Brouillon Link end-to-end smoke test.
//
//   node mcp-client.mjs <syncDir> <relay-exe> [relay args...]
//
// It spawns the relay exactly as `claude mcp add brouillon -- <relay-exe> ...` would, speaks plain
// MCP JSON-RPC over its stdio, and walks the whole path against the RUNNING editor:
//   initialize -> tools/list -> editor_state (retried until the user allowed this client in the
//   consent modal) -> read -> edit (a proposal) -> proposal/wait (until accepted on screen) -> read
//   -> [the scenario presses Ctrl+Z in the editor] -> read.
// Every step is appended as one JSON line to <syncDir>/events.jsonl with an epoch-millisecond
// timestamp; the PowerShell scenario watches that file to know when to click, and writes
// <syncDir>/undone.flag after it pressed Ctrl+Z.
import { spawn } from 'node:child_process';
import { appendFileSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const [syncDir, relay, ...relayArgs] = process.argv.slice(2);
const log = (ev, data = {}) => appendFileSync(join(syncDir, 'events.jsonl'), JSON.stringify({ t: Date.now(), ev, ...data }) + '\n');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const tSpawn = Date.now();
const child = spawn(relay, relayArgs, { stdio: ['pipe', 'pipe', 'pipe'] });
log('relay_spawned', { spawn_ms: Date.now() - tSpawn });
let buf = '';
const waiting = new Map();
child.stdout.on('data', (d) => {
  buf += d.toString('utf8');
  let i;
  while ((i = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, i).trim();
    buf = buf.slice(i + 1);
    if (!line) continue;
    let msg;
    try { msg = JSON.parse(line); } catch { log('non_json_on_stdout', { line: line.slice(0, 200) }); continue; }
    const w = waiting.get(msg.id);
    if (w) { waiting.delete(msg.id); w(msg); }
  }
});
child.stderr.on('data', (d) => log('relay_stderr', { text: d.toString('utf8').slice(0, 300) }));
child.on('exit', (code) => { log('relay_exit', { code }); });

let nextId = 1;
function rpc(method, params) {
  const id = nextId++;
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  return new Promise((res, rej) => {
    waiting.set(id, res);
    setTimeout(() => { if (waiting.delete(id)) rej(new Error(`timeout on ${method}`)); }, 60000);
  });
}
const call = async (name, args = {}) => {
  const r = await rpc('tools/call', { name, arguments: args });
  if (r.error) throw new Error(JSON.stringify(r.error));
  return r.result;
};

try {
  const init = await rpc('initialize', { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'e2e-stdio-client', version: '1.0' } });
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
  log('initialize', { server: init.result?.serverInfo, protocol: init.result?.protocolVersion });
  const list = await rpc('tools/list', {});
  log('tools_list', { count: list.result?.tools?.length, names: list.result?.tools?.map((t) => t.name) });

  // First tool call: refused until the user answers the consent modal in the editor.
  let state;
  for (;;) {
    state = await call('editor_state');
    if (!state.isError) break;
    log('editor_state_refused', { code: state.structuredContent?.error?.code ?? state.structuredContent?.code });
    await sleep(400);
  }
  log('first_call_ok', { docs: state.structuredContent.docs.map((d) => ({ id: d.id, path: d.path, version: d.version })), mode: state.structuredContent.mode });

  const active = state.structuredContent.active_doc;
  const doc = state.structuredContent.docs.find((d) => d.id === active);
  const read1 = await call('read', { doc: active });
  const text1 = read1.structuredContent.content;
  log('read_before', { version: read1.structuredContent.version, text: text1 });

  const edit = await call('edit', {
    doc: active,
    base_revision: read1.structuredContent.version ?? doc.version,
    summary: 'Replace alpha by ALPHA (e2e)',
    edits: [{ match: { old: 'alpha', new: 'ALPHA' } }, { match: { old: 'gamma', new: 'GAMMA' } }],
  });
  log('edit_proposed', { isError: edit.isError, result: edit.structuredContent });
  const pid = edit.structuredContent.id ?? edit.structuredContent.proposal_id;

  const waited = await call('proposal', { id: pid, action: 'wait', wait_s: 45 });
  log('proposal_result', { result: waited.structuredContent });

  const read2 = await call('read', { doc: active });
  log('read_after_accept', { version: read2.structuredContent.version, text: read2.structuredContent.content });

  // The scenario presses Ctrl+Z in the editor now.
  log('waiting_for_undo');
  for (let i = 0; i < 120 && !existsSync(join(syncDir, 'undone.flag')); i++) await sleep(250);
  const read3 = await call('read', { doc: active });
  log('read_after_undo', { version: read3.structuredContent.version, text: read3.structuredContent.content });
  log('done');
} catch (e) {
  log('client_error', { message: String(e) });
  writeFileSync(join(syncDir, 'error.flag'), String(e));
} finally {
  child.stdin.end();
  await sleep(300);
  process.exit(0);
}
