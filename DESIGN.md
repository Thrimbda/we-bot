---
name: we-bot
description: 清楚、安静的微信账户与会话操作界面
colors:
  canvas: "#ffffff"
  canvas-soft: "#fafafa"
  canvas-hover: "#f3f4f3"
  ink: "#171717"
  ink-secondary: "#363b38"
  muted: "#6a716d"
  line: "#e3e7e4"
  line-strong: "#c7ceca"
  primary: "#3ecf8e"
  primary-hover: "#31bf80"
  green-ink: "#12633f"
  green-soft: "#edf8f1"
  outgoing: "#e9f5ee"
  warning-bg: "#fff8e9"
  warning-ink: "#815411"
  error: "#a83530"
typography:
  headline:
    fontFamily: '-apple-system, BlinkMacSystemFont, "PingFang SC", "Noto Sans CJK SC", "Microsoft YaHei", sans-serif'
    fontSize: "26px"
    fontWeight: 500
    letterSpacing: "-0.02em"
  body:
    fontFamily: '-apple-system, BlinkMacSystemFont, "PingFang SC", "Noto Sans CJK SC", "Microsoft YaHei", sans-serif'
    fontSize: "13px"
    fontWeight: 400
  label:
    fontFamily: '-apple-system, BlinkMacSystemFont, "PingFang SC", "Noto Sans CJK SC", "Microsoft YaHei", sans-serif'
    fontSize: "13px"
    fontWeight: 500
  supporting:
    fontSize: "12px"
    fontWeight: 400
rounded:
  control: "6px"
  row: "8px"
  inset: "10px"
  panel: "12px"
spacing:
  compact: "8px"
  small: "12px"
  medium: "16px"
  roomy: "24px"
components:
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.ink}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "8px 13px"
  button-primary-hover:
    backgroundColor: "{colors.primary-hover}"
  button-secondary:
    backgroundColor: "{colors.canvas}"
    textColor: "{colors.ink-secondary}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "8px 13px"
  button-icon:
    textColor: "{colors.muted}"
    rounded: "{rounded.control}"
    padding: "8px"
    width: "36px"
    height: "36px"
  button-text:
    textColor: "{colors.muted}"
    typography: "{typography.supporting}"
    padding: "8px 0"
  credential-field:
    backgroundColor: "{colors.canvas}"
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
    padding: "10px 12px"
    height: "44px"
  account-row:
    textColor: "{colors.ink}"
    rounded: "{rounded.row}"
    padding: "15px 11px"
  account-row-current:
    backgroundColor: "{colors.green-soft}"
  message-incoming:
    backgroundColor: "{colors.canvas-soft}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    padding: "11px 15px"
  message-outgoing:
    backgroundColor: "{colors.outgoing}"
    typography: "{typography.body}"
    padding: "11px 15px"
  composer:
    backgroundColor: "{colors.canvas}"
    rounded: "{rounded.inset}"
    padding: "13px 14px 10px"
---

# Design System: we-bot

## Overview

**Creative North Star: "清楚的账户与会话"**

这是从已实现界面提炼的描述：白色操作面、浅灰账户区、细线和少量翡翠绿，把视觉注意力留给账户选择与会话阅读。信息密度适中，状态和操作直接使用中文。

