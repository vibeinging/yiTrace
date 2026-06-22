# yiTrace 后台调度逻辑（修正版）

> 日期：2026-06-17｜配套 `2026-06-16_tracevault-schema.md`（数据模型）。
> 本文是第六轮工作流（2 设计 + 2 红队）**人工修正综合稿**。两路红队都判原稿 `risky`：核心问题是**把正确性建在"时间水位 + 全局 ID 守卫"上，在 MVCC + 乱序晚到下会静默丢数据**。本文把 13 个洞全部修掉。原始设计/红队见 `appendix-M / appendix-N`。
> 全部**纯应用层服务**（内核外进程，印证不动内核）。

## 0. 三条修正后的承重原则（推翻原稿的根基）

| 原稿（错） | 修正（对） | 为什么 |
|---|---|---|
| 折叠靠 `ingest_ts` 时间水位推进 | **折叠靠事务脏队列**：摄入在**同一事务**里把 `(tenant,trace,span)` 写进 `fold_dirty` | `ingest_ts=now()`=事务**开始**时刻；长事务提交的行 ingest_ts 低于已推进水位 → 永不折叠 = **静默丢数据**。脏队列让"是否看到所有已提交事件"成为**事务不变量**，不是时钟猜测 |
| 折叠用 `MERGE INTO` | **折叠用 `INSERT ... ON DUPLICATE KEY UPDATE`** | MERGE 对同 PK 并发 INSERT **不是 race-safe**，会抛 23505 唯一冲突而非折叠；ON DUPLICATE KEY UPDATE 是 openGauss 唯一的 speculative-insert 安全 upsert（已在你们回归测试确认 `EXCLUDED.col`） |
| 冻结顺序：编码→写冷→注册 | **register-FIRST**：先写 `frozen_registry`，再从一致快照编码/写冷；并发到达一律当 remelt | 原顺序在"编码→写冷"窗口内晚到的事件：没注册→摄入不入 inbox→已冻结→折叠环跳过 = **孤儿静默丢失**（稳态而非边角，因为"早上生下午死"就是常态） |
| 冻结按 (tenant,trace) 删热分区 | **冻结不删任何分区**；热区回收 = **GC 按时间整分区 DROP** | `DROP PARTITION` 删的是**一整天所有租户/trace** 的分区；一个 trace 冻结就删整天 = 误删数千个还在跑的 trace。两个时钟必须解耦 |
| remelt 从冷区 CStore 重建折叠基 | **remelt 折叠基取自行存检索镜像**，不取 CStore | CStore 只有低基数分析列（无 attrs/text）；从冷区重建 → 深合并到空 attrs → 原文静默丢失 |

外加：**单一协调权威 = `pg_try_advisory_xact_lock`**（事务结束/崩溃自动释放），删掉 lease 看门狗（仅诊断用，否则与 advisory 锁双重所有权 → 僵尸 worker 双跑）。

> **❗上线前强制 PoC（红队 CRITICAL）**：`BEGIN; INSERT 控制表; ALTER TABLE ... DROP PARTITION; kill -9 提交前` → 重启验证**两者都回滚**。若 DDL 非事务原子（openGauss 部分 DDL 走内部提交），则一切"按步骤标记恢复"的崩溃恢复都不成立 → 须改"破坏性 DDL 放最后 + 按存在性幂等（DROP 前查 `pg_partition`，已不在=成功）+ 进度标记单独前置事务"。

## 1. 控制表（新增，不动数据表 schema）

