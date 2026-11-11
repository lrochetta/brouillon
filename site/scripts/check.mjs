#!/usr/bin/env node
// Post-build checks for site/dist (no dependencies):  node site/scripts/check.mjs [distDir]
// Exit code 1 if any check fails.
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import crypto from "node:crypto";
import { verifyUpdateSig, loadTrustedKeys } from "./update-manifest.mjs";
import { fileURLToPath } from "node:url";
import { FORBIDDEN, ALLOWED as NAME_ALLOW, scanFiles } from "./forbidden.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SITE = path.resolve(HERE, "..");
const DIST = path.resolve(process.argv[2] || path.join(SITE, "dist"));
const SITE_URL = (process.env.SITE_URL || "https://brouillon.rochetta.fr").replace(/\/+$/, "");
const INDEXABLE = String(process.env.INDEXABLE || "true").toLowerCase() === "true";
let failures = 0;
const ok = (m) => console.log(`  ok   ${m}`);
const bad = (m) => { failures++; console.log(`  FAIL ${m}`); };
const section = (s) => console.log(`\n== ${s}`);

function walk(d, out = []) {
  for (const e of fs.readdirSync(d, { withFileTypes: true })) {
    const f = path.join(d, e.name);
    e.isDirectory() ? walk(f, out) : out.push(f);
  }
  return out;
}
const rel = (f) => "/" + path.relative(DIST, f).split(path.sep).join("/");
const all = walk(DIST);
const htmls = all.filter((f) => f.endsWith(".html"));
const read = (f) => fs.readFileSync(f, "utf8");

