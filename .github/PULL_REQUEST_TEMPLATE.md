## Summary

<!-- Brief description of what this PR does -->

## Type of Change

- [ ] Bug fix
- [ ] New feature
- [ ] Enhancement to existing feature
- [ ] Refactoring (no functional change)
- [ ] Documentation
- [ ] CI/CD

## Changes Made

-

## Checklist

### Code Quality
- [ ] All terminal output goes through `output::Printer` (no `println!`, `eprintln!`)
- [ ] No `unwrap()` or `expect()` in library code
- [ ] Providers implement their traits; reconciler uses `ProviderRegistry`
- [ ] `thiserror` for library errors, `anyhow` only in `main.rs`/`cli/`
- [ ] Import grouping: std, external, internal (blank line separated)

### Testing
- [ ] `cargo test` passes
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] `bash .claude/scripts/audit.sh` passes
- [ ] New code has unit tests

### Documentation
- [ ] Help text updated (if adding/changing commands)
- [ ] User-facing changes use a conventional-commit subject (feat/fix/…) so the per-crate CHANGELOG.md captures them at release

## Testing Done

<!-- How did you test this? -->

## Related Issues

<!-- Link to related issues: Fixes #123, Relates to #456 -->
