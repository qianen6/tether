//! `tether` — the computer side without a window.
//!
//! `serve` runs the host in the foreground; `pair`, `devices` and `revoke`
//! talk to the running `serve` through a local control endpoint
//! (`control.rs`).

mod control;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde_json::Value;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

#[derive(Debug, Parser)]
#[command(
    name = "tether",
    version,
    about = "Control the agents on this computer from your phone"
)]
struct Cli {
    /// Data directory (default: the platform's application data directory).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run in the foreground: keep the tunnel and agent sessions alive.
    Serve {
        /// Computer name shown on phones.
        #[arg(long)]
        name: Option<String>,
        /// Own DERP relay map (default: Tailcat's public relays).
        #[arg(long)]
        derp_map_url: Option<String>,
        /// The `tailpocket-host` executable (default: next to `tether`, then
        /// `vendor/tailpocket/bin/`).
        #[arg(long)]
        tailpocket_helper: Option<PathBuf>,
    },
    /// Show a QR code that pairs one phone.
    Pair {
        /// Minutes the code stays valid (at most 15).
        #[arg(long, default_value_t = 10)]
        minutes: u64,
        /// Print `{"qr":…,"expires_at":…}` instead of the QR code (scripts).
        #[arg(long)]
        json: bool,
    },
    /// List paired phones.
    Devices {
        /// Print JSON instead of a table (scripts).
        #[arg(long)]
        json: bool,
    },
    /// Remove a phone; it is disconnected at once.
    Revoke { device_id: String },
}

const LOG_FILE: &str = "tether.log";

fn filter(default: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into())
}

/// Terminal plus `data_dir/tether.log` for `serve`; the terminal only otherwise.
fn init_logging(log_file: Option<&Path>) -> anyhow::Result<()> {
    let terminal = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter(if log_file.is_some() { "info" } else { "warn" }));
    let file = match log_file {
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("cannot open log {}", path.display()))?;
            Some(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(Mutex::new(file))
                    .with_filter(filter("info")),
            )
        }
        None => None,
    };
    tracing_subscriber::registry()
        .with(terminal)
        .with(file)
        .try_init()?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(tether_host::default_data_dir);

    match cli.command {
        Command::Serve {
            name,
            derp_map_url,
            tailpocket_helper,
        } => {
            std::fs::create_dir_all(&data_dir)
                .with_context(|| format!("cannot create {}", data_dir.display()))?;
            init_logging(Some(&data_dir.join(LOG_FILE)))?;
            serve(data_dir, name, derp_map_url, tailpocket_helper).await
        }
        command => {
            init_logging(None)?;
            remote(&data_dir, command).await
        }
    }
}

async fn serve(
    data_dir: PathBuf,
    name: Option<String>,
    derp_map_url: Option<String>,
    tailpocket_helper: Option<PathBuf>,
) -> anyhow::Result<()> {
    if control::call(&data_dir, control::Request::Devices)
        .await
        .is_ok()
    {
        anyhow::bail!("tether serve is already running for {}", data_dir.display());
    }
    let host = tether_host::Host::start(tether_host::HostConfig {
        data_dir: data_dir.clone(),
        name: name.unwrap_or_default(),
        derp_map_url,
        tailpocket_helper,
        ..Default::default()
    })
    .await?;
    let host = Arc::new(host);
    let control = match control::Control::start(&data_dir, host.clone()).await {
        Ok(c) => c,
        Err(error) => {
            shutdown(host).await?;
            return Err(error);
        }
    };
    let status = host.status();
    println!("Tether 正在运行（数据目录 {}）", data_dir.display());
    println!(
        "网关 {}；另开一个终端运行 `tether pair` 配对手机。按 Ctrl+C 退出。",
        status.gateway
    );
    for agent in host.agents() {
        let state = if agent.available {
            "可用".to_string()
        } else {
            agent.detail.unwrap_or_else(|| "不可用".into())
        };
        println!("  {:<10} {:<16} {state}", agent.id, agent.name);
    }
    tokio::signal::ctrl_c().await?;
    println!("正在退出…");
    control.stop();
    shutdown(host).await
}

