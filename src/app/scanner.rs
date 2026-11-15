use log::{error, info};
use notify_debouncer_full::{
	notify::{event::ModifyKind, EventKind, RecommendedWatcher, RecursiveMode},
	DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache,
};
use rayon::{Scope, ThreadPoolBuilder};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{channel, Sender, TryRecvError};
use std::sync::{Arc, LazyLock};
use std::time::SystemTime;
use std::{cmp::min, time::Duration};
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::{Notify, RwLock};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::app::{config, formats, index, Error};
use crate::utils;

#[derive(Debug, PartialEq, Eq)]
pub struct Directory {
	pub virtual_path: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Song {
	pub real_path: PathBuf,
	pub virtual_path: PathBuf,
	pub track_number: Option<i64>,
	pub disc_number: Option<i64>,
	pub title: Option<String>,
	pub artists: Vec<String>,
	pub album_artists: Vec<String>,
	pub year: Option<i64>,
	pub album: Option<String>,
	pub artwork: Option<PathBuf>,
	pub duration: Option<i64>,
	pub lyricists: Vec<String>,
	pub composers: Vec<String>,
	pub genres: Vec<String>,
	pub labels: Vec<String>,
	pub date_added: i64,
}

#[derive(Clone, Default)]
pub enum State {
	#[default]
	Initial,
	Pending,
	InProgress,
	UpToDate,
}

#[derive(Clone)]
struct Parameters {
	artwork_regex: Option<Regex>,
	exclude_regex: Option<Regex>,
	mount_dirs: Vec<config::MountDir>,
}

impl Parameters {
	fn is_excluded(&self, name: &std::ffi::OsStr) -> bool {
		is_excluded(self.exclude_regex.as_ref(), name)
	}

	/// Whether a path lives inside an excluded part of a mount directory.
	fn is_path_excluded(&self, path: &Path) -> bool {
		if self.exclude_regex.is_none() {
			return false;
		}
		self.mount_dirs
			.iter()
			.filter_map(|m| path.strip_prefix(&m.source).ok())
			.any(|relative_path| {
				relative_path
					.components()
					.any(|c| self.is_excluded(c.as_os_str()))
			})
	}
}

fn is_excluded(exclude_regex: Option<&Regex>, name: &std::ffi::OsStr) -> bool {
	exclude_regex.is_some_and(|r| name.to_str().is_some_and(|n| r.is_match(n)))
}

impl PartialEq for Parameters {
	fn eq(&self, other: &Self) -> bool {
		self.artwork_regex.as_ref().map(|r| r.as_str())
			== other.artwork_regex.as_ref().map(|r| r.as_str())
			&& self.exclude_regex.as_ref().map(|r| r.as_str())
				== other.exclude_regex.as_ref().map(|r| r.as_str())
			&& self.mount_dirs == other.mount_dirs
	}
}

#[derive(Clone, Default)]
pub struct Status {
	pub state: State,
	pub last_start_time: Option<SystemTime>,
	pub last_end_time: Option<SystemTime>,
	pub num_songs_indexed: u32,
}

#[derive(Clone)]
pub struct Scanner {
	index_manager: index::Manager,
	config_manager: config::Manager,
	file_watcher: Arc<RwLock<Option<Debouncer<RecommendedWatcher, RecommendedCache>>>>,
	on_file_change: Arc<Notify>,
	pending_scan: Arc<Notify>,
	status: Arc<RwLock<Status>>,
	parameters: Arc<RwLock<Option<Parameters>>>,
}

impl Scanner {
	pub async fn new(
		index_manager: index::Manager,
		config_manager: config::Manager,
	) -> Result<Self, Error> {
		let scanner = Self {
			index_manager,
			config_manager: config_manager.clone(),
			file_watcher: Arc::default(),
			on_file_change: Arc::default(),
			pending_scan: Arc::new(Notify::new()),
			status: Arc::new(RwLock::new(Status::default())),
			parameters: Arc::default(),
		};

		let abort_scan = Arc::new(Notify::new());

		tokio::spawn({
			let scanner = scanner.clone();
			let abort_scan = abort_scan.clone();
			async move {
				loop {
					scanner.wait_for_change().await;
					abort_scan.notify_waiters();
					scanner.status.write().await.state = State::Pending;
					while tokio::time::timeout(Duration::from_secs(2), scanner.wait_for_change())
						.await
						.is_ok()
					{}
					scanner.pending_scan.notify_waiters();
				}
			}
		});

		tokio::spawn({
			let scanner = scanner.clone();
			async move {
				loop {
					scanner.pending_scan.notified().await;
					tokio::select! {
						result = scanner.run_scan() => {
							if let Err(e) = result {
								error!("Error while updating index: {e}");
							}
						}
						_ = abort_scan.notified() => {
							info!("Interrupted index update");
						}
					};
				}
			}
		});

		Ok(scanner)
	}

