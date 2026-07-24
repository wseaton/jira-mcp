//! The MCP tool surface: nine tools, deliberately.
//!
//! Tool *descriptions* are themselves context the model pays for on every single turn, so they're
//! terse on purpose. The full-fat Atlassian MCP spends tens of thousands of tokens announcing tools
//! you never call; this one spends a few hundred.

use crate::client::JiraClient;
use crate::ops::{self, IssueFields};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

/// How much prose one description or comment may contribute before it's cut.
fn default_max_chars() -> usize {
    6000
}
fn default_limit() -> u32 {
    25
}
fn default_format() -> String {
    "text".into()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// JQL, e.g. `project = PROJ AND labels = routing ORDER BY updated DESC`.
    pub jql: String,
    /// Max issues (clamped to [1,100]). Default 25.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// `text` (default, compact) or `json` (raw rows).
    #[serde(default = "default_format")]
    pub format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetIssueArgs {
    /// Issue key, e.g. `PROJ-142`.
    pub issue_key: String,
    /// Also fetch the comment thread. Default false (comments are usually the bulk of an issue).
    #[serde(default)]
    pub comments: bool,
    /// Cut each description/comment at this many chars. Default 6000.
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// `text` (default, ~10x smaller) or `json` (the full raw issue, every field).
    #[serde(default = "default_format")]
    pub format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetCommentsArgs {
    /// Issue key.
    pub issue_key: String,
    /// Newest N comments (clamped to [1,100]), rendered oldest-first. Default 25.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Cut each comment at this many chars. Default 6000.
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// `text` (default) or `json`.
    #[serde(default = "default_format")]
    pub format: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddCommentArgs {
    /// Issue key.
    pub issue_key: String,
    /// Comment body, plain text / JIRA wiki markup (NOT ADF).
    pub comment: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateIssueArgs {
    /// Project key, e.g. `PROJ`.
    pub project: String,
    /// Issue type NAME, e.g. `Story`, `Bug`, `Epic`, `Feature`.
    pub issue_type: String,
    /// Summary line.
    pub summary: String,
    /// Description, plain text / JIRA wiki markup.
    #[serde(default)]
    pub description: Option<String>,
    /// Parent issue key (an epic, for a company-managed project).
    #[serde(default)]
    pub parent: Option<String>,
    /// Labels to set.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// Escape hatch: extra raw `fields` merged in last, e.g.
    /// `{"customfield_10001": {"id": "…"}, "priority": {"name": "Major"}}`.
    /// Use `jira_fields` to find ids.
    #[serde(default)]
    pub fields: Option<Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateIssueArgs {
    /// Issue key.
    pub issue_key: String,
    /// New summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// New description (replaces the existing one).
    #[serde(default)]
    pub description: Option<String>,
    /// New label set (replaces the existing one).
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// Escape hatch: raw `fields` merged in last. Use `jira_fields` to find ids.
    #[serde(default)]
    pub fields: Option<Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TransitionArgs {
    /// Issue key.
    pub issue_key: String,
    /// Target transition or status NAME (case-insensitive). Omit to list what's available.
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LinkArgs {
    /// Link type name, e.g. `Blocks`, `Relates`. Omit to list the site's link types.
    #[serde(default)]
    pub link_type: Option<String>,
    /// The issue on the INWARD side (for `Blocks`: the blocker).
    #[serde(default)]
    pub inward_key: Option<String>,
    /// The issue on the OUTWARD side (for `Blocks`: the blocked one).
    #[serde(default)]
    pub outward_key: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FieldsArgs {
    /// Case-insensitive substring of the field name, e.g. `team`. Empty lists every field.
    #[serde(default)]
    pub query: String,
}

/// The MCP server. One shared [`JiraClient`]; every tool is a thin call + render.
#[derive(Clone)]
pub struct JiraMcp {
    jira: Arc<JiraClient>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl JiraMcp {
    pub fn new(jira: Arc<JiraClient>) -> Self {
        Self {
            jira,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Search JIRA by JQL. Returns one compact line per issue: KEY [Type/Status] \
        Summary @assignee #labels. Use jira_get_issue for detail."
    )]
    async fn jira_search(&self, Parameters(a): Parameters<SearchArgs>) -> String {
        flatten(ops::search(&self.jira, &a.jql, a.limit, json_wanted(&a.format)).await)
    }

    #[tool(
        description = "Fetch one issue as compact text: header fields, links, subtasks, curated \
        custom fields (nulls dropped), then the description. Set comments=true for the thread, \
        format=json for every raw field."
    )]
    async fn jira_get_issue(&self, Parameters(a): Parameters<GetIssueArgs>) -> String {
        flatten(
            ops::issue(
                &self.jira,
                &a.issue_key,
                a.comments,
                a.max_chars,
                json_wanted(&a.format),
            )
            .await,
        )
    }

    #[tool(description = "Read an issue's comment thread (newest N, rendered oldest-first).")]
    async fn jira_get_comments(&self, Parameters(a): Parameters<GetCommentsArgs>) -> String {
        flatten(
            ops::comments(
                &self.jira,
                &a.issue_key,
                a.limit,
                a.max_chars,
                json_wanted(&a.format),
            )
            .await,
        )
    }

    #[tool(description = "Post a plain-text comment on an issue.")]
    async fn jira_add_comment(&self, Parameters(a): Parameters<AddCommentArgs>) -> String {
        flatten(ops::add_comment(&self.jira, &a.issue_key, &a.comment).await)
    }

    #[tool(description = "Create an issue. Returns the new key and url.")]
    async fn jira_create_issue(&self, Parameters(a): Parameters<CreateIssueArgs>) -> String {
        flatten(
            ops::create_issue(
                &self.jira,
                &a.project,
                &a.issue_type,
                a.summary,
                a.parent,
                IssueFields {
                    description: a.description,
                    labels: a.labels,
                    extra: a.fields,
                    ..Default::default()
                },
            )
            .await,
        )
    }

    #[tool(description = "Edit an issue's fields in place.")]
    async fn jira_update_issue(&self, Parameters(a): Parameters<UpdateIssueArgs>) -> String {
        flatten(
            ops::update_issue(
                &self.jira,
                &a.issue_key,
                IssueFields {
                    summary: a.summary,
                    description: a.description,
                    labels: a.labels,
                    extra: a.fields,
                },
            )
            .await,
        )
    }

    #[tool(
        description = "Move an issue to another status by transition/status name. Omit `to` to list \
        the transitions available from the current status."
    )]
    async fn jira_transition(&self, Parameters(a): Parameters<TransitionArgs>) -> String {
        flatten(ops::transition(&self.jira, &a.issue_key, a.to.as_deref()).await)
    }

    #[tool(
        description = "Link two issues (inward_key <link_type> outward_key). Omit `link_type` to \
        list the site's link types with their direction words."
    )]
    async fn jira_link_issues(&self, Parameters(a): Parameters<LinkArgs>) -> String {
        flatten(
            ops::link(
                &self.jira,
                a.link_type.as_deref(),
                a.inward_key.as_deref(),
                a.outward_key.as_deref(),
            )
            .await,
        )
    }

    #[tool(
        description = "Look up field ids by name (e.g. 'team' -> customfield_10001) for the `fields` \
        escape hatch on create/update."
    )]
    async fn jira_fields(&self, Parameters(a): Parameters<FieldsArgs>) -> String {
        flatten(ops::fields(&self.jira, &a.query).await)
    }
}

fn json_wanted(format: &str) -> bool {
    format.eq_ignore_ascii_case("json")
}

/// An op's error becomes a plain line the model can read (and act on) rather than a protocol fault.
/// The CLI does the opposite with the same `Result`: it exits non-zero.
fn flatten(r: anyhow::Result<String>) -> String {
    r.unwrap_or_else(|e| format!("error: {e:#}"))
}

#[tool_handler]
impl ServerHandler for JiraMcp {
    /// Advertise the `tools` capability during `initialize`. Without it a spec-compliant client
    /// (Claude Code) connects, sees no tools capability, and never calls `tools/list` — the server
    /// looks connected but exposes nothing.
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some(
                "JIRA Cloud. Read tools return compact text; pass format=\"json\" for raw payloads."
                    .into(),
            ),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn server() -> JiraMcp {
        JiraMcp::new(Arc::new(JiraClient::new(Config {
            base: "https://x.atlassian.net".into(),
            email: "me@x".into(),
            token: "t".into(),
            token_source: crate::config::TokenSource::Inline,
            access: crate::config::Access::ReadWrite,
            custom_fields: Vec::new(),
        })))
    }

    /// The handshake must declare tools, else clients never list them (a bug that presents as
    /// "connected, zero tools").
    #[test]
    fn get_info_advertises_tools() {
        assert!(server().get_info().capabilities.tools.is_some());
    }

    /// The whole point of the rewrite: a small, fixed tool surface. If this count grows, the context
    /// cost grows on every turn — make it a deliberate decision, not a drift.
    #[test]
    fn tool_surface_stays_small() {
        let names: Vec<_> = JiraMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(names.len(), 9, "tools: {names:?}");
        assert!(names.contains(&"jira_get_issue".to_string()), "{names:?}");
    }
}
