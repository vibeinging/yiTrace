# 附录 M：第六轮后台调度设计原文（核心状态机 + 运维）
> 注：核心 agent 产出中夹杂西语，内容有效；正文修正版见 2026-06-17_tracevault-background-scheduling.md

## M1 核心状态机：折叠/冻结/重融化

Tengo lo suficiente. Sé que MERGE INTO existe en openGauss (la fila se empareja una sola vez por statement), EXCHANGE PARTITION existe para range column-store, advisory locks session-level existen, y DDL es transaccional (como PG, ALTER TABLE ... DROP/ADD/EXCHANGE PARTITION puede ir dentro de una transacción). Tengo suficiente para producir el diseño. Lo entrego directamente.

---

# yiTrace — Diseño de las tres rondas de scheduling (capa de aplicación, fuera del kernel)

Convenciones: todo proceso de fondo es un **worker idempotente** con un `worker_id`, corre en loop, toma un **advisory lock** por (ronda, shard) para garantizar un único ejecutor lógico por shard, y persiste su watermark en una tabla de control antes de hacer trabajo irreversible. Aislamiento por defecto = **READ COMMITTED** (default openGauss); donde se necesita lectura consistente del registro de control se usa una transacción corta. El folding usa `SET LOCAL query_dop = 1` para preservar orden en la agregación ordenada.

---

## 0. Tablas de control (DDL nuevas, no tocan el schema de datos)

```sql
-- Watermark del folding microbatch, por shard (shard = hash(tenant) % K, o por tenant)
CREATE TABLE tv_ctrl_fold_watermark (
  shard_id        int           NOT NULL PRIMARY KEY,
  last_fold_ts    timestamptz   NOT NULL DEFAULT '-infinity',  -- ingest_ts ya consolidado
  safe_lag_secs   int           NOT NULL DEFAULT 5,            -- delta de retroceso
  updated_at      timestamptz   NOT NULL DEFAULT now(),
  worker_id       text                                          -- diagnóstico/lease
);

-- Cola de trabajo de freeze (estado del pipeline por trace, reentrante)
CREATE TABLE tv_ctrl_freeze_jobs (
  tenant_id     bigint        NOT NULL,
  trace_id      bigint        NOT NULL,
  -- step alcanzado de forma DURABLE (el commit de cada paso lo avanza)
  step          smallint      NOT NULL DEFAULT 0,  -- 0=pending 1=encoded 2=cold_written 3=registered 4=hot_dropped(done)
  root_end_ts   timestamptz   NOT NULL,            -- ts del evento 3/end del root
  cold_partition text,                             -- nombre de partición destino en cold
  attempts      int           NOT NULL DEFAULT 0,
  last_error    text,
  updated_at    timestamptz   NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, trace_id)
);

-- Cola de re-melt (late arrivals). late_pending también se marca en span_current del root,
-- pero esta tabla es la unidad de trabajo del worker.
CREATE TABLE tv_ctrl_remelt_jobs (
  tenant_id      bigint       NOT NULL,
  trace_id       bigint       NOT NULL,
  state          smallint     NOT NULL DEFAULT 0,  -- 0=queued 1=refolded 2=cold_rebuilt(swapped) 3=done
  new_cold_part  text,                              -- partición staging para rebuild
  enqueued_at    timestamptz  NOT NULL DEFAULT now(),
  updated_at     timestamptz  NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, trace_id)
);
```

Claves de advisory lock (constantes de clase de bloqueo + objeto):
- `FOLD_CLASS = 0x01`, objeto = `shard_id`
- `FREEZE_CLASS = 0x02`, objeto = `hash(tenant_id, trace_id)`
- `REMELT_CLASS = 0x03`, objeto = `hash(tenant_id, trace_id)`

`pg_try_advisory_lock(class::int, obj::int)` (no bloqueante; si falla, otro worker lo tiene → skip). Se libera con `pg_advisory_unlock`. **El freeze y el remelt comparten el mismo objeto** (`hash(tenant,trace)`) pero con clase distinta; ver §3.4 para el aislamiento cruzado (un remelt y un freeze del mismo trace deben serializarse — se hace tomando *ambas* clases, ordenadas por clase ascendente, para evitar deadlock).

---

## 1. Diagrama de estados (texto)

### 1.1 Ciclo de vida del span (`encoding_state` en `span_current`)
```
                INSERT evento 1/start
                       │
                       ▼
        ┌─────────────────────────────┐
        │ 0 = activo (folding vivo)    │◄─────────┐ re-fold (microbatch) mantiene 0
        └─────────────┬───────────────┘          │
                      │ freeze step a) DFS         │
                      ▼                            │
        ┌─────────────────────────────┐           │
        │ 1 = encoded (pre/post fijo)  │           │ late event golpea trace congelado:
        └─────────────┬───────────────┘           │   span vuelve a participar en re-fold
                      │ (root drop / cold escrito) │   encoding_state → 2 = stale
                      ▼                            │
        ┌─────────────────────────────┐           │
        │ (fila hot borrada al drop    │           │
        │  de partición; vive en cold) │───late────┘
        └─────────────────────────────┘
   3 = overflow: rama de fallback si DFS detecta árbol degenerado / ciclo
```

### 1.2 Estado del trace (vista lógica, derivada de root + control)
```
 ACTIVE ──root recibe end (evt 3)──► SETTLING ──(≥N días sin nuevo evento)──► FREEZING
   ▲                                    │                                        │
   │ nuevo evento                       │ nuevo evento (reabre)                  │ pipeline 4 pasos
   └────────────────────────────────────┘                                        ▼
                                                                              FROZEN
                                                                                 │
                                            evento llega y frozen_registry hit   │
                                                          ▼                      │
                                                    LATE_PENDING ◄───────────────┘
                                                          │ remelt (refold + cold rebuild)
                                                          ▼
                                                       FROZEN (versión cold reconstruida)
```

### 1.3 Pipeline de freeze (durable, reentrante) — `tv_ctrl_freeze_jobs.step`
```
0 pending ──DFS/encode (TX1)──► 1 encoded ──INSERT cold (TX2)──► 2 cold_written
   ──INSERT frozen_registry (TX3)──► 3 registered ──DROP hot partition seguro (TX4)──► 4 done
Crash en cualquier paso ⇒ reanudar desde `step` (cada paso es idempotente, ver §3.5)
```

### 1.4 Pipeline de remelt — `tv_ctrl_remelt_jobs.state`
```
0 queued ──re-fold trace c/inbox (TX1)──► 1 refolded
   ──build staging cold part + EXCHANGE/replace (TX2)──► 2 cold_rebuilt
   ──limpiar inbox + clear late_pending + done (TX3)──► 3 done
```

---

## 2. Ronda ① — Microbatch Folding

### 2.1 Idea de watermark (clave: `ingest_ts` vs `ts`)
- `ts` = tiempo lógico del evento (puede llegar tarde/desordenado).
- `ingest_ts` = tiempo de escritura en el kernel, **monótono no decreciente por la fila al INSERT** (lo asigna el ingestor). El folding avanza sobre `ingest_ts` porque es lo que crece de forma predecible; `ts` puede saltar al pasado.
- **No se puede usar `now()` como watermark** porque eventos con `ingest_ts < now()` pueden seguir entrando en vuelo (commit pendiente). Por eso el watermark efectivo es `max_seen_ingest_ts - safe_lag_secs` (δ), donde δ ≥ el mayor skew de commit observado (transacciones de ingest abiertas). Esto evita el clásico "lost update por visibilidad MVCC retrasada".

> **Incierto / a calibrar:** el límite real de δ depende de cuánto puede durar una transacción de ingest abierta. Recomiendo derivar δ dinámicamente de `pg_stat_activity` (la `xact_start` más antigua de sesiones de ingest) en lugar de una constante. Marcado como punto a validar en carga real.

