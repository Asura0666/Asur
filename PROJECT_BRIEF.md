# Asur: Project Brief

> Read this first. It explains what Asur is, why it exists, what to build, and what NOT to build.

## 1. One-liner

**Asur** is a fast, single-binary Rust CLI that removes the daily friction of local development: spinning up containers, finding port conflicts, managing `.env` files, and catching leaked secrets, all through simple interactive commands with a clean terminal UI.

## 2. Why this exists

Developers waste time on small, repetitive tasks:

- Writing long `docker run` commands and forgetting the flags, versions, or passwords.
- Typos (`reids` instead of `redis`) and digging through Docker Hub for the right tag.
- `docker ps` output that is hard to read; hunting for container logs by ID.
- "Port 3000 already in use", with no idea who is using it.
- `.env` and `.env.example` drifting apart; `.env` accidentally committed.
- API keys leaked into git history.
- Disk bloat from forgotten containers, images, and volumes.

Asur is one tool that fixes all of these with a friendly, forgiving, interactive experience.

## 3. Target users

Any developer on macOS, Linux, or Windows who uses Docker (or Podman) locally. Beginners should be able to use it without reading docs; experienced users should be able to go fast with flags.

## 4. Design principles

1. **Fast**: instant startup, no runtime, one static binary.
2. **Simple by default, powerful on demand**: "quick" mode asks almost nothing; "custom/advanced" mode exposes every option.
3. **Forgiving**: typos get suggestions ("Did you mean `redis`?"), never a dead end.
4. **Never block the user**: long-running work (image pulls, container starts) runs in the background while the user keeps working.
5. **Interactive AND scriptable**: every interactive command has flags and a `--json` output mode for scripts and CI.
6. **Safe**: never print full secrets in scan results (mask them); never delete anything without confirmation (unless `--yes`).
7. **Pretty**: clean tables, colors, progress bars, but degrade gracefully when output is piped or `NO_COLOR` is set.

## 5. Tech stack (recommended)

| Need | Crate |
|---|---|
| Arg parsing, completions | `clap` (derive) + `clap_complete` |
| Docker API (do NOT shell out to `docker`) | `bollard` |
| Async runtime | `tokio` |
| HTTP (Docker Hub tags/versions) | `reqwest` |
| Interactive prompts/menus | `inquire` (or `dialoguer`) |
| Progress / multi-progress | `indicatif` |
| TUI for ps/logs/stats (optional) | `ratatui` |
| Fuzzy matching ("did you mean") | `strsim` (+ `nucleo`/`fuzzy-matcher` for pickers) |
| Process/port info | `sysinfo`, `netstat2` or `listeners` |
| Clipboard | `arboard` |
| Git history scanning | `git2` |
| Secret detection | `regex` + Shannon entropy |
| Errors | `anyhow` / `thiserror` |
| Config & data | `serde`, `serde_json`, `toml`, `serde_yaml` |
| Colors | `owo-colors` |
| Password generation | `rand` (use `OsRng`) |

Support Docker first; design the container layer behind a trait so Podman can be added later.

## 6. Command overview

```
asur up <service...>        # spin up containers (wizard)
asur ps                     # pretty docker ps
asur logs                   # pick a container, view/follow logs
asur sh                     # pick a container, open a shell
asur db                     # jump into psql / redis-cli / mysql / mongosh
asur clean                  # find and remove unused containers, images, volumes
asur port <n>               # who is using this port? offer to kill
asur port free <n>          # next free port starting at n
asur env example            # generate .env.example from .env (values stripped)
asur env check              # validate required keys and types
asur env diff               # compare .env vs .env.example
asur scan                   # find leaked secrets (files + git history)
asur scan --install-hook    # install pre-commit hook
asur ignore                 # detect project type, add .gitignore / .dockerignore etc.
asur doctor                 # (stretch) diagnose Docker not running, conflicts, etc.
```

## 7. v1 Features in detail

### 7.1 `asur up`: container wizard (the flagship feature, make this excellent)

**Flow**

