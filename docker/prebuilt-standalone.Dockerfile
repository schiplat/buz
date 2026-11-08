# Prebuilt standalone image: Manager + Runner in one container (multi-arch),
# consuming the release archive extracted by the workflow (bin/ + conf/ +
# standalone tomls + entrypoint in the build context). Base matches the
# release build runners (ubuntu 24.04, glibc 2.39).
FROM ubuntu:24.04

RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends ca-certificates bash \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /app/buz --shell /usr/sbin/nologin buz \
 && mkdir -p /app/buz/conf /app/buz/data /app/buz/pipelines \
            /app/buz/workspace /app/buz/logs /app/buz/state \
 && chown -R buz:buz /app/buz

COPY bin/buz-manager /app/buz/buz-manager
COPY bin/buz-runner /app/buz/buz-runner
COPY manager.toml /app/buz/conf/manager.toml
COPY runner.toml /app/buz/conf/runner.toml
COPY entrypoint.sh /app/buz/entrypoint.sh
RUN chmod 755 /app/buz/entrypoint.sh

USER buz
WORKDIR /app/buz
EXPOSE 8080
VOLUME ["/app/buz/data", "/app/buz/pipelines", "/app/buz/workspace", "/app/buz/logs", "/app/buz/state"]

ENV BUZ_MODE=all
ENTRYPOINT ["/app/buz/entrypoint.sh"]
