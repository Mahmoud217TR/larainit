# Changelog

## 0.2.0 — Unreleased

Native implementation and local Ubuntu/Docker story checks are complete. Final feature acceptance
and hosted CI are separate gates; this entry is not a claim of a published release.

### Changed

- Replace the Bash wrapper with one stable Rust executable (edition 2024, MSRV 1.85.0).
  Build with `cargo build --release --locked`, then deliberately place the native executable on PATH.
- Preserve `new` as the actual Laravel Installer, plus explicit Bash/PHP/Composer/Node/npm/Artisan
  commands, exact UTF-8 argument values and numeric child statuses. Nonrepresentable bytes fail
  explicitly; no shell assembly or lossy substitution.
- Correct pipelines/redirection/EOF: inherited stdin and separate stdout/stderr, TTY only with all
  three terminal streams, controlled terminal restoration and captured-ID-only signal cleanup.
- Require healthy Docker for both `auto` and `docker`, preserving the user's context/environment;
  only pre-existing local images are used (`larainit:latest` default), inspected by immutable ID
  with `--pull=never`, never automatically pulled/built or replaced after a race.
- Retain PHP 8.5, Composer 2, Node 24 and pin Laravel Installer 5.32.0 in the toolchain Dockerfile.
- Add strict optional XDG TOML settings and CLI > environment > file > default precedence,
  read-only `doctor`, host-writable workspace with supplementary groups, and dedicated persistent
  Composer/npm download caches without credentials/global-home mounts or ownership repair.

### Intentional incompatibilities / limits

- Host Bash is no longer required. The repository prototype is retired after documented,
  tested native migration; an existing manually installed wrapper is never automatically replaced.
- `engine`, `build`, `config` are reserved local names returning status 1, not external passthrough.
- Unknown/missing/repeated leading flags are usage errors (2). Global flags must precede COMMAND;
  after COMMAND they are external data. Bare nonterminal invocation refuses to guess a shell.
- Invalid effective overrides and malformed/unreadable/unknown-key config fail without fallback.
- Docker only; Podman is deferred. Ubuntu 26.04.1 x86_64 with Docker 29.8.1 native rootful/no remap
  is the recorded local environment. WSL2, Desktop, rootless/remapped/remote Docker, SELinux-specific
  setups and other platforms remain unverified, not supported by compilation alone.
- Diagnostics allow ordinary Docker-owned bookkeeping and existing configured socket activation,
  but never explicit service management, workload requests, provisioning or user-state writes.
- Interruption cleanup is bounded best effort; missing identity, daemon loss, force kill or power
  loss can leave containers. No bulk removal, privilege escalation or automatic repair.
- Manual source/image preparation only: no image publication, native distribution installer,
  runtime version matrix, self-update, application services or orchestration.
