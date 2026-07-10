# Contributing

Thanks for helping improve `hashavatar-api`.

## Development

Use Rust 1.97 or newer.

```bash
scripts/checks.sh
```

## Pull Requests

- Keep changes focused and explain the user-visible behavior.
- Add or update tests when rendering behavior, encoders, parsing, or public API types change.
- Do not add bundled avatar art, stock assets, or generated binary assets without prior discussion.
- Preserve deterministic output unless the change is explicitly a visual-version change.

## Visual Stability

The API delegates deterministic rendering to `hashavatar`. Changes to request
normalization, namespace handling, cache keys, style options, or encoder
selection can still affect downstream users. Document intentional output or URL
contract changes in the release notes.
