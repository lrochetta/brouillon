// Automated tests for build.mjs:  node --test site/scripts/
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { build, BuildError, DEFAULTS } from "./build.mjs";
import { findForbidden, scanFiles } from "./forbidden.mjs";

function fixture(mutate) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "brouillon-site-"));
  const dl = path.join(root, "downloads");
  fs.mkdirSync(dl);
  const data = Buffer.from("fake artifact for tests, not a real build");
  fs.writeFileSync(path.join(dl, "Brouillon-test-x64.zip"), data);
  const m = {
    product: "Brouillon", version: "0.0.0-test.1", build: "42", build_date_utc: "2026-11-05T14:32:10Z",
    git_commit: "abc1234", channel: "alpha", platform: "Windows x64", min_os: "Windows 10 64-bit",
    artifacts: [{ kind: "portable-zip", file: "Brouillon-test-x64.zip", size_bytes: data.length, sha256: crypto.createHash("sha256").update(data).digest("hex") }],
    signed: false, license: "MIT", author: "Laurent Rochetta",
  };
  if (mutate) mutate(m, dl);
  if (m !== null) fs.writeFileSync(path.join(dl, "build-info.json"), JSON.stringify(m, null, 2));
  return { root, dl, out: path.join(root, "dist") };
}
const run = (f, extra = {}) => build({ downloadsDir: f.dl, outDir: f.out, ...extra });

test("ok: builds all pages and injects manifest values", () => {
  const f = fixture();
  const r = run(f);
  assert.equal(r.pages, 9);
  const dl = fs.readFileSync(path.join(f.out, "download", "index.html"), "utf8");
  assert.match(dl, /0\.0\.0-test\.1/);
  assert.match(dl, /2026-11-05T14:32:10Z/);
  assert.match(dl, /Get-FileHash \.\\Brouillon-test-x64\.zip -Algorithm SHA256/);
  const sha = JSON.parse(fs.readFileSync(path.join(f.dl, "build-info.json"), "utf8")).artifacts[0].sha256;
  assert.ok(dl.includes(sha));
  assert.ok(fs.existsSync(path.join(f.out, "downloads", "Brouillon-test-x64.zip")));
});

test("indexing flag: indexable by default, noindex with indexable=false", () => {
  const f = fixture();
  run(f);
  assert.doesNotMatch(fs.readFileSync(path.join(f.out, "index.html"), "utf8"), /name="robots"/);
  assert.doesNotMatch(fs.readFileSync(path.join(f.out, "_headers"), "utf8"), /X-Robots-Tag/);
  assert.match(fs.readFileSync(path.join(f.out, "robots.txt"), "utf8"), /Sitemap: /);
  run(f, { indexable: false });
  assert.match(fs.readFileSync(path.join(f.out, "index.html"), "utf8"), /<meta name="robots" content="noindex,follow">/);
  assert.match(fs.readFileSync(path.join(f.out, "_headers"), "utf8"), /X-Robots-Tag: noindex/);
});

test("SITE_URL is the single source of absolute URLs", () => {
  const f = fixture();
  run(f, { siteUrl: "https://example.test/" });
  const html = fs.readFileSync(path.join(f.out, "index.html"), "utf8");
  assert.match(html, /rel="canonical" href="https:\/\/example\.test\/"/);
  assert.ok(!html.includes(DEFAULTS.siteUrl) || DEFAULTS.siteUrl === "https://example.test");
});

test("fails: manifest missing", () => {
  const f = fixture((m) => { return; });
  fs.rmSync(path.join(f.dl, "build-info.json"));
  assert.throws(() => run(f), (e) => e instanceof BuildError && /Manifest missing/.test(e.message));
});

test("fails: wrong SHA-256", () => {
  const f = fixture((m) => { m.artifacts[0].sha256 = "0".repeat(64); });
  assert.throws(() => run(f), (e) => e instanceof BuildError && /SHA-256 mismatch/.test(e.message));
});

test("fails: required field absent", () => {
  const f = fixture((m) => { delete m.build; });
  assert.throws(() => run(f), (e) => e instanceof BuildError && /"build"/.test(e.message));
});

