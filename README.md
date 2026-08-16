# CASH — Cross-Agent Session History

> CASH 是三个编程助手之间的"历史记录翻译官+搬运工"，让你换工具不换思路、不用重新讲一遍需求。

更正式一点说：CASH 把同一个逻辑 Coding Agent Session 在多个原生 Agent（OpenCode、Pi Agent、Codex）之间同步，让你切换 agent 时工作上下文不断线，并且可以把一个 agent 里新增的历史增量补充回另一个 agent 的原生 session。

## 解决的问题

各 Coding Agent 把 session 历史存在不同的本地格式里。切换 agent 时，工作区的文件、依赖、运行状态都还在，但新 agent 看不到旧 agent 的对话、推理和工具执行结果。

CASH 把这两类信息分开：

- **工作区**：仍然是文件与运行状态的唯一事实来源，不需要迁移。
- **Session 历史**：被 CASH 规范化成统一的事件流，再按目标 agent 的原生格式写入。
- **映射**：同一个逻辑 session 在各 agent 中的副本关系由 CASH 自己维护。

可移植 seed 是结构化事件流，而不是纯 Markdown transcript。

## 支持矩阵

| Agent | 读取 | 原生导入 | 原生启动验证 |
| --- | --- | --- | --- |
| Codex | ✅ | ✅ | ✅ |
| Pi Agent | ✅ | ✅ | ✅ |
| OpenCode | ✅ | ✅ | ✅ |

目标适配器可能丢弃目标格式无法表示的事件；`seed.json` 始终保留完整的规范化轨迹，`manifest.json` 记录 `dropped_event_count`。

## 快速开始

### 一步转换（OpenCode → Pi）

```bash
cargo run -- convert opencode <OPENCODE_SESSION_ID> pi
```

先列出可用 session：

```bash
cargo run -- list opencode
cargo run -- list pi
```

### 指定路径

```bash
cargo run -- convert \
  opencode <OPENCODE_SESSION_ID> \
  pi \
  --seed /tmp/opencode-to-pi \
  --pi-root /tmp/pi-sessions \
  --opencode-db ~/.local/share/opencode/opencode.db
```

转换命令总是先生成结构化 seed 再导入，seed 目录包含：

```text
seed.json       完整可移植 IR
seed.md         可读 transcript
manifest.json   源指纹与目标绑定
```

### 幂等更新

同一个源 session 与同一个目标 agent 绑定到同一个目标 session。重复执行同一转换不会产生重复 session，而是更新已有副本：

```bash
cargo run -- convert opencode <SESSION_ID> pi
cargo run -- convert opencode <SESSION_ID> pi
```

第二次执行会复用 `manifest.json` 中记录的 Pi session ID 与 JSONL 文件。

如果目标 agent 在锚点之后继续过对话，CASH 默认拒绝覆盖：

```text
target Pi session continued after anchor; refusing to overwrite ...
```

需要强制替换时：

```bash
cargo run -- convert opencode <SESSION_ID> pi --force
```

`--force` 保留目标 session 身份，只替换导入内容。OpenCode 目标同样受保护。

## 种子目录配置

输出目录可选。解析顺序：

1. `CASH_SEED_DIR`
2. `CASH_CONFIG` 或 `~/.config/cash/config.json`
3. `~/.local/share/cash/seeds`

旧版本的 `MIGRATE_SEED_DIR`、`~/.config/migrate/config.json`、`~/.local/share/migrate/seeds` 会被只读兼容读取，避免已有映射失效。

```bash
export CASH_SEED_DIR=~/agent-seeds
cargo run -- convert opencode <SESSION_ID> pi
```

配置文件：

```json
{
  "seed_dir": "~/agent-seeds"
}
```

`import` 或 `status` 不指定 `--seed` 时，使用配置的种子目录，或其中最新的 seed。

## 分步操作

`convert` 是常规一步工作流；当你想先检查或编辑 seed 时用分步命令：

```bash
cargo run -- export opencode <SESSION_ID> -o /tmp/session-seed
cargo run -- import pi --seed /tmp/session-seed
cargo run -- status /tmp/session-seed
```

## 可移植轨迹

`seed.json` 是 `meta + events` 的单一表示（不重复保存原始记录，体积约等于源 session）：

- 用户消息
- 助手消息
- 可读的推理内容
- 工具调用（ID、名称、参数）
- 工具结果（输出、退出码、错误）
- 模型切换
- 无法跨 agent 表达的原生记录（label、compaction、thinking level 等）以不透明事件保留

**事件是唯一的 truth。** 约定是：

- `session → events` 无损：抽取时保留每条原生记录的全部信息。
  - 每条事件带 `original_id`（来源侧记录 ID）与 `parent_original_id`；来自同一条原生记录的多个事件共享同一个 `original_id`。
  - 原生专用元数据（usage、responseId、model、errorMessage……）放进 `native` 字段；其他 agent 没有的字段留空。
- `events → session` 允许有损：写入目标原生格式时按需重组，目标表达不了的事件计入 `manifest.json` 的 `dropped_event_count`。

同 agent 往返（如 Pi → Pi）会复用 `original_id`、回写 `native` 元数据，保证 `session → events → session → events` 事件级一致。

源格式由 reader 适配，目标格式由 importer 适配，IR 是两者之间的边界。

## 模型处理

- IR 把模型作为字面量透传：`meta.model` + `model_change` 事件。没有"翻译/映射"层。
- **Codex 目标使用默认模型**：Codex 没有 model-change 事件，导入时丢弃源模型，resume 时由 Codex 使用当前配置的默认模型。
- Pi / OpenCode 目标：源模型原样写入；目标不支持时 CASH 目前**不会自动检测**，只是提示：
  ```text
  note: source session model is <model>; if the target does not support it, pass --model <target-model>
  ```
- 显式换成目标支持的模型：
  ```bash
  cash convert opencode <SESSION_ID> pi --model gpt-5.6-sol
  ```
  `--model` 会改写目标侧写入的模型元数据，并打印覆盖提示。自动能力检测 / 模型映射表仍在设计中。

## 存储位置

```text
Codex       ~/.codex/sessions
Pi Agent    ~/.pi/agent/sessions
OpenCode    ~/.local/share/opencode/opencode.db
```

可用 `--pi-root`、`--codex-root`、`--opencode-db` 覆盖。

## 测试

```bash
cargo test
```

包含手写最小 fixture 和从真实 session 生成的脱敏 fixture。

真实本机会话 smoke：

```bash
cargo test --test real_sessions -- --ignored
```

原生 CLI 校验（不发模型请求，只读临时目录/数据库副本）：

```bash
cargo test --test native_startup -- --ignored
```

重新生成脱敏 fixture：

```bash
cargo run --bin make_fixtures -- --out tests/fixtures/real
```

## 未来方向

- **执行环境打包**：当前只迁移 session 历史，工作区/依赖/运行状态依赖磁盘。未来支持把执行环境整体打包（如 `tar.zst`），让一个"作业"可以 import 到其他执行环境运行，而不是只在当前机器上接力。
- **双向增量合并**：两个副本同时新增内容时的分支/合并语义仍在设计中。
- **模型能力检测**：根据各 agent 支持的模型做自动映射与提示。

## 当前限制

- 跨 agent 时，不同目标可表示的事件子集不同，请查看 `manifest.json` 的 `dropped_event_count`。
- 锚点之后继续过的目标 session 需要显式 `--force` 才能被替换。
- 双向增量合并（两副本同时新增）仍在设计中，当前同步模型是单方向增量追加。
- 自动模型能力检测 / 模型映射表仍在设计中。
