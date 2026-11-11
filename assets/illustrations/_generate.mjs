// Generates every SVG + illustrations.css + index.html. Run: node _generate.mjs
// Source of truth for the illustrations; the .svg files are build outputs.
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
const OUT = dirname(fileURLToPath(import.meta.url));

// ---------- shared CSS (also embedded in each SVG for <img> use) ----------
const dark = { bg: "#14151c", sf: "#1a1b26", s2: "#222436", tx: "#c0caf5", mu: "#9aa3c7", fa: "#7b84a8", ac: "#7aa2f7", ag: "#bb9af7", at: "#262238", ok: "#9ece6a", wn: "#e0af68", dg: "#f7768e", ln: "#2b2e40", ct: "#5a6184" };
const light = { bg: "#fbfaf7", sf: "#f1f0eb", s2: "#e8e6df", tx: "#1a1b26", mu: "#4a4f6a", fa: "#5c6283", ac: "#2f4fd0", ag: "#7a3fd0", at: "#efe8fb", ok: "#3d6b12", wn: "#8a5a00", dg: "#b3203f", ln: "#dcdad2", ct: "#8a8fa6" };
const tok = { bg: "--bg", sf: "--surface", s2: "--surface-2", tx: "--text", mu: "--text-muted", fa: "--text-faint", ac: "--accent", ag: "--agent", at: "--agent-tint", ok: "--ok", wn: "--warn", dg: "--danger", ln: "--border", ct: "--border-ctrl" };
const vars = (p) => Object.keys(tok).map((k) => `--i-${k}:var(${tok[k]},${p[k]})`).join(";");
const CSS =
  `.il{${vars(dark)}}` +
  `@media(prefers-color-scheme:light){.il{${vars(light)}}}` +
  Object.keys(tok).map((k) => `.f-${k}{fill:var(--i-${k})}.s-${k}{stroke:var(--i-${k})}`).join("") +
  `.sw{fill:none;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}.nf{fill:none}.o2{fill-opacity:.16}.o4{fill-opacity:.4}.dd{stroke-dasharray:4 4}.t{font:600 12px Inter,"Segoe UI",Arial,sans-serif}`;
writeFileSync(join(OUT, "illustrations.css"), CSS + "\n");

// ---------- helpers ----------
const svg = (id, w, h, title, desc, body, lang = "en") =>
  `<svg xmlns="http://www.w3.org/2000/svg" class="il" role="img" viewBox="0 0 ${w} ${h}" width="${w}" height="${h}" lang="${lang}"><title>${title}</title><desc>${desc}</desc><style>${CSS}</style>${body}</svg>\n`;
const T = {
  en: { prev: "Design preview", opt: "Optional" },
  fr: { prev: "Aperçu de conception", opt: "Optionnel" },
};
const cart = (x, y, lang) => {
  const s = T[lang].prev, w = Math.round(s.length * 6.7 + 22);
  return `<g><rect x="${x}" y="${y}" width="${w}" height="22" rx="11" class="f-at s-ag dd" stroke-width="1.5"/><text x="${x + 11}" y="${y + 15}" class="t f-ag">${s}</text></g>`;
};
const cartW = (lang) => Math.round(T[lang].prev.length * 6.7 + 22);
const check = (cx, cy, r = 12) => `<circle cx="${cx}" cy="${cy}" r="${r}" class="f-ok o2 s-ok" stroke-width="2"/><path d="M${cx - r / 2.4} ${cy + 0.5} l${r / 3.2} ${r / 3.2} l${r / 1.9} -${r / 1.7}" class="sw s-ok"/>`;
const cross = (cx, cy, r = 12) => `<circle cx="${cx}" cy="${cy}" r="${r}" class="f-dg o2 s-dg" stroke-width="2"/><path d="M${cx - r / 2.8} ${cy - r / 2.8} l${r / 1.4} ${r / 1.4} m0 -${r / 1.4} l-${r / 1.4} ${r / 1.4}" class="sw s-dg"/>`;
const bar = (x, y, w, c, extra = "") => `<rect x="${x}" y="${y}" width="${w}" height="8" rx="4" class="f-${c}${extra}"/>`;

