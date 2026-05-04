# Security Policy

## Reporting a Vulnerability

If you've found a security issue in Atlas, please report it privately rather than opening a public GitHub issue.

The preferred channel is GitHub's [private security advisory](https://github.com/HytaleModding/atlas/security/advisories/new) for this repository. This keeps the report confidential while we work on a fix.

If that isn't available to you, email the maintainers directly. A maintainer email address will be added here once Hytale Modding takes over hosting.

## What to include

- A clear description of the issue
- Steps to reproduce (or a proof-of-concept if you have one)
- Impact: what an attacker could do
- The version or commit you reproduced against

## What to expect

- An acknowledgement within a few days
- A short discussion to confirm we can reproduce the issue
- A fix coordinated with you before any public disclosure

## Scope

In scope:

- The Atlas desktop client (Tauri + Rust + React)
- The central data-package build pipeline
- The signing and verification path between them

Out of scope:

- Issues that require physical access to a user's machine
- Denial of service from a user against their own machine
- Vulnerabilities in upstream dependencies that have no Atlas-specific exploit path (please report those upstream)
