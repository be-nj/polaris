use std::path::PathBuf;

use crate::app::Error;

use super::storage;

pub const DEFAULT_USERNAME_CLAIM: &str = "preferred_username";
pub const DEFAULT_ROLE_CLAIM: &str = "groups";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TokenAuthMethod {
	/// Client credentials are sent in the `Authorization` header
	#[default]
	ClientSecretBasic,
	/// Client credentials are sent in the request body
	ClientSecretPost,
}

/// An OpenID Connect identity provider users can sign in with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OidcProvider {
	pub name: String,
	pub display_name: Option<String>,
	pub issuer: String,
	pub client_id: String,
	pub client_secret: Option<String>,
	pub scopes: Vec<String>,
	pub username_claim: Option<String>,
	pub role_claim: Option<String>,
	pub allowed_roles: Vec<String>,
	pub admin_roles: Vec<String>,
	pub token_auth_method: TokenAuthMethod,
	pub ca_file: Option<PathBuf>,
}

impl OidcProvider {
	pub fn username_claim(&self) -> &str {
		self.username_claim
			.as_deref()
			.unwrap_or(DEFAULT_USERNAME_CLAIM)
	}

	pub fn role_claim(&self) -> &str {
		self.role_claim.as_deref().unwrap_or(DEFAULT_ROLE_CLAIM)
	}
}

impl TryFrom<storage::OidcProvider> for OidcProvider {
	type Error = Error;

	fn try_from(provider: storage::OidcProvider) -> Result<Self, Self::Error> {
		let invalid = |reason: &str| {
			Err(Error::OidcProviderInvalid(
				provider.name.clone(),
				reason.to_owned(),
			))
		};

		// Provider names are part of the URLs users are redirected to
		let is_valid_name = !provider.name.is_empty()
			&& provider
				.name
				.chars()
				.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
		if !is_valid_name {
			return invalid("`name` may only contain letters, digits, `-` and `_`");
		}

		let issuer = provider.issuer.trim();
		let is_valid_issuer = match issuer.parse::<http::Uri>() {
			Ok(uri) => {
				matches!(uri.scheme_str(), Some("http") | Some("https")) && uri.host().is_some()
			}
			Err(_) => false,
		};
		if !is_valid_issuer {
			return invalid("`issuer` must be an http or https URL");
		}

		if provider.client_id.trim().is_empty() {
			return invalid("`client_id` is required");
		}

		let token_auth_method = match provider.token_auth_method.as_deref() {
			None | Some("client_secret_basic") => TokenAuthMethod::ClientSecretBasic,
			Some("client_secret_post") => TokenAuthMethod::ClientSecretPost,
			Some(_) => {
				return invalid(
					"`token_auth_method` must be `client_secret_basic` or `client_secret_post`",
				)
			}
		};

		Ok(Self {
			issuer: issuer.to_owned(),
			client_id: provider.client_id.trim().to_owned(),
			token_auth_method,
			name: provider.name,
			display_name: provider.display_name,
			client_secret: provider.client_secret,
			scopes: provider.scopes,
			username_claim: provider.username_claim,
			role_claim: provider.role_claim,
			allowed_roles: provider.allowed_roles,
			admin_roles: provider.admin_roles,
			ca_file: provider.ca_file,
		})
	}
}

impl From<OidcProvider> for storage::OidcProvider {
	fn from(provider: OidcProvider) -> Self {
		Self {
			name: provider.name,
			display_name: provider.display_name,
			issuer: provider.issuer,
			client_id: provider.client_id,
			client_secret: provider.client_secret,
			scopes: provider.scopes,
			username_claim: provider.username_claim,
			role_claim: provider.role_claim,
			allowed_roles: provider.allowed_roles,
			admin_roles: provider.admin_roles,
			token_auth_method: match provider.token_auth_method {
				TokenAuthMethod::ClientSecretBasic => None,
				TokenAuthMethod::ClientSecretPost => Some("client_secret_post".to_owned()),
			},
			ca_file: provider.ca_file,
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn valid_provider() -> storage::OidcProvider {
		storage::OidcProvider {
			name: "keycloak".to_owned(),
			issuer: "https://sso.example.com/realms/home".to_owned(),
			client_id: "polaris".to_owned(),
			..Default::default()
		}
	}

	#[test]
	fn accepts_valid_providers() {
		let provider = OidcProvider::try_from(valid_provider()).unwrap();
		assert_eq!(provider.username_claim(), "preferred_username");
		assert_eq!(provider.role_claim(), "groups");
		assert_eq!(
			provider.token_auth_method,
			TokenAuthMethod::ClientSecretBasic
		);
		assert_eq!(storage::OidcProvider::from(provider), valid_provider());
	}

	#[test]
	fn rejects_invalid_providers() {
		let rejects = |edit: fn(&mut storage::OidcProvider)| {
			let mut provider = valid_provider();
			edit(&mut provider);
			assert!(matches!(
				OidcProvider::try_from(provider),
				Err(Error::OidcProviderInvalid(_, _))
			));
		};
		rejects(|p| p.name = "".to_owned());
		rejects(|p| p.name = "my provider".to_owned());
		rejects(|p| p.name = "a/b".to_owned());
		rejects(|p| p.issuer = "sso.example.com".to_owned());
		rejects(|p| p.issuer = "ftp://sso.example.com".to_owned());
		rejects(|p| p.client_id = " ".to_owned());
		rejects(|p| p.token_auth_method = Some("private_key_jwt".to_owned()));
	}
}