// ---------- hero ----------
function hero(lang) {
  const rows = [
    [0, [[70, "ac"], [90, "mu"], [40, "tx"]]],
    [0, [[50, "wn"], [120, "tx"]]],
    [1, [[60, "ac"], [70, "ok"], [110, "tx"]]],
    [1, [[90, "tx"], [50, "wn"], [80, "mu"]]],
    [1, [[40, "ac"], [140, "tx"], [60, "ok"]]],
    [2, [[110, "tx"], [70, "ac"], [30, "mu"]]],
    [2, [[80, "mu"], [100, "tx"]]],
    [1, [[30, "ac"]]],
    [1, [[70, "ac"], [90, "ok"], [50, "tx"]]],
  ];
  const y0 = 104, step = 28, gx = 128;
  let g = "";
  rows.forEach(([ind, segs], i) => {
    const y = y0 + i * step;
    g += bar(62, y + 1, 14, "fa", "");
    let x = gx + ind * 28;
    segs.forEach(([w, c]) => { g += bar(x, y, w, c); x += w + 10; });
  });
  // current line + human cursor (row 5)
  const cy = y0 + 5 * step, ex = gx + 56 + 110 + 10 + 70 + 10 + 30 + 8;
  const cur = `<rect x="104" y="${cy - 9}" width="816" height="26" class="f-ac" fill-opacity=".1"/>` +
    `<rect x="${ex}" y="${cy - 6}" width="4" height="20" rx="2" class="f-ac"/>`;
  // ghost proposal (rows 9-10 region after row 8)
  const py = y0 + 9 * step - 6;
  const prop = `<rect x="116" y="${py}" width="560" height="74" rx="8" class="f-at s-ag dd" stroke-width="1.5"/>` +
    bar(gx + 28, py + 14, 120, "ag", " o4") + bar(gx + 28 + 130, py + 14, 80, "ag", " o4") + bar(gx + 28, py + 14 + 28, 170, "ag", " o4") + bar(gx + 28 + 180, py + 14 + 28, 60, "ag", " o4") +
    `<rect x="${gx + 28 + 252}" y="${py + 14 + 25}" width="5" height="18" rx="2.5" class="nf s-ag" stroke-width="2"/>` +
    check(620, py + 37, 11) + cross(652, py + 37, 11);
  const tabs =
    `<rect x="56" y="44" width="156" height="32" rx="6" class="f-bg"/><rect x="64" y="74" width="140" height="2" class="f-ac"/>` + bar(72, 56, 14, "ac") + bar(94, 56, 80, "tx") +
    bar(234, 56, 14, "fa") + bar(256, 56, 66, "mu") +
    bar(344, 56, 14, "fa") + bar(366, 56, 52, "mu") + `<circle cx="438" cy="60" r="4" class="f-ac"/>`;
  const status = `<rect x="40" y="476" width="880" height="28" class="f-s2"/>` + bar(60, 486, 60, "mu") + bar(132, 486, 40, "fa") + bar(184, 486, 50, "fa") + cart(920 - 16 - cartW(lang), 479, lang);
  const body =
    `<defs><clipPath id="hw"><rect x="40" y="36" width="880" height="468" rx="12"/></clipPath></defs>` +
    `<rect x="40" y="36" width="880" height="468" rx="12" class="f-sf s-ln" stroke-width="2"/>` +
    `<g clip-path="url(#hw)"><rect x="40" y="36" width="880" height="40" class="f-s2"/><rect x="40" y="76" width="64" height="400" class="f-s2" fill-opacity=".6"/>${tabs}${cur}${g}${prop}${status}</g>` +
    `<rect x="40" y="36" width="880" height="468" rx="12" class="nf s-ln" stroke-width="2"/>`;
  const t = lang === "fr"
    ? ["Un éditeur de texte, un curseur humain et un curseur fantôme", "Vue abstraite d'un éditeur : onglets, gouttière et lignes de code colorées. Le curseur bleu plein est le vôtre ; le curseur violet creux est celui d'un agent qui propose un bloc en pointillés à accepter ou refuser. Aperçu de conception, non livré."]
    : ["A text editor with a human cursor and a ghost cursor", "Abstract editor view: tabs, gutter and coloured code lines. The solid blue cursor is yours; the hollow violet cursor is an agent proposing a dashed block you can accept or reject. Design preview, not shipped yet."];
  return svg("hero", 960, 540, t[0], t[1], body, lang);
}