	async fn setup_file_watcher(
		config_manager: &config::Manager,
		parameters: Parameters,
		on_file_changed: Arc<Notify>,
	) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>, Error> {
		let mut debouncer = notify_debouncer_full::new_debouncer(
			Duration::from_millis(100),
			None,
			move |result: DebounceEventResult| match result {
				Ok(events) => {
					let is_ignorable = |e: &DebouncedEvent| {
						use EventKind::*;
						matches!(e.kind, Access(_) | Modify(ModifyKind::Metadata(_)) | Other)
							|| (!e.paths.is_empty()
								&& e.paths.iter().all(|p| parameters.is_path_excluded(p)))
					};
					if !events.iter().all(is_ignorable) {
						on_file_changed.notify_waiters();
					}
				}
				Err(e) => error!("Collection file watcher error: `{e:?}`"),
			},
		)?;

		let mount_dirs = config_manager.get_mounts().await;
		for mount_dir in &mount_dirs {
			if let Err(e) = debouncer.watch(&mount_dir.source, RecursiveMode::Recursive) {
				error!("Failed to setup file watcher for `{mount_dir:#?}`: {e}");
			}
		}

		Ok(debouncer)
	}

	async fn wait_for_change(&self) {
		tokio::select! {
			_ = async {
				loop {
					self.config_manager.on_config_change().await;
					if *self.parameters.read().await == Some(self.read_parameters().await) {
						continue;
					}
					break;
				}
			} => {},
			_ = self.on_file_change.notified() => {},
		}
	}

	async fn read_parameters(&self) -> Parameters {
		let album_art_pattern = self.config_manager.get_index_album_art_pattern().await;
		let artwork_regex = Regex::new(&format!("(?i){}", &album_art_pattern)).ok();
		let exclude_regex = self
			.config_manager
			.get_scan_exclude_pattern()
			.await
			.and_then(|p| Regex::new(&format!("(?i){}", &p)).ok());
		Parameters {
			artwork_regex,
			exclude_regex,
			mount_dirs: self.config_manager.get_mounts().await,
		}
	}

	pub async fn get_status(&self) -> Status {
		self.status.read().await.clone()
	}

	pub fn queue_scan(&self) {
		self.pending_scan.notify_one();
	}

	pub fn try_trigger_scan(&self) {
		self.pending_scan.notify_waiters();
	}

