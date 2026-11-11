# FTP / FTPS in the remote helper

Status: implemented in `_bmad-output/build/remote-helper/crates/brouillon-remote/src/ftp/` (+ `src/backend.rs`), tested against an
**in-process fake server only**. No real FTP server, no external host, and not one socket was used by the tests.
Protocol additions: `docs/REMOTE_HELPER_PROTOCOL.md` section 9.

## Decision: own client on std + rustls, not `suppaftp`

`suppaftp` 12.2.1 (MIT OR Apache-2.0, sync API, `rustls-ring`) was resolved with `cargo tree` (not built into the helper). It would add
`chrono`, `lazy-regex` + `regex` (+ `aho-corasick`, `regex-syntax`), `log`, `once_cell`, on top of the same rustls/ring set we need
anyway. Reasons it was not used:

* **Testability / policy**: it is built on `TcpStream`; an in-process fake server (our requirement, because the machine's security
  product deletes test executables that do many loopback socket cycles) is not possible without sockets. Our client is generic over
  `Read + Write` and takes a `Dialer`.
* **Hostile servers**: its reply reader and `list()` return unbounded `String`/`Vec<String>`; we need bounds on reply line, line count,
  reply bytes, listing bytes/entries/time, plus strict LIST parsing that hides (and counts) bad lines. Wrapping that would mean
  re-implementing the protocol layer anyway.
* **Security-critical details we want in our hands**: refusal when buffered plaintext exists at the TLS upgrade, `AUTH TLS` failing
  closed, mandatory `PBSZ 0`/`PROT P`, the data address always being the control peer, pinning inside the rustls verifier.
* **Size**: the regex stack alone is several hundred KB for ~300 lines of parsing we write strictly by hand (`parse.rs`, unit tested).

The client is about 2,500 lines including parsers and their unit tests (`ctrl.rs` control channel, `tls.rs` verifier, `parse.rs`,
`connect.rs`, `session.rs` operations, `backend.rs` dispatch), plus ~1,500 lines of in-process fake server and tests. It reuses `rustls` 0.23 (ring provider), `rustls-native-certs` (OS roots) and `ring` (SHA-256).

## Dependencies, licences, size

New runtime crates (all via rustls): `rustls` 0.23.46 (Apache-2.0 OR ISC OR MIT), `rustls-webpki` 0.103.16 (ISC),
`rustls-pki-types` 1.15.1 (MIT OR Apache-2.0), `rustls-native-certs` 0.8.5 (Apache-2.0 OR ISC OR MIT), `ring` 0.17.14
(Apache-2.0 AND ISC, **contains C and assembly** derived from BoringSSL: this relaxes the "no C code" property of the SFTP-only helper),
`untrusted` 0.9 (ISC), `subtle` 2.6 (BSD-3-Clause), `schannel` 0.1 + `windows-sys` (MIT OR Apache-2.0), `once_cell`. Build-time only:
`cc`, `find-msvc-tools`, `shlex`. All are compatible with MIT distribution; the ISC / BSD-3 / ring notices must go in the
third-party notices of a release. No tokio, no async runtime.

| | before | after | delta |
|---|---|---|---|
| `brouillon-remote.exe` release (opt-level s, fat LTO, strip, panic=abort) | 564,224 B | **1,561,088 B (1.49 MiB)** | +996,864 B |
| clean release build, `-j 2` | ~25 s | ~65 s | +40 s |

The growth is rustls + webpki + ring (crypto, X.509, TLS 1.2/1.3), which no FTPS client can avoid; the FTP client itself is small.
It stays at the ~1.5 MB ceiling. To shrink further: drop `tls12` (about 100 KB, but many FTP servers are TLS 1.2 only),
or put FTP/FTPS in a second helper executable so SFTP-only users keep the 564 KB binary (not done: one binary, one protocol).

## Security properties and where they are proven

