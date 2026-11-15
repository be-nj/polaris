//! Implementation of the Subsonic REST API (with OpenSubsonic extensions), which lets
//! third-party Subsonic clients browse and stream a Polaris collection.
//!
//! Features Polaris has no equivalent for (ratings, stars, play statistics, podcasts, etc.)
//! answer with empty results where a client expects a list, and with an error where a
//! client expects a change to be saved.

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, RandomState};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use axum::{
	body::Bytes,
	extract::{Path, RawQuery, State},
	response::Response,
	routing::get,
	Router,
};
use axum_extra::{headers::Range, TypedHeader};
use http::{header, HeaderMap, HeaderValue};
use rand::seq::SliceRandom;
use serde_json::{json, Map, Value};

use crate::app::{self, auth, index, scanner, thumbnail, transcode, App};

mod id;
mod model;
mod response;

use super::media;

use id::Id;
use response::{Error, Format, Payload};

const MAX_PAGE_SIZE: usize = 500;

pub fn router() -> Router<App> {
	Router::new().route("/{endpoint}", get(handle).post(handle))
}

struct Params(Vec<(String, String)>);

impl Params {
	fn parse_pairs(input: &[u8]) -> impl Iterator<Item = (String, String)> + '_ {
		form_urlencoded::parse(input).map(|(k, v)| (k.into_owned(), v.into_owned()))
	}

	fn get(&self, name: &str) -> Option<&str> {
		self.0
			.iter()
			.find(|(k, _)| k == name)
			.map(|(_, v)| v.as_str())
	}

	fn all(&self, name: &str) -> Vec<&str> {
		self.0
			.iter()
			.filter(|(k, _)| k == name)
			.map(|(_, v)| v.as_str())
			.collect()
	}

	fn required(&self, name: &str) -> Result<&str, Error> {
		self.get(name).ok_or_else(|| Error::missing_parameter(name))
	}

	fn number<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
		self.get(name).and_then(|v| v.trim().parse().ok())
	}

	fn page(&self, size_name: &str, offset_name: &str, default_size: usize) -> (usize, usize) {
		let size = self
			.number::<usize>(size_name)
			.unwrap_or(default_size)
			.min(MAX_PAGE_SIZE);
		let offset = self.number::<usize>(offset_name).unwrap_or(0);
		(offset, size)
	}

	fn id(&self, name: &str) -> Result<Id, Error> {
		Id::parse(self.required(name)?).ok_or_else(Error::not_found)
	}

	fn format(&self) -> Format {
		match self.get("f") {
			Some("json") => Format::Json,
			Some("jsonp") => {
				let callback = self
					.get("callback")
					.unwrap_or("callback")
					.chars()
					.filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.'))
					.collect::<String>();
				Format::Jsonp(callback)
			}
			_ => Format::Xml,
		}
	}
}

struct Request {
	app: App,
	params: Params,
	range: Option<Range>,
	username: String,
}

enum Reply {
	Payload(Payload),
	Binary(Response),
}

impl Reply {
	fn empty() -> Result<Reply, Error> {
		Ok(Reply::Payload(Map::new()))
	}

	fn single(key: &str, value: Value) -> Result<Reply, Error> {
		let mut payload = Map::new();
		payload.insert(key.to_owned(), value);
		Ok(Reply::Payload(payload))
	}
}

async fn handle(
	State(app): State<App>,
	Path(endpoint): Path<String>,
	RawQuery(query): RawQuery,
	range: Option<TypedHeader<Range>>,
	headers: HeaderMap,
	body: Bytes,
) -> Response {
	let mut pairs = Params::parse_pairs(query.unwrap_or_default().as_bytes()).collect::<Vec<_>>();
	let is_form_post = headers
		.get(header::CONTENT_TYPE)
		.and_then(|v| v.to_str().ok())
		.is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
	if is_form_post {
		pairs.extend(Params::parse_pairs(&body));
	}

	let params = Params(pairs);
	let format = params.format();
	let endpoint = endpoint.strip_suffix(".view").unwrap_or(&endpoint);
	let request = Request {
		app,
		params,
		range: range.map(|TypedHeader(r)| r),
		username: String::new(),
	};

	match dispatch(endpoint, request).await {
		Ok(Reply::Payload(payload)) => response::success(&format, payload),
		Ok(Reply::Binary(response)) => response,
		Err(error) => response::failure(&format, error),
	}
}