	pub async fn run_scan(&self) -> Result<(), Error> {
		info!("Beginning collection scan");

		let start = Instant::now();
		{
			let mut status = self.status.write().await;
			status.last_start_time = Some(SystemTime::now());
			status.state = State::InProgress;
			status.num_songs_indexed = 0;
		}

		let was_empty = self.index_manager.is_index_empty().await;
		let mut partial_update_time = Instant::now();

		let new_parameters = self.read_parameters().await;
		*self.parameters.write().await = Some(new_parameters.clone());

		let (scan_directories_output, collection_directories_input) = channel();
		let (scan_songs_output, collection_songs_input) = channel();
		let scan = Scan::new(
			scan_directories_output,
			scan_songs_output,
			new_parameters.clone(),
		);

		let mut scan_task_set = JoinSet::new();
		let mut index_task_set = JoinSet::new();
		let mut watch_task_set = JoinSet::<Result<(), Error>>::new();
		let mut secondary_task_set = JoinSet::new();

		scan_task_set.spawn_blocking(|| scan.run());

		watch_task_set.spawn({
			let scanner = self.clone();
			let config_manager = self.config_manager.clone();
			let parameters = new_parameters.clone();
			async move {
				let mut watcher = scanner.file_watcher.write().await;
				*watcher = None; // Drops previous watcher
				*watcher = Some(
					Self::setup_file_watcher(
						&config_manager,
						parameters,
						scanner.on_file_change.clone(),
					)
					.await?,
				);
				Ok(())
			}
		});

		let partial_index_notify = Arc::new(tokio::sync::Notify::new());
		let partial_index_mutex = Arc::new(tokio::sync::Mutex::new(index::Builder::default()));
		secondary_task_set.spawn({
			let index_manager = self.index_manager.clone();
			let partial_index_notify = partial_index_notify.clone();
			let partial_index_mutex = partial_index_mutex.clone();
			async move {
				loop {
					partial_index_notify.notified().await;
					let mut partial_index = partial_index_mutex.clone().lock_owned().await;
					let partial_index = std::mem::take(&mut *partial_index);
					let partial_index = partial_index.build();
					let num_songs = partial_index.collection.num_songs();
					if index_manager
						.clone()
						.replace_index(partial_index)
						.await
						.is_ok()
					{
						info!("Promoted partial collection index ({num_songs} songs)");
					}
				}
			}
		});

		let (status_sender, mut status_receiver) = unbounded_channel();
		secondary_task_set.spawn({
			let manager = self.clone();
			async move {
				while let Some(n) = status_receiver.recv().await {
					manager.status.write().await.num_songs_indexed = n;
				}
			}
		});

		index_task_set.spawn_blocking(move || {
			let mut index_builder = index::Builder::default();
			let mut num_songs_scanned = 0;

			loop {
				let exhausted_songs = match collection_songs_input.try_recv() {
					Ok(song) => {
						index_builder.add_song(song);
						num_songs_scanned += 1;
						status_sender.send(num_songs_scanned).ok();
						false
					}
					Err(TryRecvError::Empty) => {
						std::thread::sleep(Duration::from_millis(1));
						false
					}
					Err(TryRecvError::Disconnected) => true,
				};

				let exhausted_directories = match collection_directories_input.try_recv() {
					Ok(directory) => {
						index_builder.add_directory(directory);
						false
					}
					Err(TryRecvError::Empty) => false,
					Err(TryRecvError::Disconnected) => true,
				};

				if exhausted_directories && exhausted_songs {
					break;
				}

				if was_empty && partial_update_time.elapsed().as_secs() > 5 {
					if let Ok(mut m) = partial_index_mutex.clone().try_lock_owned() {
						*m = index_builder.clone();
						partial_index_notify.notify_one();
						partial_update_time = Instant::now()
					}
				}
			}

			index_builder.build()
		});

		scan_task_set.join_next().await.unwrap()??;
		watch_task_set.join_next().await.unwrap()??;
		let index = index_task_set.join_next().await.unwrap()?;
		secondary_task_set.abort_all();

		self.index_manager.persist_index(&index).await?;
		self.index_manager.replace_index(index).await.unwrap();

		{
			let mut status = self.status.write().await;
			status.state = State::UpToDate;
			status.last_end_time = Some(SystemTime::now());
		}

		info!(
			"Collection scan took {} seconds",
			start.elapsed().as_millis() as f32 / 1000.0
		);

		Ok(())
	}
}

struct Scan {
	directories_output: Sender<Directory>,
	songs_output: Sender<Song>,
	parameters: Parameters,
}

impl Scan {
	pub fn new(
		directories_output: Sender<Directory>,
		songs_output: Sender<Song>,
		parameters: Parameters,
	) -> Self {
		Self {
			directories_output,
			songs_output,
			parameters,
		}
	}

