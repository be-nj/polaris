use std::cmp;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImage, GenericImageView, ImageBuffer};
use tokio::task::spawn_blocking;

use crate::app::Error;
use crate::utils::{get_audio_format, AudioFormat};

#[derive(Clone, Debug, Hash)]
pub struct Options {
	pub max_dimension: Option<u32>,
	pub resize_if_almost_square: bool,
	pub pad_to_square: bool,
}

impl Default for Options {
	fn default() -> Self {
		Self {
			max_dimension: Some(400),
			resize_if_almost_square: true,
			pad_to_square: true,
		}
	}
}

#[derive(Clone)]
pub struct Manager {
	thumbnails_dir_path: PathBuf,
}

impl Manager {
	pub fn new(thumbnails_dir_path: PathBuf) -> Self {
		Self {
			thumbnails_dir_path,
		}
	}

	pub async fn get_thumbnail(
		&self,
		image_path: &Path,
		options: &Options,
	) -> Result<PathBuf, Error> {
		match self.read_from_cache(image_path, options).await {
			Some(path) => Ok(path),
			None => self.read_from_source(image_path, options).await,
		}
	}

	fn get_thumbnail_path(&self, image_path: &Path, options: &Options) -> PathBuf {
		let hash = Manager::hash(image_path, options);
		let mut thumbnail_path = self.thumbnails_dir_path.clone();
		thumbnail_path.push(format!("{}.jpg", hash));
		thumbnail_path
	}

	async fn read_from_cache(&self, image_path: &Path, options: &Options) -> Option<PathBuf> {
		let path = self.get_thumbnail_path(image_path, options);
		match tokio::fs::try_exists(&path).await.ok() {
			Some(true) => Some(path),
			_ => None,
		}
	}

	async fn read_from_source(
		&self,
		image_path: &Path,
		options: &Options,
	) -> Result<PathBuf, Error> {
		let thumbnail = spawn_blocking({
			let image_path = image_path.to_owned();
			let options = options.clone();
			move || generate_thumbnail(&image_path, &options)
		})
		.await??;

		tokio::fs::create_dir_all(&self.thumbnails_dir_path)
			.await
			.map_err(|e| Error::Io(self.thumbnails_dir_path.clone(), e))?;

		let path = self.get_thumbnail_path(image_path, options);
		let out_file = tokio::fs::File::create(&path)
			.await
			.map_err(|e| Error::Io(self.thumbnails_dir_path.clone(), e))?;

		spawn_blocking({
			let mut out_file = out_file.into_std().await;
			move || {
				let quality = 80;
				thumbnail.write_with_encoder(JpegEncoder::new_with_quality(&mut out_file, quality))
			}
		})
		.await?
		.map_err(|e| Error::Image(image_path.to_owned(), e))?;

		Ok(path)
	}

	fn hash(path: &Path, options: &Options) -> u64 {
		let mut hasher = DefaultHasher::new();
		path.hash(&mut hasher);
		options.hash(&mut hasher);
		hasher.finish()
	}
}

fn generate_thumbnail(image_path: &Path, options: &Options) -> Result<DynamicImage, Error> {
	let source_image = DynamicImage::ImageRgb8(read(image_path)?.into_rgb8());
	let (source_width, source_height) = source_image.dimensions();
	let largest_dimension = cmp::max(source_width, source_height);
	let out_dimension = cmp::min(
		options.max_dimension.unwrap_or(largest_dimension),
		largest_dimension,
	);

	let source_aspect_ratio: f32 = source_width as f32 / source_height as f32;
	let is_almost_square = source_aspect_ratio > 0.8 && source_aspect_ratio < 1.2;

	let mut final_image;
	if is_almost_square && options.resize_if_almost_square {
		final_image = source_image.thumbnail_exact(out_dimension, out_dimension);
	} else if options.pad_to_square {
		let scaled_image = source_image.thumbnail(out_dimension, out_dimension);
		let (scaled_width, scaled_height) = scaled_image.dimensions();
		let background = image::Rgb([255, 255_u8, 255_u8]);
		final_image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
			out_dimension,
			out_dimension,
			background,
		));
		final_image
			.copy_from(
				&scaled_image,
				(out_dimension - scaled_width) / 2,
				(out_dimension - scaled_height) / 2,
			)
			.map_err(|e| Error::Image(image_path.to_owned(), e))?;
	} else {
		final_image = source_image.thumbnail(out_dimension, out_dimension);
	}

	Ok(final_image)
}

