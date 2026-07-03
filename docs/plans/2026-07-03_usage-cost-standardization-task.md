# Usage and Cost Standardization Task

> 日期：2026-07-03
> 状态：基础版已落地

## 背景

Agent trace 的最优路径、回归样本和 trace inbox 排序都需要稳定的 token/cost 字段。过去 yiTrace 只有 `input_tokens` / `output_tokens` 和展示层写死的估算 `cost`，不能表达 cache token、reasoning token、provider，也不能保留上游已经计算好的真实成本。

## 已实现

- direct wire ingest 支持：
  - `provider` / `llm_provider`
  - `cached_input_tokens`
  - `reasoning_tokens`
  - `total_tokens`
  - `cost_usd`
  - `cost_usd_nanos`
  - `cost_currency`
- OTLP/OpenInference 映射支持：
  - `gen_ai.system` / `llm.provider`
  - `gen_ai.usage.cached_input_tokens`
  - `gen_ai.usage.reasoning_tokens`
  - `gen_ai.usage.total_tokens`
  - `gen_ai.usage.cost_usd`
  - `gen_ai.usage.cost_currency`
- `SpanFields` / `FoldedSpan` / WAL / segment 编码已升级到 version 4，旧 v2/v3 数据仍可读。
- Node builder 支持 camelCase/snake_case 传入 usage/cost 字段。
- 查询输出保留旧兼容字段，同时新增标准字段：
  - `usage.inputTokens`
  - `usage.outputTokens`
  - `usage.cachedInputTokens`
  - `usage.reasoningTokens`
  - `usage.totalTokens`
  - `costUsd`
  - `costDetail.costUsd`
  - `costDetail.costUsdNanos`
  - `costDetail.currency`
  - `costDetail.source`
- 覆盖端点：
  - `GET /v1/traces`
  - `GET /v1/sessions`
  - `GET /v1/sessions/:id/turns`
  - `GET /v1/traces/:id`
  - `GET /v1/traces/:id/steps`
  - `GET /v1/traces/:id/spans`
  - `GET /v1/traces/:id/spans/:spanId`
  - `POST /v1/trace-search`
- 成本排序改为优先使用显式 `cost_usd_nanos`；缺失时用默认估算：input token = 800 nano USD，output/reasoning token = 4000 nano USD。

## 验证

- `cd yitrace-engine && cargo test -p yt-engine --offline --lib`
- `cd yitrace-node && npm run build && npm test`

测试覆盖：

- wire parser 解析 usage/cost 字段。
- OTLP GenAI 映射 provider/cache/reasoning/total/cost。
- HTTP trace list / trace detail 输出 usage/cost。
- Node builder 写入 usage/cost，`traceSearch()` 读回。

## 当前边界

- 还没有模型价格表；未显式传成本时使用固定默认估算。
- `costDetail.source` 在聚合响应里可能是 `mixed`，表示其中可能混有显式成本和估算成本。
- 暂不支持按 cost/token 范围过滤；目前只支持排序和输出。

## 后续

- 增加 provider/model 价格表，并允许用户配置。
- 增加 token/cost range filter。
- 在 group-by API 中输出 sum/avg/p50/p95 cost 和 token。