### 2.2 SQL de escaneo (qué (tenant,trace,span) refoldar)
```sql
-- Parámetros: :shard_id, watermark anterior :last_fold_ts, watermark nuevo :hi_ts
-- hi_ts = (SELECT max(ingest_ts) FROM span_events WHERE shard=:shard_id) - interval ':delta sec'
-- Ventana = (last_fold_ts, hi_ts]. Se RE-ESCANEA solapando [last_fold_ts - safe_lag] por seguridad.
SELECT e.tenant_id, e.trace_id, e.span_id
FROM   span_events e
WHERE  hash_shard(e.tenant_id) = :shard_id
  AND  e.ingest_ts >  (:last_fold_ts - interval ':delta sec')   -- retroceso de seguridad
  AND  e.ingest_ts <= :hi_ts
GROUP BY e.tenant_id, e.trace_id, e.span_id;
```
El retroceso `last_fold_ts - δ` re-procesa una banda ya foldada; es **seguro porque el fold es idempotente** (ver guarda `fold_version` abajo). El borde superior `hi_ts` excluye la zona "caliente" donde aún puede haber commits en vuelo.

### 2.3 MERGE de folding (por grupo (tenant,trace), `query_dop=1`)
```sql
-- Una TX por (tenant,trace). SET LOCAL query_dop=1 preserva el orden de la agregación ordenada.
BEGIN;
SET LOCAL query_dop = 1;

-- Sub-agregación: pliega TODOS los eventos del span hasta hi_ts en una fila candidata.
-- attrs_merge_ordered() = agregado ordenado custom (deep-merge por seq asc).
WITH folded AS (
  SELECT e.span_id,
         e.tenant_id, e.trace_id,
         min(e.parent_span_id)                               AS parent_span_id,
         max(e.seq)                                          AS max_seq,
         attrs_merge_ordered(e.attrs_patch ORDER BY e.seq)   AS attrs,
         max(e.ts) FILTER (WHERE e.event_type = 1)           AS start_time,
         max(e.ts) FILTER (WHERE e.event_type = 3)           AS end_time,
         bool_or(e.event_type = 3)                           AS has_end,
         bool_or(e.event_type = 5)                           AS has_error,
         -- texto de retrieval extraído del patch consolidado
         input_text_of (attrs_merge_ordered(e.attrs_patch ORDER BY e.seq)) AS input_text,
         output_text_of(attrs_merge_ordered(e.attrs_patch ORDER BY e.seq)) AS output_text,
         count(*)                                            AS n_events
  FROM   span_events e
  WHERE  e.tenant_id = :tenant_id AND e.trace_id = :trace_id
    AND  e.ingest_ts <= :hi_ts
  GROUP BY e.span_id, e.tenant_id, e.trace_id
)
MERGE INTO span_current c
USING folded f
ON (c.span_id = f.span_id)
WHEN MATCHED AND c.encoding_state IN (0,2)            -- sólo plegar spans activos o stale
                 AND f.n_events > c.fold_version       -- GUARDA de idempotencia (ver §2.4)
  THEN UPDATE SET
        attrs        = f.attrs,
        parent_span_id = f.parent_span_id,
        status       = CASE WHEN f.has_error THEN 'error'
                            WHEN f.has_end   THEN 'ended' ELSE 'running' END,
        input_text   = f.input_text,
        output_text  = f.output_text,
        encoding_state = 0,                            -- sigue activo hasta freeze
        fold_version = f.n_events,                     -- nuevo watermark por-span
        frozen_at    = NULL
WHEN NOT MATCHED
  THEN INSERT (span_id, tenant_id, trace_id, parent_span_id, status,
               attrs, input_text, output_text, encoding_state, fold_version)
       VALUES (f.span_id, f.tenant_id, f.trace_id, f.parent_span_id,
               CASE WHEN f.has_error THEN 'error'
                    WHEN f.has_end   THEN 'ended' ELSE 'running' END,
               f.attrs, f.input_text, f.output_text, 0, f.n_events);
COMMIT;
```

### 2.4 Idempotencia (guarda `fold_version` + avance de watermark transaccional)
- `fold_version` por span = `count(*)` de eventos plegados (monótono: nunca decrece porque la tabla es append-only). La cláusula `WHEN MATCHED AND f.n_events > c.fold_version` hace el MERGE **idempotente**: re-correr la misma ventana no cambia nada (mismo `n_events`), y un crash a mitad sólo deja spans sin plegar que el siguiente ciclo recoge.
  - *Nota:* `count(*)` es un monótono robusto sólo si no hay borrado de eventos. Si pudiera haber dedup, usar `max(event_id)` (snowflake monótono) como `fold_version` en su lugar. **Recomendado: `fold_version = max(event_id)`** por ser estrictamente monótono globalmente y resistente a re-conteos.
- **El avance del watermark va en transacción separada y posterior** al fold de todos los grupos del batch:
```sql
-- Sólo tras commitear todos los grupos del batch:
UPDATE tv_ctrl_fold_watermark
SET    last_fold_ts = :hi_ts, updated_at = now(), worker_id = :worker_id
WHERE  shard_id = :shard_id AND last_fold_ts = :last_fold_ts;  -- CAS optimista
```
El `WHERE last_fold_ts = :last_fold_ts` es un compare-and-swap: si otro worker avanzó el watermark, este UPDATE afecta 0 filas y el worker descarta su avance (trabajo ya hecho por el otro). Crash entre folds y avance de watermark ⇒ el watermark no avanza ⇒ re-fold de la ventana ⇒ inocuo por la guarda.

### 2.5 Pseudocódigo ronda ①
```
loop every 5..15s:
  for shard in my_shards:
    if not pg_try_advisory_lock(FOLD_CLASS, shard): continue
    try:
      (last_fold_ts, delta) := SELECT last_fold_ts, safe_lag_secs FROM tv_ctrl_fold_watermark WHERE shard
      max_ingest := SELECT max(ingest_ts) FROM span_events WHERE shard
      delta_dyn  := max(delta, now() - oldest_open_ingest_xact_start())   // δ dinámico (incierto, calibrar)
      hi_ts := max_ingest - delta_dyn
      if hi_ts <= last_fold_ts: continue                                   // nada nuevo consolidado
      groups := scan_dirty_groups(shard, last_fold_ts - delta_dyn, hi_ts)  // §2.2
      for (tenant,trace) in distinct(groups):
        fold_trace(tenant, trace, hi_ts)                                   // §2.3 MERGE, una TX
      advance_watermark_CAS(shard, last_fold_ts -> hi_ts)                  // §2.4
    finally:
      pg_advisory_unlock(FOLD_CLASS, shard)
```

---

## 3. Ronda ② — Freeze (judgement + pipeline)

### 3.1 Judgement: ¿trace "formado"?
Condición: root con `event_type=3` (end) **y** sin eventos nuevos en ≥ N días (N = ventana de gracia > máxima latencia de feedback observada).
```sql
-- Encolar candidatos a freeze. last_event_at se deriva de span_events (no de span_current).
INSERT INTO tv_ctrl_freeze_jobs (tenant_id, trace_id, root_end_ts, step)
SELECT r.tenant_id, r.trace_id, r.end_ts, 0
FROM (
  -- root = span sin parent; su end y el último evento del trace
  SELECT c.tenant_id, c.trace_id,
         max(e_root.ts) FILTER (WHERE e_root.event_type=3)        AS end_ts,
         max(le.ingest_ts)                                        AS last_event_at,
         bool_or(e_root.event_type=3)                             AS root_ended
  FROM   span_current c
  JOIN   span_events  e_root ON e_root.span_id = c.span_id           -- eventos del root
  JOIN   span_events  le     ON le.trace_id = c.trace_id            -- cualquier evento del trace
  WHERE  c.parent_span_id IS NULL
  GROUP  BY c.tenant_id, c.trace_id
) r
WHERE r.root_ended
  AND r.last_event_at < now() - interval ':N days'
ON CONFLICT (tenant_id, trace_id) DO NOTHING;   -- no re-encolar lo ya en pipeline
```
> El JOIN amplio sobre `span_events` es caro; en producción esto se materializa incrementalmente (p.ej. un `last_event_at` por trace mantenido en una tabla de resumen actualizada por el folding). Marcado como optimización pendiente.

