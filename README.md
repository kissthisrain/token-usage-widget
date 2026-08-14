# Token 用量小组件（Windows 桌面 · Rust + Tauri）

玻璃拟态深色风格的桌面悬浮小组件，**实时**展示本机 AI 工具的 token 消耗、额度剩余、使用趋势与活跃天数。

## 功能

- **实时总量**：TOTAL TOKENS 大数字带滚动动画，每次 AI 请求完成立即跳动
- **费用**：总费用（USD），支持按模型价目表折算
- **额度剩余**：各数据源剩余百分比进度条 + 重置倒计时
  - OpenCode Go 官方额度：5 小时 $12 / 每周 $30 / 每月 $60（权威，含所有设备）
  - OpenCode Go 本地代理：基于本机代理记录估算剩余%
  - Codex：本地会话记录中的周配额（used_percent + resets_at）
  - 兼容中转站：余额接口自动探测（billing / balance / auth-key / one-api 四种风格）
- **模型明细**：按模型分组的 token 消耗与占比进度条
- **趋势**：面积折线图（标注峰值）
- **活动**：GitHub 风格热力图 + 活跃天数
- **时间筛选**：DAY / MONTH / TOTAL 三档

## 数据源（6 类）

| 类型 | 配置 | 说明 |
|---|---|---|
| **本地记录** | 自动探测（零配置） | 实时监听 ZCode（`~/.zcode/cli/rollout/model-io-*.jsonl`）与 Codex（`~/.codex/sessions/**/rollout-*.jsonl`）会话日志，毫秒级推送。ZCode 实时日志只保留当天，**历史用量自动从 `~/.zcode/cli/db/db.sqlite` 的 model_usage 表回填**（只读，按天去重，热力图/趋势立刻有 30 天数据） |
| **dsh（DeepSeek Harness）** | 自动探测（零配置） | 实时监听 dsh 会话日志（`~/.dsh/sessions/**/session.jsonl.zstd`，zstd 压缩 JSONL），统计每次模型请求的 token 用量。**口径为实际处理/计费 token（total = input + output + cacheRead）**：dsh 每次请求都会重放整段会话缓存上下文，cacheRead 占绝对大头，按此口径才反映真实消耗。会话过程中实时追加，重启不重复统计 |
| **OpenAI 官方** | Base URL + API Key | 用量 API（`/v1/usage`），需**组织管理员** Key；支持回填 90 天历史 |
| **兼容中转站** | Base URL + API Key | 自动探测常见余额接口，或自定义 URL + 字段映射 |
| **OpenCode Go 本地代理** | 代理端口 + 上游 URL | 客户端指向 `http://127.0.0.1:<端口>`，代理转发到 `opencode.ai/zen/go` 并记录每次请求（估算，仅统计本机），对照官方限额计算剩余% |
| **OpenCode Go 官方额度** | 工作区 ID + Auth Cookie | 抓取 opencode.ai 控制台内嵌数据（**权威**，包含所有设备），5 小时 / 每周 / 每月剩余用量 |

## 环境要求

