# 结构化 LLM Usage 需求说明

日期：2026-07-17

## 1. 背景

AgenticData 现在通过 yiTrace 记录 SuperAgent Trace 内 LLM 调用的模型、输入 token、输出 token 和缓存 token。

AgenticData 另外有两套全局 LLM 记录，不属于本需求的删除范围：

- `backend/logs/llm.log`：记录全系统所有 LLM 调用的 prompt、response 和执行日志，eval 和 Tuner 依赖它还原完整调用路径。
- `llm_call_logs`：持久化全系统的模型用量和成本数据。

本需求只替换 yiTrace Span 内部的 `span.log({"llm_usage": ...})`，不删除、不缩减、不改变上述两套全局记录。

yiTrace 已经支持 `input_tokens`、`output_tokens` 和 `model`，但不支持缓存读写 token。Python 高层 `Span` 也没有通用属性接口。AgenticData 只能额外调用 `span.log()` 写入一段 JSON：

```json
{
  "llm_usage": {
    "call_site": "superagent.reasoning",
    "model_id": "qwen3.6-plus",
    "model_category": "chat",
    "prompt_tokens": 12400,
    "completion_tokens": 240,
    "cached_tokens": 1200,
    "cache_write_tokens": 300,
    "cache_read_available": true,
    "cache_write_available": true
  }
}
```

这会带来三个问题：

1. 每次 LLM 调用多一条 `LOG` 事件和一次队列提交。
2. 缓存 token 是 JSON 字符串，yiTrace 不能直接汇总、排序或计算成本。
3. `0` 和“模型服务没有返回该指标”需要另外保存布尔字段才能区分。

## 2. 目标

优先级：P0。这是 AgenticData 删除 yiTrace Span 内 `llm_usage` JSON 解析和完成缓存成本展示的前置能力。

1. 缓存读写 token 作为 yiTrace 的一级数值字段存储和汇总。
2. Python 和 TypeScript SDK 可以在 Span 结束前设置通用属性。
3. 正常 LLM Span 只产生 `SPAN_START + SPAN_END`，不需要额外的 `LOG` 或 `ATTR` 事件。
4. 现有 0.1.5 数据和客户端保持可读。

## 3. 一级 Usage 字段

### 3.1 字段定义

`SpanEvent` 和折叠后的 Span 新增：

| 字段 | 类型 | 含义 |
|---|---|---|
| `cache_read_tokens` | `Optional[u64]` | 本次模型调用命中并读取的缓存 token |
| `cache_write_tokens` | `Optional[u64]` | 本次模型调用新写入的缓存 token |

空值规则：

- `null` / `None`：上游模型服务没有返回该指标。
- `0`：上游返回了该指标，但本次没有缓存读取或写入。

不在存储层增加 `cache_read_available` 和 `cache_write_available`。客户端使用“字段是否为 null”判断可用性。

`total_tokens` 不单独存储，由 `input_tokens + output_tokens` 计算。不要同时保存可推导字段，避免出现数值不一致。

### 3.2 SDK 接口

Python：

```python
span.set_tokens(
    input_tokens=12400,
    output_tokens=240,
    cache_read_tokens=1200,
    cache_write_tokens=300,
)
```

TypeScript：

```ts
span.setTokens({
  inputTokens: 12400,
  outputTokens: 240,
  cacheReadTokens: 1200,
  cacheWriteTokens: 300,
})
```

`set_tokens()` / `setTokens()` 只修改 Span 内存状态。这些字段在 `SPAN_END` 事件中一次性上报，不另外产生事件。

### 3.3 存储和查询链路

两个字段必须贯穿：

- Python / TypeScript SDK 事件对象。
- wire JSON，同时接受 snake_case 和现有 HTTP 输出风格。
- `SpanFields` 和 last-non-null 折叠逻辑。
- WAL 编解码、segment 编解码和版本兼容。
- Span detail、Trace detail、trajectory、aggregate、console API。
- Python DB client 和 HTTP client 的结果归一化。

旧 WAL 或 segment 中没有新字段时，按 `None` 处理，不需要重建原始 Trace 数据。

### 3.4 汇总规则

在现有 `input_tokens` / `output_tokens` 出现的成本视图中，增加：

- Span：`cacheReadTokens`、`cacheWriteTokens`，保留 nullable 语义。
- Trace：`totalCacheReadTokens`、`totalCacheWriteTokens`。
- Session：`totalCacheReadTokens`、`totalCacheWriteTokens`。
- Agent cost：`cacheReadTokens`、`cacheWriteTokens`。
- trajectory / aggregate：同样提供可汇总的缓存 token。

汇总必须在 Span 事件折叠后进行，不能对 `SPAN_START`、`LOG`、`SPAN_END` 原始事件直接相加，避免同一个 Span 重复计数。

## 4. Span 通用属性接口

### 4.1 SDK 接口

Python：

```python
span.set_attribute("llm.call_site", "superagent.reasoning")
span.set_attributes({
    "llm.model_id": "qwen3.6-plus",
    "llm.model_category": "chat",
})
```

