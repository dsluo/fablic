# Architecture

fablic is a single Rust binary: a k9s-style terminal UI for Microsoft Fabric,
built on [ratatui](https://ratatui.rs) and [tokio](https://tokio.rs). This
document describes how it is put together: the layers, how a key press turns
into an API call and back into a frame, and the conventions that keep the
pieces testable.

## Contents

- [Overview](#overview)
- [Source layout](#source-layout)
- [Startup](#startup)
- [The event loop](#the-event-loop)
- [App state: `App`, `Effect` and `Msg`](#app-state-app-effect-and-msg)
- [Data model: `Kind`, `Query`, `Scope` and `Row`](#data-model-kind-query-scope-and-row)
- [Navigation](#navigation)
- [The `DataSource` boundary](#the-datasource-boundary)
- [The Fabric source](#the-fabric-source)
- [The fake source](#the-fake-source)
- [Authentication and profiles](#authentication-and-profiles)
- [Writes](#writes)
- [Rendering](#rendering)
- [Configuration and on-disk state](#configuration-and-on-disk-state)
- [Testing](#testing)
- [Build, CI and releases](#build-ci-and-releases)
- [Design principles](#design-principles)

## Overview

```
                        ┌──────────────────────────────────────────────┐
  keys (crossterm) ───▶ │  App  (src/app)                              │
  ticks (1 s)      ───▶ │  pure state machine: on_key / on_msg / on_tick│
  Msg from tasks   ───▶ │  queues Effects, never does I/O              │
                        └──────────────┬───────────────────────────────┘
                                       │ take_effects()
                                       ▼
                        ┌──────────────────────────────────────────────┐
                        │  effects::perform  (src/effects.rs)          │
                        │  one tokio task per Effect, answers with Msg │
                        └──────────────┬───────────────────────────────┘
                                       │ SharedSource (one per profile)
                     ┌─────────────────┴──────────────────┐
                     ▼                                    ▼
        ┌─────────────────────────┐          ┌─────────────────────────┐
        │ FabricSource            │          │ FakeSource              │
        │ (src/fabric)            │          │ (src/fabric/fake)       │
        │ Client · OneLake · Graph│          │ in-memory tenant        │
        └───────────┬─────────────┘          └─────────────────────────┘
                    │ TokenProvider
                    ▼
        ┌─────────────────────────┐
        │ AzCli (src/auth)        │
        │ `az account             │
        │   get-access-token`     │
        └─────────────────────────┘

  ui::draw (src/ui) reads &mut App after every turn of the loop and renders a frame.
```

The design follows the Elm architecture. `App` holds all UI state and is
updated only by three entry points (`on_key`, `on_msg`, `on_tick`). Anything
that needs I/O is expressed as an `Effect` value pushed onto a queue. The
event loop drains that queue, runs each effect on a tokio task, and feeds the
result back as a `Msg`. Rendering is a function of `App`.

That split gives fablic three properties it depends on:

1. **The UI never blocks.** Every network call and every `az` process runs
   off the render loop.
2. **Everything is testable without a terminal or network.** Tests drive
   `App` with key events and run effects synchronously against
   `FakeSource`.
3. **Live, fake and snapshot modes share one code path.** `effects::perform`
   is the only place effects are carried out, whether by the real loop,
   `--snapshot`, or a test.

## Source layout

| Path | Role |
|---|---|
| `src/main.rs` | CLI (`clap`), profile assembly, the event loop, terminal suspend/resume for `az login`, headless `--snapshot` mode |
| `src/app/mod.rs` | `App` state, `Mode`, `Effect`, `Msg`, key dispatch, row cache, filter application |
| `src/app/navigation.rs` | Drilling into rows, `:kind` jumps, the back stack, scope inference |
| `src/app/workspaces.rs` | The active workspace and recent ones (k9s-style namespaces), number-key switching, workspace names on the command line |
| `src/app/command.rs` | The `:` command palette: candidates, fuzzy suggestions, command execution |
| `src/app/actions.rs` | Per-row read actions: item definitions, open in portal, flatten folders |
| `src/app/jobs.rs` | Run and cancel jobs, plus the "watch" window after an action |
| `src/app/writes.rs` | All other writes: rename, describe, delete, folders, moves, roles, shortcuts; typed prompts |
| `src/app/onelake.rs` | OneLake file preview, table schema, download |
| `src/app/profiles.rs` | Profile picker, switching, sign-in checks, renaming |
| `src/effects.rs` | `perform(effect) -> Option<Msg>`: the only place effects run; `Ctx` gates side effects |
| `src/source.rs` | The `DataSource` trait and the types that cross it: `Query`, `Scope`, `Action`, `Part` |
| `src/resources/mod.rs` | `Kind` (resource kinds), columns, aliases, scope needs, item type tables, `Row`, `Health` |
| `src/fabric/mod.rs` | `FabricSource`: maps queries and actions onto Fabric REST calls; per-workspace caches |
| `src/fabric/client.rs` | Fabric REST client: auth, paging, 429/503 retry, long-running operations |
| `src/fabric/onelake.rs` | OneLake DFS (files) and table metadata API |
| `src/fabric/graph.rs` | Microsoft Graph lookups to resolve role principals |
| `src/fabric/rows.rs` | Pure functions turning API JSON into `Row`s, plus formatting helpers |
| `src/fabric/fake/` | `FakeSource` and its fixture data |
| `src/auth/` | `TokenProvider` trait, `AzCli` provider, environment service principals, `fablic login` |
| `src/config.rs` | `config.toml` loading, profile defaults from az, format-preserving renames, XDG paths |
| `src/filter.rs` | `/` filter language: substring, `a\|b`, `/regex/`, `~fuzzy`, `!` invert |
| `src/journal.rs` | Append-only log of every write |
| `src/theme.rs` | Catppuccin Mocha / Latte palettes and terminal background detection |
| `src/ui/` | Rendering: layout, header, table, detail view, overlays, insta snapshots |

## Startup

`main` (`src/main.rs`) does the following, in order:

1. Parses `Args` and initialises file logging (`tracing` to
   `$XDG_STATE_HOME/fablic/fablic.log`, level from `FABLIC_LOG`, default
   `fablic=info`).
2. If the subcommand is `login`, runs `auth::login` and exits without
   starting the TUI.
3. Builds the profile list and one `SharedSource` per profile:
   - **Live** (`live_profiles`): loads `config.toml`, asks az for its cached
     accounts (`az account list --all`), and, when `AZURE_CLIENT_ID` and
     friends are set, adds an environment service principal profile. Each
     profile gets an `AzCli` token provider wrapped in a `FabricSource`.
   - **Fake** (`fake_profiles`): six profiles over independent `FakeSource`
     instances, one of each kind the picker can show (normal, read-only,
     service principal, from environment, expired, broken).
4. Constructs `App::new(profiles, active)`. The constructor immediately
   queues a `Fetch` for the workspaces list and a `Check` for every profile,
   so tokens are fetched up front and later profile switches are instant.
5. With `--snapshot`, replays the given steps headlessly and prints one
   frame (see [Testing](#testing)).
6. Otherwise detects the terminal theme (before raw mode, since detection
   queries the terminal), enters the alternate screen via `ratatui::init`,
   and runs the event loop.

Sources are stored in a `HashMap<String, SharedSource>` keyed by the
profile's **account**, not its display name. Names can change at runtime
(profiles can be renamed from the picker), accounts cannot.

## The event loop

`run` in `src/main.rs` is a single loop:

```
loop {
    for effect in app.take_effects() {
        Login  ─▶ handled inline (needs the terminal), see below
        other  ─▶ tokio::spawn(effects::perform(effect)) ─▶ tx.send(msg)
    }
    terminal.draw(ui::draw(app))
    if app.quit { return }
    select! {
        key event  ─▶ app.on_key
        msg on rx  ─▶ app.on_msg
        1 s tick   ─▶ app.on_tick
    }
}
```

Messages flow back through an unbounded `mpsc` channel. Each turn handles
exactly one input, then drains any effects it produced, then redraws.

`Effect::Login` is the exception. Interactive `az login` needs the real
terminal, so the loop:

1. drops the crossterm `EventStream` (otherwise it would swallow keystrokes
   meant for az),
2. leaves raw mode and the alternate screen (`suspend`),
3. runs `auth::login` in the foreground,
4. re-enters raw mode (`resume`) and paints a blank frame instead of calling
   `terminal.clear()`, since querying the cursor position right after az
   could hang,
5. recreates the event stream, calls `forget()` on the source so cached
   tokens are dropped, and delivers `Msg::LoggedIn` directly.

Sources that can sign themselves in (the fake source) return a future from
`DataSource::sign_in`, and the loop runs that on a task instead, without
touching the terminal.

## App state: `App`, `Effect` and `Msg`

`App` (`src/app/mod.rs`) is split across several `impl App` blocks in the
`src/app/` submodules, grouped by feature. The core fields:

| Field | Meaning |
|---|---|
| `mode: Mode` | Which input handler and overlay is active: `Table`, `Command`, `Filter`, `Detail`, `Help`, `Profiles`, `Confirm`, `Prompt` |
| `frame: Frame` | The current view: its `Query`, filter text and cursor position |
| `stack: Vec<Frame>` | Views to return to with `esc` |
| `rows`, `view` | Rows of the current query, and indices of those that pass the filter |
| `cache` | `(account, Query) -> Vec<Row>`: last result for every view visited, per profile |
| `generation` | Incremented on every fetch; responses carrying an older generation are dropped |
| `profile`, `profiles`, `active` | Profile list and a copy of the active one |
| `pending`, `prompt`, `moving` | State for confirmations, typed prompts and the move clipboard |
| `effects` | The outbound queue, drained by `take_effects()` |

### Effects

| Effect | Carried out by | Answers with |
|---|---|---|
| `Fetch { profile, generation, query, force }` | `DataSource::fetch` | `Msg::Rows` |
| `Detail { profile, title, request }` | `definition`, `preview` or `schema` | `Msg::Detail` |
| `Download { .. }` | `DataSource::download` into `Ctx::download_dir` | `Msg::Status` |
| `Act { profile, action }` | `DataSource::act`, then journal | `Msg::Acted` |
| `Check { profile, fresh }` | `DataSource::warm` (optionally after `forget`) | `Msg::Auth` |
| `Login { profile }` | event loop (az) or `DataSource::sign_in` | `Msg::LoggedIn` |
| `RenameProfile { .. }` | `config::rename` | `Msg::Status` |
| `Open(url)` | `open::that_detached` | nothing |

### Stale responses and caching

Navigating is never blocked by the network. `App::load` shows the cached rows
for the new view immediately (if any) and queues a fresh fetch. Every fetch
increments `generation`; `on_msg` drops `Msg::Rows` whose generation does not
match, so a slow response for a view the user already left cannot overwrite
the current one. `Msg::Detail` is matched the same way by title and mode.

`apply_filter` recomputes `view` and keeps the cursor on the same row **id**
when it is still visible, so refreshes and filter edits do not make the
selection jump.

### Ticks and live views

`on_tick` runs every second. A jobs view refreshes itself every 5 s
(`LIVE_REFRESH`) while any run is pending, or for 60 s after the user starts
or cancels a run (`live_until`, set from `jobs.rs`), so a freshly queued run
shows up without pressing `ctrl-r`.

## Data model: `Kind`, `Query`, `Scope` and `Row`

`Kind` (`src/resources/mod.rs`) enumerates what can be listed:

| Kind | Needs in scope | Source |
|---|---|---|
| `Workspaces` | nothing | `GET /workspaces` + capacity names |
| `Items` | optional workspace / folder | `GET /workspaces/{id}/items` and `/folders` (recursive) |
| `Jobs` | workspace or item | job instances, plus Livy sessions for cancellations |
| `Schedules` | item | `GET .../jobs/{jobType}/schedules` |
| `OneLake` | item | OneLake DFS directory listing |
| `Shortcuts` | item | `GET .../items/{id}/shortcuts` |
| `Roles` | workspace | `GET .../roleAssignments` |
| `Capacities`, `Connections`, `Gateways`, `Domains` | nothing | tenant-level list endpoints |

Each kind also declares its table columns, palette aliases (`ws`, `nb`,
`ol`, ...) and its `ScopeNeed`. Item type aliases (`nb` for `Notebook`,
`lakehouses`, ...) resolve to `Kind::Items` with an item type filter.
Further tables describe which item types are creatable, which have a job type
(`job_type_for`), and which have OneLake storage (`has_onelake`).

A `Query` is a `Kind` plus a `Scope` (workspace, folder, item, OneLake path),
an optional item type and a `flat` flag. `Query` is `Hash + Eq`, which is
what lets it serve as the cache key and as the "same view?" test in
`push_query`.

A `Row` is what every table shows: an `id`, a `name`, display `cells`, a
coarse `Health` used for colouring, a `dir` flag for containers that `enter`
opens, and the `raw` JSON the row was built from. `d` (describe) renders
`raw` as YAML, so every view gets a detail view for free.

## Navigation

`src/app/navigation.rs` implements three moves:

- **`drill` (`enter`)** maps the selected row to the next query: workspace to
  its items at the root folder; folder to the next tree level; an item with
  OneLake storage to its OneLake root; a runnable item to its jobs; a OneLake
  directory to the next level. Anything else falls back to a file preview
  (in OneLake) or describe.
- **`goto` (`:kind`)** jumps to another kind, keeping as much scope as still
  applies. `context_scope` narrows the current scope using the highlighted
  row, so `:jobs` on a highlighted notebook opens that notebook's runs.
  Kinds that need scope the user has not provided are refused with a hint
  rather than an error from the API. With no workspace in the view or the
  highlighted row, the active one applies (see below); `0` lists across
  every workspace.
- **`back` (`esc`)** clears an active filter first, then pops the stack.

`push_query` refuses to stack a duplicate of the current view; repeating
`:ws` on the workspaces list just forces a refresh.

### The active workspace

Workspaces work like k9s namespaces (`src/app/workspaces.rs`). Each profile
has an active workspace and a list of up to nine recent ones, kept in
`App::spaces` by account. `load` calls `follow_workspace` on every view
change: a view inside a workspace makes it active and adds it to the recent
list (new ones go first and move the rest down; ones already listed don't
move, and keep the name the workspace list last gave them), a list
across workspaces makes "all" active, and tenant-wide views leave it alone.

- `goto` falls back to the active workspace when neither the view nor the
  highlighted row gives one, so `:capacities` then `:roles` stays put.
- `1`..`9` (`switch_workspace`) show the same kind in a recent workspace when
  that kind can follow (items, roles, workspace jobs), and that workspace's
  items otherwise; `0` lists across all workspaces, or on a view that is
  already across all (workspaces, capacities, ...) stays and only makes
  "all" active. Each switch is pushed, so `esc` walks back through them; a
  number for the view already on screen does nothing.
- `:kind <name>` resolves the name against the cached workspace list and the
  recent ones (id, exact name, then a unique substring) and calls `goto_in`.
- The header lists the recent workspaces as `<0> all`, `<1> ...` ahead of the
  key hints, in one column unless the key hints still fit beside more.
  Fetching the workspace list picks up renames.

## The `DataSource` boundary

`src/source.rs` defines the only interface the app has to the outside world:

```rust
pub trait DataSource: Send + Sync {
    fn fetch(&self, q: Query, force: bool) -> BoxFuture<'static, Result<Vec<Row>>>;
    fn definition(&self, workspace: String, item: String) -> BoxFuture<'static, Result<Vec<Part>>>;
    fn preview(&self, workspace, item, path, total: Option<u64>) -> BoxFuture<'static, Result<Part>>;
    fn schema(&self, workspace, item, path) -> BoxFuture<'static, Result<Part>>;
    fn download(&self, workspace, item, path, dest: PathBuf) -> BoxFuture<'static, Result<u64>>;
    fn act(&self, action: Action) -> BoxFuture<'static, Result<String>>;
    fn forget(&self) {}
    fn warm(&self) -> BoxFuture<'static, Result<()>> { .. }
    fn sign_in(&self) -> Option<BoxFuture<'static, Result<()>>> { None }
}
```

Methods take owned arguments and return `'static` boxed futures so effects
can be moved onto tokio tasks. Two implementations exist: `FabricSource` and
`FakeSource`. Both turn API-shaped JSON into rows through the same pure
builders in `src/fabric/rows.rs`, so the fake tenant exercises the real
formatting code.

## The Fabric source

`FabricSource` (`src/fabric/mod.rs`) wraps an `Arc<Inner>` holding three HTTP
clients and two caches. All three clients share the profile's
`TokenProvider` and ask it for a token of the right audience.

### `Client`: Fabric REST (`src/fabric/client.rs`)

- Base URL `https://api.fabric.microsoft.com/v1`, 60 s request timeout,
  `fablic/<version>` user agent.
- `send` attaches a fresh bearer token each attempt and retries `429` and
  `503` up to 4 times, honouring `Retry-After` (capped at 30 s) or
  backing off exponentially.
- `list` / `list_pages` follow `continuationUri` paging; `list_pages`
  stops early for newest-first endpoints with long histories.
- `call` for plain PATCH / DELETE / POST; `post_accepted` for 202 endpoints
  that return a `Location` (job run, cancel); `post_lro` for long-running
  operations, polling the operation URL until `Succeeded` (then fetching
  `/result`) or `Failed`, with a 2 minute ceiling.
- Errors are rewritten into messages a user can act on: a 401 names the
  account and suggests `fablic login <account>`; a 403 names the account and
  the path it was denied.

### `OneLake` (`src/fabric/onelake.rs`)

OneLake is reached through two separate APIs that take **storage-audience**
tokens (`https://storage.azure.com`), not Fabric ones:

- **DFS** (`onelake.dfs.fabric.microsoft.com`, ADLS Gen2 compatible):
  directory listing with continuation from either the `x-ms-continuation`
  header or the body, ranged reads for previews (first 64 KiB), and
  streamed downloads.
- **Table metadata** (`onelake.table.fabric.microsoft.com/delta`, Unity
  Catalog compatible): column schemas for `s` on a table folder.

### `Graph` (`src/fabric/graph.rs`)

Used only when adding a workspace role. `resolve` turns whatever the user
typed into a principal id and type: an object id via `directoryObjects`, an
email via `users/{upn}` with a fallback search on `mail` (guests' UPNs differ
from their email), or an exact group / service principal display name.
Ambiguous or missing matches are reported instead of guessed.

### Caching and fan-out

| Cache / limit | Value | Why |
|---|---|---|
| `WS_TTL` | 60 s | One recursive folder + item listing per workspace is reused while walking its folder tree, flattening, and listing jobs |
| `CAP_TTL` | 10 min | Capacity names change rarely, and listing them can take seconds |
| `CAP_WAIT` | 1.5 s | The workspace list waits at most this long for capacity names; a slow listing keeps running in the background and fills the cache for next time |
| `FAN_OUT` | 6 | Parallel workspace listings for items across all workspaces |
| `JOB_FAN_OUT` | 4 | Parallel per-item run listings for a workspace jobs view |
| `SESSION_PAGES` | 3 | Pages of Livy sessions read to detect cancelled Spark runs |

`force` (from `ctrl-r` and live refresh) bypasses these caches. Every write
other than run / cancel evicts the target workspace's cached listing, even
if the write failed partway.

### Notable query mappings

- **Items** fetches folders and items once per workspace (recursive) and
  slices them locally: `items_level` for one folder, `items_flat` for a
  subtree (`F`, or when filtering by type), `items_all` across workspaces.
- **Workspace jobs** has no single Fabric endpoint, so fablic lists runs of
  every runnable item in parallel and merges them. Items that fail (for
  example, no permission) are skipped and logged unless all of them fail.
- **Cancelled Spark runs** are reported by Fabric as failed. Livy sessions
  are the only record of a user cancellation, so `rows::mark_cancelled`
  cross-references them. A failure to list sessions is logged and the runs
  are shown as Fabric reports them.
- **Definitions** (`y`) call `getDefinition` as a long-running operation and
  base64-decode each part; binary parts are shown as a byte count.

## The fake source

`FakeSource` (`src/fabric/fake/`) is a complete, in-memory tenant used by
tests, `--snapshot` and `--fake`. Fixture JSON lives in `data.rs` and uses
only made-up values (`contoso.example`, zeroed GUIDs, generic names).

- **Writes mutate state.** Renames, deletes, moves, new folders and items,
  and role changes all apply to the in-memory data behind a `Mutex`.
- **Runs progress.** With `.live()`, the clock follows real time, so a run
  started with `r` is queued for 3 s, runs for 20 s, then completes. Tests
  leave the clock frozen at a fixed instant for deterministic output.
- **Auth is scriptable.** `FakeAuth::{SignedIn, Expired, Broken(msg)}` lets
  the profile picker show every state; `sign_in` fixes `Expired` without a
  terminal.
- **Admin-only endpoints** (domains) are gated by `.admin()`.

## Authentication and profiles

### Token providers

`TokenProvider` (`src/auth/mod.rs`) returns a bearer token for one of three
audiences (`FABRIC`, `STORAGE`, `GRAPH`) and names the account it signs in
as. The only real implementation is `AzCli` (`src/auth/az.rs`), which shells
out to the Azure CLI; fablic has no OAuth code of its own.

az chooses an identity from the *active* subscription context, which would
make two profiles in one tenant indistinguishable and would mean switching
profiles changes other tools' active account. `AzCli` avoids both:

1. On first use it runs `az account list --all` and resolves the
   **subscription id** whose user is this profile's account. It refuses ids
   that another account shares (tenant-level entries reuse the tenant id)
   rather than risk returning the wrong identity's token.
2. It then calls `az account get-access-token --resource <aud>
   --subscription <id>`, which selects that identity for one call without
   touching the active context.
3. Tokens are cached per audience and reused until 5 minutes before expiry.
   The state mutex is held across the az call, so concurrent requests share
   one az process instead of spawning many.
4. `forget()` sets a flag; the next `token` call drops all cached state.

### Environment service principals

When `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` and either `AZURE_CLIENT_SECRET` or
`AZURE_CLIENT_CERTIFICATE_PATH` are set, `EnvLogin::from_env` produces a
profile (named `env`, or reusing a config profile whose `account` is that
client id). Its `AzCli` uses a private `AZURE_CONFIG_DIR`
(`$XDG_STATE_HOME/fablic/az/env`) so logins in `~/.azure` cannot evict it. It
signs itself in with `az login --service-principal` when az has no login for
it, and signs in once more if a token request fails (say, after the secret
was rotated). The `Debug` impl redacts the secret.

### `fablic login`

`auth::login` runs an interactive `az login` for the tenant az last saw the
account in, then restores whichever account was active before, because `az
login` always switches the active context and other tools may rely on it.
Service principals cannot sign in interactively; for those fablic prints the
`az login --service-principal` command to run.

### Profile state in the app

`ProfileInfo.auth` tracks `Unknown`, `Checking`, `SignedIn`, `Expired` or
`Failed(msg)`. `Auth::from_error` classifies failed token requests: known
AADSTS codes and messages that mean "sign in again" become `Expired` (shown
with a hint to press `l`), anything else is `Failed`. Each profile has its
own source and its own entries in the row cache, so switching back and forth
(`ctrl-p`, `:ctx`) shows cached rows immediately while a refresh runs.

## Writes

Every mutation is an `Action` (`src/source.rs`): run / cancel job, rename,
describe, delete item, new / rename / delete folder, new item, move, add /
set / remove role, delete shortcut. Each passes through the same pipeline:

```
key ─▶ read-only guard ─▶ confirmation ─▶ Effect::Act ─▶ DataSource::act ─▶ journal ─▶ Msg::Acted
         (writable)        y/N or typed      (task)        (Fabric/fake)     (append)    (status + refresh)
```

1. **Read-only guard.** `App::writable` refuses the write on a read-only
   profile (from config, or `--readonly` for all profiles) and says why.
   This check runs before any prompt opens.
2. **Confirmation.** Runs, cancels, moves, role removals and shortcut
   deletes use a y/N confirm (`Mode::Confirm`); anything but `y` cancels.
   Renames, descriptions, role changes, new items and folders use a typed
   prompt (`Mode::Prompt`) where submitting is the confirmation. Deletes of
   items and folders require typing the exact name.
3. **Execution.** `Effect::Act` carries the action to `effects::perform`,
   which calls `DataSource::act`. Actions always carry workspace **ids**, not
   names.
4. **Journal.** Success or failure, `journal::append` writes one
   tab-separated line (timestamp, profile, description, target ids, outcome)
   to `$XDG_STATE_HOME/fablic/journal.log`. Journal failures are logged,
   never fatal.
5. **Result.** `Msg::Acted` flashes the outcome. On success it also clears
   the whole row cache (other views may show what changed), forces a
   refresh, and, in a jobs view, keeps polling for 60 s so a new run
   appears on its own.

`Action::describe` and `Action::target` give the human and machine
descriptions used by the confirm prompt and journal respectively.

Moves are two-step: `m` stores a `Moving` (item or folder) on the app; the
user navigates to the destination folder and presses `p`. Moving a folder
into itself or a descendant is refused locally.

## Rendering

`ui::draw` (`src/ui/mod.rs`) splits the screen into four rows:

```
┌──────────────────────────────────────────────────────────┐
│ header (7 lines, or 1 when compact): profile, scope,     │  ui/header.rs
│ key hints for the current kind, logo                     │
├──────────────────────────────────────────────────────────┤
│ body: table (ui/table.rs) or detail view (ui/detail.rs)  │
│        overlays drawn on top: palette, help, profiles,   │
│        confirm, prompt                                   │
├──────────────────────────────────────────────────────────┤
│ prompt line: `:` / `/` input, or breadcrumb              │
│ status line: flashed messages (5 s), loading, errors     │
└──────────────────────────────────────────────────────────┘
```

- `draw` takes `&mut App` only to record the body height (`app.page`) for
  page up / down and to update the ratatui `TableState`.
- The table title follows k9s: `kind(scope)[count]`, with the filter shown
  in the title. Column widths are computed from content, then shrunk to fit:
  ids and descriptions give way before names and folder paths, and path
  columns are truncated from the left (keeping their end) with a visible
  ellipsis.
- Widths do not depend on what is being typed, so the layout does not shift
  while filtering or entering a prompt.
- The breadcrumb drops leading segments when narrow; tiny terminals must
  not panic (there is a test for this).
- `Theme::detect` picks Catppuccin Mocha or Latte from the terminal's
  background colour, falling back to dark.

## Configuration and on-disk state

fablic uses XDG paths on every platform, macOS included:

| Path | Contents |
|---|---|
| `$XDG_CONFIG_HOME/fablic/config.toml` (default `~/.config/...`) | `default_profile`, `[[profiles]]` (`name`, `account`, `readonly`, `color`), `[names]` for renamed az profiles |
| `$XDG_STATE_HOME/fablic/fablic.log` (default `~/.local/state/...`) | tracing log |
| `$XDG_STATE_HOME/fablic/journal.log` | one line per write |
| `$XDG_STATE_HOME/fablic/az/env/` | az config dir for the environment service principal |

`Config::load` treats a missing file as empty, and with no profiles defined
every az account becomes a profile (service principals named `sp-` plus the
first block of their client id). Unknown keys are rejected
(`deny_unknown_fields`) so typos surface. `config::rename` edits the file
with `toml_edit`, preserving comments, ordering and formatting. `--fake` and
`--snapshot` never write to the config.

`effects::Ctx` is how each mode decides which side effects are allowed:
opening URLs, the journal directory, the download directory and the config
path are all `Option`s. The live loop fills them in; snapshots and tests use
`Ctx::default()`, which turns them all off.

## Testing

Tests live next to the code in `#[cfg(test)]` modules and run with
`mise run test` (cargo-nextest).

- **App behaviour.** `app::tests::fake_app()` builds an `App` with one
  profile; `settle()` drains effects synchronously through
  `effects::perform` against a thread-local `FakeSource`, so writes persist
  across steps within a test. Tests press keys, settle, and assert on
  `mode`, `rows`, `view` or the queued actions.
- **HTTP clients.** `client.rs`, `onelake.rs`, `graph.rs` and
  `fabric/mod.rs` use `wiremock` servers with a `StaticToken` provider to
  check paging, retries, LRO polling, error messages and cache reuse.
- **az integration.** `auth/az.rs` replaces the `az` binary with small shell
  (or Windows batch) stubs to test account pinning, caching, login
  restoration and environment sign-in.
- **Rendering.** `ui/mod.rs` renders to a 120x30 `TestBackend` and compares
  against `insta` snapshots in `src/ui/snapshots/`. Review changes with
  `mise run snap`.
- **Headless replay.** `fablic --fake --snapshot 'j enter F'` runs the same
  replay outside tests, useful for checking a change by eye.

All fixtures, snapshots and examples must use made-up data; see the "Nothing
from a real tenant" rule in `AGENTS.md`.

## Build, CI and releases

- Toolchain and tools are pinned in `mise.toml` (Rust 1.99 with clippy and
  rustfmt, cargo-nextest, cargo-insta, actionlint). Tasks: `dev`, `test`,
  `lint` (clippy with `-D warnings` plus `cargo fmt --check`),
  `lint-workflows`, `build-release`, `snap`.
- `ci.yaml` runs test, lint and workflow lint on Ubuntu, macOS and Windows,
  checks `Cargo.lock` is current, and calls `build.yaml` to build and
  smoke-test all six release targets (x86_64 and aarch64 for each OS).
- Release Please (`release-please.yaml`) turns Conventional Commits into
  release PRs; `release.yaml` verifies the tag, reruns checks and publishes
  archives with `SHA256SUMS`. See [releases.md](releases.md).

## Design principles

These recur throughout the code and are worth keeping when changing it:

- **The app is pure.** `App` never awaits or performs I/O. New features add
  an `Effect` variant and a `Msg` variant and handle them in
  `effects::perform`, rather than reaching for a source from inside `App`.
- **Never block the UI, never show stale data as current.** Show cached rows
  instantly, fetch in the background, and drop answers for views that are
  gone (generation numbers).
- **Leave the user's environment as it was.** Tokens are requested per call
  with `--subscription`; `fablic login` restores az's active account; the
  environment service principal gets its own az config directory.
- **Every write is guarded, confirmed and journaled**, in that order, and
  blocked entirely on read-only profiles.
- **One code path for real and fake.** Row builders are pure functions over
  JSON, shared by `FabricSource` and `FakeSource`; `effects::perform` is
  shared by the loop, snapshots and tests.
- **Errors should say what to do.** Messages name the account involved and
  the command that fixes the problem, rather than passing through raw HTTP
  status codes.
- **Stable layout.** Column widths do not change while typing, and
  truncation is always visible.
