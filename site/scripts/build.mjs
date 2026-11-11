#!/usr/bin/env node
// Static site generator for the Brouillon landing. No dependencies (Node >= 18).
//
//   node site/scripts/build.mjs
//
// Config (environment variables, read ONCE here):
//   SITE_URL   absolute origin, default https://brouillon.rochetta.fr (provisional domain)
//   REPO_URL   public repository URL, default https://github.com/lrochetta/brouillon
//   INDEXABLE  "true" removes noindex meta + X-Robots-Tag; default false
//   DOWNLOADS_DIR  override of site/public/downloads (used by tests)
//   OUT_DIR        override of site/dist
// The download block (version, build, date, SHA-256, size) is read ONLY from build-info.json.

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath, pathToFileURL } from "node:url";
import { C } from "../src/content.mjs";
import { latestJsonText, verifyUpdateSig, loadTrustedKeys } from "./update-manifest.mjs";

export class BuildError extends Error {}

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SITE = path.resolve(HERE, "..");

export const DEFAULTS = {
  siteUrl: process.env.SITE_URL || "https://brouillon.rochetta.fr",
  repoUrl: process.env.REPO_URL || "https://github.com/lrochetta/brouillon",
  indexable: String(process.env.INDEXABLE || "true").toLowerCase() === "true",
  downloadsDir: process.env.DOWNLOADS_DIR || path.join(SITE, "public", "downloads"),
  publicDir: path.join(SITE, "public"),
  srcDir: path.join(SITE, "src"),
  outDir: process.env.OUT_DIR || path.join(SITE, "dist"),
};

// ---------- manifest ----------
const TOP = ["product", "version", "build", "build_date_utc", "git_commit", "channel", "platform", "min_os", "artifacts", "signed", "license", "author"];
const ART = ["kind", "file", "size_bytes", "sha256"];

export function loadManifest(downloadsDir) {
  const mp = path.join(downloadsDir, "build-info.json");
  if (!fs.existsSync(mp)) throw new BuildError(`Manifest missing: ${mp} (expected build-info.json from the release pipeline)`);
  let m;
  try { m = JSON.parse(fs.readFileSync(mp, "utf8")); } catch (e) { throw new BuildError(`Manifest is not valid JSON: ${e.message}`); }
  for (const k of TOP) if (!(k in m) || m[k] === null || m[k] === "") throw new BuildError(`Manifest field missing or empty: "${k}"`);
  if (typeof m.signed !== "boolean") throw new BuildError(`Manifest field "signed" must be a boolean`);
  if (!Array.isArray(m.artifacts) || m.artifacts.length === 0) throw new BuildError(`Manifest "artifacts" must be a non-empty array`);
  if (Number.isNaN(Date.parse(m.build_date_utc))) throw new BuildError(`Manifest "build_date_utc" is not a valid ISO date: ${m.build_date_utc}`);
  m.artifacts.forEach((a, i) => {
    for (const k of ART) if (!(k in a) || a[k] === null || a[k] === "") throw new BuildError(`Manifest artifacts[${i}] field missing or empty: "${k}"`);
    if (!/^[0-9a-f]{64}$/i.test(a.sha256)) throw new BuildError(`Manifest artifacts[${i}].sha256 is not 64 hex characters`);
    if (/[\\/]|\.\./.test(a.file)) throw new BuildError(`Manifest artifacts[${i}].file must be a bare file name: ${a.file}`);
    const fp = path.join(downloadsDir, a.file);
    if (!fs.existsSync(fp)) throw new BuildError(`Artifact file missing: ${fp}`);
    const buf = fs.readFileSync(fp);
    if (buf.length !== a.size_bytes) throw new BuildError(`Artifact size mismatch for ${a.file}: manifest ${a.size_bytes}, file ${buf.length}`);
    const h = crypto.createHash("sha256").update(buf).digest("hex");
    if (h.toLowerCase() !== a.sha256.toLowerCase()) throw new BuildError(`SHA-256 mismatch for ${a.file}: manifest ${a.sha256}, recomputed ${h}`);
  });
  return m;
}

