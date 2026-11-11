// node --test packaging/make-manifests.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { generate, parseSums, normalizeBase, PLACEHOLDER_BASE } from "./make-manifests.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const h = (s) => crypto.createHash("sha256").update(s).digest("hex");
const SETUP = "Brouillon-0.1.0-alpha.13-build221-win-x64-setup.exe";
const ZIP = "Brouillon-0.1.0-alpha.13-build221-win-x64-portable.zip";
const info = (extra = {}) => ({
  product: "Brouillon", version: "0.1.0-alpha.13", build: 221, build_date_utc: "2026-11-05T14:32:00Z", git_commit: "abcdef0",
  channel: "alpha-test", platform: "windows-x64", signed: false, license: "MIT", author: "Laurent Rochetta",
  artifacts: [
    { kind: "installer", file: SETUP, size_bytes: 10, sha256: h("setup") },
    { kind: "portable", file: ZIP, size_bytes: 20, sha256: h("zip") },
  ],
  ...extra,
});
const sums = (i = info()) => i.artifacts.map((a) => `${a.sha256}  ${a.file}`).join("\n") + "\n";
const BASE = "https://example.org/dl/";

// Minimal YAML reader for the subset we emit: maps, scalar lists, lists of maps, quoted scalars.
function yaml(text) {
  const lines = text.split("\n").filter((l) => l.trim() && !l.trim().startsWith("#"));
  for (const l of lines) assert.ok(!l.includes("\t"), "tab in YAML");
  let i = 0;
  const scalar = (s) => (s.startsWith('"') ? JSON.parse(s) : s);
  const indentOf = (l) => l.length - l.trimStart().length;
  function block(ind) {
    if (lines[i].trimStart().startsWith("- ")) return list(ind);
    const o = {};
    while (i < lines.length && indentOf(lines[i]) === ind && !lines[i].trimStart().startsWith("- ")) {
      const m = /^([A-Za-z0-9]+):(?: (.*))?$/.exec(lines[i].trim());
      assert.ok(m, `bad YAML line: ${lines[i]}`);
      i++;
      if (m[2] !== undefined) o[m[1]] = scalar(m[2]);
      else o[m[1]] = i < lines.length && indentOf(lines[i]) >= ind && (indentOf(lines[i]) > ind || lines[i].trimStart().startsWith("- ")) ? block(indentOf(lines[i])) : null;
    }
    return o;
  }
  function list(ind) {
    const a = [];
    while (i < lines.length && indentOf(lines[i]) === ind && lines[i].trimStart().startsWith("- ")) {
      const rest = lines[i].trimStart().slice(2);
      if (/^[A-Za-z0-9]+:( |$)/.test(rest)) {
        lines[i] = " ".repeat(ind + 2) + rest; // treat as first key of a map at ind+2
        a.push(block(ind + 2));
      } else { a.push(scalar(rest)); i++; }
    }
    return a;
  }
  const r = block(0);
  assert.equal(i, lines.length, "unparsed YAML tail");
  return r;
}

const REAL = () => generate({ info: info(), sumsText: sums(), baseUrl: BASE });
const wingetDir = "winget/manifests/l/lrochetta/Brouillon/0.1.0-alpha.13/";

test("sums parser and base url handling", () => {
  assert.equal(parseSums(sums()).get(ZIP), h("zip"));
  assert.throws(() => parseSums("nothex  file"));
  assert.equal(normalizeBase("https://x.org/d///").base, "https://x.org/d");
  assert.throws(() => normalizeBase("http://x.org"));
  assert.throws(() => normalizeBase("https://x.org/?a=1"));
  assert.equal(normalizeBase().placeholder, true);
});

test("hash mismatch between build-info and SHA256SUMS is refused", () => {
  const bad = sums().replace(h("zip"), h("other"));
  assert.throws(() => generate({ info: info(), sumsText: bad, baseUrl: BASE }), /disagrees/);
  assert.throws(() => generate({ info: info(), sumsText: "", baseUrl: BASE }), /not listed/);
});

