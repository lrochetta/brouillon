# Brouillon security and data handling

This file covers what the editor does with your data and how Brouillon Link (the opt-in "Agents" feature, an MCP server) is locked down. How to report a vulnerability is described in [`SECURITY.md`](../SECURITY.md) at the repository root.

Status of this text: it describes the code in this repository, including what is NOT done (last section).

## Data handling

The editor itself has no telemetry, no account, no analytics. Every flow below that can send data off the machine says on what basis (opt-in, or opt-out for the update check).

| Flow | What leaves the machine | To whom | When | Basis |
|---|---|---|---|---|
| `mcp-client-egress` | Whatever an MCP client (an AI program on this computer, for example Claude Code or Codex) reads through Brouillon Link: text of open documents, search hits, diagnostics, document paths and ids. Brouillon sends nothing itself; the client program decides what to forward to its own provider. | The provider of the client program you allowed (Brouillon cannot see beyond the pipe or the loopback socket). | Only after you turned Agents on AND allowed that client in the consent dialog; each tool call is then answered. Never for a client you denied, never for private buffers, never for files on the secret denylist. | Opt-in, per client. Consent can be limited to read only and is bound to the client program's hash. |
| `update-check` | Nothing about you or your files: the editor fetches a signed update manifest. The request is visible to the host (IP address, time). | The Brouillon website host (Cloudflare Pages). | A few seconds after launch, on a background thread, at most once per `updates.check_interval_hours` (default 24). Two plain GETs (`latest.json`, `latest.json.sig`); a third for the installer only when a newer signed build exists. No query string, no cookie, no identifier, `User-Agent: Brouillon-update/1` (no program version, no OS detail). | On by default; turn it off on the first-run screen or in Settings. An offline launch is unaffected (the check runs on a background thread and stays silent). `updates.check = false`, `BROUILLON_NO_UPDATE=1` or a file named `no-update` in the data folder disable everything. Test redirections (`BROUILLON_UPDATE_*`) are ignored by release builds. |
| `remote-servers` (experimental) | Your login and the files you open, list or save on a server you configured (SFTP through the system `ssh`; FTPS; plain FTP only after an explicit opt-in, and then unencrypted). Passwords go to the server only, are never logged or written to the journal, and are kept on this computer only if you chose to keep them (see `credential-vault`). | The server you typed, through `brouillon-remote.exe`. Never to the Brouillon project or any third party. | Only when you connect, browse or save in a remote tab. | User action, per connection. Not yet run against a real server through the editor. |
| `credential-vault` (experimental) | Nothing. A password you chose to keep after a successful connect is stored in `vault.bsv` on this computer. It is never sent anywhere except to the server it belongs to, when you connect to it. | Nobody. | Only after you answer "Keep this password?" with yes; the default is "use this time only". | Opt-in, per site. Portable mode keeps nothing. |
| `cloud-drives` | Nothing. Google Drive, Dropbox and Box are not implemented, so there is no connector, no API call and no login. A folder that the vendor's own desktop app syncs on your disk is just a local folder to Brouillon. | Nobody. | Never. | Not applicable (not implemented). |

Credentials policy: nothing is stored unless you choose to keep a remote password after a successful connect (default: use this time only). Then it goes to the credential vault (`brouillon-vault`, EXPERIMENTAL: tests only, never seen on screen). Host-key and certificate pins and the site list are kept without secrets. Files on the never-share list and secrets found in text are hidden from agents (see below). The importer refuses credentials embedded in old settings files.

Link adds no network listener by default. It listens on a per-user named pipe (Windows) or Unix socket only while enabled. The optional loopback HTTP transport (`[mcp] http = true`) listens on `127.0.0.1` only, on a random port, behind a bearer token. Nothing in Link dials out: there is no relay, no tunnel, no OAuth server in the editor (a remote path would be a later, separate opt-in).

What the audit log records: client name, tool, target path, sizes, content hash, decision, result. Never document content, never diffs, never tokens. The log is local (`audit.jsonl` in the link folder), hash-chained, rotated at 10 MB.

## Brouillon Link: security model in one page

Off by default. `[mcp] enabled = false`: no thread, no socket, no file. Turning it on (Preferences > Connect, or `--mcp` for one session) is the only thing that opens anything.