- Windows 10/11（含 WebView2 运行时，Win10/11 系统自带）
- [Rust](https://rustup.rs)（stable，含 MSVC 目标）+ [VS 2022 Build Tools](https://visualstudio.microsoft.com/zh-hans/downloads/)（勾选「使用 C++ 的桌面开发」）——Rust 在 Windows 编译必须 MSVC 链接器
- Node.js ≥ 18（仅用于前端脚手架与 tauri CLI）

国内网络建议配置镜像：`RUSTUP_DIST_SERVER=https://rsproxy.cn`、`~/.cargo/config.toml` 的 crates.io 源替换为 `sparse+https://rsproxy.cn/index/`。

## 安装与运行

### 直接下载（推荐，无需本地构建）

普通用户**无需安装 Rust / Node 环境**，直接前往 [Release 下载页](https://github.com/kissthisrain/token-usage-widget/releases) 下载最新版 `token-usage-widget.exe` 即可：

- 绿色版单文件，双击即用，前端资源已完整嵌入，不产生安装目录
- 由 CI 使用 release 配置构建（`cargo build --release`），随 `v*` tag 自动发布
- 依赖系统自带的 WebView2 运行时（Win10/11 自带，无需额外安装）

> ⚠️ Release 建议下载已正式发布的版本；macOS/Linux 用户请自行按下方源码方式构建。

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

1. 启动后自动探测本机工具（ZCode / Codex / OpenCode CLI / dsh），勾选启用即可看到真实数据
2. 需要 API 类数据源时：设置 → 数据源 → 添加 → 填写配置 → 「测试连接」

### OpenCode Go 官方剩余用量查询

OpenCode Go **没有公开的用量 JSON API**（`/v1/usage`、`/v1/balance` 均返回 404），剩余额度只存在于官网控制台页面。小组件通过「登录态 Cookie + 工作区 ID」抓取控制台内嵌数据，拿到**官方权威**的 5 小时 / 每周 / 每月剩余用量（本地代理估算只统计本机，官方数据包含所有设备）。

1. 浏览器登录 [opencode.ai](https://opencode.ai) → 打开用量控制台，地址栏形如 `https://opencode.ai/workspace/{id}/go`，复制 `wrk_` 开头的**工作区 ID**
2. 控制台页面按 `F12` → Application / 存储 → Cookies → `opencode.ai`，复制名为 `auth` 的 Cookie 值
3. 设置 → OpenCode Go 数据源 → 填入「工作区 ID」与「Auth Cookie」→ 保存 → 点「测试连接」验证
4. 之后小组件定时刷新官方数据，配额卡片标记「Go 官方」；未配置或 Cookie 失效时自动回退为本地代理估算（标记「Go 估算」）

> ⚠️ Cookie 等同你的登录态，**仅保存在本机配置文件**中，请勿外传；若提示「会话已过期」，重新登录后复制最新 Cookie 即可。

## 开发命令

### 日常迭代（快速，推荐）

```bash
npm run dev                       # 开发模式：前端改动保存后窗口自动热重载（秒级）
                                  # Rust 代码改动后 Ctrl+C 重跑即可（增量编译，10~60s）

cd src-tauri && cargo build       # 只改 Rust 时：增量编译（比 release 快得多）
./target/debug/token-usage-widget.exe   # 运行最新 debug 版看效果

./target/debug/token-usage-widget.exe --smoke   # 无界面数据管道自检（秒级）
```

### 发布（慢，仅打包时用）

```bash
npm run build                     # tauri build：release 全量优化编译 + NSIS 打包
                                  # 首次 10-25 分钟，之后增量也要几分钟；产物在
                                  # src-tauri/target/release/bundle/
```

### 质量与工具

```bash
cargo clippy           # lint（保持 0 警告）
cargo test             # 单元测试
npm run icon           # 重新生成应用图标（resources/icon-1024.png → src-tauri/icons/）
```

> 💡 日常开发永远走 `cargo build` + debug exe（或 `npm run dev`）；`npm run build` 是发布流程（release 优化 + 打安装包），不适合用来验证改动。

## 数据与隐私

- 所有数据仅存本机：`%APPDATA%/token-usage-widget/`（config.json 配置、history.json 历史聚合、文件监听偏移量；与 Electron 版数据兼容，无缝迁移）
- API Key 仅保存在本地配置文件中，只用于直连你配置的提供商接口
- 本地记录读取的是 ZCode / Codex / dsh 自己的日志文件，只读不修改

## 已知限制

- OpenAI 用量接口需组织管理员 Key（普通 Key 会 403，测试连接会给出明确提示）
- 本地 rollout 文件可能只保留近期，历史数据以 `.codex/sessions` 为主（ZCode 以 `db.sqlite` 为权威口径，周期回填）
- 若同时启用「本地记录」与「OpenAI 官方」统计同一提供商，数据会重复，建议二选一
