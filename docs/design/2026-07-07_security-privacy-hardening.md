# yiTrace 安全与隐私硬化设计

> 日期：2026-07-07
> 状态：设计
> 目标：先把 trace 数据的安全边界说清楚，再决定哪些能力进入实现。

## 结论

yiTrace 当前适合本地、私有、可信环境。它还不是默认企业安全版。

原因很简单：Agent trace 不是普通日志。它经常包含 prompt、工具入参、工具返回、数据库结构、内部错误、用户数据、API key 片段和业务策略。只要一次跨租户泄露或一次 snapshot 误导出，就会比普通日志事故严重。

下一阶段安全工作的原则：

1. **先分类，再脱敏**：不知道哪些字段敏感，就不要急着做复杂权限。
2. **默认诚实**：README 和文档不能暗示已经具备完整企业安全能力。
3. **权限围绕数据动作设计**：不是只有 read/write，还有 export、raw text、retention、route table reload。
4. **本地优先不等于无安全**：embedded 场景也需要知道哪些数据会落盘。

## 敏感数据分类

| 类别 | 字段/来源 | 风险 | 默认策略 |
|---|---|---|---|
| 原始输入 | `input_text`、prompt、tool args | 用户隐私、业务规则、API key | 默认保存，但文档强提示 |
| 原始输出 | `output_text`、tool result、LLM response | 用户隐私、内部数据、错误详情 | 默认保存，但文档强提示 |
| 日志 | `logs`、span event message | 可能混入 secret、SQL、内部路径 | 默认保存，但建议 SDK 侧过滤 |
| 属性 | `attrs` | project、call_site、schema、错误上下文 | 高频字段保留，未知 attrs 需可过滤 |
| 外部 ID | `external_trace_id` 等 | 可关联业务系统 | 保存但可配置 hash 化 |
| 成本与 token | usage/cost/model/provider | 商业敏感 | 默认保存 |
| 元数据 | annotation、dataset、golden path | 人工判断和训练数据线索 | tenant-scoped，操作要审计 |
| 导出产物 | snapshot、golden path export | 可离线传播，泄露面大 | 需要单独权限和审计 |

## 权限模型

第一版不急着实现完整 RBAC，但要先固定权限动作，后续实现不能临时发明。

| 权限 | 说明 |
|---|---|
| `trace:write` | 摄入 trace/span/event |
| `trace:read` | 读取 trace 列表、span detail、search |
| `trace:read_raw` | 读取 `input_text` / `output_text` / logs 等原文 |
| `trace:export` | 导出 snapshot / evidence / JSONL |
| `metadata:write` | annotation、dataset association、golden path 状态变更 |
| `retention:plan` | 生成 retention dry-run |
| `retention:apply` | 执行软删除/compact |
| `cluster:admin` | route table reload、health refresh、replication 控制 |

租户边界继续以 `X-Tenant-Id` 或 embedded open option 作为最外层隔离。生产模式下，未带 tenant 的请求不应进入共享服务。

## 脱敏策略

### SDK 侧脱敏

优先级最高。SDK 最接近业务上下文，知道哪些字段是 secret。

建议能力：

- `redactInputText`
- `redactOutputText`
- `redactAttrs(keys | pattern)`
- `hashExternalIds`
- `dropLogs`

### Server / embedded 层过滤

第二层兜底，适合统一策略：

- 按字段 drop。
- 按 attrs key drop。
- 对 text 做简单 pattern 脱敏。
- 对 external id 做 hash。

注意：这一层不知道业务语义，只能做粗过滤，不能替代 SDK 侧脱敏。

### 查询层控制

读取原文需要和读取摘要分开：

- trace list/search 默认可以返回摘要和 metadata。
- span detail 读取原文需要 `trace:read_raw`。
- snapshot/export 需要 `trace:export`。

## 审计要求

必须审计的动作：

- `retention/apply`
- `golden-path-export`
- `trace snapshot export`
- annotation / dataset / golden path mutation
- route table reload
- replication pull/apply
- auth failure

审计至少要包含：

- tenant
- actor
- action
- target
- request id
- time
- status
- reason

当前已经有 retention audit，但还不是完整安全审计。下一阶段要把安全审计作为独立底座，而不是继续塞进普通 stderr。

## 生产边界

当前可以说：

- 支持 tenant 逻辑隔离。
- 支持 token 鉴权入口。
- 支持 body limit。
- retention apply 有审计。
- embedded 单写锁保护 data dir。

当前不能说：

- 已支持完整 RBAC。
- 已支持 TLS。
- 已支持落盘加密。
- 已支持字段级权限。
- 已支持企业级审计。
- 已支持合规删除全链路证明。

## 下一步实现顺序

1. 文档和 README 调整：安全边界不能说过头。
2. SDK 脱敏 hook 设计。
3. `trace:read_raw` / `trace:export` 权限接缝。
4. 持久安全审计日志。
5. 生产模式下强制 tenant。
6. TLS / RBAC / encryption。

## 验收

- 安全文档进入 README / CURRENT_STATE 链接。
- 风险 eval 增加“未带 tenant 的生产模式拒绝”或等价接缝测试。
- `trace snapshot export` 和 `golden path export` 后续都能挂审计。
- 金融/政企 PoC 前，TLS + RBAC + 持久审计 + 脱敏至少完成第一版。