1. Identity and consent. A client that has not been allowed gets `CONSENT_REQUIRED` for every call and the user sees a modal naming the program (path and fingerprint when the relay reports them). Enter = read only, Escape = deny; the modal dims the window and swallows every other key. The grant is bound to the program hash: a different hash, or a client that stops reporting one, is asked again and its old grants are replaced. A client that touches a folder opened after it was allowed triggers a one-time question for that folder only; folders that are in no open document are never prompted and stay invisible.
2. Scopes. S1 read, S2 propose (default; every edit is shown for per-hunk review and is one undo step), S3 apply directly and `save`. The ceiling is S2 unless the user sets `[mcp] allow_apply = true`; choosing mode "trusted" alone does not raise it. S4 (execute) is not implemented.
3. `save`. Needs S3, so it does not exist in the default configuration. It saves one document, only if the client may access that path, never an unsaved buffer, never `.git` or task files, never a denylisted file. It refuses when the file changed on disk unless `force`, refuses a lossy encoding (that choice is the user's), and can refuse when the text moved (`base_revision`). No dialog ever opens for an agent.
4. `annotate`. A visible note or highlight on a line, owned by the calling client (shown with its name), removable by that client or with one click by the user, with a time limit (at most 10 minutes) and a cap of 20 per client. It never changes the text or the version.
5. `run_command`. Only a fixed allowlist of view and read-only analysis commands (line numbers, markdown preview, hex view, count text, sum numbers). No command that changes text or files, no tool runner, no shell.
6. `diagnostics`. "Lite" checks only: unbalanced brackets in code-like files, JSON syntax errors, encoding damage, mixed line endings. There is no language server yet; an empty list means these checks found nothing.
7. Limits. Per client: 10 calls per second sustained (burst 30), 2 writes per second (burst 6), 4 calls in flight, 1 MiB per message. Eight refused calls in a row pause the client for 30 s and tell the user (circuit breaker).
8. Concurrency. An edit must name the revision it was based on. A stale revision returns `STALE_VERSION` with the changed line ranges when the text the client saw is still remembered. A pending proposal is moved over the user's typing only when every line it touches and the lines just before and after are unchanged; otherwise it is stale and nothing is applied.
9. Kill switch. The chip button and `Ctrl+Alt+K` stop the server, drop every connection, cancel pending proposals, revoke grants and write `[mcp] enabled = false`, so it does not come back at the next launch. "this session only" next to the button stops without changing the setting. `[mcp] auto_off_minutes = N` switches the server off (for the session) after N minutes with no connected client.
10. Secrets. Two layers. (a) Files on the never-share list are blocked, not masked: `.env*`, private keys and certificates (`*.pem`, `*.key`, `*.pfx`, `*.p12`, `*.jks`, `id_*`), `.ssh`, `.aws`, `.netrc`, `.pgpass`, `.npmrc`, `.git-credentials`, `.docker/config.json`, `*.tfstate`, `terraform.tfvars`, `.htpasswd`, `known_hosts`, `*.kdbx`, `*.ovpn`, `secrets.*` and `credentials*` (not source files), and the editor's own files. That core list cannot be removed by any setting. Web-app configs (`wp-config.php` and similar) are blocked by default; only the user can allow them for a folder (`[mcp] allow_web_configs`). `[mcp] never_share` adds your own globs; a workspace file can add globs but can never remove one or allow anything. (b) Everything else an agent reads (`read`, `search`, `editor_state`, diagnostics, command results) goes through a local scanner (`brouillon-secrets`, no network): private key blocks, provider token shapes, `scheme://user:password@`, `password=`-style assignments, JWT and bearer tokens are replaced by `[REDACTED:<rule>:<length>]`, keeping every line break, so line numbers stay valid. Search runs on the masked text, so a pattern cannot probe a secret. An edit that touches a masked span or contains a marker is refused (`REDACTED_SPAN`); a proposal that adds a credential-looking value is flagged (`adds_credentials`) and is never applied without the user's review, even at S3. The audit log redacts every text field it writes. Anything an agent reads is marked untrusted and invisible Unicode (zero-width, bidi, tag characters) is made visible.

### Transport hardening

Pipe (Windows): created with a DACL granting access to the current user's SID only (no Everyone, no Anonymous), as the first instance (a squatted name makes the editor refuse to start the server), remote clients rejected. After accept, the connecting process must run as the same user (checked by token) and the pid the relay claims in its hello must be the pid Windows reports. A client that connects and never says hello is dropped after 5 s. On Unix the equivalent is the socket in the user's directory plus a peer-uid comparison (written but not exercised on this machine: see below). Kill drops idle connections within a few milliseconds (measured under 200 ms in tests).

Loopback HTTP (`[mcp] http = true`, off by default): `127.0.0.1` only, random port per start, random bearer token per start stored in the instance file under the user's profile (mode 0600 on Unix). Checked before the body is read: `Host` must be exactly `127.0.0.1:<port>` or `localhost:<port>` (DNS rebinding), any `Origin` header is refused (browsers always send one), path `/mcp`, `POST` only, constant-time token comparison, `Content-Type: application/json`, no chunked bodies, 1 MiB cap, 10 s I/O timeout, 16 connections. No `Access-Control-*` header is ever sent. HTTP clients live in their own namespace (`http:<name>`), so they cannot ride on a consent given to a pipe client of the same name.

## Credential vault (experimental)

What it is: one file, `vault.bsv`. A random master key is wrapped by Windows DPAPI at user scope; each secret has its own key and is sealed with XChaCha20-Poly1305. Index, header and records are bound together so that swapped, downgraded or rolled-back files are detected.

| Property | Status |
|---|---|
| Mode | Standard only. DPAPI user scope. No master password, no passphrase, recovery key, FIDO2 or TPM slot yet (the file format reserves them). |
| What is kept | Only a remote password you chose to keep, after a successful connect. Default is "use this time only". Never a password you typed on a failed connect, never keys. |
| Plaintext fallback | None. If DPAPI or the vault fails, nothing is stored and you are asked every time. |
| Portable mode | Keeps nothing (no machine-bound key). |
| Opening and locking | Opens on first real use, never at start-up. Locks after 15 minutes idle, on the agent kill switch and on Panic. |
| Showing values | Never. The Credentials view lists site names and dates only; "Forget" deletes. A secret prints as `***` in logs and debug text; a canary test covers the vault folder, the site list and debug output. |
| Agents | Never see values. The vault crate is not a dependency of the agent server (`brouillon-mcp`), and `vault.bsv`, its backups and its temporary files are on the never-share list, so even a path to them is refused. |
| Protects against | Another Windows account, another computer, someone who copies the file alone, accidental leaks into logs, settings or the journal, and an AI agent asking for it. |
| Does NOT protect against | Malware or any program running as you (it can ask DPAPI to unlock), someone who knows your Windows password or has your logged-in session, and an administrator who resets your account. Without a master password this is a convenience, not a safe. |
| Not verified | Never seen on screen, never used against a real ssh password prompt. Tests only. |

## Hex editor (experimental)

Hex editing works on a local copy of the bytes in memory and writes only when you save. Regions are stored per file hash in a folder under the settings folder; that folder is on the never-share list and hex commands are not runnable by agents. Hashes use code in the editor itself (no new dependency, no network).

## Known limits (honest list)

- The program behind the pipe is the relay (`brouillon-mcp`), which reports its parent executable itself. That report is informational input to re-consent, not a proof: a hostile process running as you can lie about it. The same-user checks stop other accounts, not your own malware.
- A connection blocked in a write to a client that never reads is released only when that client exits; the kill switch still refuses every call at once.
- The Unix peer-uid check and the Unix socket permissions are written but could not be run in this environment (Windows only).
- Notes are tied to line numbers, not to text: after an edit a note may sit on a different line until it expires or is removed.
- Secret masking is best effort and honest about it: the scanner is pattern based. Known misses: an unquoted password made only of letters (it looks like an identifier), a secret split over lines, encoded or encrypted secrets, a secret with no key name around it. A guess can still be confirmed by an edit `match` (the answer is `REDACTED_SPAN` or `NO_MATCH`). Masking is defense in depth next to the never-share list, consent and the review of every change.
- Earned-trust "always allow", resources and prompts, and any remote path are not implemented.
- The tool-set hash is pinned per grant in memory (`Policy::grant_pinned`): a grant restored with an older hash asks the user again. The host does not persist grants yet, so across restarts the hook is not used.
- Token, link and audit files get an owner-only ACL on Windows (best effort, a failure is ignored because the files already sit in the user profile); administrators and SYSTEM lose implicit access to them.
