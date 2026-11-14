# Asur v1.2.0

> One fast, single-binary CLI for everyday local dev — spin up containers, resolve port conflicts, sync `.env` files, and catch leaked secrets.

---

## Install

### Via Cargo (crates.io)

```bash
cargo install asur
```

### Pre-built binary

Go to [**Releases**](https://github.com/Asura0666/Asur/releases/latest) and download the binary for your platform:

| Platform | File |
|---|---|
| Windows x86-64 | `asur-windows-x86_64.zip` |
| macOS (Apple Silicon M1/M2/M3/M4) | `asur-macos-arm64.tar.gz` |
| Linux x86-64 | `asur-linux-x86_64.tar.gz` |

Extract and place `asur` / `asur.exe` somewhere in your `PATH`.

### From source (requires Rust 1.85+)

```bash
cargo install --git https://github.com/Asura0666/Asur
```

Or clone and build locally:

```bash
git clone https://github.com/Asura0666/Asur.git
cd Asur
cargo install --path .
```

---

## Quick Start

```bash
# Spin up a postgres and redis with one command
asur up postgres redis

# Run any public, private, or locally built OCI image
asur up --image nginx:1.27-alpine --publish 8080:80

# Discover and inspect images
asur image search nginx
asur image tags nginx
# `tag` is also accepted; add --all for RC and CPU-specific tags
asur image tag typesense
asur image ls
asur image inspect nginx:1.27-alpine

# See all running containers
asur ps

# Check who is using port 5432
asur port 5432

# Scan your project for leaked API keys
asur scan

# Generate a .gitignore for your project type
asur ignore
```

---

## Commands

| Command | Description |
|---|---|
| `asur up <target...>` | Run catalog presets or qualified OCI image references |
| `asur up --image <reference>` | Explicitly run any public, private, or local image |
| `asur image search <query>` | Discover Docker Hub repository names and official-image status |
| `asur image tags <repository-or-name>` | List stable pullable tags (`tag` alias; `--all` includes raw variants) |
| `asur image ls` | See which images are already downloaded locally |
| `asur image inspect <reference>` | Show a local image's digest, ports, volumes, entrypoint, and command |
| `asur ps` | List containers in a readable table |
| `asur down [containers...]` | Stop and remove containers (`-y / --yes` in scripts, `-v` for volumes) |
| `asur logs [container]` | Pick a container and stream its logs |
| `asur sh [container]` | Open a shell inside a container |
| `asur db [container]` | Connect to a database via native client |
| `asur clean` | Remove unused containers and images (`--dry-run` to preview, `-v` for volumes) |
| `asur port <n>` | Inspect who is using a port (use `-k / --kill` to terminate) |
| `asur port free <n>` | Find the next free port at or above `n` |
| `asur env example` | Generate `.env.example` from `.env` |
| `asur env diff` | Show missing/extra keys between env files |
| `asur env check` | Validate required keys and value types |
| `asur env hook` | Block `.env` commits with a git pre-commit hook |
| `asur scan` | Scan for leaked secrets in files and git history (`--allow-partial` for CI) |
| `asur scan --install-hook` | Block secret commits with a pre-commit hook |
| `asur ignore` | Add `.gitignore` / `.dockerignore` entries |
| `asur completions <shell>` | Print tab-completion script |

### Supported services for `asur up`

`postgres` · `redis` · `mysql` · `mariadb` · `mongodb` · `rabbitmq` · `kafka` · `minio` · `elasticsearch`

Catalog names keep their guided defaults. A tagged, path-qualified, or digest-pinned reference such as `redis:7-alpine`, `ghcr.io/acme/orders:1.4`, or `repo/image@sha256:...` automatically uses generic-image mode. Unknown bare names require `--image` in non-interactive use so a catalog typo never silently pulls another repository.

Generic-image mode supports repeatable `--publish`, `--env`, `--env-file`, and `--mount` options, plus `--name`, `--network`, `--restart`, `--command`, `--pull missing|always|never`, and `--pin`. Publishing maps a port inside the container to a host port so browsers and programs on your computer can reach the service. Image-declared ports are selected for publishing by default in the interactive wizard and can be unselected with Space. Declared volumes remain opt-in.

An image reference without a tag means exactly `:latest`; Asur never interprets it as a request to pull every repository tag. Docker Hub selection prioritizes stable numeric versions and hides pre-release and CPU-specific variants. Docker Hub does not provide LTS metadata, so confirm long-term support in the image publisher's release notes. Press **Esc** in any interactive menu, or Ctrl+C at any time, to exit cleanly.

`asur image inspect redis` resolves downloaded tags such as `redis:7-alpine`; when several local tags match, Asur asks which one to inspect.

Private pulls use the standard Docker `config.json` and Docker credential-helper protocol. Sign in with `docker login [registry]`; Asur does not create credential storage or print registry tokens.

### Supported shells for `asur completions`

`bash` · `zsh` · `fish` · `powershell` · `elvish`

---

## Shell Completions

**PowerShell** (add to your `$PROFILE`):
```powershell
asur completions powershell | Out-String | Invoke-Expression
```

**Bash** (add to `~/.bashrc`):
```bash
source <(asur completions bash)
```

**Zsh** (add to `~/.zshrc`):
```zsh
source <(asur completions zsh)
```

---

## Design Principles

1. **Fast** — instant startup, single static binary, no runtime.
2. **Simple by default** — Quick mode asks almost nothing; flags unlock everything.
3. **Forgiving** — typos get suggestions, never a dead end.
4. **Safe** — secrets are always masked; nothing deleted without confirmation.
5. **Scriptable** — every list command has `--json` output for CI.

---

## Development

Requires Rust 1.85+ and Docker.

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to contribute.

---

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) — your choice.