// ---------- 48x48 icons ----------
const ic = (name, title, desc, inner) => svg(name, 48, 48, title, desc, `<g class="sw s-tx">${inner}</g>`);
const A = (d) => `<path d="${d}" class="s-ac"/>`;
const icons = {
  "icon-quick-start": ["Quick start", "Lightning bolt.", `<path d="M27 5 L11 27 H23 L21 43 L37 20 H25 Z" class="s-ac"/>`],
  "icon-local-first": ["Local-first and private", "Laptop with a padlock on its screen.", `<rect x="8" y="9" width="32" height="23" rx="2"/><path d="M4 39 H44"/><rect x="19" y="19" width="10" height="8" rx="2" class="s-ac"/><path d="M21 19 v-2.5 a3 3 0 0 1 6 0 V19" class="s-ac"/>`],
  "icon-search-replace": ["Search and replace", "Magnifying glass with two text lines.", `<circle cx="21" cy="21" r="13"/><path d="M31 31 L42 42" class="s-ac"/><path d="M15 18 H27 M15 24 H23" class="s-ac"/>`],
  "icon-grouped-undo": ["Grouped undo", "Undo arrow inside a dashed group frame.", `<rect x="5" y="7" width="38" height="34" rx="2" class="s-fa dd"/><path d="M17 19 H27 a7 7 0 0 1 0 14 H19" class="s-ac"/><path d="M21 13 L15 19 L21 25" class="s-ac"/>`],
  "icon-crash-recovery": ["Crash recovery", "Circular restore arrow around a clock.", `<path d="M41 24 a17 17 0 1 1 -5 -12"/><path d="M37 5 V13 H29"/><path d="M24 15 V25 L30 29" class="s-ac"/>`],
  "icon-syntax": ["Syntax highlighting", "Angle brackets with an accent slash.", `<path d="M17 13 L7 24 L17 35 M31 13 L41 24 L31 35"/><path d="M27 9 L21 39" class="s-ac"/>`],
  "icon-palette": ["Shortcuts and command palette", "Palette box with a prompt chevron.", `<rect x="5" y="8" width="38" height="32" rx="2"/><path d="M5 16 H43"/><path d="M13 23 L19 28 L13 33 M23 34 H33" class="s-ac"/>`],
  "icon-backups": ["Durable backups", "Database cylinder with a check mark.", `<ellipse cx="24" cy="11" rx="14" ry="5" class="s-ac"/><path d="M10 11 V37 a14 5 0 0 0 28 0 V11 M10 24 a14 5 0 0 0 28 0"/><path d="M19 33 l4 4 l8 -9" class="s-ac"/>`],
};