依据为 `web/index.html`、`web/console.css`、`web/console.js`，参考为 [Supabase DESIGN.md](https://getdesign.md/supabase/design-md)。浅色和中文系统字体属于本次实现选择；这里不宣称存在另行批准的品牌方案或视觉稿。

**Key Characteristics:**

- 白与浅灰区分区域，细线区分相邻内容。
- 翡翠绿标出主操作，深绿保证文字与焦点的可读性。
- 账户身份、消息方向和发送状态都有文字依据。
- 窄屏逐层进入会话，更新尊重阅读位置。

## Colors

### Primary

`primary` 用于连接、发送与品牌标记；`primary-hover` 表达按钮悬停。`green-ink` 用于可发送状态、焦点与浅色底上的绿色文字；`green-soft` 用于当前账户与输入焦点底色。`outgoing` 将发送消息轻柔地从接收消息中区分出来。

**The Readable Green Rule.** 明亮翡翠绿承载深色按钮文字；需要绿色文字时采用深绿，不把主色直接当作小字颜色。

### Neutral

`canvas` 为主要阅读面，`canvas-soft` 为账户区、展开信息与接收气泡，`canvas-hover` 为轻量悬停。`ink`、`ink-secondary`、`muted` 依次承担正文、次级说明和辅助信息；`line` 用于分隔，`line-strong` 强化输入边界。

等待与连接问题使用 `warning-bg` / `warning-ink`；错误文字使用 `error`。它们是状态色，不构成第二套品牌强调色。颜色始终伴随状态文字。

Sidecar 中的色阶由实际颜色生成，仅供预览，不新增界面颜色令牌。

## Typography

中文优先，沿用前言中的系统无衬线栈，不加载展示字体。页面标题使用 `headline`；常规消息与控件使用 `body` / `label`，消息行高较宽（1.85），说明文字通常为 1.6–1.9。根字号为 14px。

手机消息正文增至 14px，输入字段与消息编辑区增至 16px。时间、计数、页脚等现有 10–11px 局部辅助文字不作为新增正文的尺寸基准。公开账户 ID 的展开详情使用系统等宽字体，时间与计数使用等宽数字。

## Layout

桌面内容居中，最大宽度为 1376px，常规左右内边距为 40px。账户与会话组成一个有边界的工作区；账户列宽 302px，右侧自适应。账户列表与聊天记录各自滚动，聊天头部和编辑区保留在各自位置。间距以已记录的 8、12、16、24px 为常用步长，边界与组件内部也保留必要的局部值。

宽度不超过 1050px 时，外边距降为 24px、账户列收为 258px。不超过 720px 时先显示账户列表；选中后会话占满剩余视口，隐藏账户列和页面标题，以明确的返回按钮回到列表。手机底部编辑区计入安全区域。不小于 1700px 时仅适当增加顶部留白。

## Elevation & Depth

当前系统没有阴影。层级来自浅灰底、单像素边界、留白和选中底色；浮在聊天底部的“查看新消息”仍采用白底细边。焦点轮廓属于交互反馈，不是抬升效果。

## Shapes

控件使用 `control` 圆角；账户行使用 `row`，编辑区使用 `inset`，会话外框使用 `panel`。聊天气泡的顶部近发送方一角收紧，左侧接收、右侧发送。SVG 线性图标保持轻量笔画；通用“微”字标记仅作账户占位，不表示真实头像或图标规范。

## Components

- **按钮：**主按钮深色字配翡翠绿底，次按钮白底细边，图标按钮和填入测试消息按钮保持透明。禁用时降低透明度并阻止操作。桌面图标按钮为 36px，手机为 40px；手机发送按钮也扩大点击高度。
- **登录与编辑区：**部署环境自动跳转 Auth Mini，已有会话直接进入账户列表；登录过期或暂不可用时用同一简洁提示面提供重新登录、重试。未配置 Auth Mini 的独立本地服务保留密钥字段。字段与编辑区为白底、较强细边；字段焦点使用深绿边与浅绿外轮廓，编辑区以深绿边框回应内部焦点。错误放在相关输入附近，保留可修正的草稿。
- **账户导航：**每行提供明确名称、状态、最近消息预览；长名称和预览省略。当前行有浅绿底和边线，使用 `aria-current` 标识。映射详情按需展开，长 ID 可换行。
- **会话：**接收消息左对齐、浅灰底细边；发送消息右对齐、浅绿底。消息保留换行并允许长串换行，桌面最大宽度为 `min(82%, 66ch)`，手机为 88%。元信息和“正在发送 / 已提交微信”文案与消息分开。

**The Reader Position Rule.** 用户已接近底部时跟随更新；正在读历史时保留可见消息位置，并提供“查看新消息”。主动打开会话或发送消息可以定位到底部。

按钮和字段仅用短暂颜色过渡（160ms，`cubic-bezier(.22, 1, .36, 1)`）；减少动态效果偏好关闭过渡。交互元素有清楚的键盘焦点。聊天记录本身不整段播报，独立的礼貌播报区域提示新增消息与发送状态，错误使用就近警报。

## Do's and Don'ts

### Do:

- **Do** 用文字补足状态颜色，明确账户、消息方向与发送阶段。
- **Do** 用灰阶分区和细线保持阅读层级，让主操作自然突出。
- **Do** 在手机保留返回账户列表的路径，并尊重历史阅读位置。

### Don't:

- **Don't** 把亮绿色用作白底辅助小字，或把局部微型文字推广为正文尺度。
- **Don't** 用虚构昵称、个人头像或消息填充运行中的空状态。
- **Don't** 将“已提交微信”表现为已读，或让整段历史因轮询而重复播报。
