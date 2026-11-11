// Local update server + signed test manifest for tools/smoke/update.ps1. TEST KEY ONLY, loopback only.
//   node update-fixture.mjs gen   --dir D --port P --build N --min M --installer FILE
//   node update-fixture.mjs serve --dir D --port P
// gen writes D/www/updates/latest.json(+.sig), D/www/downloads/Fake-setup.exe and D/pub.hex
// (the test public key, passed to the app through BROUILLON_UPDATE_DEV_PUBKEY).
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import http from "node:http";

const [cmd, ...rest] = process.argv.slice(2);
const flag = (n, d) => { const i = rest.indexOf(n); return i >= 0 ? rest[i + 1] : d; };
const dir = path.resolve(flag("--dir"));
const port = Number(flag("--port", "18741"));

if (cmd === "gen") {
  const seedFile = path.join(dir, "test.seed");
  if (!fs.existsSync(seedFile)) fs.writeFileSync(seedFile, crypto.randomBytes(32).toString("hex"));
  const seed = Buffer.from(fs.readFileSync(seedFile, "utf8").trim(), "hex");
  const priv = crypto.createPrivateKey({ key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]), format: "der", type: "pkcs8" });
  const spki = crypto.createPublicKey(priv).export({ format: "der", type: "spki" });
  const pub = spki.subarray(spki.length - 32);
  fs.writeFileSync(path.join(dir, "pub.hex"), pub.toString("hex"));
  const keyId = crypto.createHash("sha256").update(pub).digest("hex").slice(0, 16);
  const www = path.join(dir, "www");
  fs.mkdirSync(path.join(www, "updates"), { recursive: true });
  fs.mkdirSync(path.join(www, "downloads"), { recursive: true });
  const exe = fs.readFileSync(path.resolve(flag("--installer")));
  fs.writeFileSync(path.join(www, "downloads", "Fake-setup.exe"), exe);
  const now = new Date().toISOString().replace(/\.\d+Z$/, "Z");
  const m = {
    schema: 1, channel: "smoke", version: "0.99.0", build: Number(flag("--build")), build_date_utc: now, published_utc: now,
    min_supported_build: Number(flag("--min", "0")),
    installer: { file: "Fake-setup.exe", url: `http://127.0.0.1:${port}/downloads/Fake-setup.exe`, size_bytes: exe.length, sha256: crypto.createHash("sha256").update(exe).digest("hex") },
    notes_url: "https://brouillon.rochetta.fr/changelog/", notes: "",
  };
  const bytes = Buffer.from(JSON.stringify(m, null, 2) + "\n");
  fs.writeFileSync(path.join(www, "updates", "latest.json"), bytes);
  fs.writeFileSync(path.join(www, "updates", "latest.json.sig"), `brouillon-sig-v1 ${keyId} ${crypto.sign(null, bytes, priv).toString("hex")}\n`);
  console.log(`generated: build ${m.build}, min ${m.min_supported_build}, key ${keyId}`);
} else if (cmd === "serve") {
  const www = path.join(dir, "www");
  http.createServer((req, res) => {
    const rel = decodeURIComponent((req.url || "/").split("?")[0]);
    const f = path.join(www, rel);
    if (rel.includes("..") || !f.startsWith(www) || !fs.existsSync(f) || !fs.statSync(f).isFile()) { res.writeHead(404); res.end("nope"); return; }
    fs.appendFileSync(path.join(dir, "requests.log"), `${req.method} ${rel} UA=${req.headers["user-agent"]}\n`);
    res.writeHead(200); res.end(fs.readFileSync(f));
  }).listen(port, "127.0.0.1", () => console.log(`serving ${www} on 127.0.0.1:${port}`));
} else {
  console.error("usage: gen|serve"); process.exit(2);
}
