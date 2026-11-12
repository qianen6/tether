//! Standalone std-only ACP test process, compiled by process_tests.rs. It is
//! deliberately not an installed or real agent and never opens the network.

use std::io::{self, BufRead, Write};

fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch < ' ' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn send(message: &str) {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{message}").unwrap();
    stdout.flush().unwrap();
}

fn result(id: u64, result: &str) {
    send(&format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#
    ));
}

// Only the client's generated numeric id is inspected. Protocol parsing in
// production is tested separately with serde_json and the memory mock agent.
fn request_id(line: &str) -> Option<u64> {
    line.split("\"id\":")
        .nth(1)?
        .split(|ch: char| !ch.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let mut prompt = None;
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.contains("\"method\":\"initialize\"") {
            if flag("--silent-initialize") {
                continue;
            }
            if flag("--stderr-flood") {
                eprintln!("{}STDERR-END", "stderr-data".repeat(20_000));
            }
            result(
                request_id(&line).unwrap(),
                r#"{"protocolVersion":1,"agentCapabilities":{},"authMethods":[]}"#,
            );
        } else if line.contains("\"method\":\"session/new\"") {
            result(
                request_id(&line).unwrap(),
                r#"{"sessionId":"mock-process"}"#,
            );
        } else if line.contains("\"method\":\"session/prompt\"") {
            let id = request_id(&line).unwrap();
            if flag("--crash") {
                eprintln!("MOCK-CRASH");
                std::process::exit(7);
            }
            let text = format!(
                "cwd={};env={};args={}",
                std::env::current_dir().unwrap().display(),
                std::env::var("TETHER_MOCK_ENV").unwrap_or_default(),
                args.join("|")
            );
            send(&format!(
                r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"mock-process","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":{}}}}}}}}}"#,
                quoted(&text)
            ));
            if flag("--wait-for-cancel") {
                prompt = Some(id);
                continue;
            }
            result(id, r#"{"stopReason":"end_turn"}"#);
            if flag("--exit-after-prompt") {
                return;
            }
        } else if line.contains("\"method\":\"session/cancel\"") {
            if let Some(id) = prompt.take() {
                result(id, r#"{"stopReason":"cancelled"}"#);
            }
        }
    }
}
