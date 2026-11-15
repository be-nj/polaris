# Subsonic API

In addition to its own API, Polaris implements the [Subsonic API](https://www.subsonic.org/pages/api.jsp) (protocol version 1.16.1, with [OpenSubsonic](https://opensubsonic.netlify.app/) extensions). This makes it possible to use most third-party Subsonic clients with a Polaris server.

## Connecting a client

- **Server address**: the address of your Polaris server, for example `https://polaris.example.com`. Clients append `/rest/...` to it on their own.
- **Username and password**: the same as in the Polaris web UI.
- **Authentication method**: select the option your client calls _legacy authentication_, _plain text password_ or _force plain password_ (the wording varies between clients).

Polaris only stores salted hashes of user passwords. The default Subsonic authentication scheme (salted MD5 tokens) can only be verified by servers that know the actual password, so Polaris cannot support it. A client attempting to use it receives error `41` with an explanation. Since the password is then part of every request, serve Polaris over HTTPS (see the [reverse proxy examples](https://github.com/agersant/polaris/issues/118)) whenever it is reachable from outside your home network.

Clients that support OpenSubsonic API keys can be given a Polaris authentication token instead of a password. A token is returned by the `/api/auth` endpoint documented at `/api-docs/` on your server, and stops working when the user's password changes.

## Supported functionality

- Browsing by tags (artists, albums, genres) and by folders
- Album lists: random, newest, alphabetical, by year and by genre
- Random songs and songs by genre
- Search
- Streaming and downloading songs, with support for seeking
- Bitrate limits (`maxBitRate`), when [transcoding](TRANSCODING.md) is available. Without it, songs are streamed as they are.
- Album art
- Playlists (each user only sees their own playlists)
- Collection scan status, and triggering a scan (administrators only)

## Limitations

Polaris has no equivalent for the following Subsonic features:

- Starring, ratings, play counts and "now playing". Lists based on these (`starred`, `highest`, `frequent`, `recent`) are always empty, and attempts to star or rate report an error.
- Scrobbling. Scrobble requests are accepted and ignored.
- Lyrics, artist biographies, similar artists and top songs.
- Podcasts, internet radio, bookmarks, shares, chat, jukebox mode and user management.

Other differences worth knowing about:

- Songs do not report their file size or bitrate.
- Playlists cannot be made public or shared between users, and have no creation date.
- Identifiers of artists, albums and songs are derived from their names and file paths. Renaming them changes their identifier, which clients may treat as an unrelated new item.
