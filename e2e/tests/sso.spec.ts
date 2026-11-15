import { createHash, randomBytes } from "node:crypto";

import { test, expect, type Page } from "@playwright/test";

import {
	POLARIS_URL,
	SONG_PAGE,
	authorization,
	expectSignedIn,
	expectSongPage,
	readSession,
} from "./support/polaris";

interface Provider {
	/** Name of the provider in the Polaris configuration */
	name: string;
	/** Label of the provider on the Polaris login page */
	displayName: string;
	/** Fills and submits the login form of the provider */
	signIn: (page: Page, username: string, password: string) => Promise<void>;
}

const providers: Provider[] = [
	{
		name: "keycloak",
		displayName: "Keycloak",
		signIn: async (page, username, password) => {
			await page.locator("#username").fill(username);
			await page.locator("#password").fill(password);
			await page.locator("#kc-login").click();
		},
	},
	{
		name: "authelia",
		displayName: "Authelia",
		signIn: async (page, username, password) => {
			await page.locator("#username-textfield").fill(username);
			await page.locator("#password-textfield").fill(password);
			await page.locator("#sign-in-button").click();
		},
	},
	{
		name: "authentik",
		displayName: "authentik",
		signIn: async (page, username, password) => {
			// authentik asks for the username and the password on consecutive screens
			await page.locator('input[name="uidField"]').fill(username);
			await page.getByRole("button", { name: "Log in" }).click();
			const passwordField = page.locator('input[name="password"]:visible');
			await passwordField.waitFor();
			// The password screen ignores input until it has finished loading
			await page.waitForLoadState("networkidle");
			await passwordField.fill(password);
			await page.getByRole("button", { name: "Continue" }).click();
		},
	},
];

/** What an app does before sending the user to the browser: makes up a secret, and derives a challenge from it. */
function makeCodeVerifier() {
	const codeVerifier = randomBytes(32).toString("base64url");
	const codeChallenge = createHash("sha256").update(codeVerifier).digest("base64url");
	return { codeVerifier, codeChallenge };
}

const enabledProviders = (process.env.E2E_PROVIDERS ?? providers.map((p) => p.name).join(",")).split(",");

