#!/usr/bin/env node
// Update channel: latest.json is a PURE function of build-info.json (no clock), so the bytes that
// tools/release signs are exactly the bytes the site build publishes.
//
//   node site/scripts/update-manifest.mjs [--downloads <dir>] [--public <dir>]
//     writes <public>/updates/latest.json (default site/public/updates/); sign it with
//     `brouillon-release sign <that file>` (writes latest.json.sig next to it).
//
// Design: internal design notes (not published)
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SITE = path.resolve(HERE, "..");
/** The app only trusts this host; it is fixed on purpose (previews use another SITE_URL). */
export const UPDATE_SITE_URL = "https://brouillon.rochetta.fr";
export const TRUSTED_KEYS_FILE = path.resolve(SITE, "..", "tools", "release", "trusted-keys.json");
const SIG_TAG = "brouillon-sig-v1";

/** latest.json text, or null when build-info has no installer artifact (no update channel). */
export function latestJsonText(info) {
  const inst = (info.artifacts || []).find((a) => a.kind === "installer");
  if (!inst) return null;
  const m = {
    schema: 1,
    channel: String(info.channel),
    version: String(info.version),
    build: Number(info.build),
    build_date_utc: info.build_date_utc,
    published_utc: info.published_utc || info.build_date_utc,
    min_supported_build: Number(info.min_supported_build || 0),
    installer: {
      file: inst.file,
      url: `${UPDATE_SITE_URL}/downloads/${inst.file}`,
      size_bytes: inst.size_bytes,
      sha256: String(inst.sha256).toLowerCase(),
    },
    notes_url: `${UPDATE_SITE_URL}/changelog/`,
    notes: String(info.notes || ""),
  };
  return JSON.stringify(m, null, 2) + "\n";
}

export function loadTrustedKeys(file = TRUSTED_KEYS_FILE) {
  const j = JSON.parse(fs.readFileSync(file, "utf8"));
  return j.keys.map((k) => ({ keyId: k.key_id, publicKeyHex: k.public_key_hex }));
}

const SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

/** Throws Error(message) unless `sigText` is a valid signature of `bytes` by a trusted key. */
export function verifyUpdateSig(bytes, sigText, keys) {
  const p = String(sigText).trim().split(/\s+/);
  if (p.length !== 3 || p[0] !== SIG_TAG) throw new Error("malformed signature file (expected: brouillon-sig-v1 <key_id> <hex>)");
  if (!/^[0-9a-f]{128}$/i.test(p[2])) throw new Error("malformed signature (not 128 hex characters)");
  const k = keys.find((x) => x.keyId === p[1]);
  if (!k) throw new Error(`signature made by an unknown key_id ${p[1]}`);
  const pub = Buffer.from(k.publicKeyHex, "hex");
  if (pub.length !== 32) throw new Error("trusted key has a wrong length");
  const key = crypto.createPublicKey({ key: Buffer.concat([SPKI_PREFIX, pub]), format: "der", type: "spki" });
  if (!crypto.verify(null, bytes, key, Buffer.from(p[2], "hex"))) throw new Error("signature does not verify");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const arg = (n, d) => { const i = process.argv.indexOf(n); return i > 0 ? process.argv[i + 1] : d; };
  const dl = path.resolve(arg("--downloads", path.join(SITE, "public", "downloads")));
  const pub = path.resolve(arg("--public", path.join(SITE, "public")));
  const info = JSON.parse(fs.readFileSync(path.join(dl, "build-info.json"), "utf8"));
  const text = latestJsonText(info);
  if (text === null) { console.error("build-info.json has no installer artifact: no update manifest written"); process.exit(1); }
  const out = path.join(pub, "updates", "latest.json");
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.writeFileSync(out, text);
  console.log(`wrote ${out}`);
}
