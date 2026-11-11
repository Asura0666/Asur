# Asur v1.0.2

> One fast, single-binary CLI for everyday local dev — spin up containers, resolve port conflicts, sync `.env` files, and catch leaked secrets.

---

## Install

### Pre-built binary (recommended)

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
| `asur up <service...>` | Spin up containers with an interactive wizard |
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
