//! Single sign-on through OpenID Connect identity providers (Authelia, authentik, Keycloak, etc.)
//!
//! This implements the authorization code flow with PKCE. The user's browser is sent to the
//! identity provider, which sends it back with a code that is exchanged for the identity
//! of the user. Users are created on first sign-in and are matched by username afterwards.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{
	engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
	Engine,
};
use log::{info, warn};
use rand::{rngs::OsRng, RngCore};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::task::spawn_blocking;

use crate::app::{auth, config, Error};

const LOGIN_LIFETIME: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING_LOGINS: usize = 10_000;
const DISCOVERY_LIFETIME: Duration = Duration::from_secs(60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const CLOCK_SKEW_SECONDS: u64 = 60;

/// Subset of the OpenID Provider Metadata document this implementation relies on
#[derive(Clone, Debug, Deserialize)]
struct Discovery {
	issuer: String,
	authorization_endpoint: String,
	token_endpoint: String,
	userinfo_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
	id_token: Option<String>,
	access_token: Option<String>,
}

/// A sign-in which was started but has not come back from the identity provider yet
struct PendingLogin {
	provider_name: String,
	redirect_uri: String,
	nonce: String,
	code_verifier: String,
	started: Instant,
}

pub struct LoginStart {
	/// Where the browser of the user must be sent to sign in
	pub authorization_url: String,
	/// Identifies this sign-in attempt. Must be presented again to complete it.
	pub state: String,
}

#[derive(Debug)]
pub struct Login {
	pub username: String,
	pub token: auth::Token,
	pub is_admin: bool,
}

#[derive(Clone)]
pub struct Manager {
	config_manager: config::Manager,
	pending_logins: Arc<Mutex<HashMap<String, PendingLogin>>>,
	discovery_cache: Arc<Mutex<HashMap<String, (Instant, Discovery)>>>,
}

impl Manager {
	pub fn new(config_manager: config::Manager) -> Self {
		Self {
			config_manager,
			pending_logins: Arc::default(),
			discovery_cache: Arc::default(),
		}
	}

	async fn get_provider(&self, name: &str) -> Result<config::OidcProvider, Error> {
		self.config_manager
			.get_oidc_providers()
			.await
			.into_iter()
			.find(|p| p.name == name)
			.ok_or(Error::OidcProviderNotFound)
	}

	async fn discover(&self, provider: &config::OidcProvider) -> Result<Discovery, Error> {
		{
			let cache = self.discovery_cache.lock().unwrap();
			if let Some((time, discovery)) = cache.get(&provider.issuer) {
				if time.elapsed() < DISCOVERY_LIFETIME {
					return Ok(discovery.clone());
				}
			}
		}

		let discovery = spawn_blocking({
			let provider = provider.clone();
			move || fetch_discovery(&provider)
		})
		.await??;

		self.discovery_cache
			.lock()
			.unwrap()
			.insert(provider.issuer.clone(), (Instant::now(), discovery.clone()));
		Ok(discovery)
	}

	/// Starts a sign-in with the specified provider. The identity provider will send the
	/// user back to `redirect_uri` after they have signed in.
	pub async fn begin_login(
		&self,
		provider_name: &str,
		redirect_uri: String,
	) -> Result<LoginStart, Error> {
		let provider = self.get_provider(provider_name).await?;
		let discovery = self.discover(&provider).await?;

		let state = random_token();
		let nonce = random_token();
		let code_verifier = random_token();
		let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));

		let mut scopes = vec!["openid", "profile"];
		for scope in &provider.scopes {
			if !scopes.contains(&scope.as_str()) {
				scopes.push(scope);
			}
		}

		let query = form_urlencoded::Serializer::new(String::new())
			.append_pair("response_type", "code")
			.append_pair("client_id", &provider.client_id)
			.append_pair("redirect_uri", &redirect_uri)
			.append_pair("scope", &scopes.join(" "))
			.append_pair("state", &state)
			.append_pair("nonce", &nonce)
			.append_pair("code_challenge", &code_challenge)
			.append_pair("code_challenge_method", "S256")
			.finish();
		let separator = match discovery.authorization_endpoint.contains('?') {
			true => '&',
			false => '?',
		};
		let authorization_url = format!("{}{separator}{query}", discovery.authorization_endpoint);

		{
			let mut pending_logins = self.pending_logins.lock().unwrap();
			pending_logins.retain(|_, login| login.started.elapsed() < LOGIN_LIFETIME);
			if pending_logins.len() >= MAX_PENDING_LOGINS {
				return Err(Error::OidcProviderUnreachable(
					"Too many sign-in attempts are in progress".to_owned(),
				));
			}
			pending_logins.insert(
				state.clone(),
				PendingLogin {
					provider_name: provider.name.clone(),
					redirect_uri,
					nonce,
					code_verifier,
					started: Instant::now(),
				},
			);
		}

		Ok(LoginStart {
			authorization_url,
			state,
		})
	}

	/// Completes a sign-in using the code sent back by the identity provider.
	pub async fn complete_login(
		&self,
		provider_name: &str,
		state: &str,
		code: &str,
	) -> Result<Login, Error> {
		// Each sign-in attempt can only be completed once
		let pending_login = self.pending_logins.lock().unwrap().remove(state);
		let pending_login = pending_login
			.filter(|l| l.provider_name == provider_name)
			.filter(|l| l.started.elapsed() < LOGIN_LIFETIME)
			.ok_or(Error::OidcLoginExpired)?;

		let provider = self.get_provider(provider_name).await?;
		let discovery = self.discover(&provider).await?;

		let claims = spawn_blocking({
			let provider = provider.clone();
			let code = code.to_owned();
			move || fetch_claims(&provider, &discovery, &pending_login, &code)
		})
		.await??;

		let username = claims
			.get(provider.username_claim())
			.and_then(Value::as_str)
			.map(str::trim)
			.filter(|u| !u.is_empty())
			.ok_or_else(|| {
				Error::OidcInvalidResponse(format!(
					"the `{}` claim is missing. Available claims: {}",
					provider.username_claim(),
					claim_names(&claims)
				))
			})?
			.to_owned();

		let roles = read_roles(&claims, provider.role_claim());
		let has_any_role = |expected: &[String]| expected.iter().any(|r| roles.contains(r));

		if !provider.allowed_roles.is_empty() && !has_any_role(&provider.allowed_roles) {
			warn!(
				"Rejected OpenID Connect sign-in of `{username}`, who has none of the allowed roles (`{}` claim: {roles:?})",
				provider.role_claim()
			);
			return Err(Error::OidcAccessDenied(username));
		}

		let is_admin = match provider.admin_roles.is_empty() {
			true => None,
			false => Some(has_any_role(&provider.admin_roles)),
		};

		let (token, is_admin) = self
			.config_manager
			.login_external(&username, is_admin)
			.await?;
		info!(
			"User `{username}` signed in with OpenID Connect provider `{}`",
			provider.name
		);

		Ok(Login {
			username,
			token,
			is_admin,
		})
	}
}

