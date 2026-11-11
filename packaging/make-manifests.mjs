#!/usr/bin/env node
// Generates package-manager manifests (winget, Scoop, Homebrew draft) for a Brouillon release.
// OFFLINE and side-effect free: it only reads build-info.json + SHA256SUMS.txt and writes files under
// --out. It never uploads, submits or opens a pull request; see packaging/README.md for the human steps.
//
//   node packaging/make-manifests.mjs [--downloads <dir>] [--base-url <https url>] [--out <dir>]
//
//   --downloads  folder with build-info.json and SHA256SUMS.txt (default site/public/downloads)
//   --base-url   public folder that will serve the release files, e.g. https://example.org/downloads
//                The ONLY source of real URLs. Without it every URL is the inert placeholder
//                https://release-base-url.invalid and the output is marked "not submittable".
//   --out        output folder (default packaging/dist, git-ignored)
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const PLACEHOLDER_BASE = "https://release-base-url.invalid";
export const PACKAGE_ID = "lrochetta.Brouillon";
const MANIFEST_VERSION = "1.10.0";
const PUBLISHER = "Laurent Rochetta";
const HOMEPAGE = "https://brouillon.rochetta.fr";
// Inno Setup ProductCode = {AppId}_is1 (AppId from packaging/windows/brouillon.iss).
const PRODUCT_CODE = "{7B3E5C1A-4D2F-4E8B-9A61-2F0C8D5E7B14}_is1";
const SHORT = "Fast, local-first desktop text editor that does not phone home.";
const LONG =
  "Brouillon is a free, open-source (MIT) local-first desktop text editor written in Rust. No account and no telemetry in the editor.";

const q = (s) => JSON.stringify(String(s)); // a JSON string is a valid YAML double-quoted scalar

export function parseSums(text) {
  const map = new Map();
  for (const line of String(text).split(/\r?\n/)) {
    if (!line.trim()) continue;
    const m = /^([0-9a-fA-F]{64}) [ *](.+)$/.exec(line.trim());
    if (!m) throw new Error(`SHA256SUMS.txt: malformed line: ${line}`);
    map.set(m[2], m[1].toLowerCase());
  }
  return map;
}

export function normalizeBase(base) {
  if (base == null || base === "") return { base: PLACEHOLDER_BASE, placeholder: true };
  let u;
  try { u = new URL(base); } catch { throw new Error(`--base-url is not a URL: ${base}`); }
  if (u.protocol !== "https:") throw new Error("--base-url must be https");
  if (u.search || u.hash) throw new Error("--base-url must not contain a query or fragment");
  return { base: u.href.replace(/\/+$/, ""), placeholder: false };
}

function pick(info, sums, test, label) {
  const a = (info.artifacts || []).find(test);
  if (!a) return null;
  const h = sums.get(a.file);
  if (!h) throw new Error(`${label} ${a.file} is not listed in SHA256SUMS.txt`);
  if (h !== String(a.sha256).toLowerCase()) throw new Error(`${label} ${a.file}: SHA256SUMS.txt disagrees with build-info.json`);
  return { file: a.file, sha256: h, size: a.size_bytes };
}