for (const provider of providers.filter((p) => enabledProviders.includes(p.name))) {
	test.describe(`single sign-on with ${provider.displayName}`, () => {
		async function startSignIn(page: Page) {
			await page.goto("/");
			await page.getByRole("link", { name: `Sign in with ${provider.displayName}` }).click();
		}

		test("signs in administrators", async ({ page, request }) => {
			await startSignIn(page);
			await provider.signIn(page, "alice", "alice-password");

			const session = await expectSignedIn(page);
			expect(session.name).toBe("alice");
			expect(session.isAdmin).toBe(true);

			// The session is accepted by the API, with administrator rights
			const settings = await request.get(`${POLARIS_URL}/api/settings`, {
				headers: authorization(session.authToken),
			});
			expect(settings.status()).toBe(200);

			// The web client stays signed in across visits
			await page.goto("/");
			await page.waitForURL("**/files");
		});

		test("signs in regular users", async ({ page, request }) => {
			await startSignIn(page);
			await provider.signIn(page, "bob", "bob-password");

			const session = await expectSignedIn(page);
			expect(session.name).toBe("bob");
			expect(session.isAdmin).toBe(false);

			const settings = await request.get(`${POLARIS_URL}/api/settings`, {
				headers: authorization(session.authToken),
			});
			expect(settings.status()).toBe(403);

			const albums = await request.get(`${POLARIS_URL}/api/albums`, {
				headers: authorization(session.authToken),
			});
			expect(albums.status()).toBe(200);
			expect((await albums.json()).length).toBeGreaterThan(0);
		});

		test("rejects users without an allowed role", async ({ page }) => {
			await startSignIn(page);
			await provider.signIn(page, "mallory", "mallory-password");

			await expect(page.getByRole("heading", { name: "Could not sign in" })).toBeVisible();
			await expect(page.getByText("not allowed to use this server")).toBeVisible();

			await page.getByRole("link", { name: "Back to Polaris" }).click();
			await page.waitForURL("**/auth");
		});

		test("returns to the page requested before signing in", async ({ page }) => {
			// Opening a page of the web client while signed out leads to the login page
			await page.goto(SONG_PAGE);
			await page.getByRole("link", { name: `Sign in with ${provider.displayName}` }).click();
			await provider.signIn(page, "bob", "bob-password");
			await expectSongPage(page);

			// The requested page is only returned to once
			await page.goto("/");
			await page.waitForURL("**/files");
		});

		test("returns to a location passed in a direct link", async ({ page }) => {
			await page.goto(`/api/oidc/${provider.name}/start?return_to=${encodeURIComponent(SONG_PAGE)}`);
			await provider.signIn(page, "bob", "bob-password");
			await expectSongPage(page);
		});

		test("does not redirect to other websites", async ({ page }) => {
			for (const returnTo of ["https://evil.example.com/", "//evil.example.com/", "/\\evil.example.com/"]) {
				await page.context().clearCookies();
				await page.goto(`/api/oidc/${provider.name}/start?return_to=${encodeURIComponent(returnTo)}`);
				await provider.signIn(page, "bob", "bob-password");
				await expectSignedIn(page);
				expect(new URL(page.url()).origin).toBe("http://polaris.test:5050");
			}
		});

		/** Signs in the way apps do, up to the link which hands the sign-in over to the app. Returns the code it carries. */
		async function signInForApp(page: Page, codeChallenge: string): Promise<string> {
			await page.goto(`/api/oidc/${provider.name}/start?code_challenge=${codeChallenge}`);
			await provider.signIn(page, "bob", "bob-password");

			await expect(page.getByRole("heading", { name: "Signed in as bob" })).toBeVisible();
			const href = await page.getByRole("link", { name: "Open Polaris" }).getAttribute("href");
			const link = new URL(href!);
			expect(link.protocol).toBe("polaris:");
			expect(link.host).toBe("sso");
			expect([...link.searchParams.keys()]).toEqual(["code"]);

			// The session is meant for the app, the browser itself is not signed in
			expect(await readSession(page)).toBeNull();

			return link.searchParams.get("code")!;
		}

		test("signs in apps", async ({ page, request }) => {
			const { codeVerifier, codeChallenge } = makeCodeVerifier();
			const code = await signInForApp(page, codeChallenge);

			const exchange = { data: { code, code_verifier: codeVerifier } };
			const response = await request.post(`${POLARIS_URL}/api/oidc/token`, exchange);
			expect(response.status()).toBe(200);
			const session = await response.json();
			expect(session.username).toBe("bob");
			expect(session.is_admin).toBe(false);

			const albums = await request.get(`${POLARIS_URL}/api/albums`, { headers: authorization(session.token) });
			expect(albums.status()).toBe(200);
			expect((await albums.json()).length).toBeGreaterThan(0);

			// Codes can only be used once
			const replay = await request.post(`${POLARIS_URL}/api/oidc/token`, exchange);
			expect(replay.status()).toBe(400);
		});

		test("does not sign in apps which did not start the sign-in", async ({ page, request }) => {
			const { codeVerifier, codeChallenge } = makeCodeVerifier();
			const code = await signInForApp(page, codeChallenge);

			// Something which got hold of the code, but does not know the secret of the app
			for (const guess of [makeCodeVerifier().codeVerifier, codeChallenge, ""]) {
				const exchange = { data: { code, code_verifier: guess } };
				const response = await request.post(`${POLARIS_URL}/api/oidc/token`, exchange);
				expect(response.status()).toBe(400);
			}

			// The failed attempts used up the code
			const exchange = { data: { code, code_verifier: codeVerifier } };
			const response = await request.post(`${POLARIS_URL}/api/oidc/token`, exchange);
			expect(response.status()).toBe(400);
		});

		test("does not sign in apps of users without an allowed role", async ({ page }) => {
			const { codeChallenge } = makeCodeVerifier();
			await page.goto(`/api/oidc/${provider.name}/start?code_challenge=${codeChallenge}`);
			await provider.signIn(page, "mallory", "mallory-password");

			await expect(page.getByRole("heading", { name: "Could not sign in" })).toBeVisible();
			await expect(page.getByRole("link", { name: "Open Polaris" })).toHaveCount(0);
		});

		test("can be started from a direct link", async ({ page }) => {
			await page.goto(`/api/oidc/${provider.name}/start`);
			await provider.signIn(page, "bob", "bob-password");
			await expectSignedIn(page);
		});
	});
}
