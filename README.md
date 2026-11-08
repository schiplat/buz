# Buz

Lightweight Cross-Network Orchestration Platform.

**Single Binary, Unified Orchestration, Zero Trust.**

Check the installed version any time: `buz-manager --version` / `buz-runner --version` — also shown in the Web UI (sidebar footer / login page) and via `GET /health`.

Tab completion for `buz-manager` / `buz-runner` / `buz-plugin`: `buz-manager completions <bash|zsh|fish|powershell|elvish>` (see [Shell Completions](docs/content/docs/03-operations/shell-completions.md)).

![Buz overview](docs/static/image/buz_overview.png)

## 快速考察 — Standalone 单容器演练（约 5 分钟）

一个容器跑通全部栈（Manager + Runner），全程复制粘贴即可，不在任何仓库
留下内容。考察的功能面：Manager/Runner 配对、仪表盘、job 调度与执行、
插件市场、secret 静态加密。

> 要求：Linux（或 Docker Desktop）、`docker` + `docker compose`、
> `curl`、`openssl`。镜像为 linux/arm64（Apple Silicon Mac 可模拟运行）。

**获取镜像（一行，公开 DockerHub）**：

```bash
export IMAGE=schiplat/buz-standalone:latest   # linux/arm64; 版本 tag 如 :v2.1.5
```

> 或从源码本地构建：`git clone https://github.com/schiplat/buz.git && cd buz && docker build -f Dockerfile.standalone -t buz-standalone:local . && export IMAGE=buz-standalone:local`


```bash
# 1. Stage a deploy directory
mkdir -p /tmp/buz-demo && cd /tmp/buz-demo
curl -fsSL -o docker-compose.yml   https://raw.githubusercontent.com/schiplat/buz/master/deploy/standalone/docker-compose.yml

# 2. Secret + launch (demo secret is auto-generated, not committed anywhere)
echo "BUZ_JWT_SECRET=$(openssl rand -hex 32)" > .env
docker compose up -d && sleep 5
curl -s http://localhost:8080/health | grep '"status":"ok"'
```

Open **http://localhost:8080** — the first visit walks you through creating
the admin account (`/setup`). Then verify the feature surface:

```bash
# 3. 注册 Runner：浏览器打开 http://localhost:8080 → /setup 创建管理员
#    → Runners → New Runner，复制一次性 token，写入容器内
#    /app/buz/runner.toml 后重启容器（docker compose restart）
#    容器内 Manager + Runner 同箱配对，注册后即 online

# 4. 端到端 job：UI 里新建一条管道（一个 `echo` shell 步骤），触发后
#    在 Jobs 页看到它被容器内 Runner 认领 → 执行 → 成功

# 5. 插件渲染演示（可选）：导入仓库里的
#    example/pipelines/oss-config-demo.yml —— 演示 buz/config-fetch +
#    buz/env-render 全链路（先在 UI Secrets 里录入模板引用的 secret）
```

这套演练验证的功能面：Manager/Runner 配对、仪表盘、job 调度与执行、
插件市场、secret 静态加密——单机即可考察生产部署的完整功能形态。

清理：`docker compose down`，删除演练目录即可。


## 快速启动（本地开发环境）

> 面向在本仓库工作的贡献者：需要 Rust 工具链、Node.js 与 make。外部考察者请用上方 Standalone 演练。

> For contributors working in this repository — requires a Rust toolchain,
> Node.js, and `make`. Evaluators: use the Standalone walkthrough below.

```bash
# One-step: build dashboard + backend and start Manager
make run
# → http://localhost:8080  (first visit → /setup to create admin)

# Or step by step
make example        # create data/pipelines dirs (configs in example/conf/)
make build          # dashboard + release binaries (UI embedded in buz-manager)
cd example && BUZ_ALLOW_INSECURE_JWT=1 ../target/release/buz-manager
# ↑ the insecure-JWT escape hatch is for local development only

# Runner (register_token from Manager UI → Runners)
export BUZ_REGISTER_TOKEN="your-token"
./target/release/buz-runner -c example/conf/runner.toml

# Docs site (local)
make docs-serve
# → http://127.0.0.1:3001/  (or :1313 depending on Hugo)
```

More: [docs Quick Start](docs/content/docs/00-quick-start/_index.md) · [Deployment](docs/content/docs/03-operations/deployment.md) · [Configuration](docs/content/docs/03-operations/configuration.md) (includes `[standalone]`).


