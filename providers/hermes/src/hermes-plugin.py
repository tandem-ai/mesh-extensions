"""mesh for Hermes: answers from your team come back into this session.

Written by `mesh install hermes`; `mesh uninstall hermes` removes it.
Calls: mesh hook hermes <event>

The plugin serves this session's inbox (a Unix socket under $MESH_HOME/inbox)
and reports it to meshd through `mesh hook hermes start`. meshd posts answers
there. The classic CLI takes a message injected mid-turn as an interrupt, so
answers are held while Hermes works or waits on you, and go in once it is at
rest; anything still waiting when a turn starts goes in as turn context.
"""

import json
import os
import secrets
import socketserver
import subprocess
import threading
from pathlib import Path

MESH = "__MESH_BIN__"
MESH_HOME_PIN = "__MESH_HOME__"


def register(ctx):
    if os.environ.get("MESH_SESSION") == "none":
        return
    # Everything Hermes starts (terminal tool, hooks) belongs to this session.
    if MESH_HOME_PIN:
        os.environ["MESH_HOME"] = MESH_HOME_PIN
    # `mesh send` from the terminal tool works even if mesh isn't on the user's PATH.
    os.environ["PATH"] = os.path.dirname(MESH) + os.pathsep + os.environ.get("PATH", "")
    os.environ.setdefault("MESH_SESSION", f"hermes-{os.getpid()}")
    os.environ["MESH_AGENT"] = "hermes"
    os.environ["MESH_SESSION_PID"] = str(os.getpid())
    home = Path(os.environ.get("MESH_HOME") or Path.home() / ".mesh")
    sock = home / "inbox" / f"hermes-{os.getpid()}.sock"
    token = secrets.token_hex(16)
    lock = threading.Lock()
    state = {"busy": False, "asking": 0, "held": [], "intro": "", "session_key": None, "server": None}

    def hook(event, payload=None):
        """One `mesh hook` call; what it printed ("" on any failure)."""
        env = dict(os.environ, MESH_AGENT_INBOX=str(sock), MESH_AGENT_INBOX_TOKEN=token)
        body = json.dumps({"hook_event_name": event, **(payload or {})})
        try:
            done = subprocess.run(
                [MESH, "hook", "hermes", event],
                input=body, capture_output=True, text=True, env=env, timeout=10,
            )
            return done.stdout.strip()
        except Exception:
            return ""

    def flush():
        with lock:
            if state["busy"] or state["asking"] or not state["held"]:
                return
            held, state["held"] = state["held"], []
        for i, text in enumerate(held):
            if not ctx.inject_message(text, session_key=state["session_key"]):
                # Not accepted: keep it for the next time Hermes is at rest.
                with lock:
                    state["held"] = held[i:] + state["held"]
                return

    def deliver(text):
        if text:
            with lock:
                state["held"].append(text)
            flush()

    class Inbox(socketserver.StreamRequestHandler):
        def handle(self):
            authed = False
            for raw in self.rfile:
                try:
                    msg = json.loads(raw)
                except ValueError:
                    continue
                if msg.get("type") == "auth":
                    authed = msg.get("token") == token
                elif msg.get("type") == "user" and authed:
                    deliver(str((msg.get("message") or {}).get("content", "")))

    def start(session_id=None):
        # Ink TUI and desktop route by the durable `ses_…` key; the classic CLI needs none.
        if session_id and str(session_id).startswith("ses_"):
            state["session_key"] = str(session_id)
        if state["server"] is not None:
            return
        sock.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        sock.unlink(missing_ok=True)
        server = socketserver.ThreadingUnixStreamServer(str(sock), Inbox)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        state["server"] = server
        state["intro"] = hook("start")

    def on_session_start(session_id=None, **_):
        start(session_id)

    def pre_llm_call(session_id=None, **_):
        start(session_id)
        with lock:
            state["busy"] = True
        parts = [state["intro"], hook("prompt")]
        state["intro"] = ""
        text = "\n\n".join(p for p in parts if p)
        return {"context": text} if text else None

    def on_session_end(**_):
        # Fires at every turn's end, interrupted or not.
        with lock:
            state["busy"] = False
        deliver(hook("stop"))
        flush()

    def on_human_input_request(**_):
        with lock:
            state["asking"] += 1
        hook("blocked")

    def on_human_input_resolved(**_):
        with lock:
            state["asking"] = max(0, state["asking"] - 1)
        deliver(hook("step"))
        flush()

    def on_session_finalize(**_):
        server = state["server"]
        if server is not None:
            server.shutdown()
            sock.unlink(missing_ok=True)
            state["server"] = None
        hook("end")

    for name, fn in [
        ("on_session_start", on_session_start),
        ("pre_llm_call", pre_llm_call),
        ("on_session_end", on_session_end),
        ("on_human_input_request", on_human_input_request),
        ("on_human_input_resolved", on_human_input_resolved),
        ("on_session_finalize", on_session_finalize),
    ]:
        ctx.register_hook(name, fn)