/** Returns { files: {relativePath: text}, placeholder, notes[] }. Pure: no IO. */
export function generate({ info, sumsText, baseUrl }) {
  const { base, placeholder } = normalizeBase(baseUrl);
  const sums = parseSums(sumsText);
  const version = String(info.version);
  if (!/^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$/.test(version)) throw new Error(`unexpected version: ${version}`);
  const build = String(info.build);
  const date = String(info.build_date_utc || "").slice(0, 10);
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) throw new Error("build_date_utc is missing or not ISO");
  const inst = pick(info, sums, (a) => a.kind === "installer", "installer");
  const port = pick(info, sums, (a) => a.kind === "portable", "portable zip");
  const dmg = pick(info, sums, (a) => /\.dmg$/i.test(a.file), "macOS dmg");
  const files = {};
  const notes = [];
  const banner = placeholder
    ? "# NOT SUBMITTABLE: URLs are placeholders. Re-run make-manifests with --base-url <https url of the published files>.\n"
    : "";
  const unsigned = info.signed === false;

  if (inst) {
    const dir = `winget/manifests/l/lrochetta/Brouillon/${version}`;
    const id = `PackageIdentifier: ${PACKAGE_ID}\nPackageVersion: ${q(version)}\n`;
    files[`${dir}/${PACKAGE_ID}.yaml`] =
      `${banner}# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.${MANIFEST_VERSION}.schema.json\n\n` +
      `${id}DefaultLocale: en-US\nManifestType: version\nManifestVersion: ${MANIFEST_VERSION}\n`;
    files[`${dir}/${PACKAGE_ID}.installer.yaml`] =
      `${banner}# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.${MANIFEST_VERSION}.schema.json\n\n` +
      `${id}InstallerLocale: en-US\nMinimumOSVersion: ${q("10.0.19044.0")}\nInstallerType: inno\nScope: user\n` +
      `InstallModes:\n- interactive\n- silent\n- silentWithProgress\nUpgradeBehavior: install\nReleaseDate: ${date}\n` +
      `Installers:\n- Architecture: x64\n  InstallerUrl: ${q(`${base}/${inst.file}`)}\n  InstallerSha256: ${inst.sha256.toUpperCase()}\n` +
      `  ProductCode: ${q(PRODUCT_CODE)}\n  AppsAndFeaturesEntries:\n  - DisplayName: Brouillon\n    Publisher: ${q(PUBLISHER)}\n` +
      `    DisplayVersion: ${q(`${version} (build ${build})`)}\n    ProductCode: ${q(PRODUCT_CODE)}\n` +
      `ManifestType: installer\nManifestVersion: ${MANIFEST_VERSION}\n`;
    files[`${dir}/${PACKAGE_ID}.locale.en-US.yaml`] =
      `${banner}# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.${MANIFEST_VERSION}.schema.json\n\n` +
      `${id}PackageLocale: en-US\nPublisher: ${q(PUBLISHER)}\nPublisherUrl: ${q(HOMEPAGE)}\nAuthor: ${q(PUBLISHER)}\n` +
      `PackageName: Brouillon\nPackageUrl: ${q(HOMEPAGE)}\nLicense: MIT\nCopyright: ${q(`Copyright (c) 2026 ${PUBLISHER}`)}\n` +
      `ShortDescription: ${q(SHORT)}\nDescription: ${q(LONG)}\nMoniker: brouillon\nTags:\n- editor\n- text-editor\n- rust\n- local-first\n- offline\n` +
      `ManifestType: defaultLocale\nManifestVersion: ${MANIFEST_VERSION}\n`;
  } else notes.push("no installer artifact in build-info.json: winget manifests skipped");

  if (port) {
    const scoop = {
      ...(placeholder ? { "##": "NOT SUBMITTABLE: URLs are placeholders. Re-run make-manifests with --base-url." } : {}),
      version,
      description: SHORT,
      homepage: HOMEPAGE,
      license: "MIT",
      ...(unsigned ? { notes: "This build is not Authenticode-signed: Windows SmartScreen may warn on first run." } : {}),
      architecture: { "64bit": { url: `${base}/${port.file}`, hash: port.sha256 } },
      bin: "brouillon.exe",
      shortcuts: [["brouillon.exe", "Brouillon"]],
      checkver: {
        url: `${base}/build-info.json`,
        // group 1 = version; named group "build" is used by the autoupdate URL ($matchBuild)
        regex: '"version":\\s*"([^"]+)"[\\s\\S]*?"build":\\s*"?(?<build>\\d+)"?',
      },
      autoupdate: {
        architecture: { "64bit": { url: `${base}/Brouillon-$version-build$matchBuild-win-x64-portable.zip` } },
        hash: { url: `${base}/SHA256SUMS.txt`, regex: "$sha256\\s+$basename" },
      },
    };
    files["scoop/bucket/brouillon.json"] = JSON.stringify(scoop, null, 4) + "\n";
  } else notes.push("no portable artifact in build-info.json: Scoop manifest skipped");

  // Homebrew: Brouillon ships Windows x64 only. This is a DRAFT cask for the day a macOS .dmg exists.
  const dmgFile = dmg ? dmg.file : `Brouillon-${version}-build${build}-macos-universal.dmg`;
  const dmgSha = dmg ? dmg.sha256 : "REPLACE_WITH_SHA256_OF_THE_MACOS_DMG";
  files["homebrew/Casks/brouillon.rb"] =
    `# DRAFT. ${dmg ? "" : "No macOS artifact exists yet: the file name and sha256 below are placeholders. "}Do not publish until a real, signed and notarized .dmg is built.\n` +
    (placeholder ? "# NOT SUBMITTABLE: URL base is a placeholder. Re-run make-manifests with --base-url.\n" : "") +
    `cask "brouillon" do\n  version ${q(version)}\n  sha256 ${q(dmgSha)}\n\n` +
    `  url ${q(`${base}/${dmgFile}`)}\n  name "Brouillon"\n  desc ${q(SHORT)}\n  homepage ${q(HOMEPAGE)}\n\n` +
    `  livecheck do\n    url ${q(`${base}/build-info.json`)}\n    regex(/"version":\\s*"([^"]+)"/i)\n  end\n\n` +
    `  depends_on macos: ">= :monterey"\n\n  app "Brouillon.app"\nend\n`;
  if (!dmg) notes.push("no macOS artifact: Homebrew cask is a placeholder draft");

  return { files, placeholder, notes, version };
}

function parseArgs(argv) {
  const o = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!["--downloads", "--base-url", "--out"].includes(a) || i + 1 >= argv.length) throw new Error(`usage error near: ${a}`);
    o[a.slice(2)] = argv[++i];
  }
  return o;
}

export function main(argv) {
  const o = parseArgs(argv);
  const downloads = path.resolve(o.downloads || path.join(HERE, "..", "site", "public", "downloads"));
  const out = path.resolve(o.out || path.join(HERE, "dist"));
  const info = JSON.parse(fs.readFileSync(path.join(downloads, "build-info.json"), "utf8"));
  const sumsText = fs.readFileSync(path.join(downloads, "SHA256SUMS.txt"), "utf8");
  const r = generate({ info, sumsText, baseUrl: o["base-url"] });
  for (const [rel, text] of Object.entries(r.files)) {
    const f = path.join(out, rel);
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, text);
    console.log("wrote", f);
  }
  for (const n of r.notes) console.warn("note:", n);
  if (r.placeholder) console.warn("WARNING: no --base-url given; URLs are placeholders and the output is NOT submittable.");
  return r;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { main(process.argv.slice(2)); } catch (e) { console.error("error:", e.message); process.exit(1); }
}