## Architecture

Buz follows three core principles:

- **B (Binary)** — Standalone static binaries. No runtime dependencies.
- **U (Unified)** — Single Manager orchestrates runners across all network segments.
- **Z (Zero)** — Zero-trust security + zero operations burden.

Docs layout: **Quick Start → Architecture** (Authoring / Execution / Platform) → **Development** → **Operations**.

![Buz pipeline steps](docs/static/image/buz_steps.png)

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Backend | Rust, axum, tokio, sqlx, SQLite, russh |
| Frontend | Vue 3, TypeScript, TailwindCSS, shadcn/ui, Xterm.js |
| Docs | Hugo + Hextra theme |

## Project Structure

```
buz/
├── Cargo.toml                 # Workspace (shared / manager / runner / plugin-cli)
├── Makefile
├── crates/
│   ├── shared/                # Protocol, pipeline YAML schema, shared types
│   ├── manager/               # Control plane → buz-manager
│   ├── runner/                # Execution agent → buz-runner, buz-sandboxd
│   └── plugin-cli/            # Plugin authoring tool → buz-plugin
├── dashboard/                 # Vue 3 SPA (embedded into buz-manager at build)
├── docs/                      # Hugo + Hextra documentation site
├── example/                   # Local run scaffold
│   ├── conf/                  # manager.toml / runner.toml
│   └── pipelines/             # Sample pipeline YAML
├── deploy/                    # Packaged configs + ops scripts
│   ├── conf/                  # Docker / release config templates
│   ├── standalone/            # buz-standalone Compose
│   └── nginx/                 # Reverse-proxy examples
├── scripts/                   # Release packaging, e2e helpers, test.sh (feature-grouped test runner)
├── docker/
│   ├── Dockerfile             # buz-manager image
│   ├── Dockerfile.runner      # buz-runner image
│   ├── Dockerfile.standalone  # Manager + Runner (arm64 lab image)
│   ├── Dockerfile.sandboxd    # buz-sandboxd image (container / sandbox / vm isolation tiers)
│   ├── docker-compose.yml     # `make up` / `docker compose -f docker/docker-compose.yml up -d`
│   └── docker-compose.vm.yml  # sandbox (VM mode) compose overlay
└── .github/workflows/         # CI, Docker, release
```

## Security / 安全报告

**Please do not report security vulnerabilities through public GitHub issues.**

Use [Private vulnerability reporting](https://github.com/schiplat/buz/security/advisories/new) — reports stay private between you and the maintainers, with a first response targeted within **7 days**.

**请勿通过公开 issue 报告安全漏洞。** 请使用上方"私有漏洞报告"私下提交（7 天内首次响应）。社区插件的疑似恶意内容同样走 [buz-plugins 私有举报](https://github.com/schiplat/buz-plugins/security/advisories/new)。

### Security posture / 安全状态（2026-11-01）

| Scope | Status |
|-------|--------|
| Rust (`cargo audit`) | No known highs — the two `russh` highs (GHSA-vp5q-ff62-jwqv, GHSA-47hw-gvq5-r2gm) are fixed by `russh` 0.65. Accepted & documented in [SECURITY.md](SECURITY.md): RUSTSEC-2023-0071 (`rsa` Marvin Attack via `russh`, no upstream fix) and RUSTSEC-2026-0386 (`tokio` pinned at 1.53.1, `AsyncFd` unreachable). GitHub Dependabot counts each advisory per manifest, so its headline totals are ~3× the unique set. |
| Frontend (`npm audit`, incl. `--omit=dev`) | 0 vulnerabilities — `follow-redirects` 1.16.2 (GHSA-mrwp-f3q8-3g9p fixed) |

Recent dependency-security fixes (`russh` 0.65, follow-redirects 1.16.2, rustls-webpki, vite, Tailwind v4 chain) ship with the latest release — **deploy the newest tag**. The full policy, safe-harbor terms, and the complete known-limitations list (rate limiting, JWT secret handling, debug mode, CORS, WebSocket auth) live in [SECURITY.md](SECURITY.md).

依赖漏洞随每个版本发布修复，请部署最新 tag；完整安全策略、安全港条款与已知限制清单见 [SECURITY.md](SECURITY.md)。
