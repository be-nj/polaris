use http::{header, HeaderValue, StatusCode};
use std::path::PathBuf;

use crate::server::dto::{self, ThumbnailSize};
use crate::server::test::{constants::*, protocol, ServiceType, TestService};
use crate::test_name;

#[tokio::test]
async fn songs_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::songs(dto::GetSongsBulkInput::default());
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn songs_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let valid_path =
		PathBuf::from_iter([TEST_MOUNT_NAME, "Khemmis", "Hunted", "02 - Candlelight.mp3"]);
	let invalid_path = PathBuf::from_iter(["oink.mp3"]);

	let request = protocol::songs(dto::GetSongsBulkInput {
		paths: vec![valid_path.clone(), invalid_path.clone()],
	});

	let response = service
		.fetch_json::<_, dto::GetSongsBulkOutput>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);

	let payload = response.body();
	assert_eq!(payload.songs[0].path, valid_path);
	assert_eq!(payload.not_found, vec![invalid_path]);
}

#[tokio::test]
async fn audio_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "Hunted", "02 - Candlelight.mp3"]
		.iter()
		.collect();

	let request = protocol::audio(&path);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn audio_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "Hunted", "02 - Candlelight.mp3"]
		.iter()
		.collect();

	let request = protocol::audio(&path);
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(response.body().len(), 24_142);
	assert_eq!(
		response.headers().get(header::CONTENT_LENGTH).unwrap(),
		"24142"
	);
}

#[tokio::test]
async fn audio_does_not_encode_content() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "Hunted", "02 - Candlelight.mp3"]
		.iter()
		.collect();

	let mut request = protocol::audio(&path);
	let headers = request.headers_mut();
	headers.append(
		header::ACCEPT_ENCODING,
		HeaderValue::from_str("gzip, deflate, br").unwrap(),
	);

	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(response.body().len(), 24_142);
	assert_eq!(response.headers().get(header::TRANSFER_ENCODING), None);
	assert_eq!(
		response.headers().get(header::CONTENT_LENGTH).unwrap(),
		"24142"
	);
}

#[tokio::test]
async fn audio_partial_content() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "Hunted", "02 - Candlelight.mp3"]
		.iter()
		.collect();

	let mut request = protocol::audio(&path);
	let headers = request.headers_mut();
	headers.append(
		header::RANGE,
		HeaderValue::from_str("bytes=100-299").unwrap(),
	);

	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
	assert_eq!(response.body().len(), 200);
	assert_eq!(
		response.headers().get(header::CONTENT_LENGTH).unwrap(),
		"200"
	);
}

#[tokio::test]
async fn audio_bad_path_returns_not_found() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login().await;

	let path: PathBuf = ["not_my_collection"].iter().collect();

	let request = protocol::audio(&path);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn peaks_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "Hunted", "02 - Candlelight.mp3"]
		.iter()
		.collect();

	let request = protocol::peaks(&path);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn peaks_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "Hunted", "02 - Candlelight.mp3"]
		.iter()
		.collect();

	let request = protocol::peaks(&path);
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert!(response.body().len() % 2 == 0);
	assert!(!response.body().is_empty());
}

#[tokio::test]
async fn peaks_bad_path_returns_not_found() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login().await;

	let path: PathBuf = ["not_my_collection"].iter().collect();

	let request = protocol::peaks(&path);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn thumbnail_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "Hunted", "Folder.jpg"]
		.iter()
		.collect();

	let size = None;
	let pad = None;
	let request = protocol::thumbnail(&path, size, pad);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn thumbnail_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "Hunted", "Folder.jpg"]
		.iter()
		.collect();

	let size = None;
	let pad = None;
	let request = protocol::thumbnail(&path, size, pad);
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn artist_thumbnail_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::artist_thumbnail("Khemmis");
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn artist_thumbnail_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::artist_thumbnail("Khemmis");
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert!(response.body().starts_with(&[0xFF, 0xD8]));
}

#[tokio::test]
async fn artist_thumbnail_without_picture_returns_not_found() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	for name in ["Tobokegao", "Not An Artist"] {
		let request = protocol::artist_thumbnail(name);
		let response = service.fetch(&request).await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
	}
}

#[tokio::test]
async fn thumbnail_bad_path_returns_not_found() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login().await;

	let path: PathBuf = ["not_my_collection"].iter().collect();

	let size = None;
	let pad = None;
	let request = protocol::thumbnail(&path, size, pad);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn thumbnail_size_default() {
	thumbnail_size(&test_name!(), None, None, 400).await;
}

#[tokio::test]
async fn thumbnail_size_small() {
	thumbnail_size(&test_name!(), Some(ThumbnailSize::Small), None, 400).await;
}

#[tokio::test]
async fn thumbnail_size_large() {
	thumbnail_size(&test_name!(), Some(ThumbnailSize::Large), None, 1200).await;
}

