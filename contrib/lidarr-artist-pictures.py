#!/usr/bin/env python3
"""Saves the picture Lidarr has for each artist as `artist.jpg` in the folder of the artist.

Polaris (be-nj/polaris) shows the picture named `artist.jpg`, `artist.jpeg` or `artist.png`
found in the folder holding the albums of an artist. This script fills these folders from
Lidarr, which already knows a picture for most artists.

Usage:

    LIDARR_API_KEY=... ./lidarr-artist-pictures.py --url http://lidarr:8686 --dry-run
    LIDARR_API_KEY=... ./lidarr-artist-pictures.py --url http://lidarr:8686

The API key is under Settings > General in Lidarr. It is read from the LIDARR_API_KEY
environment variable (or from a file, with --api-key-file) rather than from the command
line, where other users of the machine could see it.

Lidarr reports the folder of each artist as Lidarr sees it. When this script runs somewhere
the music is mounted under another path, translate with --path-map, for example:

    --path-map /music=/mnt/storage/Musik

Folders which already have a picture are left alone, unless --overwrite is given. Nothing
else is modified or deleted. Requires Python 3.8 or newer, and nothing else.
"""

import argparse
import json
import os
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request

# Extensions Polaris recognizes pictures of artists by
PICTURE_EXTENSIONS = (".jpg", ".jpeg", ".png")
TIMEOUT_SECONDS = 30
MAX_PICTURE_BYTES = 50 * 1024 * 1024


def parse_arguments(arguments):
    parser = argparse.ArgumentParser(
        description="Saves the picture Lidarr has for each artist as artist.jpg in the folder of the artist.",
    )
    parser.add_argument("--url", required=True, help="address of Lidarr, for example http://lidarr:8686")
    parser.add_argument("--api-key-file", help="file holding the API key of Lidarr (default: LIDARR_API_KEY variable)")
    parser.add_argument(
        "--path-map",
        action="append",
        default=[],
        metavar="LIDARR_PATH=LOCAL_PATH",
        help="where a folder known to Lidarr is found on this machine (can be repeated)",
    )
    parser.add_argument("--name", default="artist", help="name of the files to write, without extension (default: artist)")
    parser.add_argument("--artist", action="append", default=[], help="only handle this artist (can be repeated)")
    parser.add_argument("--overwrite", action="store_true", help="replace pictures which already exist")
    parser.add_argument("--dry-run", action="store_true", help="only tell what would be done")
    return parser.parse_args(arguments)


def read_api_key(options):
    if options.api_key_file:
        with open(options.api_key_file, encoding="utf-8") as file:
            api_key = file.read().strip()
    else:
        api_key = os.environ.get("LIDARR_API_KEY", "").strip()
    if not api_key:
        sys.exit("No API key: set the LIDARR_API_KEY environment variable, or use --api-key-file.")
    return api_key


def parse_path_maps(path_maps):
    result = []
    for path_map in path_maps:
        lidarr_path, separator, local_path = path_map.partition("=")
        if not separator or not lidarr_path or not local_path:
            sys.exit(f"Invalid --path-map `{path_map}`, expected LIDARR_PATH=LOCAL_PATH.")
        result.append((lidarr_path.rstrip("/"), local_path.rstrip("/")))
    # Longest prefix first, so that the most specific mapping applies
    return sorted(result, key=lambda m: len(m[0]), reverse=True)


def map_path(path, path_maps):
    for lidarr_path, local_path in path_maps:
        if path == lidarr_path or path.startswith(lidarr_path + "/"):
            return local_path + path[len(lidarr_path):]
    return path


def fetch(url, api_key=None):
    """Downloads `url`. The API key is only sent to Lidarr itself, never to other sites."""
    request = urllib.request.Request(url)
    if api_key:
        request.add_header("X-Api-Key", api_key)
    with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
        data = response.read(MAX_PICTURE_BYTES + 1)
    if len(data) > MAX_PICTURE_BYTES:
        raise ValueError("the file is unreasonably large")
    return data


def detect_extension(data):
    """Tells the kind of picture from its content. Lidarr sometimes gets this wrong or omits it."""
    if data.startswith(b"\xff\xd8\xff"):
        return ".jpg"
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return ".png"
    return None


def find_poster(artist):
    for image in artist.get("images") or []:
        if str(image.get("coverType", "")).lower() == "poster":
            return image
    return None