async fn dispatch(endpoint: &str, mut request: Request) -> Result<Reply, Error> {
	if endpoint == "getOpenSubsonicExtensions" {
		return Reply::single(
			"openSubsonicExtensions",
			json!([
				{ "name": "formPost", "versions": [1] },
				{ "name": "apiKeyAuthentication", "versions": [1] },
			]),
		);
	}

	request.username = authenticate(&request).await?;
	let request = &request;

	match endpoint {
		// System
		"ping" => Reply::empty(),
		"getLicense" => Reply::single("license", json!({ "valid": true })),
		"tokenInfo" => Reply::single("tokenInfo", json!({ "username": request.username })),

		// Browsing
		"getMusicFolders" => get_music_folders(request).await,
		"getIndexes" => get_indexes(request).await,
		"getMusicDirectory" => get_music_directory(request).await,
		"getGenres" => get_genres(request).await,
		"getArtists" => get_artists(request).await,
		"getArtist" => get_artist(request).await,
		"getAlbum" => get_album(request).await,
		"getSong" => get_song(request).await,
		"getArtistInfo" => Reply::single("artistInfo", json!({})),
		"getArtistInfo2" => Reply::single("artistInfo2", json!({})),
		"getAlbumInfo" | "getAlbumInfo2" => Reply::single("albumInfo", json!({})),
		"getSimilarSongs" => Reply::single("similarSongs", json!({})),
		"getSimilarSongs2" => Reply::single("similarSongs2", json!({})),
		"getTopSongs" => Reply::single("topSongs", json!({})),

		// Lists
		"getAlbumList" => {
			let albums = get_album_list(request).await?;
			let albums = albums.iter().map(model::album_as_directory).collect();
			Reply::single("albumList", json!({ "album": Value::Array(albums) }))
		}
		"getAlbumList2" => {
			let albums = get_album_list(request).await?;
			let albums = albums.iter().map(model::album).collect();
			Reply::single("albumList2", json!({ "album": Value::Array(albums) }))
		}
		"getRandomSongs" => get_random_songs(request).await,
		"getSongsByGenre" => get_songs_by_genre(request).await,
		"getNowPlaying" => Reply::single("nowPlaying", json!({})),
		"getStarred" => Reply::single("starred", json!({})),
		"getStarred2" => Reply::single("starred2", json!({})),

		// Searching
		"search2" => search(request, false).await,
		"search3" => search(request, true).await,

		// Playlists
		"getPlaylists" => get_playlists(request).await,
		"getPlaylist" => get_playlist(request).await,
		"createPlaylist" => create_playlist(request).await,
		"updatePlaylist" => update_playlist(request).await,
		"deletePlaylist" => delete_playlist(request).await,

		// Media retrieval
		"stream" => stream(request, false).await,
		"download" => stream(request, true).await,
		"getCoverArt" => get_cover_art(request).await,
		"getLyrics" => Reply::single("lyrics", json!({})),
		"getLyricsBySongId" => Reply::single("lyricsList", json!({})),
		"getAvatar" => Err(Error::not_found()),

		// Annotation
		"scrobble" => Reply::empty(),
		"star" | "unstar" => Err(Error::not_supported("Starring")),
		"setRating" => Err(Error::not_supported("Rating")),

		// Features without a Polaris equivalent, which clients commonly query on startup
		"getPodcasts" => Reply::single("podcasts", json!({})),
		"getNewestPodcasts" => Reply::single("newestPodcasts", json!({})),
		"getInternetRadioStations" => Reply::single("internetRadioStations", json!({})),
		"getBookmarks" => Reply::single("bookmarks", json!({})),
		"getShares" => Reply::single("shares", json!({})),
		"getPlayQueue" => Reply::empty(),

		// User management
		"getUser" => get_user(request).await,

		// Media library scanning
		"getScanStatus" => get_scan_status(request).await,
		"startScan" => {
			require_admin(request).await?;
			request.app.scanner.try_trigger_scan();
			Reply::single("scanStatus", json!({ "scanning": true, "count": 0 }))
		}

		_ => Err(Error::not_supported(&format!("The method `{endpoint}`"))),
	}
}