// ---------- helpers ----------
const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const MONTHS = { en: ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"], fr: ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."] };
function fmtDate(iso, lang) {
  const d = new Date(iso);
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getUTCDate()} ${MONTHS[lang][d.getUTCMonth()]} ${d.getUTCFullYear()}, ${p(d.getUTCHours())}:${p(d.getUTCMinutes())} UTC`;
}
const isoDay = (iso) => new Date(iso).toISOString().slice(0, 10);
function fmtSize(b, lang) {
  const mb = b / 1048576;
  const v = mb >= 1 ? mb.toFixed(1) : (b / 1024).toFixed(1);
  const u = mb >= 1 ? (lang === "fr" ? "Mo" : "MB") : (lang === "fr" ? "Ko" : "KB");
  return `${lang === "fr" ? v.replace(".", ",") : v} ${u}`;
}
const JSON_LD = (o) => `<script type="application/ld+json">${JSON.stringify(o).replace(/</g, "\\u003c")}</script>`;

const PAGES = {
  home: { en: "/", fr: "/fr/" },
  download: { en: "/download/", fr: "/fr/telecharger/" },
  privacy: { en: "/privacy/", fr: "/fr/confidentialite/" },
  changelog: { en: "/changelog/", fr: "/fr/journal-des-modifications/" },
};

const LOGO = `<svg width="22" height="22" viewBox="0 0 32 32" aria-hidden="true" focusable="false"><rect x="9" y="5" width="4" height="22" rx="2" fill="var(--accent)"/><rect x="17.5" y="6.5" width="3.5" height="19" rx="1.75" fill="none" stroke="var(--agent)" stroke-width="2"/></svg>`;
// Single source of truth: assets/brand/brouillon-icon.svg (XML comments stripped, one trailing newline).
const FAVICON = fs.readFileSync(path.join(SITE, "..", "assets", "brand", "brouillon-icon.svg"), "utf8").replace(/<!--[\s\S]*?-->/g, "").trim() + "\n";
const FAVICON_LIGHT = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect x=".5" y=".5" width="31" height="31" rx="7" fill="#ffffff" stroke="#80869f"/><rect x="9" y="5" width="4" height="22" rx="2" fill="#2f4fd0"/><rect x="17.5" y="6.5" width="3.5" height="19" rx="1.75" fill="none" stroke="#7a3fd0" stroke-width="2"/></svg>\n`;
const OG_SVG = (siteName) => `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630"><rect width="1200" height="630" fill="#14151c"/><rect x="120" y="170" width="18" height="150" rx="9" fill="#7aa2f7"/><rect x="164" y="178" width="14" height="134" rx="7" fill="none" stroke="#bb9af7" stroke-width="5"/><text x="120" y="440" font-family="Inter,Segoe UI,Arial,sans-serif" font-size="120" font-weight="700" fill="#c0caf5">${siteName}</text><text x="124" y="505" font-family="Inter,Segoe UI,Arial,sans-serif" font-size="40" fill="#9aa3c7">Fast, local-first text editor. Free alpha.</text></svg>\n`;

// ---------- page chrome ----------
function head(ctx, p) {
  const { cfg, css } = ctx;
  const t = C[p.lang];
  const url = (pth) => cfg.siteUrl + pth;
  const canon = url(p.path);
  const alts = p.key && p.key !== "notfound" ? `<link rel="alternate" hreflang="en" href="${url(PAGES[p.key].en)}">\n<link rel="alternate" hreflang="fr" href="${url(PAGES[p.key].fr)}">\n<link rel="alternate" hreflang="x-default" href="${url(PAGES[p.key].en)}">\n` : "";
  const og = url(ctx.ogPath);
  const robots = !cfg.indexable || p.key === "notfound" ? `<meta name="robots" content="${cfg.indexable ? "noindex" : "noindex,follow"}">\n` : "";
  return `<!doctype html>
<html lang="${t.htmlLang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${esc(p.title)}</title>
<meta name="description" content="${esc(p.desc)}">
<meta name="color-scheme" content="dark light">
<meta name="theme-color" content="#14151c" media="(prefers-color-scheme: dark)">
<meta name="theme-color" content="#ffffff" media="(prefers-color-scheme: light)">
${robots}${p.key === "notfound" ? "" : `<link rel="canonical" href="${canon}">\n`}${alts}<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="icon" href="/favicon-light.svg" type="image/svg+xml" media="(prefers-color-scheme: light)">
<link rel="preload" href="/assets/fonts/inter-latin.woff2" as="font" type="font/woff2" crossorigin>
<link rel="preload" href="/assets/fonts/jetbrains-mono-latin.woff2" as="font" type="font/woff2" crossorigin>
<link rel="stylesheet" href="/assets/${css}">
<meta property="og:type" content="website">
<meta property="og:site_name" content="Brouillon">
<meta property="og:url" content="${canon}">
<meta property="og:title" content="${esc(p.title)}">
<meta property="og:description" content="${esc(p.desc)}">
<meta property="og:image" content="${og}">
<meta property="og:image:width" content="1200"><meta property="og:image:height" content="630">
<meta property="og:image:alt" content="Brouillon, a fast local-first text editor (logo and tagline)">
<meta property="og:locale" content="${t.locale}">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="${esc(p.title)}">
<meta name="twitter:description" content="${esc(p.desc)}">
<meta name="twitter:image" content="${og}">
${(p.jsonld || []).map(JSON_LD).join("\n")}
</head>
<body>
`;
}

// Flags are drawn inline (no id/clipPath, so the same flag can appear twice on a page) and
// carry no text: the language name is always printed next to them.
const FLAG_GB = `<svg class="flag" viewBox="0 0 60 30" width="20" height="14" aria-hidden="true" focusable="false"><rect width="60" height="30" fill="#012169"/><path d="M0 0L60 30M60 0L0 30" stroke="#fff" stroke-width="6"/><path d="M0 0L60 30M60 0L0 30" stroke="#C8102E" stroke-width="2.5"/><path d="M30 0V30M0 15H60" stroke="#fff" stroke-width="10"/><path d="M30 0V30M0 15H60" stroke="#C8102E" stroke-width="6"/></svg>`;
const FLAG_FR = `<svg class="flag" viewBox="0 0 30 20" width="20" height="14" aria-hidden="true" focusable="false"><rect width="10" height="20" fill="#0055A4"/><rect x="10" width="10" height="20" fill="#fff"/><rect x="20" width="10" height="20" fill="#EF4135"/></svg>`;
// One entry per published language. English is the default (site root); add a language here
// and in PAGES/C to get it in the menu.
const LANGS = [
  { code: "en", name: "English", short: "EN", flag: FLAG_GB },
  { code: "fr", name: "Français", short: "FR", flag: FLAG_FR },
];
// GitHub mark (Octicons "mark-github", MIT licence), 16x16.
const GITHUB_MARK = `<svg class="ghmark" viewBox="0 0 16 16" width="22" height="22" aria-hidden="true" focusable="false" fill="currentColor"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>`;