	pub fn run(self) -> Result<(), Error> {
		let key = "POLARIS_NUM_TRAVERSER_THREADS";
		let num_threads = std::env::var_os(key)
			.map(|v| v.to_string_lossy().to_string())
			.and_then(|v| usize::from_str(&v).ok())
			.unwrap_or_else(|| min(num_cpus::get(), 8));
		info!("Browsing collection using {} threads", num_threads);

		let directories_output = self.directories_output.clone();
		let songs_output = self.songs_output.clone();
		let artwork_regex = self.parameters.artwork_regex.clone();
		let exclude_regex = self.parameters.exclude_regex.clone();

		let thread_pool = ThreadPoolBuilder::new().num_threads(num_threads).build()?;
		thread_pool.scope({
			|scope| {
				for mount in self.parameters.mount_dirs {
					scope.spawn(|scope| {
						process_directory(
							scope,
							mount.source,
							mount.name,
							directories_output.clone(),
							songs_output.clone(),
							artwork_regex.clone(),
							exclude_regex.clone(),
							None,
						);
					});
				}
			}
		});

		Ok(())
	}
}

fn process_directory<P: AsRef<Path>, Q: AsRef<Path>>(
	scope: &Scope,
	real_path: P,
	virtual_path: Q,
	directories_output: Sender<Directory>,
	songs_output: Sender<Song>,
	artwork_regex: Option<Regex>,
	exclude_regex: Option<Regex>,
	parent_artwork_file: Option<PathBuf>,
) {
	let read_dir = match fs::read_dir(&real_path) {
		Ok(read_dir) => read_dir,
		Err(e) => {
			error!(
				"Directory read error for `{}`: {}",
				real_path.as_ref().display(),
				e
			);
			return;
		}
	};

	let mut directories = vec![];
	let mut files = vec![];
	let mut artwork_file: Option<PathBuf> = None;

	for entry in read_dir {
		let entry = match entry {
			Ok(e) => e,
			Err(e) => {
				error!(
					"File read error within `{}`: {}",
					real_path.as_ref().display(),
					e
				);
				continue;
			}
		};

		let is_dir = match entry.file_type().map(|f| f.is_dir()) {
			Ok(d) => d,
			Err(e) => {
				error!(
					"Could not determine file type for `{}`: {}",
					entry.path().to_string_lossy(),
					e
				);
				continue;
			}
		};
		let name = entry.file_name();
		if is_excluded(exclude_regex.as_ref(), &name) {
			continue;
		}

		let entry_real_path = real_path.as_ref().join(&name);
		let entry_virtual_path = virtual_path.as_ref().join(&name);

		if is_dir {
			directories.push((entry_real_path, entry_virtual_path));
		} else if utils::get_audio_format(&entry_real_path).is_some() {
			files.push((entry_real_path, entry_virtual_path));
		} else if artwork_regex
			.as_ref()
			.is_some_and(|r| r.is_match(name.to_str().unwrap_or_default()))
		{
			// When several files qualify, the choice must not depend on the order the
			// file system happens to list them in
			let is_preferred = artwork_file
				.as_ref()
				.is_none_or(|current| compare_file_names(&entry_virtual_path, current).is_lt());
			if is_preferred {
				artwork_file = Some(entry_virtual_path);
			}
		}
	}

	// Albums made of several discs commonly have one directory per disc, and their artwork
	// next to these directories.
	let is_disc_directory = virtual_path
		.as_ref()
		.file_name()
		.and_then(|n| n.to_str())
		.is_some_and(|n| DISC_DIRECTORY_REGEX.is_match(n));
	let artwork_file = artwork_file.or(parent_artwork_file.filter(|_| is_disc_directory));

	// Sub-directories are handed out before reading the songs of this directory, which is
	// the slow part, so that other threads can get to work
	for (directory_real_path, directory_virtual_path) in directories {
		scope.spawn({
			let directories_output = directories_output.clone();
			let songs_output = songs_output.clone();
			let artwork_regex = artwork_regex.clone();
			let exclude_regex = exclude_regex.clone();
			let artwork_file = artwork_file.clone();
			|scope| {
				process_directory(
					scope,
					directory_real_path,
					directory_virtual_path,
					directories_output,
					songs_output,
					artwork_regex,
					exclude_regex,
					artwork_file,
				);
			}
		});
	}

	for (file_real_path, file_virtual_path) in files {
		let Some(metadata) = formats::read_metadata(&file_real_path) else {
			continue;
		};
		let artwork = match metadata.has_artwork {
			true => Some(file_virtual_path.clone()),
			false => artwork_file.clone(),
		};
		let song = Song {
			date_added: get_date_created(&file_real_path).unwrap_or_default(),
			real_path: file_real_path,
			virtual_path: file_virtual_path,
			track_number: metadata.track_number.map(|n| n as i64),
			disc_number: metadata.disc_number.map(|n| n as i64),
			title: metadata.title,
			artists: metadata.artists,
			album_artists: metadata.album_artists,
			year: metadata.year.map(|n| n as i64),
			album: metadata.album,
			artwork,
			duration: metadata.duration.map(|n| n as i64),
			lyricists: metadata.lyricists,
			composers: metadata.composers,
			genres: metadata.genres,
			labels: metadata.labels,
		};
		songs_output.send(song).ok();
	}

	directories_output
		.send(Directory {
			virtual_path: virtual_path.as_ref().to_owned(),
		})
		.ok();
}

/// Names of directories holding one disc of an album, eg. `CD1`, `Disc 2` or `disc_3 (Live)`
static DISC_DIRECTORY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(r"(?i)^(cd|disc|disk|dvd|lp|vinyl)[ ._-]*\d{1,2}([^\p{L}\d]|$)").unwrap()
});

