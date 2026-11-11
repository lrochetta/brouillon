//! MCP core over the in-memory fake editor: protocol, tools, concurrency, permissions, audit.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use brouillon_mcp::api::CommandInfo;
use brouillon_mcp::audit::verify_chain;
use brouillon_mcp::core::{McpConfig, McpCore, Session};
use brouillon_mcp::fake::FakeEditor;
use brouillon_mcp::policy::{Grant, PolicyConfig, Scope};
use serde_json::{json, Value};

const FOLDER: &str = "C:\\proj";
const FILE: &str = "C:\\proj\\a.rs";

struct H {
    core: Arc<McpCore<FakeEditor>>,
    api: Arc<FakeEditor>,
    s: Session,
    doc: String,
}

fn grant(core: &McpCore<FakeEditor>, client: &str, scope: Scope) {
    core.grant(Grant { client: client.into(), folder: FOLDER.into(), scope, expires_at: None, exe_hash: None });
}

fn harness(cfg: McpConfig, scope: Option<Scope>, text: &str) -> H {
    let api = Arc::new(FakeEditor::new());
    api.add_folder(FOLDER);
    let doc = api.add_doc(Some(FILE), text);
    let core = McpCore::new(api.clone(), cfg).unwrap();
    // Time runs 1 s per reading so ordinary tests are never rate limited; the limiter has its own tests.
    let t = std::sync::atomic::AtomicU64::new(0);
    core.set_ms_clock(move || t.fetch_add(1000, std::sync::atomic::Ordering::SeqCst));
    if let Some(sc) = scope {
        grant(&core, "claude", sc);
    }
    H { core, api, s: Session::new("claude"), doc }
}

fn std_h(text: &str) -> H {
    harness(McpConfig::default(), Some(Scope::S2), text)
}

impl H {
    fn rpc(&mut self, v: Value) -> Value {
        let line = self.core.handle_line(&mut self.s, &v.to_string()).expect("a reply");
        serde_json::from_str(&line).unwrap()
    }
    /// Returns (isError, structuredContent).
    fn call(&mut self, tool: &str, args: Value) -> (bool, Value) {
        let r = self.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}));
        let res = &r["result"];
        assert!(res.is_object(), "protocol error: {r}");
        (res["isError"].as_bool().unwrap(), res["structuredContent"].clone())
    }
    fn ok(&mut self, tool: &str, args: Value) -> Value {
        let (e, v) = self.call(tool, args);
        assert!(!e, "{tool} failed: {v}");
        v
    }
    fn err(&mut self, tool: &str, args: Value) -> String {
        let (e, v) = self.call(tool, args);
        assert!(e, "{tool} should fail, got {v}");
        v["code"].as_str().unwrap().to_string()
    }
    fn version(&self) -> u64 {
        self.api.version(&self.doc)
    }
}

fn replace(old: &str, new: &str) -> Value {
    json!({"match": {"old": old, "new": new}})
}

// ---------------------------------------------------------------- protocol

#[test]
fn initialize_negotiates_and_notifications_are_silent() {
    let mut h = std_h("x");
    let r = h.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "t"}}}));
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "brouillon");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    let r = h.rpc(json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}));
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    // Notifications get no reply, known or unknown.
    for m in ["notifications/initialized", "notifications/cancelled", "notifications/whatever"] {
        assert!(h.core.handle_line(&mut h.s, &json!({"jsonrpc": "2.0", "method": m}).to_string()).is_none());
    }
    assert_eq!(h.rpc(json!({"jsonrpc": "2.0", "id": "a", "method": "ping"}))["result"], json!({}));
    let d = h.rpc(json!({"jsonrpc": "2.0", "id": 3, "method": "server/discover"}));
    assert_eq!(d["result"]["supportedVersions"][0], "2025-11-25");
}

#[test]
fn protocol_errors_are_proper_json_rpc() {
    let mut h = std_h("x");
    let parse = h.core.handle_line(&mut h.s, "{not json").unwrap();
    assert_eq!(serde_json::from_str::<Value>(&parse).unwrap()["error"]["code"], -32700);
    assert_eq!(h.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "nope"}))["error"]["code"], -32601);
    assert_eq!(h.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "rm_rf"}}))["error"]["code"], -32602);
    assert_eq!(h.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"arguments": {}}}))["error"]["code"], -32602);
    assert_eq!(h.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "read", "arguments": 5}}))["error"]["code"], -32602);
    // Wrong version tag and non-object / batch inputs are Invalid Request (batches cannot dodge per-call checks).
    assert_eq!(h.rpc(json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}))["error"]["code"], -32600);
    assert_eq!(h.rpc(json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}]))["error"]["code"], -32600);
    assert_eq!(h.rpc(json!(42))["error"]["code"], -32600);
    assert_eq!(h.rpc(json!({"jsonrpc": "2.0", "id": {"x": 1}, "method": "ping"}))["error"]["code"], -32600);
    // The id of a request is always echoed.
    assert_eq!(h.rpc(json!({"jsonrpc": "2.0", "id": "abc", "method": "nope"}))["id"], "abc");
}

