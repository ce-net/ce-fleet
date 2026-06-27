//! `ce-fleet` — own your ce-net device mesh from the terminal (and a tiny local web UI).
//!
//!   ce-fleet daemon                 # run the pairing daemon + admin UI (installed as a ceapp)
//!   ce-fleet nodes                  # list nearby / owned nodes
//!   ce-fleet pair <node|name>       # ask another device to pair (it shows an Accept prompt)
//!   ce-fleet pending                # incoming pairing requests on this device
//!   ce-fleet accept <id> | deny <id>
//!   ce-fleet name <name>            # claim a human-readable name for this node

use anyhow::{anyhow, Context, Result};
use ce_identity::Identity;
use ce_rs::CeClient;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use ce_fleet_cli::daemon::DaemonState;
use ce_fleet_cli::proto::default_abilities;
use ce_fleet_cli::util::{data_dir, device_label};
use ce_fleet_cli::{daemon, pair};

#[derive(Parser)]
#[command(name = "ce-fleet", version, about = "Own your ce-net device mesh: discover, pair, grant")]
struct Cli {
    /// Local CE node HTTP API.
    #[arg(long, global = true, env = "CE_NODE_URL", default_value = "http://127.0.0.1:8844")]
    node: String,
    /// Override the CE data directory (identity + wallet live here).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Port for the daemon's local admin UI (and where `pending`/`accept`/`deny` talk to it).
    #[arg(long, global = true, default_value_t = 8975)]
    ui_port: u16,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the pairing daemon and admin UI (foreground; the ce service supervises it).
    Daemon,
    /// List nearby / owned nodes seen on the mesh.
    Nodes {
        #[arg(long)]
        json: bool,
    },
    /// Ask another device to pair — its operator gets an Accept/Deny prompt. On accept, the issued
    /// capability is stored in this device's wallet.
    Pair {
        /// Target node id (64 hex) or a claimed human name.
        target: String,
        /// Abilities to request (comma-separated). Default: full admin.
        #[arg(long, value_delimiter = ',')]
        abilities: Vec<String>,
        /// Capability lifetime, e.g. 90d / 24h / 30m / 3600s, or `never`.
        #[arg(long, default_value = "never")]
        ttl: String,
        /// Also ask to be added to the peer's roots (owner; effective after its ce service restart).
        #[arg(long)]
        roots: bool,
        /// Label shown in the peer's prompt (default: this machine's hostname).
        #[arg(long)]
        label: Option<String>,
    },
    /// Show pending incoming pairing requests on this device.
    Pending,
    /// Accept a pending request by id.
    Accept { id: u64 },
    /// Deny a pending request by id.
    Deny { id: u64 },
    /// Claim a human-readable name for this node (on-chain; first claim wins).
    Name { name: String },
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
    let dir = data_dir(cli.data_dir.clone());
    let ce = CeClient::new(&cli.node);

    match cli.cmd {
        Cmd::Daemon => {
            let identity = Identity::load_or_generate(&dir.join("identity"))
                .context("loading node identity")?;
            tracing::info!(node = %identity.node_id_hex(), "ce-fleet daemon starting");
            let state = DaemonState::new(identity, dir, ce);
            daemon::run(state, cli.ui_port).await?;
        }
        Cmd::Nodes { json } => cmd_nodes(&ce, json).await?,
        Cmd::Pair {
            target,
            abilities,
            ttl,
            roots,
            label,
        } => {
            let identity = Identity::load_or_generate(&dir.join("identity"))
                .context("loading node identity")?;
            let target_hex = resolve_target(&ce, &target).await?;
            let abilities = if abilities.is_empty() {
                default_abilities()
            } else {
                abilities
            };
            let ttl_secs = parse_ttl(&ttl)?;
            let label = label.unwrap_or_else(|| device_label(&identity.node_id_hex()));
            println!("Requesting pairing with {target_hex} ...");
            println!("  (approve it in the ce-fleet UI / `ce-fleet accept` on that device)");
            let o = pair::initiate(
                &ce, &identity, &dir, &target_hex, abilities, ttl_secs, roots, &label,
            )
            .await?;
            if o.accepted {
                let alias = o.alias.unwrap_or_default();
                println!("Paired. Capability stored in wallet as '{alias}'.");
                println!("  abilities: {}", o.granted.join(", "));
                println!(
                    "  expires:   {}",
                    if o.not_after == 0 {
                        "never".to_string()
                    } else {
                        format!("unix {}", o.not_after)
                    }
                );
                if o.rooted {
                    println!("  also added to the peer's roots (effective after its ce restart).");
                }
            } else {
                println!("Not paired: {}", o.reason.unwrap_or_else(|| "declined".into()));
            }
        }
        Cmd::Pending => cmd_pending(cli.ui_port).await?,
        Cmd::Accept { id } => cmd_decide(cli.ui_port, id, true).await?,
        Cmd::Deny { id } => cmd_decide(cli.ui_port, id, false).await?,
        Cmd::Name { name } => {
            ce.claim_name(&name).await.context("claiming name")?;
            println!("Claimed '{name}' (takes effect once mined).");
        }
    }
    Ok(())
}

