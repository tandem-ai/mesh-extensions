//! `mesh/codex`: Codex on mesh, as a provider extension. It used to be
//! `mesh-agents/src/install/codex.rs` and the Codex parts of `hooks.rs` in
//! mesh itself; it writes the same files, byte for byte.
//!
//! - install: hooks and the MCP server in `$CODEX_HOME/config.toml` (default
//!   `~/.codex`), edited with `toml_edit` so the user's comments and layout
//!   stay;
//! - hooks: which session event each Codex hook is, and what it prints.
//!
//! ```toml
//! [mcp_servers.mesh]
//! command = "/…/mesh"
//! args = ["mcp"]
//! default_tools_approval_mode = "approve"
//!
//! [[hooks.Stop]]
//! [[hooks.Stop.hooks]]
//! type = "command"
//! command = "/…/mesh hook codex stop"
//! timeout = 10
//!
//! [hooks.state."/…/.codex/config.toml:stop:0:0"]
//! trusted_hash = "sha256:…"
//! ```
//!
//! Codex (0.162) runs a config hook only once it is trusted: the
//! `hooks.state` entry keyed `<real path of config.toml>:<event>:<group>:<handler>`
//! must carry the hook's hash (`codex-rs/hooks/src/engine/discovery.rs`,
//! `hook_hash` + `codex-rs/config/src/fingerprint.rs`, `version_for_toml`).
//! `mesh install codex` is the user's consent, so it records that trust
//! for the hooks it writes, and only for those.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
#[cfg(target_arch = "wasm32")]
use mesh_ext::Event;
use mesh_ext::{HookParsed, HookRendered, InstallRequest, InstallResult, config, door};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, value};

/// (Codex event, trust-key label, our event name, timeout in seconds).
/// SessionEnd timeouts are capped at 3 s by Codex.
const EVENTS: &[(&str, &str, &str, u64)] = &[
    ("SessionStart", "session_start", "start", 10),
    ("UserPromptSubmit", "user_prompt_submit", "prompt", 10),
    ("Stop", "stop", "stop", 10),
    // Interrupt hooks get 1 s by default and 3 s at most.
    ("Interrupt", "interrupt", "idle", 3),
    ("SessionEnd", "session_end", "end", 3),
    ("PostToolUse", "post_tool_use", "step", 10),
    // Prints nothing (the person decides); tells meshd not to type in now.
    ("PermissionRequest", "permission_request", "blocked", 5),
];

/// Events older versions hooked: install drops our handler there (uninstall
/// drops ours everywhere). `PreToolUse` ran the team-rule check.
const OLD_EVENTS: &[&str] = &["PreToolUse"];

fn label_for(event: &str) -> Option<&'static str> {
    Some(match event {
        "PreToolUse" => "pre_tool_use",
        "PermissionRequest" => "permission_request",
        "PostToolUse" => "post_tool_use",
        "PreCompact" => "pre_compact",
        "PostCompact" => "post_compact",
        "SessionStart" => "session_start",
        "SessionEnd" => "session_end",
        "UserPromptSubmit" => "user_prompt_submit",
        "SubagentStart" => "subagent_start",
        "SubagentStop" => "subagent_stop",
        "Stop" => "stop",
        "Interrupt" => "interrupt",
        _ => return None,
    })
}

// The export exists in the WASM build only: host-side unit tests can't link
// Extism's imports, and test the logic below directly.
#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle(input: String) -> FnResult<String> {
    let answer = match Event::parse(&input)? {
        Event::Install(req) => serde_json::to_value(result(install(&req)))?,
        Event::Uninstall(req) => serde_json::to_value(result(uninstall(&req)))?,
        Event::Hook { name, payload, env } => serde_json::to_value(hook(&name, &payload, &env))?,
        Event::HookRender {
            kind,
            handover,
            intro,
        } => serde_json::to_value(HookRendered {
            stdout: render(&kind, handover, intro),
        })?,
        _ => Value::Null,
    };
    Ok(answer.to_string())
}

/// A config the person must fix is an answer, not a crash.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn result(r: Result<InstallResult>) -> InstallResult {
    r.unwrap_or_else(|e| InstallResult {
        error: Some(format!("{e:#}")),
        ..InstallResult::default()
    })
}