#[test]
fn tools_list_is_static_and_deterministic() {
    let mut h = std_h("x");
    let a = h.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
    let b = h.rpc(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    assert_eq!(a["result"]["tools"], b["result"]["tools"]);
    let names: Vec<&str> = a["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["editor_state", "read", "search", "edit", "proposal", "open", "diagnostics", "run_command", "save", "annotate"]);
    assert_eq!(a["result"]["tools"][1]["annotations"]["readOnlyHint"], true);
    assert_eq!(a["result"]["tools"][3]["annotations"]["destructiveHint"], false);
    // tools/list works before any consent (static); tools/call does not.
    let mut stranger = harness(McpConfig::default(), None, "x");
    assert!(stranger.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))["result"]["tools"].is_array());
}

#[test]
fn serve_loop_handles_a_stream_of_lines() {
    let h = std_h("hello\n");
    let input = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string(),
        String::new(),
        "garbage".to_string(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "editor_state", "arguments": {}}}).to_string(),
    ]
    .join("\n");
    let mut out = Vec::new();
    let mut s = Session::new("claude");
    h.core.serve(&mut s, Cursor::new(input.into_bytes()), &mut out).unwrap();
    let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3, "ping, parse error, editor_state; the notification is silent");
    assert_eq!(lines[1]["error"]["code"], -32700);
    assert_eq!(lines[2]["result"]["isError"], false);
}

#[test]
fn oversize_message_gets_an_error_and_the_loop_survives() {
    let h = std_h("x");
    let mut data = vec![b'a'; 1024 * 1024 + 5];
    data.push(b'\n');
    data.extend_from_slice(json!({"jsonrpc": "2.0", "id": 9, "method": "ping"}).to_string().as_bytes());
    data.push(b'\n');
    let mut out = Vec::new();
    h.core.serve(&mut Session::new("claude"), Cursor::new(data), &mut out).unwrap();
    let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["error"]["code"], -32600);
    assert_eq!(lines[1]["id"], 9);
}

// ---------------------------------------------------------------- read tools

#[test]
fn read_returns_numbered_lines_version_and_hash() {
    let mut h = std_h("alpha\r\nbeta\r\ngamma\r\n");
    let r = h.ok("read", json!({}));
    assert_eq!(r["version"], 1);
    assert_eq!(r["total_lines"], 3);
    assert_eq!(r["trust"], "untrusted");
    assert_eq!(r["content"], "     1| alpha\n     2| beta\n     3| gamma\n");
    assert_eq!(r["hash"], brouillon_mcp::fake::hash_text("alpha\r\nbeta\r\ngamma\r\n"));
    let r = h.ok("read", json!({"start_line": 2, "max_lines": 1}));
    assert_eq!(r["content"], "     2| beta\n");
    assert_eq!(r["truncated"], true);
    assert_eq!(r["next_cursor"], 3);
    let st = h.ok("editor_state", json!({}));
    assert_eq!(st["docs"][0]["eol"], "crlf");
}

#[test]
fn read_is_paginated_and_capped() {
    let big: String = (1..=5000).map(|i| format!("line number {i}\n")).collect();
    let mut h = std_h(&big);
    let r = h.ok("read", json!({}));
    assert!(r["content"].as_str().unwrap().len() <= 8 * 1024 + 64);
    assert_eq!(r["truncated"], true);
    let next = r["next_cursor"].as_u64().unwrap();
    let r2 = h.ok("read", json!({"start_line": next}));
    assert_eq!(r2["start_line"].as_u64().unwrap(), next);
    assert!(r2["content"].as_str().unwrap().contains(&format!("line number {next}")));
}

#[test]
fn read_selection_and_unsaved_buffer() {
    let mut h = std_h("one\ntwo\nthree\n");
    h.api.set_selection(&h.doc.clone(), (4, 7));
    let r = h.ok("read", json!({"selection": true}));
    assert_eq!(r["content"], "     2| two\n");
    let st = h.ok("editor_state", json!({}));
    assert_eq!(st["selection"]["start"], json!({"line": 2, "col": 1}));
    let scratch = h.api.add_doc(None, "unsaved draft");
    let r = h.ok("read", json!({"doc": scratch}));
    assert!(r["content"].as_str().unwrap().contains("unsaved draft"));
}

#[test]
fn hidden_unicode_is_made_visible() {
    let mut h = std_h("ok \u{202E}evil\u{200B}\u{E0049}\n");
    let r = h.ok("read", json!({}));
    let c = r["content"].as_str().unwrap();
    assert!(c.contains("<U+202E>") && c.contains("<U+200B>") && c.contains("<U+E0049>"));
    assert!(!c.contains('\u{202E}'));
}