TypeScript：

```ts
span.setAttribute("llm.call_site", "superagent.reasoning")
span.setAttributes({
  "llm.model_id": "qwen3.6-plus",
  "llm.model_category": "chat",
})
```

第一版属性值只支持 JSON 标量：`string | number | boolean`。`None` / `null` 表示不写入该 key。暂不支持嵌套对象和数组。

SDK 负责把属性转成 yiTrace 引擎已有的 JSON 字面量格式，不要要求调用方自己做 JSON 编码。

### 4.2 事件数量约束

`set_attribute()` 和 `set_attributes()` 只更新 Span 内存状态，并在 `SPAN_END` 上报合并后的 `attrs`。

不能为每次 setter 发送一条 `ATTR` 事件。否则虽然去掉了 `LOG`，但事件数和写入频率没有下降。

`EventType.ATTR` 仍可以保留给需要运行中立即补写属性的高级场景，但不是高层 setter 的默认行为。

## 5. AgenticData 的目标接法

yiTrace 完成后，AgenticData 的 LLM 用量记录应改为：

```python
span.set_model(model_name or model_id)
span.set_tokens(
    input_tokens=usage.prompt_tokens,
    output_tokens=usage.completion_tokens,
    cache_read_tokens=(
        usage.cached_tokens if usage.cache_read_available else None
    ),
    cache_write_tokens=(
        usage.cache_write_tokens if usage.cache_write_available else None
    ),
)
span.set_attributes({
    "llm.call_site": call_site,
    "llm.model_id": model_id,
    "llm.model_category": model_category,
})
```

只删除 yiTrace 桥接层中的：

```python
span.log(json.dumps({"llm_usage": payload}))
```

AgenticData 前端不再从 yiTrace Span 的 `logs` 解析 token。优先读取一级 usage 字段，调用位置和模型分类从 `attrs` 读取。

`core/llm/chat.py` 的 `llm_session` logger、`backend/logs/llm.log`、`record_usage()` 的全局记录和 `llm_call_logs` 持久化全部保留。非 SuperAgent 的 LLM 调用仍继续走这些全局链路。

## 6. 兼容规则

1. 旧 SDK 发送的事件不带新字段，引擎按 `None` 读取。
2. 新 SDK 写旧数据目录时，WAL 和 segment 升级不得丢失历史 Span。
3. HTTP 和 Python 查询返回的新字段必须是可选字段，旧客户端忽略后仍能正常工作。
4. AgenticData 升级后不再写入或解析旧 yiTrace Span `llm_usage` LOG。历史 Trace 的缓存 token 显示为空可以接受。这不影响全局 `backend/logs/llm.log`。
5. 新字段不参与 `event_id` 计算，保持现有事件幂等规则。

## 7. 验收测试

### 7.1 SDK

- `set_tokens()` 可设置缓存读写 token。
- `set_attributes()` 可设置字符串、数字和布尔值。
- 一个正常 LLM Span 总共只导出两个事件：`SPAN_START` 和 `SPAN_END`。
- `SPAN_END` 同时包含 usage 字段和 attrs。
- `None` 和 `0` 的序列化结果不同。

### 7.2 引擎和存储

- 新旧 WAL 样例都能打开并查询。
- 同一 Span 的晚到 usage 按 last-non-null 覆盖，`None` 不清空已有值。
- Span detail 返回 nullable 缓存 token 和 attrs。
- Trace、Session、Agent、trajectory 的缓存 token 汇总正确。
- 汇总不因同一 Span 的多个原始事件重复计数。

### 7.3 集成

- embedded、HTTP ingest 和 Python DB client 的结果一致。
- AgenticData 只删除 yiTrace 桥接层的 `span.log({"llm_usage": ...})` 后，前端仍能展示模型用量、输入、输出、缓存读取和缓存写入。
- 全局 `backend/logs/llm.log` 仍覆盖 SuperAgent 和非 SuperAgent 的 LLM 调用，eval / Tuner 现有读取链路不受影响。
- 每次 LLM 调用比现在减少一条 `LOG` 事件。

## 8. 不在本次范围

- 模型单价、货币和费用计算。
- 通用 attrs 的嵌套对象、数组和大文本存储。
- buffered exporter 的批量窗口。该能力已在当前未发布源码中单独实现。
- AgenticData 的前端样式调整。
- AgenticData 全局 `backend/logs/llm.log` 和 `llm_call_logs` 的记录范围、格式或保留策略。

## 9. 完成标准

以下条件全部满足才算完成：

1. Python 和 TypeScript SDK 具有一致的 usage 和 attributes 接口。
2. 新字段可持久化、折叠、查询和汇总。
3. 旧数据目录和旧客户端兼容测试通过。
4. 一次正常 LLM 调用不再为 usage 产生额外 `LOG` / `ATTR` 事件。
5. yiTrace 接入文档包含新接口和 `None` / `0` 语义。
