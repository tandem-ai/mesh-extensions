//! `mesh/pulse`: an example org extension with a server part (E12).
//!
//! - server part (the bot meshd mesh-control runs for it): every minute it
//!   counts a tick in `org/ticks`; a POST to its webhook URL is kept in
//!   `org/last-webhook` and answered 202;
//! - on people's devices: each change of its store is a line in meshd's log;
//!   agents get a `ticks` tool (through `mesh-ext`) that reads the store.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

#[cfg(target_arch = "wasm32")]
use extism_pdk::*;
use mesh_ext::{Event, ToolReply, WebhookReply, store};
use serde_json::{Value, json};

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle(input: String) -> FnResult<String> {
    let answer = match Event::parse(&input)? {
        Event::Cron { .. } => {
            tick()?;
            Value::Null
        }
        Event::Webhook { headers, body } => serde_json::to_value(webhook(&headers, body)?)?,
        Event::Tool { name, .. } if name == "ticks" => serde_json::to_value(ticks()?)?,
        Event::StoreChange { key, version, .. } => {
            mesh_ext::log(&format!("pulse: {key} is at version {version}"));
            Value::Null
        }
        _ => Value::Null,
    };
    Ok(answer.to_string())
}

/// One more tick; a write that lost a race to another tick tries again.
fn tick() -> Result<(), mesh_ext::HostError> {
    for _ in 0..3 {
        let current = store::get("org/ticks")?;
        let (n, version) = current
            .map(|e| (e.value.as_u64().unwrap_or(0), e.version))
            .unwrap_or((0, 0));
        match store::put("org/ticks", json!(n + 1), Some(version)) {
            Err(e) if e.code == "conflict" => continue,
            other => return other.map(drop),
        }
    }
    Ok(())
}

fn webhook(
    headers: &std::collections::BTreeMap<String, String>,
    body: String,
) -> Result<WebhookReply, mesh_ext::HostError> {
    let kind = headers.get("content-type").cloned().unwrap_or_default();
    store::put(
        "org/last-webhook",
        json!({ "content_type": kind, "body": body }),
        None,
    )?;
    Ok(WebhookReply {
        status: Some(202),
        body: Some("stored".into()),
    })
}

fn ticks() -> Result<ToolReply, mesh_ext::HostError> {
    let n = store::get("org/ticks")?.map_or(0, |e| e.value.as_u64().unwrap_or(0));
    let hook = store::get("org/last-webhook")?.map(|e| e.value["body"].to_string());
    Ok(ToolReply {
        text: format!(
            "{n} ticks; last webhook: {}",
            hook.as_deref().unwrap_or("none")
        ),
        is_error: false,
    })
}