function languageMenu(p) {
  const here = LANGS.find((l) => l.code === p.lang) || LANGS[0];
  const label = p.lang === "fr" ? "Langue" : "Language";
  const hrefFor = (code) => (p.key === "notfound" ? (code === "en" ? "/" : `/${code}/`) : PAGES[p.key][code]);
  const items = LANGS.map((l) => `<li><a href="${hrefFor(l.code)}" hreflang="${l.code}" lang="${l.code}"${l.code === p.lang ? ' aria-current="true"' : ""}>${l.flag}<span>${esc(l.name)}</span></a></li>`).join("");
  return `<details class="lang"><summary aria-label="${esc(here.short)} - ${esc(label)}: ${esc(here.name)}">${here.flag}<span>${esc(here.short)}</span></summary><ul>${items}</ul></details>`;
}

function githubLink(ctx, p) {
  const repo = ctx.cfg.repoUrl;
  if (repo) return `<a class="icon-link" href="${esc(repo)}" rel="noopener" aria-label="GitHub">${GITHUB_MARK}</a>`;
  const soon = p.lang === "fr" ? "GitHub : dépôt public bientôt disponible" : "GitHub: public repository coming soon";
  return `<span class="icon-link soon" role="img" aria-label="${esc(soon)}" title="${esc(soon)}">${GITHUB_MARK}</span>`; // TODO: set REPO_URL at build time to turn this into a link
}


// No-JS theme picker: radios + :root:has(...) in style.css. Not persisted (no cookie, no storage): resets to the system theme on navigation.
const ICON = {
  auto: '<circle cx="12" cy="12" r="8"/><path d="M12 4a8 8 0 0 1 0 16z" fill="currentColor"/>',
  light: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
  dark: '<path d="M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z"/>',
};
function themePicker(t) {
  const r = (k) => `<label title="${esc(t.theme[k])}"><input type="radio" name="theme" id="th-${k}" value="${k}"${k === "auto" ? " checked" : ""}><svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">${ICON[k]}</svg><span class="vh">${esc(t.theme[k])}</span></label>`;
  return `<fieldset class="theme"><legend class="vh">${esc(t.theme.label)}</legend>${r("auto")}${r("light")}${r("dark")}</fieldset>`;
}

function header(ctx, p) {
  const t = C[p.lang];
  const L = (k) => PAGES[k][p.lang];
  const cur = (k) => (p.key === k ? ' aria-current="page"' : "");
  return `<a class="skip" href="#main">${esc(t.skip)}</a>
<header class="top"><div class="wrap">
<a class="brand" href="${L("home")}">${LOGO}<span>Brouillon</span></a>
<nav aria-label="Main"><ul>
<li><a href="${L("home")}#features">${esc(t.nav.features)}</a></li>
<li><a href="${L("home")}#agents">${esc(t.nav.agents)}</a></li>
<li><a href="${L("privacy")}"${cur("privacy")}>${esc(t.nav.privacy)}</a></li>
<li><a href="${L("home")}#faq">${esc(t.nav.faq)}</a></li>
<li><a href="${L("changelog")}"${cur("changelog")}>${esc(t.nav.changelog)}</a></li>
</ul></nav>
<span class="grow"></span>
${themePicker(t)}
${languageMenu(p)}
${githubLink(ctx, p)}
<a class="btn sm" href="${L("download")}"${cur("download")}>${esc(t.download)}</a>
</div></header>
<aside class="ribbon" aria-label="${p.lang === "fr" ? "Information sur la version" : "Release information"}"><div class="wrap"><span class="ver"><strong>${p.lang === "fr" ? "Dernière build" : "Latest build"}</strong> ${esc(ctx.man.version)} · build ${esc(ctx.man.build)} · <time datetime="${esc(ctx.man.build_date_utc)}">${esc(fmtDate(ctx.man.build_date_utc, p.lang))}</time></span> <span class="badge b-alpha">Alpha</span> ${esc(t.ribbon)} <a href="${L("download")}">${esc(t.ribbonLink)}</a></div></aside>
<main id="main">
`;
}

function footer(ctx, p) {
  const t = C[p.lang], f = t.footer, L = (k) => PAGES[k][p.lang];
  const repo = ctx.cfg.repoUrl;
  const repoLi = repo ? `<li><a href="${esc(repo)}" rel="noopener">${esc(f.repo)}</a></li>\n<li><a href="${esc(repo.replace(/\/$/, ""))}/issues" rel="noopener">${esc(f.issues)}</a></li>` : `<li>${esc(f.repoSoon)}</li>`; // TODO: set REPO_URL at build time
  return `</main>
<footer><div class="wrap">
<div class="cols">
<div><strong>${esc(f.product)}</strong><ul><li><a href="${L("download")}">${esc(t.download)}</a></li><li><a href="${L("changelog")}">${esc(t.nav.changelog)}</a></li><li><a href="${L("home")}#features">${esc(f.roadmap)}</a></li></ul></div>
<div><strong>${esc(f.project)}</strong><ul>${repoLi}<li>${esc(f.licence)}</li></ul></div>
<div><strong>${esc(f.legal)}</strong><ul><li><a href="${L("privacy")}">${esc(t.nav.privacy)}</a></li><li>${esc(f.tagline)}</li></ul></div>
</div>
<p>&copy; ${new Date().getUTCFullYear()} ${esc(f.copy)}. ${esc(f.tm)}</p>
</div></footer>
</body>
</html>
`;
}

