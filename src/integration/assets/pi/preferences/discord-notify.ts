// HERDR_PI_PREFERENCES=1
import { getAgentDir, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import { GlobalBooleanPreference, type PreferenceSnapshot } from "./global-boolean.ts";

export class DiscordNotificationPreference {
	readonly preference: GlobalBooleanPreference;
	private context?: ExtensionContext;
	private unsubscribe?: () => void;
	private rendered?: { context: ExtensionContext; fingerprint: string };

	constructor(agentDir = getAgentDir()) {
		this.preference = new GlobalBooleanPreference(agentDir, "discord-notify");
	}

	private render(value: PreferenceSnapshot): void {
		const ctx = this.context;
		if (!ctx?.hasUI) return;
		const fingerprint = JSON.stringify(value);
		if (this.rendered?.context === ctx && this.rendered.fingerprint === fingerprint) return;
		const label = value.error ? "discord:global:off!" : `discord:global:${value.enabled ? "on" : "off"}`;
		ctx.ui.setStatus("discord-notify", ctx.ui.theme.fg(value.error ? "error" : value.enabled ? "accent" : "dim", label));
		this.rendered = { context: ctx, fingerprint };
	}

	sync(ctx: ExtensionContext): boolean {
		this.context = ctx;
		const value = this.preference.read();
		this.render(value);
		return value.enabled;
	}

	start(ctx: ExtensionContext): boolean {
		this.stop();
		this.context = ctx;
		this.unsubscribe = this.preference.subscribe((value) => this.render(value));
		return this.sync(ctx);
	}

	stop(): void {
		this.unsubscribe?.();
		this.unsubscribe = undefined;
		this.context = undefined;
		this.rendered = undefined;
	}

	set(enabled: boolean): void {
		this.preference.set(enabled);
	}

	describe(): string {
		const value = this.preference.read();
		if (value.error) return "Global automatic Discord notifications are disabled: preference unavailable or invalid";
		return `Global automatic Discord notifications are ${value.enabled ? "enabled" : "disabled"}`;
	}
}