```sql
-- (1) 折叠脏队列: 摄入在同一事务写入 → 事务不变量
CREATE TABLE fold_dirty (
  tenant_id bigint NOT NULL, trace_id bigint NOT NULL, span_id bigint NOT NULL,
  enqueued_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, trace_id, span_id)   -- 同 span 多事件去重, 折叠后删除
) WITH (ORIENTATION=ROW);

-- (2) 冻结作业(可重入, step 持久化)
CREATE TABLE freeze_jobs (
  tenant_id bigint, trace_id bigint, step smallint NOT NULL DEFAULT 0,  -- 0待 1已注册 2已编码 3已写冷(done)
  root_end_ts timestamptz NOT NULL, max_event_id_at_enqueue bigint,     -- 入队时快照, 用于 re-check
  attempts int NOT NULL DEFAULT 0, last_error text,
  PRIMARY KEY (tenant_id, trace_id)
);
-- (3) 重融化作业
CREATE TABLE remelt_jobs (
  tenant_id bigint, trace_id bigint, state smallint NOT NULL DEFAULT 0, -- 0待 1已重折 2冷已重建 3done
  rebuild_version bigint,                                                -- 冷分区幂等版本(EXCHANGE 前写入)
  attempts int NOT NULL DEFAULT 0, last_error text,
  PRIMARY KEY (tenant_id, trace_id)
);
-- (4) frozen_registry / late_event_inbox 同 schema; payload_ref_decrement / index_lifecycle / trace_retention 见 §6-7
-- (5) 死信表(状态机卡死兜底)
CREATE TABLE job_dead_letter (job_type text, tenant_id bigint, trace_id bigint, reason text, dead_at timestamptz DEFAULT now());
```
**advisory 锁约定**：`pg_try_advisory_xact_lock(class, hashtext(tenant||':'||trace))`，class: FOLD=1/FREEZE=2/REMELT=3。**freeze 与 remelt 同 trace 必须串行** → 两者都先取 FREEZE 再取 REMELT（按 class 升序，防死锁）。

## 2. 折叠环（脏队列 drain + ON DUPLICATE KEY UPDATE + 内容版本）

```sql
-- drain: 取一批脏 span, SKIP LOCKED 并发安全
SELECT tenant_id, trace_id, span_id FROM fold_dirty
ORDER BY enqueued_at LIMIT :batch FOR UPDATE SKIP LOCKED;   -- [验] ASTORE + SKIP LOCKED 小流量先验
```
对每个 (tenant,trace) 取 FOLD 锁后折叠（`SET LOCAL query_dop=1` 保序）：
```sql
INSERT INTO span_current (span_id, tenant_id, trace_id, parent_span_id, status, attrs, fold_version, ...)
SELECT span_id, tenant_id, trace_id,
       (array_agg(parent_span_id ORDER BY seq,ts,event_id) FILTER (WHERE parent_span_id IS NOT NULL))[1],
       CASE WHEN bool_or(event_type=5) THEN 2 WHEN bool_or(event_type=3) THEN 1 ELSE 0 END,
       tv_jsonb_deep_merge_agg(attrs_patch ORDER BY seq,ts,event_id),
       count(DISTINCT (span_id,seq)),                       -- [改] fold_version = 内容版本(折叠到的事件集), 不是 max(event_id)
       ...
FROM span_events WHERE tenant_id=:t AND trace_id=:tr GROUP BY span_id
ON DUPLICATE KEY UPDATE                                     -- [改] race-safe, 不是 MERGE
  parent_span_id = COALESCE(EXCLUDED.parent_span_id, span_current.parent_span_id),
  status = GREATEST(span_current.status, EXCLUDED.status),
  attrs  = tv_jsonb_deep_merge_2(span_current.attrs, EXCLUDED.attrs),
  fold_version = EXCLUDED.fold_version,
  encoding_state = CASE WHEN span_current.encoding_state=1 THEN 2 ELSE span_current.encoding_state END;
-- 守卫: 折叠完在同事务删 fold_dirty 对应行(只删本批 span_id), 推进=删队列, 天然幂等
DELETE FROM fold_dirty WHERE tenant_id=:t AND trace_id=:tr AND span_id = ANY(:batch_span_ids);
```
- **[改] fold_version = 内容版本**（折叠到的 `(span_id,seq)` 计数或事件集 hash），守卫用 `<>`（输入集变了就重折），**不用 `max(event_id) >`**：SEQUENCE CACHE 的 event_id 非单调 → 严格 `>` 会把"数值更小但更新的晚到事件"静默丢掉。
- **对账兜底环**（防任何遗漏）：周期扫 `span_events.max(event_id) > span_current.fold_version` 的 span，重新入 `fold_dirty`。
- **前提**：`event_id` 必须是**真正提交单调的雪花**（节点位 + 单调时钟 + 序列），不能用 `SEQUENCE CACHE 1000`。**[验] 列为硬前置 + 跨午夜/乱序回归测试。**

## 3. 冻结（register-FIRST，不删分区）

