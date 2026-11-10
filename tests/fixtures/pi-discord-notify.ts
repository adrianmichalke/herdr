import type { ExtensionAPI, ExtensionContext } from "@mariozechner/pi-coding-agent";
import { Type } from "@sinclair/typebox";

type DiscordNotifyState = {
	enabled?: boolean;
};

const FALLBACK_WEBHOOK_URL = "fixture-webhook";

async function sendDiscord(message: string) {
	const url = process.env.DISCORD_WEBHOOK_URL || FALLBACK_WEBHOOK_URL;
	if (!url) throw new Error("Discord webhook URL is not set");

	const content = message.length > 1900 ? message.slice(0, 1899) + "…" : message;

	const res = await fetch(url, {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({
			content,
			allowed_mentions: { parse: [] },
		}),
	});

	if (!res.ok) {
		throw new Error(`Discord webhook failed: ${res.status} ${await res.text()}`);
	}
}

function readyMessage(): string {
	return "ready";
}

function updateStatus(ctx: ExtensionContext, enabled: boolean): void {
	ctx.ui.setStatus("discord-notify", enabled ? ctx.ui.theme.fg("accent", "discord:on") : undefined);
}

function restoreEnabled(ctx: ExtensionContext): boolean {
	const stateEntry = ctx.sessionManager
		.getBranch()
		.filter((entry: { type: string; customType?: string }) => entry.type === "custom" && entry.customType === "discord-notify")
		.pop() as { data?: DiscordNotifyState } | undefined;
	return stateEntry?.data?.enabled === true;
}

export default function (pi: ExtensionAPI) {
	let notificationsEnabled = false;

	function persistState() {
		pi.appendEntry("discord-notify", { enabled: notificationsEnabled });
	}

	function syncState(ctx: ExtensionContext) {
		notificationsEnabled = restoreEnabled(ctx);
		updateStatus(ctx, notificationsEnabled);
	}

	pi.on("session_start", async (_event, ctx) => {
		syncState(ctx);
	});

	pi.on("session_switch", async (_event, ctx) => {
		syncState(ctx);
	});

	pi.on("session_fork", async (_event, ctx) => {
		syncState(ctx);
	});

	pi.on("session_tree", async (_event, ctx) => {
		syncState(ctx);
	});

	pi.registerCommand("discord", {
		description: "Send a message to Discord",
		handler: async (args, ctx) => {
			try {
				await sendDiscord(args.trim() || readyMessage());
				ctx.ui.notify("Sent to Discord", "info");
			} catch (err) {
				ctx.ui.notify(
					`Discord send failed: ${err instanceof Error ? err.message : String(err)}`,
					"error",
				);
			}
		},
	});

	pi.registerCommand("discord-notify", {
		description: "Enable or disable automatic Discord notifications",
		handler: async (args, ctx) => {
			const action = args.trim().toLowerCase();

			if (action === "status") {
				updateStatus(ctx, notificationsEnabled);
				ctx.ui.notify(
					notificationsEnabled ? "Discord notifications are enabled" : "Discord notifications are disabled",
					"info",
				);
				return;
			}

			if (action === "test") {
				try {
					await sendDiscord(readyMessage());
					ctx.ui.notify("Sent Discord test notification", "info");
				} catch (err) {
					ctx.ui.notify(
						`Discord send failed: ${err instanceof Error ? err.message : String(err)}`,
						"error",
					);
				}
				return;
			}

			if (action === "" || action === "toggle") {
				notificationsEnabled = !notificationsEnabled;
			} else if (action === "on" || action === "enable" || action === "enabled") {
				notificationsEnabled = true;
			} else if (action === "off" || action === "disable" || action === "disabled") {
				notificationsEnabled = false;
			} else {
				ctx.ui.notify("Usage: /discord-notify [on|off|toggle|status|test]", "info");
				return;
			}

			persistState();
			updateStatus(ctx, notificationsEnabled);
			ctx.ui.notify(
				notificationsEnabled ? "Discord notifications enabled" : "Discord notifications disabled",
				"info",
			);
		},
	});

	pi.on("agent_end", async (_event, ctx) => {
		if (!notificationsEnabled) return;
		try {
			await sendDiscord(readyMessage());
		} catch (err) {
			ctx.ui.notify(
				`Discord notification failed: ${err instanceof Error ? err.message : String(err)}`,
				"error",
			);
		}
	});

	pi.registerTool({
		name: "discord_send",
		label: "Discord Send",
		description: "Send a message to Discord via webhook",
		promptSnippet: "Send a short message to Discord via the configured webhook",
		promptGuidelines: ["Only use this tool when the user explicitly asks to send something to Discord or wants a Discord notification."],
		parameters: Type.Object({
			message: Type.String({ description: "Message text to send to Discord" }),
		}),
		async execute(_toolCallId, params) {
			await sendDiscord(params.message);
			return {
				content: [{ type: "text", text: "Sent to Discord." }],
				details: {},
			};
		},
	});
}