fn random_token() -> String {
	let mut bytes = [0u8; 32];
	OsRng.fill_bytes(&mut bytes);
	URL_SAFE_NO_PAD.encode(bytes)
}

fn claim_names(claims: &Value) -> String {
	claims
		.as_object()
		.map(|c| c.keys().cloned().collect::<Vec<_>>().join(", "))
		.unwrap_or_default()
}

fn make_agent(provider: &config::OidcProvider) -> Result<ureq::Agent, Error> {
	let mut builder = ureq::Agent::config_builder()
		.timeout_global(Some(HTTP_TIMEOUT))
		.http_status_as_error(false);

	if let Some(ca_file) = &provider.ca_file {
		let pem = std::fs::read(ca_file).map_err(|e| Error::Io(ca_file.clone(), e))?;
		let certificates = ureq::tls::parse_pem(&pem)
			.filter_map(|item| match item {
				Ok(ureq::tls::PemItem::Certificate(certificate)) => Some(certificate),
				_ => None,
			})
			.collect::<Vec<_>>();
		if certificates.is_empty() {
			return Err(Error::OidcProviderInvalid(
				provider.name.clone(),
				"`ca_file` does not contain any PEM certificate".to_owned(),
			));
		}
		let tls_config = ureq::tls::TlsConfig::builder()
			.root_certs(ureq::tls::RootCerts::Specific(Arc::new(certificates)))
			.build();
		builder = builder.tls_config(tls_config);
	}

	Ok(builder.build().into())
}

fn read_json_response(
	response: Result<http::Response<ureq::Body>, ureq::Error>,
	description: &str,
) -> Result<Value, Error> {
	let mut response = response
		.map_err(|e| Error::OidcProviderUnreachable(format!("{description} failed: {e}")))?;
	let status = response.status();
	let body = response
		.body_mut()
		.read_to_string()
		.map_err(|e| Error::OidcProviderUnreachable(format!("{description} failed: {e}")))?;
	if !status.is_success() {
		let excerpt = body.chars().take(500).collect::<String>();
		return Err(Error::OidcProviderUnreachable(format!(
			"{description} was rejected with status {status}: {excerpt}"
		)));
	}
	serde_json::from_str(&body)
		.map_err(|_| Error::OidcInvalidResponse(format!("{description} did not return JSON")))
}

