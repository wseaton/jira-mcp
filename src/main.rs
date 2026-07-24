//! `jira-mcp` — a small MCP server for JIRA Cloud, spoken over stdio.
//!
//! Why it exists: the official Atlassian MCP announces ~40 tools and answers in raw JIRA JSON, which
//! together cost tens of thousands of context tokens before you've read a single ticket. This one
//! carries nine tools and renders compact text (see [`render`]).
//!
//! Config is env-first with sane fallbacks — see [`jira_mcp::Config::load`]. `--check` verifies the
//! credentials resolve and the site answers, then exits: run that first, not the server, when
//! debugging an install.

use anyhow::{Context, Result};
use clap::Parser;
use jira_mcp::{Config, JiraClient, JiraMcp};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::sync::Arc;

/// A token-frugal MCP server for JIRA Cloud.
///
/// With no arguments it serves MCP over stdio, which is how an MCP client (Claude Code, Cursor, …)
/// expects to launch it. Humans want `--check`.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Verify credentials against the live site and exit, instead of serving.
    #[arg(long)]
    check: bool,

    /// Write the default config to ~/.config/jira-mcp/config.toml (never overwrites) and exit.
    #[arg(long)]
    write_config: bool,

    /// Read a token from stdin and store it in the OS keychain, then exit.
    #[arg(long)]
    set_token: bool,

    /// Remove the token from the OS keychain and exit.
    #[arg(long)]
    delete_token: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // These three run BEFORE config resolution: each of them exists to fix a config that doesn't
    // load yet, so requiring a working one would be a circle.
    if cli.write_config {
        return write_config();
    }
    if cli.set_token {
        return set_token();
    }
    if cli.delete_token {
        let account = jira_mcp::config::account()?;
        jira_mcp::keychain::delete(&account)?;
        println!("removed the token for {account} from the keychain");
        return Ok(());
    }

    let jira = Arc::new(JiraClient::new(Config::load()?));

    if cli.check {
        return check(&jira).await;
    }

    // stdout is the MCP transport — anything printed there corrupts the protocol, so logs go to stderr.
    eprintln!("jira-mcp: serving {} over stdio", jira.config().base);
    let service = JiraMcp::new(jira)
        .serve(stdio())
        .await
        .context("starting the MCP stdio service")?;
    service.waiting().await.context("serving MCP over stdio")?;
    Ok(())
}

/// Drop the shipped template where [`Config::load`] will find it. Refuses to clobber an existing
/// file: the whole value of this flag is that it's safe to run twice.
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
/// stdin rather than an argument on purpose: a token passed as `--set-token ATATT…` lands in your
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
    println!("verify with: jira-mcp --check");
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