/// Orders file names without regard for case, so that `cover.jpg` and `Cover.jpg` are
/// treated alike.
fn compare_file_names(a: &Path, b: &Path) -> std::cmp::Ordering {
	let key = |p: &Path| {
		let name = p.file_name().unwrap_or_default().to_string_lossy();
		(name.to_lowercase(), name.into_owned())
	};
	key(a).cmp(&key(b))
}

fn get_date_created<P: AsRef<Path>>(path: P) -> Option<i64> {
	if let Ok(t) = fs::metadata(path).and_then(|m| m.created().or_else(|_| m.modified())) {
		t.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_secs() as i64)
			.ok()
	} else {
		None
	}
}

#[cfg(test)]
mod test {
	use std::path::PathBuf;

	use crate::app::test::{self};
	use crate::test_name;

	use super::*;

	#[tokio::test]
	async fn scan_finds_songs_and_directories() {
		let (directories_sender, directories_receiver) = channel();
		let (songs_sender, songs_receiver) = channel();
		let parameters = Parameters {
			artwork_regex: None,
			exclude_regex: None,
			mount_dirs: vec![config::MountDir {
				source: ["test-data", "small-collection"].iter().collect(),
				name: "root".to_owned(),
			}],
		};

		let scan = Scan::new(directories_sender, songs_sender, parameters);
		scan.run().unwrap();

		let directories = directories_receiver.iter().collect::<Vec<_>>();
		assert_eq!(directories.len(), 6);

		let songs = songs_receiver.iter().collect::<Vec<_>>();
		assert_eq!(songs.len(), 13);
	}

	#[tokio::test]
	async fn scan_finds_embedded_artwork() {
		let (directories_sender, _) = channel();
		let (songs_sender, songs_receiver) = channel();
		let parameters = Parameters {
			artwork_regex: None,
			exclude_regex: None,
			mount_dirs: vec![config::MountDir {
				source: ["test-data", "small-collection"].iter().collect(),
				name: "root".to_owned(),
			}],
		};

		let scan = Scan::new(directories_sender, songs_sender, parameters);
		scan.run().unwrap();

		let songs = songs_receiver.iter().collect::<Vec<_>>();

		songs
			.iter()
			.any(|s| s.artwork.as_ref() == Some(&s.virtual_path));
	}