fn read(image_path: &Path) -> Result<DynamicImage, Error> {
	match get_audio_format(image_path) {
		Some(AudioFormat::AIFF) => read_aiff(image_path),
		Some(AudioFormat::FLAC) => read_flac(image_path),
		Some(AudioFormat::MP3) => read_mp3(image_path),
		Some(AudioFormat::OGG) => read_vorbis(image_path),
		Some(AudioFormat::OPUS) => read_opus(image_path),
		Some(AudioFormat::WAVE) => read_wave(image_path),
		Some(AudioFormat::APE) | Some(AudioFormat::MPC) => read_ape(image_path),
		Some(AudioFormat::MP4) | Some(AudioFormat::M4B) => read_mp4(image_path),
		None => image::open(image_path).map_err(|e| Error::Image(image_path.to_owned(), e)),
	}
}

fn read_ape(_: &Path) -> Result<DynamicImage, Error> {
	Err(Error::UnsupportedFormat("ape"))
}

fn read_flac(path: &Path) -> Result<DynamicImage, Error> {
	let tag =
		metaflac::Tag::read_from_path(path).map_err(|e| Error::Metaflac(path.to_owned(), e))?;
	if let Some(p) = tag.pictures().next() {
		return image::load_from_memory(&p.data).map_err(|e| Error::Image(path.to_owned(), e));
	}
	Err(Error::EmbeddedArtworkNotFound(path.to_owned()))
}

fn read_mp3(path: &Path) -> Result<DynamicImage, Error> {
	let tag = id3::Tag::read_from_path(path).map_err(|e| Error::Id3(path.to_owned(), e))?;
	read_id3(path, &tag)
}

fn read_aiff(path: &Path) -> Result<DynamicImage, Error> {
	let tag = id3::Tag::read_from_path(path).map_err(|e| Error::Id3(path.to_owned(), e))?;
	read_id3(path, &tag)
}

fn read_wave(path: &Path) -> Result<DynamicImage, Error> {
	let tag = id3::Tag::read_from_path(path).map_err(|e| Error::Id3(path.to_owned(), e))?;
	read_id3(path, &tag)
}

fn read_id3(path: &Path, tag: &id3::Tag) -> Result<DynamicImage, Error> {
	tag.pictures()
		.next()
		.ok_or_else(|| Error::EmbeddedArtworkNotFound(path.to_owned()))
		.and_then(|d| {
			image::load_from_memory(&d.data).map_err(|e| Error::Image(path.to_owned(), e))
		})
}

fn read_mp4(path: &Path) -> Result<DynamicImage, Error> {
	let cfg = mp4ameta::ReadConfig {
		read_meta_items: true,
		read_image_data: true,
		..mp4ameta::ReadConfig::NONE
	};
	let tag = mp4ameta::Tag::read_with_path(path, &cfg)
		.map_err(|e| Error::Mp4aMeta(path.to_owned(), e))?;
	tag.artwork()
		.ok_or_else(|| Error::EmbeddedArtworkNotFound(path.to_owned()))
		.and_then(|d| image::load_from_memory(d.data).map_err(|e| Error::Image(path.to_owned(), e)))
}

fn read_vorbis(path: &Path) -> Result<DynamicImage, Error> {
	let file = std::fs::File::open(path).map_err(|e| Error::Io(path.to_owned(), e))?;
	let source = lewton::inside_ogg::OggStreamReader::new(file)?;
	read_vorbis_comments(path, &source.comment_hdr.comment_list)
}

fn read_opus(path: &Path) -> Result<DynamicImage, Error> {
	let headers = opus_headers::parse_from_path(path)?;
	read_vorbis_comments(path, &headers.comments.user_comments)
}