### 3.2 Paso a) DFS materializa pre/post/lvl/dotted_order (en aplicación, no en kernel)
La app lee todos los spans del trace, hace **DFS O(n)** en memoria, calcula `pre/post/lvl/dotted_order`, y los escribe vía COPY a una tabla temporal + UPDATE join (o COPY a un staging). `encoding_state → 1`.
```sql
BEGIN;  -- TX2-a (step 0 -> 1)
SET LOCAL query_dop = 1;
-- COPY tv_tmp_encode(span_id, pre, post, lvl, dotted_order) FROM STDIN  (lo hace el cliente)
UPDATE span_current c
SET    pre = t.pre, post = t.post, lvl = t.lvl, dotted_order = t.dotted_order,
       encoding_state = 1, frozen_at = now()
FROM   tv_tmp_encode t
WHERE  c.span_id = t.span_id
  AND  c.encoding_state IN (0,2);   -- idempotente: si ya está en 1 no re-escribe
UPDATE tv_ctrl_freeze_jobs SET step=1, updated_at=now()
WHERE tenant_id=:t AND trace_id=:tr AND step=0;
COMMIT;
```
Si DFS detecta ciclo/huérfanos → `encoding_state = 3 (overflow)`, se marca el job con error y se saca de la cola normal (revisión manual / fallback lineal por `seq`).

### 3.3 Paso b) INSERT a cold (CStore, ORDER BY para clustering PCK)
```sql
BEGIN;  -- TX2-b (step 1 -> 2)
INSERT INTO span_current_cold (tenant_id, trace_id, span_id, start_time, /* ...real cols... */
                               lvl, dotted_order)
SELECT c.tenant_id, c.trace_id, c.span_id, c.start_time, /* ... */
       c.lvl, c.dotted_order
FROM   span_current c
WHERE  c.tenant_id=:t AND c.trace_id=:tr AND c.encoding_state=1
ORDER BY c.tenant_id, c.trace_id, c.dotted_order;   -- clustering físico para PCK / min-max skip
UPDATE tv_ctrl_freeze_jobs SET step=2, cold_partition = :target_part, updated_at=now()
WHERE tenant_id=:t AND trace_id=:tr AND step=1;
COMMIT;
```
**Idempotencia del INSERT a CStore:** CStore es append-only, así que re-correr este paso duplicaría filas. La guarda es `step`: si crash ocurrió *después* del COMMIT de TX2-b, `step=2` ya está y el reintento salta este paso. Si crash *antes* del COMMIT, la transacción aborta y **no deja filas visibles** (la inserción CStore se revierte con la TX, igual que en heap, porque los datos no committeados no son visibles). Por eso cada paso es una TX atómica: la atomicidad MVCC nos da el "todo-o-nada" aunque CStore no permita update in-place.

### 3.4 Paso c) frozen_registry
```sql
BEGIN;  -- TX2-c (step 2 -> 3)
INSERT INTO frozen_registry (tenant_id, trace_id, frozen_at, cold_partition)
VALUES (:t, :tr, now(), :target_part)
ON CONFLICT (tenant_id, trace_id) DO NOTHING;       -- idempotente
UPDATE tv_ctrl_freeze_jobs SET step=3, updated_at=now()
WHERE tenant_id=:t AND trace_id=:tr AND step=2;
COMMIT;
```
**Orden importa:** registrar en `frozen_registry` ANTES de borrar la partición caliente. Así, una vez `step≥3`, el ingestor ya redirige late events a `late_event_inbox` (consulta `frozen_registry`), cerrando la ventana de carrera "evento llega justo mientras borro el hot".

### 3.5 Paso d) DROP seguro de partición caliente
```sql
BEGIN;  -- TX2-d (step 3 -> 4 done)
-- Sólo si grace > max feedback delay. Se borran las filas hot del trace ya en cold.
-- Opción A (preferida si la partición agrupa sólo traces ya congelados): DROP PARTITION
ALTER TABLE span_events DROP PARTITION p_2026_06;   -- partición temporal RANGE por ts ya fría
-- Opción B (si la partición mezcla traces vivos y fríos): DELETE selectivo de span_current
DELETE FROM span_current
WHERE tenant_id=:t AND trace_id=:tr AND encoding_state=1;
UPDATE tv_ctrl_freeze_jobs SET step=4, updated_at=now()
WHERE tenant_id=:t AND trace_id=:tr AND step=3;
COMMIT;
```
> **Decisión de granularidad (incierto):** `DROP PARTITION` es O(1) y barato pero sólo es seguro cuando *toda* la partición RANGE-by-ts ya está congelada (ningún trace vivo). Como los traces "nacen de mañana y mueren de tarde", una partición vieja (> N días) cumple esto casi siempre. La regla práctica: el worker hace `DROP PARTITION` a nivel de **partición completa** sólo cuando *todos* los traces cuyos eventos caen en esa partición tienen `step=4` pendiente; mientras tanto usa `DELETE` selectivo en `span_current`. Recomiendo procesar el freeze **agrupado por partición temporal** para poder usar DROP. Marcado para validar contra el patrón real de retención.

### 3.6 Reentrada por crash (pipeline)
```
resume_freeze(t, tr):
  job := SELECT * FROM tv_ctrl_freeze_jobs WHERE (t,tr)
  acquire pg_advisory_lock(FREEZE_CLASS, hash(t,tr))   // y también REMELT_CLASS (orden asc) si hay remelt
  switch job.step:
    0: do step_a(); fallthrough
    1: do step_b(); fallthrough
    2: do step_c(); fallthrough
    3: do step_d(); fallthrough
    4: delete job (done)
```
Cada `step_x` re-ejecuta su TX; las guardas (`step=N-1` en el UPDATE, `ON CONFLICT DO NOTHING`, `encoding_state` filtrado) hacen que un paso ya aplicado sea no-op. La fuente de verdad del progreso es `tv_ctrl_freeze_jobs.step`, **committeado junto con el efecto del paso** en la misma TX — nunca puede divergir del estado real.

---

## 4. Ronda ③ — Re-melt de late arrivals

### 4.1 Ingest: detección de hit en frozen_registry
En el path de ingest (no es worker, pero es donde se origina): al insertar un evento, si `(tenant,trace) ∈ frozen_registry`, el evento va a `late_event_inbox` y se marca el trace `late_pending`.
```sql
-- En el ingestor, por lote de eventos:
WITH hit AS (
  SELECT e.* FROM staging_events e
  JOIN frozen_registry f USING (tenant_id, trace_id)
)
INSERT INTO late_event_inbox SELECT * FROM hit;     -- inbox LIKE span_events

INSERT INTO tv_ctrl_remelt_jobs (tenant_id, trace_id, state)
SELECT DISTINCT tenant_id, trace_id, 0 FROM hit
ON CONFLICT (tenant_id, trace_id) DO UPDATE SET updated_at = now();  -- coalesce múltiples late
-- (los eventos NO-hit siguen el path normal a span_events)
```

### 4.2 Worker re-melt — paso 1: re-fold del trace con inbox
```sql
BEGIN;  -- TX3-1 (state 0 -> 1)
SET LOCAL query_dop = 1;
-- Re-plegar uniendo eventos originales (si la partición hot ya no existe, se re-leen de cold
-- como base) + late_event_inbox. Aquí el "base" es el estado cold actual del span.
WITH allev AS (
  SELECT * FROM late_event_inbox WHERE tenant_id=:t AND trace_id=:tr
)
MERGE INTO span_current c
USING ( SELECT span_id, attrs_merge_ordered(attrs_patch ORDER BY seq) AS attrs, ...
        FROM allev GROUP BY span_id ) f
ON (c.span_id = f.span_id)
WHEN MATCHED THEN UPDATE SET attrs = deep_merge(c.attrs, f.attrs),
                             encoding_state = 2,   -- stale: requiere re-encode/re-cold
                             fold_version = greatest(c.fold_version, f.max_event_id)
WHEN NOT MATCHED THEN INSERT (...) VALUES (...);   -- late event crea span nuevo del trace
UPDATE tv_ctrl_remelt_jobs SET state=1, updated_at=now()
WHERE tenant_id=:t AND trace_id=:tr AND state=0;
COMMIT;
```
Si el trace ya no está en `span_current` (filas hot borradas en freeze), el worker primero **rehidrata** desde la partición cold a `span_current` (re-INSERT de filas) para tener base de folding, luego aplica el inbox. Tras esto re-corre el DFS (paso a del freeze) porque la topología pudo cambiar (un late event puede ser un span nuevo).