	#[tokio::test]
	async fn album_art_pattern_is_case_insensitive() {
		let artwork_path = PathBuf::from_iter(["root", "Khemmis", "Hunted", "Folder.jpg"]);
		let patterns = vec!["folder", "FOLDER"];
		for pattern in patterns.into_iter() {
			let (directories_sender, _) = channel();
			let (songs_sender, songs_receiver) = channel();
			let parameters = Parameters {
				artwork_regex: Some(Regex::new(pattern).unwrap()),
				exclude_regex: None,
				mount_dirs: vec![config::MountDir {
					source: ["test-data", "small-collection"].iter().collect(),
					name: "root".to_owned(),
				}],
			};

			let scan = Scan::new(directories_sender, songs_sender, parameters);
			scan.run().unwrap();

			let songs = songs_receiver.iter().collect::<Vec<_>>();

			songs
				.iter()
				.any(|s| s.artwork.as_ref() == Some(&artwork_path));
		}
	}

	/// Scans a collection made of copies of a song and of an image, at the specified
	/// locations. Returns the artwork of each song.
	fn scan_artwork(
		test_name: String,
		files: &[&str],
		artwork_regex: &str,
	) -> std::collections::HashMap<String, Option<String>> {
		let collection_dir = crate::test::prepare_test_directory(test_name);
		let song: PathBuf = ["test-data", "formats", "sample.mp3"].iter().collect();
		let song_with_artwork: PathBuf = ["test-data", "artwork", "sample.mp3"].iter().collect();
		let image: PathBuf = ["test-data", "artwork", "Folder.png"].iter().collect();
		for file in files {
			let destination = collection_dir.join(PathBuf::from_iter(file.split('/')));
			fs::create_dir_all(destination.parent().unwrap()).unwrap();
			let source = match file {
				f if f.ends_with("embedded.mp3") => &song_with_artwork,
				f if f.ends_with(".mp3") => &song,
				_ => &image,
			};
			fs::copy(source, destination).unwrap();
		}

		let (directories_sender, _directories_receiver) = channel();
		let (songs_sender, songs_receiver) = channel();
		let parameters = Parameters {
			artwork_regex: Some(Regex::new(&format!("(?i){artwork_regex}")).unwrap()),
			exclude_regex: None,
			mount_dirs: vec![config::MountDir {
				source: collection_dir,
				name: "root".to_owned(),
			}],
		};
		Scan::new(directories_sender, songs_sender, parameters)
			.run()
			.unwrap();

		let to_string = |p: &Path| {
			let components = p.components().map(|c| c.as_os_str().to_string_lossy());
			components.collect::<Vec<_>>().join("/")
		};
		songs_receiver
			.iter()
			.map(|s| {
				(
					to_string(&s.virtual_path),
					s.artwork.as_deref().map(to_string),
				)
			})
			.collect()
	}

	#[tokio::test]
	async fn default_album_art_pattern_covers_common_names() {
		let artwork = scan_artwork(
			test_name!(),
			&[
				"a/song.mp3",
				"a/Folder.jpg",
				"b/song.mp3",
				"b/cover.png",
				"c/song.mp3",
				"c/FRONT.JPEG",
				"d/song.mp3",
				"d/album.jpg",
				"e/song.mp3",
				"e/back.jpg",
				"e/cover.jpg.bak",
				"e/my cover.jpg",
				"e/AlbumArtSmall.jpg",
				"f/song.mp3",
			],
			config::DEFAULT_ALBUM_ART_PATTERN,
		);
		assert_eq!(
			artwork["root/a/song.mp3"].as_deref(),
			Some("root/a/Folder.jpg")
		);
		assert_eq!(
			artwork["root/b/song.mp3"].as_deref(),
			Some("root/b/cover.png")
		);
		assert_eq!(
			artwork["root/c/song.mp3"].as_deref(),
			Some("root/c/FRONT.JPEG")
		);
		assert_eq!(
			artwork["root/d/song.mp3"].as_deref(),
			Some("root/d/album.jpg")
		);
		assert_eq!(artwork["root/e/song.mp3"], None);
		assert_eq!(artwork["root/f/song.mp3"], None);
	}

