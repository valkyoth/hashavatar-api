# Contributing

Thanks for helping improve `hashavatar`.

## Development

Use the stable Rust toolchain.

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo audit
cargo deny check licenses
```

## Pull Requests

- Keep changes focused and explain the user-visible behavior.
- Add or update tests when rendering behavior, encoders, parsing, or public API types change.
- Do not add bundled avatar art, stock assets, or generated binary assets without prior discussion.
- Preserve deterministic output unless the change is explicitly a visual-version change.

## Visual Stability

`hashavatar` is deterministic. Changes to shape generation, colors, hashing, randomization, or encoder behavior can affect downstream users. When a change intentionally affects output, document it in the changelog.
