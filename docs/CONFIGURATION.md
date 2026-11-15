# Configuration

Polaris configuration resides in a single text file whose format is documented below. You can use the Polaris web UI to modify the configuration, or write to it in any text editor. You may edit the configuration file while Polaris is running.

## Location

The location of the configuration file is always logged during Polaris startup. It is determined as follows:

- From the `--config` (or `-c`) CLI option if present. This option must point to the `.toml` file.
- If the CLI option is not specified, Polaris will look for a `polaris.toml` file, inside the directory specified by the `POLARIS_CONFIG_DIR` environment variable _at compilation time_. When using the Windows installer, this will be `%LOCALAPPDATA%/Permafrost/Polaris/polaris.toml`. When using the supplied Makefile, the default is either `/usr/local/etc/polaris` (for a system-wide installations), or `~/.config/polaris` (for a XDG installation).
- If `POLARIS_CONFIG_DIR` was not set when Polaris was compiled, it will default to `.` on Linux, and the `LOCALAPPDATA` location mentioned above on Windows. This behavior on Windows may change in future releases.

## Format

The configuration file uses the [TOML](https://toml.io/) format. Everything in the configuration file is optional and may be omitted (unless mentioned otherwise).

```toml
# Regular expression matched (case-insensitively) against the name of files next to audio files, to find album art.
# Defaults to "^(folder|cover|front|album)\\.(jpeg|jpg|png)$". See "Album art" below.
album_art_pattern = "^(folder|cover)\\.(jpeg|jpg|png)$"
# Regular expression matched (case-insensitively) against the name of files next to the directories of albums, to find
# pictures of artists. Defaults to "^(artist|folder)\\.(jpeg|jpg|png)$". See "Pictures of artists" below.
artist_art_pattern = "^artist\\.(jpeg|jpg|png)$"
# Whether artists without a picture in the collection get one looked up online, in the Deezer catalog. This sends
# the names of these artists to Deezer. Defaults to false. See "Pictures of artists" below.
fetch_artist_images = true
# Regular expression matched (case-insensitively) against the name of each file and directory found while scanning.
# Matching entries are skipped, along with everything inside matching directories.
# Defaults to "^\\." which skips hidden files and directories. Set to "" to index everything.
scan_exclude_pattern = "^(\\.|@eaDir$)"
# Maximum disk space (in megabytes) used to keep transcoded songs around. Defaults to 1024.
transcode_cache_size_mb = 2048
# A URL Polaris will regularly make requests to in order to update Dynamic DNS
ddns_update_url = "https://example.com?token=foobar"

# Array of locations Polaris should scan to find music files
[[mount_dirs]]
# Directory to scan
source = "/home/example/music"
# User-facing name for this directory (must be unique)
name = "My Music 🎧️"

[[mount_dirs]]
source = "/mnt/example/more_music"
name = "Extra Music 🎵"

# Array of user accounts who can connect to the Polaris server
[[users]]
# Username for login
name = "example-user"
# If true, user will have access to all settings in the web UI
admin = true
# Plain text password for this user. Will be ignored if hashed_password is set. Polaris will never write to this field. For each user, at least one of initial_password and hashed_password must be set.
initial_password = "top-secret-password"
# Hashed and salted password for the user. Polaris will create this field if unset.
hashed_password = "$pbkdf2-sha256$i=10000,l=32$SI8LjK1KtvcawhgmWGJgRA$t9btMwhUTQ8r3vqI1xhArn19J7Jezyoi461fFjhZXGU"

[[users]]
name = "other-user"
admin = true
initial_password = "amospheric-strawberry64"
```

Single sign-on providers are also declared in this file, see the [single sign-on documentation](SSO.md).

## Album art

Polaris looks for the album art of each song in this order:

1. A picture embedded in the audio file. This is supported for `mp3`, `flac`, `m4a`, `ogg` (Vorbis), `opus`, `aiff` and `wav` files.
2. An image in the same directory as the song, whose name matches `album_art_pattern`. By default, these are `folder`, `cover`, `front` or `album` with a `jpg`, `jpeg` or `png` extension, in any letter case. When several images match, the first one in alphabetical order is used.
3. For songs in a directory holding one disc of an album (a name such as `CD1`, `Disc 2` or `disc_3 - Live`), an image matching `album_art_pattern` in the directory above.

Polaris does not download album art, and does not use images found anywhere else. Changes to `album_art_pattern` take effect with the next scan of the collection, which starts automatically.

## Pictures of artists

Polaris shows a picture for artists when it finds one in the directory holding their albums:

```
Music/
  Dire Straits/
    artist.jpg                  <- picture of Dire Straits
    1985 - Brothers in Arms/
      cover.jpg                 <- album art
      01 - So Far Away.flac
    1991 - On Every Street/
      CD1/
        01 - Calling Elvis.flac
```

The picture must be next to the directories of the albums (one level above the songs, or two for songs in a disc directory such as `CD1`), and its name must match `artist_art_pattern`. By default, these are `artist` or `folder` with a `jpg`, `jpeg` or `png` extension, in any letter case. When several images match, the first one in alphabetical order is used.

Artists are read from the tags of songs, not from the names of directories. A picture is given to an artist when most of the songs it was found next to are theirs, going by album artist (or by artist, for songs without album artist). This means that:

- The directory does not have to be named after the artist.
- A picture next to albums of many different artists is not used. This is the case of collections which have no directory per artist, and of directories of compilations.
- Artists who only appear as guests on the albums of others have no picture.
- Artists whose albums are spread over several directories use the picture found next to most of their songs.

Like album art, pictures of artists are not downloaded unless you ask for it: they have to be saved next to your music, by hand or with the program you use to organize it.

### Looking up pictures online

With `fetch_artist_images = true`, artists who have no picture in the collection get one from the catalog of [Deezer](https://www.deezer.com/), which needs no account. Keep in mind that:

- The name of each artist without a picture is sent to Deezer, the first time a client asks for their picture.
- When several artists of the catalog share a name, the one with the largest following is used, which may not be the one in your collection. Save a picture next to their albums to correct this: pictures of the collection always come first.
- Pictures are kept in the cache directory (`artist_images`), and are only downloaded once. Artists the catalog has no picture of are looked up again after 30 days.
- These pictures are served by the `/artist/{name}/thumbnail` endpoint of the API and as the `coverArt` of artists of the Subsonic API. The `artwork` field of artists only refers to pictures of the collection.