// ---------- agent-proposes ----------
function agentProposes(lang) {
  const W = 800, H = 420;
  const hunk = (y, state) => {
    const tint = state === "ok" ? "ok" : state === "no" ? "dg" : "ag";
    return `<rect x="56" y="${y}" width="580" height="96" rx="8" class="f-bg s-ln" stroke-width="2"/>` +
      `<rect x="57" y="${y + 1}" width="578" height="46" class="f-dg o2"/><path d="M76 ${y + 24} h10" class="sw s-dg"/>` +
      `<rect x="57" y="${y + 47}" width="578" height="47" class="f-ok o2"/><path d="M76 ${y + 71} h10 M81 ${y + 66} v10" class="sw s-ok"/>`;
  };
  const lines = (y, a, b) =>
    bar(104, y + 20, a[0], "mu") + bar(114 + a[0], y + 20, a[1], "fa") +
    bar(104, y + 67, b[0], "tx") + bar(114 + b[0], y + 67, b[1], "ac");
  let g = "";
  const ys = [78, 190, 302];
  // hunk 1 accepted
  g += hunk(ys[0], "ok") + lines(ys[0], [120, 80], [150, 110]) + check(690, ys[0] + 48, 18);
  // hunk 2 rejected (dimmed content)
  g += hunk(ys[1], "no") + lines(ys[1], [100, 90], [130, 90]) + cross(690, ys[1] + 48, 18);
  // hunk 3 pending: both buttons outlined
  g += hunk(ys[2], "ag") + lines(ys[2], [140, 60], [110, 130]) +
    `<circle cx="660" cy="${ys[2] + 48}" r="18" class="nf s-fa" stroke-width="2"/><path d="M652 ${ys[2] + 49} l6 6 l11 -12" class="sw s-fa"/>` +
    `<circle cx="722" cy="${ys[2] + 48}" r="18" class="nf s-fa" stroke-width="2"/><path d="M714 ${ys[2] + 40} l16 16 m0 -16 l-16 16" class="sw s-fa"/>`;
  // move first two buttons to match: accepted button shows a check, rejected a cross; pending shows both outlined
  const ghost = `<rect x="56" y="30" width="5" height="26" rx="2.5" class="nf s-ag" stroke-width="2"/><rect x="72" y="35" width="150" height="16" rx="8" class="f-at s-ag dd" stroke-width="1.5"/>` + bar(84, 39, 90, "ag", " o4");
  const t = lang === "fr"
    ? ["Le curseur fantôme propose, vous validez", "Diff en trois blocs proposés par un agent : un bloc accepté (coche verte), un bloc refusé (croix rouge) et un bloc en attente avec les deux boutons. Aperçu de conception, non livré."]
    : ["The ghost cursor proposes, you decide", "A diff in three blocks proposed by an agent: one accepted (green check), one rejected (red cross) and one pending with both buttons. Design preview, not shipped yet."];
  return svg("ap", W, H, t[0], t[1], ghost + g + cart(W - 56 - cartW(lang) + 0, 30, lang), lang);
}

// ---------- local-first ----------
function localFirst(lang) {
  const W = 800, H = 360;
  const file = (x, y, c) => `<path d="M${x} ${y} h22 l10 10 v32 h-32 z" class="f-sf s-${c}" stroke-width="2"/><path d="M${x + 22} ${y} v10 h10" class="nf s-${c}" stroke-width="2"/><path d="M${x + 7} ${y + 24} h18 M${x + 7} ${y + 32} h12" class="sw s-mu"/>`;
  const machine =
    `<rect x="70" y="80" width="360" height="200" rx="14" class="f-sf s-ct" stroke-width="2"/>` +
    `<rect x="70" y="80" width="360" height="30" rx="14" class="f-s2"/><rect x="70" y="96" width="360" height="14" class="f-s2"/>` +
    bar(88, 91, 70, "mu") +
    file(100, 140, "ac") + file(160, 140, "ac") + file(220, 140, "ac") + file(130, 204, "ac") + file(190, 204, "ac") +
    `<rect x="316" y="170" width="70" height="56" rx="10" class="f-ok o2 s-ok" stroke-width="2"/><path d="M336 170 v-14 a15 15 0 0 1 30 0 v14" class="sw s-ok" transform="translate(0,14) translate(0,-14)"/><circle cx="351" cy="198" r="6" class="f-ok"/>` +
    `<path d="M30 300 H470" class="sw s-ct"/>`;
  const cloud = `<path d="M600 232 a30 30 0 0 1 4 -59 a44 44 0 0 1 84 -8 a36 36 0 0 1 12 70 z" class="f-s2 s-fa dd" stroke-width="2" fill-opacity=".5"/>`;
  const link = `<path d="M430 200 H520" class="sw s-fa dd"/><path d="M520 200 m-2 -10 l-8 10 l8 10" class="sw s-fa" transform="translate(-0,0) scale(1)" fill="none" style="display:none"/>`;
  const opt = `<g><rect x="568" y="262" width="${Math.round(T[lang].opt.length * 7 + 24)}" height="24" rx="12" class="nf s-fa dd" stroke-width="1.5"/><text x="580" y="278.5" class="t f-mu">${T[lang].opt}</text></g>`;
  const t = lang === "fr"
    ? ["Vos fichiers restent sur votre machine", "Schéma : les fichiers et le cadenas sont à l'intérieur de votre machine, sans nuage par défaut. Un nuage grisé en pointillés, marqué optionnel, représente un fournisseur d'IA que vous choisiriez. Aperçu de conception pour la partie IA."]
    : ["Your files stay on your machine", "Diagram: files and a padlock sit inside your machine, with no cloud by default. A greyed, dashed cloud marked optional stands for an AI provider you would choose. Design preview for the AI part."];
  return svg("lf", W, H, t[0], t[1], machine + `<path d="M430 200 H530" class="sw s-fa dd"/>` + cloud + opt + cart(W - 40 - cartW(lang), 24, lang), lang);
}