#[test]
fn private_and_denylisted_content_never_leaves() {
    let mut h = std_h("public text\n");
    h.api.add_file("C:\\proj\\.env", "API_KEY=CANARY-SECRET-123");
    h.api.add_file("C:\\proj\\notes.txt", "fine");
    let secret = h.api.add_doc(Some("C:\\proj\\private.txt"), "CANARY-PRIVATE-456");
    h.api.set_private(&secret, true);
    let envdoc = h.api.add_doc(Some("C:\\proj\\.env.local"), "TOKEN=CANARY-ENVDOC-789");
    assert_eq!(h.err("read", json!({"path": "C:\\proj\\.env"})), "DENYLISTED");
    assert_eq!(h.err("read", json!({"doc": envdoc})), "DENYLISTED");
    assert_eq!(h.err("read", json!({"doc": secret})), "NO_SUCH_DOC");
    assert_eq!(h.err("edit", json!({"doc": secret, "base_revision": 1, "edits": [replace("CANARY", "x")]})), "NO_SUCH_DOC");
    // Not listed, not searchable.
    let st = h.ok("editor_state", json!({})).to_string();
    let hits = h.ok("search", json!({"query": "CANARY"})).to_string();
    let all = format!("{st}{hits}");
    assert!(!all.contains("CANARY") && !all.contains("private.txt") && !all.contains(".env"), "{all}");
    // A file outside the granted folder is invisible too.
    assert_eq!(h.err("read", json!({"path": "C:\\other\\x.txt"})), "OUT_OF_SCOPE");
    assert_eq!(h.err("read", json!({"path": "C:\\proj\\..\\other\\x.txt"})), "OUT_OF_SCOPE");
    assert!(h.ok("read", json!({"path": "C:\\proj\\notes.txt"}))["content"].as_str().unwrap().contains("fine"));
}

#[test]
fn read_prefers_the_open_dirty_buffer_over_disk() {
    let mut h = std_h("dirty in memory\n");
    h.api.add_file(FILE, "stale on disk\n");
    let r = h.ok("read", json!({"path": FILE}));
    assert!(r["content"].as_str().unwrap().contains("dirty in memory"));
    assert_eq!(r["version"], 1);
}

#[test]
fn editor_state_hides_docs_outside_grants() {
    let mut h = std_h("a\n");
    h.api.add_doc(Some("D:\\elsewhere\\b.rs"), "b\n");
    h.api.add_doc(None, "scratch");
    let st = h.ok("editor_state", json!({}));
    let paths: Vec<String> = st["docs"].as_array().unwrap().iter().map(|d| d["path"].as_str().unwrap_or("<unsaved>").to_string()).collect();
    assert_eq!(paths, [FILE, "<unsaved>"]);
    assert_eq!(st["mode"], "S2");
    assert_eq!(st["workspace_folders"], json!([FOLDER]));
}

