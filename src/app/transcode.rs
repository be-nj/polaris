//! On-demand audio transcoding, performed by an external `ffmpeg` process.
//!
//! Transcoded audio is made available to callers while it is still being produced, and is
//! kept in a size-limited disk cache so that later requests (notably seeking, which needs
//! the complete file) can be served without transcoding again.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use log::{error, info, warn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{watch, OnceCell, Semaphore};

use crate::app::{config, Error};
use crate::utils::{get_audio_format, AudioFormat};

pub const DEFAULT_CACHE_SIZE_MB: u64 = 1024;
const PARTIAL_FILE_EXTENSION: &str = "part";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
	Mp3,
	Opus,
	Aac,
}

impl Format {
	pub const ALL: [Format; 3] = [Format::Mp3, Format::Opus, Format::Aac];

	pub fn name(&self) -> &'static str {
		match self {
			Format::Mp3 => "mp3",
			Format::Opus => "opus",
			Format::Aac => "aac",
		}
	}

	pub fn from_name(name: &str) -> Option<Self> {
		Self::ALL
			.into_iter()
			.find(|f| f.name().eq_ignore_ascii_case(name))
	}

	pub fn content_type(&self) -> &'static str {
		match self {
			Format::Mp3 => "audio/mpeg",
			Format::Opus => "audio/ogg",
			Format::Aac => "audio/aac",
		}
	}

	pub fn default_bitrate_kbps(&self) -> u32 {
		match self {
			Format::Mp3 | Format::Aac => 192,
			Format::Opus => 128,
		}
	}

	fn bitrate_range_kbps(&self) -> (u32, u32) {
		match self {
			Format::Mp3 => (32, 320),
			Format::Opus => (16, 256),
			Format::Aac => (32, 320),
		}
	}

	fn encoder_arguments(&self) -> &'static [&'static str] {
		match self {
			Format::Mp3 => &["-c:a", "libmp3lame", "-f", "mp3"],
			Format::Opus => &["-c:a", "libopus", "-f", "ogg"],
			Format::Aac => &["-c:a", "aac", "-f", "adts"],
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Options {
	pub format: Format,
	pub bitrate_kbps: u32,
}

impl Options {
	pub fn new(format: Format, bitrate_kbps: Option<u32>) -> Self {
		let (min, max) = format.bitrate_range_kbps();
		Self {
			format,
			bitrate_kbps: bitrate_kbps
				.unwrap_or_else(|| format.default_bitrate_kbps())
				.clamp(min, max),
		}
	}

	/// Whether transcoding a file with these options is worthwhile. Files in lossless or
	/// rarely supported formats always are. Files which are already compressed are only
	/// worth transcoding when this would make them noticeably smaller.
	pub fn should_transcode(
		&self,
		audio_path: &Path,
		file_size: u64,
		duration_seconds: Option<i64>,
	) -> bool {
		match get_audio_format(audio_path) {
			None => false,
			Some(
				AudioFormat::AIFF
				| AudioFormat::APE
				| AudioFormat::FLAC
				| AudioFormat::MPC
				| AudioFormat::WAVE,
			) => true,
			Some(
				AudioFormat::MP3
				| AudioFormat::MP4
				| AudioFormat::M4B
				| AudioFormat::OGG
				| AudioFormat::OPUS,
			) => match duration_seconds {
				Some(duration) if duration > 0 => {
					let source_bitrate_kbps = file_size * 8 / duration as u64 / 1000;
					source_bitrate_kbps * 100 > self.bitrate_kbps as u64 * 115
				}
				_ => false,
			},
		}
	}
}

#[derive(Clone, Copy, Debug, Default)]
struct Progress {
	/// Number of bytes written to the partial file so far
	written: u64,
	/// `Some(true)` once the transcode has completed, `Some(false)` if it failed
	outcome: Option<bool>,
}

/// A transcode which is still being written to disk
#[derive(Clone)]
struct Job {
	partial_path: PathBuf,
	progress: watch::Receiver<Progress>,
}

pub enum Output {
	/// The transcoded audio is entirely available in a file
	File(PathBuf),
	/// The transcoded audio is still being produced
	InProgress(Reader),
}

/// Reads transcoded audio as it is being produced
pub struct Reader {
	file: tokio::fs::File,
	position: u64,
	progress: watch::Receiver<Progress>,
}

impl Reader {
	/// Returns the next chunk of audio, or `None` once the entire output has been read.
	pub async fn next_chunk(&mut self) -> Option<std::io::Result<Vec<u8>>> {
		let mut buffer = vec![0; 64 * 1024];
		loop {
			let progress = *self.progress.borrow_and_update();

			if progress.written > self.position || progress.outcome == Some(true) {
				match self.file.read(&mut buffer).await {
					Ok(0) if progress.outcome == Some(true) => return None,
					Ok(0) => (),
					Ok(n) => {
						self.position += n as u64;
						buffer.truncate(n);
						return Some(Ok(buffer));
					}
					Err(e) => return Some(Err(e)),
				}
			} else if progress.outcome == Some(false) {
				return Some(Err(std::io::Error::other("Transcoding failed")));
			}

			if self.progress.changed().await.is_err() {
				return Some(Err(std::io::Error::other("Transcoding was interrupted")));
			}
		}
	}
}

#[derive(Clone)]
pub struct Manager {
	cache_dir_path: PathBuf,
	ffmpeg_path: PathBuf,
	config_manager: config::Manager,
	is_available: Arc<OnceCell<bool>>,
	jobs: Arc<Mutex<HashMap<PathBuf, Job>>>,
	concurrency: Arc<Semaphore>,
}

impl Manager {
	pub fn new(
		cache_dir_path: PathBuf,
		ffmpeg_path: PathBuf,
		config_manager: config::Manager,
	) -> Self {
		// Transcodes interrupted by a previous shutdown can never be completed
		if let Ok(entries) = std::fs::read_dir(&cache_dir_path) {
			for entry in entries.flatten() {
				let path = entry.path();
				if path
					.extension()
					.is_some_and(|e| e == PARTIAL_FILE_EXTENSION)
				{
					std::fs::remove_file(path).ok();
				}
			}
		}

		Self {
			cache_dir_path,
			ffmpeg_path,
			config_manager,
			is_available: Arc::default(),
			jobs: Arc::default(),
			concurrency: Arc::new(Semaphore::new((num_cpus::get() / 2).clamp(1, 4))),
		}
	}

	/// Whether a working `ffmpeg` executable was found on this system.
	pub async fn is_available(&self) -> bool {
		*self
			.is_available
			.get_or_init(|| async {
				let result = tokio::process::Command::new(&self.ffmpeg_path)
					.arg("-version")
					.stdin(Stdio::null())
					.stdout(Stdio::null())
					.stderr(Stdio::null())
					.status()
					.await;
				let is_available = result.is_ok_and(|status| status.success());
				if is_available {
					info!(
						"Audio transcoding is available (using {:?})",
						self.ffmpeg_path
					);
				} else {
					info!(
						"Audio transcoding is not available because {:?} could not be executed",
						self.ffmpeg_path
					);
				}
				is_available
			})
			.await
	}

	/// Transcodes an audio file, or retrieves the result of an earlier transcode.
	/// When `complete_file` is set, this only returns once the entire output is available.
	pub async fn transcode(
		&self,
		audio_path: &Path,
		options: &Options,
		complete_file: bool,
	) -> Result<Output, Error> {
		if !self.is_available().await {
			return Err(Error::TranscodingUnavailable);
		}

		let metadata = tokio::fs::metadata(audio_path)
			.await
			.map_err(|e| Error::Io(audio_path.to_owned(), e))?;
		let cache_path = self.get_cache_path(audio_path, &metadata, options);

		enum Lookup {
			Cached,
			Running(Job),
			Started(Job, watch::Sender<Progress>),
		}

		// Looking up and registering jobs under a single lock guarantees that a file is
		// never transcoded twice at the same time.
		let lookup = {
			let mut jobs = self.jobs.lock().unwrap();
			match jobs.get(&cache_path) {
				Some(job) => Lookup::Running(job.clone()),
				None if cache_path.exists() => Lookup::Cached,
				None => {
					let (progress_sender, progress) = watch::channel(Progress::default());
					let mut partial_path = cache_path.clone();
					partial_path.set_extension(PARTIAL_FILE_EXTENSION);
					let job = Job {
						partial_path,
						progress,
					};
					jobs.insert(cache_path.clone(), job.clone());
					Lookup::Started(job, progress_sender)
				}
			}
		};

		let mut job = match lookup {
			Lookup::Running(job) => job,
			Lookup::Cached => {
				// Keeps frequently used files away from cache eviction
				let file = std::fs::OpenOptions::new().write(true).open(&cache_path);
				if let Ok(file) = file {
					file.set_modified(SystemTime::now()).ok();
				}
				return Ok(Output::File(cache_path));
			}
			Lookup::Started(job, progress_sender) => {
				tokio::spawn({
					let manager = self.clone();
					let audio_path = audio_path.to_owned();
					let options = options.clone();
					let job = job.clone();
					let cache_path = cache_path.clone();
					async move {
						manager
							.run_job(audio_path, options, job, cache_path, progress_sender)
							.await
					}
				});
				job
			}
		};

		// Waits until the job has produced enough for the caller to proceed. This also
		// allows transcodes which fail right away to be reported as errors.
		let is_ready = |p: &Progress| match complete_file {
			true => p.outcome.is_some(),
			false => p.outcome.is_some() || p.written > 0,
		};
		let progress = match job.progress.wait_for(is_ready).await {
			Ok(progress) => *progress,
			Err(_) => return Err(Error::Transcoding(audio_path.to_owned())),
		};

		match progress.outcome {
			Some(true) => Ok(Output::File(cache_path)),
			Some(false) => Err(Error::Transcoding(audio_path.to_owned())),
			None => {
				let file = tokio::fs::File::open(&job.partial_path)
					.await
					.map_err(|e| Error::Io(job.partial_path.clone(), e))?;
				Ok(Output::InProgress(Reader {
					file,
					position: 0,
					progress: job.progress,
				}))
			}
		}
	}

	fn get_cache_path(
		&self,
		audio_path: &Path,
		metadata: &std::fs::Metadata,
		options: &Options,
	) -> PathBuf {
		let mut hasher = DefaultHasher::new();
		audio_path.hash(&mut hasher);
		metadata.len().hash(&mut hasher);
		metadata.modified().ok().hash(&mut hasher);
		options.hash(&mut hasher);
		self.cache_dir_path.join(format!(
			"{:016x}.{}",
			hasher.finish(),
			options.format.name()
		))
	}

	async fn run_job(
		&self,
		audio_path: PathBuf,
		options: Options,
		job: Job,
		cache_path: PathBuf,
		progress: watch::Sender<Progress>,
	) {
		let result = self
			.run_ffmpeg(&audio_path, &options, &job.partial_path, &progress)
			.await;

		let result = match result {
			Ok(()) => tokio::fs::rename(&job.partial_path, &cache_path)
				.await
				.map_err(|e| e.to_string()),
			Err(e) => Err(e),
		};

		if let Err(e) = &result {
			error!("Failed to transcode `{}`: {e}", audio_path.display());
			tokio::fs::remove_file(&job.partial_path).await.ok();
		}

		self.jobs.lock().unwrap().remove(&cache_path);
		progress.send_modify(|p| p.outcome = Some(result.is_ok()));

		if result.is_ok() {
			self.evict(&cache_path).await;
		}
	}

	async fn run_ffmpeg(
		&self,
		audio_path: &Path,
		options: &Options,
		output_path: &Path,
		progress: &watch::Sender<Progress>,
	) -> Result<(), String> {
		let _permit = self
			.concurrency
			.acquire()
			.await
			.map_err(|e| e.to_string())?;

		tokio::fs::create_dir_all(&self.cache_dir_path)
			.await
			.map_err(|e| e.to_string())?;
		let mut output = tokio::fs::File::create(output_path)
			.await
			.map_err(|e| e.to_string())?;

		let mut child = tokio::process::Command::new(&self.ffmpeg_path)
			.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
			.arg(audio_path)
			.args(["-map", "0:a:0", "-vn", "-sn", "-dn"])
			.args(["-b:a", &format!("{}k", options.bitrate_kbps)])
			.args(options.format.encoder_arguments())
			.arg("pipe:1")
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.kill_on_drop(true)
			.spawn()
			.map_err(|e| e.to_string())?;

		let mut stdout = child
			.stdout
			.take()
			.ok_or("Could not capture ffmpeg output")?;
		let mut stderr = child
			.stderr
			.take()
			.ok_or("Could not capture ffmpeg errors")?;
		let stderr_reader = tokio::spawn(async move {
			let mut message = String::new();
			stderr.read_to_string(&mut message).await.ok();
			message
		});

		let mut buffer = vec![0; 64 * 1024];
		loop {
			let num_bytes = stdout.read(&mut buffer).await.map_err(|e| e.to_string())?;
			if num_bytes == 0 {
				break;
			}
			output
				.write_all(&buffer[..num_bytes])
				.await
				.map_err(|e| e.to_string())?;
			output.flush().await.map_err(|e| e.to_string())?;
			progress.send_modify(|p| p.written += num_bytes as u64);
		}
		drop(output);

		let status = child.wait().await.map_err(|e| e.to_string())?;
		let error_message = stderr_reader.await.unwrap_or_default();
		if !status.success() {
			return Err(format!(
				"ffmpeg failed ({status}): {}",
				error_message.trim()
			));
		}
		if progress.borrow().written == 0 {
			return Err(format!(
				"ffmpeg did not produce any output: {}",
				error_message.trim()
			));
		}
		if !error_message.trim().is_empty() {
			warn!(
				"ffmpeg reported issues while transcoding `{}`: {}",
				audio_path.display(),
				error_message.trim()
			);
		}

		Ok(())
	}

	/// Deletes the least recently used cache entries until the cache fits its size limit.
	async fn evict(&self, newest_entry: &Path) {
		let max_size = self.config_manager.get_transcode_cache_size_mb().await * 1024 * 1024;
		let cache_dir_path = self.cache_dir_path.clone();
		let newest_entry = newest_entry.to_owned();

		let eviction = tokio::task::spawn_blocking(move || {
			let mut entries = std::fs::read_dir(&cache_dir_path)?
				.flatten()
				.filter(|e| {
					e.path()
						.extension()
						.is_some_and(|x| x != PARTIAL_FILE_EXTENSION)
				})
				.filter_map(|e| {
					let metadata = e.metadata().ok()?;
					let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
					Some((e.path(), metadata.len(), modified))
				})
				.collect::<Vec<_>>();
			entries.sort_by_key(|(_, _, modified)| *modified);

			let mut total_size = entries.iter().map(|(_, size, _)| size).sum::<u64>();
			for (path, size, _) in entries {
				if total_size <= max_size {
					break;
				}
				// The entry which was just created is about to be served
				if path != newest_entry && std::fs::remove_file(&path).is_ok() {
					total_size -= size;
				}
			}
			Ok::<_, std::io::Error>(())
		});

		match eviction.await {
			Ok(Ok(())) => (),
			Ok(Err(e)) => error!("Failed to clean up transcoding cache: {e}"),
			Err(e) => error!("Failed to clean up transcoding cache: {e}"),
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::app::test::ContextBuilder;
	use crate::test_name;

	async fn make_manager(test_name: String, ffmpeg_path: &str) -> Manager {
		let context_builder = ContextBuilder::new(test_name);
		let cache_dir_path = context_builder.test_directory.join("transcodes");
		let ctx = context_builder.build().await;
		Manager::new(cache_dir_path, ffmpeg_path.into(), ctx.config_manager)
	}

	async fn read_all(output: Output) -> Vec<u8> {
		match output {
			Output::File(path) => tokio::fs::read(path).await.unwrap(),
			Output::InProgress(mut reader) => {
				let mut data = Vec::new();
				while let Some(chunk) = reader.next_chunk().await {
					data.extend(chunk.unwrap());
				}
				data
			}
		}
	}

	fn sample(extension: &str) -> PathBuf {
		["test-data", "formats", &format!("sample.{extension}")]
			.iter()
			.collect()
	}

	#[test]
	fn clamps_bitrates() {
		assert_eq!(Options::new(Format::Mp3, None).bitrate_kbps, 192);
		assert_eq!(Options::new(Format::Mp3, Some(1)).bitrate_kbps, 32);
		assert_eq!(Options::new(Format::Mp3, Some(128)).bitrate_kbps, 128);
		assert_eq!(Options::new(Format::Opus, Some(9000)).bitrate_kbps, 256);
	}

	#[test]
	fn skips_pointless_transcodes() {
		let options = Options::new(Format::Mp3, Some(128));
		let megabyte = 1024 * 1024;
		// Lossless and exotic sources are always transcoded
		assert!(options.should_transcode(Path::new("a.flac"), megabyte, None));
		assert!(options.should_transcode(Path::new("a.wav"), megabyte, Some(10)));
		assert!(options.should_transcode(Path::new("a.mpc"), megabyte, None));
		// A 5MB, 200s MP3 has a bitrate of about 200kbps
		assert!(options.should_transcode(Path::new("a.mp3"), 5 * megabyte, Some(200)));
		// A 3MB, 200s MP3 has a bitrate of about 125kbps
		assert!(!options.should_transcode(Path::new("a.mp3"), 3 * megabyte, Some(200)));
		assert!(!options.should_transcode(Path::new("a.mp3"), 5 * megabyte, None));
		assert!(!options.should_transcode(Path::new("a.jpg"), 5 * megabyte, Some(200)));
	}

	#[tokio::test]
	async fn reports_missing_ffmpeg() {
		let manager = make_manager(test_name!(), "this-is-not-ffmpeg").await;
		assert!(!manager.is_available().await);
		let options = Options::new(Format::Mp3, None);
		let result = manager.transcode(&sample("flac"), &options, false).await;
		assert!(matches!(result, Err(Error::TranscodingUnavailable)));
	}

	#[tokio::test]
	async fn transcodes_and_caches() {
		let manager = make_manager(test_name!(), "ffmpeg").await;
		if !manager.is_available().await {
			eprintln!("Skipping test because ffmpeg is not installed");
			return;
		}

		for (format, magic) in [
			(Format::Mp3, &b"ID3"[..]),
			(Format::Opus, &b"OggS"[..]),
			(Format::Aac, &[0xFF][..]),
		] {
			let options = Options::new(format, Some(64));

			let streamed = manager
				.transcode(&sample("flac"), &options, false)
				.await
				.unwrap();
			let streamed = read_all(streamed).await;
			assert!(streamed.starts_with(magic), "{format:?}");

			let cached = manager
				.transcode(&sample("flac"), &options, true)
				.await
				.unwrap();
			assert!(matches!(cached, Output::File(_)));
			assert_eq!(read_all(cached).await, streamed, "{format:?}");
		}

		let cache_entries = std::fs::read_dir(&manager.cache_dir_path).unwrap().count();
		assert_eq!(cache_entries, 3);
	}

	#[tokio::test]
	async fn concurrent_requests_share_a_transcode() {
		let manager = make_manager(test_name!(), "ffmpeg").await;
		if !manager.is_available().await {
			eprintln!("Skipping test because ffmpeg is not installed");
			return;
		}

		let options = Options::new(Format::Mp3, Some(96));
		let source = sample("aif");
		let (a, b, c) = tokio::join!(
			manager.transcode(&source, &options, false),
			manager.transcode(&source, &options, true),
			manager.transcode(&source, &options, false),
		);
		let a = read_all(a.unwrap()).await;
		let b = read_all(b.unwrap()).await;
		let c = read_all(c.unwrap()).await;
		assert!(!a.is_empty());
		assert_eq!(a, b);
		assert_eq!(a, c);
		assert_eq!(
			std::fs::read_dir(&manager.cache_dir_path).unwrap().count(),
			1
		);
	}

	#[tokio::test]
	async fn reports_failed_transcodes() {
		let manager = make_manager(test_name!(), "ffmpeg").await;
		if !manager.is_available().await {
			eprintln!("Skipping test because ffmpeg is not installed");
			return;
		}

		let not_audio: PathBuf = ["test-data", "config.toml"].iter().collect();
		let options = Options::new(Format::Mp3, None);
		for complete_file in [false, true] {
			let result = manager.transcode(&not_audio, &options, complete_file).await;
			assert!(matches!(result, Err(Error::Transcoding(_))));
		}
		assert_eq!(
			std::fs::read_dir(&manager.cache_dir_path).unwrap().count(),
			0
		);
	}

	#[tokio::test]
	async fn evicts_old_cache_entries() {
		let manager = make_manager(test_name!(), "ffmpeg").await;
		if !manager.is_available().await {
			eprintln!("Skipping test because ffmpeg is not installed");
			return;
		}

		manager
			.config_manager
			.apply_config(config::storage::Config {
				transcode_cache_size_mb: Some(0),
				..Default::default()
			})
			.await
			.unwrap();

		let source = sample("flac");
		for bitrate in [64, 96, 128] {
			let options = Options::new(Format::Mp3, Some(bitrate));
			manager.transcode(&source, &options, true).await.unwrap();
		}

		// Eviction runs after the transcode is reported as complete
		let count_entries = || std::fs::read_dir(&manager.cache_dir_path).unwrap().count();
		for _ in 0..50 {
			if count_entries() == 1 {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;
		}
		assert_eq!(count_entries(), 1);
	}
}