// ---------- download ----------
function download() {
  const body =
    `<rect x="40" y="60" width="150" height="190" rx="12" class="f-sf s-ct" stroke-width="2"/><rect x="40" y="60" width="150" height="26" rx="12" class="f-s2"/><rect x="40" y="76" width="150" height="10" class="f-s2"/>` +
    `<path d="M115 120 V196 M90 172 L115 198 L140 172" class="sw s-ac" stroke-width="4"/>` +
    `<path d="M62 232 H168" class="sw s-ac"/>` +
    `<path d="M260 66 h60 l14 14 v170 h-74 z" class="f-sf s-ct" stroke-width="2"/><path d="M320 66 v14 h14" class="nf s-ct" stroke-width="2"/>` +
    `<path d="M297 80 v10 M303 90 v10 M297 100 v10 M303 110 v10 M297 120 v10" class="sw s-mu"/><rect x="288" y="140" width="18" height="24" rx="4" class="nf s-mu" stroke-width="2"/>` +
    `<path d="M410 90 L470 110 V170 C470 212 440 236 410 250 C380 236 350 212 350 170 V110 Z" class="f-ok o2 s-ok" stroke-width="2" transform="translate(110,0)"/>` +
    `<path d="M490 170 l16 16 l30 -34" class="sw s-ok" stroke-width="4"/>`;
  // equality marks between files and shield
  return svg("dl", 640, 300, "Download and verify", "An installer, a zip archive and a shield with a check mark representing SHA-256 verification. Both files are checked against the published hash.", body + `<path d="M200 150 H240 M350 150 H370" class="sw s-fa dd"/>`);
}

// ---------- 404 ----------
function notfound() {
  const body =
    `<rect x="40" y="40" width="400" height="220" rx="12" class="f-sf s-ct" stroke-width="2"/><rect x="40" y="40" width="400" height="32" rx="12" class="f-s2"/><rect x="40" y="58" width="400" height="14" class="f-s2"/>` +
    bar(58, 52, 60, "mu") +
    bar(64, 100, 150, "fa") + bar(64, 124, 100, "fa") +
    `<rect x="64" y="160" width="10" height="52" rx="5" class="f-ac"/>` +
    `<rect x="100" y="162" width="9" height="48" rx="4.5" class="nf s-fa dd" stroke-width="2"/>` +
    `<path d="M150 186 h100" class="sw s-ct dd"/>`;
  return svg("nf", 480, 300, "Page not found", "A quiet editor window with an empty line and a blinking cursor: solid when on, dashed outline when off.", body);
}

