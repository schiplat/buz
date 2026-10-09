# Standalone compose — Manager + Runner in one container (arm64)

Full guide: docs/content/docs/03-operations/standalone.md
(or site path /docs/03-operations/standalone/).

## Quick start

```bash
mkdir -p /opt/buz-standalone && cd /opt/buz-standalone
cp /path/to/buz/deploy/standalone/docker-compose.yml .
mkdir -p data pipelines workspace logs state
sudo chown -R 10001:10001 data pipelines workspace logs state

cat > .env <<EOF
BUZ_JWT_SECRET=$(openssl rand -hex 32)
IMAGE_TAG=latest
EOF
chmod 600 .env

docker compose pull
docker compose up -d
# Open http://<host>:8080 → /setup → Runners → standalone-runner online
```

## Upgrade

```bash
cd /opt/buz-standalone
# Optional pin: IMAGE_TAG=v1.7.2
docker compose pull
docker compose up -d
```

Keep the same `.env` (`BUZ_JWT_SECRET`) and volume directories, including `state/` for the Runner JWT cache, across upgrades.
