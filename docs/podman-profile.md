# Candidate prepared Podman profile

**Podman remains disabled.** These are candidate readiness checks, not a certified
profile or recorded real-workload/release acceptance. The production gate refuses
before launching fingerprints, metadata readers or Podman; no runtime bypass exists.

## Candidate requirements and observed packages

The candidate requires Ubuntu 26.04 amd64, native Linux, systemd/cgroup v2, local rootless Podman, native
overlay, SQLite schema 1, crun, seccomp support reported enabled with the pinned
`/usr/share/containers/seccomp.json` profile, SELinux disabled and AppArmor
reported unsupported by this rootless client. This is not a requirement to disable
the host's AppArmor policy. These requirements have not all been confirmed on the dedicated fixture.
Package/binary/configuration fingerprints are pinned in
`src/engine/podman.rs`; an update requires a new audit, not just a version edit.
XFS-backed overlay storage is refused because its constructor attempts quota
setup even without a configured quota. Other backing filesystems are not thereby
certified.

| Package | Observed version |
| --- | --- |
| podman | 5.7.0+ds2-3build1 |
| crun | 1.21-1ubuntu3 |
| conmon | 2.1.13+ds1-2 |
| catatonit | 0.2.1-2build1 |
| sqlite3 | 3.46.1-9ubuntu0.3 |
| bubblewrap | 0.11.1-1ubuntu0.3 |
| golang-github-containers-common | 0.66.0+ds2-3 |
| containers-storage | 1.61.0+ds1-6 |
| netavark | 1.16.1-3.1 |
| aardvark-dns | 1.16.0-3 |
| passt (pasta, including AVX2 companion) | 0.0~git20260120.386b5f5-1 |
| slirp4netns | 1.3.3-1 |
| dpkg | 1.23.7ubuntu1 |

The dedicated operator fixture has UID/GID 1001 and subordinate UID/GID ranges
165536:65536. Paths and ownership checks derive from the current local account;
neither this username nor UID is a bypass or a hardcoded runtime requirement.

## Candidate check order (behind the closed certification gate)

1. Require `/usr/bin/podman`, a non-root unchanged local account identity, its
   real HOME/default XDG locations, `/run/user/<uid>`, and no engine/storage/loader
   environment overrides (including present-empty values). PATH entries must be
   root-controlled; relative/user-writable entries are not audited.
2. Require the packaged system engine/storage configuration. Refuse system,
   rootless-global and user engine/storage overrides, drop-ins, custom mounts,
    hook directories, `/etc/containers/seccomp.json` and alternative local Podman
    helper directories. The packaged
   `/usr/libexec/podman` directory is allowed: refuse a higher-priority `conmon`
   there, and require its root-owned `catatonit -> ../../bin/catatonit` link to
   the independently fingerprinted `/usr/bin/catatonit`.
    A missing file is acceptable only through accessible, safe ancestors.
   Require the pinned dpkg configuration, no user `.dpkg.cfg` or nonempty dpkg
   configuration drop-ins, and no `DPKG_*` environment overrides. Refuse all other
   package-query tools and higher-priority network/helper/PATH replacements; see
   the helper-boundary section below.
3. Require existing user-owned default graph/run/static/tmp/volume directories,
   an existing SQLite database at `$HOME/.local/share/containers/storage/db.sql`,
   no BoltDB, an existing `false` mount-program marker, initialized runtime locks, a current boot
   marker, and accessible live pause-process user/mount namespaces. Each namespace
   mapping must map container root to the caller and have one 65,536-ID subordinate
   range above 65535. Require existing overlay feature-cache results and store
   locks/directories, including layer/mountpoint locks, OCI exits/persist directories,
   an exact `netavark` default-backend marker, the network lock and rootless-network
   directory. Require an existing events log when the default chooses file logging.
    Refuse nonempty overlay staging/deletion and layer-deletion temporary directories
    and incomplete/conflicting permanent or volatile storage records. These checks
    block the reviewed cached-feature and metadata/temporary-directory recovery
    paths, not every mount-related startup path. Missing required/stale/unknown state stops here,
    without executing Podman.
   Check the overlay home's filesystem through its validated open descriptor and
   refuse XFS before metadata or engine observation; recheck before any future info.