// ---------------------------------------------------------------- install

/// Codex's trust hash of a command hook with no matcher:
/// sha256 of the compact, key-sorted JSON of the normalized hook.
pub fn trusted_hash(label: &str, command: &str, timeout_sec: u64) -> String {
    let s = |v: &str| serde_json::to_string(v).expect("string");
    let identity = format!(
        "{{\"event_name\":{},\"hooks\":[{{\"async\":false,\"command\":{},\"timeout\":{timeout_sec},\"type\":\"command\"}}]}}",
        s(label),
        s(command)
    );
    let digest = Sha256::digest(identity.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// `dir` joined with `name` the way `PathBuf::join` does it, so the paths
/// mesh reports are the ones it used to.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn config_path(req: &InstallRequest) -> String {
    let env = req.env.get("CODEX_HOME").filter(|v| !v.is_empty());
    let dir = match (&req.config_dir, env) {
        (Some(d), _) => d.clone(),
        (None, Some(d)) => d.clone(),
        (None, None) => join(&req.home, ".codex"),
    };
    join(&dir, "config.toml")
}

/// The trust-key prefix: Codex resolves `CODEX_HOME` to its real path. The
/// door makes the folder, then resolves it.
fn key_source(path: &str) -> Result<String> {
    Ok(door::realpath(path)?)
}

fn parse_doc(text: &str, path: &str) -> Result<DocumentMut> {
    text.parse::<DocumentMut>()
        .with_context(|| format!("{path} is not valid TOML; not touching it"))
}

fn implicit_table() -> Item {
    let mut t = Table::new();
    t.set_implicit(true);
    Item::Table(t)
}

fn table_at<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table> {
    let item = parent.entry(key).or_insert_with(implicit_table);
    item.as_table_mut()
        .with_context(|| format!("`{key}` is not a table; not touching it"))
}

fn handler_is_ours(t: &Table) -> bool {
    t.get("command")
        .and_then(Item::as_str)
        .is_some_and(|c| config::is_our_hook(c, "codex"))
}

fn install(req: &InstallRequest) -> Result<InstallResult> {
    let path = config_path(req);
    let src = key_source(&path)?;
    let text = door::read(&path)
        .with_context(|| format!("reading {path}"))?
        .unwrap_or_default();
    let mut doc = parse_doc(&text, &path)?;
    install_doc(&mut doc, req, &path, &src)?;

    let mut report = InstallResult::default();
    if door::write(&path, &doc.to_string())? {
        report.files.push(path);
    }
    report.notes.push(
        "Codex: hooks (SessionStart, UserPromptSubmit, Stop, Interrupt, SessionEnd, \
         PostToolUse, PermissionRequest) and the `mesh` MCP \
         server are set, and the hooks are marked trusted; new sessions pick them up."
            .into(),
    );
    Ok(report)
}

/// The install's edit of `config.toml` at `path`, its trust keys under `src`.
fn install_doc(doc: &mut DocumentMut, req: &InstallRequest, path: &str, src: &str) -> Result<()> {
    let root = doc.as_table_mut();

    // MCP server.
    {
        let servers = table_at(root, "mcp_servers")?;
        let mut mesh = Table::new();
        mesh.insert("command", value(req.mesh_bin.clone()));
        let mut args = toml_edit::Array::new();
        args.push("mcp");
        mesh.insert("args", value(args));
        if let Some(home) = &req.mesh_home {
            let mut env = InlineTable::new();
            env.insert("MESH_HOME", home.clone().into());
            mesh.insert("env", value(env));
        }
        mesh.insert("default_tools_approval_mode", value("approve"));
        match servers.get_mut("mesh").and_then(Item::as_table_mut) {
            // Keep the table where it is (and any keys the user added).
            Some(existing) => {
                if req.mesh_home.is_none() {
                    existing.remove("env");
                }
                for (k, v) in mesh.iter() {
                    existing.insert(k, v.clone());
                }
            }
            None => {
                servers.insert("mesh", Item::Table(mesh));
            }
        }
    }

    // Hooks, and their trust.
    let hooks = table_at(root, "hooks")?;
    remove_our_hooks(hooks, src, Some(OLD_EVENTS));
    let mut trust: Vec<(String, String)> = vec![];
    for (event, label, ours, timeout) in EVENTS {
        let command = config::hook_command(req, "codex", ours);
        let mut handler = Table::new();
        handler.insert("type", value("command"));
        handler.insert("command", value(command.clone()));
        handler.insert("timeout", value(*timeout as i64));

        let groups = hooks
            .entry(event)
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
        let Some(groups) = groups.as_array_of_tables_mut() else {
            bail!(
                "hooks.{event} in {path} is not an array of tables ([[hooks.{event}]]); not touching it"
            );
        };
        let mut placed: Option<(usize, usize)> = None;
        for (g, group) in groups.iter_mut().enumerate() {
            let Some(handlers) = group
                .get_mut("hooks")
                .and_then(Item::as_array_of_tables_mut)
            else {
                continue;
            };
            let mut h = 0;
            while h < handlers.len() {
                let ours = handlers.get(h).is_some_and(handler_is_ours);
                if ours && placed.is_none() {
                    *handlers.get_mut(h).unwrap() = handler.clone();
                    placed = Some((g, h));
                    h += 1;
                } else if ours {
                    handlers.remove(h);
                } else {
                    h += 1;
                }
            }
        }
        let (g, h) = match placed {
            Some(p) => p,
            None => {
                let mut handlers = ArrayOfTables::new();
                handlers.push(handler);
                let mut group = Table::new();
                group.insert("hooks", Item::ArrayOfTables(handlers));
                groups.push(group);
                (groups.len() - 1, 0)
            }
        };
        trust.push((
            format!("{src}:{label}:{g}:{h}"),
            trusted_hash(label, &command, *timeout),
        ));
    }
    let state = table_at(hooks, "state")?;
    for (key, hash) in trust {
        let entry = state
            .entry(&key)
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .with_context(|| format!("hooks.state.{key:?} is not a table"))?;
        entry.insert("trusted_hash", value(hash));
    }
    Ok(())
}

fn uninstall(req: &InstallRequest) -> Result<InstallResult> {
    let path = config_path(req);
    let mut report = InstallResult::default();
    let Some(text) = door::read(&path).with_context(|| format!("reading {path}"))? else {
        return Ok(report);
    };
    let src = key_source(&path)?;
    let mut doc = parse_doc(&text, &path)?;
    uninstall_doc(&mut doc, &src);

    if door::write(&path, &doc.to_string())? {
        report.files.push(path);
    }
    Ok(report)
}

/// The uninstall's edit of `config.toml`, its trust keys under `src`.
fn uninstall_doc(doc: &mut DocumentMut, src: &str) {
    let root = doc.as_table_mut();

    if let Some(servers) = root
        .get_mut("mcp_servers")
        .and_then(Item::as_table_like_mut)
    {
        let ours = servers
            .get("mesh")
            .and_then(|m| m.get("command"))
            .and_then(Item::as_str)
            .is_some_and(|c| c.rsplit('/').next() == Some("mesh"));
        if ours {
            servers.remove("mesh");
        }
        if servers.is_empty() {
            root.remove("mcp_servers");
        }
    }

    if let Some(hooks) = root.get_mut("hooks").and_then(Item::as_table_mut) {
        remove_our_hooks(hooks, src, None);
        if hooks.is_empty() {
            root.remove("hooks");
        }
    }
}

/// Removes our handlers from `only` those events (every event when `None`),
/// drops emptied groups and events, and fixes the trust keys so the user's
/// other hooks stay trusted.
fn remove_our_hooks(hooks: &mut Table, src: &str, only: Option<&[&str]>) {
    // (label, removed group indices) per event, to fix trust keys.
    let mut removed: Vec<(&'static str, Vec<usize>)> = vec![];
    let events: Vec<String> = hooks
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| k != "state" && only.is_none_or(|o| o.contains(&k.as_str())))
        .collect();
    for event in events {
        let Some(groups) = hooks.get_mut(&event).and_then(Item::as_array_of_tables_mut) else {
            continue;
        };
        let mut gone = vec![];
        let mut g = 0;
        let mut original = 0;
        while g < groups.len() {
            let group = groups.get_mut(g).unwrap();
            let mut emptied = false;
            if let Some(handlers) = group
                .get_mut("hooks")
                .and_then(Item::as_array_of_tables_mut)
            {
                let before = handlers.len();
                handlers.retain(|t| !handler_is_ours(t));
                emptied = before > 0 && handlers.is_empty();
            }
            if emptied {
                groups.remove(g);
                gone.push(original);
            } else {
                g += 1;
            }
            original += 1;
        }
        let empty = groups.is_empty();
        if empty {
            hooks.remove(&event);
        }
        if let (Some(label), false) = (label_for(&event), gone.is_empty()) {
            removed.push((label, gone));
        }
    }
    if let Some(state) = hooks.get_mut("state").and_then(Item::as_table_mut) {
        rekey_state(state, src, &removed);
        if state.is_empty() {
            hooks.remove("state");
        }
    }
}

/// After removing groups, drops their trust entries and moves the later
/// groups' entries down so the user's other hooks stay trusted.
fn rekey_state(state: &mut Table, src: &str, removed: &[(&str, Vec<usize>)]) {
    for (label, gone) in removed {
        let prefix = format!("{src}:{label}:");
        let keys: Vec<String> = state
            .iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| k.starts_with(&prefix))
            .collect();
        let mut moves = vec![];
        for key in keys {
            let rest = &key[prefix.len()..];
            let Some((g, h)) = rest.split_once(':') else {
                continue;
            };
            let Ok(g) = g.parse::<usize>() else { continue };
            let shift = gone.iter().filter(|r| **r < g).count();
            if gone.contains(&g) {
                state.remove(&key);
            } else if shift > 0 {
                let item = state.remove(&key).unwrap();
                moves.push((format!("{prefix}{}:{h}", g - shift), item));
            }
        }
        for (k, item) in moves {
            state.insert(&k, item);
        }
    }
}

