import { request } from "@playwright/test";

import { POLARIS_URL, LOCAL_ADMIN } from "./polaris";

const SERVICE_TIMEOUT_MS = 5 * 60 * 1000;

async function waitFor(description: string, isReady: () => Promise<boolean>) {
	const deadline = Date.now() + SERVICE_TIMEOUT_MS;
	let lastError: unknown;
	while (Date.now() < deadline) {
		try {
			if (await isReady()) {
				return;
			}
		} catch (e) {
			lastError = e;
		}
		await new Promise((resolve) => setTimeout(resolve, 1000));
	}
	throw new Error(`Timed out waiting for ${description}: ${lastError ?? "not ready"}`);
}

/** Waits until Polaris has indexed the test collection. Other services are awaited by run.sh. */
export default async function globalSetup() {
	const context = await request.newContext({ ignoreHTTPSErrors: true });

	await waitFor("Polaris to index its collection", async () => {
		const login = await context.post(`${POLARIS_URL}/api/auth`, {
			data: { username: LOCAL_ADMIN.name, password: LOCAL_ADMIN.password },
		});
		if (!login.ok()) {
			return false;
		}
		const { token } = await login.json();
		const albums = await context.get(`${POLARIS_URL}/api/albums`, {
			headers: { Authorization: `Bearer ${token}`, "Accept-Version": "8" },
		});
		return albums.ok() && (await albums.json()).length > 0;
	});

	await context.dispose();
}