/// Verifying a password is deliberately expensive, and Subsonic clients send credentials
/// with every single request. This remembers which password was last verified for each
/// user. Only a keyed hash of the password is retained, and entries become stale as soon
/// as the stored password hash of the user changes.
static VERIFIED_PASSWORDS: LazyLock<Mutex<HashMap<String, (String, u64)>>> =
	LazyLock::new(Mutex::default);
static PASSWORD_HASHER: LazyLock<RandomState> = LazyLock::new(RandomState::new);

async fn verify_password(request: &Request, username: &str, password: &str) -> bool {
	let Ok(user) = request.app.config_manager.get_user(username).await else {
		return false;
	};

	let digest = PASSWORD_HASHER.hash_one(password);
	let cache_entry = (user.hashed_password.clone(), digest);
	if VERIFIED_PASSWORDS.lock().unwrap().get(username) == Some(&cache_entry) {
		return true;
	}

	let is_valid = tokio::task::spawn_blocking({
		let password = password.to_owned();
		move || auth::verify_password(&user.hashed_password, &password)
	})
	.await
	.unwrap_or(false);

	if is_valid {
		VERIFIED_PASSWORDS
			.lock()
			.unwrap()
			.insert(username.to_owned(), cache_entry);
	}

	is_valid
}

fn decode_password(password: &str) -> Option<String> {
	let Some(hex) = password.strip_prefix("enc:") else {
		return Some(password.to_owned());
	};
	if hex.len() % 2 != 0 || !hex.is_ascii() {
		return None;
	}
	let bytes = (0..hex.len())
		.step_by(2)
		.map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
		.collect::<Option<Vec<u8>>>()?;
	String::from_utf8(bytes).ok()
}

async fn authenticate(request: &Request) -> Result<String, Error> {
	let params = &request.params;

	if let Some(api_key) = params.get("apiKey") {
		if ["u", "p", "t", "s"].iter().any(|p| params.get(p).is_some()) {
			return Err(Error::conflicting_auth());
		}
		let authorization = request
			.app
			.config_manager
			.authenticate(&auth::Token(api_key.to_owned()), auth::Scope::PolarisAuth)
			.await
			.map_err(|_| Error::invalid_api_key())?;
		return Ok(authorization.username);
	}

	let username = params.required("u")?;
	let Some(password) = params.get("p") else {
		// Token authentication requires the server to know the plain text password,
		// which Polaris never stores.
		return Err(match (params.get("t"), params.get("s")) {
			(None, None) => Error::missing_parameter("p"),
			_ => Error::token_auth_not_supported(),
		});
	};

	let password = decode_password(password).ok_or_else(Error::wrong_credentials)?;
	if verify_password(request, username, &password).await {
		Ok(username.to_owned())
	} else {
		Err(Error::wrong_credentials())
	}
}

async fn require_admin(request: &Request) -> Result<(), Error> {
	let user = request
		.app
		.config_manager
		.get_user(&request.username)
		.await?;
	if user.is_admin() {
		Ok(())
	} else {
		Err(Error::unauthorized())
	}
}

async fn fetch_songs(request: &Request, virtual_paths: Vec<PathBuf>) -> Vec<index::Song> {
	request
		.app
		.index_manager
		.get_songs(virtual_paths)
		.await
		.into_iter()
		.filter_map(Result::ok)
		.collect()
}

async fn fetch_albums(request: &Request, headers: &[index::AlbumHeader]) -> Vec<index::Album> {
	let keys = headers
		.iter()
		.map(|h| (h.artists.clone(), h.name.clone()))
		.collect();
	request.app.index_manager.get_albums_by_key(keys).await
}

/// Every song in the collection, or nothing if the collection has not been indexed yet.
async fn all_song_paths(request: &Request) -> Vec<PathBuf> {
	request
		.app
		.index_manager
		.flatten(PathBuf::new())
		.await
		.unwrap_or_default()
}

