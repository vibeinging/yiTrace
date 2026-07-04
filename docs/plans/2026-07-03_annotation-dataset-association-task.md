# Annotation and Dataset Association Task

> 日期：2026-07-03
> 状态：基础版已落地

## 背景

AgenticData 后续要做 trace review、eval draft、golden path、训练/回归样本沉淀。yiTrace 作为底层 TraceDB 不能只返回 span，还需要保存“这条 trace/span 后来被怎么判断”和“它对应哪个外部 dataset item”。

这层不应该写进 WAL/segment 主 trace 格式。annotation 和 dataset association 是后验业务元数据，适合独立持久化，避免污染 append/fold 主链路。

## 已实现

- 新增 durable metadata store：`metadata.dat`。
- `TraceAnnotation`：
  - trace/span target。
  - tenant_id。
  - trace_id/span_id，支持外部字符串 id 稳定 hash，并保留 `externalTraceId` / `externalSpanId`。
  - label、score、reason、source、createdAtNs、attrs。
- `DatasetAssociation`：
  - tenant_id。
  - datasetId、itemId。
  - trace/span source link。
  - snapshotId、snapshotHash、evalRunId、split、label、score、attrs。
- HTTP:
  - `POST /v1/annotations`
  - `GET /v1/annotations`
  - `POST /v1/dataset-associations`
  - `GET /v1/dataset-associations`
  - `/v1/dataset-links` 作为 dataset associations 的别名。
- Node:
  - `db.annotate()`
  - `db.annotations()`
  - `db.linkDatasetItem()`
  - `db.datasetAssociations()`
- `traceSearch()` 反向过滤：
  - `filter.annotation.{label,source,target,scoreMin,scoreMax,attrs}`
  - `filter.dataset.{datasetId,itemId,evalRunId,split,label,scoreMin,scoreMax,attrs}`
  - 顶层别名：`annotationLabel`、`annotationSource`、`datasetId`、`itemId`、`evalRunId`、`datasetLabel` 等。
- `traces()` / `sessions()` 反向过滤：
  - `annotationLabel` / `annotationSource` / `annotationScoreMin` / `annotationScoreMax`
  - `datasetId` / `itemId` / `evalRunId` / `datasetLabel` / `datasetScoreMin` / `datasetScoreMax`
  - Node options 支持嵌套 `annotation` / `dataset` 对象。
- 在线备份包含 `metadata.dat`。
- Metrics 新增：
  - `yt_annotations`
  - `yt_dataset_associations`

## 验证

- `cd yitrace-engine && cargo test --offline`
- `cd yitrace-node && npm run build && npm test`

测试覆盖：

- HTTP annotation/dataset association tenant 隔离。
- durable reopen 后 metadata 仍可查询。
- `traceSearch()` 可按 annotation/dataset association 反查 source trace/span。
- `traces()` / `sessions()` 可按 annotation/dataset association 反查列表结果。
- Node ESM/CJS 包入口调用新增方法。
- 外部字符串 id 的 hash + 原始 id 保留。

## 当前边界

- 分页基础版已补齐：当前返回 `{ items, count, total, pageCount, nextCursor }`，按 `createdAtNs`/id 倒序。
- 暂不做完整 dataset item 管理。yiTrace 只保存 source link 和 snapshot identity，dataset item 本体仍由外部 eval/training 系统管理。
- annotation 更新/删除状态机基础版已补齐：支持 `active/resolved/rejected/deleted`、PATCH 更新和 DELETE 软删除；完整审批历史仍留给后续。
- `traceSearch()` / `traces()` / `sessions()` 已支持反向过滤。

## 后续

- annotation 后续可继续补 severity、审批历史和批量 review workflow。
- dataset association 增加 eval_status/result_score/assertion_type 的一等字段。
- metadata store 后续补索引，避免 annotation 规模增长后全量扫描。
