// Forbidden third-party names. Shared by check.mjs and build.test.mjs.
// Project rule: the site never names the products that were studied, their technologies or the study itself.
// Terms are assembled from fragments so that this file and the scripts folder pass their own scan.
const S = "[\\s\\u00a0]*";
export const FORBIDDEN = [
  ["Edit" + "Plus / Edit" + " Plus", new RegExp("edit" + S + "plus", "i")],
  ["Notepad" + "++", new RegExp("notepad" + "[+][+]", "i")],
  ["Notepad " + "plus plus", new RegExp("notepad" + S + "plus" + S + "plus", "i")],
  ["Sub" + "lime", new RegExp("sub" + "lime", "i")],
  ["Edit" + "Pad", new RegExp("edit" + "pad", "i")],
  ["Scin" + "tilla", new RegExp("scin" + "tilla", "i")],
  ["Lex" + "illa", new RegExp("lex" + "illa", "i")],
  [".s" + "tx", new RegExp("[.]s" + "tx(?![a-z0-9])", "i")],
  ["reverse " + "engineer", new RegExp("reverse[\\s\\u00a0-]+" + "engineer", "i")],
  ["Agent" + "Pad", new RegExp("agent" + "pad", "i")],
];
// Explicit allow-list: these exact strings are removed before matching. Nothing else is exempt.
export const ALLOWED = ["JetBrains Mono"];

export function findForbidden(text) {
  let t = text;
  for (const a of ALLOWED) t = t.split(a).join("");
  return FORBIDDEN.filter(([, re]) => re.test(t)).map(([name]) => name);
}

// files: absolute paths. Returns [{ file, names }] for every file containing a forbidden term.
export function scanFiles(files, readFile) {
  const hits = [];
  for (const f of files) {
    const names = findForbidden(readFile(f));
    if (names.length) hits.push({ file: f, names });
  }
  return hits;
}
