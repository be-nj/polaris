import { test, expect } from "@playwright/test";

import { LOCAL_USER, POLARIS_URL, expectSignedIn } from "./support/polaris";

test("signs in with a password and shows the collection", async ({ page }) => {
	await page.goto("/");
	await page.getByLabel("username").fill(LOCAL_USER.name);
	await page.getByLabel("password").fill(LOCAL_USER.password);
	await page.getByTestId("submit-login").click();

	const session = await expectSignedIn(page);
	expect(session.name).toBe(LOCAL_USER.name);
	expect(session.isAdmin).toBe(false);

	// The file browser lists the mount directories of the collection
	await expect(page.getByText("collection").first()).toBeVisible();
	await expect(page.getByText("formats").first()).toBeVisible();
});

test("rejects wrong passwords", async ({ page }) => {
	await page.goto("/");
	await page.getByLabel("username").fill(LOCAL_USER.name);
	await page.getByLabel("password").fill("not the password");
	await page.getByTestId("submit-login").click();
	await expect(page.getByTestId("login-error")).toBeVisible();
});

test("offers every configured single sign-on provider", async ({ page }) => {
	await page.goto("/");
	const providers = await (await page.request.get(`${POLARIS_URL}/api/oidc/providers`)).json();
	expect(providers.length).toBeGreaterThan(0);
	for (const provider of providers) {
		await expect(page.getByRole("link", { name: `Sign in with ${provider.display_name}` })).toBeVisible();
	}
});