async fn performing_artists(request: &Request) -> Vec<index::ArtistHeader> {
	let mut artists = request.app.index_manager.get_artists().await;
	artists.retain(|a| a.num_albums_as_performer + a.num_albums_as_additional_performer > 0);
	artists
}

fn paginate<T>(items: Vec<T>, (offset, size): (usize, usize)) -> Vec<T> {
	items.into_iter().skip(offset).take(size).collect()
}

/// Groups entries under the first letter of their name, as expected by index-style listings.
fn group_by_letter(entries: Vec<(String, Value)>, entry_key: &str) -> Value {
	let mut groups = BTreeMap::<String, Vec<Value>>::new();
	for (name, entry) in entries {
		groups
			.entry(model::index_letter(&name))
			.or_default()
			.push(entry);
	}
	groups
		.into_iter()
		.map(|(letter, entries)| json!({ "name": letter, entry_key: entries }))
		.collect()
}

async fn get_music_folders(request: &Request) -> Result<Reply, Error> {
	let mounts = request.app.config_manager.get_mounts().await;
	let folders = mounts
		.iter()
		.enumerate()
		.map(|(index, mount)| json!({ "id": index, "name": mount.name }))
		.collect::<Vec<_>>();
	Reply::single("musicFolders", json!({ "musicFolder": folders }))
}

async fn get_indexes(request: &Request) -> Result<Reply, Error> {
	let mounts = request.app.config_manager.get_mounts().await;
	let mounts = match request.params.number::<usize>("musicFolderId") {
		Some(index) => mounts.into_iter().skip(index).take(1).collect::<Vec<_>>(),
		None => mounts,
	};

	let mut directories = Vec::new();
	let mut song_paths = Vec::new();
	for mount in mounts {
		let mount_path = PathBuf::from(&mount.name);
		// Mount points which have not been indexed (yet) have no content to list
		let Ok(files) = request.app.index_manager.browse(mount_path).await else {
			continue;
		};
		for file in files {
			match file {
				index::File::Directory(path) => directories.push(path),
				index::File::Song(path) => song_paths.push(path),
			}
		}
	}

	let directories = directories
		.iter()
		.map(|path| {
			let name = path
				.file_name()
				.map(|n| n.to_string_lossy().into_owned())
				.unwrap_or_default();
			let entry = json!({ "id": Id::directory(path).encode(), "name": name });
			(name, entry)
		})
		.collect();

	let status = request.app.scanner.get_status().await;
	let last_modified = status
		.last_end_time
		.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
		.map(|d| d.as_millis() as u64)
		.unwrap_or_default();

	let songs = fetch_songs(request, song_paths).await;
	Reply::single(
		"indexes",
		json!({
			"ignoredArticles": "",
			"lastModified": last_modified,
			"index": group_by_letter(directories, "artist"),
			"child": model::songs(&songs),
		}),
	)
}

async fn get_music_directory(request: &Request) -> Result<Reply, Error> {
	let id = request.params.id("id")?;
	let index_manager = &request.app.index_manager;

	let directory = match &id {
		Id::Directory(path) => {
			let files = index_manager.browse(path.clone()).await?;
			let mut children = Vec::new();
			let mut song_paths = Vec::new();
			for file in files {
				match file {
					index::File::Directory(path) => children.push(model::directory(&path)),
					index::File::Song(path) => song_paths.push(path),
				}
			}
			children.extend(model::songs(&fetch_songs(request, song_paths).await));
			let mut directory = model::directory(path);
			directory["name"] = directory["title"].take();
			directory["child"] = Value::Array(children);
			directory
		}
		Id::Artist(name) => {
			let artist = index_manager.get_artist(name.clone()).await?;
			let albums = artist
				.albums
				.iter()
				.map(model::album_as_directory)
				.collect::<Vec<_>>();
			json!({ "id": id.encode(), "name": name, "child": albums })
		}
		Id::Album { artists, name } => {
			let album = index_manager
				.get_album(artists.clone(), name.clone())
				.await?;
			json!({
				"id": id.encode(),
				"parent": artists.first().map(|a| Id::Artist(a.clone()).encode()),
				"name": name,
				"child": model::songs(&album.songs),
			})
		}
		_ => return Err(Error::not_found()),
	};

	Reply::single("directory", directory)
}

