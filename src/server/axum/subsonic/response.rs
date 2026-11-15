use axum::response::{IntoResponse, Response};
use http::header;
use serde_json::{json, Map, Value};

use crate::app;

/// Version of the Subsonic REST protocol implemented by this server.
pub const PROTOCOL_VERSION: &str = "1.16.1";

pub type Payload = Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Format {
	Xml,
	Json,
	Jsonp(String),
}

#[derive(Debug)]
pub struct Error {
	pub code: u32,
	pub message: String,
}

impl Error {
	pub fn generic(message: &str) -> Self {
		Self::new(0, message)
	}

	pub fn missing_parameter(name: &str) -> Self {
		Self::new(10, &format!("Required parameter is missing: {name}"))
	}

	pub fn wrong_credentials() -> Self {
		Self::new(40, "Wrong username or password.")
	}

	pub fn token_auth_not_supported() -> Self {
		Self::new(
			41,
			"Token authentication is not supported by this server. Enable legacy (plain password) authentication or use an API key.",
		)
	}

	pub fn conflicting_auth() -> Self {
		Self::new(
			43,
			"Multiple conflicting authentication mechanisms provided.",
		)
	}

	pub fn invalid_api_key() -> Self {
		Self::new(44, "Invalid API key.")
	}

	pub fn unauthorized() -> Self {
		Self::new(50, "User is not authorized for the given operation.")
	}

	pub fn not_found() -> Self {
		Self::new(70, "The requested data was not found.")
	}

	pub fn not_supported(what: &str) -> Self {
		Self::new(0, &format!("{what} is not supported by this server."))
	}

	fn new(code: u32, message: &str) -> Self {
		Self {
			code,
			message: message.to_owned(),
		}
	}
}

impl From<app::Error> for Error {
	fn from(error: app::Error) -> Self {
		use app::Error::*;
		match error {
			DirectoryNotFound(_)
			| ArtistNotFound
			| AlbumNotFound
			| GenreNotFound
			| SongNotFound
			| PlaylistNotFound
			| UserNotFound
			| CouldNotMapToRealPath(_)
			| EmbeddedArtworkNotFound(_) => Self::not_found(),
			e => Self::generic(&e.to_string()),
		}
	}
}

fn envelope(status: &str, payload: Payload) -> Value {
	let mut body = Map::new();
	body.insert("status".to_owned(), json!(status));
	body.insert("version".to_owned(), json!(PROTOCOL_VERSION));
	body.insert("type".to_owned(), json!("polaris"));
	body.insert("serverVersion".to_owned(), json!(env!("CARGO_PKG_VERSION")));
	body.insert("openSubsonic".to_owned(), json!(true));
	body.extend(payload);
	let mut body = Value::Object(body);
	strip_nulls(&mut body);
	body
}

fn strip_nulls(value: &mut Value) {
	match value {
		Value::Object(members) => {
			members.retain(|_, member| !member.is_null());
			members.values_mut().for_each(strip_nulls);
		}
		Value::Array(items) => items.iter_mut().for_each(strip_nulls),
		_ => (),
	}
}

pub fn success(format: &Format, payload: Payload) -> Response {
	render(format, envelope("ok", payload))
}

pub fn failure(format: &Format, error: Error) -> Response {
	let mut payload = Map::new();
	payload.insert(
		"error".to_owned(),
		json!({ "code": error.code, "message": error.message }),
	);
	render(format, envelope("failed", payload))
}

fn render(format: &Format, body: Value) -> Response {
	match format {
		Format::Xml => {
			let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
			write_xml_element(&mut xml, "subsonic-response", &body, true);
			(
				[(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
				xml,
			)
				.into_response()
		}
		Format::Json => (
			[(header::CONTENT_TYPE, "application/json; charset=utf-8")],
			json!({ "subsonic-response": body }).to_string(),
		)
			.into_response(),
		Format::Jsonp(callback) => (
			[(
				header::CONTENT_TYPE,
				"application/javascript; charset=utf-8",
			)],
			format!("{callback}({});", json!({ "subsonic-response": body })),
		)
			.into_response(),
	}
}

/// Subsonic XML is a mechanical transformation of the JSON form: scalar members become
/// attributes, objects become child elements and arrays become repeated child elements.
/// A scalar member named `value` is the text content of its element.
fn write_xml_element(out: &mut String, name: &str, value: &Value, is_root: bool) {
	match value {
		Value::Null => (),
		Value::Array(items) => {
			for item in items {
				write_xml_element(out, name, item, false);
			}
		}
		Value::Object(members) => {
			out.push('<');
			out.push_str(name);
			if is_root {
				out.push_str(r#" xmlns="http://subsonic.org/restapi""#);
			}

			let mut text = None;
			let mut children = Vec::new();
			for (key, member) in members {
				match member {
					Value::Null => (),
					Value::Array(_) | Value::Object(_) => children.push((key, member)),
					scalar if key == "value" => text = Some(scalar_to_string(scalar)),
					scalar => {
						out.push(' ');
						out.push_str(key);
						out.push_str("=\"");
						escape_xml(out, &scalar_to_string(scalar));
						out.push('"');
					}
				}
			}

			if text.is_none() && children.is_empty() {
				out.push_str("/>");
				return;
			}

			out.push('>');
			if let Some(text) = text {
				escape_xml(out, &text);
			}
			for (key, child) in children {
				write_xml_element(out, key, child, false);
			}
			out.push_str("</");
			out.push_str(name);
			out.push('>');
		}
		scalar => {
			out.push('<');
			out.push_str(name);
			out.push('>');
			escape_xml(out, &scalar_to_string(scalar));
			out.push_str("</");
			out.push_str(name);
			out.push('>');
		}
	}
}

fn scalar_to_string(value: &Value) -> String {
	match value {
		Value::String(s) => s.clone(),
		other => other.to_string(),
	}
}

fn escape_xml(out: &mut String, text: &str) {
	for c in text.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			'\'' => out.push_str("&apos;"),
			// Characters which are not allowed in XML 1.0 documents
			c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => (),
			c => out.push(c),
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn xml_mirrors_json_structure() {
		let body = json!({
			"status": "ok",
			"artists": {
				"ignoredArticles": "",
				"index": [
					{ "name": "A", "artist": [{ "id": "1", "name": "A & <B>" }, { "id": "2", "name": "\"C\"" }] },
				],
			},
			"genres": { "genre": [{ "value": "Rock", "songCount": 3 }] },
			"folder": [0, 1],
			"missing": null,
		});
		let mut xml = String::new();
		write_xml_element(&mut xml, "subsonic-response", &body, true);
		assert_eq!(
			xml,
			concat!(
				r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok">"#,
				r#"<artists ignoredArticles="">"#,
				r#"<index name="A">"#,
				r#"<artist id="1" name="A &amp; &lt;B&gt;"/>"#,
				r#"<artist id="2" name="&quot;C&quot;"/>"#,
				r#"</index>"#,
				r#"</artists>"#,
				r#"<folder>0</folder><folder>1</folder>"#,
				r#"<genres><genre songCount="3">Rock</genre></genres>"#,
				r#"</subsonic-response>"#,
			)
		);
	}
}
