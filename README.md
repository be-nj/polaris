<div align="center">
  <h1><img src="res/readme/logo.png?raw=true"/></h1>

  [![Actions Status](https://github.com/agersant/polaris/workflows/Build/badge.svg)](https://github.com/agersant/polaris/actions)
  [![codecov](https://codecov.io/github/agersant/polaris/graph/badge.svg?token=EQqCmBEf2T)](https://codecov.io/github/agersant/polaris)
  [![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE-MIT)

  ![Polaris Web UI](res/readme/web_ui.png?raw=true "Polaris Web UI")
</div>

# About this fork

This is a fork of [agersant/polaris](https://github.com/agersant/polaris), which does not accept code contributions. It follows upstream and adds the features below. Everything else on this page describes Polaris as a whole.

## What this fork adds

| Feature | Description | Documentation |
| --- | --- | --- |
| 📻 **Subsonic API** | Use third-party Subsonic / OpenSubsonic clients (mobile apps, desktop players, web players) with your Polaris server. | [Using Subsonic clients](docs/SUBSONIC.md) |
| 🎚️ **Transcoding** | Songs can be converted to `mp3`, `opus` or `aac` at a lower bitrate while streaming, to save mobile data or to play formats a device does not support. Playback starts immediately, and converted songs are cached. | [Transcoding](docs/TRANSCODING.md) |
| 🔑 **Single sign-on** | Sign in through an OpenID Connect provider such as Authelia, authentik or Keycloak. Users are created on first sign-in; access and administrator rights can be driven by groups. | [Single sign-on](docs/SSO.md) |
| 🚫 **Scan exclusions** | Keep folders and files out of your collection with a pattern, for example Synology `@eaDir` folders. Hidden files and folders (like `.zfs` snapshots) are skipped by default. | [Configuration](docs/CONFIGURATION.md) |
| 🐳 **Docker image** | An image with the server, the web client and FFmpeg, plus a ready-to-use `docker-compose.yml`. The login page of its web client offers a button for each single sign-on provider. | [Running Polaris with Docker](docs/DOCKER.md) |

These correspond to the upstream feature requests [#229](https://github.com/agersant/polaris/issues/229), [#119](https://github.com/agersant/polaris/issues/119), [#230](https://github.com/agersant/polaris/issues/230) and [#189](https://github.com/agersant/polaris/issues/189), and to part of [#77](https://github.com/agersant/polaris/issues/77) (cross-origin requests are accepted by the Subsonic API).

## Quick start with Docker

```bash
git clone https://github.com/be-nj/polaris.git
cd polaris
# Edit docker-compose.yml to point it at your music collection, then:
docker compose up -d
```

Then open [http://localhost:5050](http://localhost:5050). See [Running Polaris with Docker](docs/DOCKER.md) for details.

## Differences in behavior

- Hidden files and folders (names starting with `.`) are no longer indexed, unless `scan_exclude_pattern = ""` is set in the configuration file.
- Subsonic clients must use plain password ("legacy") authentication or an API key. Polaris only stores password hashes, which rules out the default token authentication of the Subsonic protocol.

## Testing

Every feature of this fork is covered by automated tests that run on each push, on Linux and Windows:

- The test suite grew from the 167 tests inherited from upstream to more than 230. It exercises the new features through the HTTP API, including error cases.
- Single sign-on is tested against an identity provider built into the tests, with attempts to replay sign-ins, tamper with identity tokens, reuse authorization codes and hijack a sign-in from another browser.
- Transcoding tests run real FFmpeg conversions, concurrent requests for the same song, seeking and cache eviction.

Problems found and fixed along the way include a race condition that made a transcoded stream fail when its conversion finished at the wrong moment, sign-ins being rejected with Keycloak because it only lists roles in access tokens, and a wrong setting name (`ddns_update_url`) in the configuration documentation.

# About

Polaris is a self-hosted music streaming server, to enjoy your music collection from any computer or mobile device. It is free and open-source software, without any kind of premium version.

The goals of this project are:
- 🔥 Exceptional performance and responsiveness
- 📚️ First-class support for large music collections (100,000+ songs)
- 📦️ Ease of installation, deployment and maintenance
- ✨ Beautiful user interface

# Features

- 🖥️ Runs on Windows, Linux, BSD, or through Docker
- 🔊 Support for `flac`, `mp3`, `mp4`, `mpc`, `ogg`, `opus`, `ape`, `wav` and `aiff` files
- 🌈 Dark mode variants and customizable color palette
- 💿️ Browse your music by album, artist or genre
- 📂 Browse your music as a file tree
- 🌊 Song audio-waveform visualization
- 🏷️ Support for multi-value fields in song metadata (eg. multiple artists per song)
- 🔍️ Powerful search functionality with per-field queries
- ⚙️ Plain-text configuration also editable with built-in UI
- 👥 Setup multiple users, each with their own playlists
- 📱 Listen to your music on the go:
  - Polaris Android ([Google Play Store](https://play.google.com/store/apps/details?id=agersant.polaris) · [F-Droid](https://f-droid.org/packages/agersant.polaris/) · [Repository](https://github.com/agersant/polaris-android))
  - Polarios ([App Store](https://apps.apple.com/app/polarios/id1662366309) · [Repository](https://gitlab.com/elise/Polarios)) [third-party]
  - Polarity ([Demo](https://www.hackster.io/hardcoder/polarity-a-music-player-for-polaris-8cd4eb) · [Repository](https://github.com/p-dial8891/Polarity)) [third-party]

# Installation

[Installation documentation](docs/SETUP.md)

[Running Polaris with Docker](docs/DOCKER.md)

[Streaming from remote devices](docs/DDNS.md)

[![Packaging status](https://repology.org/badge/vertical-allrepos/polaris-streaming.svg?columns=3)](https://repology.org/project/polaris-streaming/versions)

# Documentation

- 📒 [Changelog](CHANGELOG.md)
- 🔧 [Configuration](docs/CONFIGURATION.md)
- 📻 [Using Subsonic clients](docs/SUBSONIC.md)
- 🎚️ [Transcoding](docs/TRANSCODING.md)
- 🔑 [Single sign-on](docs/SSO.md)
- 👷 [Contribute to Polaris](CONTRIBUTING.md)
- 🛟 [Maintenance Runbooks](docs/MAINTENANCE.md)

The Polaris server API is documented via OpenAPI, every installation of Polaris features this interactive documentation. To access it, open http://localhost:5050/api-docs/ in your browser on the machine running Polaris.
