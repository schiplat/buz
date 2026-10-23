# Prebuilt manager image: consumes the release archive extracted by the
# workflow (bin/ + conf/ in the build context). No compilation — binaries are
# built by the Release workflow on ubuntu-24.04 runners (glibc 2.39), so the
# base must match (ubuntu:24.04).
FROM ubuntu:24.04

RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /app/buz --shell /usr/sbin/nologin buz \
 && mkdir -p /app/buz/conf /app/buz/data /app/buz/pipelines \
 && chown -R buz:buz /app/buz

COPY bin/buz-manager /app/buz/buz-manager
COPY conf/manager.toml /app/buz/conf/manager.toml

USER buz
WORKDIR /app/buz
EXPOSE 8080
VOLUME ["/app/buz/data", "/app/buz/pipelines"]

ENTRYPOINT ["/app/buz/buz-manager"]
CMD ["-c", "/app/buz/conf/manager.toml"]
