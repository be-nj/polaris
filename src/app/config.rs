use std::{
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

use log::{error, info};
use notify_debouncer_full::{
	notify::{event::ModifyKind, EventKind, RecommendedWatcher, RecursiveMode},
	DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache,
};
use rand::{rngs::OsRng, RngCore};
use regex::Regex;
use tokio::sync::{futures::Notified, Notify, RwLock};

use crate::app::Error;

mod mounts;
mod oidc;
pub mod storage;
mod user;

pub use mounts::*;
pub use oidc::*;
pub use user::*;

use super::{auth, transcode};

pub const DEFAULT_SCAN_EXCLUDE_PATTERN: &str = r"^\.";

#[derive(Debug, Clone, Default)]
pub struct Config {
	pub album_art_pattern: Option<Regex>,
	pub scan_exclude_pattern: Option<Regex>,
	pub transcode_cache_size_mb: Option<u64>,
	pub ddns_update_url: Option<http::Uri>,
	pub mount_dirs: Vec<MountDir>,
	pub users: Vec<User>,
	pub public_url: Option<String>,
	pub oidc_providers: Vec<OidcProvider>,
}

impl TryFrom<storage::Config> for Config {
	type Error = Error;

	fn try_from(c: storage::Config) -> Result<Self, Self::Error> {
		let mut config = Config::default();
		config.set_mounts(c.mount_dirs)?;
		config.set_users(c.users)?;

		config.album_art_pattern = match c.album_art_pattern.as_deref().map(Regex::new) {
			Some(Ok(u)) => Some(u),
			Some(Err(_)) => return Err(Error::IndexAlbumArtPatternInvalid),
			None => None,
		};

		config.scan_exclude_pattern = match c.scan_exclude_pattern.as_deref().map(Regex::new) {
			Some(Ok(u)) => Some(u),
			Some(Err(_)) => return Err(Error::ScanExcludePatternInvalid),
			None => None,
		};

		config.transcode_cache_size_mb = c.transcode_cache_size_mb;

		config.public_url = c
			.public_url
			.map(|u| u.trim().trim_end_matches('/').to_owned())
			.filter(|u| !u.is_empty());
		for provider in c.oidc_providers {
			let provider = OidcProvider::try_from(provider)?;
			if config
				.oidc_providers
				.iter()
				.any(|p| p.name == provider.name)
			{
				return Err(Error::OidcProviderInvalid(
					provider.name,
					"`name` must be unique".to_owned(),
				));
			}
			config.oidc_providers.push(provider);
		}

		config.ddns_update_url = match c.ddns_update_url.map(http::Uri::try_from) {
			Some(Ok(u)) => Some(u),
			Some(Err(_)) => return Err(Error::DDNSUpdateURLInvalid),
			None => None,
		};

		Ok(config)
	}
}

impl From<Config> for storage::Config {
	fn from(c: Config) -> Self {
		Self {
			album_art_pattern: c.album_art_pattern.map(|p| p.as_str().to_owned()),
			scan_exclude_pattern: c.scan_exclude_pattern.map(|p| p.as_str().to_owned()),
			transcode_cache_size_mb: c.transcode_cache_size_mb,
			mount_dirs: c.mount_dirs.into_iter().map(|d| d.into()).collect(),
			ddns_update_url: c.ddns_update_url.map(|u| u.to_string()),
			users: c.users.into_iter().map(|u| u.into()).collect(),
			public_url: c.public_url,
			oidc_providers: c.oidc_providers.into_iter().map(|p| p.into()).collect(),
		}
	}
}

#[derive(Clone)]
pub struct Manager {
	config_file_path: PathBuf,
	config: Arc<RwLock<Config>>,
	auth_secret: auth::Secret,
	#[allow(dead_code)]
	file_watcher: Arc<Debouncer<RecommendedWatcher, RecommendedCache>>,
	change_notify: Arc<Notify>,
}

impl Manager {
	pub async fn new(config_file_path: &Path, auth_secret: auth::Secret) -> Result<Self, Error> {
		if let Some(parent) = config_file_path.parent() {
			tokio::fs::create_dir_all(parent)
				.await
				.map_err(|e| Error::Io(parent.to_owned(), e))?;
		}

		match tokio::fs::File::create_new(config_file_path).await {
			Ok(_) => (),
			Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
			Err(e) => {
				error!("Failed to create config file at {config_file_path:#?}: {e}");
				return Err(Error::Io(config_file_path.to_owned(), e));
			}
		};

		let notify = Arc::new(Notify::new());
		let mut debouncer = notify_debouncer_full::new_debouncer(Duration::from_secs(1), None, {
			let notify = notify.clone();
			move |result: DebounceEventResult| match result {
				Ok(events) => {
					let is_ignorable = |e: &DebouncedEvent| {
						use EventKind::*;
						matches!(e.kind, Access(_) | Modify(ModifyKind::Metadata(_)) | Other)
					};
					if !events.iter().all(is_ignorable) {
						notify.notify_waiters();
					}
				}
				Err(e) => error!("Config file watcher error: `{e:?}`"),
			}
		})?;

		debouncer.watch(config_file_path, RecursiveMode::NonRecursive)?;

		let manager = Self {
			config_file_path: config_file_path.to_owned(),
			config: Arc::new(RwLock::new(Config::default())),
			auth_secret,
			file_watcher: Arc::new(debouncer),
			change_notify: Arc::default(),
		};

		tokio::task::spawn({
			let manager = manager.clone();
			async move {
				loop {
					notify.notified().await;
					if let Err(e) = manager.reload_config().await {
						error!("Configuration error: {e}");
					} else {
						info!("Successfully applied configuration change");
					}
				}
			}
		});

		manager.reload_config().await?;

		Ok(manager)
	}

	pub fn on_config_change(&self) -> Notified<'_> {
		self.change_notify.notified()
	}

	async fn reload_config(&self) -> Result<(), Error> {
		let config = Self::read_config(&self.config_file_path).await?;
		self.apply_config(config).await
	}

	async fn read_config(config_file_path: &Path) -> Result<storage::Config, Error> {
		let config_content = tokio::fs::read_to_string(config_file_path)
			.await
			.map_err(|e| Error::Io(config_file_path.to_owned(), e))?;
		toml::de::from_str::<storage::Config>(&config_content).map_err(Error::ConfigDeserialization)
	}

	pub async fn save_config(&self) -> Result<(), Error> {
		let serialized = toml::ser::to_string_pretty::<storage::Config>(
			&self.config.read().await.clone().into(),
		)
		.map_err(Error::ConfigSerialization)?;
		tokio::fs::write(&self.config_file_path, serialized.as_bytes())
			.await
			.map_err(|e| Error::Io(self.config_file_path.clone(), e))?;
		Ok(())
	}

	pub async fn apply_config(&self, new_config: storage::Config) -> Result<(), Error> {
		let mut config = self.config.write().await;
		*config = new_config.try_into()?;
		self.change_notify.notify_waiters();
		Ok(())
	}

	async fn mutate<F: FnOnce(&mut Config)>(&self, op: F) -> Result<(), Error> {
		self.mutate_fallible(|c| {
			op(c);
			Ok(())
		})
		.await
	}

	async fn mutate_fallible<F: FnOnce(&mut Config) -> Result<(), Error>>(
		&self,
		op: F,
	) -> Result<(), Error> {
		{
			let mut config = self.config.write().await;
			op(&mut config)?;
		}
		self.change_notify.notify_waiters();
		self.save_config().await?;
		Ok(())
	}

	pub async fn get_index_album_art_pattern(&self) -> Regex {
		let config = self.config.read().await;
		let pattern = config.album_art_pattern.clone();
		pattern.unwrap_or_else(|| Regex::new("Folder.(jpeg|jpg|png)").unwrap())
	}

	pub async fn set_index_album_art_pattern(&self, regex: Regex) -> Result<(), Error> {
		self.mutate(|c| {
			c.album_art_pattern = Some(regex);
		})
		.await
	}

	/// Pattern matched against the names of files and directories that should be skipped
	/// while scanning the collection. Hidden entries (leading dot) are skipped by default.
	/// An empty pattern disables exclusion entirely.
	pub async fn get_scan_exclude_pattern(&self) -> Option<Regex> {
		let config = self.config.read().await;
		match &config.scan_exclude_pattern {
			Some(pattern) if pattern.as_str().is_empty() => None,
			Some(pattern) => Some(pattern.clone()),
			None => Some(Regex::new(DEFAULT_SCAN_EXCLUDE_PATTERN).unwrap()),
		}
	}

	pub async fn set_scan_exclude_pattern(&self, regex: Regex) -> Result<(), Error> {
		self.mutate(|c| {
			c.scan_exclude_pattern = Some(regex);
		})
		.await
	}

	/// Maximum amount of disk space used to keep transcoded audio files around
	pub async fn get_transcode_cache_size_mb(&self) -> u64 {
		let config = self.config.read().await;
		config
			.transcode_cache_size_mb
			.unwrap_or(transcode::DEFAULT_CACHE_SIZE_MB)
	}

	pub async fn get_ddns_update_url(&self) -> Option<http::Uri> {
		self.config.read().await.ddns_update_url.clone()
	}

	pub async fn set_ddns_update_url(&self, url: Option<http::Uri>) -> Result<(), Error> {
		self.mutate(|c| {
			c.ddns_update_url = url;
		})
		.await
	}

	pub async fn get_users(&self) -> Vec<User> {
		self.config.read().await.users.to_vec()
	}

	pub async fn get_user(&self, username: &str) -> Result<User, Error> {
		let config = self.config.read().await;
		config
			.get_user(username)
			.cloned()
			.ok_or(Error::UserNotFound)
	}

	pub async fn create_user(
		&self,
		username: &str,
		password: &str,
		admin: bool,
	) -> Result<(), Error> {
		self.mutate_fallible(|c| c.create_user(username, password, admin))
			.await
	}

	pub async fn login(&self, username: &str, password: &str) -> Result<auth::Token, Error> {
		let config = self.config.read().await;
		config.login(username, password, &self.auth_secret)
	}

	/// Signs in a user whose identity was established by an external identity provider.
	/// The user is created if needed. When `is_admin` is specified, the administrator
	/// status of the user is updated accordingly.
	pub async fn login_external(
		&self,
		username: &str,
		is_admin: Option<bool>,
	) -> Result<(auth::Token, bool), Error> {
		let needs_update = {
			let config = self.config.read().await;
			match config.get_user(username) {
				None => true,
				Some(user) => is_admin.is_some_and(|a| a != user.is_admin()),
			}
		};

		if needs_update {
			self.mutate_fallible(|c| {
				if !c.exists(username) {
					// These users never sign in with a password. A random one satisfies
					// the requirement for every user to have a password.
					let mut password = [0u8; 32];
					OsRng.fill_bytes(&mut password);
					let password = password
						.iter()
						.map(|b| format!("{b:02x}"))
						.collect::<String>();
					c.create_user(username, &password, false)?;
				}
				if let Some(is_admin) = is_admin {
					c.set_is_admin(username, is_admin)?;
				}
				Ok(())
			})
			.await?;
		}

		let config = self.config.read().await;
		let user = config.get_user(username).ok_or(Error::UserNotFound)?;
		let token = config.issue_token(username, &self.auth_secret)?;
		Ok((token, user.is_admin()))
	}

	#[cfg(test)]
	pub async fn get_config(&self) -> Config {
		self.config.read().await.clone()
	}

	pub async fn get_public_url(&self) -> Option<String> {
		self.config.read().await.public_url.clone()
	}

	pub async fn get_oidc_providers(&self) -> Vec<OidcProvider> {
		self.config.read().await.oidc_providers.clone()
	}

	pub async fn set_is_admin(&self, username: &str, is_admin: bool) -> Result<(), Error> {
		self.mutate_fallible(|c| c.set_is_admin(username, is_admin))
			.await
	}

	pub async fn set_password(&self, username: &str, password: &str) -> Result<(), Error> {
		self.mutate_fallible(|c| c.set_password(username, password))
			.await
	}

	pub async fn authenticate(
		&self,
		auth_token: &auth::Token,
		scope: auth::Scope,
	) -> Result<auth::Authorization, Error> {
		let config = self.config.read().await;
		config.authenticate(auth_token, scope, &self.auth_secret)
	}

	pub async fn delete_user(&self, username: &str) -> Result<(), Error> {
		self.mutate(|c| c.delete_user(username)).await
	}

	pub async fn get_mounts(&self) -> Vec<MountDir> {
		let config = self.config.read().await;
		config.mount_dirs.to_vec()
	}

	pub async fn resolve_virtual_path<P: AsRef<Path>>(
		&self,
		virtual_path: P,
	) -> Result<PathBuf, Error> {
		let config = self.config.read().await;
		config.resolve_virtual_path(virtual_path)
	}

	pub async fn resolve_virtual_paths<P: AsRef<Path>>(
		&self,
		virtual_paths: &[P],
	) -> Vec<Result<PathBuf, Error>> {
		let config = self.config.read().await;
		virtual_paths
			.iter()
			.map(|p| config.resolve_virtual_path(p))
			.collect()
	}

	#[allow(unused)]
	pub async fn virtualize_path<P: AsRef<Path>>(&self, real_path: P) -> Result<PathBuf, Error> {
		let config = self.config.read().await;
		config.virtualize_path(real_path)
	}

	pub async fn virtualize_paths<P: AsRef<Path>>(
		&self,
		real_paths: &[P],
	) -> Vec<Result<PathBuf, Error>> {
		let config = self.config.read().await;
		real_paths
			.iter()
			.map(|p| config.virtualize_path(p))
			.collect()
	}

	pub async fn set_mounts(&self, mount_dirs: Vec<storage::MountDir>) -> Result<(), Error> {
		self.mutate_fallible(|c| c.set_mounts(mount_dirs)).await
	}
}

