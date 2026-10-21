// teletap for Pi: answers from your team come back into this session.
// Written by `teletap install pi`; `teletap uninstall pi` removes it.
// Calls: teletap hook pi <event>
//
// The extension serves this session's inbox (a Unix socket under
// $TELETAP_HOME/inbox) and reports it to teletapd through `teletap hook pi start`.
// teletapd posts answers there; they reach Pi as a steering message while it
// works, or start a run while it is idle (after Esc too).
import { execFile } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdirSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:net";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const TELETAP = "__TELETAP_BIN__";
const TELETAP_HOME_PIN = "__TELETAP_HOME__";

export default function (pi: ExtensionAPI) {
	if (process.env.TELETAP_SESSION === "none") return;
	// Everything Pi starts (bash, hooks) belongs to this session.
	if (TELETAP_HOME_PIN) process.env.TELETAP_HOME = TELETAP_HOME_PIN;
	// `teletap send` from the bash tool works even if teletap isn't on the user's PATH.
	process.env.PATH = `${dirname(TELETAP)}:${process.env.PATH ?? ""}`;
	process.env.TELETAP_SESSION ||= `pi-${process.pid}`;
	process.env.TELETAP_AGENT = "pi";
	process.env.TELETAP_SESSION_PID = String(process.pid);
	const home = process.env.TELETAP_HOME || join(homedir(), ".teletap");
	const socket = join(home, "inbox", `pi-${process.pid}.sock`);
	const token = randomBytes(16).toString("hex");
	let server: Server | undefined;

	// One hook call; resolves with what it printed ("" on any failure).
	const hook = (event: string, input: object = {}) =>
		new Promise<string>((resolve) => {
			const child = execFile(
				TELETAP,
				["hook", "pi", event],
				{
					env: { ...process.env, TELETAP_AGENT_INBOX: socket, TELETAP_AGENT_INBOX_TOKEN: token },
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
			{ customType: "teletap", content: text, display: true },
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
			pi.sendMessage({ customType: "teletap", content: intro, display: false }, { deliverAs: "nextTurn" });
		}
	});

	// Working / idle for teletapd; anything still waiting comes back with the call.
	pi.on("agent_start", async () => deliver(await hook("prompt")));
	pi.on("agent_settled", async () => deliver(await hook("stop")));

	// A dialog waits on the person: teletapd holds off until it closes.
	pi.on("ui_prompt_start", async () => void (await hook("blocked")));
	pi.on("ui_prompt_end", async () => deliver(await hook("step")));

	pi.on("session_shutdown", async () => {
		server?.close();
		rmSync(socket, { force: true });
		await hook("end");
	});
}
