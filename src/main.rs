//! `jira-mcp` — a small MCP server for JIRA Cloud, and the same nine tools as a CLI.
//!
//! Why it exists: the official Atlassian MCP announces ~40 tools and answers in raw JIRA JSON, which
//! together cost tens of thousands of context tokens before you've read a single ticket. This one
//! carries nine tools and renders compact text (see [`jira_mcp::render`]).
//!
//! With no subcommand it serves MCP over stdio, which is how a client launches it. Every subcommand
//! is the same operation an MCP tool exposes, over the same [`jira_mcp::ops`] code, printing the same
//! bytes — so an agent can pipe, grep, and loop over `jira-mcp` in a shell script instead of making
//! one tool call per issue, and get exactly what the tool would have returned.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use jira_mcp::{Config, JiraClient, JiraMcp, ops, ops::IssueFields};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::sync::Arc;

/// A token-frugal JIRA Cloud client: an MCP server, and the same tools as a CLI.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve MCP over stdio (the default when no subcommand is given).
    Serve,

    /// Verify credentials against the live site and report where each setting came from.
    Check,

    /// Write the default config to ~/.config/jira-mcp/config.toml. Never overwrites.
    WriteConfig,

    /// Read a JIRA API token from stdin and store it in the OS keychain.
    SetToken,

    /// Remove the token from the OS keychain.
    DeleteToken,

    /// Search by JQL. One compact line per issue.
    Search {
        /// The JQL, e.g. 'project = PROJ AND status = Open ORDER BY updated DESC'.
        jql: String,
        /// Max issues (clamped to [1,100]).
        #[arg(short, long, default_value_t = 25)]
        limit: u32,
        /// Print the raw JIRA rows instead of the compact rendering.
        #[arg(long)]
        json: bool,
    },

    /// Show one issue.
    Issue {
        /// Issue key, e.g. PROJ-142.
        key: String,
        /// Include the comment thread.
        #[arg(short, long)]
        comments: bool,
        /// Cut each description/comment at this many chars.
        #[arg(long, default_value_t = 6000)]
        max_chars: usize,
        /// Print the full raw issue instead of the compact rendering.
        #[arg(long)]
        json: bool,
    },

    /// Read an issue's comment thread (newest N, printed oldest-first).
    Comments {
        key: String,
        #[arg(short, long, default_value_t = 25)]
        limit: u32,
        #[arg(long, default_value_t = 6000)]
        max_chars: usize,
        #[arg(long)]
        json: bool,
    },

    /// Post a comment. Pass `-` as the body to read it from stdin.
    Comment {
        key: String,
        /// The comment body, or `-` to read stdin (which is how you post a heredoc).
        body: String,
    },

    /// Create an issue.
    Create {
        /// Project key.
        #[arg(short, long)]
        project: String,
        /// Issue type NAME, e.g. Story, Bug, Epic.
        #[arg(short = 't', long = "type")]
        issue_type: String,
        /// Summary line.
        #[arg(short, long)]
        summary: String,
        /// Description, or `-` to read stdin.
        #[arg(short, long)]
        description: Option<String>,
        /// Parent issue key (an epic, in a company-managed project).
        #[arg(long)]
        parent: Option<String>,
        /// A label. Repeat for several.
        #[arg(short, long)]
        label: Vec<String>,
        /// Raw fields JSON, merged last: '{"customfield_10001":{"id":"…"}}'.
        #[arg(long)]
        fields: Option<String>,
    },

    /// Edit an issue's fields in place.
    Update {
        key: String,
        #[arg(short, long)]
        summary: Option<String>,
        /// New description, or `-` to read stdin.
        #[arg(short, long)]
        description: Option<String>,
        /// A label. Repeat for several. Replaces the existing set.
        #[arg(short, long)]
        label: Vec<String>,
        /// Raw fields JSON, merged last.
        #[arg(long)]
        fields: Option<String>,
    },

    /// Move an issue to another status. Omit the target to list what's available.
    Transition {
        key: String,
        /// Transition or status NAME (case-insensitive).
        to: Option<String>,
    },

    /// Link two issues. Omit everything to list the site's link types.
    Link {
        /// Link type name, e.g. Blocks.
        link_type: Option<String>,
        /// The inward issue (for Blocks: the blocker).
        inward: Option<String>,
        /// The outward issue (for Blocks: the blocked one).
        outward: Option<String>,
    },

    /// Look up field ids by name substring, for the --fields escape hatch.
    Fields {
        /// Case-insensitive substring. Omit to list every field.
        #[arg(default_value = "")]
        query: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // These three run BEFORE config resolution: each exists to fix a config that doesn't load yet,
    // so requiring a working one would be a circle.
    match &cli.command {
        Some(Command::WriteConfig) => return write_config(),
        Some(Command::SetToken) => return set_token(),
        Some(Command::DeleteToken) => {
            let account = jira_mcp::config::account()?;
            jira_mcp::keychain::delete(&account)?;
            println!("removed the token for {account} from the keychain");
            return Ok(());
        }
        _ => {}
    }

    let jira = Arc::new(JiraClient::new(Config::load()?));

    let out = match cli.command {
        None | Some(Command::Serve) => return serve(jira).await,
        Some(Command::Check) => return check(&jira).await,
        Some(Command::Search { jql, limit, json }) => ops::search(&jira, &jql, limit, json).await?,
        Some(Command::Issue {
            key,
            comments,
            max_chars,
            json,
        }) => ops::issue(&jira, &key, comments, max_chars, json).await?,
        Some(Command::Comments {
            key,
            limit,
            max_chars,
            json,
        }) => ops::comments(&jira, &key, limit, max_chars, json).await?,
        Some(Command::Comment { key, body }) => {
            ops::add_comment(&jira, &key, &or_stdin(body)?).await?
        }
        Some(Command::Create {
            project,
            issue_type,
            summary,
            description,
            parent,
            label,
            fields,
        }) => {
            ops::create_issue(
                &jira,
                &project,
                &issue_type,
                summary,
                parent,
                IssueFields {
                    description: description.map(or_stdin).transpose()?,
                    labels: labels(label),
                    extra: parse_fields(fields)?,
                    ..Default::default()
                },
            )
            .await?
        }
        Some(Command::Update {
            key,
            summary,
            description,
            label,
            fields,
        }) => {
            ops::update_issue(
                &jira,
                &key,
                IssueFields {
                    summary,
                    description: description.map(or_stdin).transpose()?,
                    labels: labels(label),
                    extra: parse_fields(fields)?,
                },
            )
            .await?
        }
        Some(Command::Transition { key, to }) => {
            ops::transition(&jira, &key, to.as_deref()).await?
        }
        Some(Command::Link {
            link_type,
            inward,
            outward,
        }) => {
            ops::link(
                &jira,
                link_type.as_deref(),
                inward.as_deref(),
                outward.as_deref(),
            )
            .await?
        }
        Some(Command::Fields { query }) => ops::fields(&jira, &query).await?,
        // Handled above, before the config load.
        Some(Command::WriteConfig | Command::SetToken | Command::DeleteToken) => unreachable!(),
    };
    println!("{}", out.trim_end());
    Ok(())
}

