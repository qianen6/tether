//! `tether` — the computer side without a window.
//!
//! `serve` runs the host in the foreground; `pair`, `devices`, `revoke`,
//! `agents` and `locate` talk to the running `serve` through a local control
//! endpoint (`control.rs`).

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
        /// Also open the QR code in the browser (for when this terminal is
        /// not the one the person looks at, e.g. an AI agent's).
        #[arg(long, conflicts_with = "json")]
        open: bool,
    },
    /// List paired phones.
    Devices {
        /// Print JSON instead of a table (scripts).
        #[arg(long)]
        json: bool,
    },
    /// Remove a phone; it is disconnected at once.
    Revoke { device_id: String },
    /// List the agents, whether each can run, and the program chosen for it.
    Agents {
        /// Print JSON instead of a table (scripts).
        #[arg(long)]
        json: bool,
    },
    /// Choose the program an agent uses (checked before it is kept). For
    /// DSH this may also be the whole line a running `dsh web` printed.
    Locate {
        /// Agent id, as `tether agents` lists it.
        agent: String,
        /// The program, its folder, or (DSH) `http://127.0.0.1:PORT/?token=…`.
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        path: Option<String>,
        /// Forget the choice: Tether finds the program by itself again.
        #[arg(long)]
        clear: bool,
    },
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
        Command::Pair {
            minutes,
            json,
            open,
        } => {
            let data = control::call(data_dir, control::Request::Pair { minutes }).await?;
            if json {
                println!("{data}");
                return Ok(());
            }
            let qr = data["qr"].as_str().context("no QR code in reply")?;
            let expires_at = data["expires_at"].as_u64().unwrap_or(0);
            println!("{}", qr_for_terminal(qr)?);
            let left = expires_at.saturating_sub(tether_proto::now_ms()) / 1000;
            println!(
                "用手机 App 扫描上面的二维码。{} 分 {} 秒内有效。",
                left / 60,
                left % 60
            );
            if open {
                let page = data_dir.join(PAIR_PAGE);
                std::fs::write(&page, pair_page(qr, left)?)
                    .with_context(|| format!("cannot write {}", page.display()))?;
                open_in_browser(&page);
                println!("二维码也已在浏览器里打开：{}", page.display());
            }
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
        Command::Agents { json } => {
            let data = control::call(data_dir, control::Request::Agents).await?;
            if json {
                println!("{data}");
                return Ok(());
            }
            for a in data.as_array().cloned().unwrap_or_default() {
                let state = if a["available"] == true {
                    "可用".to_string()
                } else {
                    a["detail"].as_str().unwrap_or("不可用").to_string()
                };
                println!(
                    "  {:<14} {:<18} {state}",
                    a["id"].as_str().unwrap_or("?"),
                    a["name"].as_str().unwrap_or("?"),
                );
                if let Some(location) = a["location"].as_str() {
                    println!("  {:<14} 使用选择的位置：{location}", "");
                }
            }
        }
        Command::Locate { agent, path, .. } => {
            let request = control::Request::Locate {
                agent: agent.clone(),
                path: path.clone(),
            };
            let data = control::call(data_dir, request).await?;
            match path {
                Some(_) => println!(
                    "检查通过{}，{agent} 之后都用 {}",
                    data["version"]
                        .as_str()
                        .map(|v| format!("（{v}）"))
                        .unwrap_or_default(),
                    data["program"].as_str().unwrap_or("?"),
                ),
                None => println!("已改回自动查找 {agent}。"),
            }
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

/// The pairing page `pair --open` writes into the data directory.
const PAIR_PAGE: &str = "pair.html";

/// A page showing the pairing QR code, for the browser.
fn pair_page(qr: &str, seconds_left: u64) -> anyhow::Result<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(qr.as_bytes())?;
    let image = code
        .render::<svg::Color<'_>>()
        .min_dimensions(320, 320)
        .quiet_zone(true)
        .build();
    Ok(format!(
        r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>Tether 配对</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;flex-direction:column;align-items:center;margin-top:48px;color:#222}}</style>
</head><body>
<h1>用手机 App 扫描配对</h1>
{image}
<p>二维码 {} 分钟内有效，只能配对一台手机。过期后在电脑上重新运行 <code>tether pair --open</code>。</p>
</body></html>
"#,
        seconds_left.div_ceil(60)
    ))
}

/// Opens `path` with the default program; failing to is not an error (the
/// terminal shows the code too).
fn open_in_browser(path: &Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(error) = std::process::Command::new(program).arg(path).spawn() {
        tracing::warn!(%error, "cannot open {}", path.display());
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
                json: false,
                open: false
            }
        ));
    }

    #[test]
    fn locate_needs_a_path_or_clear() {
        let c = Cli::parse_from([
            "tether",
            "locate",
            "dsh",
            "dsh web: http://127.0.0.1:1/?token=x",
        ]);
        assert!(matches!(
            c.command,
            Command::Locate { ref agent, path: Some(_), clear: false } if agent == "dsh"
        ));
        let c = Cli::parse_from(["tether", "locate", "dsh", "--clear"]);
        assert!(matches!(
            c.command,
            Command::Locate {
                path: None,
                clear: true,
                ..
            }
        ));
        assert!(Cli::try_parse_from(["tether", "locate", "dsh"]).is_err());
        assert!(Cli::try_parse_from(["tether", "locate", "dsh", "x", "--clear"]).is_err());
    }

    #[test]
    fn pair_page_has_the_code() {
        let page = pair_page("tether1:abc", 125).unwrap();
        assert!(page.contains("<svg"));
        assert!(page.contains("3 分钟内有效"));
        assert!(Cli::try_parse_from(["tether", "pair", "--open", "--json"]).is_err());
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