### 4.3 Paso 2: rebuild de la partición cold (NO update — explicación)
**Por qué rebuild y no update:** `span_current_cold` es CStore, **append-only, sin update in-place**. No existe `UPDATE`/`DELETE` eficiente de filas en una partición CStore comprimida; modificar requiere reescribir. La técnica:
1. Crear partición/tabla **staging** con misma estructura.
2. `INSERT ... SELECT` que **fusiona**: (filas de la partición cold original que NO pertenecen al trace re-melted) ∪ (filas nuevas recalculadas del trace), con `ORDER BY` para mantener clustering.
3. Intercambiar atómicamente con `ALTER TABLE ... EXCHANGE PARTITION ... WITH TABLE` (soportado en range column-store) o, si se reconstruye partición completa, `DROP PARTITION` viejo + `ADD PARTITION` + carga — todo en una sola TX (DDL es transaccional en openGauss).

```sql
BEGIN;  -- TX3-2 (state 1 -> 2)
-- 1) tabla ordinaria staging con misma definición de columnas que la partición cold
CREATE TABLE tv_stage_cold_:part (LIKE span_current_cold INCLUDING ALL) WITH (ORIENTATION=column);

-- 2) merge: cold viejo SIN el trace + filas nuevas del trace
INSERT INTO tv_stage_cold_:part
SELECT * FROM span_current_cold PARTITION (:part)
WHERE  NOT (tenant_id=:t AND trace_id=:tr)
UNION ALL
SELECT tenant_id, trace_id, span_id, start_time, /*...*/, lvl, dotted_order
FROM   span_current
WHERE  tenant_id=:t AND trace_id=:tr AND encoding_state=2
ORDER BY tenant_id, trace_id, dotted_order;   -- re-clustering

-- 3) swap atómico (requiere igualdad de columnas; sin unique index en la ordinaria)
ALTER TABLE span_current_cold EXCHANGE PARTITION (:part) WITH TABLE tv_stage_cold_:part
      WITHOUT VALIDATION UPDATE GLOBAL INDEX;
DROP TABLE tv_stage_cold_:part;

UPDATE tv_ctrl_remelt_jobs SET state=2, new_cold_part=:part, updated_at=now()
WHERE tenant_id=:t AND trace_id=:tr AND state=1;
COMMIT;
```
> **Incierto / a confirmar en la versión exacta de openGauss:**
> - Que `EXCHANGE PARTITION ... WITH TABLE` acepte una tabla ordinaria **column-store** para intercambio con partición column-store (la doc confirma EXCHANGE para range, y que range puede ser column-store; conviene un smoke test). Fallback robusto: `ALTER TABLE ... DROP PARTITION :part` + `ADD PARTITION :part` + `INSERT...SELECT` desde staging, dentro de la misma TX.
> - Que el conjunto de DDL+DML de TX3-2 sea atómico (revierte si crash a mitad). En openGauss/PG el DDL es transaccional, así que un crash deja la partición cold original intacta y `state` sin avanzar → reintento limpio.

### 4.4 Paso 3: limpieza
```sql
BEGIN;  -- TX3-3 (state 2 -> 3 done)
DELETE FROM late_event_inbox WHERE tenant_id=:t AND trace_id=:tr;
UPDATE frozen_registry SET frozen_at = now() WHERE tenant_id=:t AND trace_id=:tr;  -- re-sella
-- limpiar marca late_pending y volver encoding_state -> 1 (encoded) en span_current,
-- o borrar filas hot rehidratadas si la política es "frío vive sólo en cold"
DELETE FROM tv_ctrl_remelt_jobs WHERE tenant_id=:t AND trace_id=:tr AND state=2;
COMMIT;
```

### 4.5 Concurrencia con el folding normal (aislamiento)
- **Mismo trace, freeze vs remelt:** ambos toman advisory locks sobre `hash(tenant,trace)`. Para evitar que un freeze en vuelo y un remelt del mismo trace se pisen, el remelt adquiere **tanto `FREEZE_CLASS` como `REMELT_CLASS`** (en orden de clase ascendente para prevenir deadlock) antes de tocar el trace; el freeze hace lo simétrico. Resultado: freeze y remelt de un mismo trace se serializan; de traces distintos corren en paralelo.
- **Folding microbatch vs remelt:** el folding microbatch **excluye** traces con `late_pending` / presentes en `tv_ctrl_remelt_jobs` (filtro `NOT EXISTS (SELECT 1 FROM tv_ctrl_remelt_jobs ...)` en el scan §2.2). Así un trace en re-melt no es tocado por el folding normal. Al terminar el remelt (state=3, job borrado), el folding lo vuelve a ver si llegan más eventos.
- **Ingest vs remelt:** mientras el worker hace TX3-2, nuevos late events del mismo trace siguen entrando al `inbox` (no bloqueados). Como el remelt borra del inbox sólo lo que existía al inicio (filtrar por un `enqueued_at <= job.started_at` o por snapshot de event_ids leídos), los que llegan durante el rebuild quedan en el inbox y disparan otro ciclo de remelt (job se re-encola por el `ON CONFLICT DO UPDATE`). **Recomendado:** capturar el conjunto de `event_id` procesados al inicio de TX3-1 y borrar sólo esos en TX3-3, para no perder late-de-late.

### 4.6 Pseudocódigo ronda ③
```
loop every 10..30s:
  for (t,tr) in SELECT tenant_id,trace_id FROM tv_ctrl_remelt_jobs WHERE state<3 ORDER BY enqueued_at:
    if not try_lock_ordered(FREEZE_CLASS, REMELT_CLASS, hash(t,tr)): continue
    try:
      job := load remelt_job(t,tr)
      if frozen rows gone from span_current: rehydrate_from_cold(t,tr)
      switch job.state:
        0: refold_with_inbox(t,tr); redo_dfs_encode(t,tr); -> state=1; fallthrough   // §4.2
        1: rebuild_cold_partition(t,tr); -> state=2; fallthrough                      // §4.3
        2: cleanup_inbox_and_reseal(t,tr); -> done                                    // §4.4
    finally:
      unlock(REMELT_CLASS, hash); unlock(FREEZE_CLASS, hash)
```

---

## 5. Resumen de puntos inciertos (a validar antes de producción)
1. **δ (safe_lag) dinámico** desde la `xact_start` más antigua de ingest, no constante fija. Validar contra duración real de transacciones de ingest.
2. **`fold_version = max(event_id)`** (snowflake) en lugar de `count(*)` para monotonicidad estricta frente a posibles dedups.
3. **Granularidad del DROP en freeze paso d):** `DROP PARTITION` completa (O(1)) sólo cuando toda la partición RANGE-by-ts está fría; si no, `DELETE` selectivo. Procesar freeze agrupado por partición temporal para habilitar DROP.
4. **`EXCHANGE PARTITION ... WITH TABLE` con tablas column-store** y atomicidad del bloque DDL+DML de TX3-2 — confirmar con smoke test en la versión exacta de openGauss; fallback `DROP+ADD PARTITION+INSERT...SELECT` en la misma TX.
5. **`last_event_at` por trace** debe materializarse incrementalmente (tabla resumen mantenida por el folding) en vez del JOIN amplio sobre `span_events` del judgement §3.1.
6. **Rehidratación cold→hot** en remelt: definir política (¿el frío vive sólo en cold, o se reconstruye fila hot temporal?). Afecta si TX3-3 borra o conserva filas hot.