#[test]
fn search_finds_positions_and_paginates() {
    let mut h = std_h("fn main() {}\nlet Foo = 1;\nfoo foo\n");
    let r = h.ok("search", json!({"query": "foo"}));
    let hits = r["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!((hits[0]["line"].as_u64(), hits[0]["col"].as_u64()), (Some(2), Some(5)));
    let r = h.ok("search", json!({"query": "foo", "case_sensitive": true}));
    assert_eq!(r["hits"].as_array().unwrap().len(), 2);
    let r = h.ok("search", json!({"query": "o+", "regex": true, "max_results": 2}));
    assert_eq!(r["truncated"], true);
    let next = r["next_cursor"].as_u64().unwrap();
    let r2 = h.ok("search", json!({"query": "o+", "regex": true, "max_results": 50, "cursor": next}));
    assert!(!r2["hits"].as_array().unwrap().is_empty());
    assert_eq!(h.err("search", json!({"query": "(", "regex": true})), "INVALID_ARGUMENT");
    assert_eq!(h.err("search", json!({"query": ""})), "INVALID_ARGUMENT");
    // Literal mode escapes metacharacters.
    assert_eq!(h.ok("search", json!({"query": "()"}))["hits"].as_array().unwrap().len(), 1);
}

#[test]
fn diagnostics_open_and_annotate_work_without_touching_text() {
    let mut h = std_h("call(\nok()\n");
    let d = h.ok("diagnostics", json!({}));
    assert_eq!(d["diagnostics"][0]["line"], 1);
    h.api.add_file("C:\\proj\\b.rs", "b");
    let o = h.ok("open", json!({"path": "C:\\proj\\b.rs"}));
    assert!(o["doc"].is_string());
    assert_eq!(h.api.opened(), [("C:\\proj\\b.rs".to_string(), false)], "no focus unless asked");
    let before = (h.api.text(&h.doc), h.version());
    h.ok("annotate", json!({"doc": h.doc.clone(), "line": 1, "kind": "note", "text": "look\u{0007}here", "ttl_s": 5000}));
    assert_eq!(h.api.annotations()[0].3, "lookhere");
    assert_eq!((h.api.text(&h.doc), h.version()), before);
    assert_eq!(h.err("annotate", json!({"doc": h.doc.clone(), "line": 1, "kind": "explode"})), "INVALID_ARGUMENT");
    assert_eq!(h.err("open", json!({"path": "C:\\proj\\..\\x"})), "OUT_OF_SCOPE");
    h.ok("open", json!({"path": "C:\\proj\\b.rs", "focus": true}));
    assert_eq!(h.api.opened()[1].1, true);
}

// ---------------------------------------------------------------- write path

#[test]
fn edit_is_a_proposal_and_nothing_changes_until_accepted() {
    let mut h = std_h("hello world\n");
    let v = h.version();
    let r = h.ok("edit", json!({"base_revision": v, "edits": [replace("world", "there")], "summary": "greet"}));
    assert_eq!(r["status"], "pending");
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(h.api.text(&h.doc), "hello world\n", "buffer untouched");
    assert_eq!(h.version(), v);
    assert!(h.api.groups().is_empty());
    assert_eq!(h.api.events(), [format!("pending:{id}")]);
    assert_eq!(h.core.pending_proposals(), [id.clone()]);
    assert_eq!(h.ok("proposal", json!({"id": id}))["status"], "pending");
    assert_eq!(h.ok("editor_state", json!({}))["pending_proposals"][0]["id"], id.as_str());

    let out = h.core.resolve_proposal(&id, None).unwrap();
    assert_eq!(out.status, "accepted");
    assert_eq!(h.api.text(&h.doc), "hello there\n");
    assert_eq!(h.api.groups(), ["AI edit: claude: greet"]);
    let st = h.ok("proposal", json!({"id": id}));
    assert_eq!(st["status"], "accepted");
    assert_eq!(st["new_revision"], h.version());
    assert_eq!(st["applied_hunks"], json!([0]));
    // One undo gesture restores the exact text.
    assert!(h.api.undo(&h.doc));
    assert_eq!(h.api.text(&h.doc), "hello world\n");
    // A decided proposal cannot be decided twice.
    assert_eq!(h.core.resolve_proposal(&id, None).unwrap_err().code, "NOT_PENDING");
}

#[test]
fn fifty_edits_apply_as_one_undo_group() {
    let text: String = (0..50).map(|i| format!("v{i:02} = old\n")).collect();
    let mut h = std_h(&text);
    let edits: Vec<Value> = (0..50).map(|i| json!({"range": {"start": {"line": i + 1, "col": 7}, "end": {"line": i + 1, "col": 10}, "new": "new"}})).collect();
    let r = h.ok("edit", json!({"base_revision": h.version(), "edits": edits}));
    let id = r["id"].as_str().unwrap();
    h.core.resolve_proposal(id, None).unwrap();
    assert_eq!(h.api.groups().len(), 1);
    assert!(h.api.text(&h.doc).lines().all(|l| l.ends_with("= new")));
    assert!(h.api.undo(&h.doc));
    assert_eq!(h.api.text(&h.doc), text);
}

#[test]
fn partial_accept_applies_only_chosen_hunks() {
    let mut h = std_h("a1\na2\na3\n");
    let r = h.ok("edit", json!({"base_revision": h.version(), "edits": [replace("a1", "b1"), replace("a2", "b2"), replace("a3", "b3")]}));
    let id = r["id"].as_str().unwrap().to_string();
    let out = h.core.resolve_proposal(&id, Some(&[0, 2])).unwrap();
    assert_eq!(out.status, "partially_accepted");
    assert_eq!(h.api.text(&h.doc), "b1\na2\nb3\n");
    let st = h.ok("proposal", json!({"id": id}));
    assert_eq!(st["applied_hunks"], json!([0, 2]));
    assert_eq!(st["rejected_hunks"], json!([1]));
}

#[test]
fn reject_all_leaves_text_alone() {
    let mut h = std_h("a\n");
    let id = h.ok("edit", json!({"base_revision": 1, "edits": [replace("a", "b")]}))["id"].as_str().unwrap().to_string();
    let out = h.core.resolve_proposal(&id, Some(&[])).unwrap();
    assert_eq!(out.status, "rejected");
    assert_eq!(h.api.text(&h.doc), "a\n");
    assert!(h.api.groups().is_empty());
    assert_eq!(h.ok("proposal", json!({"id": id}))["status"], "rejected");
}

#[test]
fn stale_revision_never_clobbers_human_typing() {
    let mut h = std_h("hello\n");
    let v = h.version();
    h.api.human_edit(&h.doc.clone(), "hello, typed by the human\n");
    let (is_err, e) = h.call("edit", json!({"base_revision": v, "edits": [replace("hello", "bye")]}));
    assert!(is_err);
    assert_eq!(e["code"], "STALE_VERSION");
    assert_eq!(e["retryable"], true);
    assert_eq!(e["data"]["current_revision"], v + 1);
    assert_eq!(h.api.text(&h.doc), "hello, typed by the human\n");
    assert!(h.core.pending_proposals().is_empty());
}

#[test]
fn typing_after_proposal_makes_accept_stale() {
    let mut h = std_h("hello\n");
    let id = h.ok("edit", json!({"base_revision": 1, "edits": [replace("hello", "bye")]}))["id"].as_str().unwrap().to_string();
    h.api.human_edit(&h.doc.clone(), "hello!\n");
    let e = h.core.resolve_proposal(&id, None).unwrap_err();
    assert_eq!(e.code, "STALE_VERSION");
    assert_eq!(h.api.text(&h.doc), "hello!\n");
    assert_eq!(h.err("proposal", json!({"id": id})), "STALE_VERSION");
    assert!(h.api.events().contains(&format!("closed:{id}:stale")));
}

#[test]
fn match_errors_are_specific() {
    let mut h = std_h("x = 1\nx = 2\n");
    let v = h.version();
    let (_, e) = h.call("edit", json!({"base_revision": v, "edits": [replace("x = ", "y = ")]}));
    assert_eq!(e["code"], "AMBIGUOUS_MATCH");
    assert_eq!(e["data"]["count"], 2);
    assert_eq!(h.err("edit", json!({"base_revision": v, "edits": [replace("zzz", "y")]})), "NO_MATCH");
    let r = h.ok("edit", json!({"base_revision": v, "edits": [{"match": {"old": "x = ", "new": "y = ", "occurrence": 2}}]}));
    h.core.resolve_proposal(r["id"].as_str().unwrap(), None).unwrap();
    assert_eq!(h.api.text(&h.doc), "x = 1\ny = 2\n");
    let v = h.version();
    assert_eq!(h.err("edit", json!({"base_revision": v, "edits": [{"match": {"old": "x = ", "new": "q", "occurrence": 5}}]})), "INVALID_ARGUMENT");
    // base_revision is mandatory; empty and oversized batches and overlaps are refused.
    assert_eq!(h.err("edit", json!({"edits": [replace("x", "y")]})), "INVALID_ARGUMENT");
    assert_eq!(h.err("edit", json!({"base_revision": v, "edits": []})), "INVALID_ARGUMENT");
    let many: Vec<Value> = (0..201).map(|_| replace("x", "y")).collect();
    assert_eq!(h.err("edit", json!({"base_revision": v, "edits": many})), "INVALID_ARGUMENT");
    let overlap = json!([
        {"range": {"start": {"line": 1, "col": 1}, "end": {"line": 1, "col": 4}, "new": "A"}},
        {"range": {"start": {"line": 1, "col": 2}, "end": {"line": 1, "col": 3}, "new": "B"}}
    ]);
    assert_eq!(h.err("edit", json!({"base_revision": v, "edits": overlap})), "INVALID_ARGUMENT");
    assert_eq!(h.err("edit", json!({"base_revision": v, "edits": [{"range": {"start": {"line": 99, "col": 1}, "end": {"line": 99, "col": 1}, "new": "z"}}]})), "INVALID_ARGUMENT");
}

#[test]
fn insert_at_and_range_edits_use_char_columns() {
    let mut h = std_h("é1\nend\n");
    let r = h.ok("edit", json!({"base_revision": 1, "edits": [
        {"insert_at": {"line": 1, "col": 2}, "text": "-"},
        {"insert_at": {"line": 2, "col": 4}, "text": "!"}
    ]}));
    h.core.resolve_proposal(r["id"].as_str().unwrap(), None).unwrap();
    assert_eq!(h.api.text(&h.doc), "é-1\nend!\n");
}

#[test]
fn dry_run_returns_the_diff_and_stores_nothing() {
    let mut h = std_h("a b\n");
    let r = h.ok("edit", json!({"base_revision": 1, "edits": [replace("b", "c")], "dry_run": true}));
    assert_eq!(r["status"], "dry_run");
    assert_eq!(r["hunks"][0]["old"], "b");
    assert_eq!(r["hunks"][0]["new"], "c");
    assert!(h.core.pending_proposals().is_empty());
    assert_eq!(h.api.text(&h.doc), "a b\n");
    // Dry run needs only read permission.
    let mut ro = harness(McpConfig::default(), Some(Scope::S1), "a b\n");
    assert_eq!(ro.ok("edit", json!({"base_revision": 1, "edits": [replace("b", "c")], "dry_run": true}))["status"], "dry_run");
    assert_eq!(ro.err("edit", json!({"base_revision": 1, "edits": [replace("b", "c")]})), "OUT_OF_SCOPE");
}

#[test]
fn client_edit_id_makes_retries_idempotent() {
    let mut h = std_h("a\n");
    let args = json!({"base_revision": 1, "edits": [replace("a", "b")], "client_edit_id": "retry-1"});
    let first = h.ok("edit", args.clone());
    let second = h.ok("edit", args.clone());
    assert_eq!(first["id"], second["id"]);
    assert_eq!(second["replayed"], true);
    assert_eq!(h.core.pending_proposals().len(), 1);
    h.core.resolve_proposal(first["id"].as_str().unwrap(), None).unwrap();
    // Replay after the outcome returns the original outcome, not a stale-version error.
    let third = h.ok("edit", args);
    assert_eq!(third["status"], "accepted");
    assert_eq!(h.api.groups().len(), 1);
}

#[test]
fn proposals_expire() {
    let mut cfg = McpConfig::default();
    cfg.proposal_ttl = Duration::from_millis(100);
    let mut h = harness(cfg, Some(Scope::S2), "a\n");
    let id = h.ok("edit", json!({"base_revision": 1, "edits": [replace("a", "b")]}))["id"].as_str().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(h.err("proposal", json!({"id": id.clone()})), "PROPOSAL_EXPIRED");
    assert_eq!(h.core.resolve_proposal(&id, None).unwrap_err().code, "PROPOSAL_EXPIRED");
    assert_eq!(h.api.text(&h.doc), "a\n");
    assert!(h.api.events().contains(&format!("closed:{id}:expired")));
}

#[test]
fn wait_is_capped_and_wakes_on_decision() {
    let mut cfg = McpConfig::default();
    cfg.max_wait = Duration::from_millis(150);
    let mut h = harness(cfg, Some(Scope::S2), "a\n");
    let id = h.ok("edit", json!({"base_revision": 1, "edits": [replace("a", "b")]}))["id"].as_str().unwrap().to_string();
    // Cap: asks for 45 s, returns "pending" after ~150 ms.
    let t0 = std::time::Instant::now();
    assert_eq!(h.ok("proposal", json!({"id": id.clone(), "action": "wait", "wait_s": 45}))["status"], "pending");
    assert!(t0.elapsed() < Duration::from_secs(5));
    // Wakes early when the user decides on another thread.
    let mut cfg2 = McpConfig::default();
    cfg2.max_wait = Duration::from_secs(20);
    let mut h2 = harness(cfg2, Some(Scope::S2), "a\n");
    let id2 = h2.ok("edit", json!({"base_revision": 1, "edits": [replace("a", "b")]}))["id"].as_str().unwrap().to_string();
    let core = h2.core.clone();
    let id3 = id2.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        core.resolve_proposal(&id3, None).unwrap();
    });
    let t0 = std::time::Instant::now();
    let r = h2.ok("proposal", json!({"id": id2, "action": "wait", "wait_s": 30}));
    t.join().unwrap();
    assert_eq!(r["status"], "accepted");
    assert!(t0.elapsed() < Duration::from_secs(10));
}

