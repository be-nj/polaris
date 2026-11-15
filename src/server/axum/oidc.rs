//! Endpoints for signing in through OpenID Connect identity providers.
//!
//! A sign-in is a round trip made by the browser of the user: `start` sends it to the
//! identity provider, which sends it back to `callback`. The callback responds with a page
//! which hands the resulting session over to the web client.

use axum::{
	extract::{Path, Query, State},
	response::{Html, IntoResponse, Response},
	Json,
};
use http::{header, HeaderMap, HeaderValue, StatusCode};
use log::error;
use serde::Deserialize;
use serde_json::json;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
	app::{self, oidc, App},
	server::{dto, error::APIError},
};

const STATE_COOKIE: &str = "polaris_oidc_state";
const STATE_COOKIE_LIFETIME_SECONDS: u32 = 600;

pub fn router() -> OpenApiRouter<App> {
	OpenApiRouter::new()
		.routes(routes!(get_providers))
		.routes(routes!(get_start))
		.routes(routes!(get_callback))
}

fn login_path(provider_name: &str) -> String {
	format!("/api/oidc/{provider_name}/start")
}

/// Address this server is reached at by browsers. This is determined from the request
/// (accounting for reverse proxies) unless specified in the server configuration.
async fn get_public_url(app: &App, headers: &HeaderMap) -> String {
	if let Some(public_url) = app.config_manager.get_public_url().await {
		return public_url;
	}

	let read_header = |name: &str| {
		headers
			.get(name)
			.and_then(|v| v.to_str().ok())
			.and_then(|v| v.split(',').next())
			.map(str::trim)
			.filter(|v| !v.is_empty())
	};
	let scheme = read_header("x-forwarded-proto").unwrap_or("http");
	let host = read_header("x-forwarded-host")
		.or_else(|| read_header("host"))
		.unwrap_or("localhost");
	format!("{scheme}://{host}")
}

fn make_state_cookie(value: &str, max_age: u32, is_secure: bool) -> HeaderValue {
	let secure = if is_secure { "; Secure" } else { "" };
	let cookie = format!(
		"{STATE_COOKIE}={value}; Path=/api/oidc; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}"
	);
	HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn read_state_cookie(headers: &HeaderMap) -> Option<&str> {
	headers
		.get_all(header::COOKIE)
		.iter()
		.filter_map(|value| value.to_str().ok())
		.flat_map(|value| value.split(';'))
		.filter_map(|cookie| cookie.trim().split_once('='))
		.find(|(name, _)| *name == STATE_COOKIE)
		.map(|(_, value)| value)
}

#[utoipa::path(
	get,
	path = "/oidc/providers",
	tag = "User Management",
	description = "Lists the single sign-on providers users can sign in with, as an alternative to the `/auth` endpoint.",
	responses(
		(status = 200, body = Vec<dto::OidcProvider>),
	),
)]
async fn get_providers(State(app): State<App>) -> Json<Vec<dto::OidcProvider>> {
	let providers = app
		.config_manager
		.get_oidc_providers()
		.await
		.into_iter()
		.map(|p| dto::OidcProvider {
			login_url: login_path(&p.name),
			display_name: p.display_name.unwrap_or_else(|| p.name.clone()),
			name: p.name,
		})
		.collect();
	Json(providers)
}

#[utoipa::path(
	get,
	path = "/oidc/{provider}/start",
	tag = "User Management",
	description = "Starts signing in with a single sign-on provider.\n\nThis endpoint is meant to be navigated to by a web browser. It redirects to the sign-in page of the provider, which eventually redirects to the `callback` endpoint.",
	params(("provider", example = "authelia")),
	responses(
		(status = 303, description = "Redirection to the sign-in page of the provider"),
		(status = 404, description = "Unknown provider"),
		(status = 502, description = "The provider could not be reached"),
	),
)]
async fn get_start(
	State(app): State<App>,
	Path(provider): Path<String>,
	headers: HeaderMap,
) -> Result<Response, APIError> {
	let public_url = get_public_url(&app, &headers).await;
	let redirect_uri = format!("{public_url}/api/oidc/{provider}/callback");

	let start = match app.oidc_manager.begin_login(&provider, redirect_uri).await {
		Ok(start) => start,
		Err(e) => {
			error!("Could not start OpenID Connect sign-in with `{provider}`: {e}");
			return Err(e.into());
		}
	};

	let location = HeaderValue::from_str(&start.authorization_url)
		.map_err(|_| APIError::OidcProviderFailure)?;
	// The cookie ties the sign-in to the browser which started it. Without it, an attacker
	// could trick someone into completing a sign-in the attacker started (login CSRF).
	let cookie = make_state_cookie(
		&start.state,
		STATE_COOKIE_LIFETIME_SECONDS,
		public_url.starts_with("https://"),
	);

	Ok((
		StatusCode::SEE_OTHER,
		[
			(header::LOCATION, location),
			(header::SET_COOKIE, cookie),
			(header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
		],
	)
		.into_response())
}

#[derive(Deserialize)]
struct CallbackParameters {
	code: Option<String>,
	state: Option<String>,
	error: Option<String>,
	error_description: Option<String>,
}

#[utoipa::path(
	get,
	path = "/oidc/{provider}/callback",
	tag = "User Management",
	description = "Completes signing in with a single sign-on provider.\n\nThis is where providers send users back to after they have signed in, and must be registered with the provider as the redirect URI (`https://your-polaris-server/api/oidc/PROVIDER/callback`). The response is a web page which opens the Polaris web client, signed in.",
	params(
		("provider", example = "authelia"),
		("code" = Option<String>, Query, description = "Authorization code issued by the provider"),
		("state" = Option<String>, Query, description = "Identifies the sign-in attempt"),
	),
	responses(
		(status = 200, content_type = "text/html", description = "Page which opens the web client"),
		(status = 400, content_type = "text/html", description = "The sign-in attempt is invalid or has expired"),
		(status = 403, content_type = "text/html", description = "The user is not allowed to use this server"),
		(status = 502, content_type = "text/html", description = "The provider could not be reached"),
	),
)]
async fn get_callback(
	State(app): State<App>,
	Path(provider): Path<String>,
	Query(parameters): Query<CallbackParameters>,
	headers: HeaderMap,
) -> Response {
	let result = complete_login(&app, &provider, parameters, &headers).await;
	let mut response = match result {
		Ok(login) => login_page(&login),
		Err(e) => error_page(e),
	};

	let public_url = get_public_url(&app, &headers).await;
	let headers = response.headers_mut();
	headers.insert(
		header::SET_COOKIE,
		make_state_cookie("", 0, public_url.starts_with("https://")),
	);
	headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
	headers.insert(
		header::REFERRER_POLICY,
		HeaderValue::from_static("no-referrer"),
	);
	response
}