fn fetch_discovery(provider: &config::OidcProvider) -> Result<Discovery, Error> {
	let issuer = provider.issuer.trim_end_matches('/');
	if !issuer.starts_with("https://") {
		warn!(
			"OpenID Connect provider `{}` is not using HTTPS, sign-ins can be tampered with",
			provider.name
		);
	}

	let url = format!("{issuer}/.well-known/openid-configuration");
	let agent = make_agent(provider)?;
	let document = read_json_response(agent.get(&url).call(), "Provider discovery")?;
	let discovery = serde_json::from_value::<Discovery>(document).map_err(|e| {
		Error::OidcInvalidResponse(format!("provider discovery document is incomplete: {e}"))
	})?;

	if discovery.issuer.trim_end_matches('/') != issuer {
		return Err(Error::OidcInvalidResponse(format!(
			"provider identifies as `{}`, which does not match the configured issuer `{}`",
			discovery.issuer, provider.issuer
		)));
	}

	Ok(discovery)
}

/// Exchanges an authorization code for the claims describing the user who signed in.
fn fetch_claims(
	provider: &config::OidcProvider,
	discovery: &Discovery,
	pending_login: &PendingLogin,
	code: &str,
) -> Result<Value, Error> {
	let agent = make_agent(provider)?;

	let mut form = vec![
		("grant_type", "authorization_code"),
		("code", code),
		("redirect_uri", &pending_login.redirect_uri),
		("code_verifier", &pending_login.code_verifier),
	];
	let mut request = agent.post(&discovery.token_endpoint);
	match (&provider.client_secret, provider.token_auth_method) {
		(Some(secret), config::TokenAuthMethod::ClientSecretBasic) => {
			let encode =
				|s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
			let credentials = format!("{}:{}", encode(&provider.client_id), encode(secret));
			let credentials = STANDARD.encode(credentials);
			request = request.header("Authorization", format!("Basic {credentials}"));
		}
		(Some(secret), config::TokenAuthMethod::ClientSecretPost) => {
			form.push(("client_id", &provider.client_id));
			form.push(("client_secret", secret));
		}
		(None, _) => form.push(("client_id", &provider.client_id)),
	}

	let token_response = read_json_response(request.send_form(form), "Token request")?;
	let token_response = serde_json::from_value::<TokenResponse>(token_response)
		.map_err(|e| Error::OidcInvalidResponse(format!("malformed token response: {e}")))?;

	let id_token = token_response
		.id_token
		.ok_or_else(|| Error::OidcInvalidResponse("no ID token was issued".to_owned()))?;
	let mut claims = decode_jwt_claims(&id_token)
		.map_err(|e| Error::OidcInvalidResponse(format!("invalid ID token: {e}")))?;
	let now = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_secs();
	validate_id_token(
		&claims,
		&discovery.issuer,
		&provider.client_id,
		&pending_login.nonce,
		now,
	)
	.map_err(Error::OidcInvalidResponse)?;

	// Providers are free to leave most claims out of the ID token, and to only offer them
	// through the UserInfo endpoint.
	if let (Some(endpoint), Some(access_token)) =
		(&discovery.userinfo_endpoint, &token_response.access_token)
	{
		let request = agent
			.get(endpoint)
			.header("Authorization", format!("Bearer {access_token}"));
		let user_info = read_json_response(request.call(), "UserInfo request")?;
		if user_info.get("sub") != claims.get("sub") {
			return Err(Error::OidcInvalidResponse(
				"UserInfo describes a different user than the ID token".to_owned(),
			));
		}
		if let (Some(claims), Value::Object(user_info)) = (claims.as_object_mut(), user_info) {
			for (name, value) in user_info {
				claims.entry(name).or_insert(value);
			}
		}
	}

	// Some providers (notably Keycloak, for roles) only describe parts of the user in the
	// access token. Access tokens are opaque as far as OpenID Connect is concerned, but in
	// practice they are often JWTs. Like the ID token, they were received straight from
	// the provider.
	let access_token_claims = token_response
		.access_token
		.as_deref()
		.and_then(|token| decode_jwt_claims(token).ok());
	if let (Some(claims), Some(Value::Object(access_token_claims))) =
		(claims.as_object_mut(), access_token_claims)
	{
		for (name, value) in access_token_claims {
			claims.entry(name).or_insert(value);
		}
	}

	Ok(claims)
}