#[test]
fn cancel_and_ownership() {
    let mut h = std_h("a\n");
    let id = h.ok("edit", json!({"base_revision": 1, "edits": [replace("a", "b")]}))["id"].as_str().unwrap().to_string();
    // Another client cannot see or cancel it.
    grant(&h.core, "codex", Scope::S2);
    let mut other = Session::new("codex");
    std::mem::swap(&mut h.s, &mut other);
    assert_eq!(h.err("proposal", json!({"id": id.clone(), "action": "cancel"})), "NO_SUCH_PROPOSAL");
    assert!(h.ok("editor_state", json!({}))["pending_proposals"].as_array().unwrap().is_empty());
    std::mem::swap(&mut h.s, &mut other);
    assert_eq!(h.ok("proposal", json!({"id": id.clone(), "action": "cancel"}))["status"], "cancelled");
    assert_eq!(h.core.resolve_proposal(&id, None).unwrap_err().code, "NOT_PENDING");
    assert_eq!(h.api.text(&h.doc), "a\n");
}

#[test]
fn trusted_ceiling_applies_directly_and_stays_undoable() {
    let cfg = McpConfig { policy: PolicyConfig { max_scope: Scope::S3 }, ..Default::default() };
    let mut h = harness(cfg, Some(Scope::S3), "a\n");
    let r = h.ok("edit", json!({"base_revision": 1, "edits": [replace("a", "b")], "summary": "direct"}));
    assert_eq!(r["status"], "applied");
    assert_eq!(h.api.text(&h.doc), "b\n");
    assert_eq!(h.api.groups(), ["AI edit: claude: direct"]);
    assert!(h.api.undo(&h.doc));
    assert_eq!(h.api.text(&h.doc), "a\n");
    // save works only at S3.
    h.ok("save", json!({"doc": h.doc.clone()}));
    assert_eq!(h.api.saved().len(), 1);
}