| Property | How | Test |
|---|---|---|
| TLS cannot be stripped | explicit FTPS needs `234` to `AUTH TLS`; anything else is `connect_failed` before `USER`/`PASS` | `stripped_auth_tls_fails_closed_for_ftps_before_any_credential` |
| No plaintext injection at the upgrade | bytes already buffered when `234` was parsed abort the upgrade (`protocol`) | `plaintext_injected_after_234_aborts_the_upgrade`, `ctrl::tests::tls_upgrade_refused_when_extra_bytes_were_buffered` |
| Data channel protected | `PBSZ 0` + `PROT P` mandatory (refusal = failure); TLS on every data connection | `explicit_ftps_full_roundtrip...` (command order), reuse tests |
| Credentials only inside TLS | password sent after the handshake of an FTPS session; fake records `pass_in_clear` | `explicit_ftps_full_roundtrip...`, `ask_policy_prompts...` |
| Plain FTP is opt-in and flagged | `allow_plain` required, refused before any connection; result `insecure: true` + warning; no opportunistic upgrade | `plain_ftp_is_refused_unless_opted_in...`, helper test |
| Certificate validation cannot be disabled | custom `ServerCertVerifier` delegates to `rustls-webpki` (OS roots + host name); handshake signatures verified by ring; no setting skips it | `unknown_certificate_is_rejected_without_a_credential` |
| TOFU pin, user confirmed | unknown cert -> `prompt` (`host_key`) with SHA-256; on `yes` the helper reconnects pinned to that exact fingerprint and reports it | `ask_policy_prompts...`, `trust_on_first_use_goes_through_the_prompt_event` |
| Changed fingerprint = hard stop | a set pin is always enforced, even for a CA-signed cert, no prompt can override | `changed_fingerprint_is_a_hard_stop...`, helper test |
| Pin never replaces the chain under `strict` | `pin_suffices = false` | `strict_policy_never_lets_a_pin_replace_the_chain` |
| Data connection = control peer | address in `227`/`229` is parsed for validity and discarded; EPSV with an address is a protocol error | `pasv_and_epsv_hosts_are_ignored`, `an_address_inside_an_epsv_reply...` |
| Bounded replies | line 4 KiB, 512 lines, 64 KiB per reply | `ctrl::tests::malformed_and_oversized_replies_fail`, `an_oversized_reply_kills_the_session...` |
| Hostile `LIST`/`MLSD` | strict Unix/DOS/MLSx parsers; names through `RemoteName` (no `/`, `..`, NUL, controls, bidi); duplicates, malformed and over-long lines hidden and counted; byte/entry/time caps; bare `LIST` after `CWD` (no globbing argument) | `hostile_list_output_...`, `hostile_mlsd_output_...`, `an_endless_listing_is_truncated...` |
| No command injection | `RemotePath` rejects CR/LF/NUL; `Ctrl::send` refuses them again | `commands_cannot_be_injected` |
| Timeouts | socket idle timeout from `limits.op_idle_timeout_s`, throughput watchdog, listing wall-clock cap | `a_stalled_server_times_out...`, `a_dead_data_connection_times_out` |
| Old-or-new content | upload to a hidden temp, `SIZE` check, re-check `expect`, `RNFR/RNTO`; 3-step swap if the server refuses rename-over | `a_dropped_connection_mid_upload...`, `a_short_store_is_detected...`, `servers_that_refuse_rename_over...` |
| Truthful capabilities | `atomic_replace`/`atomic` always false; `supports_hash` only if HASH/XCRC/XMD5/XSHA* in `FEAT` | `hash_support_is_reported_only_when_offered`, roundtrip |
| Secret hygiene | `Secret` redacts `Debug`; no logging in the module; error text never contains commands; password absent from the protocol output | `the_password_is_only_sent_over_tls...`, helper test |
| Truncated downloads | missing TLS `close_notify` accepted only if the byte count equals `SIZE`; non-2xx final reply is an error | `read_caps_prefix_and_truncation_detection` |

## TLS session reuse on data connections

Some servers (vsftpd `require_ssl_reuse=YES`, the default there; ProFTPD `TLSOptions`; Pure-FTPd with TLS 1) refuse a data
connection whose TLS session was not resumed from the control connection's. `suppaftp`/rustls behaviour is the same as ours since
both rely on rustls: a `ClientConfig` holds a `ClientSessionStore` (in-memory, 256 entries, enabled by default for TLS 1.2 session
IDs/tickets and TLS 1.3 tickets) keyed by `ServerName`. We build **one** `ClientConfig` and one `ServerName` per session and use
them for the control handshake and every data handshake, so the data connection offers the control session. Proven against a
rustls-based fake server that rejects any non-resumed data handshake: all data handshakes resumed (`resumed > 0, full == 0`) on
TLS 1.3 and on TLS 1.2-only (`servers_that_require_session_reuse_work_on_tls13_and_tls12`).

What can still fail with real servers (unverified):

* TLS 1.3 servers that issue no tickets / session IDs, or old OpenSSL-based servers whose TLS 1.3 reuse check is buggy
  (known with some vsftpd/OpenSSL 1.1.1 combinations): the data handshake is full and the server answers 425/522. There is no
  "force TLS 1.2" switch yet; adding `tls_max: "1.2"` to `ftp` options is the obvious workaround.
* rustls uses each TLS 1.3 ticket once; the server hands out new tickets on every resumed handshake, so a long session keeps working.
* Resumed handshakes skip certificate verification (the session was verified); a full data handshake is checked against the
  control certificate (`pin_session`), so a data channel cannot present another certificate.

## Operations on FTP (what differs from SFTP)

* Binary mode (`TYPE I`) only, `OPTS UTF8 ON` when `FEAT` lists UTF8, passive mode only (EPSV, else PASV), `REST` for offsets.
* `list_dir`: `MLSD` when `FEAT` has MLST, else `CWD` + `LIST` (Unix and DOS styles). LIST dates are server-local and approximate.
* `stat`: `MLST`; without it `SIZE` (+ `MDTM`) means regular file, `CWD` success means directory (a server that answers `SIZE`
  for directories would be misreported as a file). Mode only comes from the `UNIX.mode` fact.
* `write`: see protocol doc section 9 (temp + rename, `swapped`). `perm` / the previous mode is applied with `SITE CHMOD`, best effort.
* `chmod` is `SITE CHMOD`; `realpath` is lexical; `readlink` is unsupported; symlink targets are unknowable.
* One control connection, operations serialised by a mutex (the protocol still allows 4 requests in flight; they queue).
* `cancel` is honoured between data chunks; a read blocked on a silent server ends at the idle timeout.

## Not tested against real servers (honest list)

Everything above was exercised only with the fake. Unverified with vsftpd, ProFTPD, Pure-FTPd, FileZilla Server, IIS, or hosting
panels: TLS 1.3 reuse with OpenSSL servers, servers that reject `EPSV` in odd ways, servers that need `PROT P` before `USER`,
`SIZE`/`MDTM` quirks, `RNTO` overwrite behaviour, `SITE CHMOD`, `MLSD` fact variants, IPv6 (`EPSV` is used, the code is
address-family agnostic but untested), servers that never send TLS `close_notify`, Windows root-store loading in the field
(`rustls-native-certs` is used; with an empty store only pinned certificates can connect), and the real `TcpDialer` (no socket test by design).
No FTP proxy / SOCKS support. Active mode and `PORT` are not implemented on purpose.
