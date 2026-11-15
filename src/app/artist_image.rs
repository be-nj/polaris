use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use log::{info, warn};
use serde::Deserialize;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::spawn_blocking;

use crate::app::{config, index, Error};

const DEFAULT_SEARCH_URL: &str = "https://api.deezer.com/search/artist";

/// Artists who had no picture online are looked up again after this long
const MISSING_IMAGE_RETRY_DELAY: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// After a failed lookup, none is attempted for this long. Clients ask for the pictures
/// of many artists at once, which must not turn into as many doomed requests.
const FAILURE_BACKOFF: Duration = Duration::from_secs(5 * 60);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_IMAGE_SIZE: u64 = 10 * 1024 * 1024;

/// Finds a picture for an artist: the one the collection has for them (see
/// `artist_art_pattern`), or failing that, and only when the configuration allows it, a
/// picture looked up online and kept in the cache directory.
#[derive(Clone)]
pub struct Manager {
	cache_dir_path: PathBuf,
	config_manager: config::Manager,
	index_manager: index::Manager,
	search_url: Arc<str>,
	lookups: Arc<Semaphore>,
	last_failure: Arc<Mutex<Option<Instant>>>,
}

enum Cached {
	Image(PathBuf),
	Missing,
}

impl Manager {
	pub fn new(
		cache_dir_path: PathBuf,
		config_manager: config::Manager,
		index_manager: index::Manager,
	) -> Self {
		Self {
			cache_dir_path,
			config_manager,
			index_manager,
			search_url: DEFAULT_SEARCH_URL.into(),
			lookups: Arc::new(Semaphore::new(1)),
			last_failure: Arc::default(),
		}
	}

	#[cfg(test)]
	pub fn with_search_url(mut self, search_url: &str) -> Self {
		self.search_url = search_url.into();
		self
	}

	/// Path of an image file depicting the artist
	pub async fn get_image(&self, artist_name: &str) -> Result<PathBuf, Error> {
		// Only artists of the collection are looked up, and under the name they have there
		let artist = self
			.index_manager
			.get_artist(artist_name.to_owned())
			.await?;
		let artist_name = artist.header.name.to_string();

		if let Some(virtual_path) = &artist.header.artwork {
			if let Ok(path) = self.config_manager.resolve_virtual_path(virtual_path).await {
				return Ok(path);
			}
		}

		if !self.config_manager.get_fetch_artist_images().await {
			return Err(Error::ArtistImageNotFound);
		}

		self.get_online_image(&artist_name).await
	}

	async fn get_online_image(&self, artist_name: &str) -> Result<PathBuf, Error> {
		match self.read_from_cache(artist_name).await {
			Some(Cached::Image(path)) => return Ok(path),
			Some(Cached::Missing) => return Err(Error::ArtistImageNotFound),
			None => (),
		}

		let _permit = self.lookups.acquire().await;

		// Someone else may have looked up this artist while we were waiting
		match self.read_from_cache(artist_name).await {
			Some(Cached::Image(path)) => return Ok(path),
			Some(Cached::Missing) => return Err(Error::ArtistImageNotFound),
			None => (),
		}

		if let Some(last_failure) = *self.last_failure.lock().await {
			if last_failure.elapsed() < FAILURE_BACKOFF {
				return Err(Error::ArtistImageNotFound);
			}
		}

		let lookup = spawn_blocking({
			let search_url = self.search_url.clone();
			let artist_name = artist_name.to_owned();
			move || fetch_image(&search_url, &artist_name)
		})
		.await?;

		tokio::fs::create_dir_all(&self.cache_dir_path)
			.await
			.map_err(|e| Error::Io(self.cache_dir_path.clone(), e))?;

		match lookup {
			Ok(Some((image, extension))) => {
				let path = self.image_path(artist_name, extension);
				let temporary_path = path.with_extension("part");
				tokio::fs::write(&temporary_path, image)
					.await
					.map_err(|e| Error::Io(temporary_path.clone(), e))?;
				tokio::fs::rename(&temporary_path, &path)
					.await
					.map_err(|e| Error::Io(path.clone(), e))?;
				tokio::fs::remove_file(self.missing_marker_path(artist_name))
					.await
					.ok();
				info!("Saved a picture of artist `{artist_name}` found online");
				Ok(path)
			}
			Ok(None) => {
				let path = self.missing_marker_path(artist_name);
				tokio::fs::write(&path, [])
					.await
					.map_err(|e| Error::Io(path.clone(), e))?;
				Err(Error::ArtistImageNotFound)
			}
			Err(e) => {
				*self.last_failure.lock().await = Some(Instant::now());
				let error = Error::ArtistImageLookup(artist_name.to_owned(), e);
				warn!("{error}");
				Err(error)
			}
		}
	}

