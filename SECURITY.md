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
- **One secret, two uses.** `auth.jwt_secret` both signs tokens and derives the secrets-encryption key (unsalted SHA-256, no rotation). Treat it as a root key: read access to it means access to every stored secret, and rotating it invalidates all stored secrets. A dedicated master key with data migration is planned.
- **`debug_unredact` persists plaintext.** An admin-triggered run with `debug_unredact` stores unredacted secret values in the job-log table.
- **Permissive CORS.** `Any/Any/Any` — low risk under Bearer-token auth, not hardened.
- **WebSocket token in URL.** The live-log stream authenticates via a token in the URL query string, which can appear in reverse-proxy access logs.
- **`/setup` race.** A narrow window allows two concurrent first-admin creations.
- **Password change does not revoke sessions.** Existing JWTs stay valid for up to 24 hours.
- **Audit log is best-effort.** Not tamper-proof; source IPs are not recorded; first-admin creation is not audited.
- **Runner path & TLS options.** `config_dest` accepts absolute paths and does not normalize relative joins; `tls_skip_verify` disables certificate verification (labs only). The Manager clock probe accepts any host key by design — it is a probe, not a trust boundary.
- **Runner tenancy boundary.** Team RBAC isolates Manager resources, but job execution does not create a per-job OS identity: all steps on a Runner run under the Runner's OS user. A shell job can read the Runner's token file (`0600` is meaningless against the same UID), other jobs' accessible workspace files, and any process environment reachable by that user; per-job workspaces, `0700` Docker-config dirs, log masking, and runner labels are hygiene or scheduling selectors — **not** security boundaries between jobs. Optional team binding (`runners.team_id`) restricts which team's jobs the Manager dispatches to a Runner; it does not sandbox jobs from one another. Do not share a Runner (or its host) across mutually untrusted teams — use a dedicated Runner host/OS user per trust boundary, and treat Docker socket access as host-root-equivalent.
- **Credentials on Runners (template model).** Step credentials (registry passwords, presigned URLs) transit the Runner process environment as masked values; same-user processes can read them for the step's lifetime. On ssh target hosts, `docker logout` empties the credential file and the paired logout step removes the job-scoped directory — a cancelled job leaves cleanup to the host's `/tmp` hygiene.
- **Dependency advisory without an upstream fix.** The `rsa` crate (pulled in via `russh` SSH support) is affected by [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071) (Marvin Attack, timing side channel); no fixed release exists at the time of writing. Impact is limited to RSA key exchange / RSA cipher paths in SSH connections — prefer Ed25519 keys and modern ciphers.

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
- **一钥两用**：`auth.jwt_secret` 既签发 JWT 又派生密文加密密钥（无盐 SHA-256、无轮换）。把它当根密钥保护：能读到它即能解密全部已存密文，轮换会使已存密文全部失效。独立主密钥 + 数据迁移在规划中。
- **`debug_unredact` 落盘明文**：管理员触发的调试运行会把未打码的密文值写入数据库日志表。
- **CORS 全放开**：Bearer 认证下风险较低，未收紧。
- **WS 日志流 token 走 URL 查询参数**：可能进入反代访问日志。
- **`/setup` 并发竞态**（窗口极小）；改密后旧 JWT 24 小时内仍有效。
- **审计日志 best-effort**：非防篡改、不记录来源 IP、首管理员创建未审计。
- **Runner 路径与 TLS 选项**：`config_dest` 接受绝对路径、相对拼接不归一化；`tls_skip_verify` 仅限实验环境；Manager 时钟探测不校验主机密钥（仅探测用途，非信任边界）。
- **Runner 租户边界**：团队 RBAC 隔离的是 Manager 资源，job 执行没有每-job OS 身份——同一 Runner 上所有步骤以 Runner 的 OS 用户运行。shell job 可读取 Runner 的 token 文件（`0600` 对同 UID 无意义）、其他 job 的可访问工作区文件，以及该用户可达的进程环境；per-job workspace、`0700` Docker config 目录、日志打码和 runner labels 都只是卫生措施或调度选择器，**不是** job 之间的安全边界。可选的 team 绑定（`runners.team_id`）只限制 Manager 把哪个 team 的 job 派到该 Runner，不隔离 job 之间。互不信任的 team 不要共享 Runner（或其主机）——按信任边界使用独立 Runner 主机/OS 用户，Docker socket 访问视同主机 root。
- **Runner 上的凭证（模板模型）**：步骤凭证（镜像仓库密码、预签名 URL）以打码形式经过 Runner 进程环境，同用户进程在步骤存续期内可读；SSH 目标机上 `docker logout` 会清空凭据文件、配对登出步会移除 job 级目录——job 被取消时，清理依赖目标机 `/tmp` 机制。
- **暂无上游修复的依赖通告**：`rsa` crate（经 `russh` SSH 支持引入）受 [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071)（Marvin Attack，计时侧信道）影响，暂无修复版本。影响面限于 SSH 连接中的 RSA 密钥交换 / RSA 密码套件——建议优先使用 Ed25519 密钥与现代密码套件。

## 中文 / Chinese

**请勿通过公开 issue 报告安全漏洞。**

- Buz 本体漏洞：<https://github.com/schiplat/buz/security/advisories/new>
- 官方模板疑似后门/恶意行为：<https://github.com/schiplat/buz-plugins/security/advisories/new>（细节勿公开，私下分诊）

安全报告目标 **7 天内首次响应**；修复发布后致谢（可匿名）。以善意方式进行研究即视为已获授权（safe harbor）。