判定 trace 成形：根已 end 且 `now() - root_end_ts ≥ freeze_grace`（宽限 > 最大 feedback 延迟）。取 FREEZE 锁后跑可重入流水线：
```
step0→1  注册: INSERT frozen_registry (摄入立即把新事件路由进 inbox) + 记 max_event_id_at_enqueue
step1→2  编码: 应用层 O(n) DFS 物化 pre/post/lvl/dotted_order(encoding_state→1)
         ❗同事务 re-check: 若 max(event_id) > max_event_id_at_enqueue → ABORT, 撤注册, 转 fold→remelt
step2→3  写冷: INSERT INTO span_current_cold SELECT 真列 ORDER BY (tenant,trace)(PCK 聚簇); 完成=done
```
- **冻结绝不 DROP 热分区**（修正原稿）。热区回收全交给 §5 的时间分区 GC。
- 崩溃在任一 step：按 `freeze_jobs.step` 重入；每步幂等（注册 ON DUPLICATE DO NOTHING；编码可重算；写冷按 trace 先 DELETE 冷区该 trace 行再插——但 CStore 不可行删，故写冷只在"该 trace 从未写过冷"时执行，用 frozen_registry 判重）。

## 4. 重融化（行存镜像为折叠基，幂等 EXCHANGE）

摄入命中 `frozen_registry` → 事件进 `late_event_inbox` + 入 `remelt_jobs`。取 FREEZE+REMELT 锁后：
```
state0→1 重折: 折叠基 = 行存检索镜像(span_current 保留的检索行 / span_retrieval_cold)  -- [改] 不取 CStore(有损)
         把 inbox 事件 + 镜像基 一起 ON DUPLICATE KEY UPDATE 折叠
state1→2 冷重建: rebuild_version=新值写 remelt_jobs; 建 staging 月分区 INSERT...SELECT(原冷∖本trace ∪ 重折trace);
         EXCHANGE PARTITION 换入; staging 分区头嵌 rebuild_version → 重入时比对版本判断 EXCHANGE 是否已发生(不靠 state 列)
state2→3 清 inbox + 清 late_pending + done
```
- **[改] 折叠基取行存镜像**：CStore 冷区有损（无 attrs/text），从它重建会丢原文。**schema §10 本就要求冷数据保留行存检索镜像** → remelt 正好用它。
- **[改] EXCHANGE 幂等**：冷分区嵌 `rebuild_version`，重入时读分区版本判断是否已 swap，**不靠 state 列**（state 与 DDL 同事务的原子性未证）。
- **payload 重引用**：remelt 引用的 payload 若已被 sweep 到 0 → 先按 sha 取锁 re-CAS-insert 回来再重建（见 §7）。

## 5. 时间分区 GC（与冻结解耦；stop-the-world 预算）

```python
# span_events 热区回收 = 纯时间规则的整分区 DROP, 与单个 trace 冻结无关
def gc_hot_partitions():
  for part in list_partitions('span_events'):   # 从 pg_partition 枚举 sys_pN
    if part.high_bound < now() - max_ttl() - max_feedback_latency():  # 区内每个 trace 都已过 freeze/remelt 窗
      with maintenance_window(), lock_timeout('3s'):   # ❗DROP=AccessExclusiveLock 全表, 必须维护窗+短超时+重试
        payload_ref_decrement_for_partition(part)      # 先记账(§7), 同一逻辑作业
        rescan_late_and_reroute_to_inbox(part)         # [改] DROP 前重扫该分区 frozen_at 后插入的行 → inbox(堵 TOCTOU)
        if partition_exists(part):                     # [改] 幂等: 已不在=成功
          exec(f"ALTER TABLE span_events DROP PARTITION {part.name} UPDATE GLOBAL INDEX")
```
- **[改] 解耦**：DROP 一整天分区会误删区内所有未冻结的活 trace → 只按"分区高界 < now() - 最长TTL - 最大feedback延迟"删，此时区内每个 trace 都证明性地过了所有窗口。
- **[改] stop-the-world**：`DROP/EXCHANGE/TRUNCATE PARTITION` 取**整表 AccessExclusiveLock**，阻塞所有摄入 INSERT + 活 trace 读。**绝不与摄入同跑**：维护窗 + `lock_timeout` 低值 + 失败干净中止重试；只删严格早于活跃写区的分区（只和零星 straggler 争锁）。文档明确"分区 DDL = 该表 stop-the-world"，预算它，别假装在线。
- 冷区 CStore 同理按月整分区 DROP（不可行删）；月内有 annotated/dataset 长留 trace 则整月保留（`gc.cold_keep_if_alive`）。

