# Security Policy

## Supported versions

Only the latest published build while the project is 0.x.

## Reporting a vulnerability

Please use GitHub private vulnerability reporting:
<https://github.com/lrochetta/brouillon/security/advisories/new>

Include: version and build, operating system, steps to reproduce, impact, and a fix if you have one.

## What to expect

This is a single-maintainer project. Reports are handled on a best-effort basis; you will get an acknowledgement as soon as the maintainer can read it. Reporters are credited in the changelog unless they prefer anonymity.

## Please do not

Open a public issue for a vulnerability, test on systems or files that are not yours, or disclose before a fix is released.

## Security properties of the current alpha

No telemetry and no account. The editor's only own network request is a signed update check (Ed25519, pinned public keys, can be switched off). Features that add network exposure (agent access through Brouillon Link, remote files) are off by default. Builds are not yet code-signed: verify the SHA-256 published with each release. Details: [docs/SECURITY.md](docs/SECURITY.md).