// 1. HTML well-formedness + alt
section("HTML well-formedness, alt, single h1, lang");
const VOID = new Set(["meta", "link", "br", "hr", "img", "input", "source", "area", "base", "col", "embed", "wbr", "track"]);
const ids = {}; // file -> Set of ids
for (const f of htmls) {
  const src = read(f);
  const stack = [];
  let errs = [];
  const re = /<!--[\s\S]*?-->|<script\b[^>]*>[\s\S]*?<\/script>|<(\/?)([a-zA-Z][a-zA-Z0-9-]*)((?:"[^"]*"|'[^']*'|[^>"'])*)>/g;
  let m;
  while ((m = re.exec(src))) {
    if (!m[2]) continue;
    const name = m[2].toLowerCase();
    if (m[1]) {
      const top = stack.pop();
      if (top !== name) { errs.push(`</${name}> closes <${top}> at ${m.index}`); break; }
    } else {
      if (name === "img" && !/\balt\s*=/.test(m[3])) errs.push("<img> without alt");
      if (!VOID.has(name) && !/\/\s*$/.test(m[3])) stack.push(name);
    }
  }
  if (stack.length) errs.push(`unclosed: ${stack.join(",")}`);
  const h1 = (src.match(/<h1[\s>]/g) || []).length;
  if (h1 !== 1) errs.push(`h1 count ${h1}`);
  if (!/<html lang="(en|fr)">/.test(src)) errs.push("missing html lang");
  if (!/<a class="skip" href="#main">/.test(src) || !/<main id="main">/.test(src)) errs.push("skip link/main landmark");
  const dup = [...src.matchAll(/\sid="([^"]+)"/g)].map((x) => x[1]);
  if (new Set(dup).size !== dup.length) errs.push("duplicate ids");
  ids[f] = new Set(dup);
  errs.length ? bad(`${rel(f)}: ${errs.join("; ")}`) : ok(rel(f));
}

// 2. internal links
section("Internal links and fragments");
let linkCount = 0, linkBad = 0;
function resolve(href, fromFile) {
  const u = href.split("#")[0].split("?")[0];
  let p = u === "" ? fromFile : u.startsWith("/") ? path.join(DIST, decodeURIComponent(u)) : path.join(path.dirname(fromFile), decodeURIComponent(u));
  if (u.endsWith("/") || (fs.existsSync(p) && fs.statSync(p).isDirectory())) p = path.join(p, "index.html");
  return p;
}
for (const f of htmls) {
  const src = read(f);
  for (const m of src.matchAll(/\s(?:href|src)="([^"]*)"/g)) {
    const h = m[1];
    if (/^(https?:|mailto:|data:)/.test(h)) continue;
    linkCount++;
    const target = resolve(h, f);
    if (!fs.existsSync(target)) { linkBad++; bad(`${rel(f)}: unresolved ${h}`); continue; }
    const frag = h.split("#")[1];
    if (frag && target.endsWith(".html")) {
      if (!ids[target]) ids[target] = new Set([...read(target).matchAll(/\sid="([^"]+)"/g)].map((x) => x[1]));
      if (!ids[target].has(frag)) { linkBad++; bad(`${rel(f)}: missing anchor ${h}`); }
    }
  }
}
if (!linkBad) ok(`${linkCount} internal references resolved`);
// _redirects targets
for (const l of read(path.join(DIST, "_redirects")).split("\n").filter(Boolean)) {
  const to = l.split(/\s+/)[1];
  if (!fs.existsSync(resolve(to, DIST + "/x"))) bad(`_redirects target missing: ${to}`);
}

// 3. title / description lengths
section("Title <= 60, description <= 160, hreflang/canonical/robots meta");
for (const f of htmls) {
  const src = read(f);
  const title = (src.match(/<title>([^<]*)<\/title>/) || [])[1] || "";
  const desc = (src.match(/<meta name="description" content="([^"]*)"/) || [])[1] || "";
  const un = (s) => s.replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"');
  const tl = [...un(title)].length, dl = [...un(desc)].length;
  const errs = [];
  if (!tl || tl > 60) errs.push(`title ${tl}`);
  if (!dl || dl > 160) errs.push(`description ${dl}`);
  const is404 = rel(f) === "/404.html";
  if (!is404 && !src.includes(`<link rel="canonical" href="${SITE_URL}`)) errs.push("canonical missing/wrong host");
  if (!is404 && (src.match(/hreflang="/g) || []).length < 3 + 0) errs.push("hreflang missing");
  const hasNoindex = /<meta name="robots" content="noindex/.test(src);
  if (!INDEXABLE && !hasNoindex) errs.push("noindex meta missing");
  if (INDEXABLE && hasNoindex && !is404) errs.push("noindex present but INDEXABLE=true");
  errs.length ? bad(`${rel(f)}: ${errs.join("; ")}`) : ok(`${rel(f)}  title ${tl}, desc ${dl}`);
}
const hdr = read(path.join(DIST, "_headers"));
(INDEXABLE ? !/X-Robots-Tag/.test(hdr) : /X-Robots-Tag: noindex/.test(hdr)) ? ok(`_headers X-Robots-Tag consistent with INDEXABLE=${INDEXABLE}`) : bad("_headers X-Robots-Tag inconsistent");

// 4. JSON-LD
section("JSON-LD validity");
const types = new Set();
for (const f of htmls) {
  for (const m of read(f).matchAll(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/g)) {
    try {
      const o = JSON.parse(m[1]);
      (function t(x) { if (x && typeof x === "object") { if (x["@type"]) types.add(x["@type"]); Object.values(x).forEach(t); } })(o);
      if (!o["@context"]) bad(`${rel(f)}: JSON-LD without @context`);
    } catch (e) { bad(`${rel(f)}: invalid JSON-LD ${e.message}`); }
  }
}
for (const need of ["Organization", "WebSite", "SoftwareApplication", "FAQPage", "BreadcrumbList"]) types.has(need) ? ok(`type ${need} present`) : bad(`type ${need} missing`);
const home = read(path.join(DIST, "index.html"));
/"softwareVersion":"[^"]+"/.test(home) && /"dateModified":"\d{4}-\d\d-\d\d"/.test(home) ? ok("softwareVersion + dateModified present") : bad("softwareVersion/dateModified missing");

// 5. weights
section("Page weight (HTML + shared CSS + favicon; excludes fonts, OG, downloads)");
const css = all.filter((f) => /\/assets\/site\.[0-9a-f]+\.css$/.test(rel(f)))[0];
const gz = (b) => zlib.gzipSync(b).length;
const fav = path.join(DIST, "favicon.svg");
const kb = (n) => (n / 1024).toFixed(1) + " KB";
let maxRaw = 0;
for (const f of htmls) {
  const parts = Buffer.concat([fs.readFileSync(f), fs.readFileSync(css), fs.readFileSync(fav)]);
  maxRaw = Math.max(maxRaw, parts.length);
  console.log(`       ${rel(f).padEnd(40)} raw ${kb(parts.length).padStart(9)}  gzip ${kb(gz(parts)).padStart(8)}`);
}
maxRaw < 150 * 1024 ? ok(`heaviest page ${kb(maxRaw)} raw < 150 KB`) : bad(`heaviest page ${kb(maxRaw)} >= 150 KB`);
const fonts = all.filter((f) => f.includes(`${path.sep}fonts${path.sep}`) && f.endsWith(".woff2")).reduce((s, f) => s + fs.statSync(f).size, 0);
console.log(`       fonts (woff2, separate budget): ${kb(fonts)}`);
for (const f of all.filter((f) => rel(f).startsWith("/og/"))) console.log(`       og: ${rel(f)} ${kb(fs.statSync(f).size)}`);

// 6. contrast of tokens
section("Contrast of design tokens (WCAG 2.x)");
const cssText = fs.readFileSync(css, "utf8");
const li = cssText.indexOf("@media (prefers-color-scheme:light)");
const fi = cssText.indexOf(":root:has(#th-light:checked){");
const tokens = (txt) => Object.fromEntries([...txt.matchAll(/--([a-z0-9-]+):(#[0-9a-fA-F]{6})/g)].map((m) => [m[1], m[2]]));
const dark = tokens(cssText.slice(0, li));
const lightMedia = tokens(cssText.slice(li, fi));
const lightForced = tokens(cssText.slice(fi, cssText.indexOf(":root:has(#th-dark:checked){")));
const light = { ...dark, ...lightForced };
Object.keys(lightForced).length > 20 && JSON.stringify(lightMedia) === JSON.stringify(lightForced) ? ok("light tokens identical in the system-preference and forced-light blocks") : bad("light token blocks differ (prefers-color-scheme vs #th-light)");
/--bg:#ffffff;/.test(cssText.slice(fi)) ? ok("light page background is pure white") : bad("light --bg is not #ffffff");
const lum = (h) => { const c = [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)); return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]; };
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
const PAIRS = [
  ["text", "bg", 4.5], ["text", "surface", 4.5], ["text", "surface-2", 4.5], ["text-muted", "bg", 4.5], ["text-muted", "surface", 4.5], ["text-muted", "surface-2", 4.5],
  ["text-muted", "warn-bg", 4.5], ["text", "warn-bg", 4.5], ["text-muted", "agent-tint", 4.5], ["text", "agent-tint", 4.5],
  ["text-faint", "surface", 4.5], ["text-faint", "bg", 4.5], ["accent", "bg", 4.5], ["accent", "surface", 4.5], ["accent", "surface-2", 4.5], ["accent", "warn-bg", 4.5],
  ["on-accent", "accent", 4.5], ["on-accent", "accent-hover", 4.5], ["agent", "surface", 4.5], ["agent", "surface-2", 4.5], ["agent", "agent-tint", 4.5], ["agent", "bg", 4.5], ["agent", "warn-bg", 4.5],
  ["ok", "bg", 4.5], ["ok", "surface", 4.5], ["ok", "warn-bg", 4.5], ["warn", "bg", 4.5], ["warn", "surface", 4.5], ["warn", "warn-bg", 4.5], ["danger", "surface", 4.5],
  ["diff-add-fg", "diff-add-bg", 4.5], ["diff-del-fg", "diff-del-bg", 4.5],
  ["syn-kw", "surface", 4.5], ["syn-str", "surface", 4.5], ["syn-com", "surface", 4.5], ["syn-type", "surface", 4.5], ["syn-num", "surface", 4.5],
  ["syn-kw", "surface-2", 4.5], ["syn-str", "surface-2", 4.5], ["syn-type", "surface-2", 4.5], ["syn-num", "surface-2", 4.5], // current-line background; comments never sit on it
  ["border-ctrl", "bg", 3], ["accent", "bg", 3],
];
for (const [name, set] of [["dark", dark], ["light", light]]) {
  let worst = 99, fails = [];
  for (const [a, b, min] of PAIRS) {
    const r = ratio(set[a], set[b]);
    worst = Math.min(worst, r);
    if (r < min) fails.push(`${a}/${b} ${r.toFixed(2)} < ${min}`);
  }
  fails.length ? bad(`${name}: ${fails.join("; ")}`) : ok(`${name}: ${PAIRS.length} pairs pass AA (lowest ${worst.toFixed(2)})`);
}

// 6b. theme switcher + meta (no JavaScript)
section("Theme: meta, no-JS picker, favicon");
for (const f of htmls) {
  const src = read(f);
  const errs = [];
  if (!/<meta name="color-scheme" content="dark light">/.test(src)) errs.push("color-scheme meta");
  if (!/<meta name="theme-color" content="#14151c" media="\(prefers-color-scheme: dark\)">/.test(src) || !/<meta name="theme-color" content="#ffffff" media="\(prefers-color-scheme: light\)">/.test(src)) errs.push("theme-color metas");
  const radios = [...src.matchAll(/<input type="radio" name="theme" id="th-(auto|light|dark)"/g)].map((m) => m[1]);
  if (radios.join() !== "auto,light,dark") errs.push("theme radios " + radios.join());
  if (!/<fieldset class="theme"><legend class="vh">[^<]+<\/legend>/.test(src)) errs.push("fieldset/legend");
  if (/<script(?![^>]*ld\+json)/.test(src) || /\son[a-z]+="/i.test(src)) errs.push("script or inline handler");
  if (!/favicon-light\.svg" type="image\/svg\+xml" media="\(prefers-color-scheme: light\)"/.test(src)) errs.push("light favicon");
  errs.length ? bad(`${rel(f)}: ${errs.join("; ")}`) : ok(`${rel(f)} theme markup`);
}
fs.existsSync(path.join(DIST, "favicon-light.svg")) ? ok("favicon-light.svg emitted") : bad("favicon-light.svg missing");
/script-src 'none'/.test(hdr) ? ok("CSP keeps script-src 'none'") : bad("CSP script-src 'none' missing");
const hardcoded = htmls.filter((f) => /style="[^"]*#[0-9a-f]{3,6}/i.test(read(f)));
hardcoded.length ? bad(`hard-coded colours in inline style: ${hardcoded.map(rel).join(", ")}`) : ok("no hard-coded colours in inline styles (SVG uses CSS variables)");

// 7. external requests
section("External references");
const ALLOWED = [/^https:\/\/schema\.org\/?$/, /^https:\/\/opensource\.org\/license\/mit$/, /^http:\/\/www\.w3\.org\/2000\/svg$/, /^http:\/\/www\.sitemaps\.org\/schemas\/sitemap\/0\.9$/, /^http:\/\/www\.w3\.org\/1999\/xhtml$/];
const ext = new Map();
for (const f of all.filter((f) => /\.(html|css|svg|xml|txt|mjs)$/.test(f) && !rel(f).startsWith("/downloads/") && !rel(f).startsWith("/assets/fonts/"))) {
  for (const m of read(f).matchAll(/https?:\/\/[^\s"'<>)\\]+/g)) {
    const u = m[0].replace(/[.,;]+$/, "");
    if (u.startsWith(SITE_URL) || ALLOWED.some((r) => r.test(u))) continue;
    (ext.get(u) || ext.set(u, []).get(u)).push(rel(f));
  }
}
const loaded = [];
for (const f of htmls) for (const m of read(f).matchAll(/\s(?:src|href)="(https?:[^"]+)"/g)) if (!m[1].startsWith(SITE_URL)) loaded.push([rel(f), m[1]]);
const cssExt = [...cssText.matchAll(/url\(\s*["']?(https?:[^)"']+)/g)];
const hasScript = htmls.some((f) => /<script(?![^>]*ld\+json)/.test(read(f)));
const outbound = [...ext.entries()];
cssExt.length || hasScript ? bad("external CSS url() or script tag found") : ok("no external CSS url(), no executable <script> (JSON-LD only)");
loaded.length ? console.log(`       outbound content links: ${loaded.length} (${[...new Set(loaded.map((x) => x[1]))].join(", ")})`) : ok("no outbound http(s) href/src in HTML (REPO_URL unset)");
const unexpected = outbound.filter(([u]) => !loaded.some((l) => l[1] === u));
unexpected.length ? bad(`unexpected URLs: ${unexpected.map(([u, w]) => `${u} in ${w[0]}`).join("; ")}`) : ok("no other http(s):// URL outside SITE_URL and schema/xml namespaces");

// 8. forbidden names (case-insensitive; whole dist + src, scripts, README)
section("Forbidden names");
const scan = [...all.filter((f) => !rel(f).startsWith("/downloads/") && !f.endsWith(".woff2")), ...walk(path.join(SITE, "src")), ...walk(HERE), path.join(SITE, "README.md")].filter((f) => fs.existsSync(f) && /[.](html|css|svg|xml|txt|mjs|md|toml|json)$|_headers$|_redirects$/.test(f));
const hits = scanFiles(scan, read);
hits.length ? bad(`forbidden names: ${hits.map((h) => `${h.names.join("/")} in ${h.file}`).join("; ")}`) : ok(`none of ${FORBIDDEN.length} forbidden names in ${scan.length} files (dist, src, scripts, README; allow-list: ${NAME_ALLOW.join(", ")})`);
if (!scan.some((f) => f.endsWith("forbidden.mjs"))) bad("forbidden-names scan does not cover scripts/forbidden.mjs");

// 9. sitemap hreflang reciprocity
section("Sitemap");
const sm = read(path.join(DIST, "sitemap.xml"));
const locs = [...sm.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
const missing = locs.filter((u) => !fs.existsSync(resolve(u.replace(SITE_URL, "") || "/", DIST + "/x")));
missing.length ? bad(`sitemap URLs without page: ${missing.join(", ")}`) : ok(`${locs.length} URLs, each with page + 3 hreflang`);
(sm.match(/hreflang=/g) || []).length === locs.length * 3 ? ok("hreflang count consistent") : bad("hreflang count mismatch");

// 10. signed update channel
section("Update channel (/updates/latest.json)");
const lj = path.join(DIST, "updates", "latest.json");
if (!fs.existsSync(lj)) {
  ok("no installer artifact: no update channel published");
} else {
  try {
    const bytes = fs.readFileSync(lj);
    verifyUpdateSig(bytes, read(lj + ".sig"), loadTrustedKeys());
    ok("latest.json.sig verifies against the trusted keys");
    const m = JSON.parse(bytes.toString("utf8"));
    const inst = path.join(DIST, "downloads", m.installer.file);
    const sha = crypto.createHash("sha256").update(fs.readFileSync(inst)).digest("hex");
    sha === m.installer.sha256 && fs.statSync(inst).size === m.installer.size_bytes ? ok(`installer ${m.installer.file}: size and SHA-256 match the published file`) : bad("installer in latest.json does not match /downloads");
    m.installer.url === `https://brouillon.rochetta.fr/downloads/${m.installer.file}` ? ok("installer URL is on the update host") : bad(`unexpected installer url ${m.installer.url}`);
    /\/updates\/\*\n  Cache-Control: no-store\n  Content-Type: application\/json/.test(hdr) ? ok("_headers: /updates/* is no-store application/json") : bad("_headers lacks the /updates/* rule");
  } catch (e) {
    bad(`update channel: ${e.message}`);
  }
}

console.log(failures ? `\n${failures} check(s) FAILED` : "\nAll checks passed");
process.exit(failures ? 1 : 0);
