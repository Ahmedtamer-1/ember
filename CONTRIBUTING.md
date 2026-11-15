# Contributing to Ember

Thank you for your interest in contributing to **Ember**! Before opening a pull request, please review the project design rules in [AGENTS.md](AGENTS.md) and the checks below.

## Core Guidelines

- **System-Native WebViews Only**: Ember relies on [`tao`](https://crates.io/crates/tao) and [`wry`](https://crates.io/crates/wry) (WebView2 on Windows, WebKit on macOS/Linux). Do not introduce custom rendering engines or heavy UI frameworks.
- **Lean Dependencies**: Avoid adding new crates to `Cargo.toml` unless strictly necessary. Every new dependency must include a brief inline justification comment in `Cargo.toml`.
- **No `.unwrap()` in Production Code**: Handle errors explicitly via `Result`, `?`, `Option` combinators, or safe fallbacks outside `#[cfg(test)]`.
- **Platform Isolation**: Keep OS-specific code isolated behind `#[cfg(...)]` boundaries.

## Development Workflow

1. Fork the repository and create a topic branch from `main`.
2. Build and run locally:
   ```bash
   cargo run
   ```
3. Verify formatting, lints, tests, and the UI self-test before pushing:
   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   cargo run -- --selftest
   ```
4. Open a pull request describing what changed, why, and how it was verified.

## License

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Ember is dual-licensed under the [MIT License](LICENSE-MIT) and the [Apache License, Version 2.0](LICENSE-APACHE), without any additional terms or conditions.

