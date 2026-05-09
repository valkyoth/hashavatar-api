# Versioning Policy

`hashavatar` is intended to be safe for deterministic avatar URLs.

## Stable Rendering Contract

Within a major release line, the project aims to keep avatar output stable for the same:

- `tenant`
- `style_version`
- `id`
- `kind`
- `background`
- `format`
- `size`

That means an application can cache and embed avatar URLs without expecting silent visual churn during normal minor and patch upgrades.

## When Output May Change

Visual output may change when:

- you intentionally change `style_version`
- you intentionally change `tenant`
- you adopt a new major crate release with documented breaking visual changes
- a narrowly scoped rendering bug fix is required and documented

## Recommended Production Strategy

- treat `tenant` as your product or environment namespace
- treat `style_version` as your avatar rollout version, for example `v2`
- do not send raw user emails if you can avoid it
- prefer stable internal ids or a one-way hash as the public avatar id

## Regression Protection

The repository includes golden fingerprint regression tests. Those tests are meant to catch unintended visual changes before release.
