# Prebuilt runner image: consumes the release archive extracted by the
# workflow (bin/ + conf/ in the build context). Base matches the release
# build runners (ubuntu 24.04, glibc 2.39).
FROM ubuntu:24.04

RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /app/buz --shell /usr/sbin/nologin buz \
 && mkdir -p /app/buz/conf /app/buz/workspace /app/buz/logs /app/buz/state \
 && chown -R buz:buz /app/buz

COPY bin/buz-runner /app/buz/buz-runner
COPY conf/runner.toml /app/buz/conf/runner.toml

USER buz
WORKDIR /app/buz
VOLUME ["/app/buz/workspace", "/app/buz/logs", "/app/buz/state"]

ENTRYPOINT ["/app/buz/buz-runner"]
CMD ["-c", "/app/buz/conf/runner.toml"]