#[test]
fn write_protected_files_are_not_editable() {
    let mut h = std_h("x\n");
    let git = h.api.add_doc(Some("C:\\proj\\.git\\config"), "[core]\n");
    assert_eq!(h.err("edit", json!({"doc": git, "base_revision": 1, "edits": [replace("core", "evil")]})), "DENYLISTED");
}

// ---------------------------------------------------------------- permissions

#[test]
fn no_call_before_consent_and_the_ui_is_told() {
    let mut h = harness(McpConfig::default(), None, "secret-ish\n");
    for tool in ["editor_state", "read", "search"] {
        assert_eq!(h.err(tool, json!({"query": "x"})), "CONSENT_REQUIRED", "{tool}");
    }
    assert_eq!(h.api.consent_requests().len(), 3);
    assert_eq!(h.api.consent_requests()[0], "claude");
    // After the user allows it, it works.
    grant(&h.core, "claude", Scope::S1);
    h.ok("editor_state", json!({}));
    // Block is sticky and beats everything.
    h.core.block_client("claude");
    assert_eq!(h.err("editor_state", json!({})), "DENIED");
}

#[test]
fn changed_executable_forces_reconsent() {
    let mut h = harness(McpConfig::default(), None, "x\n");
    h.core.grant(Grant { client: "claude".into(), folder: FOLDER.into(), scope: Scope::S1, expires_at: None, exe_hash: Some("aaa".into()) });
    h.s.exe_hash = Some("aaa".into());
    h.ok("editor_state", json!({}));
    h.s.exe_hash = Some("bbb".into());
    assert_eq!(h.err("editor_state", json!({})), "CONSENT_REQUIRED");
}

