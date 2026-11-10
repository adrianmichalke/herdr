// HERDR_PI_PREFERENCES=1
import { randomUUID } from "node:crypto";
import { closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

export type PreferenceSnapshot = { enabled: boolean; error?: string };

export class GlobalBooleanPreference {
	readonly path: string;

	constructor(agentDir: string, name: string) {
		if (!/^[a-z][a-z0-9-]*$/.test(name)) throw new Error("Invalid global preference name");
		this.path = join(agentDir, "preferences", `${name}.json`);
	}

	read(): PreferenceSnapshot {
		try {
			const stat = lstatSync(this.path);
			if (!stat.isFile() || stat.size > 4096) return { enabled: false, error: "invalid preference file" };
			const value: unknown = JSON.parse(readFileSync(this.path, "utf8"));
			if (!value || typeof value !== "object" || Array.isArray(value) || typeof (value as { enabled?: unknown }).enabled !== "boolean") {
				return { enabled: false, error: "invalid preference value" };
			}
			return { enabled: (value as { enabled: boolean }).enabled };
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") return { enabled: false };
			return { enabled: false, error: "preference could not be read" };
		}
	}

	set(enabled: boolean): void {
		if (typeof enabled !== "boolean") throw new Error("Global preference must be a boolean");
		const directory = dirname(this.path);
		mkdirSync(directory, { recursive: true, mode: 0o700 });
		try {
			const stat = lstatSync(this.path);
			if (!stat.isFile() || stat.nlink > 1) throw new Error("Global preference is not a private regular file");
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
		}
		const temporary = join(directory, `.${process.pid}-${randomUUID()}.tmp`);
		try {
			const file = openSync(temporary, "wx", 0o600);
			try {
				writeFileSync(file, `${JSON.stringify({ enabled })}\n`, "utf8");
				fsyncSync(file);
			} finally {
				closeSync(file);
			}
			renameSync(temporary, this.path);
		} finally {
			try {
				unlinkSync(temporary);
			} catch (error) {
				if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
			}
		}
	}

	subscribe(listener: (value: PreferenceSnapshot) => void, intervalMs = 1000): () => void {
		const refresh = () => listener(this.read());
		refresh();
		const timer = setInterval(refresh, intervalMs);
		timer.unref();
		return () => clearInterval(timer);
	}
}
