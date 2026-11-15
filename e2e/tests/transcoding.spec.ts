import { test, expect } from "@playwright/test";

import { LOCAL_USER, POLARIS_URL, authorization, login } from "./support/polaris";

const FLAC = `${POLARIS_URL}/api/audio/${encodeURIComponent("formats/sample.flac")}`;

test("reports transcoding as available", async ({ request }) => {
	const token = await login(request, LOCAL_USER);
	const response = await request.get(`${POLARIS_URL}/api/transcoding`, { headers: authorization(token) });
	expect(await response.json()).toEqual({ available: true, formats: ["mp3", "opus", "aac"] });
});

for (const [format, contentType, magic] of [
	["mp3", "audio/mpeg", "ID3"],
	["opus", "audio/ogg", "OggS"],
] as const) {
	test(`transcodes to ${format} and supports seeking`, async ({ request }) => {
		const token = await login(request, LOCAL_USER);
		const headers = authorization(token);
		const params = { format, bitrate: 48 };

		const full = await request.get(FLAC, { headers, params });
		expect(full.status()).toBe(200);
		expect(full.headers()["content-type"]).toBe(contentType);
		const body = await full.body();
		expect(body.subarray(0, magic.length).toString()).toBe(magic);

		const partial = await request.get(FLAC, { headers: { ...headers, Range: "bytes=200-299" }, params });
		expect(partial.status()).toBe(206);
		expect((await partial.body()).equals(body.subarray(200, 300))).toBe(true);
	});
}

test("serves the original file when no format is requested", async ({ request }) => {
	const token = await login(request, LOCAL_USER);
	const response = await request.get(FLAC, { headers: authorization(token) });
	expect(response.status()).toBe(200);
	expect((await response.body()).subarray(0, 4).toString()).toBe("fLaC");
});
