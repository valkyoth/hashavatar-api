# hashavatar-api 0.1.0 Release Notes

Status: released

## Summary

`0.1.0` established the first public hashavatar API service.

These notes are reconstructed from the tag and commit history.

## Added

- Added the initial Rust API service around the `hashavatar` crate.
- Added a web interface for trying avatar generation from a browser.
- Added S3-like object storage support for generated avatar assets.
- Added the first container build setup.

## Fixed

- Corrected the release Dockerfile so it no longer used a development-only
  build layout.

## Verification

```bash
cargo test
cargo build --release
```
