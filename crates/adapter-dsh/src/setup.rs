//! Whether DSH on this computer can answer at all: a model provider needs an
//! API key, and a DSH nobody set up has none. DSH only says so when a turn
//! fails (`MISSING_CREDENTIAL`); asking up front lets the phone tell the
//! person to set it up on the computer before they type anything.
//!
//! The key a provider reads is its settings profile's `apiKeyEnv`
//! (`llm.providers` names the settings namespace and path, `settings.describe`
//! has the value); `credentials.describe` says whether that key is stored or
//! in the environment. Anything that cannot be read counts as set up.

use std::collections::HashMap;

use serde_json::{Value, json};
use tether_proto::ModelInfo;

use crate::{api::Api, translate::str_at};

/// A provider whose API key is missing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MissingKey {
    pub name: String,
    pub env: String,
}

/// Providers (by id) that have no API key, among `providers`.
pub(crate) async fn missing_keys(
    api: &dyn Api,
    providers: &[String],
) -> HashMap<String, MissingKey> {
    let mut out = HashMap::new();
    let Ok(list) = api.call("llm.providers", json!({})).await else {
        return out;
    };
    let Ok(settings) = api.call("settings.describe", json!({})).await else {
        return out;
    };
    // provider → (display name, key name)
    let mut keys: Vec<(String, String, String)> = Vec::new();
    for entry in list
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = str_at(entry, &["provider"]) else {
            continue;
        };
        if !providers.iter().any(|p| p == id) {
            continue;
        }
        if let Some(env) = key_env(entry, &settings) {
            let name = str_at(entry, &["displayName"]).unwrap_or(id);
            keys.push((id.to_owned(), name.to_owned(), env));
        }
    }
    if keys.is_empty() {
        return out;
    }
    let mut refs: Vec<&str> = keys.iter().map(|(_, _, env)| env.as_str()).collect();
    refs.sort_unstable();
    refs.dedup();
    let Ok(described) = api
        .call("credentials.describe", json!({ "refs": refs }))
        .await
    else {
        return out;
    };
    for (id, name, env) in keys {
        let configured = described
            .get("credentials")
            .and_then(|c| c.get(&env))
            .and_then(|c| c.get("configured"))
            .and_then(Value::as_bool);
        if configured == Some(false) {
            out.insert(id, MissingKey { name, env });
        }
    }
    out
}

/// The `apiKeyEnv` of a provider's settings profile.
fn key_env(entry: &Value, settings: &Value) -> Option<String> {
    let ns = str_at(entry, &["settingsNs"])?;
    let namespace = settings
        .get("namespaces")?
        .as_array()?
        .iter()
        .find(|n| str_at(n, &["ns"]) == Some(ns))?;
    let mut value = namespace.get("value")?;
    for key in entry.get("settingsPath")?.as_array()? {
        value = value.get(key.as_str()?)?;
    }
    str_at(value, &["apiKeyEnv"])
        .filter(|e| !e.is_empty())
        .map(str::to_owned)
}

/// What to tell the person when DSH has no usable model.
pub(crate) fn not_set_up(missing: Option<&MissingKey>) -> String {
    let what = match missing {
        Some(m) => format!("{} 的 API Key（{}）还没有配置", m.name, m.env),
        None => "还没有可用的模型".to_owned(),
    };
    let env = missing.map(|m| m.env.as_str()).unwrap_or("对应的");
    format!(
        "电脑上的 DSH 还不能用：{what}。请在电脑上配置模型后再来：在电脑上运行 `dsh web`，在网页的「模型」（Models）页填好 API Key；或者设置环境变量 {env} 后重新打开 Tether。"
    )
}

/// The models, with those whose provider lacks a key marked; an error when
/// none can answer.
pub(crate) fn usable(
    mut models: Vec<ModelInfo>,
    missing: &HashMap<String, MissingKey>,
) -> anyhow::Result<Vec<ModelInfo>> {
    let provider_of = |m: &ModelInfo| m.id.split('/').next().unwrap_or_default().to_owned();
    if models.is_empty() {
        anyhow::bail!(not_set_up(None));
    }
    if models.iter().all(|m| missing.contains_key(&provider_of(m))) {
        let current = models
            .iter()
            .find(|m| m.is_default)
            .or(models.first())
            .map(provider_of)
            .unwrap_or_default();
        anyhow::bail!(not_set_up(missing.get(&current)));
    }
    for m in &mut models {
        if let Some(key) = missing.get(&provider_of(m)) {
            let note = format!("电脑上还没有配置 {} 的 API Key", key.env);
            m.description = Some(match m.description.take() {
                Some(provider) => format!("{provider} · {note}"),
                None => note,
            });
        }
    }
    Ok(models)
}

/// A turn that failed for want of a key (`MISSING_CREDENTIAL`), explained.
pub(crate) fn missing_credential(message: &str) -> String {
    // "…; store DEEPSEEK_API_KEY through the credentials service …"
    let env = message
        .split_once("store ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    let provider = message
        .split_once("provider route \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(p, _)| p);
    let missing = env.map(|env| MissingKey {
        name: provider.unwrap_or("模型服务商").to_owned(),
        env: env.to_owned(),
    });
    not_set_up(missing.as_ref())
}
