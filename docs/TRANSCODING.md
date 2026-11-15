# Transcoding

Polaris can convert songs to a smaller format while streaming them. This is mostly useful to save mobile data when your collection contains lossless files, and to play formats that the device at hand does not support.

## Requirements

Transcoding is performed by [FFmpeg](https://ffmpeg.org/), which must be installed on the machine running Polaris:

- Polaris looks for an `ffmpeg` executable in the `PATH`. To use a specific executable, set the `POLARIS_FFMPEG_PATH` environment variable to its location before starting Polaris.
- When FFmpeg cannot be found, transcoding is disabled and everything else works as usual. The Polaris log states whether FFmpeg was found the first time a client asks about or requests transcoding.

## How it works

Clients opt into transcoding per song, by asking for a format and bitrate:

- Through the Polaris API, with the `format` (`mp3`, `opus` or `aac`) and `bitrate` (in kbps) parameters of the `/api/audio` endpoint. The `/api/transcoding` endpoint tells clients whether transcoding is available.
- Through the [Subsonic API](SUBSONIC.md), with the `maxBitRate` and `format` parameters of the `stream` method. Most Subsonic clients expose this as a maximum bitrate setting, often separately for Wi-Fi and mobile connections.

Polaris then decides what to send:

- Lossless files (`flac`, `wav`, `aiff`, `ape`) and `mpc` files are always converted.
- Files which are already compressed (`mp3`, `m4a`, `ogg`, `opus`) are only converted when their bitrate is noticeably above the requested one. Otherwise they are sent as they are, since converting them again would only lower their quality.

Playback starts right away, while the song is still being converted. Seeking to a part of the song that has not been sent yet is only possible once the conversion has finished, which usually takes a few seconds.

## Cache

Converted songs are kept in the `transcodes` folder of the Polaris cache directory so that they are only converted once. When the cache grows beyond its size limit, the songs which have not been played for the longest time are deleted. The limit defaults to 1024 MB and can be changed with the `transcode_cache_size_mb` setting of the [configuration file](CONFIGURATION.md).