// ---------------------------------------------------------------- hooks

/// Our event word (as installed) or the hook's own name → session event.
/// The same table mesh's built-in hook read, for every agent.
fn parse_event(name: &str) -> Option<&'static str> {
    let n: String = name
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match n.as_str() {
        "start" | "sessionstart" => "start",
        "prompt" | "userpromptsubmit" | "beforeagent" => "prompt",
        "stop" | "afteragent" => "stop",
        // Codex's `Interrupt`.
        "idle" | "interrupt" => "idle",
        "step" | "posttooluse" | "aftertool" => "step",
        "blocked" | "permissionrequest" => "blocked",
        "end" | "sessionend" => "end",
        _ => return None,
    })
}

pub fn hook(name: &str, payload: &Value, _env: &BTreeMap<String, String>) -> HookParsed {
    // Cursor also runs Claude Code's hooks, with its own payload: only
    // Cursor's own hooks speak there.
    if payload.get("cursor_version").is_some() {
        return HookParsed::default();
    }
    let named = payload.get("hook_event_name").and_then(Value::as_str);
    let Some(kind) = parse_event(name).or_else(|| named.and_then(parse_event)) else {
        return HookParsed::default();
    };
    let session_id = payload
        .get("session_id")
        .or_else(|| payload.get("thread_id"))
        .or_else(|| payload.get("conversation_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    HookParsed {
        kind: Some(kind.into()),
        // A Codex chat is its thread (`codex-<thread>`), as `mesh send` from
        // its shell sees it through CODEX_THREAD_ID; the codex process meshd
        // finds (none under Codex's shared server) only tells when it ends.
        key: session_id.as_ref().map(|thread| format!("codex-{thread}")),
        session_id,
        cwd: payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string),
        inbox: None,
    }
}

