# Larainit

**A disposable, Docker-powered Laravel CLI toolchain. No local PHP, Composer, or Node.js required.**

Larainit runs the Laravel Installer and common development commands inside a temporary Docker container, while keeping your project files on your machine. Use it to bootstrap Laravel applications **before** installing Laravel Sail or setting up a project-specific Docker Compose stack.

**Build the toolchain once, then prefix your commands with `larainit`.** No local runtime setup, no toolchain files added to your application.

The v0.2.0 implementation is a **native Rust executable**, replacing the Bash wrapper.
All six story checkpoints have local execution evidence; final release acceptance and hosted CI
are tracked separately. No published binary, image, installer or self-update mechanism is provided.

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

**Observed:** Ubuntu 26.04.1 LTS x86_64, kernel 7.0.0-34-generic, Docker client/server 29.8.1,
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
larainit [--engine auto|docker] [--image IMAGE] [--] [COMMAND [ARG...]]
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
`engine`, `build`, and `config` are **reserved**, returning “not yet supported” without container
passthrough. Other first command tokens request the named container executable. For a reserved
name intentionally invoke it explicitly via a path, if supplied by your custom image.
Bare invocation refuses nonterminal stdin/stdout/stderr with an explicit-command remedy; explicit
`larainit bash` supports piped scripts and EOF. Container commands retain stdin and separate inherited
stdout/stderr; `-t` is requested only when all three are terminals, with no wrapper stdout banner.

## Settings and caches

Configuration is optional, read from `$XDG_CONFIG_HOME/larainit/config.toml`, defaulting to
`$HOME/.config/larainit/config.toml`. Larainit never creates this file. Its only accepted keys are
optional **strings**:

```toml
engine = "auto"       # case-sensitive: auto or docker
image = "larainit:latest"
```

Each value resolves independently: **CLI > `LARAINIT_ENGINE`/`LARAINIT_IMAGE` > file > default**.
Defaults are `auto` and `larainit:latest`. Empty explicit overrides are invalid, not absent.
Invalid effective values never fall back. Malformed, unreadable, wrong-type or unknown-key config
remains an error even with CLI overrides; invalid string values shadowed by a valid higher-priority
value do not become effective. No engine credentials or other arbitrary config keys are stored.

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

`larainit doctor` reports platform/version verification limits, Docker executable/server readiness,
effective image/setting sources, local immutable image identity, config/workspace/cache observations
and practical remedies. Missing optional config and absent/preparable caches are informational.
Critical failures return 1; independent observations continue, blocked ones say “not checked”.
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
On an isolated test engine with an explicitly prepared compatible local image:

```bash
export LARAINIT_TEST_IMAGE=larainit:latest
cargo test --release --locked -- --ignored --nocapture --test-threads=1
python3 tests/support/pty-check.py "$PWD/target/release/larainit" "$LARAINIT_TEST_IMAGE" --installer
```

Requested integrations fail if prerequisites are unavailable; there is no passing skip. Tests use
isolated paths, explicit package downloads and disposable local derivative images for races. The
supplementary-group test explicitly prepares one test-only owned file with container root; Larainit
itself never requests root or fixes ownership. Never run failure/race tests against production state.
Python is developer-only PTY tooling, not a runtime dependency. CI defines stable/MSRV gates and
an operator-built real Docker job; local success is not a claim that hosted CI has run.

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

Implemented: native CLI, Docker/local-image execution, actual installer, settings, doctor, persistent
download caches and controlled best-effort interruption handling. Deferred/planned: Podman, persistent
engine/config setters, `larainit build`, runtime version selection, published images/binaries,
installers/packages, signing/self-update, and service orchestration. None is usable by this release.

## License

Licensed under the [MIT License](LICENSE). You may use, modify, and distribute Larainit, including commercially, provided you retain the copyright and license notice.
