#!/bin/sh
# Starts Polaris inside the Docker image.
#
# When the container runs as root (the default), this makes sure the folders Polaris writes
# to belong to the user it is about to run as, then drops root privileges. The user is
# selected with the PUID and PGID environment variables. When the container is started as
# another user (eg. with `--user`), Polaris simply runs as that user.
set -eu

set -- polaris \
	--foreground \
	--config /config/polaris.toml \
	--data /data \
	--cache /cache \
	--web /usr/share/polaris/web \
	"$@"

if [ "$(id -u)" != "0" ]; then
	exec "$@"
fi

PUID="${PUID:-1000}"
PGID="${PGID:-1000}"

for directory in /config /data /cache; do
	mkdir -p "${directory}"
	if [ "$(stat -c '%u:%g' "${directory}")" != "${PUID}:${PGID}" ]; then
		chown -R "${PUID}:${PGID}" "${directory}"
	fi
done

exec setpriv --reuid "${PUID}" --regid "${PGID}" --clear-groups "$@"
