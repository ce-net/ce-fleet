//! `ce-fleet-enroll` — run the enrollment + rollup delegate service for one hospital site.
//!
//! It holds an org-root-anchored capability chain (`--cap`, audience = this delegate) and turns
//! one-time, nonce-tracked bootstrap enrollments into per-node working caps, while serving the
//! admin swarm console's rollup over its subtree. App over ce-rs; no node changes.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use ce_identity::Identity;
use ce_rs::CeClient;
use clap::Parser;

use ce_fleet_enroll::{
    BootstrapPolicy, NonceLedger,
    service::{AppState, DelegateConfig, load_delegate_chain, router},
};

#[derive(Parser)]
#[command(
    name = "ce-fleet-enroll",
    version,
    about = "CE fleet enrollment + rollup delegate (app over ce-rs + ce-cap; no node changes)"
)]
struct Cli {
    /// Local CE node API URL.
    #[arg(long, default_value = "http://127.0.0.1:8844")]
    node: String,
    /// CE data dir holding the delegate identity that SIGNS working caps.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// The org-root-issued capability chain this delegate holds (hex token, audience = delegate).
    /// Read from `--cap` or `$CE_FLEET_DELEGATE_CAP`.
    #[arg(long, env = "CE_FLEET_DELEGATE_CAP")]
    cap: String,
    /// Shared bootstrap secret a node must present at `/enroll` (Tailscale-authkey analogue).
    /// Read from `--bootstrap-secret` or `$CE_FLEET_BOOTSTRAP_SECRET` (keep it out of argv in prod).
    #[arg(long, env = "CE_FLEET_BOOTSTRAP_SECRET")]
    bootstrap_secret: String,
    /// Fleet tag every node enrolled through this delegate is scoped to (e.g. `radiology`).
    #[arg(long)]
    tag: Option<String>,
    /// Abilities granted to each enrolled node (comma-separated), e.g. `status,infer:chat`.
    #[arg(long, default_value = "status,infer:chat,infer:summarize")]
    abilities: String,
    /// Working-cap lifetime in seconds (clamped to the delegate's own expiry). Default 30 days.
    #[arg(long, default_value_t = 30 * 24 * 3600)]
    working_ttl_secs: u64,
    /// Address to bind the delegate HTTP service on.
    #[arg(long, default_value = "0.0.0.0:8855")]
    bind: String,
    /// Seconds before a node's atlas last-seen marks it offline in the rollup.
    #[arg(long, default_value_t = 180)]
    fresh_window_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let data_dir = cli
        .data_dir
        .clone()
        .unwrap_or_else(|| dirs_data_dir().join("ce"));

    let identity = Identity::load_or_generate(&data_dir.join("identity"))
        .context("load delegate identity")?;

    let delegate_chain = load_delegate_chain(&cli.cap)?;
    let parent = delegate_chain
        .last()
        .context("delegate cap chain is empty")?;
    if parent.cap.audience != identity.node_id() {
        bail!(
            "the --cap chain's audience is not this delegate's node id {} — pass a cap issued TO this delegate",
            identity.node_id_hex()
        );
    }

    let client = CeClient::new(cli.node.clone());
    if !client.health().await.unwrap_or(false) {
        tracing::warn!(node = %cli.node, "local CE node not reachable yet — /fleet/rollup will fail until `ce start` is up");
    }

    let abilities: Vec<String> = cli
        .abilities
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let policy = BootstrapPolicy {
        bootstrap_secret: cli.bootstrap_secret.clone(),
        tag: cli.tag.clone(),
        abilities,
        working_ttl_secs: cli.working_ttl_secs,
        bootstrap_ttl_secs: 0,
        enroll_window_start: 0,
    };

    let config = DelegateConfig {
        identity,
        delegate_chain,
        policy,
        fresh_window_secs: cli.fresh_window_secs,
    };

    let state = AppState {
        client: Arc::new(client),
        config: Arc::new(config),
        nonces: Arc::new(NonceLedger::new(3600)),
    };

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(&cli.bind)
        .await
        .with_context(|| format!("bind {}", cli.bind))?;
    tracing::info!(bind = %cli.bind, tag = ?cli.tag, "ce-fleet-enroll delegate up");
    axum::serve(listener, app).await.context("serve")?;
    Ok(())
}

/// Platform data dir without pulling an extra crate: honor `$XDG_DATA_HOME` / `%LOCALAPPDATA%`,
/// else fall back under `$HOME`.
fn dirs_data_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME")
        && !x.is_empty()
    {
        return PathBuf::from(x);
    }
    if cfg!(windows)
        && let Ok(x) = std::env::var("LOCALAPPDATA")
        && !x.is_empty()
    {
        return PathBuf::from(x);
    }
    if let Ok(home) = std::env::var("HOME") {
        // Build the path component-by-component (never a literal embedded separator) so it renders
        // with the platform separator; on unix this is byte-identical to `<home>/.local/share`.
        return PathBuf::from(home).join(".local").join("share");
    }
    PathBuf::from(".")
}
