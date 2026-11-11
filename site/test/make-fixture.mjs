// Creates a SAMPLE downloads folder (fake artifact + manifest) for local builds/checks.
//   node site/test/make-fixture.mjs && DOWNLOADS_DIR=site/test/fixture-downloads node site/scripts/build.mjs
// This is NOT the real manifest; the real one is site/public/downloads/build-info.json.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";

const dir = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixture-downloads");
fs.mkdirSync(dir, { recursive: true });
const data = Buffer.alloc(2048, "SAMPLE-ARTIFACT-NOT-A-REAL-BUILD");
const file = "Brouillon-SAMPLE-windows-x64.zip";
fs.writeFileSync(path.join(dir, file), data);
fs.writeFileSync(path.join(dir, "build-info.json"), JSON.stringify({
  product: "Brouillon", version: "0.1.0-sample", build: "0", build_date_utc: "2026-11-05T14:32:00Z",
  git_commit: "0000000", channel: "alpha", platform: "Windows x64", min_os: "Windows 10 or 11, 64-bit",
  artifacts: [{ kind: "portable-zip", file, size_bytes: data.length, sha256: crypto.createHash("sha256").update(data).digest("hex") }],
  signed: false, license: "MIT", author: "Laurent Rochetta",
}, null, 2));
console.log("fixture written to", dir);
