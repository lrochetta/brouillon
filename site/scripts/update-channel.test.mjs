// Tests of the signed update channel:  node --test site/scripts/
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { build, BuildError } from "./build.mjs";
import { latestJsonText, verifyUpdateSig, loadTrustedKeys } from "./update-manifest.mjs";

const SIG_TAG = "brouillon-sig-v1";
const keypair = () => {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const der = publicKey.export({ format: "der", type: "spki" });
  const pub = der.subarray(der.length - 32);
  const keyId = crypto.createHash("sha256").update(pub).digest("hex").slice(0, 16);
  return { privateKey, keys: [{ keyId, publicKeyHex: pub.toString("hex") }], keyId };
};
const signText = (kp, bytes) => `${SIG_TAG} ${kp.keyId} ${crypto.sign(null, bytes, kp.privateKey).toString("hex")}\n`;

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "brouillon-upd-"));
  const dl = path.join(root, "downloads");
  const pub = path.join(root, "public");
  fs.mkdirSync(dl);
  fs.mkdirSync(path.join(pub, "updates"), { recursive: true });
  const exe = Buffer.from("MZ fake installer for tests");
  const zip = Buffer.from("fake zip");
  const sha = (b) => crypto.createHash("sha256").update(b).digest("hex");
  fs.writeFileSync(path.join(dl, "Brouillon-test-setup.exe"), exe);
  fs.writeFileSync(path.join(dl, "Brouillon-test.zip"), zip);
  const info = {
    product: "Brouillon", version: "0.0.0-test.1", build: 84, build_date_utc: "2026-11-05T14:32:10Z",
    git_commit: "abc1234", channel: "alpha-test", platform: "windows-x64", min_os: "Windows 10 64-bit",
    artifacts: [
      { kind: "installer", file: "Brouillon-test-setup.exe", size_bytes: exe.length, sha256: sha(exe) },
      { kind: "portable", file: "Brouillon-test.zip", size_bytes: zip.length, sha256: sha(zip) },
    ],
    signed: false, license: "MIT", author: "Laurent Rochetta",
  };
  fs.writeFileSync(path.join(dl, "build-info.json"), JSON.stringify(info, null, 2));
  return { root, dl, pub, info, sigFile: path.join(pub, "updates", "latest.json.sig"), out: path.join(root, "dist") };
}
const run = (f, extra = {}) => build({ downloadsDir: f.dl, publicDir: f.pub, outDir: f.out, updatesSigFile: f.sigFile, ...extra });

test("latest.json is a pure function of build-info (deterministic bytes)", () => {
  const f = fixture();
  const a = latestJsonText(f.info), b = latestJsonText(JSON.parse(JSON.stringify(f.info)));
  assert.equal(a, b);
  const m = JSON.parse(a);
  assert.equal(m.schema, 1);
  assert.equal(m.build, 84);
  assert.equal(m.published_utc, m.build_date_utc);
  assert.equal(m.installer.url, "https://brouillon.rochetta.fr/downloads/Brouillon-test-setup.exe");
  assert.match(m.installer.sha256, /^[0-9a-f]{64}$/);
});

test("no installer artifact: no update channel, build still works (and ships no updates/)", () => {
  const f = fixture();
  f.info.artifacts = f.info.artifacts.filter((a) => a.kind !== "installer");
  fs.writeFileSync(path.join(f.dl, "build-info.json"), JSON.stringify(f.info));
  run(f, { trustedKeys: [] });
  assert.ok(!fs.existsSync(path.join(f.out, "updates", "latest.json")));
});

test("build FAILS when latest.json.sig is absent", () => {
  const f = fixture();
  assert.throws(() => run(f, { trustedKeys: keypair().keys }), (e) => e instanceof BuildError && /signature missing/i.test(e.message));
  assert.ok(!fs.existsSync(f.out), "nothing written");
});

test("build FAILS on a signature that does not verify, a foreign key or garbage", () => {
  const f = fixture();
  const good = keypair(), other = keypair();
  const bytes = Buffer.from(latestJsonText(f.info));
  fs.writeFileSync(f.sigFile, signText(other, bytes));
  assert.throws(() => run(f, { trustedKeys: good.keys }), (e) => e instanceof BuildError && /unknown key/.test(e.message));
  // same key id claimed, wrong signature
  const forged = signText(other, bytes).replace(other.keyId, good.keyId);
  fs.writeFileSync(f.sigFile, forged);
  assert.throws(() => run(f, { trustedKeys: good.keys }), (e) => e instanceof BuildError && /does not verify/.test(e.message));
  fs.writeFileSync(f.sigFile, "garbage");
  assert.throws(() => run(f, { trustedKeys: good.keys }), (e) => e instanceof BuildError && /malformed/.test(e.message));
});

test("a signature made before a build-info change no longer verifies", () => {
  const f = fixture();
  const kp = keypair();
  fs.writeFileSync(f.sigFile, signText(kp, Buffer.from(latestJsonText(f.info))));
  run(f, { trustedKeys: kp.keys }); // ok
  f.info.build = 85;
  fs.writeFileSync(path.join(f.dl, "build-info.json"), JSON.stringify(f.info));
  assert.throws(() => run(f, { trustedKeys: kp.keys }), (e) => e instanceof BuildError && /does not verify/.test(e.message));
});

test("valid signature: dist/updates holds the signed bytes and _headers sets no-store + json", () => {
  const f = fixture();
  const kp = keypair();
  const bytes = Buffer.from(latestJsonText(f.info));
  const sig = signText(kp, bytes);
  fs.writeFileSync(f.sigFile, sig);
  run(f, { trustedKeys: kp.keys });
  assert.deepEqual(fs.readFileSync(path.join(f.out, "updates", "latest.json")), bytes);
  assert.equal(fs.readFileSync(path.join(f.out, "updates", "latest.json.sig"), "utf8"), sig);
  verifyUpdateSig(fs.readFileSync(path.join(f.out, "updates", "latest.json")), sig, kp.keys);
  const h = fs.readFileSync(path.join(f.out, "_headers"), "utf8");
  assert.match(h, /\/updates\/\*\n  Cache-Control: no-store\n  Content-Type: application\/json/);
});

test("the repository trusted-keys.json parses and key ids match the keys", () => {
  const keys = loadTrustedKeys();
  assert.ok(keys.length >= 1);
  for (const k of keys) {
    const id = crypto.createHash("sha256").update(Buffer.from(k.publicKeyHex, "hex")).digest("hex").slice(0, 16);
    assert.equal(k.keyId, id);
  }
});
