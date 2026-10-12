# Changelog

## 0.3.0 — In development (not release-certified)

- Add local `engine` read/set, strict saved-value validation, preserved recognized image settings,
  coordinated atomic saves and explicit postcommit durability/unavailable-readiness statuses.
- Add required-context local Docker builds with native file paths, independent tags, streaming
  cancellation and private IID/immutable-image/tag verification. Artifacts can remain after failure;
  no ordinary command implicitly pulls/builds or falls back after selection.
- Add engine-specific image/run/cleanup adapters and independent diagnostic candidate reports.
  Add candidate checks for fingerprinted Ubuntu 26.04/Podman 5.7.0 default prepared
  rootless/crun/SQLite state, prepared startup paths, absent deletion leftovers,
  nonrecovering permanent/volatile storage metadata and the packaged seccomp profile.
  XFS-backed overlay is refused before observation to avoid quota initialization.
  Info helper lookup/fingerprints (including pasta's AVX2 companion), reported paths
  and dpkg configuration/environment are candidate-checked; execution remains uncertified.
  Passive pause-root/mount-view/propagation guards, canonical PID parsing and
  pre-exec-hook refusal are implemented; their dedicated-account rerun is pending.
  Prepared systemd binfmt automount stacks are compared by verified parent chain;
  untriggered/ambiguous and storage/root stacks remain refused.
  Namespace-filesystem root IDs are parsed separately from absolute mount paths,
  preserving exact object identities and field-specific refusal diagnostics.
  Podman remains disabled pending startup/security and live fixture certification.
- Configuration compatibility: invalid saved values now fail even when overridden. Only `config`
  remains reserved; `engine` and `build` are local builtins. Build-local engine overrides global selection.
- Fresh local Docker integrations, both-signal build cancellation/tag-race, PTY/installer and MSRV
  checks passed on 2026-10-12. Candidate SQLite metadata reads use a tested read-only Bubblewrap
  sandbox; real rootless workload certification, cold-store builds and hosted CI remain open.
  Unsupported platform categories and Feature 001's separate acceptance gates remain open.

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
