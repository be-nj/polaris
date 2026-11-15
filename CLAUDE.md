# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Polaris is a self-hosted music streaming server written in Rust (single binary crate, `src/main.rs`). This repository contains only the server; the web client lives in a separate repository ([polaris-web](https://github.com/agersant/polaris-web)) and is served as static files from a directory at runtime.

Upstream (`agersant/polaris`) does not accept code contributions, and `CONTRIBUTING.md` states that contributions making any use of AI/LLMs will be rejected. Do not open issues or pull requests against upstream.

## Commands

```bash
cargo build                      # debug build
cargo test                       # all tests (CI runs: cargo test --release, with and without --features ui)
cargo test server::test::collection::random_golden_path   # single test, by module path
cargo test app::index::          # all tests in a module subtree
cargo fmt                        # formatting; hard tabs (.rustfmt.toml)
cargo llvm-cov --codecov --output-path codecov.json        # coverage, as run by CI
```

There is no lint step in CI (no clippy gate). Toolchain is `stable`, pinned via `rust-toolchain.toml`; a Nix flake (`flake.nix`, `.envrc`) provides an optional dev shell.

Running the server:

```bash
cargo run -- -f -w web -c test-data/config.toml
```

- `-f` keeps the process in the foreground. Without it, on Linux Polaris daemonizes and logs to `polaris.log` instead of the terminal.
- `-w DIR` is the web client directory (unzip a polaris-web release there). `-c FILE` is the TOML config. `--data DIR` and `--cache DIR` relocate runtime state. `-p PORT` (default 5050), `--log-level LEVEL`, `--show-paths`.
- By default all runtime state is written to the current directory: `auth.secret`, `collection.index`, `polaris.ndb`, `polaris.log`, `peaks/`, `thumbnails/`. These are gitignored, as is `TestConfig.toml` for local config.
- The API is self-documented with OpenAPI at `http://localhost:5050/api-docs/`.

The `ui` cargo feature only has an effect on Windows (system tray UI in `src/ui/windows.rs`); elsewhere `src/ui/headless.rs` is used. Default paths can be overridden at compile time with `POLARIS_{CACHE,CONFIG,DATA,LOG,PID,WEB}_DIR` environment variables (`src/paths.rs`), which is how distro packages are built (`res/unix/Makefile`).

## Architecture

Two layers: `src/app` (domain logic, no HTTP) and `src/server` (HTTP API on axum). `main.rs` parses CLI options, resolves `Paths`, builds an `app::App`, queues an initial scan, then launches the server.

### `app::App` and its managers

`App` (`src/app.rs`) is a cloneable bundle of managers, each a cheap handle (`Arc` inside) around one concern. The construction order in `App::new` shows the dependency graph:

- `config::Manager` — owns the TOML config file (mounts, users, album art pattern, DDNS URL). `config/storage.rs` holds the on-disk serde types; `config.rs`/`mounts.rs`/`user.rs` hold the validated in-memory types, converted with `TryFrom`/`From`. The file is watched: external edits are reloaded, and API changes are written back. Users and their password hashes live here, so this manager also handles login and token authentication (`auth.rs` has the primitives; tokens are Branca-encrypted with the secret from `auth.secret`).
- `index::Manager` — the in-memory music collection, described below.
- `scanner::Scanner` — walks mount directories and produces a new index. Depends on config and index managers.
- `ndb::Manager` — a `native_db` embedded database (`polaris.ndb`). Currently only playlists are stored in it; models are versioned (`playlist::v1::PlaylistModel`) and registered in `ndb.rs`.
- `playlist::Manager`, `peaks::Manager` (audio waveform data), `thumbnail::Manager`, `ddns::Manager`. Peaks and thumbnails are generated on demand and cached on disk under the cache directory.
- `oidc::Manager` — single sign-on with OpenID Connect providers declared in the config file (authorization code flow with PKCE; user docs in `docs/SSO.md`). `begin_login` produces the provider URL and a state, `complete_login` exchanges the code, validates the ID token claims, merges UserInfo claims, maps roles and signs the user in through `config::Manager::login_external` (creating the user if needed). The HTTP side is `server/axum/oidc.rs`: `/api/oidc/{provider}/start` redirects and sets a state cookie, `/api/oidc/{provider}/callback` returns an HTML page which writes the session to the `current_user` key of `localStorage`, where polaris-web looks for it, then opens the web client. `start` accepts a `return_to` location which is kept with the pending login and opened by that page instead; `oidc::sanitize_return_to` only lets through paths within this server, to prevent open redirects. Apps (see be-nj/polaris-android) call `start` with a `code_challenge` instead: the callback page then opens a `polaris://sso?code=…` link rather than signing the browser in, and the app redeems the single-use code along with its code verifier at `POST /api/oidc/token` (`Manager::begin_app_login` / `redeem_app_code`). `oidc::mock` is an in-process identity provider used by tests.
- `transcode::Manager` — on-demand transcoding through an external `ffmpeg` process (path from `POLARIS_FFMPEG_PATH`, disabled when not found). Output is written to a size-limited disk cache and can be read while it is still being produced; concurrent requests for the same output share one job. `server/axum/media.rs` turns this into HTTP responses for both the Polaris and Subsonic APIs. Tests needing ffmpeg skip themselves when it is not installed.
- `formats.rs` — tag reading per audio format, dispatched by file extension, producing a common `SongMetadata`.