1. User types `asur up postgres` (or just `asur up` for a searchable picker).
2. **Name resolution**: look up the service in a built-in catalog (postgres, redis, mysql, mariadb, mongodb, rabbitmq, kafka, minio, elasticsearch, etc.). If not found, fuzzy match and ask: `Did you mean redis? (Y/n)`. If there is no close match, optionally search Docker Hub.
3. **Version selection**: fetch available tags from Docker Hub (cache locally with a TTL so it works offline and fast). Show a short list with labels such as `latest`, `LTS`, and recent major versions, then let the user pick. Hide junk tags (`-alpine-slim-rc1` etc.) unless "advanced" is chosen.
4. **Mode selection**: `Quick` or `Custom`.
   - **Quick**: sensible defaults (default port, container name, restart policy, named volume for data). **Passwords/credentials are auto-generated** (strong, random, from `OsRng`).
   - **Custom**: prompt for everything the user cares about: password (typed by the user, hidden input), username, database name, port, volume, container name, env vars, network, etc. Whatever the user types is used as-is and executed for them.
   - **Advanced** (inside Custom or via `--advanced`): reveal extra args (memory limit, extra env, command overrides, healthcheck, etc.).
5. **Port conflict handling**: if the default port is taken, tell the user and suggest the next free one (reuse the `port free` logic).
6. **Non-blocking execution (important)**: once the user confirms the configuration, start pulling/creating the container **in the background** (a spawned async task with a progress bar). Do NOT make the user wait. Immediately ask:

   > `postgres is starting... Spin up another service, or stop here? [another / done]`

   The user can queue as many services as they want. When they choose "done", show all progress bars until everything is finished, then print the summary.
7. **Result summary** for each container:
   - Status (running/healthy), container name, port.
   - **Connection string**, e.g. `postgres://user:pass@localhost:5432/db`, **copied to the clipboard** (if multiple services, copy the last one or ask which).
   - Generated credentials shown once, clearly.
8. **Multi-service**: `asur up postgres redis` runs the wizard for each (sharing the same non-blocking behavior) and offers to **generate a `docker-compose.yml`** from the choices (`--compose` to skip the question).
9. **Saved profiles** (nice-to-have for v1.1): `asur up --save my-stack` / `asur up my-stack`.

**Edge cases**: Docker not running (clear message, how to start it); image pull failure; name already used (offer to reuse, rename, or replace); no network (use cached catalog/tags if available).

### 7.2 `asur ps`
Pretty, readable `docker ps`: name, image, status (colored), ports, uptime, CPU/mem (optional). Flags: `-a` all, `--json`.

### 7.3 `asur logs`, `asur sh`, `asur db`
- All three start with a **searchable container picker** (fuzzy filter) if no container is named.
- `logs`: stream and follow, with `--tail N`, `--since`, and level highlighting if the logs are JSON/standard formats.
- `sh`: exec into the container, trying `bash` and falling back to `sh`.
- `db`: detect the DB type from the image and open the right client inside the container (`psql`, `redis-cli`, `mysql`, `mongosh`), using stored/generated credentials when available.

### 7.4 `asur clean`
List stopped containers, dangling/unused images, and unused volumes **with sizes**. Multi-select what to delete, show total space to be reclaimed, and confirm. Never remove volumes silently; warn that volume deletion means data loss.

### 7.5 `asur port`
- `asur port <n>`: show the process (name, PID, user, command) using the port, then ask `Kill it? (y/N)`. Handle permission errors gracefully (suggest sudo/admin). If a Docker container owns the port, say so and offer to stop the container instead of killing the proxy process.
- `asur port free <n>`: print the next free port at or above `n` (`--copy` to clipboard).

### 7.6 `asur env`
- `example`: generate `.env.example` from `.env`, keeping keys and comments, stripping values (optionally keeping safe placeholders such as `PORT=3000`).
- `diff`: show keys missing from either file, and extra keys.
- `check`: validate required keys and types using a small schema (inferred from `.env.example`, or an optional `.asur.toml`). Types: `string`, `number`, `bool`, `url`, `email`. Example: `PORT` must be a number, `DATABASE_URL` must be a URL.
- **Pre-commit hook** that blocks committing `.env` files.

### 7.7 `asur scan`: secret scanner
- Scan working-tree files **and git history** (all commits/branches, via `git2`).
- Detection: known provider patterns (AWS, GitHub, Stripe, Google, Slack, etc.) + high-entropy strings + suspicious variable names. Respect `.gitignore` and skip binaries/large files.
- Output: file, line, commit (for history hits), rule matched, and a **masked** preview (`AKIA****XYZ`). Never print the full secret.
- Support an allowlist/ignore file for false positives.
- **Remediation offers**: if a leaked file is not ignored, offer to add it to `.gitignore`. Remind the user that a leaked key must be **rotated**, since deleting it is not enough.
- `--install-hook`: installs a pre-commit hook that scans staged changes (fast), failing the commit on findings. Do not clobber an existing hook; append or ask.
- Exit non-zero on findings so it works in CI.