/// Reads the claims of a JWT, such as an ID token.
///
/// The signature of the token is intentionally not verified. Tokens are only ever received
/// directly from the token endpoint of the provider, in which case the OpenID Connect
/// specification (Core 1.0, section 3.1.3.7) allows relying on the TLS connection to
/// authenticate the issuer.
fn decode_jwt_claims(token: &str) -> Result<Value, String> {
	let mut segments = token.split('.');
	let (Some(_header), Some(payload), Some(_signature), None) = (
		segments.next(),
		segments.next(),
		segments.next(),
		segments.next(),
	) else {
		return Err("the token is not a signed JWT".to_owned());
	};
	let payload = URL_SAFE_NO_PAD
		.decode(payload.trim_end_matches('='))
		.map_err(|_| "the token is not valid base64".to_owned())?;
	match serde_json::from_slice::<Value>(&payload) {
		Ok(claims) if claims.is_object() => Ok(claims),
		_ => Err("the token does not contain claims".to_owned()),
	}
}

fn validate_id_token(
	claims: &Value,
	issuer: &str,
	client_id: &str,
	nonce: &str,
	now: u64,
) -> Result<(), String> {
	if claims.get("iss").and_then(Value::as_str) != Some(issuer) {
		return Err("the ID token was issued by an unexpected issuer".to_owned());
	}

	let is_intended_audience = match claims.get("aud") {
		Some(Value::String(audience)) => audience == client_id,
		Some(Value::Array(audiences)) => audiences.iter().any(|a| a.as_str() == Some(client_id)),
		_ => false,
	};
	if !is_intended_audience {
		return Err("the ID token was issued for a different client".to_owned());
	}

	match claims.get("exp").and_then(Value::as_u64) {
		Some(expiration) if expiration + CLOCK_SKEW_SECONDS > now => (),
		_ => return Err("the ID token has expired".to_owned()),
	}

	if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
		return Err("the ID token does not belong to this sign-in attempt".to_owned());
	}

	if claims.get("sub").and_then(Value::as_str).is_none() {
		return Err("the ID token does not identify a user".to_owned());
	}

	Ok(())
}

/// Finds a claim by name. Claims nested within other claims can be reached with a
/// `.`-separated path (eg. `realm_access.roles`); a literal `.` is written `\.`.
fn lookup_claim<'a>(claims: &'a Value, path: &str) -> Option<&'a Value> {
	if let Some(claim) = claims.get(path) {
		return Some(claim);
	}

	const ESCAPED_DOT: &str = "\u{0}";
	let escaped_path = path.replace("\\.", ESCAPED_DOT);
	let mut claim = claims;
	for segment in escaped_path.split('.') {
		claim = claim.get(segment.replace(ESCAPED_DOT, "."))?;
	}
	Some(claim)
}

fn read_roles(claims: &Value, role_claim: &str) -> Vec<String> {
	match lookup_claim(claims, role_claim) {
		Some(Value::Array(roles)) => roles
			.iter()
			.filter_map(|r| r.as_str().map(str::to_owned))
			.collect(),
		Some(Value::String(role)) => vec![role.clone()],
		_ => Vec::new(),
	}
}

/// A minimal OpenID Connect identity provider to test against
#[cfg(test)]
pub mod mock {
	use std::collections::HashMap;
	use std::sync::{Arc, Mutex};

	use axum::{
		body::Bytes,
		extract::State,
		http::{HeaderMap, StatusCode},
		routing::{get, post},
		Json, Router,
	};
	use base64::{
		engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
		Engine,
	};
	use serde_json::{json, Map, Value};
	use sha2::{Digest, Sha256};

	use crate::app::config::storage;

	pub const CLIENT_ID: &str = "polaris";
	pub const CLIENT_SECRET: &str = "s3cr3t/with+special&chars";

	struct Grant {
		nonce: String,
		code_challenge: String,
		redirect_uri: String,
	}

	#[derive(Default)]
	struct Inner {
		issuer: String,
		user_claims: Map<String, Value>,
		id_token_overrides: Map<String, Value>,
		claims_in_id_token: bool,
		access_token_claims: Option<Value>,
		expects_client_secret_post: bool,
		grants: HashMap<String, Grant>,
		access_tokens: Vec<String>,
		num_grants: usize,
	}

	#[derive(Clone)]
	pub struct Provider {
		pub issuer: String,
		inner: Arc<Mutex<Inner>>,
	}

	impl Provider {
		pub async fn start() -> Self {
			let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
			let issuer = format!("http://{}", listener.local_addr().unwrap());
			let inner = Arc::new(Mutex::new(Inner {
				issuer: issuer.clone(),
				..Default::default()
			}));
			let provider = Self { issuer, inner };

			let router = Router::new()
				.route("/.well-known/openid-configuration", get(discovery))
				.route("/token", post(token))
				.route("/userinfo", get(user_info))
				.with_state(provider.clone());
			tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

			provider.set_user("alice", &[]);
			provider
		}

