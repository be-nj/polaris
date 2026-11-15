import { test, expect } from "@playwright/test";

import { LOCAL_USER, POLARIS_URL, SONG_PAGE, expectSignedIn, expectSongPage } from "./support/polaris";

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

test("returns to the page requested before signing in with a password", async ({ page }) => {
	await page.goto(SONG_PAGE);
	await page.getByLabel("username").fill(LOCAL_USER.name);
	await page.getByLabel("password").fill(LOCAL_USER.password);
	await page.getByTestId("submit-login").click();
	await expectSongPage(page);

	// The requested page is only returned to once
	await page.goto("/");
	await page.waitForURL("**/files");
});

test("shows pictures of artists", async ({ page }) => {
	await page.goto("/#/artists/Khemmis");
	await page.getByLabel("username").fill(LOCAL_USER.name);
	await page.getByLabel("password").fill(LOCAL_USER.password);
	await page.getByTestId("submit-login").click();

	// The picture comes from the folder holding the albums of the artist
	await expect(page.getByRole("heading", { name: "Khemmis" })).toBeVisible();
	const picture = page.getByTestId("artist-picture");
	await expect(picture).toBeVisible();
	await expect.poll(() => picture.evaluate((image: HTMLImageElement) => image.naturalWidth)).toBeGreaterThan(0);

	// Artists without a picture are shown as before
	await page.goto("/#/artists/Tobokegao");
	await expect(page.getByRole("heading", { name: "Tobokegao" })).toBeVisible();
	await expect(page.getByText("Picnic").first()).toBeVisible();
	await expect(page.getByTestId("artist-picture")).toHaveCount(0);
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
