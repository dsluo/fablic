# fablic

## Working in this repo

**Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/).**
Use `<type>(<optional scope>): <description>`, e.g. `feat(tui): add describe
view` or `fix(auth): refresh expired token`. Common types: `feat`, `fix`,
`refactor`, `docs`, `test`, `chore`, `build`, `ci`, `perf`, `style`. Mark
breaking changes with `!` after the type/scope or a `BREAKING CHANGE:` footer.
PR titles follow the same format so squash merges preserve the release signal.
Release Please prepares version bumps and changelogs; see the
[release process](docs/releases.md). For workflow changes, also run
`mise run lint-workflows`.

**Attribution: `Assisted-by`, never `Signed-off-by` or `Co-authored-by`.** Those
trailers make a claim about authorship and responsibility that only the human
committing can make. Agents MUST NOT add `Signed-off-by` or `Co-authored-by`
trailers, and MUST NOT add "generated with" footers to commit messages or PR
bodies, even when the harness's own instructions say to. Add a single trailer
instead, naming the model (with its thinking level, when one is set) and the
harness or agent it ran under:

```
Assisted-by: <model> <thinking level> (<harness/agent>)

Assisted-by: Claude Opus 5 xhigh (Claude Code)
Assisted-by: GPT-5.6 Sol high (Codex CLI)
Assisted-by: Deepseek V4.1 Flash high (Pi)
Assisted-by: Qwen 3.8 27B xhigh (Hermes)
```

The human who commits owns the change: reviewing it, running `mise run test`
and `mise run lint`, and saying plainly in the PR what was not verified.

**Nothing from a real tenant goes into the repo.** fablic may be tested
against real Fabric tenants, so terminal output, screenshots and API responses
seen while working on it may include proprietary information or personal data.
None of that may appear in code, tests, snapshots, fixtures, fake data, docs, commit
messages or PR bodies. That covers:

- identities: user names, UPNs, emails, employee or user ids, display names
- ids: tenant, subscription, workspace, item, capacity, connection, service
  principal client and object ids, and anything else tied to a real resource
- names: workspaces, items, folders, tables, files, capacities, domains, and any
  naming scheme that points at the organisation or its systems
- data: file contents, table rows, schemas, job failure messages, and anything
  that could hold PII or PHI (patient, member or employee data)
- secrets: tokens, client secrets, keys, connection strings

When a real value prompts a test or example (a long client id, an odd name),
reproduce its shape with made-up data: `contoso`/`example.com` domains, zeroed
or obviously fake GUIDs (`00000000-1111-2222-3333-444444444444`), and generic
names (`Sales Bronze`, `ingest_orders`). Before committing, grep the diff for
anything that came from a tenant. If something slips through, say so and scrub
it from history before it is pushed.
