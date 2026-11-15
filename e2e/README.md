# End-to-end tests

These tests exercise Polaris the way it is actually used: the Docker image runs next to real third-party services, and is driven by a real web browser and a third-party Subsonic client library.

| Tests | What they cover |
| --- | --- |
| `tests/sso.spec.ts` | Single sign-on with [Keycloak](https://www.keycloak.org/), [Authelia](https://www.authelia.com/) and [authentik](https://goauthentik.io/): signing in from the login page of the web client, administrator rights driven by groups, rejection of users without an allowed group. |
| `tests/subsonic.spec.ts` | The Subsonic API, through the [`subsonic-api`](https://www.npmjs.com/package/subsonic-api) client library: authentication, browsing, search, streaming, cover art, bitrate limits and playlists. |
| `tests/transcoding.spec.ts` | Transcoding with the FFmpeg bundled in the image, including seeking in transcoded songs. |
| `tests/web.spec.ts` | Signing in to the web client with a password. |

## Running the tests

Requirements: Docker (with Compose), Node.js 22 or newer, `openssl` and `curl`.

```bash
cd e2e
npx playwright install chromium   # only needed once
./run.sh
```

`run.sh` builds the Polaris image from the source code, starts all services, waits for them to be ready, runs the tests and stops the services. Arguments are passed on to [`playwright test`](https://playwright.dev/docs/test-cli), for example `./run.sh tests/sso.spec.ts` or `./run.sh --ui`.

| Environment variable | Effect |
| --- | --- |
| `POLARIS_IMAGE` | Test this image (eg. `ghcr.io/be-nj/polaris:latest`) instead of building one. |
| `E2E_PROVIDERS` | Comma-separated list of identity providers to test (`keycloak`, `authelia`, `authentik`). Defaults to all of them. |
| `E2E_KEEP=1` | Leave the services running after the tests, to inspect them or to re-run tests with `npx playwright test`. Stop them with `docker compose down --volumes`. |

The services use ports `5050`, `8080`, `9000` and `9091` of the machine running the tests.

## How it fits together

- `docker-compose.yml` declares the services. Inside the Docker network they are known as `polaris.test`, `keycloak.test`, `authelia.test` and `authentik.test`; these names are part of the URLs the services hand out (issuers, redirections).
- The browser run by the tests resolves `*.test` to the local machine (see `playwright.config.ts`), where the services publish their ports. This way the browser, Polaris and the identity providers all agree on the same URLs, without any change to the `hosts` file.
- `setup.sh` creates the `runtime` folder: a copy of `polaris.toml` (Polaris modifies its configuration file), a throwaway certificate authority, the certificate Authelia serves HTTPS with and the key it signs tokens with.
- Every identity provider has the same three users, whose passwords are `<username>-password`: `alice` (member of `polaris-users` and `polaris-admins`), `bob` (member of `polaris-users`) and `mallory` (no group). Polaris only lets members of `polaris-users` in, and makes members of `polaris-admins` administrators.

All secrets in this folder are throwaway values that only exist for these tests.
