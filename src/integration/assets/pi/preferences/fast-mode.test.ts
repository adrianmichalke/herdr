import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, mock, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { GlobalBooleanPreference } from "./global-boolean.ts";

let agentDir: string;
mock.module("@earendil-works/pi-coding-agent", () => ({ getAgentDir: () => agentDir }));
let extension: (api: any) => void;
const sandbox = mkdtempSync(join(tmpdir(), "herdr-fast-preference-"));
let serial = 0;
let clients: ReturnType<typeof client>[];
const originalFetch = globalThis.fetch;
const originalEnvironment = process.env.PI_CODEX_FAST_MODE;
let network: ReturnType<typeof mock>;

beforeAll(async () => {
	const root = resolve(import.meta.dir, "../../../../..");
	const target = join(sandbox, "package/pi-fast-mode-extension.js");
	mkdirSync(dirname(target));
	writeFileSync(target, readFileSync(join(root, "tests/fixtures/pi-fast-mode-legacy.js")));
	const python = process.env.PI_TEST_PYTHON || (process.platform === "win32" ? "python" : "python3");
	const installation = Bun.spawn([python, join(root, "scripts/install_pi_fast_preference.py"), "--extension", target], { stdout: "ignore", stderr: "pipe" });
	if (await installation.exited !== 0) throw new Error(await new Response(installation.stderr).text());
	extension = (await import(pathToFileURL(target).href)).default;
});

beforeEach(() => {
	agentDir = join(sandbox, `agent-${serial++}`);
	mkdirSync(agentDir);
	clients = [];
	process.env.PI_CODEX_FAST_MODE = "on";
	network = mock(() => { throw new Error("No paid or remote requests are permitted"); });
	globalThis.fetch = network as typeof fetch;
});
afterEach(async () => {
	for (const item of clients) await item.emit("session_shutdown");
	expect(network).not.toHaveBeenCalled();
	globalThis.fetch = originalFetch;
});
afterAll(() => {
	if (originalEnvironment === undefined) delete process.env.PI_CODEX_FAST_MODE;
	else process.env.PI_CODEX_FAST_MODE = originalEnvironment;
	mock.restore();
	rmSync(sandbox, { recursive: true, force: true });
});

const codex = { provider: "openai-codex", api: "openai-codex-responses", id: "gpt-5.1-codex" };
const openai = { provider: "openai", api: "openai-responses", id: "gpt-5.1" };
const alternateCodex = { ...codex, id: "gpt-5.1" };
const unsupported = { provider: "anthropic", api: "anthropic-messages", id: "claude-sonnet-4-5" };

function client(model: { provider: string; api: string; id: string } | undefined = codex, hasUI = true) {
	const handlers = new Map<string, ((event: any, ctx: any) => any)[]>();
	const commands = new Map<string, { handler: (args: string, ctx: any) => Promise<void> }>();
	const statuses: string[] = [];
	const notifications: { text: string; kind: string }[] = [];
	const forbidden = mock(() => { throw new Error("Model, thinking, in-flight work and history must not change"); });
	const ctx = {
		model, hasUI, thinkingLevel: "high",
		modelRegistry: { find: (provider: string, id: string) => [codex, openai, alternateCodex].find((model) => model.provider === provider && model.id === id) },
		sessionManager: { getBranch: forbidden, getEntries: forbidden },
		abort: forbidden,
		ui: {
			theme: { fg: (_color: string, value: string) => value },
			setStatus: (_key: string, value: string) => { statuses.push(value); },
			notify: (text: string, kind: string) => { notifications.push({ text, kind }); },
		},
	};
	extension({
		on: (name: string, handler: any) => { handlers.set(name, [...(handlers.get(name) ?? []), handler]); },
		registerCommand: (name: string, command: any) => { commands.set(name, command); },
		appendEntry: forbidden, setModel: forbidden, setThinkingLevel: forbidden,
	} as any);
	const item = {
		ctx, statuses, notifications, forbidden,
		async emit(name: string, event = {}) {
			for (const handler of handlers.get(name) ?? []) await handler(event, ctx);
		},
		async command(args: string) { await commands.get("fast")!.handler(args, ctx); },
		async request(payload: unknown) {
			let result = payload;
			for (const handler of handlers.get("before_provider_request") ?? []) {
				const next = await handler({ payload: result }, ctx);
				if (next !== undefined) result = next;
			}
			return result as Record<string, unknown>;
		},
	};
	clients.push(item);
	return item;
}

