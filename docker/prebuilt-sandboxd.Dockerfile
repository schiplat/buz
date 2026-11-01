# Prebuilt sandboxd image: consumes the release archive extracted by the
# workflow (bin/ + conf/ in the build context). Base matches the release
# build runners (ubuntu 24.04, glibc 2.39). Bundles the Docker client the
# supervisor uses to manage job containers.
FROM ubuntu:24.04

RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 buz \
 && mkdir -p /app/buz/workspace /app/buz/state /run/buz-sandboxd

# Bootstrap via env (compose sets the rest); policy arrives from the Manager.
ENV BUZ_SANDBOXD_STATE_DIR=/app/buz/state
COPY --from=docker:27-cli /usr/local/bin/docker /usr/local/bin/docker
COPY bin/buz-sandboxd /usr/local/bin/buz-sandboxd

ENTRYPOINT ["/usr/local/bin/buz-sandboxd"]