async fn complete_login(
	app: &App,
	provider: &str,
	parameters: CallbackParameters,
	headers: &HeaderMap,
) -> Result<oidc::Login, APIError> {
	if let Some(provider_error) = parameters.error {
		let description = parameters.error_description.unwrap_or_default();
		error!(
			"OpenID Connect provider `{provider}` refused sign-in: {provider_error} {description}"
		);
		return Err(APIError::OidcProviderFailure);
	}

	let (Some(code), Some(state)) = (parameters.code, parameters.state) else {
		return Err(APIError::OidcLoginExpired);
	};

	if read_state_cookie(headers) != Some(state.as_str()) {
		return Err(APIError::OidcLoginExpired);
	}

	match app
		.oidc_manager
		.complete_login(provider, &state, &code)
		.await
	{
		Ok(login) => Ok(login),
		Err(e) => {
			match &e {
				app::Error::OidcLoginExpired | app::Error::OidcAccessDenied(_) => (),
				e => error!("OpenID Connect sign-in with `{provider}` failed: {e}"),
			}
			Err(e.into())
		}
	}
}

fn escape_html(text: &str) -> String {
	text.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
		.replace('"', "&quot;")
}

fn page(title: &str, body: &str) -> Html<String> {
	Html(format!(
		r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
body {{ font-family: system-ui, sans-serif; max-width: 32rem; margin: 20vh auto 0; padding: 0 1rem; text-align: center; color: #222; background: #fafafa; }}
@media (prefers-color-scheme: dark) {{ body {{ color: #eee; background: #18181b; }} a {{ color: #8ab4f8; }} }}
</style>
</head>
<body>
{body}
</body>
</html>
"#
	))
}

/// Page which stores the session where the Polaris web client expects to find it, then
/// opens the web client.
fn login_page(login: &oidc::Login) -> Response {
	login_page_html(login).into_response()
}

fn login_page_html(login: &oidc::Login) -> Html<String> {
	let session = json!({
		"name": login.username,
		"authToken": login.token.0,
		"isAdmin": login.is_admin,
	});
	// Prevents the session data from being interpreted as markup
	let session = session
		.to_string()
		.replace('<', "\\u003c")
		.replace('>', "\\u003e")
		.replace('&', "\\u0026");

	let body = format!(
		r#"<p>Signing in…</p>
<noscript><p>JavaScript is required to complete signing in.</p></noscript>
<script>
localStorage.setItem("current_user", JSON.stringify({session}));
window.location.replace("/");
</script>"#
	);
	page("Polaris", &body)
}

fn error_page(error: APIError) -> Response {
	let message = escape_html(&error.to_string());
	let status = error.into_response().status();
	let body = format!(
		r#"<h1>Could not sign in</h1>
<p>{message}</p>
<p><a href="/">Back to Polaris</a></p>"#
	);
	(status, page("Polaris - Could not sign in", &body)).into_response()
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn reads_state_cookie() {
		let mut headers = HeaderMap::new();
		assert_eq!(read_state_cookie(&headers), None);
		headers.append(header::COOKIE, "theme=dark".parse().unwrap());
		assert_eq!(read_state_cookie(&headers), None);
		headers.append(
			header::COOKIE,
			"a=b; polaris_oidc_state=abc-123; c=d".parse().unwrap(),
		);
		assert_eq!(read_state_cookie(&headers), Some("abc-123"));
	}

	#[test]
	fn login_page_cannot_be_broken_out_of() {
		let login = oidc::Login {
			username: "</script><script>alert(1)</script>".to_owned(),
			token: app::auth::Token("token".to_owned()),
			is_admin: false,
		};
		let Html(html) = login_page_html(&login);
		assert_eq!(html.matches("<script>").count(), 1);
		assert_eq!(html.matches("</script>").count(), 1);
		assert!(html.contains(r#""name":"\u003c/script\u003e\u003cscript\u003ealert(1)"#));
	}
}
