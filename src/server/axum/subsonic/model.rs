use std::path::Path;

use serde_json::{json, Value};

use crate::app::index;
use crate::utils::{get_audio_format, AudioFormat};

use super::id::Id;

/// Formats a UNIX timestamp as an ISO 8601 UTC date.
pub fn iso8601(unix_seconds: i64) -> String {
	let days = unix_seconds.div_euclid(86_400);
	let seconds_of_day = unix_seconds.rem_euclid(86_400);

	// Civil date from day count (Howard Hinnant's `civil_from_days` algorithm)
	let z = days + 719_468;
	let era = z.div_euclid(146_097);
	let day_of_era = z.rem_euclid(146_097);
	let year_of_era =
		(day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
	let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
	let month_index = (5 * day_of_year + 2) / 153;
	let day = day_of_year - (153 * month_index + 2) / 5 + 1;
	let month = if month_index < 10 {
		month_index + 3
	} else {
		month_index - 9
	};
	let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };

	format!(
		"{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
		seconds_of_day / 3600,
		(seconds_of_day % 3600) / 60,
		seconds_of_day % 60
	)
}

pub fn content_type(path: &Path) -> Option<&'static str> {
	match get_audio_format(path)? {
		AudioFormat::AIFF => Some("audio/aiff"),
		AudioFormat::APE => Some("audio/x-monkeys-audio"),
		AudioFormat::FLAC => Some("audio/flac"),
		AudioFormat::MP3 => Some("audio/mpeg"),
		AudioFormat::MP4 | AudioFormat::M4B => Some("audio/mp4"),
		AudioFormat::MPC => Some("audio/x-musepack"),
		AudioFormat::OGG => Some("audio/ogg"),
		AudioFormat::OPUS => Some("audio/ogg"),
		AudioFormat::WAVE => Some("audio/wav"),
	}
}

pub fn index_letter(name: &str) -> String {
	match name.chars().next() {
		Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
		_ => "#".to_owned(),
	}
}

fn display_path(path: &Path) -> String {
	path.components()
		.map(|c| c.as_os_str().to_string_lossy())
		.collect::<Vec<_>>()
		.join("/")
}

fn display_names(names: &[String]) -> Option<String> {
	(!names.is_empty()).then(|| names.join(", "))
}

fn artist_references(names: &[String]) -> Vec<Value> {
	names
		.iter()
		.map(|name| json!({ "id": Id::Artist(name.clone()).encode(), "name": name }))
		.collect()
}

fn genre_references<'a>(names: impl IntoIterator<Item = &'a String>) -> Vec<Value> {
	let mut seen = Vec::new();
	for name in names {
		if !seen.contains(&name) {
			seen.push(name);
		}
	}
	seen.into_iter()
		.map(|name| json!({ "name": name }))
		.collect()
}

/// Artists an album is filed under for a given song. This mirrors how the collection index
/// attributes songs to albums.
fn main_artists(song: &index::Song) -> &[String] {
	if song.album_artists.is_empty() {
		&song.artists
	} else {
		&song.album_artists
	}
}

pub fn song(song: &index::Song) -> Value {
	let path = &song.virtual_path;
	let title = song.title.clone().or_else(|| {
		path.file_stem()
			.map(|stem| stem.to_string_lossy().into_owned())
	});
	let album_artists = main_artists(song);
	let album_id = match (&song.album, album_artists.is_empty()) {
		(Some(album), false) => Some(Id::album(album_artists, album).encode()),
		_ => None,
	};
	let artists = if song.artists.is_empty() {
		album_artists
	} else {
		&song.artists
	};

	json!({
		"id": Id::song(path).encode(),
		"parent": path.parent().map(|p| Id::directory(p).encode()),
		"isDir": false,
		"title": title,
		"album": song.album,
		"artist": display_names(artists),
		"track": song.track_number,
		"discNumber": song.disc_number,
		"year": song.year,
		"genre": song.genres.first(),
		"coverArt": song.artwork.as_ref().map(|a| Id::cover_art(a).encode()),
		"duration": song.duration,
		"suffix": path.extension().map(|e| e.to_string_lossy().to_lowercase()),
		"contentType": content_type(path),
		"path": display_path(path),
		"created": iso8601(song.date_added),
		"albumId": album_id,
		"artistId": artists.first().map(|a| Id::Artist(a.clone()).encode()),
		"type": "music",
		"mediaType": "song",
		"isVideo": false,
		"genres": genre_references(&song.genres),
		"artists": artist_references(artists),
		"albumArtists": artist_references(album_artists),
		"displayArtist": display_names(artists),
		"displayAlbumArtist": display_names(album_artists),
		"displayComposer": display_names(&song.composers),
	})
}