async fn get_genres(request: &Request) -> Result<Reply, Error> {
	let genres = request
		.app
		.index_manager
		.get_genre_counts()
		.await
		.into_iter()
		.map(|(name, num_albums, num_songs)| {
			json!({ "value": name, "songCount": num_songs, "albumCount": num_albums })
		})
		.collect::<Vec<_>>();
	Reply::single("genres", json!({ "genre": genres }))
}

async fn get_artists(request: &Request) -> Result<Reply, Error> {
	let artists = performing_artists(request)
		.await
		.iter()
		.map(|header| (header.name.to_string(), model::artist(header)))
		.collect();
	Reply::single(
		"artists",
		json!({
			"ignoredArticles": "",
			"index": group_by_letter(artists, "artist"),
		}),
	)
}

async fn get_artist(request: &Request) -> Result<Reply, Error> {
	let Id::Artist(name) = request.params.id("id")? else {
		return Err(Error::not_found());
	};
	let artist = request.app.index_manager.get_artist(name).await?;
	let mut value = model::artist(&artist.header);
	value["albumCount"] = json!(artist.albums.len());
	value["album"] = artist.albums.iter().map(model::album).collect();
	Reply::single("artist", value)
}

async fn get_album(request: &Request) -> Result<Reply, Error> {
	let Id::Album { artists, name } = request.params.id("id")? else {
		return Err(Error::not_found());
	};
	let album = request.app.index_manager.get_album(artists, name).await?;
	Reply::single("album", model::album_with_songs(&album))
}

async fn get_song(request: &Request) -> Result<Reply, Error> {
	let Id::Song(path) = request.params.id("id")? else {
		return Err(Error::not_found());
	};
	let songs = fetch_songs(request, vec![path]).await;
	let song = songs.first().ok_or_else(Error::not_found)?;
	Reply::single("song", model::song(song))
}

async fn get_album_list(request: &Request) -> Result<Vec<index::Album>, Error> {
	let params = &request.params;
	let index_manager = &request.app.index_manager;
	let page @ (offset, size) = params.page("size", "offset", 10);

	let headers = match params.required("type")? {
		"random" => return Ok(index_manager.get_random_albums(None, offset, size).await?),
		"newest" => return Ok(index_manager.get_recent_albums(offset, size).await?),
		// Polaris does not keep track of playback or ratings
		"recent" | "frequent" | "starred" | "highest" => return Ok(Vec::new()),
		"alphabeticalByName" => index_manager.get_albums().await,
		"alphabeticalByArtist" => {
			let mut albums = index_manager.get_albums().await;
			albums.sort_by_cached_key(|a| a.artists.join(", ").to_lowercase());
			albums
		}
		"byYear" => {
			let from = params
				.number::<i64>("fromYear")
				.ok_or_else(|| Error::missing_parameter("fromYear"))?;
			let to = params
				.number::<i64>("toYear")
				.ok_or_else(|| Error::missing_parameter("toYear"))?;
			let range = from.min(to)..=from.max(to);
			let mut albums = index_manager.get_albums().await;
			albums.retain(|a| a.year.is_some_and(|y| range.contains(&y)));
			albums.sort_by_key(|a| a.year);
			if from > to {
				albums.reverse();
			}
			albums
		}
		"byGenre" => {
			let genre = params.required("genre")?.to_owned();
			match index_manager.get_genre(genre).await {
				Ok(genre) => genre.albums,
				Err(_) => Vec::new(),
			}
		}
		_ => return Err(Error::generic("Unknown album list type")),
	};

	Ok(fetch_albums(request, &paginate(headers, page)).await)
}