/// Ogg files (Vorbis and Opus) store pictures in their comments. The standard way is a
/// `METADATA_BLOCK_PICTURE` comment, which holds the same structure as picture blocks of FLAC
/// files, encoded as base64. `COVERART` is an older convention holding only the image.
fn read_vorbis_comments(path: &Path, comments: &[(String, String)]) -> Result<DynamicImage, Error> {
	fn find_comment<'a>(
		comments: &'a [(String, String)],
		name: &'a str,
	) -> impl Iterator<Item = Vec<u8>> + 'a {
		comments
			.iter()
			.filter(move |(key, _)| key.eq_ignore_ascii_case(name))
			.filter_map(|(_, value)| BASE64.decode(value.trim()).ok())
	}

	let pictures = find_comment(comments, "METADATA_BLOCK_PICTURE")
		.filter_map(|block| read_flac_picture_block(&block).map(<[u8]>::to_vec));
	let legacy_pictures = find_comment(comments, "COVERART");

	pictures
		.chain(legacy_pictures)
		.find_map(|data| image::load_from_memory(&data).ok())
		.ok_or_else(|| Error::EmbeddedArtworkNotFound(path.to_owned()))
}

/// Extracts the image from a FLAC picture block: picture type, MIME type, description,
/// dimensions, color depth, number of colors, then the image itself. Numbers are 32-bit
/// big-endian, and texts and image are preceded by their length.
fn read_flac_picture_block(block: &[u8]) -> Option<&[u8]> {
	fn read_u32(block: &[u8], offset: usize) -> Option<usize> {
		let bytes = block.get(offset..offset.checked_add(4)?)?;
		Some(u32::from_be_bytes(bytes.try_into().ok()?) as usize)
	}

	let mime_length = read_u32(block, 4)?;
	let description_offset = 8usize.checked_add(mime_length)?;
	let description_length = read_u32(block, description_offset)?;
	// Skips the description, and four numbers describing the image
	let data_offset = description_offset
		.checked_add(4)?
		.checked_add(description_length)?
		.checked_add(16)?;
	let data_length = read_u32(block, data_offset)?;
	let data_start = data_offset.checked_add(4)?;
	block.get(data_start..data_start.checked_add(data_length)?)
}

#[cfg(test)]
mod test {

	use super::*;

	#[test]
	fn can_read_artwork_data() {
		let ext_img = image::open("test-data/artwork/Folder.png")
			.unwrap()
			.to_rgb8();
		let embedded_img = image::open("test-data/artwork/Embedded.png")
			.unwrap()
			.to_rgb8();

		let folder_img = read(Path::new("test-data/artwork/Folder.png"))
			.unwrap()
			.to_rgb8();
		assert_eq!(folder_img, ext_img);

		let aiff_img = read(Path::new("test-data/artwork/sample.aif"))
			.unwrap()
			.to_rgb8();
		assert_eq!(aiff_img, embedded_img);

		let ape_img = read(Path::new("test-data/artwork/sample.ape"))
			.map(|d| d.to_rgb8())
			.ok();
		assert_eq!(ape_img, None);

		let flac_img = read(Path::new("test-data/artwork/sample.flac"))
			.unwrap()
			.to_rgb8();
		assert_eq!(flac_img, embedded_img);

		let mp3_img = read(Path::new("test-data/artwork/sample.mp3"))
			.unwrap()
			.to_rgb8();
		assert_eq!(mp3_img, embedded_img);

		let m4a_img = read(Path::new("test-data/artwork/sample.m4a"))
			.unwrap()
			.to_rgb8();
		assert_eq!(m4a_img, embedded_img);

		let ogg_img = read(Path::new("test-data/artwork/sample.ogg"))
			.unwrap()
			.to_rgb8();
		assert_eq!(ogg_img, embedded_img);

		let opus_img = read(Path::new("test-data/artwork/sample.opus"))
			.unwrap()
			.to_rgb8();
		assert_eq!(opus_img, embedded_img);

		let wave_img = read(Path::new("test-data/artwork/sample.wav"))
			.unwrap()
			.to_rgb8();
		assert_eq!(wave_img, embedded_img);
	}

