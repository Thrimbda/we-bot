# we-bot

`we-bot` 是一个面向个人 Agent Harness 的 Rust 通知网关。服务直接接入腾讯微信 ClawBot 的 iLink HTTP API；它不运行 OpenClaw，也不依赖第三方微信推送服务。

```text
Agent / Script ── REST or MCP ──> we-bot ── Tencent iLink ──> 微信 ClawBot
```

## 接口

- `GET /health`：公开的进程存活检查。
- `GET /`：微信账户与聊天控制台（静态资源位于 `/assets/console.css`、`/assets/console.js`）。
- `GET /auth/config`：查询网页使用的登录方式，不包含凭据。
- `POST /notify`：发送一条通知。
- `POST /notify/batch`：批量发送 1–50 条通知。
- `/mcp`：MCP Streamable HTTP，提供 `send_notification` tool。
- `GET /wechat/status`：查询 ClawBot 是否已绑定、是否已取得消息上下文。
- `GET /wechat/accounts`：列出当前服务器绑定的账户及稳定的公开账户 ID。
- `GET /wechat/accounts/{account_id}/messages`：读取该账户最近 200 条收发消息。
- `POST /wechat/accounts/{account_id}/messages`：向指定账户发送文字消息。
- `POST /wechat/login`：创建微信扫码绑定会话。
- `POST /wechat/login/{login_id}/poll`：推进扫码绑定流程。
- `POST /wechat/login/{login_id}/verify`：提交微信要求的数字验证码。

除 `/health`、`/auth/config`、控制台页面及其静态资源外，接口需要鉴权。账户与聊天接口还支持部署配置的 Auth Mini 会话；通知、MCP、绑定管理接口保持 `Authorization: Bearer <token>`。登录、账户与会话响应带 `Cache-Control: no-store`，不会返回 Bot token、微信内部 user ID 或 context token。

## 账户与聊天控制台

