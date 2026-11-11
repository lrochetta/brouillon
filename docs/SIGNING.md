# Code signing for Brouillon

Status: DRAFT, written 2026-11-10. Nothing in this document has been applied for, submitted or run. Brouillon releases are UNSIGNED today.

## 1. What code signing is, in plain words

A code signature is a tamper seal on a program file. A certificate authority checks who you are, gives you a certificate, and you use it to sign `brouillon.exe`. Windows can then show "Publisher: <name>" and can tell if the file was changed after signing. A signature proves who published the file and that it was not modified. It does not prove the program is safe, and it does not make Windows trust it instantly (section 5).

## 2. What stays unsigned until the owner acts

- `brouillon.exe`, `brouillon-mcp.exe`, `brouillon-remote.exe`, the Inno installer and the portable zip: all unsigned. `build-info.json` says `"signed": false`.
- Windows SmartScreen shows "Windows protected your PC"; the user clicks "More info" then "Run anyway". Unsigned Rust binaries are also more exposed to antivirus false positives.
- The signed update manifest (`latest.json` and its `.sig`) is a separate mechanism: it uses the project's own Ed25519 key, not a Windows certificate. It is unrelated to Authenticode.

## 3. How users can verify a download today

Each release has `SHA256SUMS.txt`. In PowerShell:

```powershell
Get-FileHash .\Brouillon-<version>-build<N>-win-x64-portable.zip -Algorithm SHA256
Get-Content .\SHA256SUMS.txt
```

The two hashes must match. This proves the file equals what the project listed, as long as you got `SHA256SUMS.txt` from a channel you trust (the project page over HTTPS, or the GitHub release). It is not a signature of identity. See also `docs/INSTALL.md`. The release workflow draft also produces CycloneDX SBOM files (the list of Rust dependencies).

## 4. SignPath Foundation

What it is: a free code signing service for open-source projects. The certificate is issued to "SignPath Foundation", not to you; Windows shows that name as publisher.