test("winget: three files, schema fields and patterns", () => {
  const f = REAL().files;
  const names = Object.keys(f).filter((k) => k.startsWith("winget/")).sort();
  assert.deepEqual(names, [
    wingetDir + "lrochetta.Brouillon.installer.yaml",
    wingetDir + "lrochetta.Brouillon.locale.en-US.yaml",
    wingetDir + "lrochetta.Brouillon.yaml",
  ]);
  const ver = yaml(f[wingetDir + "lrochetta.Brouillon.yaml"]);
  const ins = yaml(f[wingetDir + "lrochetta.Brouillon.installer.yaml"]);
  const loc = yaml(f[wingetDir + "lrochetta.Brouillon.locale.en-US.yaml"]);
  const idRe = /^[^.\s\\/:*?"<>|\x01-\x1f]{1,32}(\.[^.\s\\/:*?"<>|\x01-\x1f]{1,32}){1,7}$/;
  for (const m of [ver, ins, loc]) {
    assert.match(m.PackageIdentifier, idRe);
    assert.equal(m.PackageIdentifier, "lrochetta.Brouillon");
    assert.equal(m.PackageVersion, "0.1.0-alpha.13");
    assert.equal(m.ManifestVersion, "1.10.0");
  }
  assert.deepEqual([ver.ManifestType, ins.ManifestType, loc.ManifestType], ["version", "installer", "defaultLocale"]);
  assert.equal(ver.DefaultLocale, "en-US");
  assert.equal(loc.PackageLocale, "en-US");
  for (const k of ["Publisher", "PackageName", "License", "ShortDescription"]) assert.ok(loc[k], k);
  assert.equal(ins.InstallerType, "inno");
  assert.equal(ins.Scope, "user");
  assert.match(ins.ReleaseDate, /^\d{4}-\d{2}-\d{2}$/);
  assert.match(ins.MinimumOSVersion, /^\d+(\.\d+){0,3}$/);
  const i0 = ins.Installers[0];
  assert.equal(i0.Architecture, "x64");
  assert.equal(i0.InstallerUrl, `https://example.org/dl/${SETUP}`);
  assert.equal(i0.InstallerSha256, h("setup").toUpperCase());
  assert.match(i0.InstallerSha256, /^[A-F0-9]{64}$/);
  assert.equal(i0.AppsAndFeaturesEntries[0].DisplayVersion, "0.1.0-alpha.13 (build 221)");
  assert.match(i0.ProductCode, /^\{[0-9A-F-]{36}\}_is1$/);
  for (const t of Object.values(f)) assert.ok(!t.includes("NOT SUBMITTABLE"));
});

