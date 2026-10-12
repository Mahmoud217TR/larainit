# Larainit

**A disposable, Docker-powered Laravel CLI toolchain. No local PHP, Composer, or Node.js required.**

Larainit runs the Laravel Installer and common development commands inside a temporary Docker container, while keeping your project files on your machine. Use it to bootstrap Laravel applications **before** installing Laravel Sail or setting up a project-specific Docker Compose stack.

**Build the toolchain once, then prefix your commands with `larainit`.** No local runtime setup, no toolchain files added to your application.

The v0.2.0 implementation is a **native Rust executable**, replacing the Bash wrapper.
All six story checkpoints have local execution evidence; final release acceptance and hosted CI
are tracked separately. No published binary, image, installer or self-update mechanism is provided.

**Feature 002 / v0.3.0 is in development.** This checkout adds saved engine preferences,
explicit local builds and engine-aware diagnostics with mock/stable/MSRV validation.
Fresh local Docker suites, terminal/installer checks and MSRV checks passed on 2026-10-12.
Rootless Podman certification and unified/hosted-CI acceptance remain outstanding. Podman remains
disabled while the [Ubuntu 26.04/Podman 5.7.0 candidate profile](docs/podman-profile.md) is validated;
executable presence or a matching version alone is not support.

![Larainit showcase](art/showcase.webp)

## Table of contents

