//! The nine operations, once.
//!
//! Both front ends are thin over this module: [`crate::server`] wraps each one in an MCP tool, and
//! the CLI wraps each in a subcommand. Neither owns any logic — an agent scripting `jira-mcp search`
//! in a shell and an agent calling `jira_search` over MCP must get byte-identical output, or the
//! skill that teaches one is lying about the other.
//!
//! Every op returns `Result<String>`: the MCP side flattens an error into a readable line (a tool
//! error is data the model acts on), the CLI side exits non-zero (a failed command is a failed
//! command).

use crate::client::JiraClient;
use crate::render;
use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

/// Fields a create/update sets, assembled from typed args plus a raw escape hatch.
#[derive(Debug, Default)]
pub struct IssueFields {
    pub summary: Option<String>,
    pub description: Option<String>,
    pub labels: Option<Vec<String>>,
    /// Raw `{"customfield_N": …}` merged in LAST, so it can override anything above.
    pub extra: Option<Value>,
}

impl IssueFields {
    fn into_map(self) -> Result<Map<String, Value>> {
        let mut m = Map::new();
        put(&mut m, "summary", self.summary);
        put(&mut m, "description", self.description);
        put(&mut m, "labels", self.labels);
        merge_extra(&mut m, self.extra)?;
        Ok(m)
    }
}

/// JQL -> one compact line per issue (or the raw rows).
pub async fn search(jira: &JiraClient, jql: &str, limit: u32, json_out: bool) -> Result<String> {
    let rows = jira.search(jql, limit).await?;
    Ok(if json_out {
        dump(&Value::Array(rows))
    } else {
        render::search_rows(&rows)
    })
}

/// One issue, rendered compact (or raw).
pub async fn issue(
    jira: &JiraClient,
    key: &str,
    comments: bool,
    max_chars: usize,
    json_out: bool,
) -> Result<String> {
    let v = jira.get_issue(key, comments).await?;
    Ok(if json_out {
        dump(&v)
    } else {
        render::issue(&v, jira.config(), max_chars)
    })
}

/// The comment thread, newest `limit`, rendered oldest-first.
pub async fn comments(
    jira: &JiraClient,
    key: &str,
    limit: u32,
    max_chars: usize,
    json_out: bool,
) -> Result<String> {
    let mut list = jira.get_comments(key, limit).await?;
    // Fetched newest-first (so a limit keeps the RECENT ones); read oldest-first.
    list.reverse();
    Ok(if json_out {
        dump(&Value::Array(list))
    } else {
        render::comments(&list, max_chars)
    })
}

pub async fn add_comment(jira: &JiraClient, key: &str, body: &str) -> Result<String> {
    let id = jira.add_comment(key, body).await?;
    Ok(format!("commented (id {id}) {}", jira.browse_url(key)))
}

pub async fn create_issue(
    jira: &JiraClient,
    project: &str,
    issue_type: &str,
    summary: String,
    parent: Option<String>,
    fields: IssueFields,
) -> Result<String> {
    let mut m = IssueFields {
        summary: Some(summary),
        ..fields
    }
    .into_map()?;
    // Identity fields go in after, but never override an explicit `extra` — a caller who spells out
    // `issuetype` with an id means it.
    m.entry("project").or_insert(json!({ "key": project }));
    m.entry("issuetype")
        .or_insert(json!({ "name": issue_type }));
    if let Some(p) = parent {
        m.entry("parent").or_insert(json!({ "key": p }));
    }
    let key = jira.create_issue(m).await?;
    Ok(format!("created {key} {}", jira.browse_url(&key)))
}

pub async fn update_issue(jira: &JiraClient, key: &str, fields: IssueFields) -> Result<String> {
    let m = fields.into_map()?;
    if m.is_empty() {
        bail!("nothing to update (pass summary, description, labels, or fields)");
    }
    jira.update_issue(key, m).await?;
    Ok(format!("updated {}", jira.browse_url(key)))
}

/// Move an issue by transition/status NAME. `None` lists what's reachable from here.
pub async fn transition(jira: &JiraClient, key: &str, to: Option<&str>) -> Result<String> {
    let available = jira.transitions(key).await?;
    let names = || {
        available
            .iter()
            .map(|(_, n)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let Some(want) = to else {
        return Ok(format!("transitions: {}", names()));
    };
    let Some((id, name)) = available
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(want.trim()))
    else {
        bail!("no transition named {want:?}; available: {}", names());
    };
    jira.transition(key, id).await?;
    Ok(format!("{key} -> {name}"))
}

/// Link two issues, or list the site's link types when `link_type` is `None`.
pub async fn link(
    jira: &JiraClient,
    link_type: Option<&str>,
    inward: Option<&str>,
    outward: Option<&str>,
) -> Result<String> {
    let Some(link_type) = link_type else {
        return Ok(render::link_types(&jira.link_types().await?));
    };
    let (Some(inward), Some(outward)) = (inward, outward) else {
        bail!("a link type needs both an inward and an outward issue key");
    };
    jira.link(link_type, inward, outward).await?;
    Ok(format!("{inward} {link_type} {outward}"))
}

/// Field ids by name substring — how you find the `customfield_NNNNN` for the escape hatch.
pub async fn fields(jira: &JiraClient, query: &str) -> Result<String> {
    Ok(render::fields(&jira.fields().await?, query))
}

/// Set a field only when the caller supplied one — an absent arg must not clear the value in JIRA.
fn put<T: Into<Value>>(fields: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(v) = value {
        fields.insert(key.to_string(), v.into());
    }
}

/// Merge the raw `fields` escape hatch last, so it can override anything the typed args set.
fn merge_extra(fields: &mut Map<String, Value>, extra: Option<Value>) -> Result<()> {
    match extra {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(m)) => {
            fields.extend(m);
            Ok(())
        }
        Some(other) => bail!("`fields` must be a JSON object of field id -> value, got {other}"),
    }
}

/// `--json` output. Compact, not pretty-printed: the reader is a model or a `jq`, and indentation is
/// pure cost to one and invisible to the other.
fn dump(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("error: serializing response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_fields_override_typed_args() {
        let m = IssueFields {
            summary: Some("typed".into()),
            extra: Some(json!({"summary": "raw"})),
            ..Default::default()
        }
        .into_map()
        .expect("object merges");
        assert_eq!(m["summary"], "raw");
    }

    #[test]
    fn a_non_object_escape_hatch_is_an_error() {
        let err = IssueFields {
            extra: Some(json!("nope")),
            ..Default::default()
        }
        .into_map()
        .expect_err("must reject");
        assert!(err.to_string().contains("must be a JSON object"), "{err}");
    }

    /// An empty update would be a no-op PUT against JIRA; say so instead of pretending it worked.
    #[test]
    fn empty_update_is_rejected_before_the_call() {
        let m = IssueFields::default()
            .into_map()
            .expect("empty is fine here");
        assert!(m.is_empty(), "the guard lives in update_issue, on this map");
    }
}
