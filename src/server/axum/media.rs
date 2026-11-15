//! Helpers to serve audio files, shared between the Polaris and Subsonic APIs.

use std::ops::Bound;
use std::path::Path;

use axum::{
	body::{Body, Bytes},
	response::{IntoResponse, Response},
};
use axum_extra::headers::Range;
use axum_range::{KnownSize, Ranged};
use http::{header, HeaderValue};

use crate::app::{self, transcode, App};

/// Serves a file from disk, honoring range requests.
pub async fn file(path: &Path, range: Option<Range>) -> std::io::Result<Response> {
	let file = tokio::fs::File::open(path).await?;
	let body = KnownSize::file(file).await?;
	Ok(Ranged::new(range, body).into_response())
}

/// Requests for anything but the beginning of a file can only be served once all of it is known.
fn requires_complete_file(range: &Option<Range>) -> bool {
	range.as_ref().is_some_and(|range| {
		range
			.satisfiable_ranges(u64::MAX)
			.any(|(start, _)| start != Bound::Included(0))
	})
}

/// Serves a transcoded version of an audio file.
///
/// Returns `None` when transcoding would be pointless, which is the case for files that are
/// already compressed to the requested bitrate or less.
pub async fn transcoded(
	app: &App,
	virtual_path: &Path,
	audio_path: &Path,
	options: &transcode::Options,
	range: Option<Range>,
) -> Result<Option<Response>, app::Error> {
	let file_size = tokio::fs::metadata(audio_path)
		.await
		.map_err(|e| app::Error::Io(audio_path.to_owned(), e))?
		.len();
	let duration = app
		.index_manager
		.get_songs(vec![virtual_path.to_owned()])
		.await
		.into_iter()
		.next()
		.and_then(Result::ok)
		.and_then(|song| song.duration);

	if !options.should_transcode(audio_path, file_size, duration) {
		return Ok(None);
	}

	let output = app
		.transcode_manager
		.transcode(audio_path, options, requires_complete_file(&range))
		.await?;

	let mut response = match output {
		transcode::Output::File(path) => file(&path, range)
			.await
			.map_err(|e| app::Error::Io(path, e))?,
		transcode::Output::InProgress(reader) => {
			// The size of the output is not known yet, which rules out range requests.
			// These become available once the transcode has completed.
			let chunks = futures_util::stream::unfold(reader, |mut reader| async move {
				let chunk = reader.next_chunk().await?;
				Some((chunk.map(Bytes::from), reader))
			});
			let mut response = Body::from_stream(chunks).into_response();
			response
				.headers_mut()
				.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
			response
		}
	};

	response.headers_mut().insert(
		header::CONTENT_TYPE,
		HeaderValue::from_static(options.format.content_type()),
	);

	Ok(Some(response))
}

#[cfg(test)]
mod test {
	use super::*;
	use axum_extra::headers::{Header, HeaderMapExt};

	fn range(value: &str) -> Option<Range> {
		let mut headers = http::HeaderMap::new();
		headers.insert(Range::name(), value.parse().unwrap());
		headers.typed_get::<Range>()
	}

	#[test]
	fn detects_requests_needing_complete_files() {
		assert!(!requires_complete_file(&None));
		assert!(!requires_complete_file(&range("bytes=0-")));
		assert!(!requires_complete_file(&range("bytes=0-1023")));
		assert!(requires_complete_file(&range("bytes=1024-")));
		assert!(requires_complete_file(&range("bytes=-500")));
		assert!(requires_complete_file(&range("bytes=0-10, 500-600")));
	}
}