Acorn 上访问 [notify.0xc1.wang](https://notify.0xc1.wang/)，网页会跳转到现有 Auth Mini 登录，完成后自动返回账户列表。已有有效会话时直接打开，网页不需要输入服务 API Token。顶部「退出登录」撤销此次网站会话。

登录网关使用现有 Auth Mini 的身份与用户 ID 访问名单，并为 `notify.0xc1.wang` 单独保存会话。we-bot 通过配置的 loopback `/auth/check` 逐次验证 Cookie，将续期或清除 Cookie 的响应交给浏览器；不信任客户端传入的用户身份请求头。Cookie 鉴权仅用于账户与聊天路由，写请求必须带匹配本站的 `Origin`。Hook、REST、MCP 继续使用原有 API Token。

独立本地运行且未配置 Auth Mini 时，访问 `http://127.0.0.1:3099/` 仍使用 API Token；Token 仅在页面内存中保留。启用 Auth Mini 必须同时配置 `WE_BOT_AUTH_GATEWAY_URL` 与 `WE_BOT_CONSOLE_ORIGIN`，并由反向代理将 `/login`、`/auth/callback`、`/auth/callback/session`、`/logout` 路由到对应的 Auth Mini gateway。网关不可用时拒绝访问，不退回无鉴权模式。

控制台展示已绑定账户；点击账户后可发送消息、填入测试文案，并查看微信用户的回信。消息每 3 秒自动读取，页面隐藏时暂停；阅读历史时新消息不会强制滚到底部。手机上点击账户进入会话，使用返回按钮回到账户列表。

账户接口使用数组结构，目前包含 0 或 1 个账户。`account_id` 是服务端持久化的公开标识，所有会话查询与发送都按它定位；不存在的 ID 返回 `404`，不会退回到默认账户。服务端保留 `account_id → iLink bot / 微信用户` 的对应关系。同一个 bot 和微信用户重新绑定时保留账户 ID 与会话；换成不同绑定时生成新 ID 并清空当前会话。当前版本不提供多账户绑定引擎或前端扫码绑定。

向指定账户发送消息：

```bash
curl "https://notify.example.com/wechat/accounts/$ACCOUNT_ID/messages" \
  -H "Authorization: Bearer $WE_BOT_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"text":"这是一条测试消息，请在微信里回复。"}'
```

文字限制为 1–4,000 个字符，与 REST/MCP 通知共用发送实现和全局限流。发送成功仅表示 iLink 已接受，不代表微信用户已读；失败时控制台保留草稿，不自动重发。若 iLink 已接受而本地记录写入失败，响应中的 `history_saved` 为 `false`，避免把存储失败误报为发送失败。

收发记录随状态文件以 `0600` 权限保存，最多保留当前账户最近 200 条。从此版本开始接收到的消息才会记录，无法补齐升级前的聊天历史。文字及语音转写可直接展示，图片、视频、文件显示类型提示，媒体内容需在微信中查看。来自其他微信用户的消息与机器人回声不会进入当前账户会话。微信昵称、头像不在当前上游登录响应中，控制台使用公开账户 ID 的短标识展示。

旧版状态文件会自动迁移到 v2，并保留原有凭据、上下文及游标。旧版程序不读取 v2 状态；回退程序版本时需恢复升级前的状态文件。

前端文件位于 `web/`，编译时嵌入 Rust 二进制，无需单独构建或托管前端。设计参考与本项目取舍见 `DESIGN.md`。

## 扫码绑定

创建登录会话：

```bash
curl -X POST https://notify.example.com/wechat/login \
  -H "Authorization: Bearer $WE_BOT_API_TOKEN"
```

响应中的 `qr_content` 需要渲染成二维码并用微信扫描。扫码及手机确认后，轮询：

```bash
curl -X POST "https://notify.example.com/wechat/login/$LOGIN_ID/poll" \
  -H "Authorization: Bearer $WE_BOT_API_TOKEN"
```

如果状态为 `verification_required`，提交微信显示的数字：

```bash
curl -X POST "https://notify.example.com/wechat/login/$LOGIN_ID/verify" \
  -H "Authorization: Bearer $WE_BOT_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"code":"1234"}'
```

绑定成功后，还需要在微信中主动给新 ClawBot 发送任意一条消息。服务的长期轮询会保存该消息携带的 `context_token`；`GET /wechat/status` 返回 `ready` 后才能主动推送通知。

当前实现固定为单账号，并且只接受扫码绑定时返回的微信 user ID 所发送的上下文，其他发送者不能改变通知目标。

## 发送通知

```bash
curl https://notify.example.com/notify \
  -H "Authorization: Bearer $WE_BOT_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "source": "codex",
    "event": "task.completed",
    "title": "任务完成",
    "body": "PR 已合并并通过验证",
    "url": "https://github.com/example/repo/pull/1",
    "priority": "normal",
    "dedupe_key": "task-019203"
  }'
```

`title` 和 `body` 必填，其余字段可省略。格式化后的微信文本不能超过 4,000 个字符。`dedupe_key` 在默认 10 分钟窗口内只投递一次；去重状态保存在内存中，进程重启后清空。

## 配置

| 环境变量 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `WE_BOT_API_TOKEN` / `WE_BOT_API_TOKEN_FILE` | 是 | — | 二选一；Bearer token，32–512 字节 |
| `WE_BOT_STATE_PATH` | 否 | `./data/state.json` | iLink 凭据、同步游标及 context token 的状态文件 |
| `WE_BOT_BIND_ADDR` | 否 | `127.0.0.1:3099` | 监听地址 |
| `WE_BOT_ALLOWED_HOSTS` | 否 | `localhost,127.0.0.1,::1` | MCP Host 白名单，逗号分隔 |
| `WE_BOT_AUTH_GATEWAY_URL` | 否 | — | Auth Mini gateway 的 HTTP loopback origin，例如 `http://127.0.0.1:7783` |
| `WE_BOT_CONSOLE_ORIGIN` | 否 | — | 与 gateway 公网 origin 一致，例如 `https://notify.0xc1.wang`；与上项成对配置 |
| `WE_BOT_DEDUPE_TTL_SECONDS` | 否 | `600` | 去重窗口 |
| `WE_BOT_RATE_LIMIT_PER_MINUTE` | 否 | `60` | 全局每分钟投递上限 |
| `RUST_LOG` | 否 | `we_bot=info` | 日志过滤器 |

状态文件包含敏感凭据，服务会以 `0600` 原子写入；其父目录应只允许服务用户访问。

## 本地运行

```bash
export WE_BOT_API_TOKEN="$(openssl rand -hex 32)"
cargo run
```

## 验证

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features

# 仅浏览器测试需要 Node.js；运行服务本身不需要。
npm ci
npx playwright install chromium
npm test
```

浏览器测试自行启动 `127.0.0.1:3119` 上的隔离服务，并拦截微信账户与会话请求使用模拟数据；不会向真实微信发送消息。已安装 Chrome 的本地环境也可运行 `PLAYWRIGHT_CHANNEL=chrome npm test`。测试涵盖账户切换、收发、中文输入法、失败草稿、鉴权失效、历史滚动，以及桌面与手机布局；不替代真实微信收发验证。

## 协议参考

- 腾讯微信团队维护的 [`@tencent-weixin/openclaw-weixin`](https://www.npmjs.com/package/@tencent-weixin/openclaw-weixin)：当前 iLink 登录、长轮询和发送协议参考。
- [`Cp0204/WeClawBot-API`](https://github.com/Cp0204/WeClawBot-API)：独立通知 API 的交互模型参考。

微信 ClawBot / iLink 当前仍处于灰度阶段，上游协议和可用性可能变化。
