import { test, expect, type Page } from "@playwright/test";

import { POLARIS_URL, SONG_PAGE, authorization, expectSignedIn, expectSongPage } from "./support/polaris";

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

		test("can be started from a direct link", async ({ page }) => {
			await page.goto(`/api/oidc/${provider.name}/start`);
			await provider.signIn(page, "bob", "bob-password");
			await expectSignedIn(page);
		});
	});
}
