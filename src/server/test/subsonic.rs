use http::{header, StatusCode};
use serde_json::Value;

use crate::server::dto;
use crate::server::test::{constants::*, protocol, ServiceType, TestService};
use crate::test_name;

async fn make_service(test_name: &str) -> ServiceType {
	let mut service = ServiceType::new(test_name).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.logout().await;
	service
}

async fn call_raw(service: &mut ServiceType, endpoint: &str, params: &[(&str, &str)]) -> Value {
	let mut all_params = vec![("v", "1.16.1"), ("c", "polaris-tests"), ("f", "json")];
	all_params.extend_from_slice(params);
	let request = protocol::subsonic(endpoint, &all_params);
	let response = service.fetch_json::<_, Value>(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	response.into_body()["subsonic-response"].clone()
}

async fn call_as(
	service: &mut ServiceType,
	credentials: (&str, &str),
	endpoint: &str,
	params: &[(&str, &str)],
) -> Value {
	let mut all_params = vec![("u", credentials.0), ("p", credentials.1)];
	all_params.extend_from_slice(params);
	call_raw(service, endpoint, &all_params).await
}

async fn call(service: &mut ServiceType, endpoint: &str, params: &[(&str, &str)]) -> Value {
	let response = call_as(service, (TEST_USERNAME, TEST_PASSWORD), endpoint, params).await;
	assert_eq!(response["status"], "ok", "{endpoint} failed: {response}");
	response
}

fn error_code(response: &Value) -> Option<u64> {
	assert_eq!(response["status"], "failed");
	response["error"]["code"].as_u64()
}

fn find<'a>(items: &'a Value, key: &str, value: &str) -> &'a Value {
	items
		.as_array()
		.unwrap_or_else(|| panic!("Expected an array, got {items}"))
		.iter()
		.find(|i| i[key] == value)
		.unwrap_or_else(|| panic!("No item with {key}={value} in {items}"))
}

async fn find_album(service: &mut ServiceType, name: &str) -> Value {
	let response = call(
		service,
		"getAlbumList2",
		&[("type", "alphabeticalByName"), ("size", "100")],
	)
	.await;
	find(&response["albumList2"]["album"], "name", name).clone()
}

#[tokio::test]
async fn rejects_invalid_credentials() {
	let mut service = make_service(&test_name!()).await;

	let response = call_raw(&mut service, "ping", &[]).await;
	assert_eq!(error_code(&response), Some(10));

	let response = call_as(
		&mut service,
		(TEST_USERNAME, "not the password"),
		"ping",
		&[],
	)
	.await;
	assert_eq!(error_code(&response), Some(40));

	let response = call_as(&mut service, ("nobody", TEST_PASSWORD), "ping", &[]).await;
	assert_eq!(error_code(&response), Some(40));

	let token_auth = [
		("u", TEST_USERNAME),
		("t", "26719a1196d2a940705a59634eb18eab"),
		("s", "c19b2d"),
	];
	let response = call_raw(&mut service, "ping", &token_auth).await;
	assert_eq!(error_code(&response), Some(41));

	let response = call_raw(&mut service, "ping", &[("apiKey", "not a key")]).await;
	assert_eq!(error_code(&response), Some(44));
}

#[tokio::test]
async fn accepts_passwords() {
	let mut service = make_service(&test_name!()).await;

	let response = call(&mut service, "ping", &[]).await;
	assert_eq!(response["version"], "1.16.1");
	assert_eq!(response["openSubsonic"], true);
	assert_eq!(response["type"], "polaris");

	// Repeated requests are served by the verified password cache
	call(&mut service, "ping.view", &[]).await;

	let hex_password = TEST_PASSWORD
		.bytes()
		.map(|b| format!("{b:02x}"))
		.collect::<String>();
	let credentials = (TEST_USERNAME, &format!("enc:{hex_password}")[..]);
	let response = call_as(&mut service, credentials, "ping", &[]).await;
	assert_eq!(response["status"], "ok");

	let response = call_as(&mut service, (TEST_USERNAME, "enc:zz"), "ping", &[]).await;
	assert_eq!(error_code(&response), Some(40));
}