		pub fn config(&self, name: &str) -> storage::OidcProvider {
			storage::OidcProvider {
				name: name.to_owned(),
				issuer: self.issuer.clone(),
				client_id: CLIENT_ID.to_owned(),
				client_secret: Some(CLIENT_SECRET.to_owned()),
				..Default::default()
			}
		}

		/// Sets who is signed in at the provider.
		pub fn set_user(&self, username: &str, groups: &[&str]) {
			self.set_user_claims(json!({
				"sub": format!("id-of-{username}"),
				"preferred_username": username,
				"groups": groups,
			}));
		}

		pub fn set_user_claims(&self, claims: Value) {
			self.inner.lock().unwrap().user_claims = claims.as_object().unwrap().clone();
		}

		/// Makes the provider issue ID tokens with tampered claims.
		pub fn override_id_token_claim(&self, name: &str, value: Value) {
			let mut inner = self.inner.lock().unwrap();
			inner.id_token_overrides.insert(name.to_owned(), value);
		}

		/// Whether user claims are part of ID tokens, or only available from UserInfo.
		pub fn include_claims_in_id_token(&self, include: bool) {
			self.inner.lock().unwrap().claims_in_id_token = include;
		}

		/// Makes the provider issue JWT access tokens carrying the specified claims.
		pub fn set_access_token_claims(&self, claims: Value) {
			self.inner.lock().unwrap().access_token_claims = Some(claims);
		}

		pub fn expect_client_secret_post(&self) {
			self.inner.lock().unwrap().expects_client_secret_post = true;
		}

		/// Plays the part of the user signing in at the provider. Validates the
		/// authorization request, and returns the code the user is sent back with.
		pub fn authorize(&self, authorization_url: &str) -> String {
			let (endpoint, query) = authorization_url.split_once('?').unwrap();
			assert_eq!(endpoint, format!("{}/authorize", self.issuer));
			let params = form_urlencoded::parse(query.as_bytes())
				.into_owned()
				.collect::<HashMap<_, _>>();
			assert_eq!(params["response_type"], "code");
			assert_eq!(params["client_id"], CLIENT_ID);
			assert_eq!(params["code_challenge_method"], "S256");
			assert!(params["scope"].split(' ').any(|s| s == "openid"));
			assert!(params["state"].len() >= 32);

			let mut inner = self.inner.lock().unwrap();
			inner.num_grants += 1;
			let code = format!("code-{}", inner.num_grants);
			inner.grants.insert(
				code.clone(),
				Grant {
					nonce: params["nonce"].clone(),
					code_challenge: params["code_challenge"].clone(),
					redirect_uri: params["redirect_uri"].clone(),
				},
			);
			code
		}
	}

	async fn discovery(State(provider): State<Provider>) -> Json<Value> {
		let issuer = &provider.issuer;
		Json(json!({
			"issuer": issuer,
			"authorization_endpoint": format!("{issuer}/authorize"),
			"token_endpoint": format!("{issuer}/token"),
			"userinfo_endpoint": format!("{issuer}/userinfo"),
		}))
	}

	fn rejection(error: &str) -> (StatusCode, Json<Value>) {
		(StatusCode::BAD_REQUEST, Json(json!({ "error": error })))
	}

	async fn token(
		State(provider): State<Provider>,
		headers: HeaderMap,
		body: Bytes,
	) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
		let form = form_urlencoded::parse(&body)
			.into_owned()
			.collect::<HashMap<_, _>>();
		let mut inner = provider.inner.lock().unwrap();

		let is_authenticated = if inner.expects_client_secret_post {
			form.get("client_id").map(String::as_str) == Some(CLIENT_ID)
				&& form.get("client_secret").map(String::as_str) == Some(CLIENT_SECRET)
		} else {
			let encode =
				|s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
			let credentials = format!("{}:{}", encode(CLIENT_ID), encode(CLIENT_SECRET));
			let expected = format!("Basic {}", STANDARD.encode(credentials));
			headers.get("Authorization").and_then(|h| h.to_str().ok()) == Some(&expected)
		};
		if !is_authenticated {
			return Err(rejection("invalid_client"));
		}

