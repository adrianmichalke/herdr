// HERDR_PI_PREFERENCES=1
import { getAgentDir, type ExtensionAPI, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import { GlobalBooleanPreference, type PreferenceSnapshot } from "./global-boolean.ts";

type RequestModel = { provider: string; api: string; id: string };

export function supportsPriority(model: RequestModel | undefined): boolean {
	return model?.provider === "openai-codex" && model.api === "openai-codex-responses"
		|| model?.provider === "openai" && model.api === "openai-responses";
}

function requestModel(ctx: ExtensionContext, payload: Record<string, unknown>): RequestModel | undefined {
	const selected = ctx.model;
	if (!selected || typeof payload.model !== "string") return;
	if (payload.model === selected.id && supportsPriority(selected)) return selected;
	const codexRouter = selected.api === "pi-virtual" && selected.provider === "openai-codex" && selected.id === "jev-auto";
	if (!supportsPriority(selected) && !codexRouter) return;
	const physical = ctx.modelRegistry.find(selected.provider, payload.model);
	return supportsPriority(physical) ? physical : undefined;
}

export class GlobalFastModePreference {
	readonly preference: GlobalBooleanPreference;
	private context?: ExtensionContext;
	private unsubscribe?: () => void;
	private rendered?: { context: ExtensionContext; fingerprint: string };

	constructor(agentDir = getAgentDir()) {
		this.preference = new GlobalBooleanPreference(agentDir, "fast-mode");
	}

	private applicable(ctx: ExtensionContext): boolean {
		return supportsPriority(ctx.model)
			|| ctx.model?.provider === "openai-codex" && ctx.model.api === "pi-virtual" && ctx.model.id === "jev-auto";
	}

	private render(ctx: ExtensionContext, value: PreferenceSnapshot): void {
		try {
			if (!ctx.hasUI) return;
			const applicable = this.applicable(ctx);
			const fingerprint = JSON.stringify([value, applicable]);
			if (this.rendered?.context === ctx && this.rendered.fingerprint === fingerprint) return;
			const label = value.error ? "fast:global:off!" : `fast:global:${value.enabled ? "on" : "off"}${applicable ? "" : " (n/a)"}`;
			ctx.ui.setStatus("fast-mode", ctx.ui.theme.fg(value.error ? "error" : value.enabled && applicable ? "accent" : "dim", label));
			this.rendered = { context: ctx, fingerprint };
		} catch {
			this.rendered = undefined;
		}
	}

	sync(ctx: ExtensionContext): PreferenceSnapshot {
		const value = this.preference.read();
		this.context = ctx;
		this.render(ctx, value);
		return value;
	}

	start(ctx: ExtensionContext): void {
		this.stop();
		this.context = ctx;
		this.unsubscribe = this.preference.subscribe((value) => {
			if (this.context) this.render(this.context, value);
		});
	}

	stop(): void {
		this.unsubscribe?.();
		this.unsubscribe = undefined;
		this.context = undefined;
		this.rendered = undefined;
	}

	describe(ctx: ExtensionContext): string {
		const value = this.sync(ctx);
		if (value.error) return "Global fast mode is off: preference unavailable or invalid; provider default tier is used";
		const state = value.enabled ? "on (priority tier requested; higher pricing may apply)" : "off (provider default tier)";
		return `Global fast mode is ${state}${this.applicable(ctx) ? "" : "; not applicable to the selected provider/model (preference retained)"}`;
	}

	apply(payload: unknown, ctx: ExtensionContext): unknown {
		const value = this.sync(ctx);
		if (!payload || typeof payload !== "object" || Array.isArray(payload)) return;
		const body = payload as Record<string, unknown>;
		if (!requestModel(ctx, body)) return;
		if (value.enabled) return { ...body, service_tier: "priority" };
		if (!("service_tier" in body)) return;
		const { service_tier: _tier, ...normal } = body;
		return normal;
	}
}

export default function (pi: ExtensionAPI) {
	const preference = new GlobalFastModePreference();
	pi.on("session_start", (_event, ctx) => { preference.start(ctx); });
	pi.on("session_shutdown", () => { preference.stop(); });
	pi.on("session_tree", (_event, ctx) => { preference.sync(ctx); });
	pi.on("model_select", (_event, ctx) => { preference.sync(ctx); });
	pi.on("before_provider_request", (event, ctx) => preference.apply(event.payload, ctx));
	pi.registerCommand("fast", {
		description: "Control global OpenAI/Codex priority mode (higher pricing may apply)",
		getArgumentCompletions(prefix) {
			const items = ["on", "off", "toggle", "status"].filter((value) => value.startsWith(prefix));
			return items.length ? items.map((value) => ({ value, label: value })) : null;
		},
		handler: async (args, ctx) => {
			const action = args.trim().toLowerCase();
			if (!["", "toggle", "on", "1", "true", "off", "0", "false", "status"].includes(action)) {
				if (ctx.hasUI) ctx.ui.notify("Usage: /fast [on|off|toggle|status]", "error");
				return;
			}
			if (action !== "status") {
				const current = preference.sync(ctx);
				const enabled = ["on", "1", "true"].includes(action) || ["", "toggle"].includes(action) && !current.enabled;
				try {
					preference.preference.set(enabled);
				} catch {
					preference.sync(ctx);
					if (ctx.hasUI) ctx.ui.notify("Could not save the global fast-mode preference; no change was applied", "error");
					return;
				}
			}
			const description = preference.describe(ctx);
			if (ctx.hasUI) ctx.ui.notify(description, "info");
		},
	});
}
