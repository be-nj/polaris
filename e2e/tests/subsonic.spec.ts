import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { test, expect } from "@playwright/test";
import SubsonicAPI from "subsonic-api";

import { LOCAL_USER, POLARIS_URL, login } from "./support/polaris";

// These tests drive Polaris with a third-party Subsonic client library, the same way
// Subsonic applications do.

const testData = (path: string) => fileURLToPath(new URL(`../../test-data/${path}`, import.meta.url));

async function makeClient(request: Parameters<typeof login>[0]): Promise<SubsonicAPI> {
	// Polaris authentication tokens double as Subsonic API keys
	const apiKey = await login(request, LOCAL_USER);
	return new SubsonicAPI({ url: POLARIS_URL, auth: { apiKey } });
}

test("rejects token authentication with an explanation", async () => {
	// Clients sending salted password hashes cannot be supported, since Polaris only stores
	// hashes of passwords. They are told so with the dedicated Subsonic error code.
	const client = new SubsonicAPI({
		url: POLARIS_URL,
		auth: { username: LOCAL_USER.name, password: LOCAL_USER.password },
	});
	const response = await (await client.custom("ping", {})).json();
	expect(response["subsonic-response"].status).toBe("failed");
	expect(response["subsonic-response"].error.code).toBe(41);
});

test("accepts plain password authentication", async ({ request }) => {
	const response = await request.get(`${POLARIS_URL}/rest/ping.view`, {
		params: { u: LOCAL_USER.name, p: LOCAL_USER.password, v: "1.16.1", c: "e2e", f: "json" },
	});
	expect((await response.json())["subsonic-response"].status).toBe("ok");
});

test("identifies as an OpenSubsonic server", async ({ request }) => {
	const client = await makeClient(request);
	const pong = await client.ping();
	expect(pong.status).toBe("ok");
	expect(pong.openSubsonic).toBe(true);
	expect(pong.type).toBe("polaris");

	const { openSubsonicExtensions } = await client.getOpenSubsonicExtensions();
	expect(openSubsonicExtensions.map((e) => e.name)).toContain("apiKeyAuthentication");

	const { tokenInfo } = await client.tokenInfo();
	expect(tokenInfo.username).toBe(LOCAL_USER.name);
});

test("browses artists, albums and songs", async ({ request }) => {
	const client = await makeClient(request);

	const { artists } = await client.getArtists();
	const allArtists = (artists.index ?? []).flatMap((index) => index.artist ?? []);
	const khemmis = allArtists.find((a) => a.name === "Khemmis");
	expect(khemmis).toBeDefined();

	const { artist } = await client.getArtist({ id: khemmis!.id });
	expect(artist.album?.map((a) => a.name)).toEqual(["Hunted"]);

	const { album } = await client.getAlbum({ id: artist.album![0].id });
	expect(album.song?.map((s) => s.title)).toEqual([
		"Above The Water",
		"Candlelight",
		"Three Gates",
		"Beyond The Door",
		"Hunted",
	]);

	const { albumList2 } = await client.getAlbumList2({ type: "alphabeticalByName", size: 100 });
	expect(albumList2.album?.map((a) => a.name)).toEqual(expect.arrayContaining(["Hunted", "Picnic"]));

	const { genres } = await client.getGenres();
	expect(genres.genre?.map((g) => g.value)).toContain("Metal");
});

test("browses folders", async ({ request }) => {
	const client = await makeClient(request);

	const { musicFolders } = await client.getMusicFolders();
	const folder = musicFolders.musicFolder?.find((f) => f.name === "collection");
	expect(folder).toBeDefined();

	const { indexes } = await client.getIndexes({ musicFolderId: String(folder!.id) });
	const directories = (indexes.index ?? []).flatMap((index) => index.artist ?? []);
	const tobokegao = directories.find((d) => d.name === "Tobokegao");
	expect(tobokegao).toBeDefined();

	const { directory } = await client.getMusicDirectory({ id: tobokegao!.id });
	expect(directory.child?.map((c) => c.title).sort()).toEqual(["Picnic", "Picnic (Remixes)"]);
});

test("searches", async ({ request }) => {
	const client = await makeClient(request);
	const { searchResult3 } = await client.search3({ query: "candlelight" });
	expect(searchResult3.song?.map((s) => s.title)).toEqual(["Candlelight"]);
});

test("streams songs and cover art", async ({ request }) => {
	const client = await makeClient(request);
	const { searchResult3 } = await client.search3({ query: "candlelight" });
	const song = searchResult3.song![0];

	const original = readFileSync(testData("small-collection/Khemmis/Hunted/02 - Candlelight.mp3"));
	const stream = await client.stream({ id: song.id });
	expect(stream.headers.get("content-type")).toBe("audio/mpeg");
	expect(Buffer.from(await stream.arrayBuffer()).equals(original)).toBe(true);

	const download = await client.download({ id: song.id });
	expect(download.headers.get("content-disposition")).toContain("attachment");
	expect(Buffer.from(await download.arrayBuffer()).equals(original)).toBe(true);

	const coverArt = await client.getCoverArt({ id: song.coverArt!, size: 64 });
	expect(coverArt.headers.get("content-type")).toBe("image/jpeg");
	expect((await coverArt.arrayBuffer()).byteLength).toBeGreaterThan(500);
});

test("limits the bitrate of streams", async ({ request }) => {
	const client = await makeClient(request);
	const { searchResult3 } = await client.search3({ query: "", songCount: 500 });
	const flac = searchResult3.song!.find((s) => s.suffix === "flac");
	expect(flac).toBeDefined();
	const original = readFileSync(testData("formats/sample.flac"));

	const untouched = await client.stream({ id: flac!.id });
	expect(untouched.headers.get("content-type")).toBe("audio/flac");
	expect(Buffer.from(await untouched.arrayBuffer()).equals(original)).toBe(true);

	const limited = await client.stream({ id: flac!.id, maxBitRate: 64 });
	expect(limited.headers.get("content-type")).toBe("audio/mpeg");
	const transcoded = Buffer.from(await limited.arrayBuffer());
	expect(transcoded.subarray(0, 3).toString()).toBe("ID3");

	const opus = await client.stream({ id: flac!.id, maxBitRate: 64, format: "opus" });
	expect(opus.headers.get("content-type")).toBe("audio/ogg");
	expect(Buffer.from(await opus.arrayBuffer()).subarray(0, 4).toString()).toBe("OggS");
});

test("manages playlists", async ({ request }) => {
	const client = await makeClient(request);
	const name = `e2e-${Date.now()}`;
	const { searchResult3 } = await client.search3({ query: "hunted", songCount: 500 });
	const songIds = searchResult3.song!.map((s) => s.id);
	expect(songIds.length).toBeGreaterThanOrEqual(3);

	const { playlist: created } = await client.createPlaylist({ name, songId: songIds.slice(0, 3) });
	expect(created.name).toBe(name);
	expect(created.entry?.map((e) => e.id)).toEqual(songIds.slice(0, 3));

	const { playlists } = await client.getPlaylists();
	expect(playlists.playlist?.map((p) => p.name)).toContain(name);

	await client.updatePlaylist({ playlistId: created.id, songIndexToRemove: [0] });
	const { playlist: updated } = await client.getPlaylist({ id: created.id });
	expect(updated.entry?.map((e) => e.id)).toEqual(songIds.slice(1, 3));

	await client.deletePlaylist({ id: created.id });
	const afterDelete = await client.getPlaylists();
	expect(afterDelete.playlists.playlist?.map((p) => p.name) ?? []).not.toContain(name);
});
