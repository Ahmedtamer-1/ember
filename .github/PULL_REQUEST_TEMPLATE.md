## Summary
Briefly describe the changes in this PR and why they are needed.

## Verification Checklist
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes with zero warnings
- [ ] `cargo test` passes
- [ ] `cargo run -- --selftest` passes (if UI/IPC/tab behavior was touched)
- [ ] No `.unwrap()` added to non-test code
- [ ] Any new dependencies in `Cargo.toml` include a justification comment

