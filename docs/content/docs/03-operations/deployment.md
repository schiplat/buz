---
title: Deployment
weight: 1
description: "Deploying Center and Runner in production"
---

## Quick Start

```bash
# One-step: build all + run Center
make run
# → http://localhost:8080  (admin / admin)

# Or step by step
make example        # create example config directory
make build          # build frontend + backend
cd example && ../target/release/buz-manager
```

## Center Deployment

The Center binary includes the frontend static assets and SQLite database. No external dependencies required.

```bash
# Build
make build-frontend
make build-backend

# Create config (or use `make example`)
mkdir -p conf
cat > conf/manager.toml <<'EOF'
[server]
host = "0.0.0.0"
port = 8080

[database]
path = "./data/buz.db"

[auth]
jwt_secret = "changeme-demo-only"
admin_user = "admin"
admin_password_hash = "admin"
EOF

# Start
export BUZ_JWT_SECRET="$(openssl rand -hex 32)"
./target/release/buz-manager
```

## Runner Deployment

Runners can be deployed on any machine with outbound HTTPS access to the Center.

```bash
# Create config
mkdir -p conf
cat > conf/runner.toml <<EOF
[center]
url = "https://buz-api.example.com"
register_token = "your-register-token"

[node]
name = "prod-node-01"
labels = ["prod", "linux-x64"]
work_dir = "/tmp/buz-runner"

[engine]
local_enabled = true
ssh_enabled = true
docker_enabled = false
wasm_enabled = false
EOF

export BUZ_REGISTER_TOKEN="your-token"
./target/release/buz-runner
```

## Requirements

| Component | Minimum |
|-----------|---------|
| Runner memory (idle) | < 20MB |
| Center memory (idle) | < 50MB |
| Network | Runner → Center outbound HTTPS only |
| Database | SQLite (built-in, zero setup) |
