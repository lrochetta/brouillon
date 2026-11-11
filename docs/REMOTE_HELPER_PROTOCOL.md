# Remote helper protocol (v1)

Status: **implemented and tested against an in-process fake SFTP server; not yet run against a real sshd.**
Code: `_bmad-output/build/remote-helper/` (separate Cargo workspace, not part of the editor build).
Typed messages: crate `brouillon-remote-proto` (`msg.rs`, `frame.rs`, `path.rs`, `limits.rs`). This document and
that crate are the contract for the SafeSaver and explorer code: if they disagree, the crate wins and this file is a bug.

## 1. Process model

* `brouillon-remote.exe` is an optional helper next to `brouillon.exe`. The editor spawns it with piped stdio
  (`brouillon-remote` or `brouillon-remote serve`). **stdout carries protocol frames only**; stderr is free text for
  diagnostics and may be ignored or shown in a log pane; it never contains secrets.
* One helper process serves **one SFTP session** (one `connect`). Several sites = several helpers.
* When the editor closes the helper's stdin, the helper kills its `ssh` child and exits (no orphans). If the helper
  dies, the editor keeps its buffers and may respawn it and reconnect.
* SFTP is spoken over the OS `ssh -s sftp` child (hand-written SFTP v3 codec, no SSH library). Host keys, `known_hosts`,
  `ssh_config` aliases, ProxyJump, ssh-agent and FIDO keys are OpenSSH's job.

## 2. Framing

```
u32 LE length | u8 kind | body          length = 1 + len(body), 1 <= length <= 1 MiB
kind 1 CONTROL  body = UTF-8 JSON object
kind 2 DATA     body = u64 LE request id | u64 LE file offset | bytes (0..=256 KiB)
```

A bad frame (length 0 or > 1 MiB, unknown kind, truncated, DATA chunk > 256 KiB) is a fatal protocol error: the helper
sends `{"id":null,"error":{"kind":"protocol",...}}` and exits. Invalid JSON or an invalid request body (including an invalid
path) is **not** fatal: it is answered with an `invalid_argument` error carrying the request id when it could be read.

## 3. Messages

Editor to helper: `{"id": <u64>, "method": "<name>", "params": {...}}`. `id` is chosen by the editor, unique among
requests in flight.

Helper to editor, exactly one of:

```
{"id": N, "result": {...}}
{"id": N, "error": {"kind": "<ErrorKind>", "message": "<text, control chars removed, <= 4 KiB>"}}
{"id": null, "error": {...}}                      protocol-level error not tied to a request
{"event": "progress"|"list_page"|"prompt"|"state", ...}
```

The first request must be `hello`. Anything else before it is answered `invalid_argument`.

### Methods

| method | params | result | notes |
|---|---|---|---|
| `hello` | `proto: 1, client: str` | `{proto, helper_version, caps[], limits{...}}` | `unsupported` if `proto` differs |
| `ping` | `{}` | `{}` | |
| `connect` | `ConnectParams` | `{home, sftp_version, extensions[], posix_rename, fsync, protocol, insecure, ftp?}` (FTP additions: section 9) | once per process; emits `state connected` before the result |
| `disconnect` | `{}` | `{}` | kills ssh |
| `list_dir` | `path` | `{count, truncated, hidden_unsafe}` | entries arrive as `list_page` events (<= 500 each) before the result |
| `stat` | `path, follow=false` | `{kind, size, mtime, mode, resolved, outside_root}` | `follow` follows a final symlink and reports where it resolves |
| `read` | `path, offset=0, len?, max_bytes?, allow_symlink=false` | `{bytes, eof}` | content as DATA frames (id = request id, ascending contiguous offsets), then the result |
| `write` | `path, size, create: "new"\|"truncate", perm?, expect?{size,mtime}, fsync=false, allow_symlink=false` | `{size, mtime, fsynced}` | exactly `size` bytes follow as DATA frames from offset 0; see flow control |
| `rename` | `from, to, overwrite=false, allow_nonatomic=false` | `{atomic}` | overwrite uses `posix-rename@openssh.com` (atomic); without it: `unsupported` unless `allow_nonatomic` (remove + rename, `atomic:false`) |
| `remove` | `path` | `{}` | files and symlinks (never follows); not directories |
| `rmdir` / `mkdir{mode?}` | `path` | `{}` | `mkdir` on existing: `already_exists` |
| `chmod` | `path, mode` | `{}` | permission bits only (`& 0o7777`) |
| `realpath` | `path` | `{path}` | must stay under the root |
| `readlink` | `path` | `{target}` | untrusted display text |
| `cancel` | `target: id` | `{found: bool}` | the target finishes with error `cancelled` |
| `prompt_answer` | `prompt_id, secret?` | `{}` | `secret: null` = the user cancelled |