All domain errors are variants of the single `app::Error` enum.

### Collection index

The collection is not stored in a database. `index::Index` is an immutable in-memory structure made of four parts (`src/app/index/`): `dictionary` (string interning with `lasso2`, plus canonicalisation and collation sort keys), `browser` (file-tree view), `collection` (artists, albums, genres, songs) and `search` (query parser in `query.rs` using `chumsky`, plus the search indices). `storage.rs` defines the compact interned representations shared by these.

An index is never mutated in place. `index::Builder` accumulates `scanner::Directory` and `scanner::Song` values and `build()` produces a whole new `Index`, which `index::Manager::replace_index` swaps in behind a `std::sync::RwLock`. The lock is deliberately not a tokio lock: every read method on `index::Manager` wraps its work in `spawn_blocking`, since index queries are CPU-bound. The built index is serialized with `bitcode` to `collection.index` and restored at startup, so changing any serialized index type invalidates existing index files (a failed restore is logged and the next scan rebuilds it).

### Scanning

`Scanner` runs two background tasks. One waits for a trigger (config change, filesystem change under a mount, or an explicit request), aborts any scan in progress, debounces for 2 seconds, then signals the other task, which runs `run_scan`. A scan traverses mounts on a rayon thread pool, streams results over channels into an `index::Builder`, persists the result and swaps it in. When the collection was empty at the start of the scan, partial indices are promoted every few seconds so a first-time scan of a large library becomes browsable progressively.

The scan also decides the artwork of each song (`process_directory`): a picture embedded in the file, else an image of the same directory matching `album_art_pattern` (default in `config::DEFAULT_ALBUM_ART_PATTERN`), else that of the parent directory for disc directories (`DISC_DIRECTORY_REGEX`, eg. `CD1`). `thumbnail.rs` extracts embedded pictures per format; user docs are in `docs/CONFIGURATION.md`.

Pictures of artists work differently, since artists come from tags rather than from directories. Each song carries the image matching `artist_art_pattern` found next to the directory of its album (`Song::artist_artwork`), and `collection::Builder::assign_artist_artwork` gives such an image to an artist when most of the songs it was found next to are theirs. Index files start with `INDEX_FILE_HEADER`, which must change along with any serialized index type.

`artist_image::Manager` serves the picture of an artist wherever it comes from (`/api/artist/{name}/thumbnail`, and Subsonic `getCoverArt` with the ID of an artist): the one of the collection, or failing that, and only when `fetch_artist_images` is set, one looked up in the Deezer catalog. It takes the namesake with the largest following and keeps the download under `artist_images/` in the cache directory (a `.missing` marker remembers artists without a picture for 30 days). Tests replace the catalog with an in-process HTTP server.

Paths exposed through the API are *virtual paths*: the first component is a mount name, mapped to a real directory by `config::Manager::resolve_virtual_path` / `virtualize_path`. The index only stores virtual paths.

### HTTP server

`src/server.rs` holds the framework-independent parts: API version constants, DTOs (`dto/v8.rs`), `APIError`, OpenAPI metadata (`doc.rs`). `src/server/axum/` holds the axum implementation; `axum.rs` builds the router (API under `/api`, docs under `/api-docs`, static web client as fallback) and implements `FromRef<App>` for each manager so handlers take only the managers they need as `State<...>`.

All endpoints are in `axum/api.rs`, each annotated with `#[utoipa::path]` and registered in `router()` with `routes!`. Conventions for handlers:

- Authentication is an extractor argument: `Auth` for any logged-in user, `AdminRights` for admin-only endpoints (`axum/auth.rs`). `AdminRights` also passes while no user exists yet, which is what allows initial setup. Tokens are accepted as a bearer header or an `auth_token` query parameter.
- Endpoints whose response shape is versioned take an `APIMajorVersion` extractor, read from the `Accept-Version` request header (`axum/version.rs`). Only V8 exists; `API_MAJOR_VERSION`/`API_MINOR_VERSION` in `server.rs` must be bumped when the API changes.
- Handlers return `Result<_, APIError>`. `app::Error` converts into `APIError` (`server/error.rs`), and `axum/error.rs` maps each `APIError` variant to a status code, so a new `app::Error` variant needs handling in both.
- Domain types are never serialized directly; convert to and from the `dto` types.

### Subsonic API

`src/server/axum/subsonic.rs` implements the Subsonic REST API under `/rest/{method}` (documented for users in `docs/SUBSONIC.md`). It is a plain axum router outside of the OpenAPI document, with a single handler which parses parameters from the query string or a form body, authenticates, then dispatches on the method name. Endpoints build `serde_json` values; `subsonic/response.rs` wraps them in the response envelope and renders JSON or XML (XML is derived mechanically from the JSON shape). `subsonic/id.rs` derives the opaque string IDs Subsonic requires from names and virtual paths, and `subsonic/model.rs` converts index types into Subsonic entities. Subsonic errors are reported inside the envelope with HTTP status 200. Tests are in `src/server/test/subsonic.rs`.

## Tests

Tests are inline `#[cfg(test)]` modules next to the code, plus shared harnesses:

- `src/test.rs` — `test_name!()` derives a unique name from file and line, and `prepare_test_directory` creates a fresh `test-output/<name>/` directory for that test's config, index and database files. Tests run in parallel, so each must use its own directory via `test_name!()`.
- `src/app/test.rs` — `ContextBuilder` builds the app-layer managers with given users and mounts, without HTTP.
- `src/server/test.rs` — API tests, grouped by area in `src/server/test/*.rs`. They are written against the `TestService` trait (helpers such as `complete_initial_setup`, `login`, `login_admin`, `index`) and request builders in `test/protocol.rs`; the axum-backed implementation is `server/axum/test.rs`. A new endpoint needs a request builder in `protocol.rs` and tests in the matching area file.

Fixtures are in `test-data/` (`small-collection` is the standard mount; other directories cover formats, artwork, multi-value tags and playlist import/export). Paths to fixtures are relative, so tests must run from the repository root.

## End-to-end tests

`e2e/` holds Playwright tests which run the Docker image next to real Keycloak, Authelia and authentik containers and drive it with a real browser and the `subsonic-api` client library (see `e2e/README.md`). `./e2e/run.sh` does everything: prepares `e2e/runtime` (certificates, config copy), starts the Compose services, waits for them, runs the tests and tears down. Set `POLARIS_IMAGE` to test an existing image instead of building one. Services are addressed as `*.test` host names, which the test browser maps to localhost. These run in CI through `.github/workflows/e2e.yml`. A new identity provider needs a Compose service, an `[[oidc_providers]]` entry in `e2e/polaris.toml`, a readiness check in `run.sh` and a `signIn` implementation in `tests/sso.spec.ts`.

## Docker image

`Dockerfile` builds the image published to `ghcr.io/be-nj/polaris` by `.github/workflows/docker.yml` (user docs in `docs/DOCKER.md`, example `docker-compose.yml` at the repository root). It has three stages: the web client (a pinned polaris-web tag plus `docker/polaris-web.patch`, which adds single sign-on buttons to the login page, makes the web client return to the page that was requested before signing in, and shows pictures of artists on their page and in lists of artists), the server (cross-compiled when the target architecture differs from the build machine) and a Debian runtime with FFmpeg. `docker/entrypoint.sh` prepares `/config`, `/data` and `/cache` and drops root privileges to `PUID`/`PGID`. `.dockerignore` is an allowlist: new files needed by the build must be added to it.

## Releases and changelog

User-visible changes are recorded in `CHANGELOG.md` under "Unreleased Changes". Releases are cut by the manually triggered "Make Release" GitHub workflow (`docs/MAINTENANCE.md`); `Cargo.toml` keeps version `0.0.0` and the real version is injected at release time.
