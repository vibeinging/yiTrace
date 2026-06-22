# Apache Doris 原生支持 Lance —— 对 yiTrace / yiTrace 的可参考点

> 来源：社区文章《Apache Doris 已实现对 Lance 格式的原生支持》(译自 rayner.notes，作者 Mingyu Chen / Doris PR #62182)。
> 本文不复述原文，只抽取**对我们项目有具体落点**的部分，逐条标注"用在哪"。

## 一句话判断
文章本身讲的是"分布式 MPP 引擎 Doris 把 Rust 写的 Lance reader 嵌进 C++ BE 进程读外部 Lance 数据湖"——**和我们 build-on-yiTrace 单机 trace 存储不是同一件事**。但它有 **3 类真能参考 + 1 个战略点 + 1 条竞品情报**。

---

## ① 最直接的印证：我们的核心架构方向是对的（强化信心，零成本）
Doris 独立走到了和 yiTrace **同构**的组合：
> C++ OLAP 引擎 + **原生 HNSW ANN** + **内置倒排 BM25/短语/模糊** + **VARIANT(动态 JSON)** → **一条 SQL 完成"结构化过滤 + BM25 文本 + ANN 向量召回 + 聚合/关联"的混合检索**。

文章明说：**"纯粹的向量数据库(包括 LanceDB)通常不具备完整 OLAP 能力，无法用一条 SQL 表达完整管道。"**
→ 这正是我们"**建在完整 SQL 的 openGauss/yiTrace 内核上，而不是建在纯向量库/纯 Lance 上**"决策的**独立外部佐证**。yiTrace 已经有这套(DiskANN/HNSW + BM25/jieba + JSONB)，yiTrace 的招牌"一条 SQL 做 标量过滤+中文 BM25+语义召回"就是这个范式。**别人(Doris)也认这条路 = 我们方向对。**

## ② 最有工程参考价值：Rust-in-C++ 内核嵌入蓝图（直接套 openGauss）
这是文章最硬的干货，**直接接上一轮"是否动内核"的讨论**。Doris BE 是 C++、openGauss 内核也是 C/C++——**同一套模式可平移**：

| Doris 的做法 | 对我们的用途 |
|---|---|
| **Arrow C Data Interface** 跨 FFI 零拷贝传 RecordBatch(两侧不手写编解码) | 若 yiTrace 要在内核里跑 Rust 组件，用 Arrow C Data Interface 做边界，避免手写 FFI struct |
| **单线程 Tokio + `block_on`**：把异步关在内部，对外是同步调用，并发上移到**片段级 scan-range**(引擎侧而非 runtime 侧) | 关键设计纪律：别让 Tokio 在内核里乱起 OS 线程绕过调度器；并发在 yiTrace 的 scan 层做 |
| **Corrosion** 把 Cargo 接进 CMake(一个 `rust.cmake`) | openGauss 也是 CMake/Makefile，可同法把 Rust crate 接进内核构建 |
| 编译开关 `BUILD_RUST_READERS=OFF` 默认关 + 运行时会话变量 | 渐进合入、不影响存量用户的发布纪律 |
| 二进制代价：Rust 静态库 ~430MB，LTO 后 BE +50–80MB，**一次性基建成本被后续 Rust 组件摊薄** | 单机私有化产品 +50–80MB 可接受；一次脚手架投入解锁整个 Rust 生态 |

**落点**：我们 **v1 不需要这个**(v1 纯 SQL+应用层，已确认不动内核)。但它对两条路有用：
- **v2 的 kernel 路径**：若 PoC 证明应用层 MERGE 折叠不够快、真要做 kernel 级 merge-on-read 算子，可以**用 Rust 写、按这套嵌进 openGauss 内核**，不必裸写 C，且复用 Rust 数据生态。
- **给 yiTrace 加外部格式 reader**：读 Parquet/Lance/Iceberg 等(见 ③)。

## ③ 一个可选功能：Lance 当"摄入源/导出靶"，做 AI 管道互操作（不是替换我们的存储）
文章的本质动机：**AI/ML 管道(Ray、PyTorch、LanceDB、embedding 管道)大量产出 Lance 文件**，原地读避免"二次拷贝 + 同步管道 + 丢 Lance 版本管理"。映射到 yiTrace：
- **读**：客户的 AI 管道若用 Lance 存 trace/embedding/eval 数据集，yiTrace 原生读 Lance(像 Doris 那样)可免重导——一个**可选 interop 特性**(走 TVF/外表，不是主存储)。
- **写/导**：我们的飞轮 `export_trajectory`(导训练集)可以**导成 Lance 格式** → 直接接客户的 Ray/PyTorch/LanceDB 训练栈，比导 JSONL 更"AI 原生"。
> ⚠️ 边界：**我们的 trace 主存储仍是 openGauss UStore/CStore + 事件表折叠**(已定)，**不是**把 Lance 当存储引擎。Lance 在这里只是"和外部 AI 生态互操作的开放格式"，对应 Doris 的"湖仓读"，不对应我们的"主存储"。

## ④ 战略点：一次 Rust-FFI 脚手架 = 解锁整个 Rust 数据生态
文章的"次级效应"：**一旦 C++ 进程里能跑一个 Rust 组件，再加同类组件成本骤降**(共享 Tokio/Arrow/FFI 框架)。候选：delta-rs、iceberg-rust、**OpenDAL**(统一对象存储抽象)、选定的 DataFusion 算子。
→ 对 yiTrace 是一个**可选的未来能力**：若哪天要快速吸收 Rust 数据生态(湖格式、对象存储、向量/查询组件)，先投一次 Arrow-C-Data-Interface + Corrosion 脚手架，之后边际成本很低。是 yiTrace 平台级的战略选项，非 yiTrace 必需。

## ⑤ 竞品情报：Doris 正进"多模态 AI-OLAP"，和 yiTrace 正面同向
Doris(Apache，源自百度，SelectDB 商业化，**中国系**)在 5.0 起把 ANN+BM25+VARIANT+Lance 拼成"AI 数据湖仓"。**和 yiTrace 是正面同向的竞品/同行**。
- **印证方向**：又一个强玩家认"OLAP+原生向量+原生全文"是 AI 混合检索的底座。
- **差异化坐标**：Doris = **分布式 MPP 重栈**(湖仓、海量、多 BE)；我们 = **单机 + trace 专用 + 私有化 + 信创 + 中文**。别在"通用多模态 OLAP"上和 Doris 硬碰，守住"Agent trace 专用 + 单机私有化 + 信创"的窄而深定位。

---

## 不是我们的路（避免误读）
- **Lance 当主存储**：不是。我们建在 openGauss 内核上(已定，且更利信创继承)。
- **分布式**：不是。单机。
- **信创口径**：Doris 嵌美国系 Lance/Tokio/Arrow 对 Apache 项目无所谓；我们若嵌，须按上一轮校正的 **"开源可控 + 源码内置 vendoring + 跑国产栈 + 数据不出境"** 框定，**别因此喊"纯自研/完全自主可控"**(会双标被打脸)——但"开源底座 + 可审计"是站得住的。

## 建议
1. **③ 的"飞轮导出 Lance"** 列为平台层飞轮模块的一个**低成本互操作选项**(export_trajectory 多一个 Lance target)。
2. **② 的 Rust-FFI 蓝图** 收藏为 **v2 kernel 路径 / yiTrace 外部格式 reader 的现成参考**(若触发动内核或加湖格式读，照搬 Arrow-C-Data-Interface + 单线程 Tokio + Corrosion)。
3. 其余(①④⑤)作信心/战略/竞品输入，不产生即时工作项。