/// What the hook prints: the waiting messages as `additionalContext` on
/// `SessionStart`, `UserPromptSubmit` and `PostToolUse`. A turn end, an
/// `Interrupt` or a permission request prints nothing.
pub fn render(kind: &str, handover: Option<String>, intro: Option<String>) -> String {
    match kind {
        "step" => handover.map_or_else(String::new, |t| additional_context("PostToolUse", &t)),
        "prompt" => {
            handover.map_or_else(String::new, |t| additional_context("UserPromptSubmit", &t))
        }
        "start" => {
            let text: Vec<String> = intro.into_iter().chain(handover).collect();
            if text.is_empty() {
                String::new()
            } else {
                additional_context("SessionStart", &text.join("\n\n"))
            }
        }
        _ => String::new(),
    }
}

fn additional_context(event: &str, text: &str) -> String {
    json!({
        "hookSpecificOutput": { "hookEventName": event, "additionalContext": text }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "/real/.codex/config.toml";

    fn req(mesh_home: Option<&str>) -> InstallRequest {
        InstallRequest {
            mesh_bin: "/a b/mesh".into(),
            mesh_home: mesh_home.map(str::to_string),
            home: "/Users/p".into(),
            ..InstallRequest::default()
        }
    }

    fn installed(text: &str, req: &InstallRequest) -> String {
        let mut doc = parse_doc(text, "c.toml").unwrap();
        install_doc(&mut doc, req, "c.toml", SRC).unwrap();
        doc.to_string()
    }

    fn uninstalled(text: &str) -> String {
        let mut doc = parse_doc(text, "c.toml").unwrap();
        uninstall_doc(&mut doc, SRC);
        doc.to_string()
    }

    #[test]
    fn hash_matches_codex() {
        // Computed with Codex's algorithm (sorted compact JSON, sha256).
        assert_eq!(
            trusted_hash("stop", "MESH_HOME=/tmp/h '/a b/mesh' hook codex stop", 10),
            "sha256:02911bcc39ab4b939d35d4cbbb0f5781dc07a39441efaebadaab2acf2b8ca612"
        );
    }

    #[test]
    fn config_path_follows_codex() {
        let mut r = req(None);
        assert_eq!(config_path(&r), "/Users/p/.codex/config.toml");
        r.env.insert("CODEX_HOME".into(), "/x/".into());
        assert_eq!(config_path(&r), "/x/config.toml");
        r.env.insert("CODEX_HOME".into(), String::new());
        assert_eq!(config_path(&r), "/Users/p/.codex/config.toml");
        r.env.insert("CODEX_HOME".into(), "/x".into());
        r.config_dir = Some("/tmp/c".into());
        assert_eq!(config_path(&r), "/tmp/c/config.toml");
    }

    #[test]
    fn install_writes_hooks_trust_and_mcp() {
        let r = req(Some("/tmp/h"));
        let out = installed("", &r);
        assert!(out.starts_with(
            "[mcp_servers.mesh]\ncommand = \"/a b/mesh\"\nargs = [\"mcp\"]\n\
             env = { MESH_HOME = \"/tmp/h\" }\ndefault_tools_approval_mode = \"approve\"\n"
        ));
        assert!(out.contains(
            "[[hooks.Stop]]\n\n[[hooks.Stop.hooks]]\ntype = \"command\"\n\
             command = \"MESH_HOME=/tmp/h '/a b/mesh' hook codex stop\"\ntimeout = 10\n"
        ));
        let stop = format!(
            "[hooks.state.\"{SRC}:stop:0:0\"]\ntrusted_hash = \"sha256:02911bcc39ab4b939d35d4cbbb0f5781dc07a39441efaebadaab2acf2b8ca612\"\n"
        );
        assert!(out.contains(&stop), "{out}");
        for (event, label, _, _) in EVENTS {
            assert!(out.contains(&format!("[[hooks.{event}.hooks]]")), "{event}");
            assert!(out.contains(&format!("{SRC}:{label}:0:0")), "{label}");
        }
        // Idempotent.
        assert_eq!(installed(&out, &r), out);
    }

    #[test]
    fn install_keeps_the_users_config_and_drops_old_hooks() {
        let before = "# mine\nmodel = \"o3\"\n\n\
            [mcp_servers.mesh]\ncommand = \"/old/mesh\"\nargs = [\"mcp\"]\n\
            env = { MESH_HOME = \"/old\" }\nstartup_timeout_sec = 20\n\n\
            [[hooks.PreToolUse]]\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\n\
            command = \"mesh hook codex tool\"\n\n\
            [[hooks.PreToolUse]]\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\n\
            command = \"lint\"\n\n\
            [[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"notify\"\n\n\
            [hooks.state.\"/real/.codex/config.toml:pre_tool_use:0:0\"]\ntrusted_hash = \"a\"\n\n\
            [hooks.state.\"/real/.codex/config.toml:pre_tool_use:1:0\"]\ntrusted_hash = \"b\"\n";
        let out = installed(before, &req(None));
        assert!(out.starts_with("# mine\nmodel = \"o3\"\n"));
        // Our table stays where it was, with the user's key; no env without a home.
        assert!(out.contains(
            "[mcp_servers.mesh]\ncommand = \"/a b/mesh\"\nargs = [\"mcp\"]\n\
             startup_timeout_sec = 20\ndefault_tools_approval_mode = \"approve\"\n"
        ));
        assert!(!out.contains("hook codex tool"));
        assert!(out.contains("command = \"lint\""));
        // The user's PreToolUse group moved to 0, and keeps its trust.
        assert!(!out.contains("pre_tool_use:1:0"));
        assert!(out.contains("pre_tool_use:0:0\"]\ntrusted_hash = \"b\""));
        // Our Stop handler is a second group, after the user's.
        assert!(out.contains("command = \"notify\""));
        assert!(out.contains(&format!("{SRC}:stop:1:0")));
    }

    #[test]
    fn uninstall_leaves_only_the_users_config() {
        let user = "model = \"o3\"\n\n\
            [[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"notify\"\n\n\
            [hooks.state.\"/real/.codex/config.toml:stop:0:0\"]\ntrusted_hash = \"u\"\n";
        let with_ours = installed(user, &req(Some("/tmp/h")));
        assert_ne!(with_ours, user);
        assert_eq!(uninstalled(&with_ours), user);
        assert_eq!(uninstalled(&installed("", &req(None))), "");
        // Someone else's `mesh` server stays.
        let theirs = "[mcp_servers.mesh]\ncommand = \"/bin/meshy\"\n";
        assert_eq!(uninstalled(theirs), theirs);
    }

    #[test]
    fn a_config_to_fix_is_refused() {
        assert!(parse_doc("x = ", "c.toml").is_err());
        let mut doc = parse_doc("hooks = 1\n", "c.toml").unwrap();
        assert!(install_doc(&mut doc, &req(None), "c.toml", SRC).is_err());
        let mut doc = parse_doc("[hooks]\nStop = 1\n", "c.toml").unwrap();
        let e = install_doc(&mut doc, &req(None), "c.toml", SRC).unwrap_err();
        assert!(e.to_string().contains("hooks.Stop in c.toml"), "{e}");
    }

    #[test]
    fn reads_codex_hooks() {
        let env = BTreeMap::new();
        let p = hook(
            "prompt",
            &json!({"session_id": "0199", "cwd": "/w", "hook_event_name": "UserPromptSubmit"}),
            &env,
        );
        assert_eq!(p.kind.as_deref(), Some("prompt"));
        assert_eq!(p.session_id.as_deref(), Some("0199"));
        assert_eq!(p.key.as_deref(), Some("codex-0199"));
        assert_eq!(p.cwd.as_deref(), Some("/w"));
        assert!(p.inbox.is_none());
        let p = hook("idle", &json!({"thread_id": "t1"}), &env);
        assert_eq!(p.kind.as_deref(), Some("idle"));
        assert_eq!(p.key.as_deref(), Some("codex-t1"));
        let p = hook("stop", &json!({}), &env);
        assert_eq!((p.session_id, p.key), (None, None));
        assert!(hook("tool", &json!({}), &env).kind.is_none());
        assert!(
            hook("stop", &json!({"cursor_version": "1"}), &env)
                .kind
                .is_none()
        );
        assert_eq!(
            hook("x", &json!({"hook_event_name": "Interrupt"}), &env)
                .kind
                .as_deref(),
            Some("idle")
        );
    }

    #[test]
    fn renders_like_mesh_did() {
        for kind in ["stop", "idle", "blocked", "end"] {
            assert_eq!(render(kind, Some("m".into()), Some("hi".into())), "");
        }
        assert_eq!(render("prompt", None, None), "");
        assert_eq!(render("start", None, None), "");
        assert_eq!(
            render("prompt", Some("m".into()), None),
            r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"m"}}"#
        );
        assert_eq!(
            render("step", Some("m".into()), None),
            r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"m"}}"#
        );
        assert_eq!(
            render("start", Some("m".into()), Some("hi".into())),
            r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"hi\n\nm"}}"#
        );
    }
}
