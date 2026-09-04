# Pull request checklist

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic -W clippy::nursery`
- [ ] `cargo test --locked`
- [ ] Parser/engine changes include valid and truncated/corrupt fixtures when relevant
- [ ] Docs / CLI help / packaging versions updated only when required
- [ ] No secrets, tokens, or signed URLs in the diff

## Summary

<!-- What changed and why (1-3 sentences). -->

## Test plan

<!-- Commands or scenarios you ran. -->
