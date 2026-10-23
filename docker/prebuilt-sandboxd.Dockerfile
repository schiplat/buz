# Prebuilt sandboxd image: consumes the release archive extracted by the
# workflow (bin/ + conf/ in the build context). Base matches the release
# build runners (ubuntu 24.04, glibc 2.39). Bundles the Docker client the
# supervisor uses to manage job containers.
FROM ubuntu:24.04

RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 buz \
 && mkdir -p /app/buz/conf /app/buz/workspace /run/buz-sandboxd

COPY --from=docker:27-cli /usr/local/bin/docker /usr/local/bin/docker
COPY bin/buz-sandboxd /usr/local/bin/buz-sandboxd
COPY conf/sandboxd.toml /app/buz/conf/sandboxd.toml

ENTRYPOINT ["/usr/local/bin/buz-sandboxd"]
CMD ["--config", "/app/buz/conf/sandboxd.toml"]
