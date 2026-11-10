import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, mock, test } from "bun:test";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { GlobalBooleanPreference } from "./global-boolean.ts";

const root = resolve(import.meta.dir, "../../../../..");
const sandbox = mkdtempSync(join(tmpdir(), "herdr-discord-preference-"));
let agentDir: string;
let serial = 0;
let extension: (api: unknown) => void;
let clients: ReturnType<typeof client>[] = [];
let sends: { url: string; content: string }[] = [];
const originalFetch = globalThis.fetch;
const originalWebhook = process.env.DISCORD_WEBHOOK_URL;

mock.module("@earendil-works/pi-coding-agent", () => ({ getAgentDir: () => agentDir }));
mock.module("@sinclair/typebox", () => ({ Type: { Object: (properties: unknown) => ({ properties }), String: () => ({ type: "string" }) } }));

beforeAll(async () => {
	const target = join(sandbox, "extensions/discord-notify.ts");
	mkdirSync(dirname(target), { recursive: true });
	writeFileSync(target, readFileSync(join(root, "tests/fixtures/pi-discord-notify.ts")));
	const python = process.env.PI_TEST_PYTHON || (process.platform === "win32" ? "python" : "python3");
	const installation = Bun.spawn([python, join(root, "scripts/install_pi_discord_preference.py"), "--extension", target], { stdout: "ignore", stderr: "pipe" });
	const status = await installation.exited;
	if (status !== 0) throw new Error(await new Response(installation.stderr).text());
	extension = (await import(pathToFileURL(target).href)).default;
});

beforeEach(() => {
	agentDir = join(sandbox, `agent-${serial++}`);
	mkdirSync(agentDir);
	clients = [];
	sends = [];
	process.env.DISCORD_WEBHOOK_URL = "configured-fixture-webhook";
	globalThis.fetch = mock(async (url: unknown, options?: RequestInit) => {
		sends.push({ url: String(url), content: JSON.parse(String(options?.body)).content });
		return new Response(null, { status: 204 });
	}) as typeof fetch;
});

afterEach(async () => {
	await Promise.all(clients.map((item) => item.emit("session_shutdown")));
	globalThis.fetch = originalFetch;
});

afterAll(() => {
	if (originalWebhook === undefined) delete process.env.DISCORD_WEBHOOK_URL;
	else process.env.DISCORD_WEBHOOK_URL = originalWebhook;
	mock.restore();
	rmSync(sandbox, { recursive: true, force: true });
});

function client(historyEnabled = false) {
	const events = new Map<string, ((event: unknown, ctx: unknown) => unknown)[]>();
	const commands = new Map<string, { handler: (args: string, ctx: unknown) => Promise<void> }>();
	const tools = new Map<string, { execute: (id: string, params: unknown) => Promise<unknown> }>();
	const statuses: string[] = [];
	const notifications: { text: string; kind: string }[] = [];
	const branch = mock(() => { throw new Error(`Session history must not be read: ${historyEnabled}`); });
	const append = mock(() => { throw new Error("Session history must not be written"); });
	const ctx = {
		hasUI: true,
		sessionManager: { getBranch: branch },
		ui: {
			theme: { fg: (_color: string, text: string) => text },
			setStatus: (_key: string, value: string) => statuses.push(value),
			notify: (text: string, kind: string) => notifications.push({ text, kind }),
		},
	};
	const api = {
		on(name: string, handler: (event: unknown, ctx: unknown) => unknown) {
			events.set(name, [...(events.get(name) ?? []), handler]);
		},
		registerCommand(name: string, command: { handler: (args: string, ctx: unknown) => Promise<void> }) { commands.set(name, command); },
		registerTool(tool: { name: string; execute: (id: string, params: unknown) => Promise<unknown> }) { tools.set(tool.name, tool); },
		appendEntry: append,
	};
	extension(api);
	const instance = {
		statuses, notifications, branch, append, events, tools,
		async emit(name: string) {
			for (const handler of events.get(name) ?? []) await handler({}, ctx);
		},
		async command(args: string, name = "discord-notify") {
			await commands.get(name)!.handler(args, ctx);
		},
	};
	clients.push(instance);
	return instance;
}

const preference = () => new GlobalBooleanPreference(agentDir, "discord-notify");

