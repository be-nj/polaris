use std::path::PathBuf;

use http::StatusCode;

use crate::{
	server::{
		dto,
		test::{
			add_trailing_slash,
			constants::*,
			protocol::{self, V8},
			ServiceType, TestService,
		},
	},
	test_name,
};

#[tokio::test]
async fn random_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::random::<V8>();
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn random_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::random::<V8>();
	let response = service
		.fetch_json::<_, Vec<dto::AlbumHeader>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	let entries = response.body();
	assert_eq!(entries.len(), 3);
}

#[tokio::test]
async fn random_with_trailing_slash() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let mut request = protocol::random::<V8>();
	add_trailing_slash(&mut request);
	let response = service
		.fetch_json::<_, Vec<dto::AlbumHeader>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	let entries = response.body();
	assert_eq!(entries.len(), 3);
}

#[tokio::test]
async fn recent_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::recent::<V8>();
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recent_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::recent::<V8>();
	let response = service
		.fetch_json::<_, Vec<dto::AlbumHeader>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	let entries = response.body();
	assert_eq!(entries.len(), 3);
}

#[tokio::test]
async fn recent_with_trailing_slash() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let mut request = protocol::recent::<V8>();
	add_trailing_slash(&mut request);
	let response = service
		.fetch_json::<_, Vec<dto::AlbumHeader>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	let entries = response.body();
	assert_eq!(entries.len(), 3);
}

#[tokio::test]
async fn genres_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::genres::<V8>();
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn genres_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::genres::<V8>();
	let response = service
		.fetch_json::<_, Vec<dto::GenreHeader>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	let entries = response.body();
	assert_eq!(entries.len(), 4);
}

#[tokio::test]
async fn genre_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::genre::<V8>("Metal");
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn genre_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::genre::<V8>("Metal");
	let response = service.fetch_json::<_, dto::Genre>(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	let genre = response.body();
	assert_eq!(genre.header.name, "Metal");
}

#[tokio::test]
async fn genre_albums_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::genre_albums::<V8>("Metal");
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn genre_albums_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::genre_albums::<V8>("Metal");
	let response = service
		.fetch_json::<_, Vec<dto::AlbumHeader>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	let entries = response.body();
	assert_eq!(entries.len(), 1);
}

#[tokio::test]
async fn genre_artists_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::genre_artists::<V8>("Metal");
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn artists_have_pictures() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::artists::<V8>();
	let response = service
		.fetch_json::<_, Vec<dto::ArtistHeader>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	let artists = response.body();
	let find_artist = |name: &str| artists.iter().find(|a| a.name == name).unwrap();

	// The picture found in the directory holding the albums of the artist
	let picture: PathBuf = [TEST_MOUNT_NAME, "Khemmis", "artist.png"].iter().collect();
	assert_eq!(find_artist("Khemmis").artwork.as_ref(), Some(&picture));
	assert_eq!(find_artist("Tobokegao").artwork, None);

	let request = protocol::artist::<V8>("Khemmis");
	let response = service.fetch_json::<_, dto::Artist>(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(response.body().header.artwork.as_ref(), Some(&picture));

	let request = protocol::thumbnail(&picture, None, None);
	let response = service.fetch_bytes(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert!(response.body().starts_with(&[0xFF, 0xD8]));
}

#[tokio::test]
async fn genre_artists_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::genre_artists::<V8>("Metal");
	let response = service
		.fetch_json::<_, Vec<dto::ArtistHeader>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	let entries = response.body();
	assert_eq!(entries.len(), 1);
}

#[tokio::test]
async fn genre_songs_requires_auth() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::genre_songs::<V8>("Metal");
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn genre_songs_golden_path() {
	let mut service = ServiceType::new(&test_name!()).await;
	service.complete_initial_setup().await;
	service.login_admin().await;
	service.index().await;
	service.login().await;

	let request = protocol::genre_songs::<V8>("Metal");
	let response = service.fetch_json::<_, dto::SongList>(&request).await;
	assert_eq!(response.status(), StatusCode::OK);
	let song_list = response.body();
	assert_eq!(song_list.paths.len(), 5);
}
