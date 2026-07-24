# jira-mcp

A small MCP server for JIRA Cloud, built for agents that read a lot of tickets and pay for every token.

The official Atlassian MCP announces ~40 tools and answers in raw JIRA JSON. Between the tool schemas
(which sit in context on *every* turn) and the payloads (which are mostly `self` links, avatar urls,
and empty fields), reading three tickets can cost tens of thousands of tokens.

This one carries **nine tools** and renders **compact plain text**. Measured on a real feature
request (a long description, a dozen labels, a couple of links):

```
just compare PROJ-142
text: 3422 chars
json: 18151 chars
```

Same information, ~5x smaller, and it reads like a ticket instead of a JSON dump. The tool schemas
themselves (the part that sits in context on every turn, whether you use JIRA or not) come to 6.6 KB.

## What you get

```
PROJ-142 Cache-aware request routing
Feature Request/Approved/Critical
assignee: Sam Rivera
reporter: Jo Park
labels: routing, tech-reviewed
components: gateway
created: 2026-07-23
updated: 2026-07-24
target_version: 4.2
links: is cloned by PROJ-98 (New)

## description
…
```

Search is one line per issue: `KEY [Type/Status] Summary @assignee #labels`.

Every read tool takes `format: "json"` when you actually need the raw payload, and `max_chars` to cap
long prose.

### Tools

| Tool | What it does |
| --- | --- |
| `jira_search` | JQL -> one compact line per issue |
| `jira_get_issue` | One issue: header fields, links, subtasks, curated custom fields, description. `comments: true` for the thread |
| `jira_get_comments` | The comment thread (newest N, rendered oldest-first) |
| `jira_add_comment` | Post a plain-text comment |
| `jira_create_issue` | Create, with a raw `fields` escape hatch |
| `jira_update_issue` | Edit fields in place |
| `jira_transition` | Move status by name; omit `to` to list what's available |
| `jira_link_issues` | Link two issues; omit `link_type` to list the site's types |
| `jira_fields` | Find a `customfield_NNNNN` by name, for the escape hatch |

Authority is one setting, `access`, enforced in the client under every tool:

| `access` | What works |
| --- | --- |
| `read-only` | the four read tools |
| `read-comment` | reads plus `jira_add_comment` |
| `read-write` (default) | everything |

## Install

Needs a Rust toolchain ([rustup](https://rustup.rs)). Nothing else: TLS is rustls, so there's no
OpenSSL to hunt for.

```bash
git clone https://github.com/wseaton/jira-mcp   # or just cd into your copy
cd jira-mcp
cargo install --path . --locked                 # -> ~/.cargo/bin/jira-mcp
jira-mcp --write-config                         # -> ~/.config/jira-mcp/config.toml
```

Prebuilt binaries for linux/macos on x86_64 and arm64 are attached to each
[release](https://github.com/wseaton/jira-mcp/releases) if you'd rather not build.

### Configure

Fill in the two lines `--write-config` left commented out:

```toml
url = "https://your-site.atlassian.net"
username = "you@corp.com"

[custom_fields]
team = "customfield_10001"
```

Everything is optional and everything has an env override (`JIRA_URL`, `JIRA_USERNAME`, …) that wins
over the file. If you already use [jira-cli](https://github.com/ankitpokhrel/jira-cli), the site and
login come from its `~/.config/.jira/.config.yml` and you can skip both.

Then get a Cloud API token from
<https://id.atlassian.com/manage-profile/security/api-tokens> and put it in the OS keychain
(macOS Keychain, Windows Credential Manager, Secret Service on Linux):

```bash
jira-mcp --set-token          # reads the token from stdin
jira-mcp --check              # confirms what it found, and that it works
```

It reads from stdin so the token never lands in your shell history or in `ps` output. The token is
looked up in this order, and `--check` tells you which one answered:

1. `JIRA_API_TOKEN`
2. the OS keychain, filed under your account email (`keychain = false` to skip the lookup)
3. `token_file`, default `~/.jiratoken`
4. `token` inline in the config

`--delete-token` removes it from the keychain again.

### Hook it up to Claude Code

```bash
claude mcp add --scope user jira -- ~/.cargo/bin/jira-mcp
```

If you were using the Atlassian MCP, drop it — that's the whole point:

```bash
claude mcp remove --scope user atlassian
```

### Any other MCP client

It's a plain stdio server, so the usual JSON works:

```json
{
  "mcpServers": {
    "jira": {
      "command": "/absolute/path/to/.cargo/bin/jira-mcp",
      "env": { "JIRA_URL": "https://your-site.atlassian.net" }
    }
  }
}
```

## Custom fields

`jira_get_issue` surfaces a curated set of custom fields by friendly name (`team`, `rice_score`,
`target_version`, …), dropping the empty ones. That map lives in
[`presets/redhat.toml`](presets/redhat.toml), which is both the compiled-in default and the template
`--write-config` installs.

Those ids are **Red Hat's** — every JIRA site numbers its custom fields differently. Find yours with
the `jira_fields` tool, then replace the `[custom_fields]` table in your config (it replaces the
default wholesale, no merging). An empty table means no custom fields at all.

## Reference

| Config key | Env override | Meaning |
| --- | --- | --- |
| `url` | `JIRA_URL` | Site base url. Falls back to `server:` in a jira-cli config |
| `username` | `JIRA_USERNAME` | Account email. Falls back to `login:` in a jira-cli config |
| `keychain` | `JIRA_MCP_KEYCHAIN` | Look the token up in the OS credential store. Default true |
| `token_file` | `JIRA_API_TOKEN_FILE` | Fallback token file. Default `~/.jiratoken` |
| `token` | `JIRA_API_TOKEN` | The token inline. Last resort |
| `access` | `JIRA_MCP_ACCESS` | `read-only`, `read-comment`, or `read-write` |
| `[custom_fields]` | — | `name = "customfield_N"` |

Config file lookup: `$JIRA_MCP_CONFIG`, then `$XDG_CONFIG_HOME/jira-mcp/config.toml`, then
`~/.config/jira-mcp/config.toml`. `JIRA_CLI_CONFIG` relocates the jira-cli file.

## Embedding

The crate is a library as well as a binary, so another MCP server can reuse the client and the
renderers without inheriting this one's nine tools — which matters when the host mediates what an
agent may reach:

```rust
use jira_mcp::{Access, Config, JiraClient, render};

let Some(mut cfg) = Config::from_env() else { /* JIRA off: report `disabled` */ };
cfg.access = Access::ReadComment;   // this agent may read and comment, nothing more
let jira = JiraClient::new(cfg);
let issue = jira.get_issue("PROJ-1", false).await?;
println!("{}", render::issue(&issue, jira.config(), 6000));
```

`Config::from_env()` is the strict, file-free path for a host that injects credentials itself;
`Config::load()` is the layered one the binary uses. To expose the full tool surface instead, serve
`jira_mcp::JiraMcp`.

## Development

```bash
just            # fmt + clippy + test
just check      # verify creds against the live site
just smoke jira_get_issue '{"issue_key":"PROJ-142"}'
just compare PROJ-142
```

Notes for anyone extending it:

- Prose endpoints use `/rest/api/2` on purpose, so descriptions and comments are plain text instead
  of ADF document JSON. Never "upgrade" those to api/3.
- Search uses `POST /rest/api/3/search/jql`; the old `/rest/api/3/search` was removed (CHANGE-2046).
- Adding a tool costs context on every turn, in every session. `tool_surface_stays_small` will fail
  when the count changes — that's the point, make it a decision.
