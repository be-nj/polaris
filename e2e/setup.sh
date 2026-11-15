#!/usr/bin/env bash
# Prepares the files services need at runtime. These are not kept in the repository
# because they are either secrets (keys, certificates) or modified by the services.
set -euo pipefail
cd "$(dirname "$0")"

rm -rf runtime
mkdir -p runtime/polaris runtime/certs
cp polaris.toml runtime/polaris/polaris.toml

# Certificate authority trusted by Polaris, and certificates of the services using HTTPS
cd runtime/certs
openssl req -x509 -newkey rsa:2048 -nodes -days 30 \
	-keyout ca.key -out ca.crt -subj "/CN=Polaris E2E Test CA" 2>/dev/null
for host in authelia.test; do
	name="${host%%.*}"
	openssl req -newkey rsa:2048 -nodes \
		-keyout "${name}.key" -out "${name}.csr" -subj "/CN=${host}" 2>/dev/null
	openssl x509 -req -in "${name}.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -days 30 \
		-extfile <(printf "subjectAltName=DNS:%s" "${host}") -out "${name}.crt" 2>/dev/null
	rm "${name}.csr"
done

# Key Authelia signs its tokens with
openssl genrsa -out authelia-oidc.key 2048 2>/dev/null

# Services run as various users
chmod 644 ./*.key
