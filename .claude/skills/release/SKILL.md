---
name: release
description: Semantic versioning contract and the exact release procedure for Ordnung — bump the workspace version, tag, and let CI publish the macOS DMG. Use when cutting a release, bumping the version, or deciding whether a change is a patch/minor/major bump. Also read when pushing feature commits, since every verified change ships as a release.
---

# Ordnung releases (semver + procedure)

The single source of truth for the version is `[workspace.package] version` in
the root `Cargo.toml`. Everything else derives from it or from the git tag:
`tools/build-app.sh` stamps it into `Info.plist`, and the `Release` GitHub
workflow (`.github/workflows/release.yml`) builds the universal DMG and
publishes a GitHub Release whenever a `v*` tag is pushed.

## Semver contract

`MAJOR.MINOR.PATCH`, judged by what a DJ using the app would notice:

- **PATCH** — bug fixes, visual polish, performance work, docs/skills only.
  No new capability. `0.4.0 → 0.4.1`
- **MINOR** — a new user-visible feature or capability (new button, new
  analysis output, new export option). `0.4.0 → 0.5.0`
- **MAJOR** — reserved; only when the user explicitly asks (e.g. `1.0.0`),
  or a breaking change to on-disk data (catalog schema that can't migrate,
  cache invalidation that forces a full re-analysis).

When one release rolls up several commits, the highest-ranking change wins.
If the user names a version explicitly, use exactly that.

## Release procedure

Work must already be verified in the running app and committed (see
CLAUDE.md). Then:

1. Edit `[workspace.package] version` in the root `Cargo.toml` to the new
   `X.Y.Z`.
2. `cargo check -p ordnung-gui` — refreshes `Cargo.lock` with the new
   version and proves the workspace still builds.
3. Commit exactly these two files with the message
   `release: bump workspace version to X.Y.Z` and push.
4. Tag and push the tag:
   `git tag vX.Y.Z && git push origin vX.Y.Z`
5. CI takes over: the `Release` workflow builds the universal DMG
   (`Ordnung-X.Y.Z-macos-universal.dmg`) and publishes the GitHub Release.
   Confirm it started with `gh run list --workflow=release.yml --limit 1`
   and report the outcome; if it fails, read the log with
   `gh run view <id> --log-failed` and fix before re-tagging.

Never re-use or move a tag that has already been pushed; a botched release
gets a new patch version.

## When to release

Every verified, pushed change ships as a release — after the feature commit
lands (per CLAUDE.md's commit rule), immediately follow with the bump + tag
above. A rapid series of follow-up fixes to the same feature may roll into
one release at the end of the series instead of one per commit; use judgment,
but never leave `main` ahead of the latest tag at the end of a work session.