async fn get_random_songs(request: &Request) -> Result<Reply, Error> {
	let params = &request.params;
	let (_, size) = params.page("size", "", 10);
	let from_year = params.number::<i64>("fromYear");
	let to_year = params.number::<i64>("toYear");
	let has_year_filter = from_year.is_some() || to_year.is_some();

	// Year filters are applied after sampling, so a larger sample is drawn to compensate
	let sample_size = if has_year_filter { size * 20 } else { size };

	let mut songs = match params.get("genre") {
		Some(genre) => match request.app.index_manager.get_genre(genre.to_owned()).await {
			Ok(genre) => genre
				.songs
				.choose_multiple(&mut rand::thread_rng(), sample_size)
				.cloned()
				.collect(),
			Err(_) => Vec::new(),
		},
		None => {
			let sample = all_song_paths(request)
				.await
				.choose_multiple(&mut rand::thread_rng(), sample_size)
				.cloned()
				.collect();
			fetch_songs(request, sample).await
		}
	};

	songs.retain(|s| {
		from_year.is_none_or(|from| s.year.is_some_and(|y| y >= from))
			&& to_year.is_none_or(|to| s.year.is_some_and(|y| y <= to))
	});
	songs.truncate(size);

	Reply::single("randomSongs", json!({ "song": model::songs(&songs) }))
}

async fn get_songs_by_genre(request: &Request) -> Result<Reply, Error> {
	let params = &request.params;
	let genre = params.required("genre")?.to_owned();
	let page = params.page("count", "offset", 10);
	let songs = match request.app.index_manager.get_genre(genre).await {
		Ok(genre) => paginate(genre.songs, page),
		Err(_) => Vec::new(),
	};
	Reply::single("songsByGenre", json!({ "song": model::songs(&songs) }))
}

