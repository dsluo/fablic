# fablic

A k9s-style terminal UI for Microsoft Fabric.

```sh
fablic                 # start on the default profile
fablic -p <profile>    # start on a profile (name or az account)
fablic --readonly      # block every write
fablic --fake          # built-in fake data, no sign-in needed
fablic login <profile> # sign a device profile in (see below)
fablic logout <profile>
```

## Sign-in and profiles

fablic uses your az CLI login. Each profile is pinned to one az account. Tokens are
requested with `--subscription`, so the active az context is never changed and
several identities can be used side by side. Switch with `ctrl-p` or `:ctx <name>`.

Unless the config file names az profiles itself, every account in
`az account list --all` becomes a profile. To name them, set colours, or mark one
read-only, create `~/.config/fablic/config.toml` (or `$XDG_CONFIG_HOME/fablic/config.toml`):

```toml
default_profile = "fabric"

[[profiles]]
name = "fabric"
account = "admin123@fabric.example.net"
readonly = false
color = "#89b4fa"
```

### Signing in without az

For an identity you don't want in az, give the profile `login = "device"` and run
`fablic login <profile>`. It shows a code to enter at microsoft.com/devicelogin,
then keeps the sign-in (a refresh token and the account name) in the OS keychain
under the service `fablic`. `fablic logout <profile>` removes it.

```toml
[[profiles]]
name = "partner"
login = "device"
tenant = "partner.onmicrosoft.com"   # optional; any work account by default
client_id = "…"                      # optional; see below
```

By default this signs in as the Azure CLI's public client, as Microsoft's Azure
Identity libraries do; it is pre-consented for Fabric, Storage and Graph in most
tenants. If your tenant blocks it, register your own app with "Allow public client
flows" and set `client_id`. On Linux the kernel keyring is used, which doesn't
survive a reboot.

## Keys

| Key | Does |
|---|---|
| `:` | command palette: `:ws`, `:items`, `:nb`, `:lakehouses`, `:capacities`, `:ctx`, `:q`, ... |
| `:nb all` | a kind across every workspace |
| `/` | filter: text, `a\|b`, `/regex/`, `~fuzzy`, `!` to invert |
| `enter` / `esc` | drill in (lakehouses open in OneLake; files preview) / clear filter, then go back |
| `F` | flatten workspace folders: every item below, with its folder path |
| `d` / `y` | describe as YAML / item definition (notebook source, pipeline JSON) |
| `o` | open in the Fabric portal |
| `r` / `ctrl-k` | run the item / cancel the selected run (both ask first; blocked on read-only profiles) |
| `s` | schedules of the item; in OneLake, the schema of a table folder |
| `D` | download a OneLake file to the current directory |
| `e` / `E` | rename an item or folder (in roles: change the role) / edit an item's description |
| `ctrl-d` | delete an item or folder (type its name to confirm), a role assignment or a shortcut |
| `n` | new folder or item (lakehouse, notebook, pipeline, ...) in the folder on screen |
| `m` then `p` | pick up an item or folder, open the destination folder, drop it there |
| `a` | in roles: give a user, group or app a role (email, object id, or exact group/app name) |
| `ctrl-p` | switch profile |
| `ctrl-r` / `ctrl-e` / `?` | refresh / compact header / help |

Every write (runs, cancels, renames, deletes, moves, role changes) is blocked on read-only
profiles and recorded in `~/.local/state/fablic/journal.log`.

## Development

Tools are pinned in `mise.toml`:

```sh
mise install      # rust, cargo-nextest, cargo-insta
mise run dev      # run fablic
mise run test     # test suite
mise run lint     # clippy + rustfmt
```

`fablic --snapshot 'STEPS'` replays keys headlessly and prints one frame, e.g.
`fablic --fake --snapshot 'j enter F'`. Logs go to `~/.local/state/fablic/fablic.log` (`FABLIC_LOG=debug` for more).