	#[tokio::test]
	async fn artwork_choice_does_not_depend_on_file_listing_order() {
		let artwork = scan_artwork(
			test_name!(),
			&[
				"a/song.mp3",
				"a/folder.jpg",
				"a/front.jpg",
				"a/Cover.jpg",
				"b/song.mp3",
				"b/front.jpg",
				"b/Folder.jpg",
			],
			config::DEFAULT_ALBUM_ART_PATTERN,
		);
		assert_eq!(
			artwork["root/a/song.mp3"].as_deref(),
			Some("root/a/Cover.jpg")
		);
		assert_eq!(
			artwork["root/b/song.mp3"].as_deref(),
			Some("root/b/Folder.jpg")
		);
	}

	#[tokio::test]
	async fn disc_directories_use_artwork_of_their_album() {
		let artwork = scan_artwork(
			test_name!(),
			&[
				"artist/album/cover.jpg",
				"artist/album/CD1/song.mp3",
				"artist/album/cd 2/song.mp3",
				"artist/album/Disc 3 - Live/song.mp3",
				"artist/album/disk_04/song.mp3",
				"artist/album/CD5/song.mp3",
				"artist/album/CD5/cover.jpg",
				"artist/album/CD6/embedded.mp3",
				"artist/album/CD7/bonus/song.mp3",
				"artist/album/Vinyl-1/song.mp3",
			],
			config::DEFAULT_ALBUM_ART_PATTERN,
		);
		let album_artwork = Some("root/artist/album/cover.jpg");
		for disc in ["CD1", "cd 2", "Disc 3 - Live", "disk_04", "Vinyl-1"] {
			let song = format!("root/artist/album/{disc}/song.mp3");
			assert_eq!(artwork[&song].as_deref(), album_artwork, "{disc}");
		}
		// Artwork of the disc itself, or of the song, takes precedence
		assert_eq!(
			artwork["root/artist/album/CD5/song.mp3"].as_deref(),
			Some("root/artist/album/CD5/cover.jpg")
		);
		assert_eq!(
			artwork["root/artist/album/CD6/embedded.mp3"].as_deref(),
			Some("root/artist/album/CD6/embedded.mp3")
		);
		// Only disc directories look outside of themselves
		assert_eq!(artwork["root/artist/album/CD7/bonus/song.mp3"], None);
	}

	#[tokio::test]
	async fn other_directories_do_not_use_artwork_of_their_parent() {
		let artwork = scan_artwork(
			test_name!(),
			&[
				"folder.jpg",
				"artist/folder.jpg",
				"artist/album/song.mp3",
				"artist/CDs/song.mp3",
				"artist/Discovery/song.mp3",
				"artist/CD1986 Remaster/song.mp3",
				"artist/Disco 2/song.mp3",
				"artist/1/song.mp3",
				"artist/song.mp3",
			],
			config::DEFAULT_ALBUM_ART_PATTERN,
		);
		for album in [
			"album",
			"CDs",
			"Discovery",
			"CD1986 Remaster",
			"Disco 2",
			"1",
		] {
			let song = format!("root/artist/{album}/song.mp3");
			assert_eq!(artwork[&song], None, "{album}");
		}
		assert_eq!(
			artwork["root/artist/song.mp3"].as_deref(),
			Some("root/artist/folder.jpg")
		);
	}