		if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
			return Err(rejection("unsupported_grant_type"));
		}

		// Codes are single use
		let code = form.get("code").cloned().unwrap_or_default();
		let grant = inner
			.grants
			.remove(&code)
			.ok_or(rejection("invalid_grant"))?;

		let verifier = form.get("code_verifier").cloned().unwrap_or_default();
		let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
		if challenge != grant.code_challenge
			|| form.get("redirect_uri") != Some(&grant.redirect_uri)
		{
			return Err(rejection("invalid_grant"));
		}

		let now = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_secs();
		let mut claims = Map::new();
		if inner.claims_in_id_token {
			claims.extend(inner.user_claims.clone());
		}
		claims.insert("iss".to_owned(), json!(inner.issuer));
		claims.insert("aud".to_owned(), json!(CLIENT_ID));
		claims.insert("sub".to_owned(), inner.user_claims["sub"].clone());
		claims.insert("iat".to_owned(), json!(now));
		claims.insert("exp".to_owned(), json!(now + 300));
		claims.insert("nonce".to_owned(), json!(grant.nonce));
		claims.extend(inner.id_token_overrides.clone());

		let encode = |value: &Value| URL_SAFE_NO_PAD.encode(value.to_string());
		let id_token = format!(
			"{}.{}.{}",
			encode(&json!({ "alg": "RS256", "typ": "JWT" })),
			encode(&Value::Object(claims)),
			URL_SAFE_NO_PAD.encode("signature")
		);

		let access_token = match &inner.access_token_claims {
			Some(claims) => format!(
				"{}.{}.{}",
				encode(&json!({ "alg": "RS256", "typ": "JWT" })),
				encode(claims),
				URL_SAFE_NO_PAD.encode(format!("signature-for-{code}"))
			),
			None => format!("access-token-for-{code}"),
		};
		inner.access_tokens.push(access_token.clone());

		Ok(Json(json!({
			"access_token": access_token,
			"token_type": "Bearer",
			"expires_in": 300,
			"id_token": id_token,
		})))
	}

	async fn user_info(
		State(provider): State<Provider>,
		headers: HeaderMap,
	) -> Result<Json<Value>, StatusCode> {
		let inner = provider.inner.lock().unwrap();
		let is_authorized = headers
			.get("Authorization")
			.and_then(|h| h.to_str().ok())
			.and_then(|h| h.strip_prefix("Bearer "))
			.is_some_and(|token| inner.access_tokens.iter().any(|t| t == token));
		if !is_authorized {
			return Err(StatusCode::UNAUTHORIZED);
		}
		Ok(Json(Value::Object(inner.user_claims.clone())))
	}
}

#[cfg(test)]
mod test {
	use serde_json::json;

	use super::*;
	use crate::app::config::storage;
	use crate::app::test::ContextBuilder;
	use crate::test_name;

	const REDIRECT_URI: &str = "https://polaris.example.com/api/oidc/test/callback";

	struct Context {
		provider: mock::Provider,
		manager: Manager,
		config_manager: config::Manager,
	}

	impl Context {
		async fn new(test_name: String, configure: fn(&mut storage::OidcProvider)) -> Self {
			let provider = mock::Provider::start().await;
			let ctx = ContextBuilder::new(test_name)
				.user("existing-admin", "password", true)
				.build()
				.await;

			let mut provider_config = provider.config("test");
			configure(&mut provider_config);
			let mut config = storage::Config::from(ctx.config_manager.get_config().await);
			config.oidc_providers = vec![provider_config];
			ctx.config_manager.apply_config(config).await.unwrap();

			Self {
				provider,
				manager: Manager::new(ctx.config_manager.clone()),
				config_manager: ctx.config_manager,
			}
		}

		async fn login(&self) -> Result<Login, Error> {
			let start = self
				.manager
				.begin_login("test", REDIRECT_URI.to_owned())
				.await?;
			let code = self.provider.authorize(&start.authorization_url);
			self.manager
				.complete_login("test", &start.state, &code)
				.await
		}
	}

	#[tokio::test]
	async fn creates_users_on_first_login() {
		let ctx = Context::new(test_name!(), |_| ()).await;
		ctx.provider.set_user("alice", &[]);

		assert!(ctx.config_manager.get_user("alice").await.is_err());
		let login = ctx.login().await.unwrap();
		assert_eq!(login.username, "alice");
		assert!(!login.is_admin);

		let authorization = ctx
			.config_manager
			.authenticate(&login.token, auth::Scope::PolarisAuth)
			.await
			.unwrap();
		assert_eq!(authorization.username, "alice");

		// Later logins reuse the same user, and keep earlier tokens valid
		let user = ctx.config_manager.get_user("alice").await.unwrap();
		ctx.login().await.unwrap();
		assert_eq!(ctx.config_manager.get_user("alice").await.unwrap(), user);
		assert_eq!(ctx.config_manager.get_users().await.len(), 2);
	}

	#[tokio::test]
	async fn signs_in_existing_users() {
		let ctx = Context::new(test_name!(), |_| ()).await;
		ctx.provider.set_user("existing-admin", &[]);
		let login = ctx.login().await.unwrap();
		assert_eq!(login.username, "existing-admin");
		// Without admin roles configured, administrator status is left alone
		assert!(login.is_admin);
	}

