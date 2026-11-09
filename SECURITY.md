# Security Policy

**English below / 中文见下方。**

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Use GitHub **Private vulnerability reporting**:

- Buz (this repository): <https://github.com/schiplat/buz/security/advisories/new>
- Official templates (buz-plugins): <https://github.com/schiplat/buz-plugins/security/advisories/new> — for suspected malicious or backdoored **step templates**, do not post details publicly

Include as much of the following as possible: affected component/version, reproduction steps, impact, and any proof-of-concept. Please keep proof-of-concept details private until a fix is coordinated.

## What to expect

- **First response within 7 days** of your report (holidays may add delay).
- We will keep you updated as we triage and work on a fix.
- Once a fix is released, we will publish an advisory and credit you in the release notes (unless you prefer to remain anonymous).

## Safe harbor

We consider good-faith security research of Buz to be authorized: we will not pursue legal action against anyone who reports vulnerabilities responsibly, avoids privacy violations and service disruption, and gives us reasonable time to fix issues before public disclosure.

## Known limitations

Buz ships with the following known limitations. They are tracked and will be addressed over time; deploy with the listed mitigations in mind.

- **No rate limiting.** Authentication, `/setup`, webhook, and runner-registration endpoints have no built-in rate limiting. Deploy behind a reverse proxy with rate limiting (sample configs in `deploy/nginx/`).
- **One secret, two uses.** `auth.jwt_secret` both signs tokens and derives the secrets-encryption key (unsalted SHA-256, no rotation). Treat it as a root key: read access to it means access to every stored secret, and rotating it invalidates all stored secrets until re-encrypted — `buz-manager rotate-jwt-key` performs that rotation in place (plan/apply, snapshot + single transaction). A dedicated master key with data migration is planned.
- **`debug_unredact` persists plaintext.** An admin-triggered run with `debug_unredact` stores unredacted secret values in the job-log table.
- **Permissive CORS.** `Any/Any/Any` — low risk under Bearer-token auth, not hardened.
- **WebSocket token in URL.** The live-log stream authenticates via a token in the URL query string, which can appear in reverse-proxy access logs.
- **`/setup` race.** A narrow window allows two concurrent first-admin creations.
- **Password change does not revoke sessions.** Existing JWTs stay valid for up to 24 hours.
- **Audit log is best-effort.** Not tamper-proof; source IPs are not recorded; first-admin creation is not audited.
- **Runner path & TLS options.** `config_dest` accepts absolute paths and does not normalize relative joins; `tls_skip_verify` disables certificate verification (labs only).
- **Runner tenancy boundary.** Team RBAC isolates Manager resources, but job execution does not create a per-job OS identity: all steps on a Runner run under the Runner's OS user. A shell job can read the Runner's token file (`0600` is meaningless against the same UID), other jobs' accessible workspace files, and any process environment reachable by that user; per-job workspaces, `0700` Docker-config dirs, log masking, and runner labels are hygiene or scheduling selectors — **not** security boundaries between jobs. Optional team binding (`runners.team_id`) restricts which team's jobs the Manager dispatches to a Runner; it does not sandbox jobs from one another. Do not share a Runner (or its host) across mutually untrusted teams — use a dedicated Runner host/OS user per trust boundary, and treat Docker socket access as host-root-equivalent.
- **Sandbox host policy is a quota, not a security boundary.** Since 1.13, `buz-sandboxd` holds no policy file: its policy (resource profiles, tier images/runtimes, job network) is Manager-managed and pulled over HTTPS with a per-instance `buzsd_…` token scoped to one `(host_id, instance_id)`. `runner.resources` authorizes which *teams* may request larger envelopes, and the Manager's slot budget is a dispatch quota — none of these constrain a compromised Runner or supervisor: the Runner still runs untrusted code, the supervisor still holds host-root-equivalent Docker access, and a malicious Runner could still call sandboxd directly within whatever the applied document allows. The supervisor trusts only the Manager it is pinned to (`BUZ_MANAGER_INSTANCE_ID`) and its own root-owned cache; both are integrity and availability controls, not job-isolation boundaries. Treat Docker socket access as host-root-equivalent.
- **Runner execution tiers.** Job execution modes are isolation-strength tiers, weakest first: `host` (L0, the backward-compatible default, no job sandbox), `container` (L1, one hardened Docker container per job — shares the host kernel and trusts the host Docker daemon/supervisor; not a VM boundary), `sandbox` (L2, a userspace-kernel OCI runtime such as gVisor `runsc` — job syscalls never reach the host kernel, but the boundary is the correctness of the emulated kernel, a different risk shape than a VMM's device model), and `vm` (L3, a host-installed Firecracker-compatible OCI runtime; KVM required; Buz does not bundle Firecracker, its kernel, or the guest image). L2 and L3 are different trust trade-offs, not a strict ranking. All sandboxed tiers currently fail closed for SSH steps and per-step Docker-engine steps. Even VM mode does not sandbox the remote machine targeted by SSH (SSH is currently rejected there). The Runner autonomously probes and reports which tiers its host can deliver; a human selects the tier — the system never upgrades or downgrades a Runner's tier automatically, and an unavailable selected tier rejects jobs instead of falling back. Do not treat the tier selector as protection against a compromised host/supervisor/runtime.
- **Credentials on Runners (plugin model).** Since the retirement the kernel holds no registry or object-store credentials: delivery is plugin steps (`buz/docker-login`, `buz/config-fetch`, `buz/oss-download`) whose inputs — passwords, presigned URLs, AK/SK pairs — transit the Runner process environment as masked values, and same-user processes can read them for the step's lifetime. This is the same trust model as mainstream CI (GitHub Actions et al.); prefer short-TTL presigned URLs and least-privilege access keys.
- **SSH engine passes step env in the remote command string.** The SSH engine exports step env — including expanded `${secrets.*}` values — as `export K='v'` statements prepended to the command sent to the target host, so while an SSH step runs its secret values are visible in the remote process's argv (readable in `ps` on the target host by root and the owning user). Local engines are unaffected: values flow through the process environment, never argv. Prefer the "compute locally, transfer the artifact" pattern for secrets-bearing work on SSH targets (prepare/render on the Runner, deliver with `buz/scp`); moving the SSH env channel off the command string is the medium-term fix.
- **Dependency advisories accepted by policy.** Recorded in `.cargo/audit.toml` so `cargo audit` stays meaningful for everything else. (1) The `rsa` crate (pulled in via `russh` SSH support) is affected by [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071) (Marvin Attack, timing side channel); no fixed release exists upstream. Impact is limited to RSA key exchange / RSA cipher paths in SSH connections — prefer Ed25519 keys and modern ciphers. (2) tokio is pinned to 1.53.1 because of [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386) (`AsyncFd` constructors violating I/O safety). This workspace never uses `AsyncFd` and no dependency enables that feature, so the unsound API is unreachable; the pin predates the retirement removal of the clock probes, and the upgrade needs its own regression pass rather than riding along.

## Scope

**In scope:**

- The Buz binaries and dashboard (`buz` repository: manager, runner, shared, dashboard)
- Officially maintained step templates in the `schiplat/buz-plugins` repository (`publisher: official`, `buz/*` namespace)

**Out of scope:**

- Vulnerabilities in third-party community templates outside `schiplat/buz-plugins` — report those to their maintainers
- Self-hosted infrastructure misconfiguration unrelated to Buz behavior (e.g. exposed dashboards of the host itself)
- Reports from automated scanners without a demonstrated impact — include a working proof of concept

## 已知限制

- **无速率限制**：认证、`/setup`、webhook、runner 注册端点未内建限流，建议前置反代限流（`deploy/nginx/` 提供样例）。
- **一钥两用**：`auth.jwt_secret` 既签发 JWT 又派生密文加密密钥（无盐 SHA-256、无轮换）。把它当根密钥保护：能读到它即能解密全部已存密文；轮换后旧密文失效直至重加密——`buz-manager rotate-jwt-key` 可原地完成轮换（plan/apply 两段式、快照 + 单事务）。独立主密钥 + 数据迁移在规划中。
- **`debug_unredact` 落盘明文**：管理员触发的调试运行会把未打码的密文值写入数据库日志表。
- **CORS 全放开**：Bearer 认证下风险较低，未收紧。
- **WS 日志流 token 走 URL 查询参数**：可能进入反代访问日志。
- **`/setup` 并发竞态**（窗口极小）；改密后旧 JWT 24 小时内仍有效。
- **审计日志 best-effort**：非防篡改、不记录来源 IP、首管理员创建未审计。
- **Runner 路径与 TLS 选项**：`config_dest` 接受绝对路径、相对拼接不归一化；`tls_skip_verify` 仅限实验环境。
- **Runner 租户边界**：团队 RBAC 隔离的是 Manager 资源，job 执行没有每-job OS 身份——同一 Runner 上所有步骤以 Runner 的 OS 用户运行。shell job 可读取 Runner 的 token 文件（`0600` 对同 UID 无意义）、其他 job 的可访问工作区文件，以及该用户可达的进程环境；per-job workspace、`0700` Docker config 目录、日志打码和 runner labels 都只是卫生措施或调度选择器，**不是** job 之间的安全边界。可选的 team 绑定（`runners.team_id`）只限制 Manager 把哪个 team 的 job 派到该 Runner，不隔离 job 之间。互不信任的 team 不要共享 Runner（或其主机）——按信任边界使用独立 Runner 主机/OS 用户，Docker socket 访问视同主机 root。
- **Job 执行档位**：执行模式按隔离强度分级，由弱到强：`host`（L0，向后兼容默认值，不提供 job sandbox）、`container`（L1，每个 job 一个加固 Docker 容器——仍共享宿主内核且信任宿主 Docker daemon/supervisor，不等同 VM 边界）、`sandbox`（L2，用户态内核 OCI runtime，如 gVisor `runsc`——job 的 syscall 不再触达宿主内核，但该边界的信任基础是模拟内核实现的正确性，与 VMM 设备模型是不同形态的风险）、`vm`（L3，宿主预装的 Firecracker-compatible OCI runtime，需要 KVM；Buz 不打包 Firecracker、kernel 或 guest image）。L2 与 L3 是不同的信任取舍，不是严格的强弱排序。当前所有 sandbox 档位对 SSH 与 per-step Docker engine 均 fail closed；VM 档也不隔离 SSH 目标机（当前该模式直接拒绝 SSH）。Runner 自主探测并上报宿主可支持的档位；档位由人选择——系统不会自动升降档，所选档位不可用时 job 直接失败而非回退。档位选择不保护 job 免受宿主/supervisor/runtime 被攻陷的影响。
- **Runner 上的凭证（插件模型）**：自内核退役起不再持有任何镜像仓库或对象存储凭证——交付由插件步骤承担（`buz/docker-login`、`buz/config-fetch`、`buz/oss-download`），其输入（密码、预签名 URL、AK/SK）以打码形式经过 Runner 进程环境，同用户进程在步骤存续期内可读。与主流 CI（GitHub Actions 等）同一信任模型；建议使用短 TTL 预签名 URL 与最小权限访问密钥。
- **SSH 引擎把步骤 env 拼进远端命令串**：SSH 引擎将步骤 env——包括展开后的 `${secrets.*}` 值——以 `export K='v'` 形式拼在发往目标主机的命令串前面，因此 SSH 步骤执行期间其 secret 值出现在远端进程的 argv 里（目标主机上 root 与属主用户经 `ps` 可见）。本地引擎不受影响：值走进程环境，不进 argv。对含 secret 的 SSH 目标机操作建议采用"本地计算、传输制品"模式（Runner 上准备/渲染，经 `buz/scp` 交付）；把 SSH env 通道移出命令串是中期修复方向。
- **按策略接受的依赖通告**：记录在 `.cargo/audit.toml`，使 `cargo audit` 对其余依赖保持有效。(1) `rsa` crate（经 `russh` SSH 支持引入）受 [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071)（Marvin Attack，计时侧信道）影响，暂无修复版本；影响面限于 SSH 连接中的 RSA 密钥交换 / RSA 密码套件——建议优先使用 Ed25519 密钥与现代密码套件。(2) tokio 固定在 1.53.1，因 [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386)（`AsyncFd` 构造违反 I/O safety）。本仓库不使用 `AsyncFd`，也没有依赖启用该特性，不安全的 API 不可达；该 pin 早于退役时移除时钟探测，升级需单独做回归验证，不宜顺手带上。

## 中文 / Chinese

**请勿通过公开 issue 报告安全漏洞。**

- Buz 本体漏洞：<https://github.com/schiplat/buz/security/advisories/new>
- 官方模板疑似后门/恶意行为：<https://github.com/schiplat/buz-plugins/security/advisories/new>（细节勿公开，私下分诊）

安全报告目标 **7 天内首次响应**；修复发布后致谢（可匿名）。以善意方式进行研究即视为已获授权（safe harbor）。
