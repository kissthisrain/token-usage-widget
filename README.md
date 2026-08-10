# Token 用量小组件（Windows 桌面）

玻璃拟态深色风格的桌面悬浮小组件，**实时**展示本机 AI 工具的 token 消耗、额度剩余、使用趋势与活跃天数。

## 功能

- **实时总量**：TOTAL TOKENS 大数字带滚动动画，每次 AI 请求完成立即跳动
- **费用**：总费用（USD），支持按模型价目表折算
- **额度剩余**：各数据源剩余百分比进度条 + 重置倒计时
  - OpenCode Go：5 小时 $12 / 每周 $30 / 每月 $60（官方限额，美元计）
  - Codex：本地会话记录中的周配额（used_percent + resets_at）
  - 兼容中转站：余额接口自动探测（billing / balance / auth-key / one-api 四种风格）
- **模型明细**：按模型分组的 token 消耗与占比进度条
- **趋势**：面积折线图（标注峰值）
- **活动**：GitHub 风格热力图 + 活跃天数
- **时间筛选**：DAY / MONTH / TOTAL 三档

## 数据源（4 类）

| 类型 | 配置 | 说明 |
|---|---|---|
| **本地记录** | 自动探测（零配置） | 实时监听 ZCode（`~/.zcode/cli/rollout/model-io-*.jsonl`）与 Codex（`~/.codex/sessions/**/rollout-*.jsonl`）会话日志，毫秒级推送。ZCode 实时日志只保留当天，**历史用量自动从 `~/.zcode/cli/db/db.sqlite` 的 model_usage 表回填**（只读，按天去重，热力图/趋势立刻有 30 天数据） |
| **OpenAI 官方** | Base URL + API Key | 用量 API（`/v1/usage`），需**组织管理员** Key；支持回填 90 天历史 |
| **兼容中转站** | Base URL + API Key | 自动探测常见余额接口，或自定义 URL + 字段映射 |
| **OpenCode Go** | 本地代理（端口） | 客户端指向 `http://127.0.0.1:<端口>`，代理转发到 `opencode.ai/zen/go` 并记录每次请求，对照官方限额计算剩余% |

## 安装与运行

### 方式一：直接运行 exe（推荐）

```bash
npm run package     # 打包到 dist/token-usage-widget/
```

然后双击 `dist\token-usage-widget\token-usage-widget.exe` 即可运行——无需安装 Node/npm，可把该目录复制到任意位置，或右键发送快捷方式到桌面。

### 方式二：开发模式运行

```bash
npm install          # 国内网络建议先设置 ELECTRON_MIRROR=https://npmmirror.com/mirrors/electron/
npm start            # 启动小组件
```

- 托盘图标：显示/隐藏、设置、退出
- 全局热键：`Ctrl+Alt+U`（设置中可改）
- 窗口：无边框半透明、置顶悬浮、可拖动（顶部/底部区域）、位置记忆
- 开机自启：设置 → 通用 → 开机自启

### 首次使用

1. 启动后自动探测本机工具（ZCode / Codex / OpenCode CLI），勾选启用即可看到真实数据
2. 需要 API 类数据源时：设置 → 数据源 → 添加 → 填写配置 → 「测试连接」

### OpenCode Go 使用方式

1. 设置 → 数据源 → 添加「OpenCode Go 套餐」
2. 点「启动代理」（默认端口 12345）
3. 把客户端的 OpenAI 兼容 Base URL 指向 `http://127.0.0.1:12345`（保留 `/v1` 路径，Key 不变）
4. 之后每次请求都会记录 token 与费用，小组件实时显示三个限额窗口的剩余%

### OpenCode Go 官方剩余用量查询（推荐）

OpenCode Go **没有公开的用量 JSON API**（`/v1/usage`、`/v1/balance` 均返回 404），剩余额度只存在于官网控制台页面。小组件通过「登录态 Cookie + 工作区 ID」抓取控制台内嵌数据，拿到**官方权威**的 5 小时 / 每周 / 每月剩余用量（本地代理估算只统计本机，官方数据包含所有设备）。

1. 浏览器登录 [opencode.ai](https://opencode.ai) → 打开用量控制台，地址栏形如 `https://opencode.ai/workspace/{id}/go`，复制 `wrk_` 开头的**工作区 ID**
2. 控制台页面按 `F12` → Application / 存储 → Cookies → `opencode.ai`，复制名为 `auth` 的 Cookie 值
3. 设置 → OpenCode Go 数据源 → 填入「工作区 ID」与「Auth Cookie」→ 保存 → 点「测试连接」验证
4. 之后小组件定时刷新官方数据，配额卡片标记「Go 官方」；未配置或 Cookie 失效时自动回退为本地代理估算（标记「Go 估算」）

> ⚠️ Cookie 等同你的登录态，**仅保存在本机配置文件**中，请勿外传；若提示「会话已过期」，重新登录后复制最新 Cookie 即可。

## 开发命令

```bash
npm start            # 正常启动
npm run smoke        # 冒烟测试：无界面验证探测/解析/聚合，输出数据画像
electron . --shot <path.png> [settings]   # 截图调试模式（截小组件或设置窗口）
```

## 数据与隐私

- 所有数据仅存本机：`%APPDATA%/token-usage-widget/`（config.json 配置、history.json 历史聚合、文件监听偏移量）
- API Key 仅保存在本地配置文件中，只用于直连你配置的提供商接口
- 本地记录读取的是 ZCode/Codex 自己的日志文件，只读不修改

## 已知限制

- OpenAI 用量接口需组织管理员 Key（普通 Key 会 403，测试连接会给出明确提示）
- Electron 透明窗口不支持桌面级 backdrop blur，以半透明深色模拟玻璃拟态
- 本地 rollout 文件可能只保留近期，历史数据以 `.codex/sessions` 为主
- 若同时启用「本地记录」与「OpenAI 官方」统计同一提供商，数据会重复，建议二选一
