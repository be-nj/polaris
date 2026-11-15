import { expect, type APIRequestContext, type Page } from "@playwright/test";

/** Address of the Polaris API for requests made by the tests themselves (not by a browser) */
export const POLARIS_URL = process.env.POLARIS_URL ?? "http://127.0.0.1:5050";

export const LOCAL_ADMIN = { name: "local-admin", password: "local-admin-password" };
export const LOCAL_USER = { name: "local-user", password: "local-user-password" };

export interface Session {
	name: string;
	authToken: string;
	isAdmin: boolean;
}

/** Signs in through the Polaris API, like the web client does with a username and password. */
export async function login(request: APIRequestContext, user: { name: string; password: string }): Promise<string> {
	const response = await request.post(`${POLARIS_URL}/api/auth`, {
		data: { username: user.name, password: user.password },
	});
	expect(response.status()).toBe(200);
	return (await response.json()).token;
}

export function authorization(token: string) {
	return { Authorization: `Bearer ${token}`, "Accept-Version": "8" };
}

/** Reads the session the web client is currently using, if any. */
export async function readSession(page: Page): Promise<Session | null> {
	return page.evaluate(() => JSON.parse(localStorage.getItem("current_user") ?? "null"));
}

/** Waits for the web client to show the collection, which only signed in users can see. */
export async function expectSignedIn(page: Page): Promise<Session> {
	await page.waitForURL("**/files");
	const session = await readSession(page);
	expect(session).not.toBeNull();
	return session!;
}