// ---------- components ----------
const badge = (t, k) => `<span class="badge b-${k}">${esc(t.status[k])}</span>`;

function mockup(t) {
  const m = t.mock;
  return `<figure class="win"><div role="img" aria-label="${esc(t.mockAria + " " + t.mockSeq)}"><div class="frame" aria-hidden="true">
<div class="tb"><i></i><i></i><i></i><span>${esc(m.title)}</span></div>
<div class="tabs"><b class="on">main.rs</b><b>lib.rs</b><b>+</b></div>
<div class="bd"><div class="gut"><div>1</div><div>2</div><div>3</div><div>4</div><div>5</div><div>6</div><div>7</div><div>8</div><div>9</div></div>
<div class="code"><div class="c">${esc(m.comment)}</div><div><span class="k">fn</span> total(xs: &amp;[<span class="t">u32</span>]) -&gt; <span class="t">u32</span> {</div><div class="ghl">    xs.iter().sum()<span class="gh"></span><span class="flag">${esc(m.agentFlag)}</span></div><div>}</div><div> </div><div><span class="k">fn</span> main() {</div><div class="cl">    <span class="k">let</span> n = total(&amp;[<span class="n">1</span>, <span class="n">2</span>, <span class="n">3</span>]);<span class="car"></span></div><div>    println!(<span class="s">"{n}"</span>);</div><div>}</div></div>
<div class="ag"><h4>${esc(m.agents)} <span class="badge b-soon">${esc(m.soonPreview)}</span></h4><div>${esc(m.proposes)}</div><div class="anim"><div class="d r">- xs.iter().sum()</div><div class="d a">+ xs.iter().copied().sum::&lt;u32&gt;()</div><div class="bt"><span>${esc(m.apply)}</span><span>${esc(m.reject)}</span></div><div>${esc(m.undo)}</div></div></div></div>
<div class="sb"><span>${esc(m.status)}</span><span><em>${esc(m.chip)}</em> - ${esc(m.local)}</span></div>
</div></div><figcaption>${esc(t.mockCaption)}</figcaption></figure>`;
}

function artifactsTable(t, man, withHash = true) {
  const d = t.dl;
  const rows = man.artifacts.map((a) => `<tr><th scope="row"><a href="/downloads/${encodeURIComponent(a.file)}" download>${esc(a.file)}</a></th><td>${esc(a.kind)}</td><td>${esc(fmtSize(a.size_bytes, t.lang))}</td>${withHash ? `<td><code class="hash">${esc(a.sha256)}</code></td>` : ""}</tr>`).join("\n");
  return `<div class="tw" tabindex="0" role="region" aria-label="${esc(d.files)}"><table><caption>${esc(d.files)}</caption><thead><tr><th scope="col">${esc(d.colFile)}</th><th scope="col">${esc(d.colKind)}</th><th scope="col">${esc(d.colSize)}</th>${withHash ? `<th scope="col">${esc(d.colSha)}</th>` : ""}</tr></thead><tbody>
${rows}
</tbody></table></div>`;
}

function versionMeta(t, man, full) {
  const d = t.dl;
  const items = [[d.version, man.version], [d.build, man.build], [d.date, `${fmtDate(man.build_date_utc, t.lang)}`], [d.platform, man.platform]];
  if (full) items.push([d.channel, man.channel], [d.commit, man.git_commit], [d.minos, man.min_os], [d.signed, man.signed ? d.signedYes : d.unsigned]);
  return `<dl class="meta">${items.map(([k, v]) => `<div><dt>${esc(k)}</dt><dd>${esc(v)}</dd></div>`).join("")}</dl>
<p class="small"><time datetime="${esc(man.build_date_utc)}">${esc(man.build_date_utc)}</time></p>`;
}

function verifyBlock(t, man) {
  const d = t.dl;
  const cmds = man.artifacts.map((a) => `Get-FileHash .\\${a.file} -Algorithm SHA256`).join("\n");
  const alt = man.artifacts.map((a) => `certutil -hashfile ${a.file} SHA256`).join("\n");
  return `<h2>${esc(d.verifyTitle)}</h2>
<p>${esc(d.verifyBody)}</p>
<pre tabindex="0"><code>${esc(cmds)}</code></pre>
<p>${esc(d.verifyAlt)}</p>
<pre tabindex="0"><code>${esc(alt)}</code></pre>
<p>${esc(d.verifyLimit)}</p>`;
}

function crumbs(ctx, p, label) {
  const t = C[p.lang];
  return `<nav class="crumbs" aria-label="${esc(t.crumbs)}"><a href="${PAGES.home[p.lang]}">${esc(t.home)}</a> / <span>${esc(label)}</span></nav>`;
}

function breadcrumbLd(ctx, p, label) {
  const t = C[p.lang];
  return { "@context": "https://schema.org", "@type": "BreadcrumbList", itemListElement: [
    { "@type": "ListItem", position: 1, name: t.home, item: ctx.cfg.siteUrl + PAGES.home[p.lang] },
    { "@type": "ListItem", position: 2, name: label, item: ctx.cfg.siteUrl + p.path },
  ] };
}