#[cfg(test)]
mod test {
	use crate::app::test;
	use crate::test_name;

	use super::*;

	#[tokio::test]
	async fn blank_config_round_trip() {
		let config_path = PathBuf::from_iter(["test-data", "blank.toml"]);
		let manager = Manager::new(&config_path, auth::Secret([0; 32]))
			.await
			.unwrap();
		let config: storage::Config = manager.config.read().await.clone().into();
		assert_eq!(config, storage::Config::default());
	}

	#[tokio::test]
	async fn can_read_config() {
		let config_path = PathBuf::from_iter(["test-data", "config.toml"]);
		let manager = Manager::new(&config_path, auth::Secret([0; 32]))
			.await
			.unwrap();
		let config: storage::Config = manager.config.read().await.clone().into();

		assert_eq!(
			config.album_art_pattern,
			Some(r#"^Folder\.(png|jpg|jpeg)$"#.to_owned())
		);
		assert_eq!(
			config.mount_dirs,
			vec![storage::MountDir {
				source: PathBuf::from("test-data/small-collection"),
				name: "root".to_owned(),
			}]
		);
		assert_eq!(config.users[0].name, "test_user");
		assert_eq!(config.users[0].admin, Some(true));
		assert_eq!(
			config.users[0].initial_password,
			Some("very_secret_password".to_owned())
		);
		assert!(config.users[0].hashed_password.is_some());
	}

	#[tokio::test]
	async fn can_write_config() {
		let ctx = test::ContextBuilder::new(test_name!()).build().await;
		ctx.config_manager
			.create_user("Walter", "example_password", false)
			.await
			.unwrap();

		let manager = Manager::new(&ctx.config_manager.config_file_path, auth::Secret([0; 32]))
			.await
			.unwrap();
		assert!(manager.get_user("Walter").await.is_ok());
	}
}
