# Workflow examples

`software-engineering.workflow.json` is the definition Agentree runs when you pass no `--workflow`. It is generated from the code (`AGENTREE_WRITE_EXAMPLES=1 cargo test workflow` rewrites it) and a normal test run fails if the two differ.

Run a copy of it, or your own definition, on the Claude native-team adapter:

```powershell
agentree --project C:\path\to\repo --workflow .\my.workflow.json --workflow-path thorough
```

`--workflow-path` is `fast`, `balanced` (default) or `thorough`; the workflow's `max_path` budget cap still lowers it, and final review runs on every path.

What the adapter does with a definition (details in [docs/WORKFLOWS.md](../../docs/WORKFLOWS.md#claude-adapter)):

- **Executes:** the lead's model, effort, permission mode and advisor as launch flags; the default model for unnamed subagents as an environment variable (a per-call model can override it). The single Claude coordinator and its named teammates are unchanged.
- **Only instructs:** each teammate's model, tools, permission mode and concurrency; delegation of bulk reading to the lowest-cost capable model; the review packet (real diff, objective, acceptance criteria, verification evidence); loading a role's skill from its `SKILL.md`; Git-write authorization.
- **Unsupported:** per-teammate enforcement of any of the above, reviewer packet isolation, and plugin MCP servers.

Setting `"strict_enforcement": true` makes validation fail on Claude native teams, because the mandatory bulk-reading and review policies can only be instructed there. Leave it `false` until a backend can enforce them; the launch then lists every limitation instead.

A skill is offered to a role only when its plugin is trusted at its current contents (`agentree --plugin <folder> --plugin-trust`). Only the bundled `software-engineering` plugin is loaded in this release. A definition you supply that names an untrusted or other plugin is rejected; the built-in definition runs without skills and says so.
