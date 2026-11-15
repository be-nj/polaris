use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

const SEPARATOR: char = '\u{0}';

/// Subsonic clients refer to everything by opaque string identifiers. Polaris has no such
/// identifiers, so they are derived from the natural keys of the collection (names and
/// virtual paths), which keeps them stable across collection scans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id {
	Artist(String),
	Album { artists: Vec<String>, name: String },
	Song(PathBuf),
	Directory(PathBuf),
	CoverArt(PathBuf),
	Playlist(String),
}

impl Id {
	pub fn album(artists: &[String], name: &str) -> Self {
		Self::Album {
			artists: artists.to_vec(),
			name: name.to_owned(),
		}
	}

	pub fn song(virtual_path: &Path) -> Self {
		Self::Song(virtual_path.to_owned())
	}

	pub fn directory(virtual_path: &Path) -> Self {
		Self::Directory(virtual_path.to_owned())
	}

	pub fn cover_art(virtual_path: &Path) -> Self {
		Self::CoverArt(virtual_path.to_owned())
	}

	pub fn encode(&self) -> String {
		let (prefix, payload) = match self {
			Id::Artist(name) => ("ar", name.clone()),
			Id::Album { artists, name } => {
				let mut payload = name.clone();
				for artist in artists {
					payload.push(SEPARATOR);
					payload.push_str(artist);
				}
				("al", payload)
			}
			Id::Song(path) => ("so", path.to_string_lossy().into_owned()),
			Id::Directory(path) => ("di", path.to_string_lossy().into_owned()),
			Id::CoverArt(path) => ("ca", path.to_string_lossy().into_owned()),
			Id::Playlist(name) => ("pl", name.clone()),
		};
		format!("{prefix}-{}", URL_SAFE_NO_PAD.encode(payload))
	}

	pub fn parse(id: &str) -> Option<Self> {
		let (prefix, payload) = id.split_once('-')?;
		let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
		let payload = String::from_utf8(payload).ok()?;
		match prefix {
			"ar" => Some(Id::Artist(payload)),
			"al" => {
				let mut parts = payload.split(SEPARATOR).map(str::to_owned);
				let name = parts.next()?;
				Some(Id::Album {
					artists: parts.collect(),
					name,
				})
			}
			"so" => Some(Id::Song(PathBuf::from(payload))),
			"di" => Some(Id::Directory(PathBuf::from(payload))),
			"ca" => Some(Id::CoverArt(PathBuf::from(payload))),
			"pl" => Some(Id::Playlist(payload)),
			_ => None,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn ids_round_trip() {
		let ids = [
			Id::Artist("Tobokegao".to_owned()),
			Id::Album {
				artists: vec!["Khemmis".to_owned(), "Someone / Else".to_owned()],
				name: "Hunted".to_owned(),
			},
			Id::Album {
				artists: vec![],
				name: "".to_owned(),
			},
			Id::Song(PathBuf::from_iter(["root", "ピクニック", "01 - a?b&c.mp3"])),
			Id::Directory(PathBuf::from("root")),
			Id::CoverArt(PathBuf::from_iter(["root", "Folder.jpg"])),
			Id::Playlist("my playlist".to_owned()),
		];
		for id in ids {
			let encoded = id.encode();
			assert!(encoded
				.chars()
				.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
			assert_eq!(Id::parse(&encoded), Some(id));
		}
	}

	#[test]
	fn rejects_malformed_ids() {
		assert_eq!(Id::parse(""), None);
		assert_eq!(Id::parse("so"), None);
		assert_eq!(Id::parse("xx-YQ"), None);
		assert_eq!(Id::parse("so-***"), None);
	}
}
