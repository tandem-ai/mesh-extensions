// mesh for Pi: answers from your team come back into this session.
// Written by `mesh install pi`; `mesh uninstall pi` removes it.
// Calls: mesh hook pi <event>
//
// The extension serves this session's inbox (a Unix socket under
// $MESH_HOME/inbox) and reports it to meshd through `mesh hook pi start`.
// meshd posts answers there; they reach Pi as a steering message while it
// works, or start a run while it is idle (after Esc too).
import { execFile } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdirSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:net";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const MESH = "__MESH_BIN__";
const MESH_HOME_PIN = "__MESH_HOME__";

export default function (pi: ExtensionAPI) {
	if (process.env.MESH_SESSION === "none") return;
	// Everything Pi starts (bash, hooks) belongs to this session.
	if (MESH_HOME_PIN) process.env.MESH_HOME = MESH_HOME_PIN;
	// `mesh send` from the bash tool works even if mesh isn't on the user's PATH.
	process.env.PATH = `${dirname(MESH)}:${process.env.PATH ?? ""}`;
	process.env.MESH_SESSION ||= `pi-${process.pid}`;
	process.env.MESH_AGENT = "pi";
	process.env.MESH_SESSION_PID = String(process.pid);
	const home = process.env.MESH_HOME || join(homedir(), ".mesh");
	const socket = join(home, "inbox", `pi-${process.pid}.sock`);
	const token = randomBytes(16).toString("hex");
	let server: Server | undefined;

	// One hook call; resolves with what it printed ("" on any failure).
	const hook = (event: string, input: object = {}) =>
		new Promise<string>((resolve) => {
			const child = execFile(
				MESH,
				["hook", "pi", event],
				{
					env: { ...process.env, MESH_AGENT_INBOX: socket, MESH_AGENT_INBOX_TOKEN: token },
					timeout: 10_000,
				},
				(_err, stdout) => resolve((stdout ?? "").trim()),
			);
			child.stdin?.on("error", () => {});
			child.stdin?.end(JSON.stringify({ hook_event_name: event, ...input }));
		});

	const deliver = (text: string) => {
		if (!text) return;
		pi.sendMessage(
			{ customType: "mesh", content: text, display: true },
			{ triggerTurn: true, deliverAs: "steer" },
		);
	};

	pi.on("session_start", async (_event, ctx) => {
		mkdirSync(join(home, "inbox"), { recursive: true, mode: 0o700 });
		rmSync(socket, { force: true });
		server = createServer((conn) => {
			let buf = "";
			let authed = false;
			conn.setEncoding("utf8");
			conn.on("error", () => {});
			conn.on("data", (chunk) => {
				buf += chunk;
				for (let nl = buf.indexOf("\n"); nl >= 0; nl = buf.indexOf("\n")) {
					const line = buf.slice(0, nl);
					buf = buf.slice(nl + 1);
					let msg: { type?: string; token?: string; message?: { content?: unknown } };
					try {
						msg = JSON.parse(line);
					} catch {
						continue;
					}
					if (msg.type === "auth") authed = msg.token === token;
					else if (msg.type === "user" && authed) deliver(String(msg.message?.content ?? ""));
				}
			});
		});
		server.on("error", () => {});
		server.listen(socket);
		const intro = await hook("start", { cwd: ctx.cwd });
		if (intro) {
			pi.sendMessage({ customType: "mesh", content: intro, display: false }, { deliverAs: "nextTurn" });
		}
	});

	// Working / idle for meshd; anything still waiting comes back with the call.
	pi.on("agent_start", async () => deliver(await hook("prompt")));
	pi.on("agent_settled", async () => deliver(await hook("stop")));

	// A dialog waits on the person: meshd holds off until it closes.
	pi.on("ui_prompt_start", async () => void (await hook("blocked")));
	pi.on("ui_prompt_end", async () => deliver(await hook("step")));

	pi.on("session_shutdown", async () => {
		server?.close();
		rmSync(socket, { force: true });
		await hook("end");
	});
}