#[test]
fn grants_expire_with_the_clock() {
    let api = Arc::new(FakeEditor::new());
    api.add_doc(Some(FILE), "x");
    let core = McpCore::new(api, McpConfig::default()).unwrap();
    core.set_clock(|| 1000);
    core.grant(Grant { client: "claude".into(), folder: FOLDER.into(), scope: Scope::S1, expires_at: Some(1001), exe_hash: None });
    let mut s = Session::new("claude");
    let call = |core: &McpCore<FakeEditor>, s: &mut Session| -> bool {
        let r: Value = serde_json::from_str(&core.handle_line(s, &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "editor_state"}}).to_string()).unwrap()).unwrap();
        r["result"]["isError"].as_bool().unwrap()
    };
    assert!(!call(&core, &mut s));
    core.set_clock(|| 1001);
    assert!(call(&core, &mut s));
}

#[test]
fn tool_by_scope_matrix() {
    // (tool, args, minimum scope that is allowed at all)
    let doc_args = |h: &H, extra: Value| {
        let mut a = json!({"doc": h.doc.clone()});
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        a
    };
    for (scope, expect) in [(Scope::S1, [true, true, true, false, false, true, true, false, false, false]), (Scope::S2, [true, true, true, true, true, true, true, true, false, true])] {
        let mut h = harness(McpConfig::default(), Some(scope), "a\ncall(\n");
        h.api.add_command(CommandInfo { id: "view.zoom".into(), caption: "Zoom".into(), mutates: false, exec: false, ai_exposed: true });
        h.api.add_file("C:\\proj\\b.rs", "b");
        let cases: Vec<(&str, Value)> = vec![
            ("editor_state", json!({})),
            ("read", json!({})),
            ("search", json!({"query": "a"})),
            ("edit", json!({"base_revision": 1, "edits": [replace("a", "z")]})),
            ("proposal", json!({"id": "p999", "action": "cancel"})),
            ("open", json!({"path": "C:\\proj\\b.rs"})),
            ("diagnostics", json!({})),
            ("run_command", json!({"id": "view.zoom"})),
            ("save", doc_args(&h, json!({}))),
            ("annotate", doc_args(&h, json!({"line": 1, "kind": "note"}))),
        ];
        for (i, (tool, args)) in cases.into_iter().enumerate() {
            let (is_err, v) = h.call(tool, args);
            let denied = is_err && matches!(v["code"].as_str(), Some("OUT_OF_SCOPE"));
            assert_eq!(!denied, expect[i], "{tool} at {scope:?}: {v}");
        }
    }
}