test("fails: artifact field absent", () => {
  const f = fixture((m) => { delete m.artifacts[0].size_bytes; });
  assert.throws(() => run(f), (e) => e instanceof BuildError && /size_bytes/.test(e.message));
});

test("fails: artifact file missing", () => {
  const f = fixture((m, dl) => { fs.rmSync(path.join(dl, m.artifacts[0].file)); });
  assert.throws(() => run(f), (e) => e instanceof BuildError && /Artifact file missing/.test(e.message));
});

test("theme: no-JS picker, dual theme-color, light favicon, no script", () => {
  const f = fixture();
  run(f);
  for (const p of ["index.html", "fr/index.html", "download/index.html"]) {
    const html = fs.readFileSync(path.join(f.out, p), "utf8");
    assert.equal((html.match(/<input type="radio" name="theme"/g) || []).length, 3);
    assert.match(html, /<meta name="theme-color" content="#ffffff" media="\(prefers-color-scheme: light\)">/);
    assert.match(html, /<meta name="theme-color" content="#14151c" media="\(prefers-color-scheme: dark\)">/);
    assert.doesNotMatch(html, /<script(?![^>]*ld\+json)/);
  }
  assert.match(fs.readFileSync(path.join(f.out, "fr", "index.html"), "utf8"), /Thème sombre/);
  assert.ok(fs.existsSync(path.join(f.out, "favicon-light.svg")));
  const css = fs.readdirSync(path.join(f.out, "assets")).find((n) => n.endsWith(".css"));
  assert.match(fs.readFileSync(path.join(f.out, "assets", css), "utf8"), /:root:has\(#th-light:checked\)\{color-scheme:light/);
});

test("fails: size mismatch", () => {
  const f = fixture((m) => { m.artifacts[0].size_bytes += 1; });
  assert.throws(() => run(f), (e) => e instanceof BuildError && /size mismatch/.test(e.message));
});

test("no comparison page: not built, redirected to home, absent from sitemap", () => {
  const f = fixture();
  run(f);
  assert.ok(!fs.existsSync(path.join(f.out, "compare")));
  assert.ok(!fs.existsSync(path.join(f.out, "fr", "comparatif")));
  const sm = fs.readFileSync(path.join(f.out, "sitemap.xml"), "utf8");
  assert.doesNotMatch(sm, /compar/i);
  const red = fs.readFileSync(path.join(f.out, "_redirects"), "utf8");
  assert.match(red, new RegExp("^/compare/ / {2}301$", "m"));
  assert.match(red, new RegExp("^/fr/comparatif/ /fr/ {2}301$", "m"));
});

test("forbidden names: detection is case-insensitive and the allow-list is explicit", () => {
  const bad = ["Edit" + "Plus", "edit" + "plus", "Edit " + "Plus", "Notepad" + "++", "Notepad plus " + "plus", "SUB" + "LIME text", "Edit" + "Pad Lite", "Scin" + "tilla", "lex" + "illa", "x." + "STX", "Reverse " + "Engineering", "Agent" + "Pad"];
  for (const t of bad) assert.ok(findForbidden("see " + t + " here").length > 0, t);
  assert.deepEqual(findForbidden("Fonts: Inter and JetBrains Mono"), []);
  assert.ok(findForbidden("JetBrains Mono and " + "Edit" + "Plus").length > 0);
});

test("forbidden names: the generated site and the sources are clean; a dirty file fails the scan", () => {
  const f = fixture();
  run(f);
  const files = [];
  (function walk(d) { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); if (e.isDirectory()) { if (e.name !== "downloads" && e.name !== "fonts") walk(p); } else files.push(p); } })(f.out);
  const here = path.dirname(fileURLToPath(import.meta.url));
  for (const x of ["build.mjs", "check.mjs", "forbidden.mjs"]) files.push(path.join(here, x));
  files.push(path.join(here, "..", "src", "content.mjs"));
  const read = (p) => fs.readFileSync(p, "utf8");
  assert.deepEqual(scanFiles(files, read), []);
  const dirty = path.join(f.root, "dirty.txt");
  fs.writeFileSync(dirty, "inspired by " + "Edit" + "Plus");
  const hits = scanFiles([...files, dirty], read);
  assert.equal(hits.length, 1);
  assert.equal(hits[0].file, dirty);
});
