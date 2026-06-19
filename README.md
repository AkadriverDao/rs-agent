# Agent Engine

一个参考 OpenCode 架构、用 Rust 编写的 Android Agent 引擎。

## 架构总览

```
┌──────────────────────────────────────────────────────────────┐
│                   Android App (Kotlin/Java)                   │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐  │
│  │              AgentEngine (Kotlin 封装)                   │  │
│  │  - nativeInit(apiKey, systemPrompt)                     │  │
│  │  - sendMessage(text) -> AgentOutput                     │  │
│  │  - registerTool(name, executor)                         │  │
│  │  - destroy()                                            │  │
│  └──────────────────────┬─────────────────────────────────┘  │
│                         │ JNI                                │
│  ┌──────────────────────▼─────────────────────────────────┐  │
│  │           Rust Agent Engine (.so)                       │  │
│  │                                                         │  │
│  │  ┌──────────────────────────────────────────────────┐   │  │
│  │  │  Agent Loop (agent.rs)                           │   │  │
│  │  │                                                  │   │  │
│  │  │  ┌──────────┐    ┌──────────┐    ┌──────────┐   │   │  │
│  │  │  │  Think   │───►│   Act    │───►│ Observe  │──┐ │   │  │
│  │  │  │ (LLM调)  │    │(执行工具) │    │(工具结果) │  │ │   │  │
│  │  │  └──────────┘    └──────────┘    └──────────┘  │ │   │  │
│  │  │    ▲                                           │ │   │  │
│  │  │    └───────────────────────────────────────────┘ │   │  │
│  │  │           循环直到 stop / max_iterations          │   │  │
│  │  └──────────────────────┬───────────────────────────┘   │  │
│  │                         │                                │  │
│  │  ┌──────────────────────▼───────────────────────────┐   │  │
│  │  │  LLM Client (llm.rs)                             │   │  │
│  │  │  - DeepSeek API streaming                        │   │  │
│  │  │  - SSE 解析 → LlmEvent 流                        │   │  │
│  │  └──────────────────────────────────────────────────┘   │  │
│  │                                                         │  │
│  │  ┌──────────────────────────────────────────────────┐   │  │
│  │  │  ToolRegistry (tool.rs)                           │   │  │
│  │  │  - 注册/查询工具                                   │   │  │
│  │  │  - 生成 ToolDefinition (JSON Schema → LLM)         │   │  │
│  │  │  - settle(): 批量执行工具调用                       │   │  │
│  │  └──────────────────────────────────────────────────┘   │  │
│  │                                                         │  │
│  │  ┌──────────────────────────────────────────────────┐   │  │
│  │  │  ContextManager (context.rs)                     │   │  │
│  │  │  - 消息历史管理 + Token 估算                       │   │  │
│  │  │  - 自动溢出检测 → 触发 Compaction                  │   │  │
│  │  │  - 工具输出截断 (50KB 上限)                        │   │  │
│  │  └──────────────────────────────────────────────────┘   │  │
│  │                                                         │  │
│  │  ┌──────────────────────────────────────────────────┐   │  │
│  │  │  PermissionChecker (permission.rs)                │   │  │
│  │  │  - Allow / Ask / Deny 三级权限                    │   │  │
│  │  │  - 通配符规则匹配 (edit.*, bash, read.*)          │   │  │
│  │  └──────────────────────────────────────────────────┘   │  │
│  └─────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

## 核心模块

### types.rs — 核心类型系统

参考 OpenCode 的 `packages/llm/src/schema/`，定义了：

| 类型 | 对应 OpenCode | 说明 |
|------|---------------|------|
| `Message` | `Session.Message` | System/User/Assistant/Tool 四角色 |
| `ToolCall` | `ToolCallPart` | 工具调用请求 |
| `ToolResultValue` | `ToolResultValue` | 工具结果 (Text/Json/Error) |
| `LlmEvent` | `LLMEvent` | LLM 流事件 (Text/Reasoning/ToolCall 等) |
| `MessageHistory` | `Session` | 消息历史 + Token 计数 |

### tool.rs — 工具系统

参考 OpenCode 的 `packages/core/src/tool/`：

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;        // JSON Schema
    fn execute(&self, input: Value, ctx: ToolContext)
        -> BoxFuture<'static, ToolResult<ToolOutput>>;
}
```

