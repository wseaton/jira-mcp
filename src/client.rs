//! A thin JIRA Cloud REST client.
//!
//! Endpoint choices that matter:
//! - search is the bounded `POST /rest/api/3/search/jql` (the old `/rest/api/3/search` was removed,
//!   Atlassian CHANGE-2046),
//! - everything that carries prose (get/create/update issue, comments) uses `/rest/api/2/…` so
//!   descriptions and comment bodies are PLAIN TEXT instead of ADF document JSON. ADF round-trips are
//!   a tax on both ends: the model can't read it cheaply and can't write it correctly.
//! - the `_v3`/`_adf` variants are the deliberate exception: when the caller opts into markdown,
//!   [`crate::adf`] builds the ADF and `/rest/api/3/…` carries it. Reads stay on api/2.

use crate::config::{Access, Config};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// A connected JIRA Cloud client. Cheap to share behind an `Arc`.
pub struct JiraClient {
    cfg: Config,
    http: reqwest::Client,
}

/// The compact fields a search row carries (enough to triage; `get_issue` for detail).
const SEARCH_FIELDS: &[&str] = &["summary", "status", "issuetype", "labels", "assignee"];

impl JiraClient {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// `https://site/browse/KEY`, the link a human clicks.
    pub fn browse_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.cfg.base)
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.cfg.base))
            .basic_auth(&self.cfg.email, Some(&self.cfg.token))
            .header("Accept", "application/json")
    }

    /// The single choke point for authority: every mutating call goes through here first, so an
    /// embedding host that configures `read-comment` cannot be talked into a create by any tool,
    /// present or future.
    fn require(&self, need: Access) -> Result<()> {
        if !self.cfg.access.allows(need) {
            bail!(
                "this ujira client is configured {} — {need} is required for that call",
                self.cfg.access
            );
        }
        Ok(())
    }

    /// JQL search. Returns the raw `issues` array (rendering is the caller's job).
    pub async fn search(&self, jql: &str, limit: u32) -> Result<Vec<Value>> {
        let body = json!({"jql": jql, "maxResults": limit.clamp(1, 100), "fields": SEARCH_FIELDS});
        let v = self
            .send(
                self.req(reqwest::Method::POST, "/rest/api/3/search/jql")
                    .json(&body),
                "search",
            )
            .await?;
        Ok(v.get("issues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// One issue, plus its links and (optionally) its comments. `/rest/api/2` for plain-text prose.
    pub async fn get_issue(&self, key: &str, with_comments: bool) -> Result<Value> {
        // Comments cost a page of prose each, so they're opt-in: `*all,-comment` is api/2's
        // "everything except" selector.
        let path = if with_comments {
            format!("/rest/api/2/issue/{key}")
        } else {
            format!("/rest/api/2/issue/{key}?fields=*all,-comment")
        };
        self.send(self.req(reqwest::Method::GET, &path), "get_issue")
            .await
    }

    /// The newest `limit` comments on an issue.
    pub async fn get_comments(&self, key: &str, limit: u32) -> Result<Vec<Value>> {
        let path = format!(
            "/rest/api/2/issue/{key}/comment?orderBy=-created&maxResults={}",
            limit.clamp(1, 100)
        );
        let v = self
            .send(self.req(reqwest::Method::GET, &path), "get_comments")
            .await?;
        Ok(v.get("comments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Post a comment with an ADF body (api/v3). Returns the new comment id.
    pub async fn add_comment_adf(&self, key: &str, body_adf: Value) -> Result<String> {
        self.require(Access::ReadComment)?;
        let v = self
            .send(
                self.req(
                    reqwest::Method::POST,
                    &format!("/rest/api/3/issue/{key}/comment"),
                )
                .json(&json!({ "body": body_adf })),
                "add_comment_adf",
            )
            .await?;
        Ok(id_of(&v))
    }

    /// Post a plain-text comment. Returns the new comment id.
    pub async fn add_comment(&self, key: &str, body: &str) -> Result<String> {
        self.require(Access::ReadComment)?;
        let v = self
            .send(
                self.req(
                    reqwest::Method::POST,
                    &format!("/rest/api/2/issue/{key}/comment"),
                )
                .json(&json!({ "body": body })),
                "add_comment",
            )
            .await?;
        Ok(id_of(&v))
    }

    /// Create an issue from an already-assembled `fields` object. Returns the new key.
    pub async fn create_issue(&self, fields: Map<String, Value>) -> Result<String> {
        self.require(Access::ReadWrite)?;
        let v = self
            .send(
                self.req(reqwest::Method::POST, "/rest/api/2/issue")
                    .json(&json!({ "fields": Value::Object(fields) })),
                "create_issue",
            )
            .await?;
        Ok(v.get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Create an issue via api/v3 (accepts ADF in description). Returns the new key.
    pub async fn create_issue_v3(&self, fields: Map<String, Value>) -> Result<String> {
        self.require(Access::ReadWrite)?;
        let v = self
            .send(
                self.req(reqwest::Method::POST, "/rest/api/3/issue")
                    .json(&json!({ "fields": Value::Object(fields) })),
                "create_issue_v3",
            )
            .await?;
        Ok(v.get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Edit an issue's fields in place. A 204 carries no body, so there's nothing to return.
    pub async fn update_issue(&self, key: &str, fields: Map<String, Value>) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(reqwest::Method::PUT, &format!("/rest/api/2/issue/{key}"))
                .json(&json!({ "fields": Value::Object(fields) })),
            "update_issue",
        )
        .await?;
        Ok(())
    }

    /// Edit an issue's fields via api/v3 (accepts ADF values for description/comment bodies).
    pub async fn update_issue_v3(&self, key: &str, fields: Map<String, Value>) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(reqwest::Method::PUT, &format!("/rest/api/3/issue/{key}"))
                .json(&json!({ "fields": Value::Object(fields) })),
            "update_issue_v3",
        )
        .await?;
        Ok(())
    }

    /// The transitions available from the issue's current status, as `(id, name)`.
    pub async fn transitions(&self, key: &str) -> Result<Vec<(String, String)>> {
        let v = self
            .send(
                self.req(
                    reqwest::Method::GET,
                    &format!("/rest/api/2/issue/{key}/transitions"),
                ),
                "transitions",
            )
            .await?;
        Ok(v.get("transitions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|t| (id_of(t), str_at(t, "/name").to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Drive a transition by id (resolve the name first with [`Self::transitions`]).
    pub async fn transition(&self, key: &str, id: &str) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(
                reqwest::Method::POST,
                &format!("/rest/api/2/issue/{key}/transitions"),
            )
            .json(&json!({"transition": {"id": id}})),
            "transition",
        )
        .await?;
        Ok(())
    }

    /// Link two issues: `inward` <-link type-> `outward` (e.g. Blocks: inward blocks outward).
    pub async fn link(&self, link_type: &str, inward: &str, outward: &str) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(reqwest::Method::POST, "/rest/api/2/issueLink")
                .json(&json!({
                    "type": {"name": link_type},
                    "inwardIssue": {"key": inward},
                    "outwardIssue": {"key": outward},
                })),
            "link",
        )
        .await?;
        Ok(())
    }

    /// The site's link type names (the only legal values for `jira_link_issues`).
    pub async fn link_types(&self) -> Result<Vec<Value>> {
        let v = self
            .send(
                self.req(reqwest::Method::GET, "/rest/api/2/issueLinkType"),
                "link_types",
            )
            .await?;
        Ok(v.get("issueLinkTypes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Incremental label add: appends without replacing the existing set.
    pub async fn add_labels(&self, key: &str, labels: &[String]) -> Result<()> {
        self.require(Access::ReadWrite)?;
        let ops: Vec<Value> = labels.iter().map(|l| json!({"add": l})).collect();
        self.send(
            self.req(reqwest::Method::PUT, &format!("/rest/api/2/issue/{key}"))
                .json(&json!({"update": {"labels": ops}})),
            "add_labels",
        )
        .await?;
        Ok(())
    }

    /// Incremental label remove: drops specific labels without touching the rest.
    pub async fn remove_labels(&self, key: &str, labels: &[String]) -> Result<()> {
        self.require(Access::ReadWrite)?;
        let ops: Vec<Value> = labels.iter().map(|l| json!({"remove": l})).collect();
        self.send(
            self.req(reqwest::Method::PUT, &format!("/rest/api/2/issue/{key}"))
                .json(&json!({"update": {"labels": ops}})),
            "remove_labels",
        )
        .await?;
        Ok(())
    }

    /// Upload a file attachment. Returns the attachment JSON array from JIRA.
    pub async fn add_attachment(&self, key: &str, filename: &str, data: Vec<u8>) -> Result<Value> {
        self.require(Access::ReadWrite)?;
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(filename.to_string())
            .mime_str(&mime_from_filename(filename))
            .context("setting attachment MIME type")?;
        self.send(
            self.req(
                reqwest::Method::POST,
                &format!("/rest/api/2/issue/{key}/attachments"),
            )
            // Without this header JIRA rejects the multipart POST as potential XSRF.
            .header("X-Atlassian-Token", "no-check")
            .multipart(reqwest::multipart::Form::new().part("file", part)),
            "add_attachment",
        )
        .await
    }

    /// Delete an attachment by id.
    pub async fn delete_attachment(&self, id: &str) -> Result<()> {
        self.require(Access::ReadWrite)?;
        self.send(
            self.req(
                reqwest::Method::DELETE,
                &format!("/rest/api/2/attachment/{id}"),
            ),
            "delete_attachment",
        )
        .await?;
        Ok(())
    }

    /// Every field on the site (id + name + custom flag) — the lookup behind `jira_fields`, which is
    /// how you find the `customfield_NNNNN` to pass to create/update.
    pub async fn fields(&self) -> Result<Vec<Value>> {
        let v = self
            .send(
                self.req(reqwest::Method::GET, "/rest/api/2/field"),
                "fields",
            )
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Send, then map a non-2xx to an error carrying the (truncated) body. A 204/empty body becomes
    /// `null` rather than a parse error, which is what the write endpoints return.
    async fn send(&self, req: reqwest::RequestBuilder, what: &str) -> Result<Value> {
        let resp = req
            .send()
            .await
            .with_context(|| format!("jira {what} request"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("jira {what} failed ({status}): {}", truncate(&text, 400));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .with_context(|| format!("parsing jira {what} response: {}", truncate(&text, 200)))
    }
}

fn id_of(v: &Value) -> String {
    v.get("id")
        .map(|i| match i {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> &'a str {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or_default()
}

/// Best-effort MIME type from a filename extension. Falls back to application/octet-stream.
fn mime_from_filename(name: &str) -> String {
    match name
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md" | "markdown") => "text/markdown".into(),
        Some("txt") => "text/plain".into(),
        Some("json") => "application/json".into(),
        Some("pdf") => "application/pdf".into(),
        Some("png") => "image/png".into(),
        Some("jpg" | "jpeg") => "image/jpeg".into(),
        Some("gif") => "image/gif".into(),
        Some("svg") => "image/svg+xml".into(),
        Some("html" | "htm") => "text/html".into(),
        Some("xml") => "application/xml".into(),
        Some("csv") => "text/csv".into(),
        Some("zip") => "application/zip".into(),
        Some("gz" | "tgz") => "application/gzip".into(),
        Some("yaml" | "yml") => "text/yaml".into(),
        _ => "application/octet-stream".into(),
    }
}

/// Char-safe truncation (byte slicing would panic mid-UTF-8 on a Jira description).
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}… [truncated]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("héllo wörld", 5), "héllo… [truncated]");
    }

    #[test]
    fn id_of_handles_string_and_number() {
        assert_eq!(id_of(&json!({"id": "10001"})), "10001");
        assert_eq!(id_of(&json!({"id": 10001})), "10001");
        assert_eq!(id_of(&json!({})), "");
    }
}
