export default function (pi) {
	let fastMode = ["on", "1", "true"].includes(process.env.PI_CODEX_FAST_MODE?.toLowerCase() ?? "");

	const syncEnvironment = () => {
		process.env.PI_CODEX_FAST_MODE = fastMode ? "on" : "off";
	};

	const setStatus = (ctx) => {
		if (!ctx.hasUI) return;
		ctx.ui.setStatus("fast-mode", fastMode ? "⚡ fast" : undefined);
	};

	syncEnvironment();

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

			syncEnvironment();
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
		if (ctx.model.api !== "openai-codex-responses" &&
			!(ctx.model.api === "pi-virtual" && ctx.model.id === "jev-auto")) return;
		if (!event.payload || typeof event.payload !== "object" || Array.isArray(event.payload)) return;

		return {
			...event.payload,
			service_tier: "priority",
		};
	});
}