- `ToolRegistry`: 注册/查询/批量执行工具
- `definitions()`: 生成 `ToolDefinition` 列表发给 LLM
- `settle()`: 批量执行，并行处理

### llm.rs — LLM 客户端

参考 OpenCode 的 `packages/opencode/src/session/llm.ts`：

- DeepSeek API 兼容 (OpenAI API 格式)
- SSE 流式解析 → `LlmEvent` 枚举流
- 支持 `reasoning_content` (DeepSeek R1 推理)
- 支持 `tool_calls` (function calling)

### agent.rs — Agent 循环

参考 OpenCode 的 `packages/opencode/src/session/processor.ts`：

```
循环:
  1. 构建请求 (system prompt + 消息历史 + 工具定义)
  2. 检测溢出 → 触发 Compaction
  3. 调 LLM stream()
  4. 收集事件:
     - TextDelta → 拼接文本
     - ReasoningDelta → 拼接推理
     - ToolCall* → 构建 ToolCall
     - StepFinish/Finish → 判断结束/工具调用
  5. 将模型回复写入历史
  6. 如果有工具调用 → settle() → 结果写回历史 → 回到 1
  7. 否则返回最终输出
```

### context.rs — 上下文管理

- Token 估算（4 chars ≈ 1 token 近似）
- 溢出检测（>75% max_tokens 触发 compaction）
- 工具输出截断（50KB 上限）

### permission.rs — 权限系统

参考 OpenCode 的 `PermissionV1.Ruleset`：

- `Allow` / `Ask` / `Deny` 三级
- 通配符匹配（`edit.*`, `bash`, `read.*`）
- `Ask` 级可触发 Kotlin 层弹窗确认

## Android 集成

### 编译

在 `Cargo.toml` 启用 `android` feature:

```toml
[features]
android = ["jni"]
```

用 Android NDK 交叉编译:

```bash
rustup target add aarch64-linux-android
cargo build --target aarch64-linux-android --features android
```

### Kotlin 侧封装

```kotlin
class AgentEngine(private val apiKey: String) {
    companion object {
        init {
            System.loadLibrary("agent_engine")
        }
    }

    private external fun nativeInit(apiKey: String, systemPrompt: String)
    private external fun nativeSendMessage(message: String): String
    private external fun nativeDestroy()

    fun init(systemPrompt: String) = nativeInit(apiKey, systemPrompt)
    fun sendMessage(message: String): AgentOutput {
        val json = nativeSendMessage(message)
        return Json.parseToJsonElement(json).let { /* 解析 */ }
    }
    fun destroy() = nativeDestroy()
}
```

### Android 自定义工具示例

要在 Kotlin 中实现工具（如管理 app 内容），需要在 Rust 侧加 JNI 回调。

推荐架构: Rust 负责 Agent 循环 + LLM 调用，Android 工具通过 JNI 委托给 Kotlin 执行，结果返回给 Rust 继续循环。

```
Agent Loop (Rust) → 解析出 tool_call → JNI → Kotlin 执行 → JSON 结果 → Rust 写回历史 → 继续循环
```

## 和 OpenCode 的关键差异

| 特性 | OpenCode | 本引擎 |
|------|----------|--------|
| 语言 | TypeScript + Bun | Rust |
| 运行环境 | 桌面/终端 | Android (JNI) |
| 工具集 | 文件系统/bash/git | Android 原生工具 |
| 持久化 | SQLite (Drizzle) | SQLite (rusqlite) |
| 事件系统 | Effect-TS | async enum stream |
| 工具注册 | Effect Layer DI | Trait + Arc<dyn Tool> |
