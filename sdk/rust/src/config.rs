//! For providers: an agent's JSON config edited through the door the way
//! mesh's own installer did it. Only our entries change; everything else
//! keeps its place.

use anyhow::{Context, Result, bail};

use crate::{InstallRequest, door};
use serde_json::{Map, Value};

/// Quotes a word for `sh -c` when it needs it.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+=:,@%".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `[MESH_HOME=… ]<mesh> hook <agent> <event>`, run by the agent's shell.
pub fn hook_command(req: &InstallRequest, agent: &str, event: &str) -> String {
    let mut cmd = String::new();
    if let Some(home) = &req.mesh_home {
        cmd.push_str(&format!("MESH_HOME={} ", shell_quote(home)));
    }
    cmd.push_str(&format!(
        "{} hook {agent} {event}",
        shell_quote(&req.mesh_bin)
    ));
    cmd
}

/// True for a hook command mesh wrote (any mesh path or home).
pub fn is_our_hook(command: &str, agent: &str) -> bool {
    let words: Vec<String> = command
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '\'' || c == '"').to_string())
        .collect();
    words.windows(3).any(|w| {
        let bin = w[0].rsplit('/').next().unwrap_or("");
        bin == "mesh" && w[1] == "hook" && w[2] == agent
    })
}

/// True when an MCP server entry runs `mesh mcp`.
pub fn is_our_mcp(entry: &Value) -> bool {
    let bin = entry
        .get("command")
        .and_then(Value::as_str)
        .and_then(|c| c.rsplit('/').next())
        .unwrap_or("");
    bin == "mesh"
}

/// The MCP server entry for JSON configs.
pub fn mcp_entry(req: &InstallRequest, extra: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    for (k, v) in extra.iter().filter(|(k, _)| *k == "type") {
        m.insert(k.to_string(), v.clone());
    }
    m.insert("command".into(), Value::String(req.mesh_bin.clone()));
    m.insert(
        "args".into(),
        Value::Array(vec![Value::String("mcp".into())]),
    );
    if let Some(home) = &req.mesh_home {
        let mut env = Map::new();
        env.insert("MESH_HOME".into(), Value::String(home.clone()));
        m.insert("env".into(), Value::Object(env));
    }
    for (k, v) in extra.iter().filter(|(k, _)| *k != "type") {
        m.insert(k.to_string(), v.clone());
    }
    Value::Object(m)
}

/// Reads a JSON object file through the door; missing or empty is `{}`.
pub fn read(path: &str) -> Result<Value> {
    match door::read(path)? {
        Some(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Some(text) => {
            let v: Value = serde_json::from_str(&text)
                .with_context(|| format!("{path} is not valid JSON; not touching it"))?;
            if !v.is_object() {
                bail!("{path} is not a JSON object; not touching it");
            }
            Ok(v)
        }
        None => Ok(Value::Object(Map::new())),
    }
}

/// Writes `value` (pretty, with a final newline) when it differs.
pub fn write(path: &str, value: &Value) -> Result<bool> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(door::write(path, &text)?)
}

/// `obj[key]` as an object, created if missing.
pub fn object_at<'a>(obj: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>> {
    let map = obj.as_object_mut().context("expected a JSON object")?;
    let slot = map
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    slot.as_object_mut()
        .with_context(|| format!("`{key}` is not an object"))
}

/// Claude hook shape: `hooks.<Event> = [{matcher?, hooks: [{type, command,
/// timeout}]}]`. Puts our handler in place (or appends a group).
pub fn set_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    agent: &str,
    handler: Value,
    matcher: Option<&str>,
) -> Result<()> {
    let groups = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
        .with_context(|| format!("hooks.{event} is not an array"))?;
    let mut placed = false;
    for group in groups.iter_mut() {
        let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut i = 0;
        while i < handlers.len() {
            let ours = handlers[i]
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| is_our_hook(c, agent));
            if ours && !placed {
                handlers[i] = handler.clone();
                placed = true;
                i += 1;
            } else if ours {
                handlers.remove(i);
            } else {
                i += 1;
            }
        }
    }
    groups.retain(|g| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|h| !h.is_empty())
    });
    if !placed {
        let mut group = serde_json::json!({ "hooks": [handler] });
        if let Some(m) = matcher {
            group["matcher"] = m.into();
        }
        groups.push(group);
    }
    Ok(())
}

fn remove_from_groups(groups: &mut Value, agent: &str) {
    let Some(groups) = groups.as_array_mut() else {
        return;
    };
    for group in groups.iter_mut() {
        if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            handlers.retain(|h| {
                !h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| is_our_hook(c, agent))
            });
        }
    }
    groups.retain(|g| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|h| !h.is_empty())
    });
}

/// Removes our handlers from `events` only; drops emptied groups and events.
pub fn remove_old_hooks(hooks: &mut Map<String, Value>, events: &[&str], agent: &str) {
    for event in events {
        let Some(groups) = hooks.get_mut(*event) else {
            continue;
        };
        remove_from_groups(groups, agent);
        if groups.as_array().is_some_and(Vec::is_empty) {
            hooks.remove(*event);
        }
    }
}

/// Removes our handlers from every event; drops emptied groups/events.
pub fn remove_hooks(root: &mut Value, agent: &str) {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for groups in hooks.values_mut() {
        remove_from_groups(groups, agent);
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty() {
        root.as_object_mut().unwrap().remove("hooks");
    }
}

/// Removes `mcpServers.mesh` when it is ours; drops an emptied table.
pub fn remove_mcp(root: &mut Value) -> bool {
    let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return false;
    };
    if !servers.get("mesh").is_some_and(is_our_mcp) {
        return false;
    }
    servers.remove("mesh");
    if servers.is_empty() {
        root.as_object_mut().unwrap().remove("mcpServers");
    }
    true
}