	#[tokio::test]
	async fn reads_claims_from_id_token_or_user_info() {
		let ctx = Context::new(test_name!(), |_| ()).await;
		for claims_in_id_token in [true, false] {
			ctx.provider.include_claims_in_id_token(claims_in_id_token);
			assert_eq!(ctx.login().await.unwrap().username, "alice");
		}
	}

	#[tokio::test]
	async fn supports_client_secret_post() {
		let ctx = Context::new(test_name!(), |p| {
			p.token_auth_method = Some("client_secret_post".to_owned())
		})
		.await;
		ctx.provider.expect_client_secret_post();
		assert_eq!(ctx.login().await.unwrap().username, "alice");
	}

	#[tokio::test]
	async fn rejects_wrong_client_secret() {
		let ctx = Context::new(test_name!(), |p| p.client_secret = Some("wrong".to_owned())).await;
		let result = ctx.login().await;
		assert!(matches!(result, Err(Error::OidcProviderUnreachable(_))));
		assert!(ctx.config_manager.get_user("alice").await.is_err());
	}

	#[tokio::test]
	async fn grants_and_revokes_admin_rights() {
		let ctx = Context::new(test_name!(), |p| {
			p.admin_roles = vec!["polaris-admins".to_owned()]
		})
		.await;

		ctx.provider.set_user("alice", &["users", "polaris-admins"]);
		assert!(ctx.login().await.unwrap().is_admin);
		assert!(ctx
			.config_manager
			.get_user("alice")
			.await
			.unwrap()
			.is_admin());

		ctx.provider.set_user("alice", &["users"]);
		assert!(!ctx.login().await.unwrap().is_admin);
		assert!(!ctx
			.config_manager
			.get_user("alice")
			.await
			.unwrap()
			.is_admin());
	}

	#[tokio::test]
	async fn restricts_access_to_allowed_roles() {
		let ctx = Context::new(test_name!(), |p| {
			p.allowed_roles = vec!["music".to_owned(), "family".to_owned()]
		})
		.await;

		ctx.provider.set_user("mallory", &["guests"]);
		let result = ctx.login().await;
		assert!(matches!(result, Err(Error::OidcAccessDenied(u)) if u == "mallory"));
		assert!(ctx.config_manager.get_user("mallory").await.is_err());

		ctx.provider.set_user("alice", &["family"]);
		assert_eq!(ctx.login().await.unwrap().username, "alice");
	}

	#[tokio::test]
	async fn supports_custom_claims() {
		let ctx = Context::new(test_name!(), |p| {
			p.username_claim = Some("email".to_owned());
			p.role_claim = Some("realm_access.roles".to_owned());
			p.admin_roles = vec!["admin".to_owned()];
		})
		.await;
		ctx.provider.set_user_claims(json!({
			"sub": "1234",
			"preferred_username": "ignored",
			"email": "alice@example.com",
			"realm_access": { "roles": ["offline_access", "admin"] },
		}));

		let login = ctx.login().await.unwrap();
		assert_eq!(login.username, "alice@example.com");
		assert!(login.is_admin);
	}

	#[tokio::test]
	async fn reads_roles_from_access_token() {
		// By default, Keycloak only lists roles in its access tokens
		let ctx = Context::new(test_name!(), |p| {
			p.role_claim = Some("realm_access.roles".to_owned());
			p.allowed_roles = vec!["polaris-users".to_owned()];
			p.admin_roles = vec!["polaris-admins".to_owned()];
		})
		.await;
		ctx.provider.set_access_token_claims(json!({
			"preferred_username": "not-alice",
			"realm_access": { "roles": ["polaris-users", "polaris-admins"] },
		}));

		let login = ctx.login().await.unwrap();
		// Claims of the ID token and UserInfo take precedence
		assert_eq!(login.username, "alice");
		assert!(login.is_admin);

		ctx.provider.set_access_token_claims(json!({
			"realm_access": { "roles": ["other"] },
		}));
		assert!(matches!(ctx.login().await, Err(Error::OidcAccessDenied(_))));
	}

	#[tokio::test]
	async fn requires_username_claim() {
		let ctx = Context::new(test_name!(), |_| ()).await;
		ctx.provider.set_user_claims(json!({ "sub": "1234" }));
		assert!(matches!(
			ctx.login().await,
			Err(Error::OidcInvalidResponse(_))
		));
	}

	#[tokio::test]
	async fn rejects_unknown_providers_and_states() {
		let ctx = Context::new(test_name!(), |_| ()).await;

		let result = ctx
			.manager
			.begin_login("nope", REDIRECT_URI.to_owned())
			.await;
		assert!(matches!(result, Err(Error::OidcProviderNotFound)));

		let start = ctx
			.manager
			.begin_login("test", REDIRECT_URI.to_owned())
			.await
			.unwrap();
		let code = ctx.provider.authorize(&start.authorization_url);

		let result = ctx.manager.complete_login("test", "made-up", &code).await;
		assert!(matches!(result, Err(Error::OidcLoginExpired)));

		// Sign-in attempts are bound to the provider they were started with
		let result = ctx
			.manager
			.complete_login("other", &start.state, &code)
			.await;
		assert!(matches!(result, Err(Error::OidcLoginExpired)));
	}