test("winget ProductCode matches the Inno AppId", () => {
  const iss = fs.readFileSync(path.join(HERE, "windows", "brouillon.iss"), "utf8");
  const appId = /AppId=\{\{([0-9A-F-]{36})\}/.exec(iss)[1];
  const ins = yaml(REAL().files[wingetDir + "lrochetta.Brouillon.installer.yaml"]);
  assert.equal(ins.Installers[0].ProductCode, `{${appId}}_is1`);
});

test("scoop: valid JSON, hash, checkver and autoupdate", () => {
  const j = JSON.parse(REAL().files["scoop/bucket/brouillon.json"]);
  assert.equal(j.version, "0.1.0-alpha.13");
  assert.equal(j.license, "MIT");
  assert.equal(j.architecture["64bit"].url, `https://example.org/dl/${ZIP}`);
  assert.equal(j.architecture["64bit"].hash, h("zip"));
  assert.equal(j.bin, "brouillon.exe");
  assert.equal(j.checkver.url, "https://example.org/dl/build-info.json");
  // the checkver regex must extract version and build from a real PowerShell-style build-info.json
  const re = new RegExp(j.checkver.regex);
  const ps = JSON.stringify(info(), null, 2).replace(/": /g, '":  ');
  const m = re.exec(ps);
  assert.equal(m[1], "0.1.0-alpha.13");
  assert.equal(m.groups.build, "221");
  const au = j.autoupdate.architecture["64bit"].url
    .replace("$version", m[1]).replace("$matchBuild", m.groups.build);
  assert.equal(au, `https://example.org/dl/${ZIP}`);
  // hash regex must find the zip line in SHA256SUMS
  const hre = new RegExp(j.autoupdate.hash.regex.replace("$sha256", "([a-f0-9]{64})").replace("$basename", ZIP.replace(/\./g, "\\.")));
  assert.equal(hre.exec(sums())[1], h("zip"));
  assert.match(j.notes, /not Authenticode-signed/);
});

test("homebrew: draft cask is explicit about missing macOS artifact", () => {
  const rb = REAL().files["homebrew/Casks/brouillon.rb"];
  assert.match(rb, /^# DRAFT/);
  assert.match(rb, /REPLACE_WITH_SHA256_OF_THE_MACOS_DMG/);
  assert.match(rb, /\ncask "brouillon" do\n[\s\S]*\nend\n$/);
  assert.equal((rb.match(/\bdo\b/g) || []).length, (rb.match(/^\s*end$/gm) || []).length);
  const withDmg = info();
  withDmg.artifacts.push({ kind: "installer-macos", file: "Brouillon-0.1.0-alpha.13-macos.dmg", size_bytes: 5, sha256: h("dmg") });
  const rb2 = generate({ info: withDmg, sumsText: sums(withDmg), baseUrl: BASE }).files["homebrew/Casks/brouillon.rb"];
  assert.ok(rb2.includes(h("dmg")));
  assert.ok(!rb2.includes("REPLACE_WITH"));
});

test("without --base-url every URL is the inert placeholder and the output says so", () => {
  const r = generate({ info: info(), sumsText: sums() });
  assert.equal(r.placeholder, true);
  for (const [name, t] of Object.entries(r.files)) {
    assert.ok(t.includes("NOT SUBMITTABLE"), name);
    for (const u of t.match(/https:\/\/[^\s"]+/g) || []) {
      if (/schema|aka\.ms/.test(u)) continue;
      assert.ok(u.startsWith(PLACEHOLDER_BASE) || u.startsWith("https://brouillon.rochetta.fr"), `${name}: ${u}`);
    }
  }
  assert.ok(!JSON.stringify(r.files).includes("example.org"));
});

test("CLI writes the files under --out and never needs the network", () => {
  const dl = fs.mkdtempSync(path.join(os.tmpdir(), "mm-dl-"));
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "mm-out-"));
  fs.writeFileSync(path.join(dl, "build-info.json"), JSON.stringify(info()));
  fs.writeFileSync(path.join(dl, "SHA256SUMS.txt"), sums());
  const r = spawnSync(process.execPath, [path.join(HERE, "make-manifests.mjs"), "--downloads", dl, "--base-url", BASE, "--out", out], { encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  assert.ok(fs.existsSync(path.join(out, wingetDir, "lrochetta.Brouillon.installer.yaml")));
  assert.ok(fs.existsSync(path.join(out, "scoop/bucket/brouillon.json")));
  const bad = spawnSync(process.execPath, [path.join(HERE, "make-manifests.mjs"), "--downloads", dl, "--base-url", "http://insecure.example"], { encoding: "utf8" });
  assert.notEqual(bad.status, 0);
});

test("winget validate accepts the generated manifests (only when winget is available)", (t) => {
  const probe = spawnSync("winget", ["--version"], { encoding: "utf8" });
  if (probe.error || probe.status !== 0) return t.skip("winget not available");
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "mm-wg-"));
  for (const [rel, text] of Object.entries(REAL().files)) {
    if (!rel.startsWith("winget/")) continue;
    const f = path.join(out, rel);
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, text);
  }
  const r = spawnSync("winget", ["validate", "--manifest", path.join(out, wingetDir)], { encoding: "utf8" });
  assert.equal(r.status, 0, r.stdout + r.stderr);
});

test("build-release.ps1: signing is optional and skipped with a SmartScreen warning", () => {
  const ps = fs.readFileSync(path.join(HERE, "build-release.ps1"), "utf8");
  const iss = fs.readFileSync(path.join(HERE, "windows", "brouillon.iss"), "utf8");
  assert.match(ps, /SignCommand/);
  assert.match(ps, /SmartScreen/);
  assert.match(ps, /signed = \$signed/);
  assert.match(iss, /#ifdef Sign\r?\nSignTool=brouillon/);
  assert.match(iss, /PrivilegesRequired=lowest/);
  assert.ok(!/PrivilegesRequiredOverridesAllowed/.test(iss.replace(/;.*$/gm, "")), "per-user must not be overridable");
});
