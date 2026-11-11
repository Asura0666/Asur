# Contributing to Asur

Thank you for considering a contribution! Asur is a small, focused CLI and we want to keep it that way — simple, fast, and reliable.

---

## Ground Rules

- **Read [PROJECT_BRIEF.md](PROJECT_BRIEF.md) first.** It defines scope and non-goals. If you are unsure whether a feature belongs, open an issue first before coding.
- One PR per logical change. Keep diffs small and focused.
- All public functions should have a doc comment.
- Never print secrets in plaintext anywhere in the code.
- Never delete user files or containers without an explicit confirmation (or `--yes`).

---

## Development Setup

You need Rust 1.85+ and Docker Desktop (or Docker Engine) running locally.

```bash
git clone https://github.com/your-username/asur.git
cd asur
cargo build
```

### Run the checks (all must pass before opening a PR)

```bash
cargo fmt --all -- --check    # formatting
cargo clippy --all-targets -- -D warnings   # lints
cargo test --quiet            # tests
```

---

## Project Layout

```
src/
  main.rs          # Entry point, module declarations
  cli/             # One file per command (up, ps, logs, sh, db, clean, port, env, scan, ignore)
  docker/          # ContainerRuntime trait + bollard implementation
  catalog/         # Built-in service catalog (services.toml embedded in binary)
  scan/            # Secret scanner (rules, entropy, walker, git history, allowlist, masking)
  env/             # .env parser, diff, validator, example generator
  ui/              # Terminal output helpers (error, warn, info)
  util/            # Ports, clipboard, password generation
```

---

## Adding a New Catalog Service

Edit [`src/catalog/services.toml`](src/catalog/services.toml). Each entry defines:

```toml
[[services]]
name        = "mydb"
image       = "mydb:latest"
default_port = 5555
env_vars    = [{ key = "MYDB_PASSWORD", kind = "password" }]
volume_path = "/data"
conn_string = "mydb://root:{password}@localhost:{port}"
db_client   = ["mydb-cli", "-h", "localhost", "-p", "{port}"]
```

No code changes needed for a new service — only data.

---

## Submitting a Pull Request

1. Fork the repo and create a branch: `git checkout -b feat/my-feature`.
2. Make your changes and ensure all checks pass.
3. Open a PR against `main` with a clear title and description.
4. Reference any related issue (e.g. `Closes #12`).

---

## Reporting Issues

Use GitHub Issues. Include:
- Your OS and Rust version (`rustc --version`).
- The exact command you ran and the full error output.
- Docker version if relevant (`docker --version`).

---

## License

By contributing, you agree that your contributions will be dual-licensed under MIT and Apache-2.0, the same as this project.

