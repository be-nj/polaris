use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::server::error::APIError;

impl IntoResponse for APIError {
	fn into_response(self) -> Response {
		let message = self.to_string();
		let status_code = match self {
			APIError::MissingAPIVersionHeader => StatusCode::BAD_REQUEST,
			APIError::InvalidAPIVersionHeader => StatusCode::BAD_REQUEST,
			APIError::APIVersionHeaderParse => StatusCode::BAD_REQUEST,
			APIError::UnsupportedAPIVersion => StatusCode::NOT_ACCEPTABLE,
			APIError::AuthorizationTokenEncoding => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::AdminPermissionRequired => StatusCode::FORBIDDEN,
			APIError::AudioFileIO => StatusCode::NOT_FOUND,
			APIError::AuthenticationRequired => StatusCode::UNAUTHORIZED,
			APIError::BrancaTokenEncoding => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::DDNSUpdateRejected(s) => {
				StatusCode::from_u16(s).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
			}
			APIError::DDNSUpdate(_) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::NativeDatabase(_) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::DeletingOwnAccount => StatusCode::CONFLICT,
			APIError::DirectoryNotFound(_) => StatusCode::NOT_FOUND,
			APIError::DuplicateUsername => StatusCode::CONFLICT,
			APIError::ArtistNotFound => StatusCode::NOT_FOUND,
			APIError::ArtistImageNotFound => StatusCode::NOT_FOUND,
			APIError::AlbumNotFound => StatusCode::NOT_FOUND,
			APIError::GenreNotFound => StatusCode::NOT_FOUND,
			APIError::SongNotFound => StatusCode::NOT_FOUND,
			APIError::EmbeddedArtworkNotFound => StatusCode::NOT_FOUND,
			APIError::EmptyPassword => StatusCode::BAD_REQUEST,
			APIError::EmptyUsername => StatusCode::BAD_REQUEST,
			APIError::IncorrectCredentials => StatusCode::UNAUTHORIZED,
			APIError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::InvalidAlbumArtPattern => StatusCode::BAD_REQUEST,
			APIError::InvalidScanExcludePattern => StatusCode::BAD_REQUEST,
			APIError::InvalidDDNSURL => StatusCode::BAD_REQUEST,
			APIError::Io(_, _) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::MultipartError(_, s) => {
				StatusCode::from_u16(s).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
			}
			APIError::OwnAdminPrivilegeRemoval => StatusCode::CONFLICT,
			APIError::PasswordHashing => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::PlaylistNotFound => StatusCode::NOT_FOUND,
			APIError::PlaylistExportError => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::PlaylistImportError => StatusCode::BAD_REQUEST,
			APIError::InvalidPlaylistTextEncoding(_) => StatusCode::BAD_REQUEST,
			APIError::SearchQueryParse => StatusCode::BAD_REQUEST,
			APIError::ThumbnailFlacDecoding(_, _) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::ThumbnailFileIO => StatusCode::NOT_FOUND,
			APIError::OidcProviderNotFound => StatusCode::NOT_FOUND,
			APIError::OidcProviderFailure => StatusCode::BAD_GATEWAY,
			APIError::OidcLoginExpired => StatusCode::BAD_REQUEST,
			APIError::OidcInvalidCodeChallenge => StatusCode::BAD_REQUEST,
			APIError::OidcAccessDenied => StatusCode::FORBIDDEN,
			APIError::TranscodingUnavailable => StatusCode::NOT_IMPLEMENTED,
			APIError::TranscodingFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::ThumbnailId3Decoding(_, _) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::ThumbnailImageDecoding(_, _) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::ThumbnailMp4Decoding(_, _) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::UnsupportedThumbnailFormat(_) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::AudioEmpty(_) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::AudioDecoding(_) => StatusCode::INTERNAL_SERVER_ERROR,
			APIError::UserNotFound => StatusCode::NOT_FOUND,
			APIError::RealPathNotFound(_) => StatusCode::NOT_FOUND,
			APIError::VirtualPathNotFound(_) => StatusCode::NOT_FOUND,
		};

		(status_code, message).into_response()
	}
}