pub fn songs(songs: &[index::Song]) -> Vec<Value> {
	songs.iter().map(song).collect()
}

/// Album in the tag-based (`ID3`) representation, without its songs.
pub fn album(album: &index::Album) -> Value {
	let header = &album.header;
	let genres = album.songs.iter().flat_map(|s| s.genres.iter());
	json!({
		"id": Id::album(&header.artists, &header.name).encode(),
		"name": header.name,
		"artist": display_names(&header.artists),
		"artistId": header.artists.first().map(|a| Id::Artist(a.clone()).encode()),
		"coverArt": header.artwork.as_ref().map(|a| Id::cover_art(a).encode()),
		"songCount": album.songs.len(),
		"duration": album.songs.iter().filter_map(|s| s.duration).sum::<i64>(),
		"created": iso8601(header.date_added),
		"year": header.year,
		"genre": album.songs.iter().find_map(|s| s.genres.first()),
		"genres": genre_references(genres),
		"artists": artist_references(&header.artists),
		"displayArtist": display_names(&header.artists),
	})
}

pub fn album_with_songs(album_data: &index::Album) -> Value {
	let mut value = album(album_data);
	value["song"] = Value::Array(songs(&album_data.songs));
	value
}

/// Album in the folder-based representation, where it appears as a directory.
pub fn album_as_directory(album: &index::Album) -> Value {
	let header = &album.header;
	json!({
		"id": Id::album(&header.artists, &header.name).encode(),
		"parent": header.artists.first().map(|a| Id::Artist(a.clone()).encode()),
		"isDir": true,
		"title": header.name,
		"album": header.name,
		"artist": display_names(&header.artists),
		"coverArt": header.artwork.as_ref().map(|a| Id::cover_art(a).encode()),
		"year": header.year,
		"created": iso8601(header.date_added),
	})
}

/// `online_pictures` tells whether pictures of artists who have none in the collection are
/// looked up online. Whether one exists is then only known once it is asked for, which is
/// why such artists refer to a picture which may turn out to be missing.
pub fn artist(header: &index::ArtistHeader, online_pictures: bool) -> Value {
	let name = header.name.to_string();
	let cover_art = match &header.artwork {
		Some(artwork) => Some(Id::cover_art(artwork).encode()),
		None if online_pictures => Some(Id::Artist(name.clone()).encode()),
		None => None,
	};
	json!({
		"id": Id::Artist(name.clone()).encode(),
		"name": name,
		"coverArt": cover_art,
		"albumCount": header.num_albums_as_performer + header.num_albums_as_additional_performer,
	})
}

pub fn directory(path: &Path) -> Value {
	let name = path
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default();
	json!({
		"id": Id::directory(path).encode(),
		"parent": path
			.parent()
			.filter(|p| !p.as_os_str().is_empty())
			.map(|p| Id::directory(p).encode()),
		"isDir": true,
		"title": name,
	})
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn formats_dates() {
		assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
		assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
		assert_eq!(iso8601(1_763_211_723), "2025-11-15T13:02:03Z");
		assert_eq!(iso8601(-1), "1969-12-31T23:59:59Z");
	}

	#[test]
	fn picks_index_letters() {
		assert_eq!(index_letter("khemmis"), "K");
		assert_eq!(index_letter("ピクニック"), "ピ");
		assert_eq!(index_letter("2Pac"), "#");
		assert_eq!(index_letter(""), "#");
	}
}