Sources:
- [openGauss Advisory Lock Functions](https://docs.opengauss.org/en/docs/2.0.0/docs/Developerguide/advisory-lock-functions.html)
- [openGauss Alter Table Partition (EXCHANGE/DROP/ADD)](https://docs.opengauss.org/en/docs/5.1.0/docs/SQLReference/alter-table-partition.html)
- [openGauss SQL Syntax (MERGE INTO)](https://docs.opengauss.org/en/docs/5.1.0/docs/SQLReference/sql-syntax.html)
- [openGauss Column Store](https://docs.opengauss.org/en/docs/3.1.1/docs/BriefTutorial/column-store.html)
---

## M2 索引生命周期/TTL/payload GC/背压

I have enough confirmed facts. One critical finding: interval partitions **cannot** be manually `ADD PARTITION`ed (only auto-created on INSERT), but they **can** be `DROP`/`TRUNCATE`/`EXCHANGE`ed by name. This shapes the GC design. Now producing the full deliverable.

---

# yiTrace 后台调度逻辑设计（纯应用层，内核外）

## 0. 全局约定（所有作业共用）

**已验证的 openGauss 事实**（标注 ✅ 已验证 / ⚠️ 待验 / ❗约束）：
- ✅ 支持 `pg_try_advisory_xact_lock(key1,key2)`、`pg_advisory_xact_lock`，事务结束自动释放。
- ✅ 支持 `ALTER TABLE ... DROP PARTITION p [UPDATE GLOBAL INDEX]` / `TRUNCATE PARTITION` / `EXCHANGE PARTITION ... WITH TABLE ... [WITHOUT VALIDATION]`。
- ✅ RANGE+INTERVAL 分区：INSERT 命中空隙时**自动建分区**（命名 `sys_pN`），但 ❗**不能手动 `ALTER TABLE ADD PARTITION`**。GC 只能按 partition_name DROP/TRUNCATE。
- ✅ `MERGE INTO ... WHEN MATCHED/NOT MATCHED [WHERE]` 支持；默认隔离级 READ COMMITTED。
- ⚠️ DiskANN/HybridANN「空表不能建索引」「inplace filter」是 yiTrace 私有扩展约束，无公开文档，按你给的前提当硬约束处理。
- ⚠️ CStore 列存「仅追加、不可原地改」→ 冷区**不能 UPDATE/DELETE 行**，回收只能整分区 DROP。

**共用的调度骨架（所有作业都遵守）**：

```
JOB_REGISTRY 表 = 持久化水位 + 幂等锁 + 心跳
每个 worker 单线程跑一类 job；用 advisory_xact_lock 防同名 job 并发
每步先读水位 → 干活 → 在同一事务里推进水位（水位与副作用同事务=崩溃可恢复）
所有"副作用 SQL"必须幂等：用 WHERE 守卫 / NOT EXISTS / ON CONFLICT / 状态列 CAS
```

```sql
-- 调度元数据（行存 ASTORE）
CREATE TABLE job_registry (
  job_name        text PRIMARY KEY,         -- 'embed_sampler' / 'gc_ttl' / 'payload_gc' / 'index_lifecycle'
  shard_key       text NOT NULL DEFAULT '*',-- 可按 tenant 分片；'*'=全局单实例
  watermark       jsonb NOT NULL DEFAULT '{}'::jsonb, -- 该 job 的进度游标(见各作业)
  lease_owner     text,                     -- 当前持有者 worker_id
  lease_until     timestamptz,              -- 租约到期(看门狗)
  last_run_at     timestamptz,
  last_ok_at      timestamptz,
  consec_errors   int NOT NULL DEFAULT 0,
  state           smallint NOT NULL DEFAULT 0 -- 0 idle / 1 running / 2 backoff
);

-- advisory lock 命名空间：key1=job域常量, key2=hashtext(job_name||shard_key)
-- 例: SELECT pg_try_advisory_xact_lock(74010, hashtext('gc_ttl:*'));
```

**配置参数（全局）**

| 参数 | 默认 | 说明 |
|---|---|---|
| `job.lease_ttl_sec` | 60 | 租约时长，看门狗 2×TTL 抢占 |
| `job.tick_interval_sec` | 5 | 调度轮询间隔 |
| `job.max_consec_errors` | 5 | 超过则进 backoff，指数退避 |
| `job.batch_size` | 1000 | 单批处理行数（防长事务） |

---

## ① 向量 / BM25 索引生命周期

### 1.1 三区语义召回模型（呼应 schema）

| 区 | 物理位置 | 索引策略 | 重建方式 |
|---|---|---|---|
| **活区（hot）** | `span_vectors` 中 `frozen_at IS NULL` 的近期采样 | 小 HNSW（增量友好，可在小表上在线 INSERT 维护） | 在线增量，不重建 |
| **冷区（cold/sealed）** | 已 frozen 的 trace 的采样向量 | 大 DiskANN（高召回、低内存、批量构建） | 按「段」(seal 批次)批量 build |
| **(过渡)** 新租户/新分区 | 空 → 数据未达阈值 | **无索引**（顺序扫 brute-force） | 攒够数据后首次 build |

核心约束：**DiskANN 不能空表建索引** → 状态机必须有「先灌数据、达阈值再建」的门槛。

### 1.2 索引状态机（per tenant × per 区）

```
                  ┌─────────────────────────────────────────────┐
                  ▼                                             │
[NO_INDEX]  ──rows>=build_threshold──▶ [BUILDING] ──ok──▶ [ACTIVE]
 (brute-force扫,                          │  fail              │
  对查询透明)                              ▼                    │ 增量INSERT(活区HNSW)
                                      [BUILD_FAILED]            │ 累计新增>rebuild_ratio
                                       (backoff重试)            ▼
                                                          [STALE]──segment build──▶[ACTIVE]
                                                          (冷区:积累新seal段
                                                           触发批量重建/合并)
```

`span_vectors` 不带 `encoding_state`（那是 `span_current` 的列）；索引态存在 `index_lifecycle` 表里，不污染数据表：

```sql
CREATE TABLE index_lifecycle (
  tenant_id    bigint NOT NULL,
  index_scope  smallint NOT NULL,  -- 0 hot-hnsw / 1 cold-diskann / 2 bm25-fulltext
  state        smallint NOT NULL DEFAULT 0, -- 0 NO_INDEX/1 BUILDING/2 ACTIVE/3 STALE/4 FAILED
  index_name   text,               -- 实际 index 对象名(分区/段后缀)
  row_estimate bigint NOT NULL DEFAULT 0,
  built_rows   bigint NOT NULL DEFAULT 0, -- 上次 build 时的行数,用于算 rebuild_ratio
  last_build_at timestamptz,
  PRIMARY KEY (tenant_id, index_scope)
);
```

### 1.3 Embedding 异步采样器（只 root/LLM/error span）

采样判定在折叠环写 `span_current` 时打 `is_sampled_for_vector` 标记（避免重扫），采样器只搬已标记、未向量化的行。

```sql
-- 采样判定(折叠环里 or 采样器里二选一)。这里给采样器拉取SQL:
-- 选出"该embed但还没进span_vectors"的span
SELECT c.span_id, c.tenant_id, c.input_text, c.output_text, c.attrs->>'span_kind' AS kind
FROM span_current c
WHERE c.tenant_id = $tenant
  AND c.is_sampled_for_vector = true
  AND c.status = 'closed'              -- 只 embed 终态span,避免重复embed活span
  AND NOT EXISTS (SELECT 1 FROM span_vectors v WHERE v.span_id = c.span_id)
ORDER BY c.span_id                      -- span_id≈雪花,稳定游标
LIMIT $batch_size
FOR UPDATE SKIP LOCKED;                 -- ⚠️ openGauss行存ASTORE支持SKIP LOCKED;待验冷门组合
```

采样规则（应用层判定，写 `is_sampled_for_vector`）：

```python
def should_sample(span):
    kind = span.attrs.get("span_kind")
    if span.status == "error":          return True            # error 100%
    if span.parent_span_id is None:     return True            # root 100%
    if kind == "llm":                   return rand() < cfg.llm_sample_rate  # 默认0.3
    return False                                               # 其余不 embed
```

采样器主循环（幂等、水位持久化）：

```python
def embed_sampler_tick(tenant):
    with txn() as t:                                    # 单事务=水位与写入原子
        if not t.exec("SELECT pg_try_advisory_xact_lock(74011, hashtext($1))",
                      f"embed_sampler:{tenant}"): return # 别人在跑
        rows = t.query(PULL_SQL, tenant, cfg.batch_size)
        if not rows: return
        # 背压感知:队列积压时降级(见④)
        rows = backpressure_degrade(rows)               # 可能只留 root
        embeddings = embed_model.encode([r.text for r in rows])  # 外部GPU/服务
        # 幂等写入:ON CONFLICT DO NOTHING(span_id PK)
        t.copy_into("span_vectors", build_vec_rows(rows, embeddings),
                    on_conflict="(span_id) DO NOTHING")
        # 推进 index_lifecycle 行数估计(触发build/rebuild判定)
        t.exec("""UPDATE index_lifecycle
                  SET row_estimate = row_estimate + $cnt
                  WHERE tenant_id=$t AND index_scope=0""", len(rows), tenant)
        # 水位前进(可选,主要靠 NOT EXISTS 守卫,水位仅用于监控/恢复加速)
        t.exec("""UPDATE job_registry SET watermark = jsonb_set(
                    watermark,'{last_span_id}', to_jsonb($w)), last_ok_at=now()
                  WHERE job_name='embed_sampler' AND shard_key=$t""",
               rows[-1].span_id, tenant)
```

> 崩溃恢复点：写入 + 水位在同一事务。崩溃后 `NOT EXISTS(span_vectors)` 守卫保证重跑只补未写的，绝不重复 embed 已写行。`ON CONFLICT DO NOTHING` 是第二道幂等闸。

### 1.4 索引编排：build / rebuild 决策

```python
def index_lifecycle_tick(tenant):
    # ---- 活区 HNSW (scope=0) ----
    st = get_state(tenant, scope=0)
    if st.state == NO_INDEX and st.row_estimate >= cfg.hnsw_build_threshold:
        build_index(tenant, scope=0)        # 见下,空表→非空才建
    # HNSW 增量:新INSERT自动进图,无需重建;只在 row过大需迁冷区时处理

    # ---- 冷区 DiskANN (scope=1):按"seal段"批量 ----
    # 当 frozen 的新向量累计超过段阈值,build 一个新段 or 重建合并
    pending = count_cold_pending(tenant)    # frozen_at>last_build_at 的向量数
    if st1.state == NO_INDEX and pending >= cfg.diskann_build_threshold:
        build_index(tenant, scope=1)
    elif st1.state == ACTIVE and pending >= cfg.diskann_rebuild_threshold:
        rebuild_index_blue_green(tenant, scope=1)  # 见1.5

    # ---- BM25 (scope=2):见1.6 ----
    bm25_maybe_build(tenant)
```

**空表门槛守卫（关键，回应「不能空表建索引」）**：

```sql
-- 建索引前必须确认非空,且达阈值。用一条带行数门槛的守卫:
DO $$
DECLARE n bigint;
BEGIN
  SELECT count(*) INTO n FROM span_vectors
    WHERE tenant_id = :tenant AND frozen_at IS NULL;   -- 活区
  IF n < :threshold THEN
     RAISE NOTICE 'skip build: rows=% < threshold', n; -- 留 brute-force
  ELSE
     -- DiskANN inplace filter: 索引带 tenant_id, span_kind 过滤列
     EXECUTE format(
       'CREATE INDEX %I ON span_vectors USING diskann (embedding, tenant_id, span_kind)',
       :index_name);
  END IF;
END $$;
```

> ⚠️ openGauss 社区版无 `CREATE INDEX CONCURRENTLY` 的强保证（部分版本支持，部分阻塞写）。`span_vectors` 是行存可在线建，但 DiskANN 大段建议**用 blue-green（建新对象→切换→丢旧），不在主对象上原地重建**。

### 1.5 冷区按段重建（blue-green，在线无停机）

```python
def rebuild_index_blue_green(tenant, scope):
    new_name = f"idx_vec_t{tenant}_s{scope}_{epoch_ms()}"
    set_state(tenant, scope, BUILDING)
    # 1. 在(已非空的)数据上建新索引对象,旧索引继续服务查询
    sql_create_index(new_name, tenant, scope)          # 见1.4守卫
    # 2. 原子切换:查询层从 catalog 读"当前活跃索引名" → 改 index_lifecycle.index_name
    with txn() as t:
        old = t.query1("SELECT index_name FROM index_lifecycle WHERE ...")
        t.exec("UPDATE index_lifecycle SET index_name=$1, state=$2, "
               "built_rows=row_estimate, last_build_at=now() WHERE ...", new_name, ACTIVE)
    # 3. 切换生效后,旧索引无引用 → DROP(放到下个 tick,留缓冲避免在途查询)
    schedule_drop_index(old, after=cfg.drop_grace_sec)
```

> 不确定项：yiTrace 查询路由是否支持「按 index_lifecycle.index_name 动态选索引」。若优化器只认固定索引名，则改为「DROP 旧名→CREATE 同名」，接受短暂 brute-force 窗口（小客户可接受）。**标注待确认查询层契约。**

### 1.6 BM25 fulltext（在 `span_current` 行存，随折叠增长）

```
span_current 是单PK行存、持续 MERGE 写入 → BM25 索引必须支持在线增量维护。
- 首建门槛:折叠态行数 >= bm25_build_threshold(默认 5万) 才建,否则 LIKE/seq 兜底。
- 维护:行存上 BM25 随 INSERT/UPDATE 增量更新(无需重建)。
- 重建触发:仅当死元组膨胀(MERGE 大量 UPDATE 产生旧版本)使召回质量/体积退化时,
  夜间窗口 REINDEX(或 blue-green 重建)。用 pg_stat 估膨胀率,不靠定时硬重建。
```

```sql
-- 首建守卫(同样防"空/小表建索引浪费")
SELECT count(*) FROM span_current WHERE tenant_id=:t;  -- >= threshold 才执行下行
CREATE INDEX idx_bm25_t:tenant ON span_current USING bm25 (input_text, output_text);
-- 重建判定:估膨胀
SELECT n_dead_tup::float / NULLIF(n_live_tup,0) AS bloat
FROM pg_stat_user_tables WHERE relname='span_current';  -- > bm25_reindex_bloat 触发
```

**① 配置参数**

| 参数 | 默认 | 说明 |
|---|---|---|
| `vec.llm_sample_rate` | 0.30 | 非 root/error 的 LLM span 采样率 |
| `vec.hnsw_build_threshold` | 2000 | 活区首建 HNSW 的最小行数（>0 防空表） |
| `vec.diskann_build_threshold` | 50000 | 冷区首建 DiskANN 段阈值 |
| `vec.diskann_rebuild_threshold` | 200000 | 冷区累计新增触发重建 |
| `vec.drop_grace_sec` | 30 | blue-green 切换后旧索引 DROP 缓冲 |
| `bm25.build_threshold` | 50000 | BM25 首建行数门槛 |
| `bm25.reindex_bloat` | 0.4 | 死元组比触发 REINDEX |

---

## ② 差异化 TTL / 保留

### 2.1 保留规则表（声明式，优先级覆盖）

```sql
CREATE TABLE retention_policy (
  policy_id     serial PRIMARY KEY,
  tenant_id     bigint,              -- NULL=全租户默认
  match_kind    smallint NOT NULL,   -- 0 default / 1 has_error / 2 annotated / 3 in_dataset / 4 tenant_override
  ttl_days      int NOT NULL,        -- 普通30; error 180; annotated/dataset = -1(永久)
  priority      int NOT NULL DEFAULT 0, -- 大者覆盖小者
  enabled       bool NOT NULL DEFAULT true
);

-- 种子规则
INSERT INTO retention_policy(tenant_id,match_kind,ttl_days,priority) VALUES
 (NULL, 0, 30,  0),    -- 普通 trace 30天
 (NULL, 1, 180, 10),   -- 含 error 的 trace 180天
 (NULL, 2, -1,  20),   -- 被人工标注 永久
 (NULL, 3, -1,  20);   -- 进了数据集 永久
```

trace 级保留标记（避免 GC 时全表扫判定）。在 `frozen_registry` 旁挂一张轻量保留台账，折叠/标注/入集时维护：

```sql
CREATE TABLE trace_retention (
  tenant_id   bigint NOT NULL,
  trace_id    bigint NOT NULL,
  has_error   bool NOT NULL DEFAULT false,  -- 任一 span error → true
  annotated   bool NOT NULL DEFAULT false,  -- 人工标注事件(event_type=6)出现 → true
  in_dataset  bool NOT NULL DEFAULT false,  -- 被数据集引用
  start_time  timestamptz NOT NULL,         -- 决定落在哪个RANGE分区
  expire_at   timestamptz,                  -- 物化的到期时刻(规则求值结果)
  PRIMARY KEY (tenant_id, trace_id)
);
```

### 2.2 到期求值（物化 expire_at，规则变更或 trace 状态变更时刷新）

```sql
-- 取最高优先级命中规则的 ttl,物化 expire_at
UPDATE trace_retention r SET expire_at = CASE
   WHEN p.ttl_days < 0 THEN 'infinity'::timestamptz
   ELSE r.start_time + (p.ttl_days || ' days')::interval END
FROM LATERAL (
   SELECT ttl_days FROM retention_policy
   WHERE enabled AND (tenant_id = r.tenant_id OR tenant_id IS NULL)
     AND ( (match_kind=1 AND r.has_error)
        OR (match_kind=2 AND r.annotated)
        OR (match_kind=3 AND r.in_dataset)
        OR  match_kind=0 )
   ORDER BY priority DESC LIMIT 1
) p
WHERE r.tenant_id = $tenant;  -- 批量,可加 WHERE 限定脏行
```

### 2.3 GC 作业：分区粒度回收（热区 RANGE / 冷区按月 DROP）

**关键约束**：`span_events` 是 INTERVAL 分区，`sys_pN` 不能手动建只能 DROP；冷区 CStore 不能行删。因此 GC **优先整分区 DROP**，行级删除仅用于「分区内混有长留 trace」的少数情况。

```python
def gc_ttl_tick(tenant):
    with txn() as t:
        if not t.exec("SELECT pg_try_advisory_xact_lock(74020, hashtext($1))",
                      f"gc_ttl:{tenant}"): return

        # --- A. 冷区 CStore 按月分区:整分区全过期才能DROP(列存不可行删) ---
        for part in list_cold_partitions(t, tenant):       # 读 pg_partition
            # 该分区内是否还有"未到期/永久"的 trace?
            alive = t.query1("""
              SELECT EXISTS(
                SELECT 1 FROM trace_retention
                WHERE tenant_id=$t AND start_time >= $lo AND start_time < $hi
                  AND (expire_at IS NULL OR expire_at > now()))
            """, tenant, part.lo, part.hi)
            if not alive:
                t.exec(f"ALTER TABLE span_current_cold DROP PARTITION {part.name}")
                t.exec("DELETE FROM trace_retention WHERE tenant_id=$t "
                       "AND start_time>=$lo AND start_time<$hi", tenant, part.lo, part.hi)
                # 关联 payload refcount-1 (见③),DROP前必须先记账!见下"顺序"

        # --- B. 热区 span_events / span_current:分区到期DROP + 残留行删 ---
        for part in list_hot_partitions(t, tenant, table='span_events'):
            if part.hi <= now() - max_ttl():               # 整分区超过最长TTL → 直接DROP
                payload_refcount_dec_for_partition(t, part) # ③先记账
                t.exec(f"ALTER TABLE span_events DROP PARTITION {part.name} UPDATE GLOBAL INDEX")
            else:
                # 分区未整体过期,但内含已到期普通trace → 行级删(行存ASTORE可删)
                gc_expired_rows_in_partition(t, tenant, part)
```

**热区分区内行级回收（仅普通短留 trace，批量、限流）**：

```sql
-- 折叠态:删到期 trace 的 span_current 行(ASTORE 行存可 DELETE)
WITH victim AS (
  SELECT tenant_id, trace_id FROM trace_retention
  WHERE tenant_id=$t AND expire_at IS NOT NULL AND expire_at <= now()
  LIMIT $batch
)
DELETE FROM span_current c USING victim v
WHERE c.tenant_id=v.tenant_id
  AND c.attrs->>'trace_id' = v.trace_id::text;  -- ⚠️ 若有 trace_id 列直接用列
-- span_events 同理按 trace_id 删(分区裁剪 + trace_id)
```

> ❗**DROP PARTITION 顺序铁律**：先 `payload refcount-1`（③），再 DROP 分区。反序会丢失引用记账导致 payload 泄漏。两步必须在同一逻辑作业内、记账成功后才 DROP；若 DROP 后崩溃，refcount 已减为准（幂等，见③用 batch_id 去重）。

### 2.4 冷区月分区生命周期

```
span_current_cold: PARTITION BY RANGE(start_time) INTERVAL 1 month
- 写入:折叠环把低基数真列追加进当月分区(INSERT,自动建 sys_pN)
- 回收:超过 max_ttl 的整月分区,且月内无长留trace → DROP PARTITION
- 长留trace处理:若某月分区内有 annotated/dataset trace → 不DROP整分区;
  二选一:(a)接受冷分区长留(空间换简单,推荐小客户);
         (b)EXCHANGE PARTITION 出来→过滤重写→换回(复杂,v1不做)
```

**② 配置参数**

| 参数 | 默认 | 说明 |
|---|---|---|
| `ttl.default_days` | 30 | 普通 trace |
| `ttl.error_days` | 180 | 含 error |
| `ttl.annotated/dataset` | -1 | 永久 |
| `gc.batch_size` | 1000 | 行级删批大小 |
| `gc.cold_keep_if_alive` | true | 冷分区有长留 trace 则保留整分区 |

---

## ③ payload CAS GC（refcount 正确性）

`payload_store` 是 CAS：`(tenant_id, sha256)` 主键 + `refcount`。多 trace 共享同一 payload（同样的 prompt/输出去重）。删 trace/分区 → 相关 ref 减 1；异步清 refcount=0。

### 3.1 引用边台账（谁引用了哪个 payload）

`span_events.payload_ref` 指向 sha256。但分区被 DROP 后无法反查，所以记账必须**在 DROP 前**完成，且要幂等。引入「待减记账」中间表，把「减引用」与「物理删行/分区」解耦：

```sql
CREATE TABLE payload_ref_decrement (        -- 待处理的减引用任务(幂等队列)
  dec_id      bigserial PRIMARY KEY,
  batch_id    text NOT NULL,                -- 同一GC批的幂等键(分区名/tracebatch)
  tenant_id   bigint NOT NULL,
  sha256      bytea  NOT NULL,
  delta       int    NOT NULL,              -- 通常为该sha在被删集合里的引用条数(负向应用)
  applied     bool   NOT NULL DEFAULT false,
  UNIQUE (batch_id, tenant_id, sha256)      -- 幂等:同批同sha只记一次
);
```

### 3.2 记账：DROP 分区前先汇总该分区的 payload 引用数

```sql
-- 在 DROP partition 前,把该分区内每个 sha 的引用条数汇总进 decrement 队列(幂等)
INSERT INTO payload_ref_decrement(batch_id, tenant_id, sha256, delta)
SELECT $batch_id, e.tenant_id, e.payload_ref, count(*)
FROM span_events PARTITION ($part_name) e        -- 分区裁剪精确读
WHERE e.payload_ref IS NOT NULL
GROUP BY e.tenant_id, e.payload_ref
ON CONFLICT (batch_id, tenant_id, sha256) DO NOTHING;  -- 重跑不重复记账
```

### 3.3 应用减引用（原子 UPDATE，避免读-改-写竞态）

并发正确性两层防护：(a) 每个 sha 的减操作用**原子 `UPDATE ... SET refcount = refcount - delta`**（DB 行锁天然串行化，无需读出再写回）；(b) 跨作业用 advisory lock 防同一 batch 重复应用。

```python
def payload_gc_apply(tenant):
    with txn() as t:
        if not t.exec("SELECT pg_try_advisory_xact_lock(74030, hashtext($1))",
                      f"payload_gc:{tenant}"): return
        rows = t.query("""SELECT dec_id, sha256, delta FROM payload_ref_decrement
                          WHERE tenant_id=$t AND applied=false
                          ORDER BY dec_id LIMIT $batch FOR UPDATE SKIP LOCKED""",
                       tenant, cfg.batch)
        for r in rows:
            # 原子减:并发安全,不读refcount到应用层
            t.exec("""UPDATE payload_store SET refcount = refcount - $d
                      WHERE tenant_id=$t AND sha256=$s""", r.delta, tenant, r.sha256)
            t.exec("UPDATE payload_ref_decrement SET applied=true WHERE dec_id=$1", r.dec_id)
        # 注意:applied 翻转与 refcount 更新在同一事务 → 崩溃要么全做要么全不做(幂等)
```

> ❗**关键正确性论证**：减引用与「标记 applied」在**同一事务**。崩溃回滚则 `applied=false`，下次重跑；因为是「按 dec_id 逐条 + applied 守卫」，已应用的不会再减。`UNIQUE(batch_id,sha256)` 保证同一删除批不会被记两次。整体满足**恰好一次语义**。

### 3.4 物理清理 refcount=0（与 increment 竞态处理）

危险点：清理判定 `refcount=0` 后、DELETE 前，可能有新 trace **复用**该 payload（CAS 命中老 sha → refcount+1）。用 advisory lock 把「写入侧 CAS 命中 +1」与「GC 侧删 0」串行化到同一 sha 的临界区。

```python
def payload_gc_sweep(tenant):
    cands = query("""SELECT sha256 FROM payload_store
                     WHERE tenant_id=$t AND refcount<=0 LIMIT $batch""", tenant, cfg.batch)
    for sha in cands:
        with txn() as t:
            # 对该 sha 上事务级 advisory 锁;写入侧 CAS+1 也对同 sha 加同锁
            t.exec("SELECT pg_advisory_xact_lock(74031, hashtext($1))", sha.hex())
            # 锁内复核:可能这期间被复用了
            still0 = t.query1("""DELETE FROM payload_store
                                 WHERE tenant_id=$t AND sha256=$s AND refcount<=0
                                 RETURNING toast_oid/blob_ref""", tenant, sha)
            if still0:
                free_external_blob(still0)   # TOAST 随行删;外置对象单独删
```

写入侧（摄入路径，非本作业但必须配套）：

```sql
-- CAS 命中老 payload 时的 +1,必须对同 sha 加同一 advisory 锁,否则与 sweep 竞态
-- SELECT pg_advisory_xact_lock(74031, hashtext(:sha_hex)); 然后:
INSERT INTO payload_store(tenant_id, sha256, refcount, ...) VALUES ($t,$s,1,...)
ON CONFLICT (tenant_id, sha256) DO UPDATE SET refcount = payload_store.refcount + 1;
```

> ⚠️ 不确定项：若摄入侧热路径不愿对每次 payload 写都抢 advisory lock（性能），替代方案是 sweep 侧用**宽限期**：只删 `refcount<=0 AND last_ref_change < now() - grace`（payload_store 加 `last_ref_change` 列），用时间窗规避竞态。**两方案二选一，标注权衡。**

**③ 配置参数**

| 参数 | 默认 | 说明 |
|---|---|---|
| `payload.gc_batch` | 500 | 单批清理数 |
| `payload.sweep_grace_sec` | 300 | 宽限期方案的时间窗 |
| `payload.lock_strategy` | `advisory` | `advisory` 或 `grace` |

---

## ④ 写背压

三级背压：**摄入限速（令牌桶）→ embedding 队列降级 → 折叠环跟不上的背压信号**。

### 4.1 per-tenant 令牌桶（应用层，内存态 + 周期落盘）

```python
class TokenBucket:                       # 每 tenant 一个,内存
    def __init__(self, rate, burst): self.rate, self.cap, self.tokens, self.ts = rate, burst, burst, now()
    def allow(self, n=1):
        self.tokens = min(self.cap, self.tokens + (now()-self.ts)*self.rate); self.ts = now()
        if self.tokens >= n: self.tokens -= n; return True
        return False

def ingest(tenant, batch):
    if not buckets[tenant].allow(len(batch)):
        raise Backpressure(429, retry_after=estimate_wait(tenant))  # 429 给客户端,客户端退避重试
    write_span_events(batch)             # 通过则正常 INSERT
```

> 令牌桶是软状态，崩溃后从满桶重启（保守允许短时突发，可接受）。限速维度：`tenant.rate_per_sec`、`tenant.burst`。

### 4.2 embedding 队列积压 → 三档降级

降级信号来自待 embed 积压量（`is_sampled_for_vector=true AND NOT EXISTS span_vectors` 的近似计数，定期采样存指标表）：

```python
def backpressure_degrade(rows):
    lag = embed_lag_gauge.value          # 待embed积压估计
    if   lag < cfg.embed_lag_warn:  return rows                    # L0 正常:全采样
    elif lag < cfg.embed_lag_high:  cfg.runtime_sample_rate *= 0.5 # L1 调低LLM采样率
                                    return rows
    else:                                                          # L2 严重:只 embed root/error
        return [r for r in rows if r.kind=='root' or r.status=='error']
        # L3(可加):完全暂停 embed,只攒标记,事后补 embed
```

```sql
-- 积压量度量(周期写指标,不在热路径精确count)
SELECT count(*) AS embed_backlog
FROM span_current
WHERE is_sampled_for_vector AND status='closed'
  AND NOT EXISTS (SELECT 1 FROM span_vectors v WHERE v.span_id=span_current.span_id)
  AND tenant_id=$t;
```

### 4.3 折叠环跟不上摄入 → 背压信号

折叠环消费 `span_events` 折叠成 `span_current`。背压信号 = **折叠 lag**（最新 ingest 的 event 与折叠水位的差）。lag 高时反压到令牌桶（降摄入速率）形成闭环。

```sql
-- 折叠 lag:摄入最高event_id - 折叠已处理水位
SELECT (SELECT max(event_id) FROM span_events WHERE tenant_id=$t)
     - (watermark->>'fold_event_id')::bigint AS fold_lag
FROM job_registry WHERE job_name='fold_loop' AND shard_key=$t;
```

```python
def fold_backpressure_signal(tenant):
    lag = read_fold_lag(tenant)
    if lag > cfg.fold_lag_critical:
        buckets[tenant].rate = base_rate * 0.3   # 折叠环濒临崩 → 砍摄入到30%
        alert("fold_loop falling behind", tenant, lag)
    elif lag > cfg.fold_lag_warn:
        buckets[tenant].rate = base_rate * 0.7
    else:
        buckets[tenant].rate = base_rate         # 恢复
    # 同时:折叠期 SET query_dop=1 保序不可改 → 只能靠减摄入,不能加并发提速
```

> ❗折叠环本身有 `query_dop=1`（保序）的硬约束 → **不能靠加并行追赶 lag**，只能反压摄入端。这是设计上必须接受的：写入速率上限由单线程折叠吞吐决定。背压信号闭环把这个物理上限传导回客户端 429。

**④ 配置参数**

| 参数 | 默认 | 说明 |
|---|---|---|
| `tenant.rate_per_sec` | 2000 | 每租户 span/s 基线 |
| `tenant.burst` | 10000 | 突发容量 |
| `embed.lag_warn` / `lag_high` | 5000 / 50000 | embedding 降级阈值 |
| `embed.min_sample_rate` | 0.05 | 降级下限，不归零（除非 L3） |
| `fold.lag_warn` / `lag_critical` | 100k / 1M | 折叠 lag 反压阈值 |

---

## 待确认 / 不确定项汇总（诚实标注）

1. **DiskANN/HybridANN「空表不能建」「inplace filter」** — yiTrace 私有扩展，无公开文档，按前提当硬约束。blue-green 重建是否被查询层支持（动态索引名路由）**需确认查询层契约**；不支持则退化为「DROP→CREATE 同名 + 短暂 brute-force 窗口」。
2. **CStore 列存不可行删** — 据此冷区只能整分区 DROP，月内有长留 trace 则整分区保留（`gc.cold_keep_if_alive`）。EXCHANGE-重写方案 v1 不做。
3. **`FOR UPDATE SKIP LOCKED` + ASTORE 行存** — PostgreSQL 语义支持，openGauss 该组合在采样器/payload 队列上**建议小流量验证**。
4. **`CREATE INDEX CONCURRENTLY`** — openGauss 版本差异大，本设计不依赖它，统一走 blue-green。
5. **payload +1/sweep 竞态** — 给了 advisory-lock 与 grace-window 两方案，按摄入热路径性能取舍，默认 advisory。
6. **INTERVAL 分区不能手动 ADD** ✅已验证 — 所以热区分区由 INSERT 自动创建，GC 只 DROP/TRUNCATE 已知 `sys_pN`；需周期从 `pg_partition` 枚举分区边界（`list_hot_partitions`）。
7. **令牌桶软状态** — 崩溃从满桶重启，短时允许突发，可接受；若要严格需 Redis/落盘，v1 不做。

来源：
- [openGauss Advisory Lock Functions](https://docs.opengauss.org/en/docs/2.0.0/docs/Developerguide/advisory-lock-functions.html)
- [openGauss Alter Table Partition (DROP/TRUNCATE/EXCHANGE)](https://docs.opengauss.org/en/docs/5.1.0/docs/SQLReference/alter-table-partition.html)
- [openGauss Create Table Partition (INTERVAL 自动建分区)](https://docs.opengauss.org/en/docs/5.0.0/docs/SQLReference/create-table-partition.html)
- [openGauss Merge Into](https://docs.opengauss.org/en/docs/5.1.0/docs/SQLReference/merge-into.html)
- [openGauss Transaction Isolation](https://docs.opengauss.org/en/docs/5.1.0/docs/DatabaseOMGuide/transaction-isolation.html)