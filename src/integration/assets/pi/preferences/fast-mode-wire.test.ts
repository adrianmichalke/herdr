import { afterAll, afterEach, beforeAll, describe, expect, mock, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { zstdDecompressSync } from "node:zlib";

mock.module("@earendil-works/pi-coding-agent", () => ({ getAgentDir: () => { throw new Error("Test must not access user preferences"); } }));
const { GlobalFastModePreference } = await import("./fast-mode.ts");
const sdk = process.env.PI_AI_TEST_PACKAGE_DIR;
const directory = mkdtempSync(join(tmpdir(), "herdr-fast-wire-"));
const recordings: { transport: string; payload: Record<string, unknown> }[] = [];
let responses: any;
let codexResponses: any;
let openai: any;
let codex: any;
let server: ReturnType<typeof Bun.serve>;
let connections = 0;
let rejectWebSockets = false;
let upgrades = 0;
const completed = { type: "response.completed", response: { id: "response-local-test", object: "response", status: "completed", output: [], usage: { input_tokens: 0, output_tokens: 0, input_tokens_details: { cached_tokens: 0 } } } };
const jwt = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url");
const inertKey = `${jwt({ alg: "none" })}.${jwt({ "https://api.openai.com/auth": { chatgpt_account_id: "offline-test-account" } })}.test`;

const load = (name: string) => import(pathToFileURL(join(sdk!, "dist", name)).href);

afterAll(() => {
	server?.stop(true);
	codexResponses?.closeOpenAICodexWebSocketSessions();
	mock.restore();
	rmSync(directory, { recursive: true, force: true });
});
afterEach(() => { codexResponses?.closeOpenAICodexWebSocketSessions(); recordings.length = 0; connections = 0; upgrades = 0; rejectWebSockets = false; });

describe.skipIf(!sdk)("priority tier through real Pi AI transports (loopback only)", () => {
	beforeAll(async () => {
		responses = await load("api/openai-responses.js");
		codexResponses = await load("api/openai-codex-responses.js");
		const openaiCatalog = await load("providers/openai.models.js");
		const codexCatalog = await load("providers/openai-codex.models.js");
		openai = Object.values(openaiCatalog.OPENAI_MODELS).find((model: any) => model.api === "openai-responses" && model.id.startsWith("gpt-5"));
		codex = Object.values(codexCatalog.OPENAI_CODEX_MODELS).find((model: any) => model.api === "openai-codex-responses");
		expect(openai).toBeDefined();
		expect(codex).toBeDefined();
		server = Bun.serve({
			hostname: "127.0.0.1", port: 0,
			async fetch(request, server) {
				if (request.headers.get("upgrade") === "websocket") {
					upgrades++;
					if (rejectWebSockets) return new Response("WebSocket deliberately unavailable", { status: 426 });
					if (server.upgrade(request)) return;
					return new Response("upgrade failed", { status: 400 });
				}
				const data = Buffer.from(await request.arrayBuffer());
				const body = request.headers.get("content-encoding") === "zstd" ? zstdDecompressSync(data) : data;
				recordings.push({ transport: "sse", payload: JSON.parse(body.toString("utf8")) });
				return new Response(`data: ${JSON.stringify(completed)}\n\n`, { headers: { "content-type": "text/event-stream" } });
			},
			websocket: {
				open() { connections++; },
				message(socket, data) {
					recordings.push({ transport: "websocket", payload: JSON.parse(String(data)) });
					socket.send(JSON.stringify(completed));
				},
			},
		});
	});

	for (const [provider, transports] of [["openai", ["sse"]], ["openai-codex", ["sse", "websocket", "websocket-cached"]]] as const) {
		for (const transport of transports) {
			test(`${provider} ${transport}: on requests priority; off restores default, without changing reasoning`, async () => {
				const model = { ...(provider === "openai" ? openai : codex), baseUrl: new URL("v1", server.url).href };
				const preference = new GlobalFastModePreference(join(directory, `${provider}-${transport}`));
				const ctx = { hasUI: false, model } as any;
				const stream = provider === "openai" ? responses.stream : codexResponses.stream;
				const states = transport === "websocket-cached" ? [true, true, false] : [true, false];
				for (const enabled of states) {
					preference.preference.set(enabled);
					const result = await stream(model, { messages: [] }, {
						apiKey: provider === "openai-codex" ? inertKey : "inert-local-test-key",
						transport, sessionId: `offline-${provider}-${transport}`, reasoningEffort: "high", serviceTier: "priority", maxRetries: 0,
						timeoutMs: 3000, websocketConnectTimeoutMs: 3000, signal: AbortSignal.timeout(5000),
						env: { HTTP_PROXY: "", HTTPS_PROXY: "", ALL_PROXY: "", NO_PROXY: "*" },
						onPayload: (payload: unknown) => preference.apply(payload, ctx),
						fetch: (url: string | URL | Request, options: RequestInit) => {
							expect(new URL(String(url)).hostname).toBe("127.0.0.1");
							return fetch(url, options);
						},
					}).result();
					expect(result.errorMessage).toBeUndefined();
					expect(result.stopReason).toBe("stop");
					const request = recordings.at(-1)!;
					expect(request.transport).toBe(transport === "sse" ? "sse" : "websocket");
					expect(request.payload.service_tier).toBe(enabled ? "priority" : undefined);
					expect(request.payload.model).toBe(model.id);
					expect(request.payload.reasoning).toMatchObject({ effort: "high" });
				}
				expect(recordings).toHaveLength(states.length);
				if (transport !== "sse") expect(connections).toBe(1);
				if (transport === "websocket-cached") {
					expect(recordings[1].payload.previous_response_id).toBe(completed.response.id);
					expect(recordings[2].payload.previous_response_id).toBeUndefined();
				}
			}, 15000);
		}
	}

	test("a rejected WebSocket upgrade falls back to SSE with the global tier policy", async () => {
		rejectWebSockets = true;
		const model = { ...codex, baseUrl: new URL("v1", server.url).href };
		const preference = new GlobalFastModePreference(join(directory, "fallback"));
		const ctx = { hasUI: false, model } as any;
		const sessionId = "offline-fallback";
		for (const enabled of [true, false]) {
			preference.preference.set(enabled);
			const result = await codexResponses.stream(model, { messages: [] }, {
				apiKey: inertKey, transport: "websocket", sessionId, maxRetries: 0,
				signal: AbortSignal.timeout(5000), websocketConnectTimeoutMs: 1000, timeoutMs: 3000,
				env: { HTTP_PROXY: "", HTTPS_PROXY: "", ALL_PROXY: "", NO_PROXY: "*" },
				onPayload: (payload: unknown) => preference.apply(payload, ctx),
				fetch: (url: string | URL | Request, options: RequestInit) => {
					expect(new URL(String(url)).hostname).toBe("127.0.0.1");
					return fetch(url, options);
				},
			}).result();
			expect(result.stopReason).toBe("stop");
			expect(recordings.at(-1)?.transport).toBe("sse");
			expect(recordings.at(-1)?.payload.service_tier).toBe(enabled ? "priority" : undefined);
		}
		expect(recordings).toHaveLength(2);
		expect(upgrades).toBeGreaterThan(0);
		expect(codexResponses.getOpenAICodexWebSocketDebugStats(sessionId)?.sseFallbacks).toBeGreaterThan(0);
	}, 15000);
});