	#[tokio::test]
	async fn logins_cannot_be_replayed() {
		let ctx = Context::new(test_name!(), |_| ()).await;
		let start = ctx
			.manager
			.begin_login("test", REDIRECT_URI.to_owned())
			.await
			.unwrap();
		let code = ctx.provider.authorize(&start.authorization_url);
		ctx.manager
			.complete_login("test", &start.state, &code)
			.await
			.unwrap();
		let result = ctx
			.manager
			.complete_login("test", &start.state, &code)
			.await;
		assert!(matches!(result, Err(Error::OidcLoginExpired)));
	}

	#[tokio::test]
	async fn rejects_codes_from_other_login_attempts() {
		let ctx = Context::new(test_name!(), |_| ()).await;
		let first = ctx
			.manager
			.begin_login("test", REDIRECT_URI.to_owned())
			.await
			.unwrap();
		let second = ctx
			.manager
			.begin_login("test", REDIRECT_URI.to_owned())
			.await
			.unwrap();
		// PKCE ties each code to the attempt which requested it
		let code = ctx.provider.authorize(&first.authorization_url);
		let result = ctx
			.manager
			.complete_login("test", &second.state, &code)
			.await;
		assert!(matches!(result, Err(Error::OidcProviderUnreachable(_))));
	}

	#[tokio::test]
	async fn rejects_tampered_id_tokens() {
		let overrides = [
			("iss", json!("https://evil.example.com")),
			("aud", json!("another-client")),
			("aud", json!(["another-client", "yet-another-client"])),
			("exp", json!(1_000_000_000)),
			("nonce", json!("replayed")),
			("sub", json!("someone-else")),
		];
		for (claim, value) in overrides {
			let ctx = Context::new(format!("{}-{claim}", test_name!()), |_| ()).await;
			ctx.provider.override_id_token_claim(claim, value);
			let result = ctx.login().await;
			assert!(
				matches!(result, Err(Error::OidcInvalidResponse(_))),
				"{claim}: {result:?}"
			);
			assert!(ctx.config_manager.get_user("alice").await.is_err());
		}

		let ctx = Context::new(test_name!(), |_| ()).await;
		ctx.provider
			.override_id_token_claim("aud", json!(["another-client", mock::CLIENT_ID]));
		assert!(ctx.login().await.is_ok());
	}

	#[tokio::test]
	async fn rejects_issuer_mismatch() {
		let ctx = Context::new(test_name!(), |p| p.issuer = format!("{}/", p.issuer)).await;
		assert!(ctx.login().await.is_ok());

		let ctx = Context::new(test_name!(), |p| {
			p.issuer = p.issuer.replace("127.0.0.1", "localhost")
		})
		.await;
		assert!(matches!(
			ctx.login().await,
			Err(Error::OidcInvalidResponse(_))
		));
	}

	#[test]
	fn looks_up_nested_claims() {
		let claims = json!({
			"groups": ["a", "b"],
			"role": "single",
			"realm_access": { "roles": ["c"] },
			"https://example.com/roles": ["d"],
			"resource_access": { "my.client": { "roles": ["e"] } },
		});
		assert_eq!(read_roles(&claims, "groups"), ["a", "b"]);
		assert_eq!(read_roles(&claims, "role"), ["single"]);
		assert_eq!(read_roles(&claims, "realm_access.roles"), ["c"]);
		assert_eq!(read_roles(&claims, "https://example.com/roles"), ["d"]);
		assert_eq!(
			read_roles(&claims, r"resource_access.my\.client.roles"),
			["e"]
		);
		assert!(read_roles(&claims, "missing").is_empty());
		assert!(read_roles(&claims, "realm_access.missing").is_empty());
	}

	#[test]
	fn decodes_jwt_claims() {
		let encode = |s: &str| URL_SAFE_NO_PAD.encode(s);
		let token = format!("{}.{}.sig", encode("{}"), encode(r#"{"sub":"1"}"#));
		assert_eq!(decode_jwt_claims(&token).unwrap(), json!({ "sub": "1" }));
		assert!(decode_jwt_claims("not-a-jwt").is_err());
		assert!(decode_jwt_claims("a.b").is_err());
		assert!(decode_jwt_claims("a.b.c.d").is_err());
		assert!(decode_jwt_claims(&format!("a.{}.c", encode("[1]"))).is_err());
	}
}