function softwareLd(ctx, p) {
  const { cfg, man } = ctx;
  const o = {
    "@context": "https://schema.org", "@type": "SoftwareApplication", "@id": cfg.siteUrl + "/#app",
    name: "Brouillon",
    description: p.lang === "fr" ? "Éditeur de texte de bureau local-first écrit en Rust, en alpha." : "A fast, local-first desktop text editor written in Rust, currently in alpha.",
    applicationCategory: "DeveloperApplication", operatingSystem: man.platform,
    softwareVersion: man.version, dateModified: isoDay(man.build_date_utc),
    downloadUrl: cfg.siteUrl + PAGES.download.en, license: "https://opensource.org/license/mit",
    isAccessibleForFree: true, offers: { "@type": "Offer", price: "0", priceCurrency: "EUR" },
    author: { "@type": "Person", name: man.author }, url: cfg.siteUrl + "/",
  };
  return o;
}

// ---------- pages ----------
function pageHome(ctx, lang) {
  const t = C[lang], { man, cfg } = ctx, L = (k) => PAGES[k][lang];
  const m = t.meta.home;
  const p = { key: "home", lang, path: L("home"), title: m.title, desc: m.desc };
  const sw = softwareLd(ctx, p);
  p.jsonld = [
    { "@context": "https://schema.org", "@graph": [
      { "@type": "Organization", "@id": cfg.siteUrl + "/#org", name: "Brouillon", url: cfg.siteUrl + "/", logo: cfg.siteUrl + "/favicon.svg", founder: { "@type": "Person", name: man.author }, ...(cfg.repoUrl ? { sameAs: [cfg.repoUrl] } : {}) },
      { "@type": "WebSite", "@id": cfg.siteUrl + "/#website", url: cfg.siteUrl + "/", name: "Brouillon", inLanguage: ["en", "fr"], publisher: { "@id": cfg.siteUrl + "/#org" } },
    ] },
    sw,
    { "@context": "https://schema.org", "@type": "FAQPage", mainEntity: t.faq.map(([q, a]) => ({ "@type": "Question", name: q, acceptedAnswer: { "@type": "Answer", text: a } })) },
  ];
  const first = man.artifacts[0];
  const card = (title, body, k) => `<div class="card"><h3>${esc(title)} ${badge(t, k)}</h3><p>${esc(body)}</p></div>`;
  const html = head(ctx, p) + header(ctx, p) + `
<section class="first"><div class="wrap hero">
<div><h1>${esc(t.h1)}</h1>
<p class="lede">${esc(t.lede)}</p>
<p>${esc(t.answer)}</p>
<div class="cta"><a class="btn" href="${L("download")}">${esc(t.dl.btn)} ${esc(man.version)}</a><a class="btn sec" href="#features">${esc(t.ctaSee)}</a></div>
<p class="small">${esc(man.platform)} - ${esc(fmtSize(first.size_bytes, lang))} - ${esc(man.signed ? t.dl.signedYes : t.dl.unsigned)}</p>
<p class="small">${badge(t, "opt")} ${esc(t.agentLine)}</p></div>
${mockup(t)}
</div></section>
<section id="download"><div class="wrap"><div class="dl">
<p class="note" role="note"><strong>Alpha.</strong> ${esc(t.dl.warn)}</p>
${versionMeta(t, man, false)}
${artifactsTable(t, man)}
<p class="small"><a href="${L("download")}">${esc(t.dl.all)}</a> - ${esc(t.dl.license)}</p>
</div></div></section>
<section id="goals"><div class="wrap"><h2>${esc(t.goalsTitle)}</h2><p>${esc(t.goalsIntro)}</p>
<div class="stats">${t.goals.map(([v, l]) => `<div class="stat"><b>${esc(v)}</b><span>${esc(l)}</span> ${badge(t, "goal")}</div>`).join("")}</div>
<h3 class="mt32">${esc(t.measTitle)}</h3><p class="small">${esc(t.measNote)}</p>
<div class="tw" tabindex="0" role="region" aria-label="${esc(t.measTitle)}"><table><thead><tr>${t.measCols.map((c) => `<th scope="col">${esc(c)}</th>`).join("")}</tr></thead><tbody>${t.meas.map(([a, b, c]) => `<tr><th scope="row">${esc(a)}</th><td class="mono">${esc(b)}</td><td>${esc(c)}</td></tr>`).join("")}</tbody></table></div>
</div></section>
<section id="features"><div class="wrap"><h2>${esc(t.featuresTitle)}</h2><p>${esc(t.featuresIntro)}</p>
<h3>${esc(t.availTitle)}</h3><div class="grid">${t.avail.map(([a, b]) => card(a, b, "ok")).join("\n")}</div>
<h3 class="mt40">${esc(t.engineTitle)}</h3><div class="grid">${t.engine.map(([a, b]) => card(a, b, "engine")).join("\n")}</div>
<h3 class="mt40">${esc(t.roadTitle)}</h3><div class="grid">${t.road.map(([k, a, b]) => card(a, b, k)).join("\n")}</div>
</div></section>
<section id="agents"><div class="wrap"><h2>${esc(t.agentsTitle)} ${badge(t, "opt")}</h2>
<ol class="plain">${t.agentsPromises.map(([a, b]) => `<li><strong>${esc(a)}</strong> ${esc(b)}</li>`).join("")}</ol>
<p class="note" role="note">${esc(t.agentsNote)}</p></div></section>
<section id="privacy"><div class="wrap"><div><h2>${esc(t.privacyTitle)}</h2><ul class="plain">${t.privacyBullets.map((b) => `<li>${esc(b)}</li>`).join("")}</ul><p><a href="${L("privacy")}">${esc(t.privacyLink)}</a></p></div></div></section>
<section id="faq"><div class="wrap"><h2>${esc(t.faqTitle)}</h2>
${t.faq.map(([q, a]) => `<details><summary>${esc(q)}</summary><p>${esc(a)}</p></details>`).join("\n")}
</div></section>
` + footer(ctx, p);
  return { p, html };
}

