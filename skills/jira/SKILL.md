---
name: jira
description: Read and write JIRA Cloud issues from the shell with the jira-mcp CLI — search by JQL, read issues and comment threads, comment, create, update, transition, and link. Use when a task involves JIRA tickets, requirements written in an issue, sprint or backlog state, or when you need to leave a comment or file an issue. Output is compact plain text built for reading in context, and the same nine operations are available as MCP tools.
---

# Work JIRA from the shell

`jira-mcp` is both an MCP server and a CLI over the same nine operations, printing
the same bytes either way. Reach for the CLI when you want to **script** JIRA:
loop over keys, pipe into grep, pull one field out of thirty issues. Reach for the
MCP tools when you want a single lookup mid-conversation.

The output is deliberately compact — a ticket costs roughly a fifth of what its raw
JSON does, so you can read ten of them without burning the context you needed for
the actual work.

## Prerequisites

- `jira-mcp` on PATH (`cargo install --git https://github.com/wseaton/jira-mcp`).
- Working credentials: `jira-mcp check` prints the site, account, access level, and
  where the token came from. **Run it first** if anything behaves oddly; it
  distinguishes "not configured" from "wrong token" from "no permission".
- Access level matters. `jira-mcp check` reports it:
  - `read-only` — the four read verbs
  - `read-comment` — reads plus `comment`
  - `read-write` — everything
  A refusal names the level, so don't retry a create against a `read-only` client.

## Reading

```bash
# One compact line per issue: KEY [Type/Status] Summary @assignee #labels
jira-mcp search 'project = PROJ AND status = "In Progress" ORDER BY updated DESC' -l 20

# One issue: header fields, links, subtasks, curated custom fields, description
jira-mcp issue PROJ-142

# ...with the comment thread, and a tighter cap on prose
jira-mcp issue PROJ-142 --comments --max-chars 2000

# Just the thread (newest 10, printed oldest-first)
jira-mcp comments PROJ-142 -l 10
```

Empty fields are dropped, so what you see is what the issue has. Timestamps are
trimmed to the day, and Jira's `{color}` markup is stripped.

### JQL that pays off

```bash
# Everything assigned to me that isn't done
jira-mcp search 'assignee = currentUser() AND statusCategory != Done'

# Recently touched, most recent first — the usual "what's happening here"
jira-mcp search 'project = PROJ AND updated >= -7d ORDER BY updated DESC'

# By label, by epic, by fix version
jira-mcp search 'labels = platform AND fixVersion = "4.2"'
jira-mcp search 'parent = PROJ-7'
```

`search` returns at most 100 (`-l`, default 25). Narrow the JQL rather than paging:
a tighter query is cheaper than a longer list you then have to read.

## Scripting

The one-line-per-issue format is built for this.

```bash
# Read every open bug in a project, in one pass
for key in $(jira-mcp search 'project = PROJ AND type = Bug AND statusCategory != Done' -l 50 \
             | tail -n +2 | cut -d' ' -f1); do
  jira-mcp issue "$key" --max-chars 800
  echo "---"
done

# Which of these tickets mention a term?
jira-mcp search 'project = PROJ AND updated >= -30d' -l 100 | grep -i cache

# When you need to parse rather than read, ask for JSON
jira-mcp issue PROJ-142 --json | jq -r '.fields.customfield_10001.name'
```

The first line of `search` output is the count (`12 issues`); `tail -n +2` skips it.

## Writing

```bash
# Comment. `-` reads the body from stdin, which is how you post multiple lines.
jira-mcp comment PROJ-142 'Confirmed on 4.2; the fix is in #1234.'
jira-mcp comment PROJ-142 - <<'EOF'
Reproduced with:
  ./run --config cache.yaml
EOF

# Create. --label repeats; --parent puts it under an epic.
jira-mcp create -p PROJ -t Story -s 'Cache-aware routing' \
  -d 'Route by cache residency instead of round-robin.' \
  --parent PROJ-7 --label routing --label perf

# Update in place. Omitted fields are left alone; --label REPLACES the set.
jira-mcp update PROJ-142 -s 'Better summary' -d - <<'EOF'
Rewritten description.
EOF

# Move status. Omit the target to see what's reachable from here.
jira-mcp transition PROJ-142
jira-mcp transition PROJ-142 'In Progress'

# Link. Omit everything to list the site's link types and their direction words.
jira-mcp link
jira-mcp link Blocks PROJ-7 PROJ-142     # PROJ-7 blocks PROJ-142
```

Bodies and descriptions are **plain text / JIRA wiki markup**, not Markdown and not
ADF. `h2. Heading`, `* bullet`, `{code}…{code}`.

### Custom fields

Typed flags cover the common fields; anything else goes through `--fields`, which is
merged last and can override them:

```bash
jira-mcp fields team                      # -> customfield_10001  Team
jira-mcp create -p PROJ -t Story -s 'x' --fields '{"customfield_10001":{"id":"42"}}'
```

`jira-mcp issue` already surfaces a curated set of custom fields by friendly name
(`team`, `rice_score`, `target_version`, …), configured in
`~/.config/jira-mcp/config.toml`.

## Before you write

- **Transitions are workflow-specific.** Run `jira-mcp transition KEY` with no target
  and pick from what it lists; guessing a status name fails.
- **`update --label` replaces the label set**, it doesn't append. Read the current
  labels first if you mean to add one.
- **A create is hard to take back.** Confirm the project and issue type with the user
  when you're inferring them rather than being told.