describe("global Discord notification preference", () => {
	test("defaults off regardless of old branch entries, without writes or notifications", async () => {
		const first = client(true);
		const second = client(false);
		for (const item of [first, second]) {
			for (const event of ["session_start", "session_switch", "session_fork", "session_tree", "agent_end"]) await item.emit(event);
			await item.command("status");
			expect(item.statuses.at(-1)).toBe("discord:global:off");
			expect(item.branch).not.toHaveBeenCalled();
			expect(item.append).not.toHaveBeenCalled();
		}
		expect(readdirSync(agentDir)).toEqual([]);
		expect(sends).toEqual([]);
	});

	test("other running sessions read the latest setting before commands and automatic sends", async () => {
		const first = client();
		const second = client();
		await first.emit("session_start");
		await second.emit("session_start");
		await first.command("on");
		expect(sends).toEqual([]);
		await second.command("status");
		expect(second.notifications.at(-1)?.text).toBe("Global automatic Discord notifications are enabled");
		await second.emit("agent_end");
		expect(sends).toEqual([{ url: "configured-fixture-webhook", content: "ready" }]);
		await second.command("off");
		await first.emit("agent_end");
		expect(sends).toHaveLength(1);
		expect(first.statuses.at(-1)).toBe("discord:global:off");
		await first.command("toggle");
		await second.command("toggle");
		expect(preference().read()).toEqual({ enabled: false });
	});

	test("idle status indicators synchronize without a command or restart", async () => {
		const first = client();
		const second = client();
		await first.emit("session_start");
		await second.emit("session_start");
		await first.command("on");
		await Bun.sleep(1150);
		expect(second.statuses.at(-1)).toBe("discord:global:on");
		unlinkSync(preference().path);
		await Bun.sleep(1150);
		expect(first.statuses.at(-1)).toBe("discord:global:off");
		expect(second.statuses.at(-1)).toBe("discord:global:off");
		expect(sends).toEqual([]);
	});

	test("rapid changes between polls reconcile the last rendered value", async () => {
		const first = client();
		const second = client();
		await first.emit("session_start");
		await second.emit("session_start");
		await first.command("on");
		expect(first.statuses.at(-1)).toBe("discord:global:on");
		await second.command("off");
		await Bun.sleep(1150);
		expect(first.statuses.at(-1)).toBe("discord:global:off");
		expect(second.statuses.at(-1)).toBe("discord:global:off");
		const count = first.statuses.length;
		await Bun.sleep(1150);
		expect(first.statuses).toHaveLength(count);
		expect(sends).toEqual([]);
	});

	test("new, resumed, forked and branch-switched sessions retain the same global state", async () => {
		const first = client();
		await first.command("on");
		const saved = readFileSync(preference().path, "utf8");
		await first.emit("session_shutdown");
		for (let index = 0; index < 3; index++) {
			const next = client(false);
			for (const event of ["session_start", "session_switch", "session_fork", "session_tree"]) await next.emit(event);
			expect(next.statuses.at(-1)).toBe("discord:global:on");
			expect(next.branch).not.toHaveBeenCalled();
			expect(next.append).not.toHaveBeenCalled();
		}
		expect(readFileSync(preference().path, "utf8")).toBe(saved);
		expect(sends).toEqual([]);
	});

	test("manual command, notification test and discord_send remain independent of automatic notifications", async () => {
		const item = client();
		await item.emit("session_start");
		await item.command("manual message", "discord");
		await item.command("test");
		await item.tools.get("discord_send")!.execute("manual-call", { message: "explicit tool message" });
		expect(sends.map((send) => send.content)).toEqual(["manual message", "ready", "explicit tool message"]);
		expect(preference().read()).toEqual({ enabled: false });
		await item.emit("agent_end");
		expect(sends).toHaveLength(3);
	});

	test("corrupt preferences fail closed, are visible, and can be repaired by explicit commands", async () => {
		mkdirSync(dirname(preference().path));
		writeFileSync(preference().path, '{"enabled":"true"}');
		const item = client(true);
		await item.emit("session_start");
		await item.emit("agent_end");
		await item.command("status");
		expect(item.statuses.at(-1)).toBe("discord:global:off!");
		expect(item.notifications.at(-1)?.text).toContain("unavailable or invalid");
		expect(sends).toEqual([]);
		await item.command("on");
		expect(preference().read()).toEqual({ enabled: true });
		expect(sends).toEqual([]);
	});

	test("failed writes report an error without enabling notifications or replacing other files", async () => {
		mkdirSync(preference().path, { recursive: true });
		const item = client();
		await item.emit("session_start");
		await item.command("on");
		expect(item.notifications.at(-1)?.kind).toBe("error");
		expect(statSync(preference().path).isDirectory()).toBe(true);
		await item.emit("agent_end");
		expect(sends).toEqual([]);
	});

	test("shutdown removes synchronization timers and never notifies Discord", async () => {
		const item = client();
		await item.emit("session_start");
		await item.emit("session_shutdown");
		const count = item.statuses.length;
		preference().set(true);
		await Bun.sleep(1150);
		expect(item.statuses).toHaveLength(count);
		expect(sends).toEqual([]);
	});
});

describe("reusable global boolean storage", () => {
	test("isolates independent preferences and user-level Pi configurations", () => {
		const discord = preference();
		const other = new GlobalBooleanPreference(agentDir, "another-toggle");
		const isolated = new GlobalBooleanPreference(join(agentDir, "other-user"), "discord-notify");
		discord.set(true);
		other.set(false);
		expect(discord.read()).toEqual({ enabled: true });
		expect(other.read()).toEqual({ enabled: false });
		expect(isolated.read()).toEqual({ enabled: false });
		if (process.platform !== "win32") expect(statSync(discord.path).mode & 0o777).toBe(0o600);
		expect(() => new GlobalBooleanPreference(agentDir, "../escape")).toThrow();
	});

	test("concurrent processes publish only complete preference files", async () => {
		const worker = join(agentDir, "writer.ts");
		writeFileSync(worker, `import { GlobalBooleanPreference } from ${JSON.stringify(pathToFileURL(join(import.meta.dir, "global-boolean.ts")).href)};\nconst store = new GlobalBooleanPreference(process.argv[2], "discord-notify");\nfor (let i=0; i<20; i++) store.set(i % 2 === 0);\n`);
		const workers = [0, 1].map(() => Bun.spawn([process.execPath, worker, agentDir], { stdout: "ignore", stderr: "pipe" }));
		let done = false;
		const completion = Promise.all(workers.map((worker) => worker.exited)).then((codes) => { done = true; return codes; });
		while (!done) {
			expect(preference().read().error).toBeUndefined();
			await Bun.sleep(1);
		}
		expect(await completion).toEqual([0, 0]);
		expect(preference().read()).toEqual({ enabled: false });
		expect(readdirSync(dirname(preference().path))).toEqual(["discord-notify.json"]);
	});
});