function pageDownload(ctx, lang) {
  const t = C[lang], d = t.dl, { man } = ctx, L = (k) => PAGES[k][lang];
  const m = t.meta.download;
  const p = { key: "download", lang, path: L("download"), title: m.title, desc: m.desc };
  p.jsonld = [breadcrumbLd(ctx, p, d.title), softwareLd(ctx, p)];
  const html = head(ctx, p) + header(ctx, p) + `
<section class="first"><div class="wrap">${crumbs(ctx, p, d.title)}
<h1>${esc(d.h1)}</h1><p class="lede">${esc(d.answer)}</p>
<p class="note" role="note"><strong>Alpha.</strong> ${esc(d.warn)}</p>
<div class="dl">${versionMeta(t, man, true)}
${artifactsTable(t, man)}
<div class="cta">${man.artifacts.map((a) => `<a class="btn" href="/downloads/${encodeURIComponent(a.file)}" download>${esc(d.btn)} ${esc(a.file)}</a>`).join("")}</div>
<p class="small">${esc(d.license)} ${cfgSourceLink(ctx, t)}</p></div>
</div></section>
<section><div class="wrap">${verifyBlock(t, man)}</div></section>
<section><div class="wrap"><h2>${esc(d.ssTitle)}</h2><p>${esc(d.ssWhy)}</p><p>${esc(d.ssHow)}</p>
<h2>${esc(d.reqTitle)}</h2><p>${esc(man.min_os)}. ${esc(d.req)}</p><p>${esc(d.other)}</p></div></section>
` + footer(ctx, p);
  return { p, html };
}
function cfgSourceLink(ctx, t) { return ctx.cfg.repoUrl ? `<a href="${esc(ctx.cfg.repoUrl)}" rel="noopener">${esc(t.dl.source)}</a>` : ""; }

function pagePrivacy(ctx, lang) {
  const t = C[lang], v = t.pv, L = (k) => PAGES[k][lang];
  const m = t.meta.privacy;
  const p = { key: "privacy", lang, path: L("privacy"), title: m.title, desc: m.desc };
  p.jsonld = [breadcrumbLd(ctx, p, v.h1)];
  const html = head(ctx, p) + header(ctx, p) + `
<section class="first"><div class="wrap">${crumbs(ctx, p, v.h1)}
<h1>${esc(v.h1)}</h1><p class="lede">${esc(v.answer)}</p><p class="small">${esc(v.updated)}</p>
${v.sections.map(([h, ps]) => `<h2>${esc(h)}</h2>${ps.map((x) => `<p>${esc(x)}</p>`).join("")}`).join("\n")}
</div></section>
` + footer(ctx, p);
  return { p, html };
}

function pageChangelog(ctx, lang) {
  const t = C[lang], c = t.cl, { man } = ctx, L = (k) => PAGES[k][lang];
  const m = t.meta.changelog;
  const p = { key: "changelog", lang, path: L("changelog"), title: m.title, desc: m.desc };
  p.jsonld = [breadcrumbLd(ctx, p, c.h1)];
  const html = head(ctx, p) + header(ctx, p) + `
<section class="first"><div class="wrap">${crumbs(ctx, p, c.h1)}
<h1>${esc(c.h1)}</h1><p class="lede">${esc(c.answer)}</p>
<article><h2 id="v-${esc(man.version)}">${esc(man.version)} <span class="badge b-alpha">${esc(man.channel)}</span></h2>
<p class="small">${esc(t.dl.build)} ${esc(man.build)} - <time datetime="${esc(man.build_date_utc)}">${esc(fmtDate(man.build_date_utc, lang))}</time> (${esc(man.build_date_utc)}) - ${esc(man.platform)}</p>
<p>${esc(c.intro)}</p>
<h3>${esc(c.works)}</h3><p>${esc(c.worksBody)}</p>
<h3>${esc(c.limits)}</h3><p>${esc(c.limitsBody)}</p>
<p class="small">${esc(c.rename)} <a href="${L("download")}">${esc(t.dl.all)}</a></p></article>
</div></section>
` + footer(ctx, p);
  return { p, html };
}

function pageNotFound(ctx) {
  const t = C.en, f = C.fr;
  const m = t.meta.notfound;
  const p = { key: "notfound", lang: "en", path: "/404.html", title: m.title, desc: m.desc, jsonld: [] };
  const html = head(ctx, p) + header(ctx, p) + `
<section class="first"><div class="wrap"><h1>${esc(t.nf.h1)}</h1><p>${esc(t.nf.body)} / ${esc(f.nf.body)}</p>
<div class="cta"><a class="btn" href="/">${esc(t.nf.home)}</a><a class="btn sec" href="/fr/">${esc(f.nf.home)}</a><a class="btn sec" href="${PAGES.download.en}">${esc(t.download)}</a></div></div></section>
` + footer(ctx, p);
  return { p, html };
}

