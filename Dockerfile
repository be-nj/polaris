# syntax=docker/dockerfile:1

# Version of the web client (https://github.com/agersant/polaris-web) bundled in the image
ARG POLARIS_WEB_REF=build-99

# Web client, with the single sign-on buttons added to its login page
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS web
ARG POLARIS_WEB_REF
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates git \
	&& rm -rf /var/lib/apt/lists/*
WORKDIR /web
RUN git clone --depth 1 --branch "${POLARIS_WEB_REF}" https://github.com/agersant/polaris-web.git .
COPY docker/polaris-web-sso.patch /tmp/polaris-web-sso.patch
RUN git apply /tmp/polaris-web-sso.patch \
	&& npm ci --no-audit --no-fund \
	&& npm run build

# Server. This stage always runs on the architecture of the build machine, and
# cross-compiles when the image is built for another architecture.
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS server
ARG BUILDARCH
ARG TARGETARCH
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
	CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
	CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
	CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
RUN set -eu; \
	case "${TARGETARCH}" in \
		amd64) rust_target=x86_64-unknown-linux-gnu; cross_gcc=gcc-x86-64-linux-gnu ;; \
		arm64) rust_target=aarch64-unknown-linux-gnu; cross_gcc=gcc-aarch64-linux-gnu ;; \
		*) echo "Unsupported architecture: ${TARGETARCH}" >&2; exit 1 ;; \
	esac; \
	if [ "${TARGETARCH}" != "${BUILDARCH}" ]; then \
		apt-get update; \
		apt-get install -y --no-install-recommends "${cross_gcc}" "libc6-dev-${TARGETARCH}-cross"; \
		rm -rf /var/lib/apt/lists/*; \
	fi; \
	rustup target add "${rust_target}"; \
	echo "${rust_target}" > /rust-target
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
	--mount=type=cache,target=/usr/local/cargo/git \
	--mount=type=cache,target=/src/target,sharing=locked \
	set -eu; \
	rust_target="$(cat /rust-target)"; \
	cargo build --release --locked --target "${rust_target}"; \
	cp "target/${rust_target}/release/polaris" /polaris

FROM debian:bookworm-slim
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates ffmpeg \
	&& rm -rf /var/lib/apt/lists/* \
	&& mkdir /config /data /cache /music
COPY --from=server /polaris /usr/local/bin/polaris
COPY --from=web /web/dist /usr/share/polaris/web
COPY --chmod=755 docker/entrypoint.sh /usr/local/bin/polaris-entrypoint

# User and group Polaris runs as. See docker/entrypoint.sh.
ENV PUID=1000 PGID=1000
EXPOSE 5050
VOLUME ["/config", "/data", "/cache"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s \
	CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/5050"]
ENTRYPOINT ["polaris-entrypoint"]
