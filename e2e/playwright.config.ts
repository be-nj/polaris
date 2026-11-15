import { defineConfig } from "@playwright/test";

// Services are published on the machine running the tests, but identify themselves by
// the host names they have inside the Docker network. The browser is told to resolve
// these names locally, so that every party agrees on the same URLs.
const hostResolverRules = "MAP *.test 127.0.0.1";
// None of the services can be reached through a proxy the test machine may be configured with.
const proxy = "--proxy-server=direct://";

export default defineConfig({
	testDir: "./tests",
	globalSetup: "./tests/support/global-setup.ts",
	timeout: 60_000,
	expect: { timeout: 10_000 },
	forbidOnly: !!process.env.CI,
	retries: process.env.CI ? 1 : 0,
	workers: 4,
	reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
	use: {
		baseURL: "http://polaris.test:5050",
		ignoreHTTPSErrors: true,
		testIdAttribute: "data-pw",
		trace: "retain-on-failure",
		screenshot: "only-on-failure",
		launchOptions: {
			args: [`--host-resolver-rules=${hostResolverRules}`, proxy],
		},
	},
	projects: [{ name: "chromium", use: { browserName: "chromium" } }],
});
