.PHONY: dev dev-frontend dev-manager run build build-frontend build-backend build example test test-docker-e2e ci package

# ── One-step: build everything and run ─────────────────────────────
run: build
	@echo "=== Starting Buz Manager ==="
	cd example && BUZ_ALLOW_INSECURE_JWT=1 ../target/release/buz-manager

run-runner: build
	@echo "=== Starting Buz Runner ==="
	cd example && ../target/release/buz-runner

# ── Build all ─────────────────────────────────────────────────────
# Frontend must be built first — release embeds frontend/dist into buz-manager.
build: build-frontend build-backend
	@echo "=== Build complete (UI embedded in buz-manager) ==="

# ── Frontend ──────────────────────────────────────────────────────
build-frontend:
	cd frontend && npm run build

dev-frontend:
	cd frontend && npm run dev

# ── Backend ───────────────────────────────────────────────────────
build-backend:
	cargo build --release

dev-manager:
	cargo run --release -p manager

# ── Quick dev (no release optimization) ───────────────────────────
dev:
	@echo "=== Building frontend + debug manager ==="
	cd frontend && npm run build
	cargo build -p manager
	@echo "=== Starting ==="
	cd example && BUZ_ALLOW_INSECURE_JWT=1 ../target/debug/buz-manager

# ── Example scaffolding ───────────────────────────────────────────
# Creates data/pipelines dirs. Config templates live in example/conf/*.toml (committed).
example:
	mkdir -p example/conf example/data example/pipelines
	@test -f example/conf/manager.toml || { echo "missing example/conf/manager.toml"; exit 1; }
	@test -f example/conf/runner.toml || { echo "missing example/conf/runner.toml"; exit 1; }
	@echo "Example ready in ./example/"
	@echo "  Manager:  cd example && BUZ_ALLOW_INSECURE_JWT=1 ../target/release/buz-manager"
	@echo "  Runner:   edit example/conf/runner.toml register_token, then"
	@echo "            cd example && ../target/release/buz-runner"
	@echo "  Prefer:   export BUZ_JWT_SECRET=\"\$$(openssl rand -hex 32)\"  # then omit BUZ_ALLOW_INSECURE_JWT"
	@echo "  Labels / engines / concurrency → Manager UI → Runners"

# ── Docs ──────────────────────────────────────────────────────────
docs-serve:
	cd docs && hugo server --port 3001

docs-build:
	cd docs && hugo --minify

docs-clean:
	rm -rf docs/public docs/.hugo_build.lock

# ── Clean ─────────────────────────────────────────────────────────
clean:
	cargo clean
	rm -rf frontend/dist
	rm -rf example/data
	# Keep example/conf and example/pipelines (committed templates)
# ── Test & CI ────────────────────────────────────────────────────
test:
	cargo test --all

# Real Docker daemon e2e (ignored tests). Requires docker CLI + daemon.
test-docker-e2e:
	./scripts/docker-engine-e2e.sh

ci: test
	cargo fmt --all -- --check
	cargo check --all
	cd frontend && npm run build
	@echo "=== CI passed ==="

# ── Release tarball (local) ───────────────────────────────────────
# Produces dist/release/buz-<version>-<os-arch>.tar.gz (+ .sha256)
package: build
	chmod +x scripts/package-release.sh
	./scripts/package-release.sh
