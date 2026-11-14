## What

Briefly describe the change and the problem it solves. Link the issue if there is one.

## How

Short tour of the approach, and any alternatives you considered.

## Checklist

- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --all-features` passes (and `--no-default-features` if feature gating is touched)
- [ ] Public API changes are documented in rustdoc; behavior changes come with tests
- [ ] Commits follow Conventional Commits (summary ≤ 50 chars)
- [ ] No unrelated changes in the diff

## Notes for reviewers

Anything you want the reviewer to focus on, known limitations, or follow-ups.
