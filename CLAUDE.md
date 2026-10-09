# fablic

## Working in this repo

**Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/).**
Use `<type>(<optional scope>): <description>`, e.g. `feat(tui): add describe
view` or `fix(auth): refresh expired token`. Common types: `feat`, `fix`,
`refactor`, `docs`, `test`, `chore`, `build`, `ci`, `perf`, `style`. Mark
breaking changes with `!` after the type/scope or a `BREAKING CHANGE:` footer.

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
