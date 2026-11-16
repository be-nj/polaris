"""Tests lidarr-artist-pictures.py against a simulated Lidarr: python3 contrib/test_lidarr_artist_pictures.py"""
import contextlib, importlib.util, io, json, os, sys, tempfile, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

here = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("script", os.path.join(here, "lidarr-artist-pictures.py"))
script = importlib.util.module_from_spec(spec); spec.loader.exec_module(script)

JPEG = b"\xff\xd8\xff\xe0" + b"jpeg-data" * 200
PNG = b"\x89PNG\r\n\x1a\n" + b"png-data" * 200
API_KEY = "secret-key"
requests_seen = []

def make_server(routes):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            path = self.path.split("?")[0]
            requests_seen.append((self.server.server_port, path, self.headers.get("X-Api-Key")))
            handler = routes.get(path)
            if handler is None:
                self.send_response(404); self.end_headers(); return
            status, body = handler(self.headers)
            self.send_response(status); self.end_headers(); self.wfile.write(body)
        def log_message(self, *args): pass
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server

def needs_key(body):
    return lambda headers: (200, body) if headers.get("X-Api-Key") == API_KEY else (401, b"")

remote = make_server({"/pictures/remote.jpg": lambda headers: (200, JPEG)})
remote_url = f"http://127.0.0.1:{remote.server_port}/pictures/remote.jpg"

artists = [
    {"id": 2, "artistName": "Toto", "sortName": "toto", "path": "/music/toto",
     "images": [{"coverType": "fanart", "url": "/MediaCover/2/fanart.jpg", "extension": ".jpg"},
                {"coverType": "poster", "url": "/MediaCover/2/poster.JPG?lastWrite=639212594534800291",
                 "remoteUrl": "https://unreachable.invalid/poster.JPG", "extension": ".JPG"}]},
    # Only served by the older address, and actually a PNG despite its extension
    {"id": 3, "artistName": "Dire Straits", "sortName": "dire straits", "path": "/music/dire-straits",
     "images": [{"coverType": "poster", "url": "/MediaCover/3/poster.jpg", "extension": ".jpg"}]},
    # Lidarr has no local copy: only the remote address works
    {"id": 4, "artistName": "Khemmis", "sortName": "khemmis", "path": "/music/khemmis",
     "images": [{"coverType": "poster", "url": "/MediaCover/4/poster.jpg", "remoteUrl": remote_url, "extension": ".jpg"}]},
    {"id": 5, "artistName": "No Picture", "sortName": "no picture", "path": "/music/no-picture", "images": []},
    {"id": 6, "artistName": "No Folder", "sortName": "no folder", "path": "/music/missing",
     "images": [{"coverType": "poster", "url": "/MediaCover/6/poster.jpg", "extension": ".jpg"}]},
    # What Lidarr serves is not a picture
    {"id": 7, "artistName": "Broken", "sortName": "broken", "path": "/music/broken",
     "images": [{"coverType": "poster", "url": "/MediaCover/7/poster.jpg", "extension": ".jpg"}]},
]
lidarr = make_server({
    "/api/v1/artist": needs_key(json.dumps(artists).encode()),
    "/api/v1/mediacover/artist/2/poster.JPG": needs_key(JPEG),
    "/MediaCover/3/poster.jpg": needs_key(PNG),
    "/MediaCover/7/poster.jpg": needs_key(b"<html>login page</html>"),
})
lidarr_url = f"http://127.0.0.1:{lidarr.server_port}"

def run(music, *arguments, key=API_KEY):
    os.environ["LIDARR_API_KEY"] = key
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        try:
            code = script.main(["--url", lidarr_url + "/", "--path-map", f"/music={music}", *arguments])
        except SystemExit as exit:
            code, _ = exit.code, print(exit.code)
    return code, output.getvalue()

def listing(music):
    return sorted(os.path.relpath(os.path.join(d, f), music) for d, _, fs in os.walk(music) for f in fs)

with tempfile.TemporaryDirectory() as music:
    for folder in ["toto", "dire-straits", "khemmis", "no-picture", "broken"]:
        os.makedirs(os.path.join(music, folder))
    open(os.path.join(music, "toto", "artist.nfo"), "w").write("<artist/>")

    # A dry run tells what would happen and writes nothing
    code, output = run(music, "--dry-run")
    assert listing(music) == ["toto/artist.nfo"], listing(music)
    assert "would write Toto" in output and "Dry run" in output, output

    code, output = run(music)
    print(output)
    assert code == 1, code  # one artist failed
    assert listing(music) == ["dire-straits/artist.png", "khemmis/artist.jpg", "toto/artist.jpg", "toto/artist.nfo"], listing(music)
    assert open(os.path.join(music, "toto", "artist.jpg"), "rb").read() == JPEG
    assert open(os.path.join(music, "dire-straits", "artist.png"), "rb").read() == PNG
    assert open(os.path.join(music, "khemmis", "artist.jpg"), "rb").read() == JPEG
    assert "no picture  No Picture" in output and "no folder   No Folder" in output and "FAILED      Broken" in output
    assert "3 written, 0 kept, 1 no picture, 1 no folder, 1 failed" in output, output
    # No temporary file is left behind, and files are readable by other users (Polaris)
    assert oct(os.stat(os.path.join(music, "toto", "artist.jpg")).st_mode & 0o777) == "0o644"

    # The API key is sent to Lidarr, and never to another site
    assert all(key == API_KEY for port, _, key in requests_seen if port == lidarr.server_port)
    assert any(port == remote.server_port for port, _, _ in requests_seen)
    assert all(key is None for port, _, key in requests_seen if port == remote.server_port)

    # Existing pictures are kept, whatever their letter case
    os.rename(os.path.join(music, "toto", "artist.jpg"), os.path.join(music, "toto", "Artist.JPEG"))
    open(os.path.join(music, "toto", "Artist.JPEG"), "wb").write(b"my own picture")
    code, output = run(music, "--artist", "toto")
    assert "kept        Toto" in output and "Dire Straits" not in output, output
    assert open(os.path.join(music, "toto", "Artist.JPEG"), "rb").read() == b"my own picture"

    # Unless asked to replace them, which leaves a single picture
    code, output = run(music, "--artist", "Toto", "--overwrite")
    assert code == 0 and listing(music)[-2:] == ["toto/artist.jpg", "toto/artist.nfo"], (code, listing(music))
    assert open(os.path.join(music, "toto", "artist.jpg"), "rb").read() == JPEG

    # Another file name can be chosen
    code, output = run(music, "--artist", "Toto", "--name", "folder")
    assert "toto/folder.jpg" in listing(music)

    # A wrong API key is reported clearly, and nothing is written
    before = listing(music)
    code, output = run(music, key="wrong")
    assert "Could not list the artists" in output and listing(music) == before, output

    # Without --path-map, paths are used as Lidarr reports them
    assert script.map_path("/music/toto", []) == "/music/toto"
    maps = script.parse_path_maps(["/music=/mnt/a", "/music/live=/mnt/b/"])
    assert script.map_path("/music/live/toto", maps) == "/mnt/b/toto"
    assert script.map_path("/music/toto", maps) == "/mnt/a/toto"
    assert script.map_path("/musical/toto", maps) == "/musical/toto"

print("ALL CHECKS PASSED")