#[tokio::test]
async fn thumbnail_size_native() {
	thumbnail_size(&test_name!(), Some(ThumbnailSize::Native), None, 1423).await;
}

async fn thumbnail_size(name: &str, size: Option<ThumbnailSize>, pad: Option<bool>, expected: u32) {
	let mut service = ServiceType::new(name).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let path: PathBuf = [TEST_MOUNT_NAME, "Tobokegao", "Picnic", "Folder.png"]
		.iter()
		.collect();

	let request = protocol::thumbnail(&path, size, pad);
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	let thumbnail = image::load_from_memory(response.body()).unwrap().to_rgb8();
	assert_eq!(thumbnail.width(), expected);
	assert_eq!(thumbnail.height(), expected);
}

const FORMATS_MOUNT_NAME: &str = "formats";

/// Sets up a service whose collection includes songs in a variety of formats.
/// Returns `None` when transcoding is not available on the machine running the tests.
async fn make_transcoding_service(test_name: &str) -> Option<ServiceType> {
	let mut service = ServiceType::new(test_name).await;
	service.complete_initial_setup().await;
	service.login_admin().await;

	let mounts = vec![
		dto::MountDir {
			name: TEST_MOUNT_NAME.into(),
			source: TEST_MOUNT_SOURCE.into(),
		},
		dto::MountDir {
			name: FORMATS_MOUNT_NAME.into(),
			source: "test-data/formats".into(),
		},
	];
	let response = service.fetch(&protocol::put_mount_dirs(mounts)).await;
	assert_eq!(response.status(), StatusCode::OK);
	service.index().await;
	service.login().await;

	let request = protocol::transcoding();
	let response = service
		.fetch_json::<_, dto::TranscodingSupport>(&request)
		.await;
	let support = response.body();
	assert_eq!(support.available, !support.formats.is_empty());
	if !support.available {
		let path = PathBuf::from_iter([FORMATS_MOUNT_NAME, "sample.flac"]);
		let request = protocol::audio_transcoded(&path, "mp3", 64);
		let response = service.fetch(&request).await;
		assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
		eprintln!("Skipping test because ffmpeg is not installed");
		return None;
	}

	Some(service)
}

#[tokio::test]
async fn transcoding_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::transcoding();
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn audio_can_be_transcoded() {
	let Some(mut service) = make_transcoding_service(&test_name!()).await else {
		return;
	};

	let path = PathBuf::from_iter([FORMATS_MOUNT_NAME, "sample.flac"]);
	for (format, content_type, magic) in [
		("mp3", "audio/mpeg", &b"ID3"[..]),
		("opus", "audio/ogg", &b"OggS"[..]),
		("aac", "audio/aac", &[0xFF][..]),
	] {
		let request = protocol::audio_transcoded(&path, format, 64);
		let response = service.fetch_bytes(&request).await;
		assert_eq!(response.status(), StatusCode::OK);
		assert_eq!(
			response.headers().get(header::CONTENT_TYPE).unwrap(),
			content_type
		);
		let transcoded = response.into_body();
		assert!(transcoded.starts_with(magic), "{format}");

		// Seeking is possible once the transcode has completed
		let mut request = protocol::audio_transcoded(&path, format, 64);
		request
			.headers_mut()
			.insert(header::RANGE, "bytes=100-199".parse().unwrap());
		let response = service.fetch_bytes(&request).await;
		assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
		assert_eq!(
			response.headers().get(header::CONTENT_TYPE).unwrap(),
			content_type
		);
		assert_eq!(response.body(), &transcoded[100..200], "{format}");
	}
}

#[tokio::test]
async fn audio_can_be_seeked_before_being_transcoded() {
	let Some(mut service) = make_transcoding_service(&test_name!()).await else {
		return;
	};

	let path = PathBuf::from_iter([FORMATS_MOUNT_NAME, "sample.aif"]);
	let mut request = protocol::audio_transcoded(&path, "mp3", 96);
	request
		.headers_mut()
		.insert(header::RANGE, "bytes=100-199".parse().unwrap());
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
	assert_eq!(response.body().len(), 100);
}

#[tokio::test]
async fn audio_is_not_transcoded_needlessly() {
	let Some(mut service) = make_transcoding_service(&test_name!()).await else {
		return;
	};

	let path = PathBuf::from_iter([TEST_MOUNT_NAME, "Khemmis", "Hunted", "02 - Candlelight.mp3"]);
	let original = service.fetch_bytes(&protocol::audio(&path)).await;

	// This song is already well below the requested bitrate
	let request = protocol::audio_transcoded(&path, "mp3", 320);
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(response.body(), original.body());

	let request = protocol::audio_transcoded(&path, "not-a-format", 320);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
