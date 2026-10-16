# Release process

Release Please runs on pushes to `main`, or through the `Release Please`
workflow's manual run button. It collects Conventional Commits into a release
PR with changes to `CHANGELOG.md`, `Cargo.toml`, `Cargo.lock`, and
`.release-please-manifest.json`. Keep PR titles in Conventional Commit format
when squash-merging so the merged commit describes the change.

The empty manifest bootstraps the first release at the current Cargo version,
`0.1.0`. After that, before 1.0, features and fixes increment the patch version
(for example, `0.1.0` to `0.1.1`); breaking changes marked with `!` or a
`BREAKING CHANGE:` footer increment the minor version (`0.1.1` to `0.2.0`).
Starting at 1.0, features increment minor and breaking changes increment major.
Tags and release names use `X.Y.Z` without a `v` prefix. The bot maintains the
manifest after bootstrap; do not bump release versions separately by hand.

The CI workflow runs `mise run --skip-tools test`, `mise run --skip-tools lint`,
and `mise run --skip-tools lint-workflows` on Linux, macOS, and Windows for every
PR, including release PRs. It also runs on pushes to `main` and merge queue
events. Tests and builds use the committed Cargo lockfile. The login restoration
and environment sign-in tests that use Unix shell stubs run on Linux and macOS;
Windows uses a batch stub for account lookup, token cache, and ambiguous-account
tests.

Each job installs its required tools explicitly, then runs tasks with
`mise run --skip-tools`. This keeps task execution from installing unrelated
development tools such as `cargo-insta`, whose prebuilt binaries do not cover
every release target.

CI also builds and smoke-tests the targets in the matrix below. Tier 1
targets build on every PR, push to `main`, and merge queue event. Tier 2
targets build only on release PRs (branches starting `release-please--`),
pushes to `main`, manual runs, and releases, so a tier 2 break on an ordinary
PR shows up after merge, before the release PR can pass. The matrix and build steps live in
`.github/workflows/build.yaml`, which both CI and releases call. CI builds use
the commit under test; release builds use the validated tag and additionally
package and upload archives. Release runs perform these builds after their
test and lint checks pass.

Merge the release PR when its version and changelog are ready and its checks
pass. Release Please then creates the tag and publishes the GitHub Release.
The `Release` workflow checks the tag against `Cargo.toml` and runs tests and
lint against the tagged code before calling the shared build workflow. Each
native build runs `fablic --fake --snapshot 'j enter F'` without sign-in or tenant data.

The build matrix provides:

| Platform | Target | Build runner | Tier |
| --- | --- | --- | --- |
| Linux Intel/AMD | `x86_64-unknown-linux-gnu` | Ubuntu 24.04 | 1 |
| Linux ARM | `aarch64-unknown-linux-gnu` | Ubuntu 24.04 ARM | 2 |
| macOS Intel | `x86_64-apple-darwin` | macOS 15 Intel | 2 |
| macOS Apple Silicon | `aarch64-apple-darwin` | macOS 15 | 1 |
| Windows Intel/AMD | `x86_64-pc-windows-msvc` | Windows Server 2025 | 1 |
| Windows ARM | `aarch64-pc-windows-msvc` | Windows 11 ARM | 2 |

After every build succeeds, the workflow uploads all six archives and
`SHA256SUMS` to the GitHub Release. Linux and macOS use
`fablic-X.Y.Z-TARGET.tar.gz`; Windows uses `fablic-X.Y.Z-TARGET.zip`.
Archives contain the binary (`fablic.exe` on Windows), README, and changelog.
The release is visible while checks and builds run; downloads are ready when
the `Release` workflow succeeds. Transient failures can be retried by rerunning the workflow; uploads
replace assets with the same names. Fixes to tagged source or workflow code
need a new release.

## Repository setup

Install a GitHub App on `dsluo/fablic` with repository **Contents**, **Issues**,
and **Pull requests** permissions set to read/write. Add its client ID as the
Actions secret `BOT_CLIENT_ID` and its private key as `BOT_APP_PRIVATE_KEY`.
The bot token allows CI to run on generated release PRs and publishing to run
on generated releases; the default `GITHUB_TOKEN` suppresses those events.
See [Release Please's token requirements](https://github.com/googleapis/release-please-action#github-credentials).

Protect `main` with the CI checks `Check (ubuntu-24.04)`, `Check (macos-15)`, and
`Check (windows-2025)` required before merging. Also require the three tier 1
build checks from the target matrix above. Leave the tier 2 build checks
optional: they do not run on most PRs, and a required check that never runs
blocks the merge. Release builds still need all six to succeed before
uploading. The release bot prepares PRs;
merging remains the release decision. Binary uploads use the workflow's built-in `GITHUB_TOKEN`
with Contents write permission, and need no distribution service secrets.