	async fn read_from_cache(&self, artist_name: &str) -> Option<Cached> {
		for extension in ["jpg", "png"] {
			let path = self.image_path(artist_name, extension);
			if tokio::fs::try_exists(&path).await.unwrap_or(false) {
				return Some(Cached::Image(path));
			}
		}

		let marker = self.missing_marker_path(artist_name);
		let marked_at = tokio::fs::metadata(&marker).await.ok()?.modified().ok()?;
		let age = SystemTime::now().duration_since(marked_at).ok()?;
		(age < MISSING_IMAGE_RETRY_DELAY).then_some(Cached::Missing)
	}

	fn image_path(&self, artist_name: &str, extension: &str) -> PathBuf {
		self.cache_dir_path
			.join(format!("{}.{extension}", cache_key(artist_name)))
	}

	fn missing_marker_path(&self, artist_name: &str) -> PathBuf {
		self.image_path(artist_name, "missing")
	}
}

/// Reduces the name of an artist to its letters and digits, so that differences of case,
/// spacing and punctuation between the collection and the catalog do not matter.
fn simplify_artist_name(name: &str) -> String {
	name.chars()
		.filter(|c| c.is_alphanumeric())
		.flat_map(char::to_lowercase)
		.collect()
}

fn cache_key(artist_name: &str) -> String {
	let mut hasher = DefaultHasher::new();
	simplify_artist_name(artist_name).hash(&mut hasher);
	format!("{:016x}", hasher.finish())
}

#[derive(Deserialize)]
struct SearchResults {
	data: Option<Vec<SearchResult>>,
	error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct SearchResult {
	name: String,
	picture_xl: Option<String>,
	#[serde(default)]
	nb_fan: u64,
}

/// Looks an artist up in the Deezer catalog and downloads their picture. Returns the image
/// and the file extension matching its format, or `None` when the catalog has no picture
/// of an artist with this name.
fn fetch_image(
	search_url: &str,
	artist_name: &str,
) -> Result<Option<(Vec<u8>, &'static str)>, String> {
	let agent: ureq::Agent = ureq::Agent::config_builder()
		.timeout_global(Some(REQUEST_TIMEOUT))
		.build()
		.into();

	let body = agent
		.get(search_url)
		.query("q", artist_name)
		.query("limit", "25")
		.call()
		.and_then(|mut r| r.body_mut().read_to_string())
		.map_err(|e| format!("search failed: {e}"))?;

	let results: SearchResults =
		serde_json::from_str(&body).map_err(|e| format!("search returned invalid data: {e}"))?;
	if let Some(error) = results.error {
		return Err(format!("search was rejected: {error}"));
	}
	let Some(candidates) = results.data else {
		return Err("search returned no result list".to_owned());
	};

	let Some(picture_url) = pick_picture(candidates, artist_name) else {
		return Ok(None);
	};

	let image = agent
		.get(&picture_url)
		.call()
		.and_then(|mut r| {
			r.body_mut()
				.with_config()
				.limit(MAX_IMAGE_SIZE)
				.read_to_vec()
		})
		.map_err(|e| format!("download of `{picture_url}` failed: {e}"))?;

	let extension = match image::guess_format(&image) {
		Ok(image::ImageFormat::Jpeg) => "jpg",
		Ok(image::ImageFormat::Png) => "png",
		_ => return Err(format!("`{picture_url}` is not a JPEG or PNG image")),
	};

	Ok(Some((image, extension)))
}

/// Several artists often share a name, and the catalog does not list the best known one
/// first. Among those whose name matches exactly, the one with the largest following wins.
fn pick_picture(candidates: Vec<SearchResult>, artist_name: &str) -> Option<String> {
	let wanted = simplify_artist_name(artist_name);
	candidates
		.into_iter()
		.filter(|c| simplify_artist_name(&c.name) == wanted)
		.filter_map(|c| {
			let url = c.picture_xl.filter(|u| !is_placeholder(u))?;
			Some((c.nb_fan, url))
		})
		.max_by_key(|(fans, _)| *fans)
		.map(|(_, url)| url)
}

/// Artists without a picture get a generic one, served from a URL with an empty image identifier
fn is_placeholder(picture_url: &str) -> bool {
	picture_url.is_empty() || picture_url.contains("/artist//")
}

#[cfg(test)]
mod test {
	use std::net::SocketAddr;
	use std::sync::atomic::{AtomicUsize, Ordering};

