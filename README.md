# fablic

A k9s-style terminal UI for Microsoft Fabric.

```sh
fablic                 # start on the default profile
fablic -p <profile>    # start on a profile (name or az account)
fablic --readonly      # block every write
fablic --fake          # built-in fake data, no sign-in needed
```

## Sign-in and profiles

fablic uses your az CLI login. Each profile is pinned to one az account. Tokens are
requested with `--subscription`, so the active az context is never changed and
several identities can be used side by side. Switch with `ctrl-p` or `:ctx <name>`.

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
| `:nb all` | a kind across every workspace |
| `/` | filter: text, `a\|b`, `/regex/`, `~fuzzy`, `!` to invert |
| `enter` / `esc` | drill in / clear filter, then go back |
| `F` | flatten workspace folders: every item below, with its folder path |
| `d` / `y` | describe as YAML / item definition (notebook source, pipeline JSON) |
| `o` | open in the Fabric portal |
| `ctrl-p` | switch profile |
| `ctrl-r` / `ctrl-e` / `?` | refresh / compact header / help |

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
