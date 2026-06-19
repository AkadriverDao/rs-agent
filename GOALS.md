# 对标 OpenCode 实现路线

按优先级从高到低排列。

## P0 — 核心稳定性

### [x] Usage 从 API 响应解析
- `llm.rs`: `StepFinish` / `Finish` 的 `usage` 字段始终为 `None` ✅
- 需要从 DeepSeek SSE 响应的 `usage` 字段解析并填充 ✅
- 实现: `stream_options.include_usage` + `StreamUsage` 反序列化 + 传递到事件

### [x] Doom-loop 检测
- 检测连续 N 次工具调用无实质进展（如相同错误反复出现） ✅
- 达到阈值后自动 break 而非死循环到 max_iterations ✅
- 实现: 连续 6 轮无文本输出时自动终止，返回友好错误信息

### [x] 权限 Ask 交互流
- 当前 `Ask` 直接返回 PermissionDenied 错误，无法恢复 ✅
- 需要改为：Ask → 暂停 → 回调用户询问 → 用户批准/拒绝 → 继续或终止 ✅
- 实现: `Approver` 回调类型 + `DefaultPermissionChecker.with_approver()`

## P1 — 工具系统

### [x] 并行 settle()
- `tool.rs` `settle()` 当前顺序执行工具调用 ✅
- 改为 `futures::join_all` 并行执行 ✅

### [x] 内置工具集
参考 OpenCode `packages/core/src/tool/` 实现以下工具：

#### P1a — 文件工具
- [x] `read` — 读取文件内容
- [x] `write` — 写入/覆盖文件
- [x] `edit` — 精确字符串替换编辑
- [x] `glob` — 文件名模式匹配搜索

#### P1b — 搜索工具
- [x] `grep` — 文件内容正则搜索

#### P1c — Shell 工具
- [x] `bash` — 执行 shell 命令并返回输出（带超时）

#### P1d — 网络工具
- [x] `webfetch` — HTTP GET 获取网页内容
- [x] `websearch` — 网页搜索（需配置 SEARCH_API_KEY）

### [x] 工具 Schema 类型化
- 给 `Tool` trait 加 `ToolBuilder`，对标 OpenCode 的 `Tool.make()` ✅
- 用法: `ToolBuilder::new("name", "desc").input_schema(json!({...})).handler(|input, ctx| Box::pin(async { ... })).build()`

### [x] LlmEvent 补全
- `TextEnd`/`ReasoningEnd` — finish_reason / stream end 时发射 ✅
- `ToolCallStart`/`ToolCallDelta`/`ToolCallEnd` — 工具调用增量流 ✅
- 保留 `ToolCallReceived` 作为完整事件的兼容事件

## P2 — LLM 客户端增强

### [x] 重试逻辑 (指数退避)
- `llm.rs`: 对 `ProviderError` 实现指数退避重试 ✅
- 区分可重试 (5xx, rate limit) 和不可重试 (4xx auth) 错误 ✅
- 实现: `send_request()` 独立方法 + 3 次重试，1s/2s/4s 指数退避

### [x] 多 Provider 支持
- `LlmConfig` 支持自定义 `base_url`/`chat_path`/`auth_header_name`/`auth_header_value` ✅
- 内置 `LlmConfig::deepseek()` 和 `LlmConfig::openai_compatible()` 构造函数 ✅

### [x] Usage 跟踪增强
- 累计会话级 token 用量 ✅
- 支持 cache_hit/cache_miss 区分并显示 ✅
- `print_output` 显示累计 token 统计

## P3 — 存储增强

### [x] 消息分表存储
- `storage.rs`: messages + message_parts + tool_results 三表 ✅
- `sessions` 表新增 `parent_id` / `input_tokens` / `output_tokens` ✅
- 新增 `permission_rules` 表 ✅
- 新增 `get_tool_usage_stats()` 查询工具调用统计 ✅
- Schema 版本迁移 (v0 → v1)

### [x] 会话树/父子关系
- `sessions` 表 `parent_id` 字段 ✅
- `create_child_session()` 方法 ✅
- compaction 时创建子会话并自动切换 ✅

### [x] 权限规则持久化
- `permission_rules` 表 ✅
- `save_permission_rule()` / `load_permission_rules()` ✅
- plan 模式用户批准后自动写入 DB，重启后保留 ✅

## P4 — 快照与撤销

### [x] 文件快照
- `snapshot.rs`: 文件修改前自动备份到 `~/.local/share/agent-engine/snapshots/` ✅
- `Tool` trait 新增 `is_modifier()` 方法（`write`/`edit` 返回 true）✅
- Agent 执行 modifier 工具前自动创建快照 ✅
- `undo` 工具：恢复文件到上一个版本 ✅

## P5 — 缺失大模块

### [x] 多 Agent 系统
- build / plan / general 三种模式 ✅
- `AgentDef` + `builtin_agents()` 定义 ✅
- `PermissionChecker::from_agent()` 根据模式配置权限 ✅
- 启动时选择、运行中用 `/agent build|plan|general` 切换 ✅

### [x] Git 集成
- `git_commit` — 自动 commit 变更 ✅
- `git_status` — 查看工作区状态 ✅
- `git_diff` — 查看未提交的 diff ✅
- `git_log` — 查看提交历史 ✅
- write/edit 前自动 `git add -A && commit` ✅
- 修改后自动 `git diff` 展示变更 ✅

### [x] Event Sourcing
- `events` 表记录所有关键操作 ✅
- 事件类型: `session.created` / `message.added` / `tool.executed` ✅
- `append_event()` / `get_events()` 方法 ✅
- 每次 `save_message()` 和工具执行时自动记录事件 ✅

### [ ] MCP 服务器支持
- 通过 Model Context Protocol 接入外部工具生态

---

## 当前进度

- [x] 项目初始化 (types/tool/llm/agent/context/permission)
- [x] SQLite 持久化基础 (sessions/messages 表)
- [x] Usage 从 API 响应解析
- [x] Doom-loop 检测
- [x] 权限 Ask 交互流
- [x] 并行 settle()
- [x] 内置工具集 (read/write/edit/glob/grep/bash/webfetch/websearch)
- [x] 重试逻辑 (指数退避)
- [x] 多 Provider 支持
- [x] Usage 跟踪增强
- [x] LlmEvent 补全
- [x] 工具 Schema 类型化 (ToolBuilder)
- [x] 消息分表存储
- [x] 文件快照/撤销
- [x] 多 Agent 系统
- [x] 会话树 + 权限持久化
- [ ] 👉 Android 移植