	use axum::extract::{Query, State};
	use axum::routing::get;
	use axum::{Json, Router};
	use serde_json::{json, Value};

	use super::*;
	use crate::app::test::ContextBuilder;
	use crate::test_name;

	const TEST_MOUNT_NAME: &str = "root";
	const TEST_MOUNT_SOURCE: &str = "test-data/small-collection";

	#[derive(Clone)]
	struct Catalog {
		address: SocketAddr,
		num_searches: Arc<AtomicUsize>,
	}

	/// Stands in for the online catalog: knows Tobokegao, twice, and nobody else.
	async fn start_catalog() -> Catalog {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let catalog = Catalog {
			address: listener.local_addr().unwrap(),
			num_searches: Arc::default(),
		};

		async fn search(
			State(catalog): State<Catalog>,
			Query(params): Query<std::collections::HashMap<String, String>>,
		) -> Json<Value> {
			catalog.num_searches.fetch_add(1, Ordering::SeqCst);
			let base = format!("http://{}", catalog.address);
			let data = match params.get("q").map(String::as_str) {
				Some("Tobokegao") => json!([
					{ "name": "Tobokegao Tribute Band", "nb_fan": 9000, "picture_xl": format!("{base}/wrong.jpg") },
					{ "name": "TOBOKEGAO", "nb_fan": 3, "picture_xl": format!("{base}/wrong.jpg") },
					{ "name": "tobokegao", "nb_fan": 500, "picture_xl": format!("{base}/right.jpg") },
					{ "name": "Tobokegao", "nb_fan": 7000, "picture_xl": format!("{base}/images/artist//1000x1000.jpg") },
				]),
				_ => json!([]),
			};
			Json(json!({ "data": data, "total": 4 }))
		}

		async fn right_picture() -> Vec<u8> {
			tokio::fs::read("test-data/small-collection/Khemmis/Hunted/Folder.jpg")
				.await
				.unwrap()
		}

		async fn search_nobody(State(catalog): State<Catalog>) -> Json<Value> {
			catalog.num_searches.fetch_add(1, Ordering::SeqCst);
			Json(json!({ "data": [], "total": 0 }))
		}

		let router = Router::new()
			.route("/search/artist", get(search))
			.route("/empty/search/artist", get(search_nobody))
			.route("/right.jpg", get(right_picture))
			.with_state(catalog.clone());
		tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

		catalog
	}

	struct Context {
		manager: Manager,
		config_manager: config::Manager,
		cache_dir_path: PathBuf,
	}

	async fn setup(test_name: String, search_url: &str) -> Context {
		let builder = ContextBuilder::new(test_name).mount(TEST_MOUNT_NAME, TEST_MOUNT_SOURCE);
		let cache_dir_path = builder.test_directory.join("artist_images");
		let ctx = builder.build().await;
		ctx.scanner.run_scan().await.unwrap();
		let manager = Manager::new(
			cache_dir_path.clone(),
			ctx.config_manager.clone(),
			ctx.index_manager.clone(),
		)
		.with_search_url(search_url);
		Context {
			manager,
			config_manager: ctx.config_manager,
			cache_dir_path,
		}
	}

