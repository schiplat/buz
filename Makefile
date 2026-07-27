.PHONY: dev dev-frontend dev-manager run build build-frontend build-backend build example test ci

# ── One-step: build everything and run ─────────────────────────────
run: build
	@echo "=== Starting Buz Manager ==="
	cd example && ../target/release/buz-manager

run-runner: build
	@echo "=== Starting Buz Runner ==="
	cd example && ../target/release/buz-runner

# ── Build all ─────────────────────────────────────────────────────
build: build-frontend build-backend
	@echo "=== Build complete ==="

# ── Frontend ──────────────────────────────────────────────────────
build-frontend:
	cd frontend && npm run build

dev-frontend:
	cd frontend && npm run dev

# ── Backend ───────────────────────────────────────────────────────
build-backend:
	cargo build --release

dev-manager:
	cargo run --release -p buz-manager

# ── Quick dev (no release optimization) ───────────────────────────
dev:
	@echo "=== Building frontend + debug manager ==="
	cd frontend && npm run build
	cargo build -p buz-manager
	@echo "=== Starting ==="
	cd example && ../target/debug/buz-manager

# ── Example scaffolding ───────────────────────────────────────────
example:
	mkdir -p example/conf example/data example/pipelines
	@echo '# Buz Manager Configuration'      >  example/conf/manager.toml
	@echo ''                                 >> example/conf/manager.toml
	@echo '[server]'                         >> example/conf/manager.toml
	@echo 'host = "0.0.0.0"'                 >> example/conf/manager.toml
	@echo 'port = 8080'                      >> example/conf/manager.toml
	@echo ''                                 >> example/conf/manager.toml
	@echo '[database]'                       >> example/conf/manager.toml
	@echo 'path = "./data/buz.db"'           >> example/conf/manager.toml
	@echo ''                                 >> example/conf/manager.toml
	@echo '[auth]'                           >> example/conf/manager.toml
	@echo 'jwt_secret = "changeme-demo-only"'          >> example/conf/manager.toml
	@echo 'admin_user = "admin"'             >> example/conf/manager.toml
	@echo 'admin_password = "changeme-demo-only"'         >> example/conf/manager.toml
	@echo ''                                 >> example/conf/manager.toml
	@echo '[pipeline]'                       >> example/conf/manager.toml
	@echo 'yaml_scan_dir = "./pipelines"'    >> example/conf/manager.toml
	@echo ''                                 >  example/conf/runner.toml
	@echo '# Buz Runner Configuration'       >> example/conf/runner.toml
	@echo ''                                 >> example/conf/runner.toml
	@echo '[manager]'                         >> example/conf/runner.toml
	@echo 'url = "http://127.0.0.1:8080"'    >> example/conf/runner.toml
	@echo '# Get this token from Buz Manager web UI → Nodes → New Node' >> example/conf/runner.toml
	@echo 'register_token = "REPLACE-ME"'    >> example/conf/runner.toml
	@echo ''                                 >> example/conf/runner.toml
	@echo '[node]'                           >> example/conf/runner.toml
	@echo 'name = "my-runner"'               >> example/conf/runner.toml
	@echo 'labels = ["linux-x64"]'           >> example/conf/runner.toml
	@echo ""                                                                                        && echo "Example configs created in ./example/conf/" && echo ""                                       && echo "Start the Manager:"                                                                          && echo "  (cd example && ../target/release/buz-manager)"                                              && echo ""                                                                                           && echo "Start a Runner (edit register_token first):"                                                && echo "  (cd example && ../target/release/buz-runner)"                                              && echo ""

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
	rm -rf example

# ── Test & CI ────────────────────────────────────────────────────
test:
	cargo test --all

ci: test
	cargo fmt --all -- --check
	cargo check --all
	cd frontend && npm run build
	@echo "=== CI passed ==="
