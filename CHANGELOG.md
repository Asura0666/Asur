# Changelog

All notable changes to `asur` are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and this project adheres to [Semantic Versioning](https://semver.org/).

---

## [1.1.0] – 2026-11-12

### Added

- **Polished terminal presentation**:
  - Added shared headers, sections, aligned summaries, status messages, and terminal-mode fallback for rich, line-oriented, and plain output.
  - Added global `--plain`, `--no-color`, and `--no-progress` presentation flags.
  - Added elapsed-time spinners for indeterminate Docker and scanning work, plus determinate progress indicators where Docker reports byte totals.
- **Concurrent `asur up` operations**:
  - Queue multiple service launches without waiting for earlier pulls to complete.
  - Added live operation inspection with per-service lifecycle state, Docker pull status, errors, and elapsed time.
  - Docker pull events now update while waiting for operations, and progress is cleared before the final ready/error summary.

### Changed

- `asur scan` now reports streaming traversal progress and Git-history scan activity while preserving clean JSON output.
- `asur clean` now uses coordinated inspection and cleanup status, aligned resource summaries, and clearly labels reclaimed space as an estimate.
- Advanced container setup now defaults a named volume to `<container-name>-data`, including custom container names.

### Fixed

- Docker layer events are aggregated per operation to avoid resetting a pull display when Docker switches layers.
- Prevented a completed pull bar from lingering during image extraction or readiness checks.

---

## [1.0.3] – 2026-11-12

### Packaging & Distribution (crates.io readiness)

- **Crate Package Optimization**:
  - Added `exclude` rules in `Cargo.toml` (`.github/`, `.asurignore`, `PROJECT_BRIEF.md`, `CONTRIBUTING.md`) to package only necessary source, licenses, and documentation for crates.io.
- **Linux glibc Compatibility**:
  - Moved GitHub Actions release build runner to `ubuntu-22.04` (glibc 2.35) so Linux release binaries run across Ubuntu 22.04+, Debian 12+, and other mainstream distributions without requiring newer glibc.

---

## [1.0.2] – 2026-11-12

### Fixed & Security Hardening (Post-Audit Hardening)

- **Non-Interactive Container Removal Guard (`asur down`)**:
  - Enforced a non-interactive safety guard: scripts and CI environments must explicitly supply `--yes` (`-y`) to stop or remove containers, preventing accidental data or service loss during non-interactive runs.
  - Added warnings when `--volumes` (`-v`) is combined with removal in both interactive and automated contexts.
- **Fail-Safe Secret Scanner Coverage (`asur scan`)**:
  - `asur scan` now fails with a non-zero exit code if coverage is incomplete (files skipped due to the 1 MB limit, unreadable files, or inaccessible git history).
  - Added `--allow-partial` flag for workflows that intentionally wish to accept partial scans as success.
  - Both CLI exit status and JSON output (`status: "partial_clean"`) reflect incomplete coverage so CI pipelines never report false confidence.
- **Accurate Git Repository & Tool Diagnostics**:
  - Distinguishes between "not a git repository" and "git executable or repository operation failed".
  - Fails transparently with actionable diagnostics when git history is requested but git is unavailable or errors.

---

## [1.0.1] – 2026-11-12

### Fixed & Security Hardening (Production Readiness Audit)

- **Docker Volume Protection (`asur clean`)**:
  - `asur clean` in non-interactive / scripted environments now requires explicit `--yes` to proceed and never executes silent deletions.
  - Docker volumes are now strictly protected: automated cleanup will **never** delete volumes unless `-v / --volumes` is explicitly passed alongside `--yes`.
  - Added `--dry-run` flag to inspect cleanable resources without touching disk.
- **Safe Port Inspection (`asur port`)**:
  - `asur port <n>` is now strictly inspection-only by default. It will never terminate processes or stop containers without the explicit `-k / --kill` flag.
  - In non-interactive environments, `--kill` requires `--yes` to prevent accidental termination in CI/scripts.
- **Secret Scanner Reliability & Error Transparency (`asur scan`)**:
  - Scanner no longer silently ignores read errors or large files. Skipped files (>1 MB limit or read errors) are explicitly reported in human output and JSON.
  - Distinguishes between clean scans, partial coverage with skipped files, and findings.
  - Git history scanning errors are now explicitly reported instead of silently returning empty findings.
- **Predictable `.asurignore` Glob Matching**:
  - Replaced loose substring matching with predictable gitignore-style glob pattern matching (`fixtures/`, `*.test.ts`, `tests/**/*.key`).
  - Prevents false-positive ignore matches on unrelated files.
- **Robust `.env` Parsing**:
  - Added support for multiline values (e.g. RSA private keys and SSL certificates).
  - Fixed escaped quotes (`\"`) inside quoted values so strings are not truncated prematurely.
  - Fixed inline comment parsing to require whitespace before `#` in unquoted values (allowing `#` in URLs and fragments).
- **Automated SHA256 Checksums**:
  - Release pipeline now generates and publishes `.sha256` checksum files alongside Windows, macOS, and Linux release binaries.

---

## [1.0.0] – 2026-11-11

### Added

#### Container Management
- **`asur up <service>`** — Interactive wizard to spin up PostgreSQL, Redis, MySQL, MariaDB, MongoDB, RabbitMQ, Kafka, MinIO, and Elasticsearch containers.
  - Quick mode (auto-generates secure credentials) and Custom/Advanced mode (full control).
  - Automatic port conflict detection with smart next-free-port suggestion.
  - Cryptographically strong password generation (OsRng, 16 chars).
  - Connection string display and automatic clipboard copy.
  - Multi-service queue with non-blocking parallel startup.
  - `docker-compose.yml` export from chosen services.
- **`asur ps`** — Clean, readable container table with name, image, status (colored), ports, and uptime.
  - Flags: `--running`, `--json`.
- **`asur down [containers...]`** — Gracefully stop and remove containers with an interactive multi-select picker.
  - Flags: `--stop-only`, `-v / --volumes`, `-y / --yes`.
- **`asur logs [container]`** — Live-stream container logs with log-level color highlighting.
  - Flags: `-f / --follow`, `-t / --tail <N>`.
  - Clean single Ctrl+C exit (no double-press needed).
- **`asur sh [container]`** — Interactive shell inside any container (tries `bash`, falls back to `sh`).
- **`asur db [container]`** — Native client handover (`psql`, `redis-cli`, `mysql`, `mongosh`) with credentials extracted from container environment.
- **`asur clean`** — Scan for and remove stopped containers, dangling images, and unused volumes.
  - Shows reclaimable disk space upfront before prompting. Volume removal warns about data loss.

#### Port Tools
- **`asur port <n>`** — Identify which process or Docker container holds a port; offer to kill or stop it.
- **`asur port free <n> [--copy]`** — Find next available port at or above `n`, respecting Docker and OS sockets.

#### Environment (.env) Tools
- **`asur env example`** — Generate `.env.example` from `.env`, preserving comments and safe defaults, stripping secrets.
- **`asur env diff`** — Highlight missing or undocumented keys between `.env` and `.env.example`.
- **`asur env check`** — Validate required keys and types (`Number`, `Bool`, `Url`, `Email`).
- **`asur env hook`** — Install a Git pre-commit hook preventing `.env` from being committed.

#### Secret Scanner
- **`asur scan [path]`** — Scan working tree and git commit history for leaked secrets.
  - Provider patterns: AWS, GitHub, Stripe, Google API, Slack, Private Keys, Anthropic, OpenAI.
  - Shannon entropy analysis for generic high-entropy strings.
  - Output is always masked (`AKIA****MPLE`), never plaintext.
  - Allowlist via `.asurignore` and inline `# asur:ignore` comments.
  - Flags: `--no-history`, `--staged`, `--json`, `-y / --yes`, `--install-hook`.
  - `--install-hook`: installs non-destructive Git pre-commit hook.
  - Remediation: offers to add leaked files to `.gitignore`.
  - Exits non-zero on findings (CI-ready).

#### Ignore File Generator
- **`asur ignore [path]`** — Detect project stack and update `.gitignore` / `.dockerignore`.
  - Supports Rust, Node.js, Python, Go, Java, and Docker.
  - Merges into existing files without duplicates.
  - Shows diff preview; confirms before writing.
  - Flags: `--dry-run`, `-y / --yes`.

#### Shell Completions
- **`asur completions <shell>`** — Print completions for `bash`, `zsh`, `fish`, `powershell`, `elvish`.