Conditions as publicly documented (page: https://signpath.org/terms, fetched 2026-11-10; the page itself says its code of conduct is still a draft):

- OSI-approved open-source license without commercial dual-licensing, for all components. Brouillon is MIT: compatible.
- No proprietary, non open-source component in the signed software (system libraries are allowed).
- The signing team maintains the project and owns the repository; binaries are built from source in a verifiable way (a CI build, not a laptop).
- Project actively maintained; roles split into authors, reviewers and approvers; every signing request approved by a trusted team member; multi-factor authentication for all members.
- The words "Code signing policy" must appear on the home page and the download page (section or link), listing roles and members.
- A privacy statement, or an explicit statement that the program does not transfer information to other networked systems unless the user asks.
- The line "Free code signing provided by SignPath.io, certificate by SignPath Foundation." must be displayed.
- A "certain verifiable reputation" is required; acceptance is at SignPath's discretion after an initial check of project reputation and control.
- Signed binaries need consistent metadata: product name equal to the project name, consistent product version.

Could NOT verify:

- The application procedure, form fields and timeline: https://signpath.org/apply returned only a title and navigation to my fetch. Read it yourself.
- Whether a brand-new, low-star project meets the "verifiable reputation" bar. There is no public number; assume it may be refused or delayed.
- Whether a single-maintainer project can satisfy the roles rule. The terms say roles are split into author, reviewer, approver and require approval "from a trusted team member"; I could not confirm that one person can fill all roles. Ask SignPath before applying.
- Exact SignPath GitHub Actions behaviour beyond the documented inputs (read from https://docs.signpath.io/trusted-build-systems/github, 2026-11-10); the draft workflow was never executed.

The one-solo-maintainer question matters: the project's own decision note says SignPath is the primary choice, Azure second, a paid OV certificate last.

## 5. SmartScreen reputation is not instant

Per Microsoft's guidance (https://learn.microsoft.com/windows/apps/package-and-deploy/code-signing-options and /smartscreen-reputation, via search results 2026-11-10; I did not read the full pages): Artifact Signing does not provide instant SmartScreen trust, OV certificates behave the same way for SmartScreen, and EV certificates are no longer an instant bypass. Reputation builds automatically from download history and analysis; developers cannot enrol files in a known-good list. Microsoft Q&A threads (user reports, not official) describe renewed warnings after Artifact Signing changed intermediate CAs in March 2026. Do not promise users a warning-free first run, even when signed. A signature makes the reputation attach to the publisher identity instead of to each new file hash, which helps over time.

## 6. Alternatives

- Azure Artifact Signing (formerly Trusted Signing): paid, identity validation. Per the search result above (2026-11-10), individual developers are eligible only in the USA and Canada; organizations in the USA, Canada, EU and UK. Verify on Microsoft Learn before relying on it.
- OV code-signing certificate from a commercial CA plus SignTool: paid, last resort (private key handling and hardware-token rules apply).

## 7. What the project must provide (for SignPath)

1. A public GitHub repository that the owner controls (the workflow draft cannot run before this).
2. A signing policy page (`SIGNING_POLICY.md`, template at the repository root) linked from the home and download pages, with the exact words "Code signing policy".
3. Named roles and approvers, with MFA on those accounts.
4. Build provenance: release exes built only by the GitHub Actions workflow (`.github/workflows/release.yml.draft`, renamed), from a tagged commit, with `--locked`, pinned toolchain and pinned actions.
5. Privacy statement: the editor has no telemetry; flows that can send data are listed in `docs/SECURITY.md` ("Data handling").
6. Product metadata in the exe: product name "Brouillon" and one version number everywhere (check the Windows resource of each of the three exes).
7. The attribution line on the download page.

Domain separation: the signing identity and the update-manifest key stay separate; never reuse one for the other.

## 8. Owner checklist (in order)

1. Decide to publish the repository (owner decision, not yet taken).
2. Read https://signpath.org/terms and https://signpath.org/apply; ask SignPath whether a solo maintainer satisfies the roles rule.
3. Fill `SIGNING_POLICY.md` (placeholders), link it from the site home and download pages, add the attribution line.
4. Enable MFA on the GitHub account; consider branch protection and required review (or document why a solo author is the reviewer).
5. Rename `.github/workflows/release.yml.draft` to `release.yml`; replace the placeholders (`RUST_VERSION`, cargo-cyclonedx version); run it once with no `SIGN_PROVIDER` and check the unsigned artifacts and hashes.
6. Apply to SignPath Foundation (outward-facing: only you do this).
7. After approval: create the SignPath project, artifact configuration (root `<zip-file>`, sign the three exes), link the GitHub.com trusted build system; set repository variables and the `SIGNPATH_API_TOKEN` secret; set `SIGN_PROVIDER=signpath`.
8. Add the second signing round for the installer (see open items), then publish the release.
9. If refused: reconsider Azure Artifact Signing (set `SIGN_PROVIDER=azure`) or an OV certificate (`-SignCommand` hook, `packaging/README.md`).

## 9. Open items in the draft

- `packaging/build-release.ps1` overwrites `SOURCE_DATE_EPOCH` with the current time; for reproducible zips it should honour an existing value.
- It does not bundle `brouillon-remote.exe`; the workflow ships it as a separate asset.
- Only the three exes are signed in the draft; the Inno installer needs a second signing request or the Inno `SignTool` hook, which the SignPath async model does not fit directly.
- cargo-cyclonedx must be downloaded and compiled (Apache-2.0, https://github.com/CycloneDX/cyclonedx-rust-cargo, checked 2026-11-10); the pinned version number in the draft is unverified, and the exact flags were not confirmed from the README (check `cargo cyclonedx --help`).
- Action pins were resolved with `git ls-remote` on 2026-11-10; re-resolve and review before enabling.