async fn search(request: &Request, tag_based: bool) -> Result<Reply, Error> {
	let params = &request.params;
	let index_manager = &request.app.index_manager;

	// Clients commonly wrap their queries in quotes or append a wildcard.
	// An empty query is used to enumerate the entire collection.
	let words = params
		.get("query")
		.unwrap_or_default()
		.split_whitespace()
		.map(|w| w.replace(['"', '*'], "").to_lowercase())
		.filter(|w| !w.is_empty())
		.collect::<Vec<_>>();
	let matches = |text: &str| {
		let text = text.to_lowercase();
		words.iter().all(|w| text.contains(w))
	};

	let mut artists = performing_artists(request).await;
	artists.retain(|a| matches(a.name.as_ref()));
	let artists = paginate(artists, params.page("artistCount", "artistOffset", 20));

	let mut albums = index_manager.get_albums().await;
	albums.retain(|a| matches(&a.name));
	let albums = paginate(albums, params.page("albumCount", "albumOffset", 20));
	let albums = fetch_albums(request, &albums).await;

	let song_paths = if words.is_empty() {
		all_song_paths(request).await
	} else {
		let query = words
			.iter()
			.map(|w| format!(r#""{w}""#))
			.collect::<Vec<_>>()
			.join(" ");
		index_manager.search(query).await.unwrap_or_default()
	};
	let song_paths = paginate(song_paths, params.page("songCount", "songOffset", 20));
	let songs = fetch_songs(request, song_paths).await;

	let artists = artists.iter().map(model::artist).collect::<Vec<_>>();
	let songs = model::songs(&songs);
	if tag_based {
		let albums = albums.iter().map(model::album).collect::<Vec<_>>();
		Reply::single(
			"searchResult3",
			json!({ "artist": artists, "album": albums, "song": songs }),
		)
	} else {
		let albums = albums
			.iter()
			.map(model::album_as_directory)
			.collect::<Vec<_>>();
		Reply::single(
			"searchResult2",
			json!({ "artist": artists, "album": albums, "song": songs }),
		)
	}
}

async fn read_playlist(request: &Request, name: &str) -> Result<(Value, Vec<PathBuf>), Error> {
	let playlist = request
		.app
		.playlist_manager
		.read_playlist(name, &request.username)
		.await?;
	// Polaris does not record when playlists are created or modified
	let timestamp = model::iso8601(0);
	let description = json!({
		"id": Id::Playlist(name.to_owned()).encode(),
		"name": name,
		"owner": request.username,
		"public": false,
		"songCount": playlist.songs.len(),
		"duration": playlist.header.duration.as_secs(),
		"created": timestamp,
		"changed": timestamp,
	});
	Ok((description, playlist.songs))
}

async fn playlist_with_entries(request: &Request, name: &str) -> Result<Reply, Error> {
	let (mut playlist, song_paths) = read_playlist(request, name).await?;
	let songs = fetch_songs(request, song_paths).await;
	playlist["entry"] = Value::Array(model::songs(&songs));
	Reply::single("playlist", playlist)
}

fn song_paths(ids: Vec<&str>) -> Vec<PathBuf> {
	ids.into_iter()
		.filter_map(|id| match Id::parse(id) {
			Some(Id::Song(path)) => Some(path),
			_ => None,
		})
		.collect()
}

async fn get_playlists(request: &Request) -> Result<Reply, Error> {
	let headers = request
		.app
		.playlist_manager
		.list_playlists(&request.username)
		.await?;
	let mut playlists = Vec::new();
	for header in headers {
		let (playlist, _) = read_playlist(request, &header.name).await?;
		playlists.push(playlist);
	}
	Reply::single("playlists", json!({ "playlist": playlists }))
}

async fn get_playlist(request: &Request) -> Result<Reply, Error> {
	let Id::Playlist(name) = request.params.id("id")? else {
		return Err(Error::not_found());
	};
	playlist_with_entries(request, &name).await
}

async fn create_playlist(request: &Request) -> Result<Reply, Error> {
	let params = &request.params;
	let name = match (params.get("playlistId"), params.get("name")) {
		(Some(id), _) => match Id::parse(id) {
			Some(Id::Playlist(name)) => name,
			_ => return Err(Error::not_found()),
		},
		(None, Some(name)) if !name.trim().is_empty() => name.to_owned(),
		_ => return Err(Error::missing_parameter("name")),
	};

	request
		.app
		.playlist_manager
		.save_playlist(&name, &request.username, song_paths(params.all("songId")))
		.await?;

	playlist_with_entries(request, &name).await
}

async fn update_playlist(request: &Request) -> Result<Reply, Error> {
	let params = &request.params;
	let playlist_manager = &request.app.playlist_manager;
	let Id::Playlist(name) = params.id("playlistId")? else {
		return Err(Error::not_found());
	};

	let (_, mut songs) = read_playlist(request, &name).await?;

	let mut indices_to_remove = params
		.all("songIndexToRemove")
		.into_iter()
		.filter_map(|i| i.parse::<usize>().ok())
		.collect::<Vec<_>>();
	indices_to_remove.sort_unstable();
	indices_to_remove.dedup();
	for index in indices_to_remove.into_iter().rev() {
		if index < songs.len() {
			songs.remove(index);
		}
	}
	songs.extend(song_paths(params.all("songIdToAdd")));

	let new_name = match params.get("name") {
		Some(new_name) if !new_name.trim().is_empty() => new_name,
		_ => &name,
	};
	playlist_manager
		.save_playlist(new_name, &request.username, songs)
		.await?;
	if new_name != name {
		playlist_manager
			.delete_playlist(&name, &request.username)
			.await?;
	}

	Reply::empty()
}

async fn delete_playlist(request: &Request) -> Result<Reply, Error> {
	let Id::Playlist(name) = request.params.id("id")? else {
		return Err(Error::not_found());
	};
	request
		.app
		.playlist_manager
		.delete_playlist(&name, &request.username)
		.await?;
	Reply::empty()
}

/// Subsonic clients cap the bitrate of what they stream with `maxBitRate`, and may ask for
/// a specific `format` (`raw` being the file exactly as it exists on disk).
fn transcode_options(params: &Params) -> Option<transcode::Options> {
	let format = params.get("format");
	if format == Some("raw") {
		return None;
	}
	let format = format.and_then(transcode::Format::from_name);
	let max_bitrate = params.number::<u32>("maxBitRate").filter(|b| *b > 0);
	match (format, max_bitrate) {
		(None, None) => None,
		(format, max_bitrate) => Some(transcode::Options::new(
			format.unwrap_or(transcode::Format::Mp3),
			max_bitrate,
		)),
	}
}

async fn stream(request: &Request, as_attachment: bool) -> Result<Reply, Error> {
	let Id::Song(virtual_path) = request.params.id("id")? else {
		return Err(Error::not_found());
	};

	let audio_path = request
		.app
		.config_manager
		.resolve_virtual_path(&virtual_path)
		.await?;

	let options = match as_attachment {
		true => None,
		false => transcode_options(&request.params),
	};
	if let Some(options) = options {
		let range = request.range.clone();
		let response =
			media::transcoded(&request.app, &virtual_path, &audio_path, &options, range).await;
		match response {
			Ok(Some(response)) => return Ok(Reply::Binary(response)),
			Ok(None) => (),
			// Bitrate limits are a preference: when this server cannot transcode, clients
			// are better served by the original file than by an error.
			Err(app::Error::TranscodingUnavailable) => (),
			Err(e) => return Err(e.into()),
		}
	}

	let mut response = media::file(&audio_path, request.range.clone())
		.await
		.map_err(|_| Error::not_found())?;
	let headers = response.headers_mut();
	if let Some(content_type) = model::content_type(&audio_path) {
		headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
	}
	if as_attachment {
		let filename = audio_path
			.file_name()
			.map(|n| n.to_string_lossy())
			.unwrap_or_default()
			.chars()
			.map(|c| match c {
				'"' | '\\' => '_',
				c if c.is_ascii() && !c.is_ascii_control() => c,
				_ => '_',
			})
			.collect::<String>();
		if let Ok(value) = HeaderValue::from_str(&format!(r#"attachment; filename="{filename}""#)) {
			headers.insert(header::CONTENT_DISPOSITION, value);
		}
	}

	Ok(Reply::Binary(response))
}

async fn get_cover_art(request: &Request) -> Result<Reply, Error> {
	let index_manager = &request.app.index_manager;
	let artwork_path = match request.params.id("id")? {
		Id::CoverArt(path) => Some(path),
		Id::Song(path) => fetch_songs(request, vec![path])
			.await
			.into_iter()
			.next()
			.and_then(|s| s.artwork),
		Id::Album { artists, name } => index_manager.get_album(artists, name).await?.header.artwork,
		_ => None,
	};
	let artwork_path = artwork_path.ok_or_else(Error::not_found)?;

	let image_path = request
		.app
		.config_manager
		.resolve_virtual_path(&artwork_path)
		.await?;

	let options = thumbnail::Options {
		max_dimension: request
			.params
			.number::<u32>("size")
			.map(|s| s.clamp(16, 2000)),
		resize_if_almost_square: false,
		pad_to_square: false,
	};
	let thumbnail_path = request
		.app
		.thumbnail_manager
		.get_thumbnail(&image_path, &options)
		.await?;

	let mut response = media::file(&thumbnail_path, request.range.clone())
		.await
		.map_err(|_| Error::not_found())?;
	response
		.headers_mut()
		.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
	Ok(Reply::Binary(response))
}

async fn get_user(request: &Request) -> Result<Reply, Error> {
	let config_manager = &request.app.config_manager;
	let username = request.params.get("username").unwrap_or(&request.username);
	if username != request.username {
		require_admin(request).await?;
	}

	let user = config_manager.get_user(username).await?;
	let num_folders = config_manager.get_mounts().await.len();
	Reply::single(
		"user",
		json!({
			"username": user.name,
			"scrobblingEnabled": false,
			"adminRole": user.is_admin(),
			"settingsRole": user.is_admin(),
			"downloadRole": true,
			"uploadRole": false,
			"playlistRole": true,
			"coverArtRole": false,
			"commentRole": false,
			"podcastRole": false,
			"streamRole": true,
			"jukeboxRole": false,
			"shareRole": false,
			"videoConversionRole": false,
			"folder": (0..num_folders).collect::<Vec<_>>(),
		}),
	)
}

async fn get_scan_status(request: &Request) -> Result<Reply, Error> {
	let status = request.app.scanner.get_status().await;
	let is_scanning = matches!(
		status.state,
		scanner::State::Pending | scanner::State::InProgress
	);
	Reply::single(
		"scanStatus",
		json!({ "scanning": is_scanning, "count": status.num_songs_indexed }),
	)
}