	async fn allow_online_lookups(config_manager: &config::Manager) {
		let mut config: config::storage::Config = config_manager.get_config().await.into();
		config.fetch_artist_images = Some(true);
		config_manager.apply_config(config).await.unwrap();
	}

	#[tokio::test]
	async fn uses_picture_of_the_collection() {
		let ctx = setup(test_name!(), "http://127.0.0.1:9").await;
		let path = ctx.manager.get_image("Khemmis").await.unwrap();
		assert_eq!(
			path,
			PathBuf::from_iter([TEST_MOUNT_SOURCE, "Khemmis", "artist.png"])
		);
	}

	#[tokio::test]
	async fn rejects_unknown_artists() {
		let ctx = setup(test_name!(), "http://127.0.0.1:9").await;
		allow_online_lookups(&ctx.config_manager).await;
		assert!(matches!(
			ctx.manager.get_image("Nobody").await,
			Err(Error::ArtistNotFound)
		));
	}

	#[tokio::test]
	async fn does_not_go_online_unless_allowed() {
		let catalog = start_catalog().await;
		let url = format!("http://{}/search/artist", catalog.address);
		let ctx = setup(test_name!(), &url).await;

		assert!(matches!(
			ctx.manager.get_image("Tobokegao").await,
			Err(Error::ArtistImageNotFound)
		));
		assert_eq!(catalog.num_searches.load(Ordering::SeqCst), 0);
	}

	#[tokio::test]
	async fn fetches_picture_of_best_known_namesake_once() {
		let catalog = start_catalog().await;
		let url = format!("http://{}/search/artist", catalog.address);
		let ctx = setup(test_name!(), &url).await;
		allow_online_lookups(&ctx.config_manager).await;

		let path = ctx.manager.get_image("Tobokegao").await.unwrap();
		assert!(path.starts_with(&ctx.cache_dir_path));
		assert_eq!(
			tokio::fs::read(&path).await.unwrap(),
			tokio::fs::read("test-data/small-collection/Khemmis/Hunted/Folder.jpg")
				.await
				.unwrap()
		);

		assert_eq!(ctx.manager.get_image("Tobokegao").await.unwrap(), path);
		assert_eq!(catalog.num_searches.load(Ordering::SeqCst), 1);
	}

	#[tokio::test]
	async fn remembers_artists_without_a_picture() {
		let catalog = start_catalog().await;
		let url = format!("http://{}/empty/search/artist", catalog.address);
		let ctx = setup(test_name!(), &url).await;
		allow_online_lookups(&ctx.config_manager).await;

		for _ in 0..2 {
			assert!(matches!(
				ctx.manager.get_image("Tobokegao").await,
				Err(Error::ArtistImageNotFound)
			));
		}
		assert_eq!(catalog.num_searches.load(Ordering::SeqCst), 1);
	}

	#[tokio::test]
	async fn stops_asking_after_a_failed_lookup() {
		// Nothing listens on this port
		let ctx = setup(test_name!(), "http://127.0.0.1:9/search/artist").await;
		allow_online_lookups(&ctx.config_manager).await;

		assert!(matches!(
			ctx.manager.get_image("Tobokegao").await,
			Err(Error::ArtistImageLookup(_, _))
		));
		assert!(matches!(
			ctx.manager.get_image("Tobokegao").await,
			Err(Error::ArtistImageNotFound)
		));
	}

	#[test]
	fn recognizes_placeholder_pictures() {
		assert!(is_placeholder(
			"https://cdn-images.dzcdn.net/images/artist//1000x1000-000000-80-0-0.jpg"
		));
		assert!(!is_placeholder(
			"https://cdn-images.dzcdn.net/images/artist/489308e79a340dfd05d78fcd91f26a10/1000x1000-000000-80-0-0.jpg"
		));
	}
}