## 6. 索引生命周期（修正 DROP 顺序 + 死状态）

沿用原稿三区模型（活区 HNSW 在线增量 / 冷区 DiskANN 按段 build / 过渡期 brute-force），修正两点：
- **[改] blue-green 严格"先建新、确认 ACTIVE 可查、再 DROP 旧"**；`BUILD_FAILED` 保留旧索引名 → 召回回退到旧索引而非"无索引"。**绝不 DROP-before-CREATE**（DiskANN 空表不能建 → 一旦 CREATE 失败就裸奔）。
- **[改] 死状态**：每个状态机加 `attempts` 上限 → 超限进 `job_dead_letter` + 告警；**畸形 trace（如 parent=self 成环）退化为按 seq 线性编码**，仍可冻结/回收，**不许卡死整个分区/GC**（红队：一个坏 trace 钉住一个分区 + remelt 也卡死）。
- 查询层是否支持"动态索引名路由"是 blue-green 的前提 → **[验]**。

## 7. TTL + payload CAS GC（修正幂等键 + 宽限窗）

- 差异化 TTL（error 180d / annotated·dataset 永久 / 普通 30d）：物化 `trace_retention.expire_at`，GC 按分区粒度回收（同 §5）。
- **payload refcount**（CAS 去重，多 trace 共享）：
  - **[改] `batch_id` 全局唯一不复用**（分区名 + 单调 epoch）→ re-freeze 重叠数据拿新批，`ON CONFLICT DO NOTHING` 不会把新减记吞掉。
  - 减引用：原子 `UPDATE refcount = refcount - delta`（行锁串行）+ `applied` 守卫同事务（恰好一次）。
  - **[改] sweep 宽限窗 ≥ remelt 窗（天级，不是 300s）**：绝不删"近期引用 trace 仍在 remelt 窗内"的 payload；remelt 引用已 sweep 的 payload 时按 sha 取锁 re-CAS-insert 回来再用。
  - **[改] `refcount < 0` 告警 tripwire** + 周期对账（对未删分区 refcount vs 实际 payload_ref 计数）。

## 8. 写背压（基本沿用，闭环正确）
三级：per-tenant 令牌桶（超限 429 + retry-after）→ embedding 队列积压三档降级（正常/调低采样/只 root+error）→ **折叠 lag 反压**。
> ❗折叠环 `query_dop=1` 保序**不能靠加并行追赶** → 单线程折叠吞吐就是写入速率物理上限，背压信号把它闭环传导回客户端 429。这是设计必须接受的约束（也提示：若中小规模单线程折叠扛不住，才是触发 §kernel-merge-on-read 的真信号 → 但先 PoC 实测）。

## 9. 上线前强制 PoC（红队列为 ship-gate）
1. **DDL-in-TX 原子性**：`INSERT 控制; DROP PARTITION; kill -9` → 验两者都回滚。不原子则改"破坏性 DDL 最后 + 按存在性幂等 + 标记前置事务"。
2. **`FOR UPDATE SKIP LOCKED` + ASTORE** 在 fold_dirty/payload 队列上小流量验。
3. **event_id 真提交单调雪花**（非 SEQUENCE CACHE）+ 跨午夜/乱序折叠回归测试（断言无丢事件）。
4. **EXCHANGE PARTITION + 列存 + 全局索引**：换后显式查全局索引有效性；确认 INTERVAL 分区 DROP+ADD 不可行（sys_pN 不能手动 ADD）下 remelt 冷重建路径成立。
5. **折叠吞吐 benchmark**：1亿 span/天单机，脏队列 drain + 折叠延迟 + 折叠 lag，证明 query_dop=1 单线程够用（否则触发 kernel 算子评估）。
6. 查询层"动态索引名路由"契约（blue-green 前提）。

---
### 一句话
原稿的 bug 是"用时钟和全局 ID 猜正确性"；修正版把它换成**事务脏队列(折叠) + register-first(冻结) + 时间整分区GC(回收) + 行存镜像(重融化) + 单一 xact advisory 锁 + 内容版本守卫**，并把"分区 DDL 是 stop-the-world 且原子性未证"作为必须 PoC 的硬前置。所有修正**仍全在应用层，不动内核**。
