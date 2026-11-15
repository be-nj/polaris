use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct User {
	pub name: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub admin: Option<bool>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub initial_password: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub hashed_password: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MountDir {
	pub source: PathBuf,
	pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Config {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub album_art_pattern: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub scan_exclude_pattern: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub transcode_cache_size_mb: Option<u64>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub mount_dirs: Vec<MountDir>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ddns_update_url: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub users: Vec<User>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub public_url: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub oidc_providers: Vec<OidcProvider>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OidcProvider {
	pub name: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub display_name: Option<String>,
	pub issuer: String,
	pub client_id: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub client_secret: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub scopes: Vec<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub username_claim: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub role_claim: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub allowed_roles: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub admin_roles: Vec<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub token_auth_method: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ca_file: Option<PathBuf>,
}
