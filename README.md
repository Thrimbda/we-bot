# we-bot

`we-bot` 是一个面向个人 Agent Harness 的小型通知网关。调用方只需要向统一的 REST 或 MCP 接口发送消息；服务端保存 WxPusher SPT，并把消息转成微信通知。

## 接口

- `GET /health`：公开的进程存活检查。
- `POST /notify`：发送一条通知。
- `POST /notify/batch`：批量发送 1–50 条通知。
- `/mcp`：MCP Streamable HTTP，提供 `send_notification` tool。

除 `/health` 外，所有接口都要求 `Authorization: Bearer <token>`。

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

`title` 和 `body` 必填，其余字段可省略。`dedupe_key` 在默认 10 分钟窗口内只投递一次；该状态保存在内存中，进程重启后会清空。批量接口接受：

```json
{
  "notifications": [
    { "title": "任务一完成", "body": "结果 A" },
    { "title": "任务二完成", "body": "结果 B" }
  ]
}
```

## 配置

| 环境变量 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `WE_BOT_API_TOKEN` / `WE_BOT_API_TOKEN_FILE` | 是 | — | 二选一；Bearer token，32–512 字节 |
| `WXPUSHER_SPT` / `WXPUSHER_SPT_FILE` | 是 | — | 二选一；WxPusher `SPT_...` |
| `WE_BOT_BIND_ADDR` | 否 | `127.0.0.1:3099` | 监听地址 |
| `WE_BOT_ALLOWED_HOSTS` | 否 | `localhost,127.0.0.1,::1` | MCP Host 白名单，逗号分隔 |
| `WE_BOT_DEDUPE_TTL_SECONDS` | 否 | `600` | 去重窗口 |
| `WE_BOT_RATE_LIMIT_PER_MINUTE` | 否 | `60` | 全局每分钟投递上限 |
| `RUST_LOG` | 否 | `we_bot=info` | 日志过滤器 |

SPT 获取方式见 [WxPusher 官方 SPT 文档](https://wxpusher.zjiecode.com/docs/spt.html)。不要把 SPT 或 API token 提交到仓库、命令历史或日志。

## 本地运行

```bash
export WE_BOT_API_TOKEN="$(openssl rand -hex 32)"
export WXPUSHER_SPT="SPT_xxx"
cargo run
```

服务会把正文作为 Markdown 发送给 WxPusher。WxPusher 返回业务码 `1000` 才视为投递已受理；网络失败或业务拒绝会返回 `502`，但不会向调用方暴露上游凭据或完整错误。
