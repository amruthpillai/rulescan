# Discovery coverage matrix

Profile revision: `builtin-1`. These are fixture-tested candidate conventions,
not a verified loading map for every installed agent/client version. No agent
installation is required; no agent binary executes during scanning.

| Profile | Candidate rules | Loading/reference limits | Primary source |
| --- | --- | --- | --- |
| Shared / Codex | AGENTS.md, AGENTS.override.md | Any depth; no installed-version or configured-fallback-name parser (use a custom rule) | [Codex instructions](https://developers.openai.com/codex/guides/agents-md) |
| Claude | CLAUDE.md, CLAUDE.local.md, `.claude/rules/**/*.md` | Conservative standalone `@local/path` directives only; 16-level bounded graph, cycles/outside scopes incomplete | [Claude memory](https://code.claude.com/docs/en/memory) |
| Gemini | GEMINI.md | Same bounded standalone local-reference adapter; extension/settings-specific context names need custom rules | [Gemini context](https://geminicli.com/docs/cli/gemini-md/) |
| Skills | SKILL.md / custom marker directories | Entire bounded manifest includes files, empty directories, scripts, resources, mode bits, link descriptions | [Agent Skills](https://agentskills.io/specification) |
| Copilot | `.github/copilot-instructions.md`, `.github/instructions/**/*.instructions.md`, `.github/prompts/**/*.prompt.md`, `.github/agents/**/*.agent.md` | Client-specific loading conditions not resolved | [GitHub instructions](https://docs.github.com/en/copilot/how-tos/copilot-on-github/customize-copilot/add-custom-instructions/add-repository-instructions) |
| Cursor | .cursorrules, `.cursor/rules/**/*.mdc` | Rule metadata and installed-client semantics not interpreted | [Cursor rules](https://cursor.com/docs/context/rules) |
| Hooks / config | .mcp.json, mcp.json, `.claude`/`.gemini` settings JSON, `.codex/config.toml` | Inspected as data; no hook execution, MCP server startup or implicit recursive settings imports | Vendor configuration documentation |
| Custom | Exact basename, root-relative glob, bundle marker | Declarative matching; categories do not grant trust | `patterns test` and `schema --kind config` |

Interactive default global suggestions are existing `~/.codex`, `~/.claude`,
`~/.gemini`, `~/.config/agents`, and `~/.agents/skills`. Other locations need explicit
roots or saved configuration. Candidate discovery includes hidden and ignored
dependencies. Root replacements and new worktrees create distinct location
identities; repository origin and Git commits grant no approval.

Imports are separately inventoried nodes. Only standalone lines beginning with
`@` are recognized; inline Markdown imports, JSON/TOML references and each vendor's
full syntax are not advertised. Remote references are not fetched. Paths outside
the parent's covered root are not followed even if they exist. A shared imported
file invalidates all dependent fingerprints. Unresolved/cyclic graphs cannot be
approved. A bundle spanning an explicit exclusion or private state is incomplete,
rather than silently approving only the visible subset.

Budgets, unsupported link cases and inaccessible roots are explicit gaps. Each
scan caps detailed gap records at 1,000; reaching that cap still leaves coverage
incomplete. The manifest/entry budgets are safety bounds, not measured scale
claims. Vendor URLs above are research entry points; their current behavior and
installed versions require release-specific native fixtures before publishing a
"known to load" compatibility claim.