- [Features](#features)
- [Requirements](#requirements)
- [Install](#install)
- [Quick start](#quick-start)
- [Usage](#usage)
  - [Command behavior](#command-behavior)
- [How it works](#how-it-works)
- [Settings and caches](#settings-and-caches)
- [Diagnostics and exit status](#diagnostics-and-exit-status)
- [Verification](#verification)
- [Larainit vs. Sail](#larainit-vs-sail)
- [Troubleshooting](#troubleshooting)
- [Scope](#scope)
- [License](#license)

## Features

- Create projects with the interactive **Laravel Installer** (`laravel new`), not `composer create-project`.
- Run **PHP, Composer, Node.js, npm, and Artisan** without installing their runtimes on the host.
- Open a shell with `larainit bash`.
- Mount your current directory into the container at `/workspace`.
- Run commands under your host UID/GID to avoid root-owned generated files on Linux.
- Keep project-specific Docker and Sail configuration separate from the bootstrap toolchain.

## Requirements

- Installed Docker CLI and a healthy, independently configured Docker server. `auto` and `docker`
  both require Docker; there is no alternative engine or context fallback. Keep your normal Docker
  environment/context; Larainit does not override it with a private socket.
- A Linux x86_64 native executable and compatible system C library (the default GNU build uses
  glibc). Other platforms and Docker modes require separate verification; they are not implicitly supported.
- No host Bash, Rust, PHP, Composer or Node runtime is required to run the native executable.
  Rust/Cargo and a C linker are only required to build it from source.

**Observed (2026-10-12):** Ubuntu 26.04.1 LTS x86_64, kernel 7.0.0-38-generic, Docker server 29.8.1,
native rootful Docker without user-namespace remapping, host UID/GID 1000:1000. The tested image
supplies PHP 8.5.11, Composer 2.10.3, Node 24.21.0/npm 11.19.0 and Laravel Installer 5.32.0.
The Dockerfile retains PHP 8.5/Composer 2/Node 24 tags and pins the installer at 5.32.0;
moving base tags mean future builds require fresh verification.
WSL2 is the intended Windows route with the Linux binary, but is **unverified**. Remote bind mounts,
rootless/remapped Docker, Docker Desktop, SELinux-specific setups, other distributions/architectures,
macOS and native Windows are also unverified. Podman is **deferred/unsupported** in this iteration.

## Install

Clone the repository and build the image:

```bash
git clone https://github.com/Mahmoud217TR/larainit.git
cd larainit
docker build -t larainit:latest .
```

Build the native executable with stable Rust (MSRV **1.85.0**, edition 2024):

```bash
cargo build --release --locked
```

Place it in a **new, isolated** directory, without overwriting an installed binary:

```bash
bin_dir="$(mktemp -d)"
cp target/release/larainit "$bin_dir/larainit"
export PATH="$bin_dir:$PATH"
cd /path/to/your/project
larainit --help
larainit --version
```

The executable does not need this checkout or a Dockerfile alongside it. Help/version work without
Docker, a local image, configuration or network access. Container commands require healthy Docker and
the **pre-existing local** `larainit:latest` image (or an explicit override); no automatic pull/build
or engine startup is performed. The image build above is an explicit operator action from the source
checkout, not executable startup. The repository Bash prototype is retired; use the native
`target/release/larainit` for v0.2.0 validation. To migrate, deliberately replace your own old wrapper
only after testing the placed native binary; these instructions never overwrite an existing install.

## Quick start

Once installed, create an application from any writable directory:

```bash
larainit new my-app
cd my-app
larainit npm install
larainit npm run build
```

Follow the installer's prompts to choose your application setup. Your project stays on your machine; each command's container is removed automatically when it finishes.

Already have a project? Just `cd` into it and run `larainit composer install` or `larainit php artisan --version`.

## Usage

Run commands **from the directory where you want to work**:

```bash
# Start the interactive Laravel Installer
larainit new my-app

# Open a Bash shell in the CLI container
larainit bash

# Install or update Composer dependencies
cd my-app
larainit composer require laravel/sail --dev
larainit composer update

# Run PHP, Artisan, and Node tooling
larainit php artisan --version
larainit npm install
larainit npm run build
```

### Command behavior

| Command | Runs in the container |
| --- | --- |
| `larainit new my-app` | `laravel new my-app` |
| `larainit bash` | `bash` |
| `larainit composer install` | `composer install` |
| `larainit npm install` | `npm install` |
| `larainit php artisan test` | `php artisan test` |
| `larainit` with all three streams terminal | interactive `bash` |
| `larainit doctor` | local prerequisite report; no container execution |

```text
larainit [--engine auto|docker|podman] [--image IMAGE] [--] [COMMAND [ARG...]]
larainit [GLOBAL_OPTIONS] engine [auto|docker|podman]
larainit [GLOBAL_OPTIONS] build --context PATH [--file PATH] [--tag IMAGE] [--engine auto|docker|podman]
larainit --help
larainit --version
```

Global flags apply **only before COMMAND**. Unknown/missing/repeated leading options are usage
errors. A leading `--` is consumed; after the command, every value belongs to that executable,
including empty strings and `--`, `--help`, `--version`, `--engine`, and `--image`. Values are never
joined into a host shell program. Docker transport requires UTF-8 without NUL; unrepresentable
native bytes/path encodings fail explicitly rather than being replaced. CSV-quoted bind paths handle
spaces, Unicode, commas, quotes and colons; CR/LF and edge-whitespace paths are rejected.

`new` always runs the actual `laravel new` installer. `doctor` accepts no arguments.
`engine` and `build` are local builtins; `config` remains **reserved**, returning “not yet supported” without container
passthrough. Other first command tokens request the named container executable. For a reserved
name intentionally invoke it explicitly via a path, if supplied by your custom image.
Bare invocation refuses nonterminal stdin/stdout/stderr with an explicit-command remedy; explicit
`larainit bash` supports piped scripts and EOF. Container commands retain stdin and separate inherited
stdout/stderr; `-t` is requested only when all three are terminals, with no wrapper stdout banner.

## Settings and caches

Configuration is optional, read from `$XDG_CONFIG_HOME/larainit/config.toml`, defaulting to
`$HOME/.config/larainit/config.toml`. Only `engine VALUE` creates/saves this file. Its only accepted keys are
optional **strings**:

```toml
engine = "auto"       # case-sensitive: auto, docker or podman (currently unavailable)
image = "larainit:latest"
```

Each value resolves independently: **CLI > `LARAINIT_ENGINE`/`LARAINIT_IMAGE` > file > default**.
Defaults are `auto` and `larainit:latest`. Empty explicit overrides are invalid, not absent.
Invalid effective values never fall back. Malformed, unreadable, wrong-type or unknown-key config
remains an error even with CLI overrides; every present saved value is validated, even if overridden.
No engine credentials or other arbitrary config keys are stored.

`larainit engine` reads saved/effective preference and readiness without writes.
`larainit engine docker` atomically saves its argument, ignoring transient engine overrides for that save.
It preserves a recognized saved image, not comments/formatting. Cooperating setters use a directory
lock; unsafe links/permissions and detected external edits are refused, never repaired. Precommit
failure preserves the old config. A postcommit sync failure returns 1 with **saved, durability
unconfirmed**, without rollback or readiness probing. A durable unavailable choice returns 1 with
**preference saved; execution unavailable**, retaining the preference. Uncoordinated external changes
at the final replacement race are outside the guarantee.

Engine precedence for builds is **build-local option > global CLI > environment > saved > auto**.
Explicit choices never fall back. Auto prefers healthy Docker; Podman cannot currently pass its
certification gate. There is no fallback after image, mount, run, build or verification failure.

### Explicit local builds (development checkout)

```bash
larainit build --engine docker --context /path/to/larainit/source
larainit build --context /path/to/source --file docker/Dockerfile --tag custom/toolchain:dev
larainit --engine docker --image custom/toolchain:dev php -v
```

Context is mandatory and never inferred. Relative files resolve inside that context; canonicalized
symlinks are accepted only within its boundary. Defaults are `Dockerfile` and `larainit:latest`,
independent of image overrides/saved settings. Remote/Git/stdin sources, parent traversal, digest
destinations and option-like tags are refused. Build paths remain native argv (unlike tool JSON
transport). The temporary directory must permit private IID scratch outside the context.
Builds use the selected engine's builder/ignore rules and may fetch base images as part of this
explicit request; no separate pull or builder/service bootstrap is performed.

Output streams without probe limits or a normal execution timeout. Zero engine status alone is not
success: a private no-follow IID, immutable local result and target tag must agree in the selected
store. Unsupported builder output, missing image/IID or retag mismatch returns 1 with **build
completed; verification failed**. Verification is point-in-time, not permanent ownership of a tag.
Custom tags never save an image preference. Cancellation forwards the requested signal to the
owned client group, then escalates boundedly; it never uses an IID as a container cleanup target.
Base images, layers, caches, intermediates or a completed tag may remain on failure/cancellation;
there is no image pruning, rollback or artifact-removal promise.

Podman remains disabled for ordinary commands **and builds**. The
[candidate prepared Ubuntu 26.04 amd64 profile](docs/podman-profile.md) checks exact binary/configuration
fingerprints, no engine/storage overrides, default owned paths, existing rootless namespace/locks,
current boot marker, schema-1 SQLite metadata and crun. The metadata reader uses an existing
Bubblewrap read-only sandbox; standalone SQLite `-readonly` is insufficient. Live fixture certification
and real rootless workload acceptance are still pending. Candidate checks are point-in-time; concurrent configuration/storage changes are
unsupported. Unknown/cold/refresh-required profiles are not probed. No installation, refresh, rootful fallback,
relabeling, ownership repair or source fetching is performed. Remote/machine/rootful Podman,
WSL2, SELinux and untested distribution/group/mount modes are unsupported/unverified.

XDG bases must be absolute; unset/empty/relative values use HOME fallbacks. Missing HOME is an error
only when a required location cannot resolve through an absolute XDG override.

Runnable invocations create these **dedicated download** caches after prerequisites succeed:

- `$XDG_CACHE_HOME/larainit/composer` → `/cache/composer` (`COMPOSER_CACHE_DIR`).
- `$XDG_CACHE_HOME/larainit/npm` → `/cache/npm` (`npm_config_cache`).
- Default cache base: `$HOME/.cache`. New directories use mode 0700; existing modes/ownership
  are left unchanged. File/inaccessible/creation failures identify the path; no bypass or repair.

The container HOME/XDG/Composer global state is ephemeral under `/tmp`; installer PATH remains
`/opt/composer/vendor/bin`. Only the workspace and two cache directories are host-mounted by default:
no SSH/auth.json/npmrc/token/engine-socket mounts, no global host-home copying, relabeling or recursive
chown. Project files and download caches persist; global runtime installs do not.
Simultaneous projects use the package managers' own cache coordination, without a Larainit global
lock. Untrusted packages/scripts still run with access to these three intended mounts; only request
commands and projects you trust.

## Diagnostics and exit status

`larainit doctor` reports platform/version verification limits, independent engine candidate outcomes,
effective image/setting sources, local immutable image identity, config/workspace/cache observations
and practical remedies. Missing optional config and absent/preparable caches are informational.
Critical failures return 1; independent observations continue, blocked ones say “not checked”.
Unused-engine failures are informational; only the required selected path determines engine/image
readiness. A strict Podman diagnostic can observe unused Docker, but never selects/executes it as a fallback.
Permission inspection is evidence, **not a guarantee** that future mounts/writes will succeed.

Doctor/help/version do not request workloads or provision images, prepare caches, install packages,
write user files or explicitly install/enable/start services. Ordinary Docker inspection can produce
Docker-owned **bookkeeping** or trigger **existing user-configured socket activation**; diagnostics do
not promise unchanged daemon/service state. No arbitrary CLI plugin metadata is invoked.
Server/local-image observations use null stdin, a five-second deadline and combined 64-KiB output cap;
tool output and runtime are not captured or given an arbitrary deadline.

- Usage failures: **2**. Wrapper operational/reserved/critical-doctor failures: **1**.
- Numeric child statuses pass through, including Docker **125/126/127**; signal-only status maps to
  **128 + signal**. Cleanup diagnostics do not replace a returned tool status.
- SIGINT/SIGTERM use bounded, **best-effort** cleanup of only the validated invocation container ID:
  five-second stop grace, eight-second stop-client limit, three-second forced-removal limit, and
  two-second final client-reaping limit. Terminal state is restored on controlled exits.
- Missing/late/invalid identity, daemon loss, SIGKILL, power loss or kernel stalls may leave a
  container. No guessed-name/bulk removal is attempted. Inspect the specific reported invocation/ID
  manually once Docker is reachable; never prune unrelated user containers as a “fix”.

## Verification

From the checkout, with the native toolchain installed:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo +1.85.0 check --locked --all-targets
```

Real Docker suites are opt-in; default ignored tests are **not** real-engine acceptance evidence.
Run permission regressions under a non-root development account: root can bypass unreadable mode
bits, so that test fails explicitly rather than counting a passing skip. Larainit never creates users
or changes process identity to manufacture permission evidence.
Developer tests honor `TMPDIR`. For configuration persistence checks, use a private temporary
directory under safe ancestors; a group-writable source checkout is intentionally rejected.
If temporary storage fills, choose an available safe location rather than deleting shared files
or weakening the configuration permission checks.
On an isolated test engine with an explicitly prepared compatible local image:

```bash
export LARAINIT_TEST_IMAGE=larainit:latest
cargo test --release --locked --test engines --test laravel --test caches --test cache_concurrency --test standalone --test diagnostic_safety --test build_real -- --ignored --nocapture --test-threads=1
python3 tests/support/pty-check.py "$PWD/target/release/larainit" "$LARAINIT_TEST_IMAGE" --installer
```

`build_real` explicitly builds derivative images from the supplied local base. It checks native
ignore rules, verified image identity, ordinary execution and unchanged saved settings; it retains
the printed test tags/IDs and builder artifacts for inspection rather than pruning shared storage.
Authorized local runs on 2026-10-12 passed all 13 Docker integrations (zero skips) and six PTY/installer cases,
including `build_real` success/failure/ignore checks, both-signal cancellation and a controlled
tag race. These are not rootless or unified acceptance. Existing container/image identities
were preserved; no newly created invocation containers remained after validation.

Requested integrations fail if prerequisites are unavailable; there is no passing skip. Tests use
isolated paths, explicit package downloads and disposable local derivative images for races. The
supplementary-group test explicitly prepares one test-only owned file with container root; Larainit
itself never requests root or fixes ownership. Never run failure/race tests against production state.
Python is developer-only PTY tooling, not a runtime dependency. CI defines stable/MSRV gates and
an operator-built real Docker job; local success is not a claim that hosted CI has run.

Rootless harness preparation is separate: `tests/podman.rs` requires an explicitly authorized
dedicated non-root account and `LARAINIT_TEST_PODMAN_IMAGE`, preserving that account's engine
HOME/config/runtime settings. Its installer/tool/host-edit/cache checks are opt-in:

```bash
cargo test --release --locked --test podman -- --ignored --nocapture --test-threads=1
```

This is **not currently a passing release gate**: no certified profile is enabled, so the harness fails
before workloads or direct engine snapshots. It also requires an explicitly prepared compatible local image. A missing profile
or image fails, rather than skipping. Do not run it against production state. PTY preparation accepts `--engine podman`
only for the independently certified fixture; mocks/ignored tests do not establish support.
The rootless harness also defines four concurrent locked Composer/npm installs against one
download cache, requires Composer cache-hit logs/offline npm and unchanged artifact bytes, and
checks the global installer. Its workflow requires `LARAINIT_TEST_PODMAN_SENTINEL_CID`, the full
ID of an operator-prepared unrelated running container; it verifies that container's image,
process and start time remain unchanged and never stops/removes it. Its separate group-only case requires
`LARAINIT_TEST_PODMAN_GROUP_FILE`: an absolute, operator-prepared regular file owned by another
UID, writable through a non-primary supplementary group and not writable by others. That opt-in
test appends one marker and leaves it in place; it never creates users or changes ownership/modes.
Another opt-in rootless case defines exact native-tail/special-workspace checks, separate streams,
stdin/EOF, statuses 42/126/127, both interruption signals, bounded wrapper reap and exact-CID
disappearance. These assertions are prepared tests, not recorded Podman runtime passes.

## How it works

Each invocation starts a **new, temporary container** from the `larainit:latest` image. The current host directory is bind-mounted to `/workspace`, and the container is removed after the command exits.

- **Persists:** files written inside `/workspace` (your project).
- **Also persists:** dedicated Composer/npm download caches.
- **Does not persist:** other container changes, globally installed packages added at runtime, and temporary HOME state.
- **Does not automatically provide:** database, Redis, web server, exposed ports, or a Docker daemon inside the container.

Use Laravel Sail or your own Docker Compose setup for the application's actual development services. Installing Sail's Composer package is possible with Larainit, but starting Compose services requires access to the Docker daemon from the host or appropriately configured tooling.

## Larainit vs. Sail

**Larainit** is a project-independent bootstrap and CLI toolchain. **Laravel Sail** is a Docker Compose-based development environment for a Laravel application. They can complement one another; neither requires the other.

## Troubleshooting

**`laravel: Permission denied`** — Check that the Laravel Installer was installed in a shared path and that its executable and parent directories are accessible by your container user. Rebuild after changing the Dockerfile.

**`I have no name!` in Bash** — Your host UID might not have an `/etc/passwd` entry inside the container. This is generally a cosmetic shell prompt issue.

**File permission issues** — Verify that the wrapper uses your host UID/GID and that the bind-mounted directory is writable.

**Docker absent/unavailable/permission denied** — Run `larainit doctor` and check your installed CLI,
effective context/endpoint and ordinary user access. Larainit does not use sudo, repair Docker,
start services or switch engines/contexts. Configure Docker independently.

**Missing image** — From a source checkout, explicitly run `docker build -t larainit:latest .`, or
prepare your compatible custom local image and use `larainit --image custom:tag php -v`.
Standalone executable directories are not build contexts. The inspected immutable ID and
`--pull=never` prevent retags/removal races from causing a pull/build or replacement-image selection.

**Config/cache errors** — Check the named path, absolute XDG overrides and schema. Fix permissions
deliberately yourself; Larainit never chowns/relabels existing paths or silently disables caching.

## Scope

Larainit is intentionally small: a repeatable Laravel bootstrap tool, not a replacement for project-specific container orchestration, CI/CD, or production deployments.

Implemented baseline: native CLI, Docker/local-image execution, actual installer, settings, doctor,
download caches and controlled best-effort interruption handling. Development additions: engine
preference setters, explicit Docker builds and independent candidate diagnostics (Feature 002 real
acceptance pending). Podman remains disabled/unverified, with candidate profile checks implemented. Arbitrary config editing, runtime version
selection, published images/binaries, installers/packages, signing/self-update and service
orchestration remain deferred; no release-completion claim is made here.

## License

Licensed under the [MIT License](LICENSE). You may use, modify, and distribute Larainit, including commercially, provided you retain the copyright and license notice.