async fn serve(jira: Arc<JiraClient>) -> Result<()> {
    // stdout is the MCP transport — anything printed there corrupts the protocol, so logs go to stderr.
    eprintln!("jira-mcp: serving {} over stdio", jira.config().base);
    let service = JiraMcp::new(jira)
        .serve(stdio())
        .await
        .context("starting the MCP stdio service")?;
    service.waiting().await.context("serving MCP over stdio")?;
    Ok(())
}

/// `-` means "read it from stdin", the usual convention, and the only sane way to pass a multi-line
/// description or comment from a shell.
fn or_stdin(value: String) -> Result<String> {
    if value != "-" {
        return Ok(value);
    }
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).context("reading stdin")?;
    Ok(buf.trim_end().to_string())
}

/// An empty `--label` list means "don't touch labels", not "clear them".
fn labels(v: Vec<String>) -> Option<Vec<String>> {
    (!v.is_empty()).then_some(v)
}

fn parse_fields(raw: Option<String>) -> Result<Option<serde_json::Value>> {
    raw.map(|s| serde_json::from_str(&s).context("parsing --fields as JSON"))
        .transpose()
}

/// Drop the shipped template where [`Config::load`] will find it. Refuses to clobber an existing
/// file: the whole value of this command is that it's safe to run twice.
fn write_config() -> Result<()> {
    let path = jira_mcp::config::config_path().context("no config directory (is HOME set?)")?;
    if path.exists() {
        println!("config already exists at {} (untouched)", path.display());
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, jira_mcp::config::default_config_toml())
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Store a token from stdin in the OS keychain, filed under the account email.
///
/// stdin rather than an argument on purpose: a token passed as `set-token ATATT…` lands in your
/// shell history and in `ps` output for every other user on the box.
fn set_token() -> Result<()> {
    let account = jira_mcp::config::account()?;
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("paste the JIRA API token for {account}, then press ctrl-d:");
    }
    let mut token = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut token).context("reading stdin")?;
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("no token on stdin");
    }
    jira_mcp::keychain::set(&account, token)?;
    println!("stored the token for {account} in the keychain");
    println!("verify with: jira-mcp check");
    Ok(())
}

/// Print the resolved config and prove the credentials work. The field count is a cheap authenticated
/// call that every account can make, so a failure here is genuinely auth and not permissions.
async fn check(jira: &JiraClient) -> Result<()> {
    let cfg = jira.config();
    println!("site:    {}", cfg.base);
    println!("user:    {}", cfg.email);
    println!("access:  {}", cfg.access);
    println!("token:   {}", cfg.token_source);
    println!("fields:  {} curated custom fields", cfg.custom_fields.len());
    let fields = jira.fields().await.context("calling JIRA (auth check)")?;
    println!("auth:    ok ({} fields visible)", fields.len());
    Ok(())
}