	#[test]
	fn can_read_large_artwork_in_ogg_files() {
		// Album art of real-world size spans several Ogg pages (about 64 kB each)
		for path in [
			"test-data/artwork/large.ogg",
			"test-data/artwork/large.opus",
		] {
			let path = Path::new(path);
			let metadata = crate::app::formats::read_metadata(path).unwrap();
			assert!(metadata.has_artwork, "{path:?}");
			assert_eq!(metadata.title.as_deref(), Some("TEST TITLE"), "{path:?}");
			let image = read(path).unwrap();
			assert_eq!((image.width(), image.height()), (600, 600), "{path:?}");
		}
	}

	#[test]
	fn ogg_files_without_artwork_have_no_thumbnail() {
		for path in [
			"test-data/formats/sample.ogg",
			"test-data/formats/sample.opus",
		] {
			let result = read(Path::new(path));
			assert!(
				matches!(result, Err(Error::EmbeddedArtworkNotFound(_))),
				"{path}"
			);
		}
	}

	fn make_picture_block(mime: &str, description: &str, data: &[u8]) -> Vec<u8> {
		let mut block = vec![];
		block.extend(3u32.to_be_bytes());
		block.extend((mime.len() as u32).to_be_bytes());
		block.extend(mime.as_bytes());
		block.extend((description.len() as u32).to_be_bytes());
		block.extend(description.as_bytes());
		for number in [4u32, 4, 24, 0] {
			block.extend(number.to_be_bytes());
		}
		block.extend((data.len() as u32).to_be_bytes());
		block.extend(data);
		block
	}

	#[test]
	fn reads_flac_picture_blocks() {
		let block = make_picture_block("image/png", "", b"image");
		assert_eq!(read_flac_picture_block(&block), Some(b"image".as_slice()));

		let block = make_picture_block("image/jpeg", "Front cover ✨", b"image");
		assert_eq!(read_flac_picture_block(&block), Some(b"image".as_slice()));

		let block = make_picture_block("", "", b"");
		assert_eq!(read_flac_picture_block(&block), Some(b"".as_slice()));
	}

	#[test]
	fn rejects_malformed_flac_picture_blocks() {
		let block = make_picture_block("image/png", "cover", b"image");
		// Truncated anywhere
		for length in 0..block.len() {
			assert_eq!(read_flac_picture_block(&block[..length]), None, "{length}");
		}

		// Lengths pointing far outside of the block
		for offset in [4, 8 + "image/png".len(), block.len() - b"image".len() - 4] {
			let mut block = block.clone();
			block[offset..offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
			assert_eq!(read_flac_picture_block(&block), None, "{offset}");
		}
	}

	#[test]
	fn reads_pictures_from_vorbis_comments() {
		let path = Path::new("song.ogg");
		let image = std::fs::read("test-data/artwork/Embedded.png").unwrap();
		let expected = image::load_from_memory(&image).unwrap().to_rgb8();
		let comment = |key: &str, value: &[u8]| (key.to_owned(), BASE64.encode(value));
		let read = |comments: &[(String, String)]| {
			read_vorbis_comments(path, comments)
				.map(|i| i.to_rgb8())
				.ok()
		};

		let block = make_picture_block("image/png", "", &image);
		assert_eq!(
			read(&[comment("metadata_block_picture", &block)]),
			Some(expected.clone())
		);
		// Older convention, where the comment only holds the image
		assert_eq!(read(&[comment("COVERART", &image)]), Some(expected.clone()));

		// Pictures which cannot be read are skipped
		let broken_block = make_picture_block("image/png", "", b"not an image");
		let comments = [
			("TITLE".to_owned(), "Song".to_owned()),
			(
				"METADATA_BLOCK_PICTURE".to_owned(),
				"not base64!".to_owned(),
			),
			comment("METADATA_BLOCK_PICTURE", &broken_block),
			comment("METADATA_BLOCK_PICTURE", &block),
		];
		assert_eq!(read(&comments), Some(expected));

		assert_eq!(read(&[]), None);
		assert_eq!(
			read(&[comment("METADATA_BLOCK_PICTURE", &broken_block)]),
			None
		);
	}
}