`ConnectParams`: `host` (name, address or **ssh_config alias**), `port?`, `user?`, `identity_file?`, `proxy_jump?`,
`host_key_policy` (`strict` = `StrictHostKeyChecking=yes`, `accept_new` = `accept-new`, `ask` = `ask`; there is no value that
maps to `no`), `known_hosts_file?` (Brouillon-owned file), `ssh_config_file?`, `interactive` (allow prompts; `false` =
`BatchMode=yes`), `connect_timeout_s?`, `ssh_path?`, `root?` (see 5). Values that could be read by `ssh` as options
(leading `-`, whitespace, quotes, control characters) are rejected with `invalid_argument`; **no secret is ever put in argv**.

### Events

* `{"event":"progress","id":N,"done":bytes,"total":bytes}`: for `write`, `done` bytes are acknowledged by the server.
* `{"event":"list_page","id":N,"entries":[Entry...]}` with `Entry = {name, kind: file|dir|symlink|other, size?, mtime?, mode?, local_unsafe}`.
  `mtime` is Unix seconds from the server and must never be treated as a content hash.
* `{"event":"prompt","prompt_id":P,"kind":"host_key"|"passphrase"|"password"|"keyboard_interactive","text":"..."}`:
  show `text` verbatim (it is ssh's own prompt, sanitised). Reply with `prompt_answer`. For `host_key` the answer is the literal
  `yes` or `no`. The secret goes editor to helper only; the helper never echoes it back and never logs it.
* `{"event":"state","state":"connected"|"lost","reason"?}`: `lost` is sent once when the transport died or timed out;
  later requests fail with `not_connected`.

### Errors (`kind`)

`invalid_argument, outside_root, not_found, permission_denied, already_exists, not_a_directory, conflict, unsupported, too_large,
limit_exceeded, timeout, cancelled, connection_lost, connect_failed, auth_failed, host_key_rejected, host_key_changed, no_ssh,
protocol, busy, symlink_refused, not_connected, failure, internal`. They are provider-neutral (an FTP backend would map to the
same set). `connection_lost`, `protocol` and `timeout` on an SFTP operation poison the session (a `state lost` event follows).
`cancelled`, `conflict`, `not_found`, `permission_denied`... do not.

## 4. Data transfer and flow control

* **Read**: the helper streams DATA frames in file order, each at most 256 KiB, then sends the result. It counts the bytes
  that really arrive and stops with `too_large` at `min(max_bytes, limits.max_read_bytes)`: a server that lies about the
  size or streams forever cannot exceed the cap. `len` omitted = to end of file (still capped).
* **Write**: send the `write` request, then DATA frames (`id` = the request id, `offset` contiguous from 0, <= 256 KiB each, total
  exactly `size`). The editor **must keep at most `limits.write_window` (1 MiB) unacknowledged**: unacknowledged = bytes sent minus
  the last `progress.done`. Exceeding it by more than one chunk is a `protocol` error and aborts the write. DATA frames that
  arrive after a request has finished (for example a write refused with `conflict` before all data was sent) are dropped silently.
* The write targets exactly `path`; **the helper does not implement the safe-save sequence**. SafeSaver composes it:
  `write` to a temp name with `create: "new"` (`O_EXCL`), `fsync`, then `rename` with `overwrite: true` (atomic when
  `posix_rename` is true in the connect result; otherwise the editor decides whether `allow_nonatomic` is acceptable and tells
  the user). `expect {size, mtime}` makes the helper stat first and refuse with `conflict` when the file changed (best effort:
  there is a window between the stat and the open, which the temp+rename sequence closes for the live file).
* Cancellation is cooperative: `cancel{target}` sets a flag checked between chunks.

## 5. Limits (announced in `hello.limits`, defaults)

| limit | default |
|---|---|
| frame | 1 MiB; data chunk 256 KiB |
| path | 4096 bytes; name 255 bytes |
| in-flight requests | 4 (the fifth gets `busy` immediately) |
| listing | 50 000 entries, 64 MiB raw names, 500 per `list_page`, 30 s total; then `truncated: true` |
| read / write cap | 100 MiB each |
| idle | 20 s without a byte from the server aborts the operation (`timeout`) |
| throughput watchdog | fewer than 5120 bytes per 20 s aborts a transfer (`timeout`) |
| SFTP packet from the server | 1 MiB + 4 KiB; handle <= 256 bytes; NAME counts and string lengths checked against the packet |

## 6. Hostile server rules (what the editor can rely on)

* `RemotePath` / `RemoteName` (crate `brouillon-remote-proto::path`) deliberately implement no `AsRef<Path>`, `Deref<str>` or
  `Into<PathBuf>`; `std::fs` cannot take them (compile-fail doctests). Use `RemotePath::cache_key(site_id)` (a hash) to name
  anything local. Every path in a request is parsed through `RemotePath::parse` (absolute, `//` and `.` collapsed, `..` resolved
  lexically, never above `/`); an invalid path makes the request `invalid_argument`.
* Listing names are validated: empty, `.`/`..` (dropped silently), containing `/`, NUL, control characters, bidi/zero-width/tag
  characters, longer than 255 bytes, non-UTF-8, or duplicated entries are **hidden** and counted in `hidden_unsafe`. The server's
  `longname` text is ignored. Names legal remotely but dangerous as Windows file names (`con`, `a:b`, trailing dot...) are listed
  with `local_unsafe: true`.
* `connect.root`: every path argument (and every path the server returns from `realpath`) must be under it, else `outside_root`.
  A final symlink pointing outside the root makes `read`/`write` fail with `symlink_refused` unless `allow_symlink` is set; `stat`
  with `follow` reports `resolved` and `outside_root`. The check-then-open is not atomic (a server can swap the link in between).
* Server text (errors, prompts, readlink) is stripped of control characters and capped at 4 KiB.
* The server cannot make the helper allocate from a declared length: packet length, string length and entry count are all
  checked against what actually arrived. Replies with an unknown id, an empty or oversized DATA, a wrong protocol version or
  a malformed packet kill the session with `protocol`.

## 7. Authentication, prompts and secrets

* Key files, agent, FIDO keys and certificates: handled by ssh. Passwords, passphrases and host-key confirmation: ssh is started with
  `SSH_ASKPASS=<this exe>` and `SSH_ASKPASS_REQUIRE=force`; the exe re-invoked by ssh forwards the prompt to the running helper over a
  loopback socket protected by a random 256-bit token, the helper emits a `prompt` event, the editor answers with `prompt_answer`.
  **Untested against a real ssh.exe** (see Known gaps).
* Host-key policy is the editor's decision per site. `ask` needs `interactive: true`; otherwise it degrades to `strict`.
  A changed host key is a hard failure (`host_key_changed`), there is no override in the protocol.
* The editor holds secrets (Credential Manager); it sends one only in response to a `prompt` event. Secrets are wiped from the
  helper's memory on drop, are never written to disk, argv or logs, and `Debug` prints `[redacted]`.

## 8. Known gaps

* Not run against a real sshd: SFTP through a real `ssh.exe`, host-key prompts, password/passphrase askpass through ssh.exe,
  agent and key auth. What is verified with the real OpenSSH 10.3: `ssh -G` resolves our option list as intended (policy values,
  alias from a user config, forwarding disabled even if the alias enables them), and a connection to a closed local port fails
  cleanly with a classified error.
* Non-UTF-8 file names are hidden (counted), not displayed lossy.
* Throughput watchdog is evaluated when data arrives or the idle timeout fires; a server that trickles one byte per idle period
  is stopped by the idle timeout, not by the rate check.
* Writes through the pipe to ssh have no timeout of their own; a stalled ssh is ended by the idle timeout on the reply side, the
  helper then kills the child, which unblocks the writer. ssh's own `ServerAliveInterval=15` x 3 also ends a dead link.
* The askpass channel is a loopback TCP port; a named pipe would avoid opening a port at all (needs hand-written FFI).
* FTP/FTPS: see section 9 and docs/REMOTE_FTP.md (tested against an in-process fake server only, never a real FTP server).
* No compression; no ControlMaster (Windows OpenSSH has none): one ssh process per session.

## 9. FTP / FTPS (additive, protocol version stays 1)

Same stdio protocol, same operations, same errors; only `connect` and three results grow. Every new field is optional with a
default, so an editor that does not know them keeps working (and keeps getting SFTP).

`connect` params (new, both optional): `protocol` = `"sftp"` (default) | `"ftps"` | `"ftp"`, and `ftp` =
`{ tls: "explicit"|"implicit" (default explicit), fingerprint_policy: "strict"|"pinned"|"ask" (default ask),
pinned_fingerprint?: sha256 hex, allow_plain: bool (default false), password?: string }`. With `ftps`/`ftp` the ssh fields
(`identity_file`, `proxy_jump`, `host_key_policy`, ...) are ignored; `host`, `port` (21, or 990 for implicit), `user` (absent =
anonymous), `interactive`, `connect_timeout_s`, `root` keep their meaning.

* `ftp` (plain) is refused with `invalid_argument` unless `allow_plain` is true. The result then has `insecure: true`: show it for
  the whole session (permanent warning). There is no opportunistic upgrade and no option that disables certificate validation.
* Certificate: system roots + host name; `pinned_fingerprint` (SHA-256 of the leaf, hex) is ALWAYS enforced when set: another
  certificate is `host_key_changed`, even if a CA signed it, and no prompt can override it. `pinned` accepts a matching pin without
  a chain (known_hosts semantics), `strict` never lets a pin replace the chain. `ask` + `interactive`: an unknown certificate raises
  a `prompt` event (`kind: host_key`, text with the SHA-256 fingerprint); answer `"yes"` to trust it. The helper then reconnects
  pinned to exactly that certificate (nothing secret was sent yet) and reports it in `ftp.cert_fingerprint` /
  `ftp.cert_trust = "user_confirmed"`: the editor stores it as the pin for next time. A refusal or a missing answer is
  `host_key_rejected`.
* The password comes from `ftp.password` or, when absent and `interactive`, from a `prompt` event (`kind: password`). It is only
  sent after the TLS handshake of an `ftps` session succeeded, is redacted by `Debug`, and is never logged or put on a command line.
* Failure mapping (no new error kinds): TLS refused / downgrade -> `connect_failed`; untrusted certificate -> `host_key_rejected`;
  changed pin -> `host_key_changed`; 530 -> `auth_failed`; 550 -> `not_found` / `permission_denied` / `already_exists` /
  `not_a_directory` by the reply text; a malformed or oversized reply -> `protocol` (poisons the session).

`connect` result (new): `protocol`, `insecure`, and `ftp`: `{ tls, tls_version?, cert_fingerprint?, cert_trust?
("system_roots"|"pinned"|"user_confirmed"), atomic_replace (always false), mlst, mdtm, size, utf8, supports_hash (true only if
HASH/XCRC/XMD5/XSHA* is offered), hash_algos[], warnings[] }`. `posix_rename` and `fsync` are false; `extensions` lists the FEAT
capabilities that matter; `sftp_version` is 0.

`write` result (new): `atomic` (always false on FTP) and `swapped`. Semantics on FTP: `create: "truncate"` uploads to
`.<name>.brouillon-w-<rand>.tmp` in the same directory, checks the size with `SIZE`, re-checks `expect`, then `RNFR/RNTO` over the
target; if the server refuses to rename over an existing file it uses the 3-step swap (target -> `.<name>.brouillon-old-<rand>.tmp`,
temp -> target, delete old; the old file is moved back if step 2 fails) and returns `swapped: true`. A cut connection leaves
the old content and at most an orphan `.brouillon-w-*.tmp`. `create: "new"` writes the exact name (existence is checked first; a
race is possible). `rename` with `overwrite` is reported `atomic: false`; without `allow_nonatomic` a refusal is `unsupported`.
`expect` compares size and, when the server reports one, mtime. `readlink` is `unsupported`, `realpath` returns the lexical path of
an existing entry, `symlink` targets are unknowable (with a `root`, a final symlink is refused unless `allow_symlink`).