	#[test]
	fn recognizes_disc_directories() {
		let disc_directories = [
			"CD1",
			"cd1",
			"CD 1",
			"CD01",
			"CD-2",
			"CD_2",
			"CD.2",
			"Disc 1",
			"disc2",
			"Disk 3",
			"DVD1",
			"LP 2",
			"Vinyl 1",
			"CD1 - Bonus Tracks",
			"Disc 2 (Live)",
			"CD2_Remixes",
		];
		for name in disc_directories {
			assert!(DISC_DIRECTORY_REGEX.is_match(name), "{name}");
		}

		let other_directories = [
			"",
			"1",
			"CD",
			"CDs",
			"Discovery",
			"Disco 2",
			"Disc One",
			"Discipline",
			"CD1a",
			"CD1986",
			"LP",
			"2 CD",
			"Vinyl",
			"Diskette 1",
			"CD2Remixes",
		];
		for name in other_directories {
			assert!(!DISC_DIRECTORY_REGEX.is_match(name), "{name}");
		}
	}

	#[tokio::test]
	async fn scan_skips_excluded_files_and_directories() {
		let collection_dir = crate::test::prepare_test_directory(test_name!());
		let sample: PathBuf = ["test-data", "formats", "sample.mp3"].iter().collect();
		for directory in ["album", ".zfs", "@eaDir", "album/@eaDir"] {
			let directory = collection_dir.join(directory);
			fs::create_dir_all(&directory).unwrap();
			fs::copy(&sample, directory.join("song.mp3")).unwrap();
		}
		fs::copy(&sample, collection_dir.join("album").join(".hidden.mp3")).unwrap();

		let run_scan = |exclude_regex: Option<Regex>| {
			let (directories_sender, directories_receiver) = channel();
			let (songs_sender, songs_receiver) = channel();
			let parameters = Parameters {
				artwork_regex: None,
				exclude_regex,
				mount_dirs: vec![config::MountDir {
					source: collection_dir.clone(),
					name: "root".to_owned(),
				}],
			};
			Scan::new(directories_sender, songs_sender, parameters)
				.run()
				.unwrap();
			(
				directories_receiver.iter().count(),
				songs_receiver.iter().collect::<Vec<_>>(),
			)
		};

		let (num_directories, songs) = run_scan(None);
		assert_eq!(num_directories, 5);
		assert_eq!(songs.len(), 5);

		let (num_directories, songs) = run_scan(Some(Regex::new(r"(?i)^(\.|@eadir$)").unwrap()));
		assert_eq!(num_directories, 2);
		assert_eq!(songs.len(), 1);
		assert_eq!(
			songs[0].virtual_path,
			PathBuf::from_iter(["root", "album", "song.mp3"])
		);
	}

	#[test]
	fn excluded_paths_are_relative_to_mounts() {
		let parameters = Parameters {
			artwork_regex: None,
			exclude_regex: Some(Regex::new(config::DEFAULT_SCAN_EXCLUDE_PATTERN).unwrap()),
			mount_dirs: vec![config::MountDir {
				source: PathBuf::from_iter(["home", ".music"]),
				name: "root".to_owned(),
			}],
		};
		let in_mount = |p: &[&str]| PathBuf::from_iter(["home", ".music"].iter().chain(p));
		assert!(!parameters.is_path_excluded(&in_mount(&["album", "song.mp3"])));
		assert!(parameters.is_path_excluded(&in_mount(&[".zfs", "snapshot", "song.mp3"])));
		assert!(parameters.is_path_excluded(&in_mount(&["album", ".hidden.mp3"])));
		assert!(!parameters.is_path_excluded(&PathBuf::from_iter(["elsewhere", ".zfs"])));
	}

	#[tokio::test]
	async fn scanner_reacts_to_config_changes() {
		let ctx = test::ContextBuilder::new(test_name!()).build().await;

		assert!(ctx.index_manager.is_index_empty().await);

		ctx.config_manager
			.set_mounts(vec![config::storage::MountDir {
				source: ["test-data", "small-collection"].iter().collect(),
				name: "root".to_owned(),
			}])
			.await
			.unwrap();

		tokio::time::timeout(Duration::from_secs(10), async {
			loop {
				tokio::time::sleep(Duration::from_millis(100)).await;
				if !ctx.index_manager.is_index_empty().await {
					break;
				}
			}
		})
		.await
		.expect("Index did not populate");
	}
}
