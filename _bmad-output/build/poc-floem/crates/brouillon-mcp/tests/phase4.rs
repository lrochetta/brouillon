//! Phase 4: save, annotate, rate limits, STALE_VERSION ranges, proposal rebase, consent binding.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
    // Time runs 1 s per reading so ordinary tests are never rate limited.
    let t = AtomicU64::new(0);
    core.set_ms_clock(move || t.fetch_add(1000, Ordering::SeqCst));
    if let Some(sc) = scope {
        grant(&core, "claude", sc);
    }
    H { core, api, s: Session::new("claude"), doc }
}

fn std_h(text: &str) -> H {
    harness(McpConfig::default(), Some(Scope::S2), text)
}

fn s3_cfg() -> McpConfig {
    McpConfig { policy: PolicyConfig { max_scope: Scope::S3 }, ..McpConfig::default() }
}

impl H {
    fn call(&mut self, tool: &str, args: Value) -> (bool, Value) {
        let v = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}});
        let line = self.core.handle_line(&mut self.s, &v.to_string()).expect("a reply");
        let r: Value = serde_json::from_str(&line).unwrap();
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

#[test]
fn save_needs_the_explicit_s3_ceiling_and_a_document_the_client_may_access() {
    // Default ceiling is S2: even a client granted S3 cannot save.
    let mut h = harness(McpConfig::default(), Some(Scope::S3), "a\n");
    let d = h.doc.clone();
    assert_eq!(h.err("save", json!({"doc": d})), "OUT_OF_SCOPE");
    assert!(h.api.saved().is_empty());

    // With the ceiling raised and an S3 grant it works, for that one document.
    let mut h = harness(s3_cfg(), Some(Scope::S3), "a\n");
    let d = h.doc.clone();
    let r = h.ok("save", json!({"doc": d}));
    assert_eq!(r["saved"], true);
    assert_eq!(h.api.saved(), [d.clone()]);
    // A client with only S2 on the folder is still refused when the ceiling is S3.
    let mut h2 = harness(s3_cfg(), Some(Scope::S2), "a\n");
    let d2 = h2.doc.clone();
    assert_eq!(h2.err("save", json!({"doc": d2})), "OUT_OF_SCOPE");

    // A document outside the granted folder is not reachable.
    let other = h.api.add_doc(Some("C:\\elsewhere\\x.rs"), "x");
    assert_eq!(h.err("save", json!({"doc": other.clone()})), "OUT_OF_SCOPE");
    assert!(!h.api.saved().contains(&other));
    // Unsaved buffers have no file; version-control files are write protected; secrets are denylisted.
    let scratch = h.api.add_doc(None, "s");
    assert_eq!(h.err("save", json!({"doc": scratch})), "INVALID_ARGUMENT");
    let git = h.api.add_doc(Some("C:\\proj\\.git\\config"), "g");
    assert_eq!(h.err("save", json!({"doc": git})), "DENYLISTED");
    let secret = h.api.add_doc(Some("C:\\proj\\.env"), "K=1");
    assert_eq!(h.err("save", json!({"doc": secret})), "DENYLISTED");
    assert_eq!(h.api.saved().len(), 1);
}

#[test]
fn save_is_refused_after_an_external_change_unless_forced_and_after_user_typing_with_base_revision() {
    let mut h = harness(s3_cfg(), Some(Scope::S3), "a\n");
    let d = h.doc.clone();
    h.api.set_save_conflict(true);
    assert_eq!(h.err("save", json!({"doc": d.clone()})), "FILE_CHANGED_ON_DISK");
    assert!(h.api.saved().is_empty());
    let r = h.ok("save", json!({"doc": d.clone(), "force": true}));
    assert_eq!(r["forced"], true);
    assert_eq!(h.api.saved().len(), 1);
    h.api.set_save_conflict(false);
    let v = h.version();
    h.api.human_edit(&d, "a, typed\n");
    assert_eq!(h.err("save", json!({"doc": d.clone(), "base_revision": v})), "STALE_VERSION");
    assert_eq!(h.api.saved().len(), 1);
    h.ok("save", json!({"doc": d, "base_revision": v + 1}));
}

