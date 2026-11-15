#!/usr/bin/env bash
# Runs the end-to-end tests: starts the services they need, waits for them, runs the
# tests, then stops the services.
#
# Usage: ./run.sh [arguments for `playwright test`]
#
# Environment variables:
#   POLARIS_IMAGE  Use this Polaris image instead of building one from the source code
#   E2E_KEEP=1     Leave the services running afterwards (stop them with `docker compose down -v`)
set -euo pipefail
cd "$(dirname "$0")"

export E2E_UID="${E2E_UID:-$(id -u)}"
export E2E_GID="${E2E_GID:-$(id -g)}"

./setup.sh

if [ -n "${POLARIS_IMAGE:-}" ]; then
	docker compose up --detach --no-build
else
	docker compose up --detach --build
fi

stop_services() {
	if [ "${E2E_KEEP:-}" != "1" ]; then
		docker compose down --volumes
	fi
}
trap stop_services EXIT

# Services identify themselves by the host names they have inside the Docker network.
# Requests made from here reach them through their published ports instead.
wait_for() {
	local description="$1" host="$2" port="$3" path="$4" scheme="${5:-http}"
	echo "Waiting for ${description}..."
	for _ in $(seq 1 150); do
		if curl --silent --fail --insecure --output /dev/null --noproxy '*' \
			--resolve "${host}:${port}:127.0.0.1" "${scheme}://${host}:${port}${path}"; then
			return 0
		fi
		sleep 2
	done
	echo "Timed out waiting for ${description}" >&2
	docker compose logs --tail 50
	return 1
}

wait_for "Polaris" polaris.test 5050 /api/version
wait_for "Keycloak" keycloak.test 8080 /realms/polaris/.well-known/openid-configuration
wait_for "Authelia" authelia.test 9091 /.well-known/openid-configuration https
wait_for "authentik" authentik.test 9000 /application/o/polaris/.well-known/openid-configuration

npm ci --no-audit --no-fund
npx playwright test "$@"
