# Versioning Policy

`hashavatar-website` is the deployable demo application for `hashavatar`. Its
Cargo package is private (`publish = false`); version tags describe website and
container releases, not crates.io releases.

## Stable Rendering Contract

Within a major release line, the project aims to keep avatar output stable for the same:

- `tenant`
- `style_version`
- `algorithm`
- `id`
- `kind`
- `background`
- `accessory`
- `color`
- `expression`
- `shape`
- `format`
- `size`

That means an application can cache and embed avatar URLs without expecting silent visual churn during normal minor and patch upgrades.

## When Output May Change

Visual output may change when:

- you intentionally change `style_version`
- you intentionally change `tenant`
- the service adopts a renderer release with documented visual changes
- a narrowly scoped rendering bug fix is required and documented

## Recommended Production Strategy

- treat `tenant` as your product or environment namespace
- treat `style_version` as your avatar rollout version, for example `v2`
- keep `algorithm=sha512` and `format=webp`; the public API rejects other modes
- email-shaped identifiers are accepted, but stable internal ids or one-way
  hashes are preferred when you want less personal data in URL logs

## Regression Protection

The repository tests renderer output, request normalization, and cache-key
components. The release gates also exercise the published renderer through the
local service and Wolfi container.