#[test]
fn annotate_is_owned_visible_removable_and_never_touches_the_text() {
    let mut h = std_h("one\ntwo\n");
    let d = h.doc.clone();
    let before = (h.api.text(&d), h.version());
    h.ok("annotate", json!({"doc": d.clone(), "line": 2, "kind": "note", "text": "check\u{202E}this"}));
    assert_eq!(h.api.annotations()[0].3, "check<U+202E>this", "bidi controls are made visible");
    assert_eq!(h.api.annotation_owners(), ["claude"]);
    assert_eq!((h.api.text(&d), h.version()), before);
    // Another client cannot remove it; the owner can.
    let mut other = Session::new("codex");
    grant(&h.core, "codex", Scope::S2);
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "annotate", "arguments": {"doc": d.clone(), "remove": true}}});
    let line = h.core.handle_line(&mut other, &req.to_string()).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["result"]["structuredContent"]["removed"], 0);
    assert_eq!(h.api.annotations().len(), 1);
    let r = h.ok("annotate", json!({"doc": d.clone(), "remove": true, "line": 2}));
    assert_eq!(r["removed"], 1);
    assert!(h.api.annotations().is_empty());
    assert_eq!(h.err("annotate", json!({"doc": d, "kind": "note"})), "INVALID_ARGUMENT", "line is required when adding");
    // Needs S2: a read-only client cannot annotate.
    let mut ro = harness(McpConfig::default(), Some(Scope::S1), "x\n");
    let rd = ro.doc.clone();
    assert_eq!(ro.err("annotate", json!({"doc": rd, "line": 1, "kind": "note"})), "OUT_OF_SCOPE");
}

#[test]
fn a_flood_is_rate_limited_and_repeated_refusals_open_the_circuit_breaker() {
    let mut h = harness(McpConfig::default(), Some(Scope::S1), "a\n");
    h.core.set_ms_clock(|| 0);
    let (mut limited, mut ok) = (0, 0);
    for _ in 0..100 {
        let (e, v) = h.call("editor_state", json!({}));
        if e {
            assert_eq!(v["code"], "RATE_LIMITED");
            assert_eq!(v["retryable"], true);
            limited += 1;
        } else {
            ok += 1;
        }
    }
    assert_eq!(ok, 30, "burst");
    assert_eq!(limited, 70);
    // Time passes: calls work again.
    h.core.set_ms_clock(|| 60_000);
    h.ok("editor_state", json!({}));

    // Eight refused calls in a row pause the client (and tell the user).
    let mut h = harness(McpConfig::default(), Some(Scope::S1), "a\n");
    let t = AtomicU64::new(0);
    h.core.set_ms_clock(move || t.fetch_add(250, Ordering::SeqCst));
    for _ in 0..8 {
        assert_eq!(h.err("edit", json!({"base_revision": 1, "edits": [replace("a", "b")]})), "OUT_OF_SCOPE");
    }
    assert_eq!(h.err("editor_state", json!({})), "PAUSED");
    assert!(h.api.notices().iter().any(|n| n.contains("paused")));
    h.core.set_ms_clock(|| 40_000);
    h.ok("editor_state", json!({}));
}

#[test]
fn stale_version_reports_the_changed_ranges_the_client_has_to_re_read() {
    let mut h = std_h("l1\nl2\nl3\nl4\nl5\n");
    let d = h.doc.clone();
    let v = h.ok("read", json!({}))["version"].as_u64().unwrap();
    h.api.human_edit(&d, "l1\nL2 typed\nl3\nl4\nl5\nl6\n");
    let (is_err, e) = h.call("edit", json!({"base_revision": v, "edits": [replace("l3", "X")]}));
    assert!(is_err);
    assert_eq!(e["code"], "STALE_VERSION");
    assert_eq!(e["data"]["changed_ranges"], json!([{"start_line": 2, "end_line": 2, "kind": "changed"}, {"start_line": 6, "end_line": 6, "kind": "added"}]));
    assert_eq!(e["data"]["current_revision"], v + 1);
    // A base the server never showed has no ranges: null, not a guess.
    let (_, e2) = h.call("edit", json!({"base_revision": 0, "edits": [replace("l3", "X")]}));
    assert_eq!(e2["data"]["changed_ranges"], Value::Null);
}

#[test]
fn a_proposal_is_rebased_over_typing_elsewhere_and_goes_stale_when_the_typing_touches_it() {
    let text = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
    let mut h = std_h(text);
    let d = h.doc.clone();
    let id = h.ok("edit", json!({"base_revision": 1, "edits": [replace("gamma", "GAMMA")], "summary": "caps"}))["id"].as_str().unwrap().to_string();
    // The user types a new first line and at the very end: far from the hunk.
    h.api.human_edit(&d, "intro\nalpha\nbeta\ngamma\ndelta\nepsilon\nouttro\n");
    let out = h.core.resolve_proposal(&id, None).unwrap();
    assert_eq!(out.status, "accepted");
    assert_eq!(h.api.text(&d), "intro\nalpha\nbeta\nGAMMA\ndelta\nepsilon\nouttro\n");
    assert_eq!(h.api.groups(), ["AI edit: claude: caps"], "still ONE undo group");

    // Typing on the hunk's own line, or on the line right next to it: stale, nothing applied.
    for typed in ["alpha\nbeta\ngamma!\ndelta\nepsilon\n", "alpha\nbeta!\ngamma\ndelta\nepsilon\n", "alpha\nbeta\ngamma\ndelta!\nepsilon\n"] {
        let mut h = std_h(text);
        let d = h.doc.clone();
        let id = h.ok("edit", json!({"base_revision": 1, "edits": [replace("gamma", "GAMMA")]}))["id"].as_str().unwrap().to_string();
        h.api.human_edit(&d, typed);
        assert_eq!(h.core.resolve_proposal(&id, None).unwrap_err().code, "STALE_VERSION", "{typed:?}");
        assert_eq!(h.api.text(&d), typed);
        assert!(h.api.groups().is_empty());
    }
}