### 7.8 `asur ignore`
Detect the project type (Rust, Node, Python, Go, Java, etc., plus Docker usage) from marker files, then create or update `.gitignore`, `.dockerignore`, and other relevant files. Merge into existing files without duplicating or removing user entries, show a diff preview, and ask before writing. Always make sure `.env` is covered.

## 8. What NOT to build (non-goals)

- Not a Docker Desktop or Portainer replacement; no web UI, no GUI.
- Not a Kubernetes tool; no orchestration, cluster management, or cloud deploys.
- Not a full secret-management platform (no vault, no cloud sync). It only detects leaks.
- Not a CI/CD system or deployment tool.
- Do not shell out to the `docker` CLI for core features; use the Docker API.
- No telemetry or analytics by default. If ever added, it must be opt-in and documented.
- No always-running daemon in v1.
- Do not store generated passwords in plaintext in an unprotected place without telling the user (see Security).
- No feature creep: if it is not listed here, discuss before adding.

## 9. Security & privacy rules

- Generate passwords with a cryptographically secure RNG, with a sensible length and character set.
- Show generated credentials once in the summary; if stored for `asur db` convenience, store them locally with restrictive file permissions (0600) and document where. Prefer reading credentials from the container's environment instead of duplicating them.
- Secrets in scan output are always masked.
- Never send any scanned content or secrets over the network. Only Docker Hub tag lookups go out, and nothing about the user's code.
- Destructive actions (kill, delete, volume removal) always require confirmation unless `--yes` is passed.

## 10. UX & quality bar

- Startup time under about 50 ms for non-network commands.
- Every error is human-readable and says what to do next.
- Works when piped: no prompts or color in non-TTY mode, and `--json` available on list/inspect commands.
- Shell completions for bash, zsh, fish, and PowerShell.
- Cross-platform: macOS, Linux, Windows. Be careful with port/process lookup and path handling.
- Tests: unit tests for fuzzy matching, env parsing/validation, and secret rules; integration tests against a real Docker daemon in CI.

## 11. Suggested architecture

```
src/
  main.rs            # entry, clap setup
  cli/               # one module per command (up, ps, logs, sh, db, clean, port, env, scan, ignore)
  docker/            # trait ContainerRuntime + bollard impl (Podman impl later)
  catalog/           # built-in service catalog (image, default port, env vars, client cmd, conn-string template)
  hub/               # Docker Hub tags client + local cache
  ui/                # prompts, tables, progress, theme
  scan/              # rules, entropy, git history walker
  env/               # parser, diff, validator
  util/              # ports, clipboard, passwords, fuzzy
```

The **service catalog** (data-driven, ideally TOML/JSON embedded in the binary) is the key design choice: adding a new service should mean adding data, not code. Each entry defines the image name, default tag strategy, default port, required env vars (and which are passwords), data volume path, connection-string template, and the CLI client command for `asur db`.

## 12. Milestones

1. **M1**: project skeleton, clap, UI helpers, Docker connection, `asur ps`.
2. **M2**: catalog + `asur up` (quick mode, fuzzy match, connection string + clipboard).
3. **M3**: custom/advanced mode, version picker, port-conflict handling, non-blocking multi-queue flow, compose generation.
4. **M4**: `logs`, `sh`, `db`, `clean`.
5. **M5**: `port` (who/kill/free).
6. **M6**: `env` (example, diff, check) + `.env` pre-commit hook.
7. **M7**: `scan` (files, history, hook, gitignore offer) + `ignore`.
8. **M8**: polish: completions, `--json`, docs, release pipeline (GitHub Releases, `cargo install`, Homebrew, winget/scoop).

## 13. Open questions (decide early)

- Final check that the name `asur` is available on crates.io, GitHub, and Homebrew.
- Where and how to store generated credentials (file, OS keychain, or only show once).
- Docker-only for v1, or abstract for Podman from day one (recommended: abstract, implement Docker only).
- Exact secret-rule set and false-positive strategy.

## 14. Instructions for any agent working on this project

- Treat Sections 7 and 8 as the source of truth for scope. If a request conflicts with a non-goal, ask first.
- Build in milestone order; keep each milestone shippable and tested.
- Keep the catalog data-driven and the Docker layer behind a trait.
- Prefer small, well-tested modules over clever abstractions.
- Never log or print full secrets. Never delete user data without confirmation.
- When unsure about a UX decision, choose the simpler path with fewer prompts, and expose the extra options behind an "advanced" path.
