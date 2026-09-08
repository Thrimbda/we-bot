# we-bot

`we-bot` 是一个面向个人 Agent Harness 的 Rust 通知网关。服务直接接入腾讯微信 ClawBot 的 iLink HTTP API；它不运行 OpenClaw，也不依赖第三方微信推送服务。

```text
Agent / Script ── REST or MCP ──> we-bot ── Tencent iLink ──> 微信 ClawBot
```

## 接口

- `GET /health`：公开的进程存活检查。
- `POST /notify`：发送一条通知。
- `POST /notify/batch`：批量发送 1–50 条通知。
- `/mcp`：MCP Streamable HTTP，提供 `send_notification` tool。
- `GET /wechat/status`：查询 ClawBot 是否已绑定、是否已取得消息上下文。
- `POST /wechat/login`：创建微信扫码绑定会话。
- `POST /wechat/login/{login_id}/poll`：推进扫码绑定流程。
- `POST /wechat/login/{login_id}/verify`：提交微信要求的数字验证码。

除 `/health` 外，所有接口都只接受 `Authorization: Bearer <token>`。登录响应带 `Cache-Control: no-store`，不会返回 Bot token、微信 user ID 或 context token。

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
| `WE_BOT_DEDUPE_TTL_SECONDS` | 否 | `600` | 去重窗口 |
| `WE_BOT_RATE_LIMIT_PER_MINUTE` | 否 | `60` | 全局每分钟投递上限 |
| `RUST_LOG` | 否 | `we_bot=info` | 日志过滤器 |

状态文件包含敏感凭据，服务会以 `0600` 原子写入；其父目录应只允许服务用户访问。

## 本地运行

```bash
export WE_BOT_API_TOKEN="$(openssl rand -hex 32)"
cargo run
```

## 协议参考

- 腾讯微信团队维护的 [`@tencent-weixin/openclaw-weixin`](https://www.npmjs.com/package/@tencent-weixin/openclaw-weixin)：当前 iLink 登录、长轮询和发送协议参考。
- [`Cp0204/WeClawBot-API`](https://github.com/Cp0204/WeClawBot-API)：独立通知 API 的交互模型参考。

微信 ClawBot / iLink 当前仍处于灰度阶段，上游协议和可用性可能变化。