#[test]
fn consent_binds_the_program_hash_and_a_changed_program_is_asked_again_without_stale_grants() {
    let mut h = harness(McpConfig::default(), None, "x\n");
    h.s.exe_hash = Some("aaa".into());
    h.s.exe_path = Some("C:\\tools\\claude.exe".into());
    assert_eq!(h.err("editor_state", json!({})), "CONSENT_REQUIRED");
    let r = &h.api.consent_reqs()[0];
    assert_eq!((r.exe_hash.as_deref(), r.exe_path.as_deref(), r.exe_changed), (Some("aaa"), Some("C:\\tools\\claude.exe"), false));
    h.core.grant(Grant { client: "claude".into(), folder: FOLDER.into(), scope: Scope::S1, expires_at: None, exe_hash: Some("aaa".into()) });
    h.ok("editor_state", json!({}));
    // The program changed.
    h.s.exe_hash = Some("bbb".into());
    assert_eq!(h.err("editor_state", json!({})), "CONSENT_REQUIRED");
    assert!(h.api.consent_reqs().last().unwrap().exe_changed);
    // A client that stops reporting its program is not trusted on the old grant either.
    h.s.exe_hash = None;
    assert_eq!(h.err("editor_state", json!({})), "CONSENT_REQUIRED");
    // The user allows the new program: the new grant replaces the old one (no stale hash left).
    h.s.exe_hash = Some("bbb".into());
    h.core.grant(Grant { client: "claude".into(), folder: FOLDER.into(), scope: Scope::S1, expires_at: None, exe_hash: Some("bbb".into()) });
    h.ok("editor_state", json!({}));
}

#[test]
fn a_folder_opened_after_consent_is_asked_about_once_and_nothing_else_is_ever_prompted() {
    let mut h = harness(McpConfig::default(), Some(Scope::S1), "x\n");
    h.api.add_folder("C:\\proj2");
    h.api.add_doc(Some("C:\\proj2\\b.rs"), "b\n");
    h.api.add_file("C:\\proj2\\b.rs", "b\n");
    let before = h.api.consent_reqs().len();
    // The new folder is invisible and the first touch prompts the user.
    assert_eq!(h.call("read", json!({"path": "C:\\proj2\\b.rs"})).1["code"], "OUT_OF_SCOPE");
    let reqs = h.api.consent_reqs();
    assert_eq!(reqs.len(), before + 1);
    assert_eq!(reqs.last().unwrap().folder.as_deref(), Some("C:\\proj2"));
    // Asking again does not stack a second prompt.
    h.call("read", json!({"path": "C:\\proj2\\b.rs"}));
    assert_eq!(h.api.consent_reqs().len(), before + 1);
    // A path that is in no open folder is never prompted.
    h.call("read", json!({"path": "C:\\nowhere\\z.rs"}));
    assert_eq!(h.api.consent_reqs().len(), before + 1);
    // Once the user allows it, it reads.
    h.core.grant(Grant { client: "claude".into(), folder: "C:\\proj2".into(), scope: Scope::S1, expires_at: None, exe_hash: None });
    assert!(h.ok("read", json!({"path": "C:\\proj2\\b.rs"}))["content"].as_str().unwrap().contains('b'));
    // The first folder is still granted (the second grant did not replace it).
    h.ok("read", json!({}));
    // Denying a folder wins over everything for that folder.
    let mut h = harness(McpConfig::default(), Some(Scope::S1), "x\n");
    h.api.add_folder("C:\\proj3");
    h.api.add_file("C:\\proj3\\c.rs", "c\n");
    h.call("read", json!({"path": "C:\\proj3\\c.rs"}));
    h.core.grant(Grant { client: "claude".into(), folder: "C:\\proj3".into(), scope: Scope::S0, expires_at: None, exe_hash: None });
    assert_eq!(h.err("read", json!({"path": "C:\\proj3\\c.rs"})), "DENIED");
    h.ok("read", json!({}));
}

#[test]
fn activity_counters_feed_the_auto_off_timer() {
    let h = std_h("x\n");
    assert_eq!(h.core.activity().0, 0);
    h.core.conn_open();
    h.core.conn_open();
    assert_eq!(h.core.activity().0, 2);
    h.core.conn_close();
    h.core.conn_close();
    h.core.conn_close();
    assert_eq!(h.core.activity().0, 0, "never below zero");
}