async fn shutdown(host: Arc<tether_host::Host>) -> anyhow::Result<()> {
    // The control tasks hold clones only while answering; wait for them.
    let mut host = host;
    for _ in 0..100 {
        match Arc::try_unwrap(host) {
            Ok(h) => return h.shutdown().await,
            Err(h) => host = h,
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("a control request is still running; exiting without a clean shutdown")
}

async fn remote(data_dir: &Path, command: Command) -> anyhow::Result<()> {
    match command {
        Command::Pair { minutes, json } => {
            let data = control::call(data_dir, control::Request::Pair { minutes }).await?;
            if json {
                println!("{data}");
                return Ok(());
            }
            let qr = data["qr"].as_str().context("no QR code in reply")?;
            println!("{}", qr_for_terminal(qr)?);
            let left = data["expires_at"]
                .as_u64()
                .unwrap_or(0)
                .saturating_sub(tether_proto::now_ms())
                / 1000;
            println!(
                "用手机 App 扫描上面的二维码。{} 分 {} 秒内有效。",
                left / 60,
                left % 60
            );
        }
        Command::Devices { json } => {
            let data = control::call(data_dir, control::Request::Devices).await?;
            if json {
                println!("{data}");
                return Ok(());
            }
            let devices = data.as_array().cloned().unwrap_or_default();
            if devices.is_empty() {
                println!("还没有配对的手机。");
            }
            for d in devices {
                println!(
                    "{}  {}  配对于 {}  最近在线 {}{}",
                    d["id"].as_str().unwrap_or("?"),
                    d["name"].as_str().unwrap_or("?"),
                    ago(&d["paired_at"]),
                    ago(&d["last_seen"]),
                    if d["linked"] == true {
                        ""
                    } else {
                        "  （未关联隧道密钥）"
                    },
                );
            }
        }
        Command::Revoke { device_id } => {
            control::call(
                data_dir,
                control::Request::Revoke {
                    device_id: device_id.clone(),
                },
            )
            .await?;
            println!("已移除 {device_id}，它的连接已断开。");
        }
        Command::Serve { .. } => unreachable!("handled by main"),
    }
    Ok(())
}

/// "5 分钟前" for a Unix-millisecond value.
fn ago(ms: &Value) -> String {
    let Some(ms) = ms.as_u64() else {
        return "从未".into();
    };
    let s = tether_proto::now_ms().saturating_sub(ms) / 1000;
    match s {
        0..60 => "刚刚".into(),
        60..3600 => format!("{} 分钟前", s / 60),
        3600..86400 => format!("{} 小时前", s / 3600),
        _ => format!("{} 天前", s / 86400),
    }
}

/// Renders `text` as a QR code made of Unicode half blocks for the terminal.
fn qr_for_terminal(text: &str) -> anyhow::Result<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(text.as_bytes())?;
    Ok(code.render::<Dense1x2>().quiet_zone(true).build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses() {
        Cli::command().debug_assert();
        let c = Cli::parse_from(["tether", "pair", "--minutes", "5"]);
        assert!(matches!(
            c.command,
            Command::Pair {
                minutes: 5,
                json: false
            }
        ));
    }

    #[test]
    fn qr_renders() {
        let q = qr_for_terminal("tether1:abc").unwrap();
        assert!(q.lines().count() > 10);
    }

    #[test]
    fn ages() {
        let now = tether_proto::now_ms();
        assert_eq!(ago(&Value::Null), "从未");
        assert_eq!(ago(&Value::from(now)), "刚刚");
        assert_eq!(ago(&Value::from(now - 5 * 60_000)), "5 分钟前");
        assert_eq!(ago(&Value::from(now - 3 * 86_400_000)), "3 天前");
    }
}
