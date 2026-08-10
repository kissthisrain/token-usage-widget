# Token 用量小组件（Windows 桌面 · Rust + Tauri）

玻璃拟态深色风格的桌面悬浮小组件，**实时**展示本机 AI 工具的 token 消耗、额度剩余、使用趋势与活跃天数。

> 本分支为 Rust + Tauri 重写版（v0.1.0）。原 Electron 版位于 `main` 分支。

## 功能

- **实时总量**：TOTAL TOKENS 大数字带滚动动画，每次 AI 请求完成立即跳动
- **费用**：总费用（USD），支持按模型价目表折算
- **额度剩余**：各数据源剩余百分比进度条 + 重置倒计时
  - OpenCode Go：5 小时 $12 / 每周 $30 / 每月 $60（官方限额，美元计，本地估算）
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

> ⏳ 一期状态：本地代理（SSE 流式转发）与 OpenCode Go 官方用量抓取在二期实现，当前 OpenCode Go 使用本地估算（`Go 估算`）。

## 环境要求

- Windows 10/11（含 WebView2 运行时，Win10/11 系统自带）
- [Rust](https://rustup.rs)（stable，含 MSVC 目标）+ [VS 2022 Build Tools](https://visualstudio.microsoft.com/zh-hans/downloads/)（勾选「使用 C++ 的桌面开发」）——Rust 在 Windows 编译必须 MSVC 链接器
- Node.js ≥ 18（仅用于前端脚手架与 tauri CLI）

国内网络建议配置镜像：`RUSTUP_DIST_SERVER=https://rsproxy.cn`、`~/.cargo/config.toml` 的 crates.io 源替换为 `sparse+https://rsproxy.cn/index/`。

## 安装与运行

### 开发模式

```bash
npm install            # 安装 @tauri-apps/cli
npm run dev            # 开发模式（热重载）
```

### 打包（生成安装包）

```bash
npm run build          # 产物在 src-tauri/target/release/bundle/
```

### 冒烟测试（无界面验证数据管道）

```bash
cargo run -p token-usage-widget -- --smoke   # 或构建后运行 exe --smoke
```

### 使用

- 托盘图标：显示/隐藏、设置、退出
- 全局热键：`Ctrl+Alt+U`（设置中可改）
- 窗口：无边框半透明（Win11 亚克力毛玻璃）、置顶悬浮、可拖动（顶部/底部区域）、位置记忆
- 开机自启：设置 → 通用 → 开机自启

### 首次使用

1. 启动后自动探测本机工具（ZCode / Codex / OpenCode CLI），勾选启用即可看到真实数据
2. 需要 API 类数据源时：设置 → 数据源 → 添加 → 填写配置 → 「测试连接」

### OpenCode Go 官方剩余用量查询（二期支持）

OpenCode Go **没有公开的用量 JSON API**，官方剩余额度在二期通过「登录态 Cookie + 工作区 ID」抓取控制台页面获取（当前版本显示本地估算）。

## 开发命令

```bash
npm run dev            # 开发模式
cargo clippy           # lint
cargo test             # 单元测试
npm run icon           # 重新生成应用图标（resources/icon-1024.png → src-tauri/icons/）
```

## 数据与隐私

- 所有数据仅存本机：`%APPDATA%/token-usage-widget/`（config.json 配置、history.json 历史聚合、文件监听偏移量；与 Electron 版数据兼容，无缝迁移）
- API Key 仅保存在本地配置文件中，只用于直连你配置的提供商接口
- 本地记录读取的是 ZCode/Codex 自己的日志文件，只读不修改

## 已知限制

- OpenAI 用量接口需组织管理员 Key（普通 Key 会 403，测试连接会给出明确提示）
- 一期版本：OpenCode Go 本地代理与官方用量抓取未实现（显示本地估算）
- 本地 rollout 文件可能只保留近期，历史数据以 `.codex/sessions` 为主
- 若同时启用「本地记录」与「OpenAI 官方」统计同一提供商，数据会重复，建议二选一