// ---------- crawl / text files ----------
function robotsTxt(cfg) {
  const bots = ["OAI-SearchBot", "ChatGPT-User", "GPTBot", "Claude-SearchBot", "Claude-User", "ClaudeBot", "PerplexityBot", "Google-Extended", "CCBot", "Applebot-Extended", "Bytespider", "Amazonbot", "meta-externalagent"];
  return `# Brouillon - all crawlers, including AI crawlers, are allowed (project decision).
${cfg.indexable ? "" : "# NOTE: pages are served with noindex (INDEXABLE=false) while the domain is provisional.\n"}User-agent: *
Allow: /
Disallow: /404.html

${bots.map((b) => `User-agent: ${b}\nAllow: /`).join("\n\n")}
${cfg.indexable ? `\nSitemap: ${cfg.siteUrl}/sitemap.xml\n` : "\n# Sitemap intentionally not declared here while the site is not indexable: see /sitemap.xml (not submitted).\n"}`;
}

function sitemapXml(cfg, man) {
  const lm = isoDay(man.build_date_utc);
  const u = (x) => cfg.siteUrl + x;
  const urls = [];
  for (const [k, v] of Object.entries(PAGES)) for (const lang of ["en", "fr"]) {
    urls.push(`  <url>
    <loc>${u(v[lang])}</loc>
    <lastmod>${lm}</lastmod>
    <xhtml:link rel="alternate" hreflang="en" href="${u(v.en)}"/>
    <xhtml:link rel="alternate" hreflang="fr" href="${u(v.fr)}"/>
    <xhtml:link rel="alternate" hreflang="x-default" href="${u(v.en)}"/>
  </url>`);
  }
  return `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">\n${urls.join("\n")}\n</urlset>\n`;
}

function llmsTxt(cfg, man) {
  const e = C.en, u = (x) => cfg.siteUrl + x;
  return `# Brouillon

> Brouillon is a free, open-source (MIT) local-first desktop text editor written in Rust by ${man.author}. Current build: ${man.version} (build ${man.build}, ${man.build_date_utc}), an unsigned alpha test build for ${man.platform}. No account, no telemetry in the editor. Optional AI-agent link (Brouillon Link, MCP) is off by default and asks for consent; remote editing (SFTP, FTPS, FTP) is experimental. macOS and Linux are planned.

Status notes: features are labelled available, engine ready, in progress / roadmap or coming soon. Performance figures are goals unless dated measurements are given. Syntax colouring is limited to 9 languages.

## Product
- [Home](${u("/")}): what Brouillon is, what works today
- [Download](${u(PAGES.download.en)}): version, build, UTC date, SHA-256 of each file
- [Changelog](${u(PAGES.changelog.en)}): release notes
- [Privacy](${u(PAGES.privacy.en)}): what the editor and this site send

## Optional
- [Version francaise](${u("/fr/")})
- [Full text for LLMs](${u("/llms-full.txt")})
${cfg.repoUrl ? `- [Source repository](${cfg.repoUrl})\n` : ""}`;
}

