# Contributing to ModelMeter

## Development environment

### Prerequisites

- **Rust** stable toolchain — the version is pinned in `rust-toolchain.toml` and will be installed automatically by `rustup` on first build.
- **Node.js** 20+ with npm — for the React frontend.
- **Tauri prerequisites** — platform-specific system libraries. Follow the [Tauri prerequisites guide](https://tauri.app/start/prerequisites/) for your OS before attempting a build.
- **Perl** — required on macOS to compile vendored OpenSSL (SQLCipher). Already present via Xcode. Not required on Windows — pre-built static OpenSSL libs are checked in under `deps/openssl-windows-x64/` (see that directory's `openssl-version.txt` for regeneration instructions).

### First build

```sh
# Install frontend dependencies
cd src-tauri/../ && npm install

# Build and run the development app (hot-reload enabled)
npm run tauri dev
```

### Running checks locally

```sh
# Rust
cargo fmt --check
cargo clippy -- -D warnings
cargo test

# Frontend
npm run format:check
npm run lint

# Security audit
cargo audit
cargo deny check
```

## Project structure

```
modelmeter/
├── src/              # React + TypeScript frontend
├── src-tauri/        # Tauri shell crate
│   ├── src/
│   └── Cargo.toml
├── crates/
│   └── core/         # Core Rust crate: storage, providers, sync, secrets
├── Cargo.toml        # Workspace root
├── rust-toolchain.toml
└── package.json
```

## Coding standards

### Rust

- **Formatting:** `rustfmt` with default settings. Run `cargo fmt` before committing; CI rejects unformatted code.
- **Linting:** Clippy with `-D warnings` — all lints are errors. Use `#[allow(...)]` only with a comment explaining why.
- **Naming:** `snake_case` for functions/variables/modules, `PascalCase` for types/traits/enums, `SCREAMING_SNAKE_CASE` for constants.
- **Error handling:** `anyhow::Result` in application code, `thiserror` for library-style error enums. Avoid `unwrap()` and `expect()` outside tests and `main()`.
- **Unsafe code:** Forbidden (`#![forbid(unsafe_code)]`). Any exception requires a `SAFETY` comment and code review.
- **Dependencies:** Pin exact versions in `Cargo.toml`, commit `Cargo.lock`. Prefer well-maintained crates with low transitive dependency counts.
- **Logging:** `tracing` crate with structured logs. No `println!` in production paths.

### TypeScript / React

- **Language:** TypeScript with strict mode enabled.
- **Formatting:** Prettier with default settings.
- **Linting:** ESLint with the recommended TypeScript ruleset.
- **State:** Kept minimal — the Rust backend is the source of truth; the frontend renders.

### Security

- Never log API keys, decrypted secrets, or full request/response bodies.
- Redact bearer tokens in any error output.

## Git and commits

Commits follow [Conventional Commits](https://www.conventionalcommits.org/) format:

```
feat: add spending over time widget
fix: handle empty usage_records result on first sync
chore: update reqwest to 0.12.4
docs: document key rotation behaviour in README
refactor: extract balance computation into separate function
test: add integration tests for Anthropic cost report endpoint
```

- `main` is always releasable.
- Work happens on short-lived feature branches.
- Every change goes through a merge request with passing CI before merge.

## CI

The CI pipeline (`.gitlab-ci.yml`) runs on every push:

1. `cargo fmt --check`
2. `cargo clippy -- -D warnings`
3. `cargo test`
4. `cargo audit`
5. `cargo deny check`
6. Build on Windows and macOS

All checks must pass before a merge request can be merged.

## Releases

Releases are triggered by pushing a version tag (`v*`). The CI pipeline builds release binaries for Windows and macOS and uploads them to the GitLab Releases page. Version numbers follow [SemVer](https://semver.org/).