// ---------- empty states 240x180 ----------
const es = (n, t, d, b) => svg(n, 240, 180, t, d, `<g class="sw s-tx">${b}</g>`);
const empties = {
  "empty-no-results": ["No results", "A magnifying glass over an empty list.", `<rect x="40" y="36" width="110" height="92" rx="8" class="s-ct dd"/><path d="M58 62 H132 M58 84 H116 M58 106 H100" class="s-fa dd"/><circle cx="148" cy="106" r="28" class="f-bg s-ac"/><path d="M168 126 L196 154" class="s-ac" stroke-width="4"/><path d="M138 106 H158" class="s-ac"/>`],
  "empty-nothing-to-recover": ["Nothing to recover", "A clean restore arrow around an empty page with a check mark.", `<path d="M178 90 a58 58 0 1 1 -16 -40" class="s-ct"/><path d="M168 26 V54 H140" class="s-ct"/><path d="M96 66 h34 l14 14 v50 h-48 z" class="s-ct dd"/><path d="M104 104 l9 9 l17 -20" class="s-ok" stroke-width="3"/>`],
  "empty-folder": ["Empty folder", "An open, empty folder drawn with dashed lines.", `<path d="M44 62 a6 6 0 0 1 6 -6 h36 l12 14 h72 a6 6 0 0 1 6 6 v62 a6 6 0 0 1 -6 6 H50 a6 6 0 0 1 -6 -6 z" class="s-ct dd"/><path d="M44 86 H184" class="s-ac"/>`],
};
// ---------- write ----------
const files = { "hero.svg": hero("en"), "hero.fr.svg": hero("fr"), "agent-proposes.svg": agentProposes("en"), "agent-proposes.fr.svg": agentProposes("fr"), "local-first.svg": localFirst("en"), "local-first.fr.svg": localFirst("fr"), "download.svg": download(), "notfound.svg": notfound() };
for (const [k, [t, d, b]] of Object.entries(icons)) files[`${k}.svg`] = ic(k, t, d, b);
for (const [k, [t, d, b]] of Object.entries(empties)) files[`${k}.svg`] = es(k, t, d, b);
for (const [f, c] of Object.entries(files)) writeFileSync(join(OUT, f), c);

// ---------- preview ----------
let n = 0;
const inline = (s) => { const k = ++n; return s.replace(/<style>.*?<\/style>/, "").replace(/id="(\w+)"/g, `id="$1${k}"`).replace(/url\(#(\w+)\)/g, `url(#$1${k})`).replace(/ width="\d+" height="\d+"/, ""); };
const cards = Object.entries(files).map(([f, c]) => `<figure class="c ${f.startsWith("icon-") ? "i" : f.startsWith("empty") ? "e" : "w"}"><div class="a">${inline(c)}</div><figcaption>${f} - ${(Buffer.byteLength(c) / 1024).toFixed(1)} KB</figcaption></figure>`).join("\n");
const imgs = ["hero.svg", "agent-proposes.fr.svg", "local-first.svg", "icon-quick-start.svg", "empty-folder.svg"].map((f) => `<img src="${f}" alt="${f}" class="im">`).join("");
const dk = Object.keys(tok).map((k) => `${tok[k]}:${dark[k]}`).join(";"), lt = Object.keys(tok).map((k) => `${tok[k]}:${light[k]}`).join(";");
writeFileSync(join(OUT, "index.html"), `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Illustrations preview</title><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="illustrations.css">
<style>
:root{${dark.bg ? lt : ""};color-scheme:light dark}
@media(prefers-color-scheme:dark){:root{${dk}}}
html:has(#t-dark:checked){${dk};color-scheme:dark}
html:has(#t-light:checked){${lt};color-scheme:light}
body{margin:0;padding:24px;background:var(--bg);color:var(--text);font:14px Inter,"Segoe UI",Arial,sans-serif}
h1,h2{font-size:16px;margin:24px 0 12px}.bar{display:flex;gap:16px;align-items:center}
.g{display:grid;gap:16px;grid-template-columns:repeat(auto-fill,minmax(280px,1fr))}
.c{margin:0;background:var(--surface);border:1px solid var(--border);border-radius:10px;padding:12px}.c.w{grid-column:span 2}
.c .a svg{width:100%;height:auto;display:block}.c.i .a svg{width:96px;height:96px}.c.e .a svg{width:200px;height:auto}
figcaption{color:var(--text-muted);font-size:12px;margin-top:8px}.im{max-width:300px;margin:0 12px 12px 0;background:var(--surface);border-radius:8px}
</style></head><body>
<div class="bar"><h1 style="margin:0">Brouillon illustrations</h1><label><input type="radio" name="t" id="t-auto" checked> auto</label><label><input type="radio" name="t" id="t-dark"> dark</label><label><input type="radio" name="t" id="t-light"> light</label></div>
<h2>Inline (uses illustrations.css, follows the toggle)</h2><div class="g">${cards}</div>
<h2>As img (isolated, follows prefers-color-scheme only)</h2><div>${imgs}</div></body></html>
`);
const sizes = Object.entries(files).map(([f, c]) => `${f} ${Buffer.byteLength(c)}`);
console.log(sizes.join("\n"), "\ncss", Buffer.byteLength(CSS));