const preference = () => new GlobalBooleanPreference(agentDir, "fast-mode");
const payload = (model = codex) => ({ model: model.id, input: [], reasoning: { effort: "high" }, stream: true });

describe("authoritative global Pi fast mode", () => {
	test("defaults off despite legacy environment or branch state and never writes history", async () => {
		const item = client();
		for (const reason of ["startup", "new", "resume", "fork", "reload"]) await item.emit("session_start", { reason });
		await item.emit("session_tree");
		await item.command("status");
		expect(await item.request(payload())).toEqual(payload());
		expect(item.statuses.at(-1)).toBe("fast:global:off");
		expect(readdirSync(agentDir)).toEqual([]);
		expect(item.forbidden).not.toHaveBeenCalled();
	});

	test("running sessions see command changes on the very next request", async () => {
		const first = client();
		const second = client();
		await first.emit("session_start");
		await second.emit("session_start");
		await first.command("on");
		expect(await second.request(payload())).toEqual({ ...payload(), service_tier: "priority" });
		await second.command("off");
		expect(await first.request({ ...payload(), service_tier: "priority" })).toEqual(payload());
		await second.command("toggle");
		await first.command("status");
		expect(first.notifications.at(-1)?.text).toContain("Global fast mode is on");
		expect(first.notifications.at(-1)?.text).toContain("higher pricing");
		await first.command("");
		expect(preference().read()).toEqual({ enabled: false });
		expect(first.forbidden).not.toHaveBeenCalled();
		expect(second.forbidden).not.toHaveBeenCalled();
	});

	test("new, resumed, forked and branch-switched sessions share the persistent value", async () => {
		await client().command("on");
		const saved = readFileSync(preference().path, "utf8");
		for (const reason of ["startup", "new", "resume", "fork", "reload"]) {
			const item = client();
			await item.emit("session_start", { reason });
			await item.emit("session_tree");
			expect(await item.request(payload())).toHaveProperty("service_tier", "priority");
			expect(item.forbidden).not.toHaveBeenCalled();
		}
		expect(readFileSync(preference().path, "utf8")).toBe(saved);
	});

	test("OpenAI and Codex Responses preserve the model, reasoning and original payload", async () => {
		for (const model of [codex, openai]) {
			const item = client(model);
			const body = Object.freeze(payload(model));
			await item.command("on");
			expect(await item.request(body)).toEqual({ ...body, service_tier: "priority" });
			expect(body).not.toHaveProperty("service_tier");
			expect(item.ctx.model).toEqual(model);
			expect(item.ctx.thinkingLevel).toBe("high");
			await item.command("off");
			expect(await item.request({ ...body, service_tier: "priority" })).toEqual(body);
			expect(item.forbidden).not.toHaveBeenCalled();
		}
	});

	test("unsupported providers and APIs retain the preference without changing their requests", async () => {
		for (const model of [unsupported, { ...openai, provider: "openrouter" }, { ...openai, api: "openai-completions" }, { ...openai, api: "openai-image", id: "gpt-image-1" }]) {
			const item = client(model);
			await item.command("on");
			const body = { ...payload(model), service_tier: "provider-owned" };
			expect(await item.request(body)).toBe(body);
			expect(preference().read()).toEqual({ enabled: true });
			expect(item.statuses.at(-1)).toBe("fast:global:on (n/a)");
			expect(item.notifications.at(-1)?.text).toContain("not applicable");
			item.ctx.model = codex;
			await item.emit("model_select");
			expect(await item.request(payload())).toHaveProperty("service_tier", "priority");
		}
	});

	test("same-provider auxiliary requests also use the global preference", async () => {
		const item = client();
		await item.command("on");
		expect(await item.request(payload(alternateCodex))).toHaveProperty("service_tier", "priority");
		await item.command("off");
		expect(await item.request({ ...payload(alternateCodex), service_tier: "priority" })).toEqual(payload(alternateCodex));
	});

	test("the existing Codex virtual router only modifies resolved Codex request payloads", async () => {
		const item = client({ ...codex, api: "pi-virtual", id: "jev-auto" });
		await item.command("on");
		expect(await item.request(payload())).toHaveProperty("service_tier", "priority");
		const classifier = { model: "jev-latest", state: {}, questions: {} };
		expect(await item.request(classifier)).toBe(classifier);
		await item.command("off");
		expect(await item.request({ ...payload(), service_tier: "priority" })).toEqual(payload());
	});

	test("invalid or removed preferences fail closed and explicit writes can repair them", async () => {
		await client().command("on");
		const item = client();
		writeFileSync(preference().path, '{"enabled":"true"}');
		expect(await item.request({ ...payload(), service_tier: "priority" })).toEqual(payload());
		expect(item.statuses.at(-1)).toBe("fast:global:off!");
		await item.command("status");
		expect(item.notifications.at(-1)?.text).toContain("unavailable or invalid");
		await item.command("on");
		expect(await item.request(payload())).toHaveProperty("service_tier", "priority");
		unlinkSync(preference().path);
		expect(await item.request({ ...payload(), service_tier: "priority" })).toEqual(payload());
	});

	test("failed writes and invalid commands never enable the preference", async () => {
		const item = client();
		await item.command("enable everything");
		expect(readdirSync(agentDir)).toEqual([]);
		mkdirSync(preference().path, { recursive: true });
		await item.command("on");
		expect(item.notifications.at(-1)?.kind).toBe("error");
		expect(await item.request({ ...payload(), service_tier: "priority" })).toEqual(payload());
	});

	test("footer refreshes while idle and shuts down cleanly without touching in-flight requests", async () => {
		const first = client();
		const second = client();
		await first.emit("session_start");
		await second.emit("session_start");
		await first.command("on");
		const inFlight = await second.request(payload());
		await first.command("off");
		await Bun.sleep(1150);
		expect(second.statuses.at(-1)).toBe("fast:global:off");
		expect(inFlight).toHaveProperty("service_tier", "priority");
		await second.emit("session_shutdown");
		const count = second.statuses.length;
		await first.command("on");
		await Bun.sleep(1150);
		expect(second.statuses).toHaveLength(count);
		expect(second.forbidden).not.toHaveBeenCalled();
	});

	test("UI failures and non-interactive contexts cannot bypass the request policy", async () => {
		const item = client();
		item.ctx.ui.setStatus = () => { throw new Error("UI failed"); };
		expect(await item.request({ ...payload(), service_tier: "priority" })).toEqual(payload());
		const headless = client(codex, false);
		await headless.emit("session_start");
		await headless.command("on");
		expect(await headless.request(payload())).toHaveProperty("service_tier", "priority");
		expect(headless.statuses).toEqual([]);
	});

	test("global preferences for different agent directories and Discord stay independent", async () => {
		const item = client();
		const discord = new GlobalBooleanPreference(agentDir, "discord-notify");
		discord.set(true);
		await item.command("off");
		expect(discord.read()).toEqual({ enabled: true });
		expect(new GlobalBooleanPreference(join(agentDir, "another-user"), "fast-mode").read()).toEqual({ enabled: false });
		expect(await item.request(null)).toBeNull();
		expect(await item.request([])).toEqual([]);
	});
});
