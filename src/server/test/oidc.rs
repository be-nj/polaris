use std::collections::HashMap;
use std::time::Duration;

use http::{header, Request, StatusCode};

use crate::app::oidc::mock;
use crate::server::dto;
use crate::server::test::{constants::*, protocol, ServiceType, TestService};
use crate::test_name;

const PROVIDER: &str = "test-sso";
const PUBLIC_URL: &str = "https://polaris.example.com";

/// Starts a Polaris server configured to sign users in with a mock identity provider.
async fn make_service(test_name: &str, extra_config: &str) -> (ServiceType, mock::Provider) {
	let provider = mock::Provider::start().await;
	let mut service = ServiceType::new(test_name).await;

	// Single sign-on providers can only be configured through the configuration file
	let config = format!(
		r#"
{extra_config}

[[users]]
name = "{TEST_USERNAME_ADMIN}"
initial_password = "{TEST_PASSWORD_ADMIN}"
admin = true

[[oidc_providers]]
name = "{PROVIDER}"
display_name = "Test SSO"
issuer = "{}"
client_id = "{}"
client_secret = "{}"
allowed_roles = ["music"]
admin_roles = ["admins"]
"#,
		provider.issuer,
		mock::CLIENT_ID,
		mock::CLIENT_SECRET,
	);
	let config_path = format!("test-output/{test_name}/polaris.toml");
	std::fs::write(config_path, config).unwrap();

	// Waits for the server to notice its configuration file was modified
	let request = protocol::oidc_providers();
	for _ in 0..100 {
		let response = service
			.fetch_json::<_, Vec<dto::OidcProvider>>(&request)
			.await;
		if !response.body().is_empty() {
			provider.set_user("alice", &["music"]);
			return (service, provider);
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	panic!("Server did not pick up single sign-on configuration");
}

struct LoginAttempt {
	authorization_url: String,
	cookie: String,
	state: String,
}

async fn start_login(service: &mut ServiceType, request: Request<()>) -> LoginAttempt {
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::SEE_OTHER);
	let headers = response.headers();
	assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");

	let authorization_url = headers.get(header::LOCATION).unwrap().to_str().unwrap();
	let (_, query) = authorization_url.split_once('?').unwrap();
	let parameters = form_urlencoded::parse(query.as_bytes())
		.into_owned()
		.collect::<HashMap<_, _>>();

	let set_cookie = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
	assert!(set_cookie.contains("HttpOnly"));
	assert!(set_cookie.contains("SameSite=Lax"));
	let cookie = set_cookie.split(';').next().unwrap().to_owned();
	assert_eq!(
		cookie,
		format!("polaris_oidc_state={}", parameters["state"])
	);

	LoginAttempt {
		authorization_url: authorization_url.to_owned(),
		cookie,
		state: parameters["state"].clone(),
	}
}

fn callback(attempt: &LoginAttempt, code: &str) -> Request<()> {
	let parameters = [("code", code), ("state", &attempt.state)];
	let mut request = protocol::oidc_callback(PROVIDER, &parameters);
	request
		.headers_mut()
		.insert(header::COOKIE, attempt.cookie.parse().unwrap());
	request
}

/// Reads the session handed over to the web client by the page ending a sign-in.
fn read_session(page: &[u8]) -> serde_json::Value {
	let page = std::str::from_utf8(page).unwrap();
	let (_, session) = page.split_once("JSON.stringify(").unwrap();
	let (session, _) = session.split_once(")").unwrap();
	serde_json::from_str(session).unwrap()
}

/// Reads where the page ending a sign-in sends the browser to.
fn read_destination(page: &[u8]) -> String {
	let page = std::str::from_utf8(page).unwrap();
	let (_, destination) = page.split_once("window.location.replace(").unwrap();
	let (destination, _) = destination.split_once(");").unwrap();
	serde_json::from_str(destination).unwrap()
}

#[tokio::test]
async fn lists_no_providers_by_default() {
	let mut service = ServiceType::new(&test_name!()).await;
	let request = protocol::oidc_providers();
	let response = service
		.fetch_json::<_, Vec<dto::OidcProvider>>(&request)
		.await;
	assert_eq!(response.status(), StatusCode::OK);
	assert!(response.body().is_empty());

	let response = service.fetch(&protocol::oidc_start("nope")).await;
	assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn login_golden_path() {
	let config = format!(r#"public_url = "{PUBLIC_URL}/""#);
	let (mut service, provider) = make_service(&test_name!(), &config).await;

	let request = protocol::oidc_providers();
	let response = service
		.fetch_json::<_, Vec<dto::OidcProvider>>(&request)
		.await;
	assert_eq!(
		response.body(),
		&vec![dto::OidcProvider {
			name: PROVIDER.to_owned(),
			display_name: "Test SSO".to_owned(),
			login_url: format!("/api/oidc/{PROVIDER}/start"),
		}]
	);

	let attempt = start_login(&mut service, protocol::oidc_start(PROVIDER)).await;
	assert!(attempt.authorization_url.starts_with(&provider.issuer));
	let expected_redirect_uri = form_urlencoded::byte_serialize(
		format!("{PUBLIC_URL}/api/oidc/{PROVIDER}/callback").as_bytes(),
	)
	.collect::<String>();
	assert!(attempt
		.authorization_url
		.contains(&format!("redirect_uri={expected_redirect_uri}&")));

	let code = provider.authorize(&attempt.authorization_url);
	let response = service.fetch_bytes(&callback(&attempt, &code)).await;
	assert_eq!(response.status(), StatusCode::OK);
	let headers = response.headers();
	assert!(headers
		.get(header::CONTENT_TYPE)
		.unwrap()
		.to_str()
		.unwrap()
		.starts_with("text/html"));
	assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
	let cleared_cookie = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
	assert!(cleared_cookie.starts_with("polaris_oidc_state=;"));
	assert!(cleared_cookie.contains("Max-Age=0"));
	assert!(cleared_cookie.contains("Secure"));

	let session = read_session(response.body());
	assert_eq!(session["name"], "alice");
	assert_eq!(session["isAdmin"], false);
	assert_eq!(read_destination(response.body()), "/");

	// The session works with the rest of the API
	service.set_authorization(Some(dto::Authorization {
		username: "alice".to_owned(),
		token: session["authToken"].as_str().unwrap().to_owned(),
		is_admin: false,
	}));
	let response = service.fetch(&protocol::transcoding()).await;
	assert_eq!(response.status(), StatusCode::OK);
	let response = service.fetch(&protocol::get_settings()).await;
	assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_grants_admin_rights() {
	let (mut service, provider) = make_service(&test_name!(), "").await;
	provider.set_user("alice", &["music", "admins"]);

	let attempt = start_login(&mut service, protocol::oidc_start(PROVIDER)).await;
	let code = provider.authorize(&attempt.authorization_url);
	let response = service.fetch_bytes(&callback(&attempt, &code)).await;
	let session = read_session(response.body());
	assert_eq!(session["isAdmin"], true);

	service.set_authorization(Some(dto::Authorization {
		username: "alice".to_owned(),
		token: session["authToken"].as_str().unwrap().to_owned(),
		is_admin: true,
	}));
	let response = service.fetch(&protocol::get_settings()).await;
	assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn login_uses_address_from_reverse_proxy() {
	let (mut service, _provider) = make_service(&test_name!(), "").await;

	let mut request = protocol::oidc_start(PROVIDER);
	let headers = request.headers_mut();
	headers.insert("X-Forwarded-Proto", "https".parse().unwrap());
	headers.insert("X-Forwarded-Host", "music.example.com".parse().unwrap());
	let attempt = start_login(&mut service, request).await;
	assert!(attempt.authorization_url.contains(
		"redirect_uri=https%3A%2F%2Fmusic.example.com%2Fapi%2Foidc%2Ftest-sso%2Fcallback&"
	));

	let mut request = protocol::oidc_start(PROVIDER);
	let headers = request.headers_mut();
	headers.insert(header::HOST, "192.168.1.10:5050".parse().unwrap());
	let attempt = start_login(&mut service, request).await;
	assert!(attempt.authorization_url.contains(
		"redirect_uri=http%3A%2F%2F192.168.1.10%3A5050%2Fapi%2Foidc%2Ftest-sso%2Fcallback&"
	));
}

#[tokio::test]
async fn login_is_bound_to_browser() {
	let (mut service, provider) = make_service(&test_name!(), "").await;

	// Without the cookie set when the sign-in was started
	let attempt = start_login(&mut service, protocol::oidc_start(PROVIDER)).await;
	let code = provider.authorize(&attempt.authorization_url);
	let parameters = [("code", &code[..]), ("state", &attempt.state)];
	let request = protocol::oidc_callback(PROVIDER, &parameters);
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::BAD_REQUEST);

	// With the cookie of another sign-in attempt
	let other_attempt = start_login(&mut service, protocol::oidc_start(PROVIDER)).await;
	let mut request = protocol::oidc_callback(PROVIDER, &parameters);
	request
		.headers_mut()
		.insert(header::COOKIE, other_attempt.cookie.parse().unwrap());
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::BAD_REQUEST);

	// The legitimate browser is still able to complete its sign-in, but only once
	let response = service.fetch(&callback(&attempt, &code)).await;
	assert_eq!(response.status(), StatusCode::OK);
	let response = service.fetch(&callback(&attempt, &code)).await;
	assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_reports_failures() {
	let (mut service, provider) = make_service(&test_name!(), "").await;

	// Users without any of the allowed roles
	provider.set_user("mallory", &["guests"]);
	let attempt = start_login(&mut service, protocol::oidc_start(PROVIDER)).await;
	let code = provider.authorize(&attempt.authorization_url);
	let response = service.fetch(&callback(&attempt, &code)).await;
	assert_eq!(response.status(), StatusCode::FORBIDDEN);

	// Codes the provider does not recognize
	let attempt = start_login(&mut service, protocol::oidc_start(PROVIDER)).await;
	let response = service.fetch(&callback(&attempt, "made-up-code")).await;
	assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

	// Errors reported by the provider
	let attempt = start_login(&mut service, protocol::oidc_start(PROVIDER)).await;
	let parameters = [("error", "access_denied"), ("state", &attempt.state)];
	let mut request = protocol::oidc_callback(PROVIDER, &parameters);
	request
		.headers_mut()
		.insert(header::COOKIE, attempt.cookie.parse().unwrap());
	let response = service.fetch(&request).await;
	assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

	// Missing parameters
	let response = service.fetch(&protocol::oidc_callback(PROVIDER, &[])).await;
	assert_eq!(response.status(), StatusCode::BAD_REQUEST);

	// None of these created a user
	service.login_admin().await;
	let request = protocol::list_users();
	let response = service.fetch_json::<_, Vec<dto::User>>(&request).await;
	assert_eq!(response.body().len(), 1);
}

#[tokio::test]
async fn login_returns_to_requested_location() {
	let (mut service, provider) = make_service(&test_name!(), "").await;

	let deep_link = "/#/songs/Musik%2Fdire-straits%2F02_money-for-nothing.flac";
	let request = protocol::oidc_start_returning_to(PROVIDER, deep_link);
	let attempt = start_login(&mut service, request).await;
	// The location stays on this server, and is not disclosed to the provider
	assert!(!attempt.authorization_url.contains("money-for-nothing"));

	let code = provider.authorize(&attempt.authorization_url);
	let response = service.fetch_bytes(&callback(&attempt, &code)).await;
	assert_eq!(response.status(), StatusCode::OK);
	assert_eq!(read_destination(response.body()), deep_link);
	assert_eq!(read_session(response.body())["name"], "alice");
}

#[tokio::test]
async fn login_does_not_redirect_to_other_sites() {
	let (mut service, provider) = make_service(&test_name!(), "").await;

	let other_sites = [
		"https://evil.example.com/",
		"//evil.example.com/",
		"/\\evil.example.com/",
		"/\t/evil.example.com/",
		"javascript:alert(1)",
		"evil.example.com",
	];
	for return_to in other_sites {
		let request = protocol::oidc_start_returning_to(PROVIDER, return_to);
		let attempt = start_login(&mut service, request).await;
		let code = provider.authorize(&attempt.authorization_url);
		let response = service.fetch_bytes(&callback(&attempt, &code)).await;
		// Signing in still works, and leads to the start page of the web client
		assert_eq!(response.status(), StatusCode::OK, "{return_to:?}");
		assert_eq!(read_destination(response.body()), "/", "{return_to:?}");
	}
}