async fn cmd_nodes(ce: &CeClient, json: bool) -> Result<()> {
    let me = ce.status().await.map(|s| s.node_id).unwrap_or_default();
    let atlas = ce.atlas().await.context("fetching atlas")?;
    if json {
        // AtlasEntry isn't Serialize, so project it into JSON explicitly.
        let out: Vec<serde_json::Value> = atlas
            .iter()
            .map(|e| {
                serde_json::json!({
                    "node_id": e.node_id,
                    "is_self": e.node_id == me,
                    "cpu_cores": e.cpu_cores,
                    "mem_mb": e.mem_mb,
                    "running_jobs": e.running_jobs,
                    "last_seen_secs": e.last_seen_secs,
                    "tags": e.tags,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if atlas.is_empty() {
        println!("No nodes seen yet. Make sure `ce start` is running.");
        return Ok(());
    }
    let now = ce_fleet_cli::util::now_secs();
    println!("{:<66}  {:>5}  {:>6}  TAGS", "NODE", "CPU", "MEM");
    for e in atlas {
        let online = now.saturating_sub(e.last_seen_secs) < 120;
        let mark = if e.node_id == me {
            " (this machine)"
        } else if online {
            ""
        } else {
            " (offline)"
        };
        println!(
            "{:<66}  {:>5}  {:>5}G  {}{}",
            e.node_id,
            e.cpu_cores,
            e.mem_mb / 1024,
            e.tags.join(","),
            mark
        );
    }
    Ok(())
}

async fn cmd_pending(ui_port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{ui_port}/api/pending");
    let reqs: serde_json::Value = reqwest::get(&url)
        .await
        .and_then(|r| r.error_for_status())
        .context("is the ce-fleet daemon running? (`ce-fleet daemon`)")?
        .json()
        .await?;
    let arr = reqs.as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        println!("No pending pairing requests.");
        return Ok(());
    }
    for r in arr {
        println!(
            "[{}] {} ({})\n     abilities: {}\n",
            r["id"],
            r["label"].as_str().unwrap_or("?"),
            r["from_node"].as_str().unwrap_or("?"),
            r["abilities"]
                .as_array()
                .map(|a| a
                    .iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default(),
        );
    }
    println!("Accept with: ce-fleet accept <id>");
    Ok(())
}

async fn cmd_decide(ui_port: u16, id: u64, accept: bool) -> Result<()> {
    let url = format!("http://127.0.0.1:{ui_port}/api/decide");
    let resp: serde_json::Value = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "id": id, "accept": accept }))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("is the ce-fleet daemon running? (`ce-fleet daemon`)")?
        .json()
        .await?;
    if resp["ok"].as_bool().unwrap_or(false) {
        println!("{} request {id}.", if accept { "Accepted" } else { "Denied" });
    } else {
        println!("No such pending request {id} (already decided or expired).");
    }
    Ok(())
}

/// Resolve a target to a 64-hex node id: pass through hex, else resolve a claimed human name.
async fn resolve_target(ce: &CeClient, target: &str) -> Result<String> {
    let t = target.trim();
    if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(t.to_lowercase());
    }
    match ce.resolve_name(t).await.context("resolving name")? {
        Some(node) => Ok(node),
        None => Err(anyhow!(
            "'{t}' is neither a 64-hex node id nor a claimed name"
        )),
    }
}

/// Parse a TTL like `90d`, `24h`, `30m`, `3600s`, or `never`/`0` -> seconds (`0` = never).
fn parse_ttl(s: &str) -> Result<u64> {
    let s = s.trim().to_lowercase();
    if s.is_empty() || s == "never" || s == "0" {
        return Ok(0);
    }
    let (num, mult) = match s.chars().last().unwrap() {
        'd' => (&s[..s.len() - 1], 86_400),
        'h' => (&s[..s.len() - 1], 3_600),
        'm' => (&s[..s.len() - 1], 60),
        's' => (&s[..s.len() - 1], 1),
        c if c.is_ascii_digit() => (s.as_str(), 1),
        _ => return Err(anyhow!("bad ttl '{s}' (use 90d/24h/30m/3600s or never)")),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow!("bad ttl number in '{s}'"))?;
    Ok(n * mult)
}
