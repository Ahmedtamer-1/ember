# Ember — Workspace Rules (`AGENTS.md`)

**Ember** is an extremely lightweight desktop web browser written in Rust. All contributors and coding agents working in this repository must follow these rules.

## 1. Project Goal
- **User Experience**: Deliver a Chrome-like workflow and feel.
- **Footprint**: Maintain minimal RAM usage and binary size.
- **Privacy & Filtering**: Include a built-in adblocker out of the box.

## 2. Architecture
- **Core Stack**: Rust application shell built on [`tao`](https://crates.io/crates/tao) for window creation/event-loop management and [`wry`](https://crates.io/crates/wry) for the system webview.
- **No Custom Engine**: **Do NOT attempt to write a custom rendering engine** (HTML/CSS/JS parser or layout engine). Rely strictly on `wry` and the underlying OS webview.

## 3. Platform Strategy
- **Target Platform First**: Windows first (with cross-platform support across `[Windows/macOS/Linux]`).
- **Platform Abstraction**: Keep all platform-specific code isolated behind clean traits or `#[cfg(...)]` modules. Avoid scattering OS-specific conditionals throughout core application logic.

## 4. Dependency Policy
- **Minimal Dependencies**: Keep the dependency tree as small and lean as possible to protect compile times, binary size, and memory footprint.
- **Justification Required**: Every new crate added to `Cargo.toml` must be justified either in a short inline comment in `Cargo.toml` or documented in `DEPENDENCIES.md`.

## 5. Release Profile & Binary Size Optimization
Ensure `Cargo.toml` configures the release profile for minimal binary size:

```toml
[profile.release]
opt-level = "z"       # or "s" — optimize for binary size
lto = "fat"           # full link-time optimization across all crates
codegen-units = 1     # maximize size reduction and optimization opportunities
panic = "abort"       # remove unwinding overhead and panic strings
strip = true          # strip symbols from the resulting binary
```

## 6. Code Quality & Safety Standards
- **Formatting & Lints**: All code must pass `cargo fmt --check` and `cargo clippy` without warnings before a step is considered complete.
- **No `unwrap()` in Production**: Do **not** use `.unwrap()` in non-test code. Handle errors explicitly using `Result`, `?`, `Option` combinators, or graceful fallbacks.

## 7. Agent Workflow
- **Before each major step**: Produce a concise plan outlining the intended changes and architectural impact.
- **After each step**: Run the build (`cargo build` / `cargo clippy` / `cargo fmt`) and provide a brief summary of what changed.