4. Verify the SHA-256 fingerprints of the root-controlled Podman, crun, conmon,
    catatonit, SQLite and Bubblewrap binaries, seven info helper binaries (including
    pasta's AVX2 companion), three container configuration files and dpkg's config.
   Use the existing root-controlled `/usr/bin/sha256sum` (including Ubuntu's exact
   package-managed Rust-coreutils link); no new dependency,
   installation, source download or repair occurs at runtime.
5. Query the existing SQLite `DBConfig` in Bubblewrap with a read-only root bind,
   isolated user/PID/network namespaces, no capabilities and no further user
   namespaces. SQLite has explicit `/dev/null` startup configuration, readonly
   mode and a cleared reader environment. Validate exactly one schema-1 row and
   every stored path. Missing WAL/SHM requiring recovery fails; no alternate
   unsandboxed reader or `immutable=1` shortcut is attempted.
6. Recheck the boot marker and startup-state guards, then perform bounded `podman info --format json` and
   validate the expected local/rootless distribution, cgroups, security, SQLite,
    crun/conmon paths, default storage and exact 5.7.0 version. Contradictory output
    or unexpected netavark/aardvark/pasta/slirp4netns paths
   authorizes no image/workload/build operation.

Once certified, the fingerprint command, metadata reader and info command would share the existing
five-second/64-KiB observation budget and signal supervision. Metadata-process
errors are redacted. No remote/rootful/other-engine fallback is added.

Candidate checks describe existing state at observation time. Concurrent configuration
changes, storage resets or namespace teardown are unsupported; this is not a
transactional lock over all of Podman's state. Normal engine bookkeeping during
the authorized info command may occur; **only the metadata reader** has a
filesystem-enforced no-write boundary. This distinction is not a promise of an
unchanged Podman store after every engine operation.

## Source review and execution evidence

Reviewed upstream v5.7.0 sources:

- [`libpod/sqlite_state.go`](https://github.com/containers/podman/blob/v5.7.0/libpod/sqlite_state.go):
  default database location, SQLite connection and stored configuration validation.
- [`libpod/sqlite_state_internal.go`](https://github.com/containers/podman/blob/v5.7.0/libpod/sqlite_state_internal.go):
  schema 1 skips schema creation/migration when the existing schema matches.
- [`libpod/runtime.go`](https://github.com/containers/podman/blob/v5.7.0/libpod/runtime.go)
  and [`runtime_linux.go`](https://github.com/containers/podman/blob/v5.7.0/libpod/runtime_linux.go):
  runtime paths/locks, state refresh and boot-marker handling.
- [`pkg/rootless/rootless_linux.c`](https://github.com/containers/podman/blob/v5.7.0/pkg/rootless/rootless_linux.c):
  early joins of the existing pause-process user/mount namespaces.
- Vendored `go.podman.io/common/pkg/config/new.go` and
  `go.podman.io/storage/types/options.go`: configuration precedence, rootless-global
  drop-ins and environment overrides.
- Vendored `go.podman.io/storage/store.go` and `drivers/overlay/overlay.go`:
  store locks/directories, the `false` native mount-program marker, cached overlay,
  native-diff/metacopy/volatile/idmapped checks, and staging cleanup on shutdown.

On 2026-10-12, operator output established rootless/local mode, these installed
packages, selected crun, matching schema/default stored paths and matching boot
marker. The system engine/storage files were inspected; no system overrides were
present. The existing real SQLite sandbox regression passed with
`LARAINIT_TEST_SQLITE=/usr/bin/sqlite3`, preserving live DB/WAL/SHM bytes and mtimes
and refusing missing-SHM/cold-state cases. These are readiness/reader observations,
not PHP/installer/cache/group/terminal/signal acceptance evidence.

The operator's additional 2026-10-12 filesystem output confirmed:

- Native-overlay marker bytes are exactly `false`; the five feature-cache files
  match the candidate's values and are empty, caller-owned regular files.
- Graph/run overlay directories and all listed store/runtime locks exist with
  caller ownership and no group/world write permissions. The shared lock is 82,488 bytes.
- `overlay/staging` is absent, an accepted state (no staging directory to recover).
- Pause PID 109516 has UID and GID maps `0 1001 1` and `1 165536 65536`;
  user/mount namespace links are visible and caller-owned. Listing a namespace link
  alone does not prove an actual namespace open or join. The operator metadata-only
  runs below subsequently established that the opens succeed, not that a join does.

### Ubuntu source/build provenance

Ubuntu [amd64 build 32308514](https://launchpad.net/ubuntu/+source/podman/5.7.0+ds2-3build1/+build/32308514)
records common-dev `0.66.0+ds2-3`, storage-dev `1.61.0+ds1-6`, and
go-sqlite3-dev `1.14.32~ds1-1`; it builds with Go 1.25.0 and the
`apparmor seccomp selinux systemd libsubid` tags. These are build dependencies,
not versions inferred from today's installed runtime configuration packages.

The downloaded amd64 `.deb` matched its buildinfo SHA-256
`bc8fbe3c31f0904550a9e6cb32363643fd34ad9b78553fda233b50caef2e11bd`.
Its `usr/bin/podman` matched the installed binary and the pinned fingerprint
`6627da0abfd3b0189498089319f5759c7b132bb6a51130bf8ff43d7f102d9673`.
Packages were read, not installed or executed.

Original source and Debian patch archives were fetched from the official Ubuntu
HTTPS archive and checked against their `.dsc` SHA-256 fields. PGP signatures
were not independently verified. Source manifests:

| Source | Original archive SHA-256 | Patch archive SHA-256 |
| --- | --- | --- |
| [podman 5.7.0+ds2-3build1](https://archive.ubuntu.com/ubuntu/pool/universe/p/podman/podman_5.7.0+ds2-3build1.dsc) | `d5f4e3d2798d8dfa5caa24e3db878b3a0ee3d8748caf0b3c0fae2e7cda59bd79` | `f325d6a23220aef8bec93eb023da4b166ce426feda68aa98a50964479358e6a4` |
| [storage 1.61.0+ds1-6](https://archive.ubuntu.com/ubuntu/pool/universe/g/golang-github-containers-storage/golang-github-containers-storage_1.61.0+ds1-6.dsc) | `de29d94a719a077e9937e423a01f1106b0195970f640ada8da94d58597059762` | `eb8d8bcc7b5674e4c6d31c5e1796b0c5e6bbe1c5e5d808167e52eb2dd4e9e3ce` |
| [common 0.66.0+ds2-3](https://archive.ubuntu.com/ubuntu/pool/universe/g/golang-github-containers-common/golang-github-containers-common_0.66.0+ds2-3.dsc) | `a1d94f2577c649f6adfb4f36aa626864b8141490d1edfe171a6a4a101a04c19a` | `0e966aaef21ce2df6a25059945ba2d8fd339e4d79b3b587c30bf40a33d287085` |

The original packaged sources are byte-identical to the reviewed v5.7.0 files:
Podman's SQLite state/schema, runtime/Linux runtime, info, CLI root and rootless
Go/C files; storage's store, overlay driver, options and rootless unshare files;
and common's config/new/config_linux/default files. None of the applied patch
series changes these files. Podman's patches affect Docker-API compatibility,
machine/tests and the conmon attach buffer; storage patches affect tests/docs;
common patches change systemd import paths and docs. Ubuntu's `build1` changelog
entry is a no-change gpgme rebuild. This closes the patch-equivalence question for
these reviewed paths, not all of Podman's transitive startup behavior.

The extended startup/security review compared 21 original source files against
v5.7.0 using the same verified archives: Podman's OCI constructor, event backends,
container/state synchronization and seccomp-path selection; storage's layer/image/
container loaders and temporary-directory recovery; common's network constructors,
AppArmor verifier, journal selection, helper lookup/version queries and seccomp
support check. All originals match upstream. No applied storage/common patch
touches these files. Podman's `0006-Avoid-dependency-on-conmon-runner-config.patch`
does touch `oci_conmon_common.go`, but only removes the conmon-config import and
substitutes the attach-buffer constant with `8192 + 1`; it does not change the
constructor or runtime-info/version paths reviewed here. This extends source
provenance evidence, not live security/helper certification.

The continued mount audit checked 11 source files directly in the same Ubuntu
original/patch archives, revalidating both archive checksums. No applied patch
touches these files: storage's overlay/quota/mount/store implementations; Podman's
CLI shutdown, runtime shutdown, ABI container shutdown and rootless Go/C join
paths; common's image shutdown and package-version queries. This was a packaged-
source review, not a fresh upstream comparison or live mount/helper experiment.

## Explicit metadata-only fixture check

The ignored unit test `engine::podman::tests::real_ubuntu_candidate_metadata_only`
requires `LARAINIT_TEST_PODMAN_PROFILE=1` and the dedicated non-root account.
It exercises candidate filesystem/namespace/fingerprint checks, then the real
sandboxed metadata reader, comparing DB/WAL/SHM/journal bytes, identity, mtime,
ctime, the five storage-JSON snapshots and graph-root entries. Its final startup
recheck is included in the snapshot comparison. It **never executes Podman**, creates fixture paths,
refreshes storage or opens the production certification gate.

With an accessible checkout and Rust toolchain, run:

```sh
LARAINIT_TEST_PODMAN_PROFILE=1 cargo test --locked --bin larainit \
  engine::podman::tests::real_ubuntu_candidate_metadata_only \
  -- --exact --ignored --nocapture --test-threads=1
```

Alternatively, build the unit-test executable in the development account with
`cargo test --locked --bin larainit --no-run`, copy only that executable to an
accessible location, and run it as the dedicated account with the same exact test
filter, environment authorization and test-harness arguments. Do not run all
ignored tests or make the source checkout world-writable.

The first operator run stopped before fingerprints/metadata at an incorrect
candidate assumption that `/usr/libexec/podman` must be absent. Ubuntu normally
ships this root-controlled directory with rootlessport, quadlet, podman-testing,
and the catatonit package link. The check now rejects the actual higher-priority
conmon executable rather than the entire packaged directory, and validates the
exact catatonit link. No fixture permissions or package files were changed.

On 2026-10-12, the operator reran the rebuilt test as `larainit-podman-test`
with `LARAINIT_TEST_PODMAN_PROFILE=1` and `PATH=/usr/bin:/bin`. The exact
metadata-only test passed in 0.11 seconds (one passed, zero failed). Actual
descriptor-based path checks, namespace opens, binary/configuration fingerprints
and stored DBConfig validation succeeded. DB/WAL/SHM/journal snapshots and
graph-root entries compared unchanged, including database bytes, identity, mtime
and ctime. No Podman process was executed. This closes the live candidate-reader
check, not production startup/security or workload certification. That run predates
the extended startup-state guards described below; the following rerun supplies
their dedicated-account reader/preflight evidence.

The operator then ran the freshly staged helper-audit executable as
`larainit-podman-test` with `LARAINIT_TEST_PODMAN_PROFILE=1`, `PATH=/usr/bin:/bin`
and `XDG_RUNTIME_DIR=/run/user/1001`:

```sh
/tmp/opencode/larainit-helper-audit-oi0avd28/larainit-profile-tests \
  engine::podman::tests::real_ubuntu_candidate_metadata_only \
  --exact --ignored --nocapture --test-threads=1
```

Supplied operator output reports **one passed, zero failed, zero ignored, 53
filtered out in 0.13 seconds**. The executable's staged SHA-256 was
`1baef014fdea6dca4458aa4cbe36eb6b41b7b9196003f0d6230c59495ddd1db9`.
The current startup/storage/helper guards, namespace descriptor opens, all 17
binary/configuration fingerprints and DBConfig validation passed. Database/storage
metadata bytes, identity, mtime and ctime and graph-root entries compared unchanged,
including the final startup recheck. No Podman process was executed. This is
operator-provided execution evidence, not an independently repeated run by the
agent; it does not establish namespace-join/mount-view safety, helper execution,
info security fields or workload acceptance. The production gate remains false.

## Startup/security audit findings

The v5.7.0 startup path is not a filesystem-read-only operation just because it is
called by `info`. The audit found additional prerequisites, now checked in the
shared startup guard both before candidate metadata observation and before any
future production `info` observation:

| Reviewed path | Side effect if not prepared | Candidate check |
| --- | --- | --- |
| `libpod/oci_conmon_common.go:newConmonOCIRuntime` | Creates `tmp/exits` and `tmp/persist` | Both must already be owned, safe directories |
| `common/libnetwork/network/interface.go:defaultNetworkBackend` | Creates a backend marker, or migrates a stored CNI marker to netavark | Existing bytes must be exactly `netavark` |
| `common/libnetwork/netavark/network.go:NewNetworkInterface` | Opens/creates `networks/netavark.lock` and network runtime directories | Existing graph network lock and run-root `networks/rootless-netns` directory required |
| `storage/layers.go:newLayerStore` | Creates layer directories and layer/mountpoint locks | Existing graph `overlay-layers/layers.lock` and run-root `overlay-layers/mountpoints.lock` required |
| `storage/layers.go:load`, `storage/internal/tempdir/tempdir.go:RecoverStaleDirs`, `storage/drivers/overlay/overlay.go:GetTempDirRootDirs` | Acquires/creates temporary locks and deletes stale layer-deletion directories | `overlay-layers/tmp` and `overlay/tempdirs`, like `overlay/staging`, must be absent or empty; no locks are acquired to decide whether leftovers are stale |
| `storage/{layers,images,containers}.go:load` | Deletes incomplete layers or rewrites conflicting record names | Read permanent and volatile JSON together within each store; refuse incomplete/unknown flags, conflicting IDs/names and ambiguous recovery fields without saving anything |
| `storage/drivers/overlay/overlay.go:Init` → `quota.NewControl` | XFS quota initialization can create/replace a device node and set quota/attribute state | Read-only descriptor-based `fstatfs` refuses XFS; no quota operation or test mount is attempted |
| `common/pkg/config/systemd.go` and `libpod/events/logfile.go:newLogFileEventer` | If no readable journal directory is found, file logging creates `tmp/events/events.log` | Mirror the readable-journal selection and require an existing event log for the fallback; require an existing systemd boot directory |
| `libpod/util.go:DefaultSeccompPath` | An existing `/etc/containers/seccomp.json` supersedes the pinned packaged profile | Refuse that override before observation and require the packaged profile path in info JSON |

Tests remove each test-owned prerequisite and assert the guard refuses without
recreating it; legacy/empty/unknown backend markers are preserved rather than
migrated. Temporary-directory tests accept absent/empty directories, reject links
and every nonempty case, and preserve sentinel bytes. Mock observations prove a
missing startup marker or pending layer/overlay deletion never launches the info
command. Info validation refuses missing, malformed, empty, overridden or
unconfined seccomp-profile paths. No real Podman command was executed during this audit.

After these changes, `cargo test --locked` passed the complete normal regression
suite (129 passed, zero failed, 18 ignored); all ignored tests remained ignored.
`cargo clippy --locked --all-targets --
-D warnings`, `cargo fmt --check` and `git diff --check` also passed. This is mock/
unit-regression evidence; the distinct expanded live reader/preflight result is
recorded above, not inferred from these normal tests.

### Persisted storage recovery guard

The shared startup check now reads these five files with the existing descriptor-
based no-follow/ownership checks and a 1-MiB per-file ceiling:

- Graph-root `overlay-layers/{layers,volatile-layers}.json`.
- Graph-root `overlay-images/images.json`.
- Graph-root `overlay-containers/{containers,volatile-containers}.json`.

In this nontransient, single-image-store profile, volatile records still reside in
the graph root. Missing, zero-byte, `null` and empty-array files are harmless empty
inputs to the reviewed loaders and are accepted without creation. Each nonempty
record requires a nonempty canonical `id`; optional `names` and `flags` must have
their expected types. IDs and names must be unique within each store, across both
permanent and volatile files where applicable. An `incomplete` flag must be absent
or explicitly false. Null records, duplicate recovery fields and case-folded
aliases are refused. Non-ASCII top-level keys are also refused rather than
attempting to reproduce Go's Unicode field-name folding; Unicode name values are
not forbidden.

This is deliberately stricter than Go's permissive handling of some malformed or
noncanonical state. In particular, duplicate IDs are refused rather than skipping
a record that could hide a pending deletion. Unrelated canonical ASCII fields are
ignored; the guard covers these loaders' recovery decisions, not the entire
storage schema, layer contents, mount-state validity or all transitive startup.
It does not acquire storage write locks, repair metadata or execute Podman.
Concurrent storage mutation remains unsupported.

One regression checks every file's accepted/refused inputs and preserves its
bytes, inode, mtime and ctime, including oversized-file and symlink refusals.
Cross-file tests cover permanent/volatile ID and name conflicts; separate store
namespaces may legitimately share names. Mock observation cases prove incomplete
volatile layers, cross-file conflicts and duplicate flags prevent the info command
from launching. The operator's 0.13-second metadata-only rerun also passed these
checks against the dedicated fixture, with unchanged database/storage snapshots.

### Security reporting

The host kernel reports AppArmor enabled (`/sys/module/apparmor/parameters/enabled`
contains `Y`). This does not contradict the candidate's expected
`host.security.apparmorEnabled=false`: common's
`pkg/apparmor/internal/supported/supported.go:IsSupported` returns an error for
rootless mode before testing the host kernel or finding `apparmor_parser`;
`apparmor.IsEnabled` reports that error as false. Host confinement must not be
disabled to satisfy the candidate check.

`seccompEnabled=true` is a support report, not proof of an installed workload
filter. In the seccomp-tagged build, `seccomp.IsEnabled` calls `IsSupported`, which
tests `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, NULL)` and treats any error other
than `EINVAL` as support. That invalid-pointer probe does not install a filter.
The candidate now also checks the selected profile path; workload enforcement
still needs separately authorized acceptance evidence.

### Info subprocesses and container bookkeeping

The reviewed info path executes more than the pinned OCI runtime/conmon:

- `common/libnetwork/netavark/network.go:NetworkInfo` executes the selected netavark
  and aardvark-dns binaries with `--version`, plus package-version queries.
- `libpod/info_linux.go:setPlatformHostInfo` discovers and executes both pasta and
  slirp4netns when present, even if one is not the configured rootless network
  command. Helper lookup tries the configured/default helper directories, then
  PATH when requested. The candidate rejects environment/configuration overrides
   and requires root-controlled PATH directories. Its helper lookup/fingerprint
   checks are described below; actual helper execution is not yet certified.
- `common/pkg/version/version.go:Package` can execute absolute-path package
  tools, trying rpm, dlocate, dpkg/dpkg-query, pacman, qfile, equery, apk and pkg
  subject to existence and prior query results. On Ubuntu, narrowing this to the
   actual installed tools is now enforced as described below. Actual execution
   evidence remains necessary; checking PATH alone does not cover these absolute paths.
- `libpod/info.go:getContainerStoreInfo` calls `Container.State`, then
  `syncContainer`/`checkExitFile`. A running/paused/stopping container with an
  existing exit file transitions to stopped; `handleExitFile` records its exit
  code, emits an exited event and `syncContainer` saves the changed state. This is
  ordinary exit-state bookkeeping, not boot refresh or storage recovery, and is
  within the documented non-read-only engine boundary. No restart or cleanup is
  launched by these reviewed functions. This does not establish that all other
  startup/state paths are safe.

### Candidate helper boundary

Read-only inventory found root-owned, regular executable netavark/aardvark files
in `/usr/lib/podman` and pasta, pasta.avx2, slirp4netns, dpkg and dpkg-query in
`/usr/bin`. Their installed SHA-256 values are candidate-pinned alongside the
existing binaries/configuration, with no setuid/setgid bits allowed. These hashes
identify the observed files; they are not an independent source-to-binary build
attestation or live helper certification.

The candidate now refuses:

- Netavark/aardvark/pasta/slirp4netns replacements in `/usr/libexec/podman`, and
  pasta/slirp4netns in `/usr/lib/podman`. The two `/usr/local/.../podman` directories
  remain unsupported. No helper search override or container-config change is made.
- Pasta, slirp4netns or dpkg-query in any canonical PATH directory other than
  `/usr/bin`; `/bin` may canonicalize to that same directory. Unrelated tools in
  safe PATH directories are not forbidden. Even nonexecutable replacement files
  are refused instead of reproducing every fallback case.
- Every alternate absolute packager path in `version.Package`: rpm, dlocate,
  pacman, qfile, equery, apk and pkg. Ubuntu's root-owned `/sbin -> usr/sbin` link
  is checked explicitly before refusing `/usr/sbin/apk`.
- Any `DPKG_*` environment key, including present-empty values, `$HOME/.dpkg.cfg`,
  nonempty `/etc/dpkg/dpkg.cfg.d`, or bytes differing from the pinned
  `/etc/dpkg/dpkg.cfg`. Nothing is cleared, rewritten or executed to test a config.

The future info validator also requires `networkBackend` and
`networkBackendInfo.backend` to report netavark, with exact netavark/aardvark paths,
plus the expected pasta/slirp4netns executable paths. Missing/null/malformed or
conflicting path evidence is refused. This post-observation consistency check is
not a substitute for the pre-execution lookup guards.

Five exact installed-version source descriptors/archives were fetched from official
Ubuntu HTTPS and checked against their `.dsc` SHA-256 entries (PGP signatures not
independently verified). Dpkg is a native source archive; the other four include
separate original and Debian patch archives. Reviewed behavior:

| Helper path | Source finding before/at `--version` or package query |
| --- | --- |
| netavark 1.16.1 | Initializes env_logger, then Clap exits for `--version` before network command dispatch. The applied Debian default-aardvark-path patch is after parsing and does not change this branch |
| aardvark-dns 1.16.0 | Initializes logging and attempts a Unix syslog connection before Clap version parsing; does not enter DNS-server dispatch |
| pasta from passt 20260120 | May exec its adjacent `.avx2` companion, adjusts its own capabilities/fds/resource limits, creates an epoll fd and opens a temporary socket for buffer/device-option probing before version parsing. The version branch exits before namespace/network configuration, log/pcap file creation, tap setup and daemon dispatch |
| slirp4netns 1.3.3 | Argument parsing prints version and exits before socketpair/fork/tap/network setup |
| dpkg 1.23.7ubuntu1 | Loads dpkg config before its search backend. `-S` selects an unset hook-action code (not an install action), then execs `dpkg-query --search -- ...` through PATH. Dpkg-query does not load dpkg config; search/show use readonly database mode. Update/trigger rewrite and locking branches require write mode, and the captured queries do not start a pager |

These are source-level findings, not a claim that all version commands are
syscall-free. Source manifests and selected-file/patch review are retained in
`/tmp/opencode/larainit-helper-audit-oi0avd28/{sources,review,inventory}.json`.
Regression tests first exposed accepted `DPKG_ROOT` and contradictory network
backend evidence, then passed with the guards. Helper-refusal tests preserve
nonexecutable files, links and unrelated helpers without running or removing them.
No info/helper/package-query process was executed during this continuation.

### Passive namespace evidence and candidate guards

The operator ran `namespace-readiness.py` as the dedicated account. Supplied output
shows pause PID **109516**, distinct user/mount namespaces, mappings `0 1001 1`
and `1 165536 65536`, ext4 covering graph storage, and slave (`master:`), not shared,
pause mounts covering `/`, `/run` and `/run/user/1001`. All **20** inspected paths
(three storage directories and 17 pinned binaries/configuration files) report
matching caller/pause-root device/inode identities. No namespace was joined or
Podman process executed. These are operator-provided point-in-time observations,
not an independently repeated run or proof of actual join/cleanup behavior.

Candidate preflight now enforces a stronger boundary:

- Require canonical pause-PID bytes, accessible distinct namespaces with the audited
  mappings, and an opened pause-root descriptor matching the caller root identity.
- Compare bounded (1 MiB each) caller/pause mountinfo views for HOME/storage, PATH
  directories and `/usr`, `/etc`, `/var`, `/sys`, `/dev`, `/run` and `/proc`. Compare
  device, mount root/point, flags, filesystem, source and superblock options while
  allowing namespace-local mount IDs and expected propagation differences. Refuse
  relevant shared/unbindable mounts, unsupported stacked/duplicate relevant points, unknown
  propagation fields, malformed/unsupported paths and differing relevant views.
- Reuse the no-symlink descriptor walk from the opened pause root to compare all
  pinned files, the database and prepared storage/runtime directories. Check the
  pause-view overlay descriptor with `fstatfs`, including XFS refusal.
- Require `/etc/containers/podman_preexec_hooks.txt` absent in both views. The C
  constructor checks this marker before joining namespaces; ordinary OCI hook
  directory guards alone do not cover pre-exec hooks.
- Recheck PID bytes, process/namespace/root identities and both mountinfo snapshots
  before completing inspection. Repeat namespace-view and startup checks after
  metadata reading and before future production info; the production gate is
  still closed. The private observation test seam supplies checks for mocked
  ordering only; no production flag or gate bypass was added.

Three new regressions cover mount remaps/propagation/malformed input and escaped
paths, changed filesystem roots and descriptor-walk path identities, and canonical
PID parsing. The existing mocked flow also refuses a missing pause PID before
info. Normal tests pass **132/0/18**, with formatting and denied-warning Clippy clean;
ignored real tests stayed ignored. The previous 0.13-second metadata pass predates
these namespace guards. Their later authorized operator rerun failed as described below.

The freshly tested executable is staged at
`/tmp/opencode/larainit-namespace-audit-fb1ax6kp/larainit-profile-tests`, SHA-256
`c5a19d32502dc2623e175e81f7ea072d7cff6d33bed433d1edf832ae8867ad57`.
It has not been run as the dedicated account. Do not substitute an earlier binary.
Checks remain point-in-time: holding descriptors while reading does not isolate
the future engine from concurrent mount/process changes. No join or mount syscall
is used by these guards, and no certification is inferred from their unit tests.

The operator subsequently authorized the metadata rerun plus a disposable-child
namespace join, but the agent's noninteractive sudo attempt stopped at account
authentication before executing the test. A separate helper is staged for the
operator's existing dedicated-account session:

```sh
LARAINIT_TEST_PODMAN_PROFILE=1 LARAINIT_TEST_NAMESPACE_JOIN=1 \
  PATH=/usr/bin:/bin XDG_RUNTIME_DIR=/run/user/1001 \
  /usr/bin/python3 -I -S -B \
  /tmp/opencode/larainit-nsfs-audit-6vq1w_1s/certification-check.py
```

It verifies the executable hash, runs the metadata-only test, joins user/mount
namespaces only in its disposable child, checks mapped identity and 21 root/path
identities, compares database/storage/PID/boot/mountinfo snapshots and reruns the
metadata test. It does not execute Podman or mount anything. Its engine-free
`--self-check` passed; the operator's successful dedicated-account run is recorded
below. A Python-child join does not attest Podman's executable-specific AppArmor policy
or certify startup/cleanup/helper effects. That real execution remains separately
unauthorized, and the production gate stays false.

#### Prepared binfmt automount correction

The first authorized operator run used the earlier `namespace-audit` artifact and
reported **0 passed, 1 failed, 0 ignored, 56 filtered in 0.00 seconds**, followed by
wrapper status **101**. Preflight rejected a repeated mountpoint before launching
the SQLite metadata reader or namespace child. The snapshot assertions were not
reached; do not infer unchanged-storage or successful-join evidence from this refusal.

A passive read of the agent's caller mountinfo (UID 1000, same reported host mount
namespace `mnt:[4026531832]`) reproduced the duplicate at
`/proc/sys/fs/binfmt_misc`: autofs mount ID 40 with binfmt_misc mount ID 172 whose
parent is 40. This is the documented Linux representation of an already-triggered
systemd automount, not an ambiguous duplicate row. No directory under that
automount was opened, and no mount was triggered or removed to investigate it.

The parser now handles only this narrow prepared shape at that exact location:
one mounted binfmt_misc filesystem, or an autofs parent plus mounted binfmt_misc
child. Both filesystem roots must be `/`, IDs must be unique, and the parent chain
must be unambiguous. Both signatures are retained for caller/pause comparison;
enumeration order and numeric mount-ID ordering are not used to choose a visible
layer. Autofs-only, extra/cyclic/unrelated layers, submounts below the interface,
shared pause layers, changed covered/top signatures and all other relevant stacks
(including overlay home and `/`) remain refused. Errors now identify caller versus
pause view and the unsupported mountpoint.

This location must remain checked: verified Ubuntu ABI info calls
`emulation.Registered()`, whose binfmt probe walks the directory and opens status/
handler entries read-only while skipping `register`. An autofs-only view could
trigger a first mount during that walk and is not eligible. Original source hashes
and applied patch review for ABI info and the binfmt probe are retained with the
new test log; no relevant applied patch touches them. Linux kernel documentation
and `proc_pid_mountinfo(5)` confirm the stack-parent semantics; this does not attest
live Podman execution or imply every kernel mount configuration is supported.

A regression first reproduced the valid-stack rejection, then passed valid,
reordered and renumbered chains, with refusals for ambiguous/untriggered/changed
stacks and storage/root stacks. Current normal suite: **133/0/18**; formatting,
standalone mock formatting and denied-warning Clippy passed. No ignored workload
test or namespace child ran during the correction.

The binfmt correction staged the executable at
`/tmp/opencode/larainit-binfmt-audit-vhzv9_q4/larainit-profile-tests`, SHA-256
`644a03061fc016dbb3003aa8b2264fa7cf6ede5b074b36f03195740d965792aa`.
The copied harness checks that new hash; older artifacts are preserved as historical
evidence. Its self-check passed without fixture access. That artifact's rerun failed
as recorded below; the subsequent nsfs-corrected artifact passed the authorized checks.
No real Podman execution, filesystem repair or production-gate change is authorized.

#### Namespace filesystem root correction

The authorized binfmt-artifact rerun failed **0/1/0**, 57 filtered, in 0.00 seconds,
wrapper status **101**, at `caller mount view: invalid or noncanonical mountinfo
path`. Again, the SQL reader, namespace child and snapshot assertions were not
reached. A passive caller mountinfo inventory found `nsfs` rows under
`/run/docker/netns` whose root fields are identifiers such as
`net:[4026533483]`, not absolute pathnames. No Docker command, namespace handle,
mountpoint directory or unrelated container was opened or changed to investigate.

`check_mount_root` now distinguishes these kernel object names from paths. Only
`nsfs` may use `<known namespace kind>:[<canonical positive u64 inode>]`; ordinary
filesystem roots and all mountpoints still require canonical absolute paths.
The complete namespace kind/inode string remains in the compared signature, so
different namespace objects still refuse readiness. Unknown kinds, malformed/
overflowing/zero/signed/leading-zero IDs, pseudo-roots on other filesystems and
relative/traversing mountpoints remain rejected. Errors identify the mount ID,
root or mountpoint field, and filesystem; view mismatches report their location.

The regression reproduced the old rejection, then passed valid names for all eight
supported namespace kinds while retaining the negative cases and identity mismatch
checks. Normal suite **134/0/18**, formatting, standalone mock formatting and
denied-warning Clippy passed. Linux v7.0 `fs/nsfs.c:nsfs_show_path` was reviewed from
the official upstream repository and matches this root format. That is source/API
evidence, not attestation of Ubuntu's compiled `7.0.0-38-generic` kernel. The Context7
lookup did not cover this detail; upstream source and `namespaces(7)` supplied it.

The command above now targets
`/tmp/opencode/larainit-nsfs-audit-6vq1w_1s/certification-check.py`, with fresh test
executable SHA-256
`6c6a2bf31bc5bd8470afc31695f872d393337a730b2dcbe3a9cca2494d169d1c`.
The harness changed only its expected executable hash and passed its engine-free
self-check. The operator subsequently passed the checks as recorded below;
earlier artifacts and failures are preserved. Gate false, no real Podman execution
or fixture repair, and no successful snapshot/join evidence inferred from failures.

#### Authorized metadata and child-join result

Operator-provided output for the hash-checked nsfs artifact reports:

- Exact metadata test **1/0/0**, 58 filtered, in **0.17 seconds** before the child.
- Child successfully joined the expected user/mount namespaces with mapped
  **UID/GID 0**; all **21** root/storage/binary/configuration identities matched.
  Its reported security label was **`unconfined`**.
- Database/storage/PID/boot snapshots and both caller/pause mountinfo tables
  compared unchanged around the child. No mount operation or Podman execution
  exists in this helper's child path.
- Exact metadata test **1/0/0**, 58 filtered, in **0.13 seconds** afterward,
  including the strengthened namespace/binfmt/nsfs guards and snapshot assertions.

The final supplied summary line is truncated; no wrapper exit status was supplied.
The explicit stage results above are recorded as operator-provided evidence, not
an independently repeated agent run. They establish the current reader/preflight
and the isolated Python-child join, not Podman's executable-specific AppArmor
attachment or actual constructor/runtime/shutdown behavior. The user subsequently
authorized the guarded help/conditional-info audit described below; it has not run.
No workload/build/preparation is authorized and production certification stays false.

#### Authorized real-executable audit: help stage first

The user selected **Authorize guarded info audit**: real `/usr/bin/podman info
--help`, followed by at most one `info --format json` only after the help/security
and current preflight prerequisites pass. Authorization covers expected ordinary
lock/event bookkeeping and temporary overlay bind/private/unmount effects during
that eventual info call, not initialization/refresh/recovery, fixture preparation,
workloads, repair, package/config changes, publishing or production-gate bypass.
Unexpected lasting changes stop the audit without repair.

The first stage is staged at
`/tmp/opencode/larainit-info-audit-kpoi8k58/help-audit.py`. It executes **help only**;
there is no info invocation or mount/join operation in this harness. The reviewed,
checksum-verified Ubuntu original archive's `cmd/podman/main.go:addCommand` notes
that persistent pre-run is not called for `--help`. The C constructor nevertheless
permits the `info` namespace shortcut before Go dispatch. Successful help text alone
does not prove it took that shortcut: the harness must also sample the actual
`/usr/bin/podman` executable in the expected pause user/mount namespaces and retain
its host-view credentials and actual security label. If this fast process cannot
be sampled, the result is incomplete, not an inferred pass.

Before execution, the unchanged hash-checked metadata preflight runs, and both
mount tables, the pause/process identity set, and bounded filesystem inventories
are saved. The inventories cover the entire prepared graph and dedicated runtime,
`$HOME/.config/containers`, `$HOME/.dpkg.cfg`, and the rootless shared lock file.
They exclude atime, hash regular files, retain symlink/special-node metadata without
following/opening those objects, use no-follow descriptor walks, and refuse nested
mounts or budget excess rather than trying to unmount/repair them. The limits are
10,000 entries, 128 MiB of regular-file data and a checked 20-second inventory
budget. The process gets 10 seconds and 1 MiB per output stream, checked while
sampled. If necessary only the still-unreaped direct child is killed; no pause or
unrelated process is signaled and no descendant cleanup is attempted.

Evidence writes are isolated to a private dedicated-account directory under the
new artifact's sticky `results/` directory. An exclusive persistent invocation
claim prevents accidental help reruns using this artifact. Before/after comparisons
must report no help-stage filesystem/mount changes or dedicated-account process
identity-set changes. The final metadata preflight must pass again. This is bounded
point-in-time and sampled process evidence, not a syscall trace, complete dependency
attestation or proof of all transient side effects/concurrent-mutation safety.
The harness's engine-free snapshot/change/no-follow/budget/mount/sample self-check
passed; no fixture or actual Podman execution occurred while staging it.

The original help-stage command follows for historical reference. Its inventory
refusal is recorded below; use the newer **inventory-only diagnostic** next, not
this superseded command. Help evidence still needs review before the authorized
conditional info invocation; missing/failed prerequisites stop that invocation.

```sh
LARAINIT_TEST_PODMAN_PROFILE=1 LARAINIT_TEST_PODMAN_HELP_AUDIT=1 \
  PATH=/usr/bin:/bin XDG_RUNTIME_DIR=/run/user/1001 \
  /usr/bin/python3 -I -S -B \
  /tmp/opencode/larainit-info-audit-kpoi8k58/help-audit.py
```

#### Help-stage inventory refusal and passive diagnosis

The operator ran that command. The first exact metadata test passed **1/0/0**,
58 filtered, in **0.13 seconds**. The subsequent before-inventory failed with
`PermissionError: [Errno 13] Permission denied: 'dir'`. Its private evidence
directory is `results/help-fbvxg7u2` under the original audit directory. This
failure occurred before saving the complete before snapshot, creating the help
invocation claim or launching Podman. No help/security sampling, after comparison
or info observation was reached; the failed inventory is not preservation evidence.

The descriptor-relative error omitted the complete path. The actual blocked entry,
its owner/mode and the reason for denial remain unknown; a subordinate-owned image
entry is only a possibility, not an observed explanation. Do not change permissions,
skip that subtree, or join a namespace as an automatic permission workaround.

A fresh copy at `/tmp/opencode/larainit-inventory-audit-itc6bby1/help-audit.py`
retains the original artifact and adds contextual inventory errors, saved failure
details and an explicit `--inventory-only` mode. That mode returns before the help
claim or subprocess launch even when inspection passes. It retains all preflight,
no-follow, mount and budget checks. Nested errors preserve their exact descendant
path; available stat evidence reports UID/GID, mode, device and inode. The existing
help invocation claim is shared with the original artifact, not reset or bypassed.

An engine-free unreadable-directory regression reproduced the basename-only error
before correction and now passes, including exact descendant path/mode diagnostics.
Permissions were changed only on a newly created self-check directory, never on the
dedicated fixture. Rust and checklists are unchanged; certification stays false.
The passive diagnostic command was:

```sh
LARAINIT_TEST_PODMAN_PROFILE=1 LARAINIT_TEST_PODMAN_HELP_AUDIT=1 \
  PATH=/usr/bin:/bin XDG_RUNTIME_DIR=/run/user/1001 \
  /usr/bin/python3 -I -S -B \
  /tmp/opencode/larainit-inventory-audit-itc6bby1/help-audit.py --inventory-only
```

#### Systemd placeholder diagnosis and approved runtime scope

The operator's passive diagnostic again passed metadata **1/0/0**, 58 filtered,
**0.13 seconds**, then reported the exact inaccessible entry:
`/run/user/1001/systemd/inaccessible/dir`, UID/GID **1001**, mode **0000**,
device **52**, inode **5**. Its saved failure explicitly reports help/info not
executed. Evidence directory:
`/tmp/opencode/larainit-inventory-audit-itc6bby1/results/help-h8ilatr0`.
The failed inventory does not establish unchanged runtime contents.

Systemd's official documentation describes mode-0000 inaccessible nodes as sandbox
mount sources, and upstream `src/shared/mount-util.c:mode_to_inaccessible_node`
maps user-context nodes to `$XDG_RUNTIME_DIR/systemd/inaccessible/<type>`.
This explains why full user-runtime recursion is unsuitable; it does not attest
the installed systemd build or independently verify the contents of that directory.
No permissions, ownership, mounts or services were changed to investigate it.

The user explicitly selected **Scoped audit**: recursively inspect Podman state,
but compare unrelated runtime directories only as entries, with no content claim.
The corrected harness at
`/tmp/opencode/larainit-runtime-scope-audit-f9h62v8n/help-audit.py` retains full graph
recursion and recurses into these runtime-root trees when present:
`containers`, `libpod`, `podman`, `netns`, `rootless-netns`. The configured store/
network and tmp/pause paths are covered by `containers` and `libpod`; the additional
Podman API and namespace trees are included explicitly. Runtime-root membership,
regular-file hashes and entry metadata are still compared. Unrelated directories
such as `systemd` are labeled **entry metadata only; contents not inspected**.
Changes below those unrelated directories can remain undetected and are not
certified as absent. The config/shared-lock, process, mount, fingerprint, budget,
no-follow and production profile guards remain unchanged. No general EACCES
exception, chmod, subtree repair or namespace permission workaround was added.

A regression recreated an unrelated mode-0000 sentinel and failed before the
scope correction. It now passes scoped inspection, detects new runtime-root
membership, and still rejects unreadable descendants in each of the five Podman
trees. Existing snapshot/security self-checks pass. Only test-owned temporary
directories had permissions changed; the dedicated fixture is untouched. Earlier
artifacts and the shared invocation claim are preserved. Actual help is still
unrun; one conditional info call remains authorized but cannot precede reviewed
help/security evidence. Production certification stays false.

The corrected help-only command was attempted as recorded below. Do not rerun it
or remove its shared claim until the earlier claimed attempt's evidence is reviewed.
The helper never runs `info --format json`.

```sh
LARAINIT_TEST_PODMAN_PROFILE=1 LARAINIT_TEST_PODMAN_HELP_AUDIT=1 \
  PATH=/usr/bin:/bin XDG_RUNTIME_DIR=/run/user/1001 \
  /usr/bin/python3 -I -S -B \
  /tmp/opencode/larainit-runtime-scope-audit-f9h62v8n/help-audit.py
```

#### Existing invocation claim: review evidence, do not retry (2026-10-13)

The operator's latest attempt passed metadata **1/0/0**, 58 filtered, **0.13s**,
and completed the scoped before-inventory/`before.json` step. It reported evidence
directory `results/help-aet2c5ml` under the runtime-scope artifact, then refused at
the exclusive claim open with `FileExistsError` for
`/tmp/opencode/larainit-info-audit-kpoi8k58/results/help-invocation-1001`.
Source ordering establishes no Podman spawn in **this attempt** and no after-state
comparison or help/security pass. A completed single before snapshot is not evidence
of unchanged state around execution. No wrapper exit status was supplied.

The agent passively confirmed the claim is a UID/GID **1001**, mode **0600**,
74-byte file (inode **67716**); reading it as UID 1000 was refused. The staged
runtime-scope script hash remains unchanged. The claim may refer to an earlier
attempt not included in the supplied output; its presence alone proves neither
Podman execution nor a completed audit. Prior help history is now unresolved,
not assumed unrun or successful. No claim deletion, replacement, permission change,
new claim path, engine retry or gate change is authorized by this refusal.

As the dedicated account, read the claim and provide its named evidence directory's
`summary.json` and `help.stderr`, if present. Missing files are incomplete evidence,
not permission to retry. The existing summaries are written before the final metadata
test, so they alone do not attest that final test's completion.

```sh
/usr/bin/cat /tmp/opencode/larainit-info-audit-kpoi8k58/results/help-invocation-1001
```

Conditional JSON info and production certification remain blocked pending review.

#### Claimed help result: executable join observed, filesystem change unresolved

The operator read the claim, which names
`/tmp/opencode/larainit-runtime-scope-audit-f9h62v8n/results/help-23e1w9yk`.
The supplied summary fields report help exit **0**, **10** process samples and
joined executable label **`podman (unconfined)`**. They report no JSON info
execution, but `changed_evidence=["files"]` and exactly one changed path:
`/run/user/1001/libpod`. This provides real-executable join/security-context
evidence beyond the earlier Python-child test, not a passing whole help audit.
No mount/process differences were reported in those fields. Their comparison
scope remains bounded; unrelated runtime contents and transient effects are not
attested. The supplied summary text lacks its closing brace; no complete wrapper
transcript/status or explicit stderr-content confirmation was supplied.

`error=null` reflects no captured help-command error; the helper still refuses
because filesystem differences are nonempty, before the final metadata test.
No final metadata pass is inferred and the invocation claim must remain intact.

Reviewed common `pkg/config/default.go:defaultEngineConfig` calls `defaultTmpDir`.
For an existing `$XDG_RUNTIME_DIR/libpod`, that function unconditionally attempts
`os.Chmod(..., 0700|ModeSticky)`. Root flag initialization accesses Podman config
before help dispatch, so this is a candidate explanation for the observed directory
change even though help skips engine pre-run. Exact before/after entry fields are
still needed: neither a ctime-only update nor a changed mode/identity is yet established.
Do not hide ctime differences, accept changed permissions or repair/retry to obtain
a pass. Actual info/startup/cleanup certification stays blocked pending that review.

#### Ctime-only result reviewed; conditional JSON-info observer staged

Operator extraction of the saved `libpod` entry reports identical first eight stat
fields before/after: device **52**, inode **81**, mode **17344** (directory with
permissions **01700**), UID/GID **1001**, link count **3**, size **60**, mtime ns
**1791764404811719978**. Only ctime differs, from **1791764777095574406** to
**1791838935666034026**. This is observed metadata bookkeeping consistent with
the reviewed same-mode chmod, not changed permissions/identity and not proof of
the particular syscall or absence of concurrent activity. The earlier helper's
change refusal is preserved; it is not rewritten as a full unchanged-state pass.

The already authorized conditional info observation is now staged at
`/tmp/opencode/larainit-json-info-audit-v873rbbk/info-audit.py`. It reads the existing
help receipt and saved before/after/summary files without executing help. It requires
the exact known label/result and ctime-only difference, rejects all other file/
mount/process/critical pause-identity changes, and retains both actual ctime values.
A fresh full metadata preflight runs after this review; the unreached old final test
is not inferred as passed. Current `libpod` must already have UID/GID 1001 and
directory mode 01700, so no automatic permission repair is accepted as a prerequisite.

The observer reuses the hash-verified scoped inventory/process helpers, existing
metadata test executable and pinned Podman binary. It retains HOME/PATH/runtime,
environment and umask rather than clearing overrides or silently changing the engine
context. Before execution it saves scoped filesystem/mount/process snapshots, repeats
metadata preflight, and creates a separate exclusive **info** claim in the original
shared claims directory. The **help** claim is untouched. No fallback, repeat or new
claim path is allowed if the info claim already exists.

It executes exactly `/usr/bin/podman info --format json`, without store overrides,
with null stdin and separate evidence files. Checked bounds are **10 seconds**,
**64 KiB combined output**, **1000 process snapshots** and **16 sampled pause-mount
transitions**; inventory bounds remain unchanged. It samples dedicated-account
executables and the expected-namespace direct Podman label. Polling can miss brief
helpers/mounts and is explicitly not syscall-complete lifecycle/provenance evidence.
Only the unreaped direct child may be killed on interruption/timeout; no descendant,
pause or unrelated-process cleanup is attempted.

After any attempted launch it attempts to save process/mount samples, after-context
and filesystem snapshots, every changed file's before/after records, a fresh metadata
result, raw stdout/stderr and parsed JSON. Persistent/unknown changes are retained for
review, not repaired or automatically classified away. It always ends with **STOP FOR
REVIEW**; `acceptance_passed=false` and production certification false even if the
command returned zero. Raw info fields still need comparison with the existing typed
candidate validator; parsing JSON is not validation or a workload permission.

Its engine-free self-check passes precise ctime-only review, non-ctime/mode/identity/
mount/process and contradictory-summary refusal, bounded no-follow evidence reads,
private-parent protection, and evidence-preservation assertions. No dedicated-account
fixture or Podman process ran while staging it; source/checklists/gate are unchanged.
Expected ordinary lock/event bookkeeping and temporary overlay bind/private/unmount
effects are within the existing info authorization, not an initialization/refresh/
recovery, preparation, workload, build, repair or production-enabling authorization.

The operator has now used this one-shot invocation, as recorded below. The command
is historical: keep its claim/evidence; do not rerun it or repair state.

```sh
LARAINIT_TEST_PODMAN_PROFILE=1 LARAINIT_TEST_PODMAN_INFO_AUDIT=1 \
  PATH=/usr/bin:/bin XDG_RUNTIME_DIR=/run/user/1001 \
  /usr/bin/python3 -I -S -B \
  /tmp/opencode/larainit-json-info-audit-v873rbbk/info-audit.py
```

#### Authorized JSON-info result (2026-10-13): field match, effects review pending

Operator output names
`/tmp/opencode/larainit-json-info-audit-v873rbbk/results/info-naww9sv_` and reports:

- Exact metadata preflight **1/0/0**, 58 filtered, **0.16s**; second pre-launch
  preflight **1/0/0**, **0.13s**; post-info preflight **1/0/0**, **0.13s**.
- Info exit **0**, no captured command error, empty stderr, observed elapsed
  **0.901503077999223s**, direct joined label **`podman (unconfined)`**.
- Before/after caller/pause mount tables unchanged, dedicated-account process
  identity set unchanged, and no critical pause identity change reported.
- Exactly two changed filesystem paths: prepared graph `storage.lock` and runtime
  `libpod`. **One** pause-mount transition was sampled; its rows and the exact
  filesystem deltas remain to be inspected. Sampling is not syscall-complete.
- Raw stdout SHA-256 reported as
  `4068dd09bb1a7be07d94d0643986e1c192ad90ff96aab2762740c6684e3c721f`.
  This hash identifies the operator's captured raw bytes, not the projected unit
  fixture or a separately observed agent execution. No wrapper exit status supplied.

The reported required fields match local rootless Ubuntu 26.04/amd64, systemd/v2,
crun, SQLite, native overlay and the exact graph/run/volume and helper paths. Security
reports rootless/ seccomp true, SELinux/AppArmor workload support false, and the
expected seccomp profile path. This does not imply host AppArmor is disabled or a
workload seccomp filter is installed. The store contains **0 images / 0 containers**;
the required image/sentinel are absent in this reported store; the group-only file
remains unverified. `serviceIsRemote=false` remains
the locality field even though `remoteSocket.exists=true`; that socket report does
not prove remote CLI transport or authorize service changes. The reported nominal
`store.configFile` path likewise does not prove that config file exists or was used;
all three strict preflights passed the profile's absence/override guards.

Added a nonignored engine-free regression using a required-field projection of the
operator JSON through the **existing Rust `validate_info`**, rather than a duplicate
Python validator. Expected dedicated paths are independently fixed, not derived
from the fixture. It passes. The fixture is not byte-identical raw stdout, independent
engine execution or a full JSON-field/dependency attestation. Full normal suite
**135/0/18**, formatting, standalone mock formatting and denied-warning Clippy pass.
Only the unit test/fixture and evidence changed; production validation/gate is unchanged.

Verified storage source provides candidate effect explanations: `store.Shutdown`
calls graph-lock `RecordWrite` before driver cleanup; Unix `RecordWrite` writes a
new token at offset zero. Overlay `Init` makes its home private, binding it if needed;
cleanup can unmount that home. `defaultTmpDir` can repeat same-mode chmod on runtime
libpod. These source paths are not proof of the exact sampled mount or observed
lock-file delta. Inspect saved `file-changes.json` and `pause-mount-samples.json`
relative to `before.json`/`after.json` before classifying these effects.

The one authorized info observation is spent. Preserve both invocation claims; do
not repeat help/info, repair/reset state, prepare fixtures or run blocked workloads.
Raw observer `acceptance_passed=false` is retained, not rewritten as a certification
pass. T021 and broader startup/refusal/concurrent-reader/provenance/workload gates
remain open; production certification stays false and task total **31/50**.

#### Saved info deltas reviewed (2026-10-13)

Operator supplied the saved filesystem differences and each sampled pause-mount
delta. Runtime `libpod` again differs only in ctime, from
**1791838935666034026** to **1791841052986350737**; device/inode, directory
01700 mode, UID/GID, links, size and mtime remain identical. The graph
`storage.lock` keeps device **66312**, inode **15764704**, regular-file mode
**0644**, UID/GID **1001**, one link and **64 bytes**. Its SHA-256 changes from
`f3ad74b75d76a0e6e4f6704737226fc3a8d10121daa72caff1f46247746d0bd8` to
`79801c5ff37c51802516198cc5f4674b7381793e35f775717f2009a57aa02a42`;
both mtime/ctime change from **1791764777678580442** to
**1791841053866359617**. This is a real content write, not merely taking a lock.
Verified storage `newLastWrite` serializes a 64-byte time/counter/PID/random token,
consistent with the reviewed shutdown `RecordWrite` path. Hashes/stat alone do not
decode the token, prove a particular writer or exclude concurrent activity.

The only added sampled mount row is ID **1328**, parent **620**, device **259:8**,
root and mountpoint both the prepared graph's `overlay` directory, writable ext4
on `/dev/nvme1n1p1`. It has no shared/master propagation fields, consistent with a
private self-bind of overlay home, not a newly mounted container overlay filesystem.
That exact row is removed in the after snapshot, with no other rows added/removed
in these comparisons. Together with the matching final caller/pause mount tables,
this supports observed overlay-home cleanup for this invocation; polling does not
prove every intermediate mount operation or behavior on populated/concurrent stores.

These reported deltas fit the already authorized ordinary lock bookkeeping,
same-mode directory chmod and temporary overlay-home effects. **No unexpected lasting
change is evidenced within the scoped snapshots**; this is not unchanged-content
certification of unrelated runtime directories or a full startup certification pass.
The original refused help result and `acceptance_passed=false` info receipt are
unchanged. No command rerun, repair, claim removal, checklist mark or gate change.
Remaining work is startup/refusal/concurrent-reader and helper/dependency evidence,
then separately authorized fixture/workload/build acceptance; **31/50**, T021 open.

#### Saved package-query/process context reviewed (2026-10-13)

Operator supplied new-process and namespace-transition records from the existing
info samples. Podman PID **320466**, starttime **13099089**, parent **320453**,
keeps the exact `info --format json` command while moving from
`user:[4026531837]` / `mnt:[4026531832]` to
`user:[4026533212]` / `mnt:[4026533216]`. The original observer's joined predicate
matched the sample to its saved prepared-pause namespace IDs.

Twelve distinct `/usr/bin/dpkg-query` children, parent **320466**, are sampled in
that joined namespace pair: one `--search -- <absolute-helper-path>` and one
`-f ${Package}_${Version}_${Architecture} -W <package>` for each of netavark,
aardvark-dns, slirp4netns, pasta (package **passt**), crun and conmon. This agrees
with reviewed common `version.Package` and dpkg search/show source. Search samples
use argv[0] `dpkg-query`; `/proc/exe` supplies the actual absolute executable.
The short earlier dpkg execution before its search-backend exec was not sampled.
This is positive evidence of the selected query executable/arguments, not complete
version-helper, library, configuration-read or syscall tracing.

All supplied process contexts show UID/GID **1001** in all four credential slots,
group **1001**, label **`podman (unconfined)`**, `NoNewPrivs: 0`, `Seccomp: 0`.
Credentials are reported in the host observer's namespace; they do not mean the
joined child failed to acquire mapped root inside the rootless user namespace.
The inherited label is observed on the executable-specific dpkg-query samples,
not assumed to be dpkg-query's own independently attached profile. Zero seccomp
and no-new-privileges values describe the CLI/query processes: the info JSON's
seccomp-support field is not a claim that these processes or a future workload
have an installed filter. No workload security test or host confinement change.

A second sample for query PID **320490** retains its starttime/parent/namespaces
but has null executable and empty command line. That is incomplete evidence
compatible with exit, not a new executable or proof of a zombie; State/exe_error
were not included in the operator extraction. No conmon/crun/network-helper
version process is present in the supplied new-process samples. Polling can miss
them; returned version strings and prior source review do not remove that limit.

Recorded the supplied PID/starttime/argument/context evidence in
`/tmp/opencode/larainit-json-info-audit-v873rbbk/operator-info-process-review.json`.
Original samples/claims unchanged, no rerun/repair, no source or checklist change.
Package-query selection/context is now observed for this one invocation, while
complete helper/provenance, startup/refusal/concurrency and separately authorized
fixture/workload/build/CI acceptance remain open. Production gate false; **31/50**.

### Remaining mount and helper paths

- Default native-overlay `Init` calls `mount.MakePrivate(home)` unless
  `skip_mount_home` is set; the pinned packaged configuration leaves it unset.
  `storage/pkg/mount/sharedsubtree_linux.go:ensureMountedAs` checks whether the
  overlay home is mounted, creates a bind mount if it is not, and changes mount
  propagation. Cached feature files do not remove this branch. The relevant
  existing pause-namespace mount state and permissible side effects still need
  certification; directory/database readiness alone does not establish it.
- CLI `Execute` normally shuts down its ABI engines, leading through
  `libpod.Runtime.Shutdown` and `libimage.Runtime.Shutdown` to `store.Shutdown(false)`.
  With no mounted layers, storage records a graph-lock write and invokes driver
  cleanup; overlay cleanup prunes staging and can unmount its home. With mounted
  layers, nonforced shutdown reports the layer-in-use condition instead of driver
  cleanup. No forced shutdown was requested here. This mount lifecycle must be
  considered with startup, not mistaken for a read-only info process.
- XFS is now explicitly excluded. `quota.NewControl` can create/replace
  `backingFsBlockDev` through a temporary device node, set project quotas and clear
  the home directory's project-inherit flag. Do not rely on rootless permission
  errors to prevent side effects. The guard uses `fstatfs` on the existing checked
  overlay directory; it neither assigns quotas nor installs a filesystem helper.
  A synthetic magic-value regression tests XFS refusal and a real test-owned
  directory checks the descriptor-based path without changing its inode, mtime or ctime.
- The C constructor permits the namespace shortcut for `info` and joins the
  existing pause user/mount namespaces before Go startup. This does not create a
  fresh per-info mount namespace: mount effects can persist in the pause namespace.
  Accessible namespace descriptors alone do not prove a successful join or the
  namespace's path/propagation state. If joining fails, the later Go
  `TryJoinPauseProcess` can acquire a lock and remove the pause PID file. The operator
  has now demonstrated the Python-child join and passed the full namespace-view/
  pause-overlay preflight. Podman's own executable/security-context join and actual
  startup/propagation/cleanup behavior remain unverified by those checks.
- The constrained helper/package probes above still need live binary-selection,
  successful version/query and filesystem/process-effects evidence in the actual
  pause namespace. Installed-file hashes and reviewed source behavior do not
  independently attest the complete builds or dynamic dependencies. The fixture's
  expanded startup/storage/helper guards have passed the new metadata-only reader
  audit. Seccomp/AppArmor info fields and actual helper execution remain unverified
  by that run.

A separate passive namespace inspector is staged at
`/tmp/opencode/larainit-helper-audit-oi0avd28/namespace-readiness.py`. Run it only as
the dedicated account with `/usr/bin/python3 -I -S -B`; its engine-free
`--self-check` passed mount selection, escaped paths and malformed-input cases.
It reads namespace links/mappings/mountinfo and compares caller versus pause-root
device/inode identities for the storage directories and 17 pinned paths. It does
not join a namespace, execute a subprocess, mount anything or create fixture state.
DIFFERENT/UNREADABLE results are unresolved evidence, not a passing certification
or an invitation to repair permissions. The operator result is recorded above;
the inspector's narrower observations do not exercise the new production guards.

The certification constant remains false. More passing directory checks are not
permission to ignore these remaining mount/helper paths. The mount lifecycle above
was traced in the checksum-verified Ubuntu original archives; no mount command,
storage option change or real Podman execution was used to investigate it.

## Remaining certification evidence

- Establish that eligible startup avoids initialization, refresh, recovery and
  mount probing, including remaining transitive startup paths and security-mode
  evidence; preserve the reader's enforced nonwriting boundary.
- Only after this evidence is complete, enable the production profile and record
  dedicated-account readiness. Then prepare workload prerequisites deliberately
  and run separately authorized real acceptance tests.

Do not reset storage, synthesize cache markers or relax checks to obtain a pass.
Do not run the blocked ignored Podman workload suite. The image, sentinel and
group-file prerequisites remain unprepared; missing evidence is not a passing skip.
