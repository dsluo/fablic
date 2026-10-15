# fablic

A TUI for Microsoft Fabric inspired by [sofka](https://github.com/nklmilojevic/sofka).

```sh
fablic                 # start on the default profile
fablic -p <profile>    # start on a profile (name or az account)
fablic --readonly      # block every write
fablic --fake          # built-in fake data, no sign-in needed
fablic login [profile] # az login, keeping az's active account as it was
```

`--fake` runs against a made-up tenant, so everything can be tried without
Fabric: writes change the fake data, runs you start queue, run and finish,
and `ctrl-p` has one profile of each kind (read-only, service principal,
from the environment, signed out, failing). Nothing is saved; each start is
fresh.

## Why?

As someone used to command line tools like `lazygit`, `sofka`, and `vim`, the web UI
for Microsoft Fabric is painfully, ***painfully*** slow. This project hopes to create
a much of the web UI functionality with the snappiness of a TUI experience.

## Installation

Download the archive for your platform from [GitHub Releases](https://github.com/dsluo/fablic/releases).
Releases provide macOS, Linux, and Windows binaries for Intel/AMD (`x86_64`) and
ARM (`aarch64`). Extract the archive and put `fablic` (`fablic.exe` on Windows)
on your `PATH`. Install the
[Azure CLI](https://learn.microsoft.com/cli/azure/install-azure-cli) for sign-in;
`--fake` works without it.

Each release includes `SHA256SUMS`. Verify a downloaded archive with
`sha256sum --ignore-missing -c SHA256SUMS` on Linux, or
`shasum -a 256 --ignore-missing -c SHA256SUMS` on macOS. Linux binaries are built
on Ubuntu 24.04. macOS binaries are not notarized.

Windows downloads are ZIP archives. In PowerShell, compare
`(Get-FileHash .\fablic-X.Y.Z-TARGET.zip -Algorithm SHA256).Hash` with the entry
for that archive in `SHA256SUMS`. The Azure CLI installer provides `az.cmd`,
which fablic uses on Windows.

To build from source, run `mise install` and `mise run build-release`; the binary
is written to `target/release/fablic` (`target/release/fablic.exe` on Windows).

## Sign-in and profiles

fablic uses your az CLI login. Each profile is pinned to one az account. Tokens are
requested with `--subscription`, so the active az context is never changed and
several identities can be used side by side. Switch with `ctrl-p` or `:ctx <name>`.

To sign in (or sign in again when your tenant's session limit runs out), run
`fablic login <profile>`. It runs `az login` for that account's tenant, then makes
az's previously active account active again, since `az login` always switches to
the new login and other tools may rely on the active one. Service principals
(`az login --service-principal`) can't sign in interactively; for those it prints
the `az` command to run instead, since fablic never sees their secret.

### A service principal from the environment

If `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` and `AZURE_CLIENT_SECRET` (or
`AZURE_CLIENT_CERTIFICATE_PATH`) are set, the variables the Azure SDKs read, fablic adds
a profile `env` for that service principal. It signs itself in with
`az login --service-principal` into its own az config directory
(`~/.local/state/fablic/az/env`), so logins in your usual `~/.azure` can never drop it,
and signs in again on its own if a token request fails (e.g. after the secret was
rotated) or when you press `l` on it. A profile in the config with that client id as its
`account` is used instead of `env`, keeping its name and settings. Note that az takes the
secret as a command-line argument, so it is briefly visible to other processes on this
machine while it signs in. Profiles
made from them are named `sp-` plus the first block of the client id. The profile picker
(`ctrl-p`) shows which profiles are signed in, and `l` there does the same without
leaving fablic.

A profile made from an az account can be renamed in the picker (`e`); the name is
saved under `[names]` in the config (`"<account or client id>" = "name"`), so the
other az accounts still appear on their own.

With no config file, every account in `az account list --all` becomes a profile.
To name them, set colours, or mark one read-only, create
`~/.config/fablic/config.toml` (or `$XDG_CONFIG_HOME/fablic/config.toml`):

```toml
default_profile = "fabric"

[[profiles]]
name = "fabric"
account = "admin123@fabric.example.net"
readonly = false
color = "#89b4fa"
```

## Keys

| Key | Does |
|---|---|
| `:` | command palette: `:ws`, `:items`, `:nb`, `:lakehouses`, `:capacities`, `:ctx`, `:q`, ... |
| `:nb <workspace>` | a kind in one workspace, by name or part of one (`:ws <workspace>` opens it) |
| `1`-`9` / `0` | this view in a recently used workspace / across all workspaces (`:nb` then `0`: notebooks everywhere) |
| `/` | filter: text, `a\|b`, `/regex/`, `~fuzzy`, `!` to invert |
| `enter` / `esc` | drill in (lakehouses open in OneLake; files preview) / clear the filter, then a type filter, `F` or `T`, then go up a level (OneLake folder, item's folder, parent folder, workspace list) |
| `F` | flatten workspace folders: every item below, with its folder path |
| `T` | tree: a workspace's folders and items (not what's inside the items), or inside an item, all of its OneLake files |
| `d` / `y` | describe as YAML / item definition (notebook source, pipeline JSON) |
| `o` | open in the Fabric portal |
| `r` / `ctrl-k` | run the item / cancel the selected run (both ask first; blocked on read-only profiles) |
| `s` | schedules of the item; in OneLake, the schema of a table folder |
| `D` | download a OneLake file to the current directory |
| `e` / `E` | rename an item or folder (in roles: change the role) / edit an item's description |
| `ctrl-d` | delete an item, folder, role assignment or shortcut (y to confirm; items that hold data, like lakehouses and warehouses, need their name typed) |
| `n` | new folder or item (lakehouse, notebook, pipeline, ...) in the folder on screen |
| `m` then `p` | pick up an item or folder, open the destination folder, drop it there |
| `a` | in roles: give a user, group or app a role (email, object id, or exact group/app name) |
| `ctrl-p` | profiles: see who is signed in; `enter` switch, `e` rename (saved to the config), `l` sign in again |
| `ctrl-r` / `ctrl-e` / `?` | refresh / compact header / help |

Every write (runs, cancels, renames, deletes, moves, role changes) is blocked on read-only
profiles and recorded in `~/.local/state/fablic/journal.log`.

## Development

Tools are pinned in `mise.toml`:

```sh
mise install      # rust, cargo-nextest, cargo-insta, actionlint
mise run dev      # run fablic
mise run test     # test suite
mise run lint     # clippy + rustfmt
mise run lint-workflows # GitHub Actions checks
```

`fablic --snapshot 'STEPS'` replays keys headlessly and prints one frame, e.g.
`fablic --fake --snapshot 'j enter F'`. Logs go to `~/.local/state/fablic/fablic.log` (`FABLIC_LOG=debug` for more).

Releases are prepared as pull requests by Release Please. Before 1.0, features
and fixes bump the patch version; breaking changes bump the minor version.
See the [release process](docs/releases.md) for setup and publishing.

## AI Usage Disclosure

AI is heavily used in the development of this project. There's no way I'd find the time to do this outside
of the actually productive things I have to do for day job, my other hobbies and projects, and life that does
not involve touching computers if not for the very fast autocomplete machine.