function llmsFull(cfg, man) {
  const e = C.en, u = (x) => cfg.siteUrl + x;
  const list = (arr) => arr.map(([a, b]) => `- ${a}: ${b}`).join("\n");
  return `# Brouillon - full text

> Generated for release ${man.version} (build ${man.build}, ${man.build_date_utc}) from the release manifest.

## Facts
- Name: Brouillon
- Maker: ${man.author}
- Status: alpha, ${man.channel} channel, unsigned test build: ${man.signed ? "signed" : "not signed"}
- Current version: ${man.version}, build ${man.build}, commit ${man.git_commit}
- Build date (UTC): ${man.build_date_utc}
- Platform: ${man.platform} (minimum OS: ${man.min_os}); macOS and Linux not released
- Licence: ${man.license}; price: free, alpha included
- Telemetry: none in the editor; the only own network request is a signed update check at start-up, which can be turned off
- Repository: ${cfg.repoUrl || "public repository, link to be added"}
- Contact: issues of the public repository
- Site: ${u("/")}

## Files
${man.artifacts.map((a) => `- ${a.file} (${a.kind}, ${a.size_bytes} bytes), SHA-256: ${a.sha256}`).join("\n")}

## Answers
${e.faq.map(([q, a]) => `### ${q}\n${a}`).join("\n\n")}

## Available in the test build
${list(e.avail)}

## Engine ready, no screen yet
${list(e.engine)}

## In progress, roadmap and coming soon
${e.road.map(([k, a, b]) => `- ${a} (${e.status[k]}): ${b}`).join("\n")}

## Goals (not achieved claims)
Start time < 100 ms, memory < 80 MB, binary < 30 MB are goals.
${e.meas.map(([a, b, c]) => `- ${a}: ${b} - ${c}`).join("\n")}

`;
}

function headersFile(cfg) {
  const noidx = cfg.indexable ? "" : "  X-Robots-Tag: noindex\n";
  return `/*
  X-Content-Type-Options: nosniff
  Referrer-Policy: no-referrer
  Permissions-Policy: camera=(), microphone=(), geolocation=(), interest-cohort=()
  X-Frame-Options: DENY
  Cross-Origin-Opener-Policy: same-origin
  Strict-Transport-Security: max-age=31536000
  Content-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'
  Cache-Control: public, max-age=0, must-revalidate
${noidx}
/assets/*
  Cache-Control: public, max-age=31536000, immutable

/assets/fonts/*
  Cache-Control: public, max-age=31536000, immutable

/og/*
  Cache-Control: public, max-age=86400

/favicon.svg
  Cache-Control: public, max-age=86400

/downloads/*
  Cache-Control: public, max-age=3600
  X-Content-Type-Options: nosniff

/updates/*
  Cache-Control: no-store
  Content-Type: application/json

/robots.txt
  Cache-Control: public, max-age=3600
  Content-Type: text/plain; charset=utf-8

/llms.txt
  Cache-Control: public, max-age=3600
  Content-Type: text/plain; charset=utf-8

/llms-full.txt
  Cache-Control: public, max-age=3600
  Content-Type: text/plain; charset=utf-8

/sitemap.xml
  Cache-Control: public, max-age=3600
`;
}

const REDIRECTS = `/index.html /  301
/fr/index.html /fr/  301
/fr /fr/  301
/download.html /download/  301
/telecharger /fr/telecharger/  301
/telecharger/ /fr/telecharger/  301
/compare /  301
/compare/ /  301
/comparatif /fr/  301
/fr/comparatif /fr/  301
/fr/comparatif/ /fr/  301
/confidentialite /fr/confidentialite/  301
`;

// ---------- update channel ----------
// latest.json is derived from build-info.json; its Ed25519 signature (made at publication by
// tools/release) must verify against a trusted key, otherwise the build FAILS. No installer
// artifact in build-info means no update channel (e.g. test fixtures).
function updateChannel(man, cfg) {
  const text = latestJsonText(man);
  if (text === null) return null;
  const sigFile = cfg.updatesSigFile || path.join(cfg.publicDir, "updates", "latest.json.sig");
  if (!fs.existsSync(sigFile)) {
    throw new BuildError(`Update signature missing: ${sigFile}. Run: node site/scripts/update-manifest.mjs && brouillon-release sign site/public/updates/latest.json`);
  }
  const sig = fs.readFileSync(sigFile, "utf8");
  const bytes = Buffer.from(text, "utf8");
  try { verifyUpdateSig(bytes, sig, cfg.trustedKeys || loadTrustedKeys()); }
  catch (e) { throw new BuildError(`Update signature invalid (${e.message}). Re-sign latest.json after every build-info change.`); }
  return { bytes, sig };
}

// ---------- main ----------
function copyDir(src, dst, skip = []) {
  fs.mkdirSync(dst, { recursive: true });
  for (const e of fs.readdirSync(src, { withFileTypes: true })) {
    if (skip.includes(e.name)) continue;
    const s = path.join(src, e.name), d = path.join(dst, e.name);
    if (e.isDirectory()) copyDir(s, d); else fs.copyFileSync(s, d);
  }
}
function write(out, rel, data) {
  const f = path.join(out, rel);
  fs.mkdirSync(path.dirname(f), { recursive: true });
  fs.writeFileSync(f, data);
}
const pagePath = (p) => (p === "/404.html" ? "404.html" : p.replace(/^\//, "") + "index.html");

export function build(opts = {}) {
  const cfg = { ...DEFAULTS, ...opts };
  cfg.siteUrl = cfg.siteUrl.replace(/\/+$/, "");
  const man = loadManifest(cfg.downloadsDir); // throws BuildError before anything is written
  const update = updateChannel(man, cfg); // idem: throws unless latest.json.sig verifies
  fs.rmSync(cfg.outDir, { recursive: true, force: true });
  fs.mkdirSync(cfg.outDir, { recursive: true });
  copyDir(cfg.publicDir, cfg.outDir, ["downloads"]);
  copyDir(cfg.downloadsDir, path.join(cfg.outDir, "downloads"));
  if (update) {
    write(cfg.outDir, "updates/latest.json", update.bytes);
    write(cfg.outDir, "updates/latest.json.sig", update.sig);
  }

  const cssSrc = fs.readFileSync(path.join(cfg.srcDir, "style.css"));
  const css = `site.${crypto.createHash("sha256").update(cssSrc).digest("hex").slice(0, 8)}.css`;
  write(cfg.outDir, `assets/${css}`, cssSrc);
  const ogPath = fs.existsSync(path.join(cfg.publicDir, "og", "brouillon-og.png")) ? "/og/brouillon-og.png" : "/og/brouillon-og.svg";
  const ctx = { cfg, man, css, ogPath };

  const out = [];
  for (const lang of ["en", "fr"]) for (const fn of [pageHome, pageDownload, pagePrivacy, pageChangelog]) out.push(fn(ctx, lang));
  out.push(pageNotFound(ctx));
  for (const { p, html } of out) write(cfg.outDir, pagePath(p.path), html);

  write(cfg.outDir, "favicon.svg", FAVICON);
  write(cfg.outDir, "favicon-light.svg", FAVICON_LIGHT);
  if (ogPath.endsWith(".svg")) write(cfg.outDir, "og/brouillon-og.svg", OG_SVG("Brouillon"));
  else write(cfg.outDir, "og/brouillon-og.svg", OG_SVG("Brouillon"));
  write(cfg.outDir, "robots.txt", robotsTxt(cfg));
  write(cfg.outDir, "sitemap.xml", sitemapXml(cfg, man));
  write(cfg.outDir, "llms.txt", llmsTxt(cfg, man));
  write(cfg.outDir, "llms-full.txt", llmsFull(cfg, man));
  write(cfg.outDir, "_headers", headersFile(cfg));
  write(cfg.outDir, "_redirects", REDIRECTS);
  return { pages: out.length, version: man.version, build: man.build, outDir: cfg.outDir, indexable: cfg.indexable, siteUrl: cfg.siteUrl };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const r = build();
    console.log(`OK: ${r.pages} pages, version ${r.version} build ${r.build}, indexable=${r.indexable}, SITE_URL=${r.siteUrl} -> ${r.outDir}`);
  } catch (e) {
    if (e instanceof BuildError) { console.error(`BUILD FAILED: ${e.message}`); process.exit(1); }
    throw e;
  }
}