#[tokio::test]
async fn accepts_polaris_tokens_as_api_keys() {
	let mut service = make_service(&test_name!()).await;

	let request = protocol::login(TEST_USERNAME, TEST_PASSWORD);
	let response = service.fetch_json::<_, dto::Authorization>(&request).await;
	let token = response.into_body().token;

	let response = call_raw(&mut service, "tokenInfo", &[("apiKey", &token)]).await;
	assert_eq!(response["status"], "ok");
	assert_eq!(response["tokenInfo"]["username"], TEST_USERNAME);

	let conflicting = [("apiKey", &token[..]), ("u", TEST_USERNAME)];
	let response = call_raw(&mut service, "ping", &conflicting).await;
	assert_eq!(error_code(&response), Some(43));
}

#[tokio::test]
async fn lists_extensions_without_authentication() {
	let mut service = make_service(&test_name!()).await;
	let response = call_raw(&mut service, "getOpenSubsonicExtensions", &[]).await;
	assert_eq!(response["status"], "ok");
	find(&response["openSubsonicExtensions"], "name", "formPost");
}

#[tokio::test]
async fn responds_with_xml_by_default() {
	let mut service = make_service(&test_name!()).await;
	let params = [("u", TEST_USERNAME), ("p", TEST_PASSWORD)];

	let request = protocol::subsonic("getMusicFolders", &params);
	let response = service.fetch_bytes(&request).await;
	assert_eq!(
		response.headers().get(header::CONTENT_TYPE).unwrap(),
		"application/xml; charset=utf-8"
	);
	let body = String::from_utf8(response.into_body()).unwrap();
	assert!(body.starts_with(r#"<?xml version="1.0" encoding="UTF-8"?><subsonic-response"#));
	assert!(body.contains(r#"status="ok""#));
	assert!(body.contains(&format!(
		r#"<musicFolders><musicFolder id="0" name="{TEST_MOUNT_NAME}"/></musicFolders>"#
	)));

	let request = protocol::subsonic("ping", &[("u", TEST_USERNAME), ("p", "nope")]);
	let body = String::from_utf8(service.fetch_bytes(&request).await.into_body()).unwrap();
	assert!(body.contains(r#"status="failed""#));
	assert!(body.contains(r#"<error code="40""#));
}

#[tokio::test]
async fn accepts_form_posts() {
	let mut service = make_service(&test_name!()).await;
	let params = [("u", TEST_USERNAME), ("p", TEST_PASSWORD), ("f", "json")];
	let request = protocol::subsonic_form_post("getLicense", &params);
	let (response, body) = service.send_binary(&request).await;
	assert_eq!(response.body(()).unwrap().status(), StatusCode::OK);
	let body: Value = serde_json::from_slice(&body.unwrap()).unwrap();
	assert_eq!(body["subsonic-response"]["license"]["valid"], true);
}

#[tokio::test]
async fn browses_by_tags() {
	let mut service = make_service(&test_name!()).await;

	let response = call(&mut service, "getArtists", &[]).await;
	let index = find(&response["artists"]["index"], "name", "K");
	let artist = find(&index["artist"], "name", "Khemmis");
	let artist_id = artist["id"].as_str().unwrap();

	let response = call(&mut service, "getArtist", &[("id", artist_id)]).await;
	assert_eq!(response["artist"]["name"], "Khemmis");
	let album = find(&response["artist"]["album"], "name", "Hunted");
	assert_eq!(album["songCount"], 5);
	assert_eq!(album["artist"], "Khemmis");
	assert_eq!(album["artistId"], artist_id);
	let album_id = album["id"].as_str().unwrap();

	let response = call(&mut service, "getAlbum", &[("id", album_id)]).await;
	let songs = response["album"]["song"].as_array().unwrap();
	assert_eq!(songs.len(), 5);
	let song = &songs[1];
	assert_eq!(song["title"], "Candlelight");
	assert_eq!(song["track"], 2);
	assert_eq!(song["album"], "Hunted");
	assert_eq!(song["albumId"], album_id);
	assert_eq!(song["suffix"], "mp3");
	assert_eq!(song["contentType"], "audio/mpeg");
	assert_eq!(song["isDir"], false);
	assert_eq!(
		song["path"],
		format!("{TEST_MOUNT_NAME}/Khemmis/Hunted/02 - Candlelight.mp3")
	);

	let song_id = song["id"].as_str().unwrap();
	let response = call(&mut service, "getSong", &[("id", song_id)]).await;
	assert_eq!(&response["song"], song);

	let response = call_as(
		&mut service,
		(TEST_USERNAME, TEST_PASSWORD),
		"getSong",
		&[("id", "so-bm90IGEgc29uZw")],
	)
	.await;
	assert_eq!(error_code(&response), Some(70));
}

#[tokio::test]
async fn browses_by_folders() {
	let mut service = make_service(&test_name!()).await;

	let response = call(&mut service, "getMusicFolders", &[]).await;
	let folder = find(
		&response["musicFolders"]["musicFolder"],
		"name",
		TEST_MOUNT_NAME,
	);
	assert_eq!(folder["id"], 0);

	let response = call(&mut service, "getIndexes", &[("musicFolderId", "0")]).await;
	let index = find(&response["indexes"]["index"], "name", "T");
	let directory = find(&index["artist"], "name", "Tobokegao");
	let directory_id = directory["id"].as_str().unwrap().to_owned();

	let response = call(&mut service, "getMusicDirectory", &[("id", &directory_id)]).await;
	assert_eq!(response["directory"]["name"], "Tobokegao");
	let children = &response["directory"]["child"];
	assert_eq!(children.as_array().unwrap().len(), 2);
	let album_directory = find(children, "title", "Picnic");
	assert_eq!(album_directory["isDir"], true);
	assert_eq!(album_directory["parent"], directory_id[..]);

	let album_directory_id = album_directory["id"].as_str().unwrap();
	let response = call(
		&mut service,
		"getMusicDirectory",
		&[("id", album_directory_id)],
	)
	.await;
	let songs = response["directory"]["child"].as_array().unwrap();
	assert_eq!(songs.len(), 7);
	assert!(songs.iter().all(|s| s["isDir"] == false));
	assert!(songs.iter().all(|s| s["parent"] == album_directory_id));
}

#[tokio::test]
async fn lists_albums() {
	let mut service = make_service(&test_name!()).await;

	for list_type in [
		"alphabeticalByName",
		"alphabeticalByArtist",
		"newest",
		"random",
	] {
		let params = [("type", list_type), ("size", "100")];
		let response = call(&mut service, "getAlbumList2", &params).await;
		let albums = response["albumList2"]["album"].as_array().unwrap();
		assert_eq!(albums.len(), 3, "{list_type}");

		let response = call(&mut service, "getAlbumList", &params).await;
		let albums = response["albumList"]["album"].as_array().unwrap();
		assert_eq!(albums.len(), 3, "{list_type}");
		assert!(albums.iter().all(|a| a["isDir"] == true));
	}

	let params = [
		("type", "alphabeticalByName"),
		("size", "1"),
		("offset", "1"),
	];
	let response = call(&mut service, "getAlbumList2", &params).await;
	let albums = response["albumList2"]["album"].as_array().unwrap();
	assert_eq!(albums.len(), 1);
	assert_eq!(albums[0]["name"], "Picnic");

	let years = |response: &Value| {
		response["albumList2"]["album"]
			.as_array()
			.unwrap()
			.iter()
			.map(|a| a["year"].as_i64().unwrap())
			.collect::<Vec<_>>()
	};
	let params = [("type", "byYear"), ("fromYear", "1900"), ("toYear", "2100")];
	let ascending = years(&call(&mut service, "getAlbumList2", &params).await);
	assert_eq!(ascending.len(), 3);
	assert!(ascending.is_sorted());
	let params = [("type", "byYear"), ("fromYear", "2100"), ("toYear", "1900")];
	let descending = years(&call(&mut service, "getAlbumList2", &params).await);
	assert!(descending.iter().eq(ascending.iter().rev()));
	let params = [("type", "byYear"), ("fromYear", "1900"), ("toYear", "1901")];
	assert!(years(&call(&mut service, "getAlbumList2", &params).await).is_empty());

	let response = call(&mut service, "getAlbumList2", &[("type", "frequent")]).await;
	assert!(response["albumList2"]["album"]
		.as_array()
		.unwrap()
		.is_empty());

	let response = call_as(
		&mut service,
		(TEST_USERNAME, TEST_PASSWORD),
		"getAlbumList2",
		&[],
	)
	.await;
	assert_eq!(error_code(&response), Some(10));
}

#[tokio::test]
async fn lists_genres_and_random_songs() {
	let mut service = make_service(&test_name!()).await;

	let response = call(&mut service, "getGenres", &[]).await;
	let genres = response["genres"]["genre"].as_array().unwrap();
	assert!(!genres.is_empty());
	let genre = &genres[0];
	assert!(genre["songCount"].as_u64().unwrap() > 0);
	let genre_name = genre["value"].as_str().unwrap().to_owned();

	let params = [("genre", &genre_name[..]), ("count", "500")];
	let response = call(&mut service, "getSongsByGenre", &params).await;
	let songs = response["songsByGenre"]["song"].as_array().unwrap();
	assert_eq!(songs.len() as u64, genre["songCount"].as_u64().unwrap());

	let response = call(&mut service, "getRandomSongs", &[("size", "4")]).await;
	assert_eq!(response["randomSongs"]["song"].as_array().unwrap().len(), 4);

	let response = call(&mut service, "getRandomSongs", &[("size", "500")]).await;
	assert_eq!(
		response["randomSongs"]["song"].as_array().unwrap().len(),
		13
	);
}

#[tokio::test]
async fn searches() {
	let mut service = make_service(&test_name!()).await;

	let response = call(&mut service, "search3", &[("query", "\"hunted\"*")]).await;
	let result = &response["searchResult3"];
	find(&result["album"], "name", "Hunted");
	assert_eq!(result["song"].as_array().unwrap().len(), 5);

	let response = call(&mut service, "search3", &[("query", "khem")]).await;
	find(&response["searchResult3"]["artist"], "name", "Khemmis");

	let response = call(&mut service, "search2", &[("query", "candlelight")]).await;
	let songs = response["searchResult2"]["song"].as_array().unwrap();
	assert_eq!(songs.len(), 1);
	assert_eq!(songs[0]["title"], "Candlelight");

	let response = call(&mut service, "search3", &[("query", "(unbalanced \"quote")]).await;
	assert!(response["searchResult3"]["song"]
		.as_array()
		.unwrap()
		.is_empty());

	// Empty queries enumerate the collection
	let params = [
		("query", "\"\""),
		("songCount", "500"),
		("albumCount", "500"),
	];
	let response = call(&mut service, "search3", &params).await;
	let result = &response["searchResult3"];
	assert_eq!(result["song"].as_array().unwrap().len(), 13);
	assert_eq!(result["album"].as_array().unwrap().len(), 3);

	let params = [("query", ""), ("songCount", "5"), ("songOffset", "10")];
	let response = call(&mut service, "search3", &params).await;
	assert_eq!(
		response["searchResult3"]["song"].as_array().unwrap().len(),
		3
	);
}

#[tokio::test]
async fn serves_media() {
	let mut service = make_service(&test_name!()).await;

	let album = find_album(&mut service, "Hunted").await;
	let album_id = album["id"].as_str().unwrap();
	let response = call(&mut service, "getAlbum", &[("id", album_id)]).await;
	let song = &response["album"]["song"][1];
	let song_id = song["id"].as_str().unwrap();
	let credentials = [("u", TEST_USERNAME), ("p", TEST_PASSWORD)];

	let expected = std::fs::read(format!(
		"{TEST_MOUNT_SOURCE}/Khemmis/Hunted/02 - Candlelight.mp3"
	))
	.unwrap();

	for endpoint in ["stream", "download"] {
		let mut params = credentials.to_vec();
		params.push(("id", song_id));
		let request = protocol::subsonic(endpoint, &params);
		let response = service.fetch_bytes(&request).await;
		assert_eq!(response.status(), StatusCode::OK);
		assert_eq!(
			response.headers().get(header::CONTENT_TYPE).unwrap(),
			"audio/mpeg"
		);
		assert_eq!(
			response.headers().contains_key(header::CONTENT_DISPOSITION),
			endpoint == "download"
		);
		assert_eq!(response.body(), &expected);
	}

	let mut params = credentials.to_vec();
	params.push(("id", song_id));
	let mut request = protocol::subsonic("stream", &params);
	request
		.headers_mut()
		.insert(header::RANGE, "bytes=100-299".parse().unwrap());
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
	assert_eq!(response.body(), &expected[100..300]);

	for cover_art_id in [album["coverArt"].as_str().unwrap(), album_id, song_id] {
		let mut params = credentials.to_vec();
		params.extend([("id", cover_art_id), ("size", "64")]);
		let request = protocol::subsonic("getCoverArt", &params);
		let response = service.fetch_bytes(&request).await;
		assert_eq!(response.status(), StatusCode::OK);
		assert_eq!(
			response.headers().get(header::CONTENT_TYPE).unwrap(),
			"image/jpeg"
		);
		assert!(response.body().starts_with(&[0xFF, 0xD8]));
	}

	let params = [("f", "json"), ("id", "so-bm90IGEgc29uZw")];
	let response = call_as(
		&mut service,
		(TEST_USERNAME, TEST_PASSWORD),
		"stream",
		&params,
	)
	.await;
	assert_eq!(error_code(&response), Some(70));
}

#[tokio::test]
async fn manages_playlists() {
	let mut service = make_service(&test_name!()).await;

	let album = find_album(&mut service, "Hunted").await;
	let album_id = album["id"].as_str().unwrap();
	let response = call(&mut service, "getAlbum", &[("id", album_id)]).await;
	let song_ids = response["album"]["song"]
		.as_array()
		.unwrap()
		.iter()
		.map(|s| s["id"].as_str().unwrap().to_owned())
		.collect::<Vec<_>>();

	let response = call(&mut service, "getPlaylists", &[]).await;
	assert_eq!(response["playlists"]["playlist"], serde_json::json!([]));

	let params = [
		("name", TEST_PLAYLIST_NAME),
		("songId", &song_ids[0][..]),
		("songId", &song_ids[1][..]),
		("songId", &song_ids[2][..]),
	];
	let response = call(&mut service, "createPlaylist", &params).await;
	let playlist = &response["playlist"];
	assert_eq!(playlist["name"], TEST_PLAYLIST_NAME);
	assert_eq!(playlist["owner"], TEST_USERNAME);
	assert_eq!(playlist["songCount"], 3);
	assert_eq!(playlist["entry"].as_array().unwrap().len(), 3);
	let playlist_id = playlist["id"].as_str().unwrap().to_owned();

	// Playlists are private to their owner
	let admin = (TEST_USERNAME_ADMIN, TEST_PASSWORD_ADMIN);
	let response = call_as(&mut service, admin, "getPlaylists", &[]).await;
	assert_eq!(response["playlists"]["playlist"], serde_json::json!([]));
	let response = call_as(&mut service, admin, "getPlaylist", &[("id", &playlist_id)]).await;
	assert_eq!(error_code(&response), Some(70));

	let params = [
		("playlistId", &playlist_id[..]),
		("name", "renamed"),
		("songIndexToRemove", "0"),
		("songIdToAdd", &song_ids[4][..]),
	];
	call(&mut service, "updatePlaylist", &params).await;

	let response = call(&mut service, "getPlaylists", &[]).await;
	let playlists = response["playlists"]["playlist"].as_array().unwrap();
	assert_eq!(playlists.len(), 1);
	assert_eq!(playlists[0]["name"], "renamed");
	let playlist_id = playlists[0]["id"].as_str().unwrap().to_owned();

	let response = call(&mut service, "getPlaylist", &[("id", &playlist_id)]).await;
	let entries = response["playlist"]["entry"]
		.as_array()
		.unwrap()
		.iter()
		.map(|s| s["id"].as_str().unwrap())
		.collect::<Vec<_>>();
	assert_eq!(entries, vec![&song_ids[1], &song_ids[2], &song_ids[4]]);

	call(&mut service, "deletePlaylist", &[("id", &playlist_id)]).await;
	let response = call(&mut service, "getPlaylists", &[]).await;
	assert_eq!(response["playlists"]["playlist"], serde_json::json!([]));
}

#[tokio::test]
async fn reports_users_and_scans() {
	let mut service = make_service(&test_name!()).await;
	let user = (TEST_USERNAME, TEST_PASSWORD);
	let admin = (TEST_USERNAME_ADMIN, TEST_PASSWORD_ADMIN);

	let response = call(&mut service, "getUser", &[]).await;
	assert_eq!(response["user"]["username"], TEST_USERNAME);
	assert_eq!(response["user"]["adminRole"], false);
	assert_eq!(response["user"]["streamRole"], true);

	let params = [("username", TEST_USERNAME_ADMIN)];
	let response = call_as(&mut service, user, "getUser", &params).await;
	assert_eq!(error_code(&response), Some(50));

	let response = call_as(
		&mut service,
		admin,
		"getUser",
		&[("username", TEST_USERNAME)],
	)
	.await;
	assert_eq!(response["user"]["adminRole"], false);
	let response = call_as(&mut service, admin, "getUser", &[]).await;
	assert_eq!(response["user"]["adminRole"], true);

	let response = call(&mut service, "getScanStatus", &[]).await;
	assert_eq!(response["scanStatus"]["count"], 13);

	let response = call_as(&mut service, user, "startScan", &[]).await;
	assert_eq!(error_code(&response), Some(50));
	let response = call_as(&mut service, admin, "startScan", &[]).await;
	assert_eq!(response["status"], "ok");
}

#[tokio::test]
async fn reports_unsupported_features() {
	let mut service = make_service(&test_name!()).await;
	let user = (TEST_USERNAME, TEST_PASSWORD);

	for endpoint in ["getStarred2", "getNowPlaying", "getPodcasts", "scrobble"] {
		call(&mut service, endpoint, &[]).await;
	}

	for endpoint in ["star", "setRating", "jukeboxControl"] {
		let response = call_as(&mut service, user, endpoint, &[]).await;
		assert_eq!(error_code(&response), Some(0));
	}
}

#[tokio::test]
async fn limits_stream_bitrate() {
	let mut service = make_service(&test_name!()).await;

	service.login_admin().await;
	let mounts = vec![dto::MountDir {
		name: "formats".into(),
		source: "test-data/formats".into(),
	}];
	let response = service.fetch(&protocol::put_mount_dirs(mounts)).await;
	assert_eq!(response.status(), StatusCode::OK);
	service.index().await;
	let request = protocol::transcoding();
	let response = service
		.fetch_json::<_, dto::TranscodingSupport>(&request)
		.await;
	let can_transcode = response.body().available;
	service.logout().await;

	let params = [("query", ""), ("songCount", "500")];
	let response = call(&mut service, "search3", &params).await;
	let songs = &response["searchResult3"]["song"];
	let flac_id = find(songs, "suffix", "flac")["id"].as_str().unwrap();
	let mp3_id = find(songs, "suffix", "mp3")["id"].as_str().unwrap();
	let credentials = [("u", TEST_USERNAME), ("p", TEST_PASSWORD)];
	let flac = std::fs::read("test-data/formats/sample.flac").unwrap();
	let mp3 = std::fs::read("test-data/formats/sample.mp3").unwrap();

	let mut fetch = async |id: &str, extra: &[(&str, &str)]| {
		let mut params = credentials.to_vec();
		params.push(("id", id));
		params.extend_from_slice(extra);
		let response = service
			.fetch_bytes(&protocol::subsonic("stream", &params))
			.await;
		assert_eq!(response.status(), StatusCode::OK);
		let content_type = response.headers().get(header::CONTENT_TYPE).unwrap();
		(
			content_type.to_str().unwrap().to_owned(),
			response.into_body(),
		)
	};

	// Without a bitrate limit, or when explicitly asked to, files are served as they are
	assert_eq!(
		fetch(flac_id, &[]).await,
		("audio/flac".to_owned(), flac.clone())
	);
	let params = [("maxBitRate", "0")];
	assert_eq!(fetch(flac_id, &params).await.1, flac);
	let params = [("maxBitRate", "64"), ("format", "raw")];
	assert_eq!(fetch(flac_id, &params).await.1, flac);

	// Files which already satisfy the limit are served as they are
	let params = [("maxBitRate", "320")];
	assert_eq!(fetch(mp3_id, &params).await, ("audio/mpeg".to_owned(), mp3));

	let (content_type, body) = fetch(flac_id, &[("maxBitRate", "64")]).await;
	let (opus_content_type, opus_body) = fetch(flac_id, &[("format", "opus")]).await;
	if can_transcode {
		assert_eq!(content_type, "audio/mpeg");
		assert!(body.starts_with(b"ID3"));
		assert_eq!(opus_content_type, "audio/ogg");
		assert!(opus_body.starts_with(b"OggS"));
	} else {
		// Servers which cannot transcode fall back to the original file
		assert_eq!(content_type, "audio/flac");
		assert_eq!(body, flac);
		assert_eq!(opus_body, flac);
	}
}

#[tokio::test]
async fn allows_requests_from_web_clients() {
	let mut service = make_service(&test_name!()).await;
	let params = [("u", TEST_USERNAME), ("p", TEST_PASSWORD), ("f", "json")];
	let mut request = protocol::subsonic("ping", &params);
	request.headers_mut().insert(
		header::ORIGIN,
		"https://subsonic-client.example.com".parse().unwrap(),
	);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(
		response
			.headers()
			.get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
			.unwrap(),
		"*"
	);

	// The Polaris API is not opened up to other origins
	let mut request = protocol::version();
	request.headers_mut().insert(
		header::ORIGIN,
		"https://subsonic-client.example.com".parse().unwrap(),
	);
	let response = service.fetch(&request).await;
	assert!(!response
		.headers()
		.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
}
