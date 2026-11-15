# Running Polaris with Docker

The Polaris Docker image contains everything needed to run Polaris: the server, the web client, and FFmpeg for [transcoding](TRANSCODING.md).

## Quick start

1. Download [`docker-compose.yml`](../docker-compose.yml) into an empty folder.
2. Edit it to replace `/path/to/your/music` with the location of your music collection.
3. Start Polaris:

   ```bash
   docker compose up -d
   ```

4. Open [http://localhost:5050](http://localhost:5050) in a web browser. On first launch, Polaris asks where your music is and which user to create. Enter `/music` as the location of your music.

Polaris stores its files in three folders next to `docker-compose.yml`:

| Folder     | Content                                                                                             |
| ---------- | --------------------------------------------------------------------------------------------------- |
| `./config` | `polaris.toml`, the [configuration file](CONFIGURATION.md). You can edit it while Polaris is running. |
| `./data`   | Playlists, the collection index and the secret used to sign users in. Worth backing up.               |
| `./cache`  | Thumbnails, waveforms and transcoded songs. Can be deleted at any time.                               |

## Updating

```bash
docker compose pull
docker compose up -d
```

## File permissions

Polaris runs as the user and group set by the `PUID` and `PGID` environment variables (`1000` and `1000` by default). The files it creates in the folders above belong to this user, who must also be allowed to read your music collection. Run `id` on the host to find the values matching your own user.

The container starts as `root` to prepare these folders, then switches to this user before starting Polaris. To never run anything as `root`, start the container with the [`user`](https://docs.docker.com/reference/compose-file/services/#user) option instead; you are then responsible for creating the folders with suitable permissions.

## Options

- **Port**: change the left-hand side of `"5050:5050"` to serve Polaris on another port.
- **Several music folders**: add one volume per folder (eg. `/mnt/disk2/music:/music2:ro`) and add each one in the Polaris settings.
- **Command line options**: anything set as `command` is passed to Polaris, for example `command: ["--log-level", "debug"]`.
- **Single sign-on**: declare your identity provider in `./config/polaris.toml` as described in the [single sign-on documentation](SSO.md). The login page of the web client bundled in the image then offers a button for each provider.

## Using Polaris behind a reverse proxy

Polaris has to be served from the root of a domain or subdomain (eg. `https://music.example.com`), not from a sub-path. When using [single sign-on](SSO.md), make sure the proxy forwards the `X-Forwarded-Proto` and `X-Forwarded-Host` headers, or set `public_url` in the configuration file.

## Building the image yourself

From a checkout of this repository:

```bash
docker build -t polaris .
```

The `POLARIS_WEB_REF` build argument selects the version (a tag of the [polaris-web](https://github.com/agersant/polaris-web) repository) of the web client included in the image. The patch in `docker/polaris-web-sso.patch`, which adds single sign-on buttons to the login page, is applied on top of it.