#[test]
fn run_command_exposes_only_ai_exposed_and_respects_flags() {
    let mut h = std_h("x\n");
    for (id, mutates, exec, exposed) in [("view.zoom", false, false, true), ("edit.sort", true, false, true), ("tools.build", false, true, true), ("secret.cmd", false, false, false)] {
        h.api.add_command(CommandInfo { id: id.into(), caption: id.into(), mutates, exec, ai_exposed: exposed });
    }
    let l = h.ok("run_command", json!({"list": true}));
    let ids: Vec<&str> = l["commands"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["view.zoom", "edit.sort", "tools.build"]);
    assert_eq!(h.err("run_command", json!({"id": "secret.cmd"})), "NO_SUCH_COMMAND");
    assert_eq!(h.err("run_command", json!({"id": "edit.sort"})), "OUT_OF_SCOPE");
    assert_eq!(h.err("run_command", json!({"id": "tools.build"})), "OUT_OF_SCOPE");
    h.ok("run_command", json!({"id": "view.zoom"}));
    assert_eq!(h.api.ran(), ["view.zoom"]);
}

#[test]
fn nothing_writes_without_a_user_action_in_default_mode() {
    // Even a client granted S4 is clamped to S2: no tool call may change text, save, or run a mutating command.
    let mut h = harness(McpConfig::default(), Some(Scope::S4), "one\ntwo\n");
    h.api.add_command(CommandInfo { id: "edit.sort".into(), caption: "Sort".into(), mutates: true, exec: false, ai_exposed: true });
    h.api.add_command(CommandInfo { id: "tools.run".into(), caption: "Run".into(), mutates: false, exec: true, ai_exposed: true });
    let (text, ver) = (h.api.text(&h.doc), h.version());
    let doc = h.doc.clone();
    let calls: Vec<(&str, Value)> = vec![
        ("edit", json!({"base_revision": ver, "edits": [replace("one", "1")]})),
        ("edit", json!({"base_revision": ver, "edits": [replace("one", "1")], "client_edit_id": "x"})),
        ("edit", json!({"base_revision": 0, "edits": [replace("one", "1")]})),
        ("save", json!({"doc": doc.clone(), "force": true})),
        ("run_command", json!({"id": "edit.sort"})),
        ("run_command", json!({"id": "tools.run"})),
        ("annotate", json!({"doc": doc.clone(), "line": 1, "kind": "highlight"})),
        ("open", json!({"path": "C:\\proj\\nope.rs", "focus": true})),
        ("proposal", json!({"id": "p1", "action": "cancel"})),
    ];
    for (tool, args) in calls {
        let _ = h.call(tool, args);
    }
    assert_eq!((h.api.text(&h.doc), h.version()), (text, ver));
    assert!(h.api.saved().is_empty());
    assert!(h.api.ran().is_empty());
    assert!(h.api.groups().is_empty());
}

#[test]
fn kill_switch_stops_everything_and_cancels_proposals() {
    let mut h = std_h("a\n");
    let id = h.ok("edit", json!({"base_revision": 1, "edits": [replace("a", "b")]}))["id"].as_str().unwrap().to_string();
    h.core.kill();
    assert!(h.core.is_killed());
    assert_eq!(h.err("read", json!({})), "KILLED");
    assert_eq!(h.err("editor_state", json!({})), "KILLED");
    assert!(h.core.pending_proposals().is_empty());
    assert!(h.api.events().contains(&format!("closed:{id}:cancelled")));
    assert_eq!(h.core.resolve_proposal(&id, None).unwrap_err().code, "NOT_PENDING");
    assert_eq!(h.api.text(&h.doc), "a\n");
    // The serve loop ends instead of reading further.
    let mut out = Vec::new();
    h.core.serve(&mut h.s, Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec()), &mut out).unwrap();
    assert!(out.is_empty());
    // Re-arming does not restore grants.
    h.core.rearm();
    assert_eq!(h.err("read", json!({})), "CONSENT_REQUIRED");
}

// ---------------------------------------------------------------- audit

#[test]
fn audit_log_records_metadata_only_and_chains() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit.jsonl");
    let cfg = McpConfig { audit_path: Some(path.clone()), ..Default::default() };
    let mut h = harness(cfg, Some(Scope::S2), "CANARY-BODY-TEXT line\n");
    h.ok("read", json!({}));
    h.ok("search", json!({"query": "CANARY-BODY"}));
    let r = h.ok("edit", json!({"base_revision": 1, "edits": [replace("CANARY-BODY-TEXT", "CANARY-NEW-TEXT")], "summary": "CANARY-SUMMARY"}));
    h.core.resolve_proposal(r["id"].as_str().unwrap(), None).unwrap();
    let _ = h.call("read", json!({"path": "C:\\proj\\.env"}));
    h.core.kill();
    let log = std::fs::read_to_string(&path).unwrap();
    assert!(!log.contains("CANARY"), "audit log must never hold content: {log}");
    assert!(log.contains("\"tool\":\"read\"") && log.contains("\"decision\":\"deny\"") && log.contains("DENYLISTED"));
    assert!(log.contains("\"approver\":\"user\"") && log.contains("kill_switch"));
    assert_eq!(verify_chain(&path), Ok(log.lines().count() as u64));
    assert!(log.lines().count() >= 6);
}