def picture_sources(base_url, artist, image):
    """Places the picture can be downloaded from, most reliable first."""
    sources = []
    extension = image.get("extension") or ""
    artist_id = artist.get("id")
    if artist_id is not None and extension:
        # Served by the API of Lidarr, which accepts the API key
        sources.append((f"{base_url}/api/v1/mediacover/artist/{artist_id}/poster{extension}", True))
    local_url = image.get("url") or ""
    if local_url.startswith("/"):
        sources.append((base_url + local_url, True))
    for candidate in (image.get("remoteUrl") or "", local_url):
        if candidate.startswith(("http://", "https://")):
            # Another site: the API key must not be disclosed to it
            sources.append((candidate, False))
    return sources


def download_picture(base_url, api_key, artist, image):
    errors = []
    for url, is_lidarr in picture_sources(base_url, artist, image):
        try:
            data = fetch(url, api_key if is_lidarr else None)
        except (urllib.error.URLError, OSError, ValueError) as error:
            errors.append(f"{url.split('?')[0]}: {error}")
            continue
        extension = detect_extension(data)
        if extension is None:
            errors.append(f"{url.split('?')[0]}: neither a JPEG nor a PNG picture")
            continue
        return data, extension
    raise RuntimeError("; ".join(errors) or "Lidarr gave no address for the picture")


def existing_pictures(folder, name):
    wanted = {name.lower() + extension for extension in PICTURE_EXTENSIONS}
    return sorted(entry for entry in os.listdir(folder) if entry.lower() in wanted)


def write_file(path, data):
    """Writes the whole file or nothing, so that Polaris never sees half a picture."""
    folder = os.path.dirname(path)
    handle, temporary_path = tempfile.mkstemp(dir=folder, prefix=".artist-picture-")
    try:
        with os.fdopen(handle, "wb") as file:
            file.write(data)
        os.chmod(temporary_path, 0o644)
        os.replace(temporary_path, path)
    except BaseException:
        if os.path.exists(temporary_path):
            os.unlink(temporary_path)
        raise


def main(arguments=None):
    options = parse_arguments(arguments)
    api_key = read_api_key(options)
    path_maps = parse_path_maps(options.path_map)
    base_url = options.url.rstrip("/")
    only_artists = {name.lower() for name in options.artist}

    try:
        artists = json.loads(fetch(f"{base_url}/api/v1/artist", api_key))
    except (urllib.error.URLError, OSError, ValueError) as error:
        sys.exit(f"Could not list the artists of Lidarr at {base_url}: {error}")

    counts = {"written": 0, "kept": 0, "no picture": 0, "no folder": 0, "failed": 0}
    for artist in sorted(artists, key=lambda a: str(a.get("sortName") or a.get("artistName") or "")):
        name = artist.get("artistName") or "?"
        if only_artists and name.lower() not in only_artists:
            continue

        folder = map_path(artist.get("path") or "", path_maps)
        if not folder or not os.path.isdir(folder):
            print(f"no folder   {name}: {folder or '(none)'} does not exist here")
            counts["no folder"] += 1
            continue

        existing = existing_pictures(folder, options.name)
        if existing and not options.overwrite:
            print(f"kept        {name}: {os.path.join(folder, existing[0])}")
            counts["kept"] += 1
            continue

        image = find_poster(artist)
        if image is None:
            print(f"no picture  {name}: Lidarr has none")
            counts["no picture"] += 1
            continue

        try:
            data, extension = download_picture(base_url, api_key, artist, image)
            destination = os.path.join(folder, options.name + extension)
            if not options.dry_run:
                write_file(destination, data)
                # A picture with another extension would otherwise compete with the new one
                for other in existing:
                    if other != os.path.basename(destination):
                        os.unlink(os.path.join(folder, other))
        except (RuntimeError, OSError) as error:
            print(f"FAILED      {name}: {error}")
            counts["failed"] += 1
            continue

        action = "would write" if options.dry_run else "written    "
        print(f"{action} {name}: {destination} ({len(data) // 1024} kB)")
        counts["written"] += 1

    summary = ", ".join(f"{count} {label}" for label, count in counts.items())
    print(f"\n{'Dry run, nothing was written. ' if options.dry_run else ''}{summary}.")
    return 1 if counts["failed"] else 0


if __name__ == "__main__":
    sys.exit(main())
