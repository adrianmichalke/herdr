export default function (pi) {
	let fastMode = false;

	const setStatus = (ctx) => {
		if (!ctx.hasUI) return;
		ctx.ui.setStatus("fast-mode", fastMode ? "⚡ fast" : undefined);
	};

	pi.on("session_start", async (_event, ctx) => {
		setStatus(ctx);
	});

	pi.registerCommand("fast", {
		description: "Toggle Codex fast mode",
		handler: async (args, ctx) => {
			const value = args.trim().toLowerCase();

			if (value === "" || value === "toggle") {
				fastMode = !fastMode;
			} else if (value === "on" || value === "1" || value === "true") {
				fastMode = true;
			} else if (value === "off" || value === "0" || value === "false") {
				fastMode = false;
			} else if (value !== "status") {
				throw new Error('Usage: /fast [on|off|toggle|status]');
			}

			setStatus(ctx);
			if (ctx.hasUI) {
				ctx.ui.notify(`Fast mode ${fastMode ? "on" : "off"}`, "info");
			}
		},
	});

	pi.on("before_provider_request", async (event, ctx) => {
		if (!fastMode) return;
		if (!ctx.model) return;
		if (ctx.model.provider !== "openai-codex") return;
		if (ctx.model.api !== "openai-codex-responses") return;
		if (!event.payload || typeof event.payload !== "object" || Array.isArray(event.payload)) return;

		return {
			...event.payload,
			service_tier: "priority",
		};
	});
}
