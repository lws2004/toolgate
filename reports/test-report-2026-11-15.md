# toolgate 测试报告（2026-11-15）

范围：R9「列表按请求自然语言 query 排序」+ R10「治理时间预算优化（长文采样触发线下调、小列表无 query 跳过 rerank）」+ R11「classify 体量自洽护栏（缺陷 E 修复）」+ R12「分段 rerank（默认关）/ 长文先去重再采样（含可选 embedding 选行）/ L0 机械字节 clamp」的验证与真实重测。
代码基线：未提交工作区，基于 a45c498。daemon 由 launchd（com.toolgate.daemon）托管，本轮重启后 /health 正常（`{"ok":true,"service":"toolgate-daemon"}`）。

## 一、自动化测试

| 命令 | 结果 |
|---|---|
| `cargo test --workspace` | 556 passed / 0 failed |
| `cargo test --workspace --features external-protocols` | 549 passed / 0 failed |
| `cargo clippy --workspace --all-targets` | 0 warning |
| `cargo build --release --features external-protocols` | 0 warning |
| `./scripts/check-doc.sh` | 通过（7086 行） |
| 适配器 `node --test test/*.test.mjs` | 35 passed / 0 failed |

### 新增 / 调整单测

R9（core govern.rs）：
- `listing_passes_request_query_to_rerank`：queryApplied=applied 且槽位收到的 query 等于请求值；
- `listing_rejects_non_natural_query_and_uses_default`：非自然语言退回 DEFAULT_RERANK_QUERY，queryApplied=rejected_non_natural，content 含 [DEGRADED] 与 reason=non_natural_query；
- `listing_delegation_keeps_query_as_data_field`：委派 input 的 relevance_query 等于 query，指令句不含 query 原文且声明其为数据（§15.1）；
- `listing_fallback_omits_query_applied_and_degraded`：兜底路径无 queryApplied 键、无 [DEGRADED]（§4.4b 第 1 条）。

R10（core govern.rs）：
- `listing_small_without_query_skips_rerank`：小列表（未超 max_lines、≤32 KiB）无 query → 不调 rerank（槽位 seen 为空）、无 queryApplied、truncated=false、原文返回（替换原 `listing_without_query_reports_not_provided`）；
- `listing_over_32kib_without_query_still_reranks`：120 行长行 39 KiB 无 query → 仍调 rerank（seen=DEFAULT_RERANK_QUERY），确保跳过条件不误伤大列表；
- `long_text_under_trigger_keeps_full_input`：改用含 ANSI 的短文本（≤32 KiB 但判 long_text）验证「未超触发线不采样」；`long_text_over_trigger_samples_before_summary` 保持 >32 KiB 采样。

## 二、R10 真实 daemon 重测（reports/bench-r10-2026-11-15.txt）

| 场景 | 输入字节 | shape | sampled | 关键指标 | 耗时 | 说明 |
|---|---|---|---|---|---|---|
| 长文 600 行 / summary | 63046 | long_text | true | 385B / 2 行 | 2507ms | R10 前 40385ms（17×提速），摘要有损但头尾+关键行保留 |
| 长文 1000 行 / summary | 105376 | long_text | true | 394B / 2 行 | 2233ms | R10 前 42070ms（19×提速） |
| 列表 60 行 / 无 query | 3916 | listing | — | items=60，无 queryApplied | 1ms | R10 跳过 rerank，原样返回（R10 前 448ms） |
| 列表 120 行 / 带 query | 7920 | listing | — | queryApplied=applied，rerank=local | 1198ms | 3 条 migration 行排最前 |
| 列表 3000 行 / 无 query | 202002 | listing | — | truncated=true，兜底 | 3ms | >max_lines 且 >32KiB，仍 SLOT_UNAVAILABLE 兜底 |
| 列表 120 行 39KB 长行 / 无 query | 39383 | listing | — | queryApplied=not_provided，rerank=local | 4952ms | 超 32KiB 仍 rerank（已知边界，见 §五） |
| 短 60 行 info / debug | 769 | long_text | false | 45B（摘要为空） | 924ms | **缺陷 E**（见 §五），非本次引入 |

对照：同一 60 行 info 文本以 `intent=raw` 请求 → pass_through，769B 原样返回，1ms。

## 二·R12 真实 daemon 重测（2026-11-15，daemon external-protocols，重启后 /health ok）

| 场景 | 输入字节 | shape | 关键指标 | 耗时 | 说明 |
|---|---|---|---|---|---|
| 600 行日志 / summary | 63046 | long_text | 352B / 2 行；dedup 312→3 行 | 2626ms | 先去重再采样 + embedding 聚类，送模型仅 3 行 |
| 4000 行唯一 / summary | 157300 | long_text | 428B / 2 行；dedup 4000→853 | 946ms（热态） | 精确行去重后 sampled=true；daemon 重建后首个大请求冷启 19.4s |
| 2100 行重复 / summary | 62999 | long_text | 93B；先压成 1 行，sampled=false | 752ms | 「先去重再采样」：去重后体量远低于 32KiB，不再采样 |
| 3000 行列表 / 无 query | 123779 | listing | 32969B / 2000 行；truncated（字节 clamp） | 2ms | R12 L0/机械字节上限：改前 71927B → 现在 32KiB 头截断 |
| 1500 行列表 / rerankChunk 开 + query | 96440 | listing | 32846B；chunked=true；applied | 13941ms | 分段重排 opt-in（默认关），1500 docs 分 2 批 |
| 60 行列表 / 无 query | 2259 | listing | 原样 2259B，无 queryApplied | 2ms | 小列表跳过 rerank（回归） |
| 60 行列表 / 带 query | 2259 | listing | applied，rerank=local | 364ms | 对照 |

R12 三项：①分段 rerank（`policy.rerankChunk`，默认关，超预算分批、`maxTotalMs` 限总时延）；②长文先去重再采样 + 关键行额度回收 + 可选 `policy.sampleEmbedding`（默认关）按密度选行；③L0/机械路径正文字节 clamp（32KiB 头截断）。新增单测 4 个（truncate `sample_ends_reclaims_unused_key_budget`；govern `long_text_dedups_before_sampling` / `long_text_sample_embedding_selects_representatives` / `listing_chunk_rerank_opt_in`，并改 `listing_over_max_lines_clamped_to_bytes`）。



## 二·P1/P2 真实重测（2026-11-15，reports/bench-p13-2026-11-15.py，daemon external-protocols）

P1（rerank 预算按字节，ADR 0009 P1）：估算式 `100 + 15*lines + 0.005*bytes`。

| 场景 | 输入字节 | 结果 | 结论 |
|---|---|---|---|
| 39KiB/120 行列表，无 query | 39379 | 1ms、truncated、`SLOT_UNAVAILABLE`、字节 clamp、无 RANKED | 改前 4952ms 重排 → 现走预算兜底 |
| 120 行短列表，带 query「数据库迁移失败的原因」 | ~5.3K | 955ms / applied / rerank=local | 正常列表仍重排，未误伤 |
| 600 行唯一长文 summary（回归） | ~35K | 3223ms / long_text / TRUNCATED | 未见回归 |

P2（daemon 冷启预热，ADR 0009 P2）：重启后立即发首个 157KiB（170889B）唯一长文 summarize 请求。

| 预热方案 | 首个请求 COLD | 热态 WARM | 结论 |
|---|---|---|---|
| 大样本（1500 行 ≈43KiB、max_tokens 256） | 38647ms | — | 有害：占用串行推理线程、与首请求竞争 |
| 轻量（8 token，原方案） | 1498ms（模型服务热态） | 963ms | 保留 |

结论：P2 否决大样本预热（保留轻量）。冷启主因在外部模型服务首次加载，daemon 侧样本大小无助、大样本反向有害（§18.1 第 4 条实例内串行）。

## 三、R9 真实 daemon 重测（reports/bench-rerank-query-fallback-2026-11-14.txt）

| 场景 | shape | queryApplied | 槽位 | 耗时 | 观察 |
|---|---|---|---|---|---|
| 120 行 + query=数据库迁移失败的原因 | listing | applied | rerank=local | 1694ms | 3 条 migration 行排最前 |
| 120 行 + query=^fn .*handler | listing | rejected_non_natural | rerank=local | 1574ms | notices 含 DEGRADED(actual=generic_intent reason=non_natural_query requested=none slot=rerank)，content 末行同步 |
| 1200 行（>1000 候选，超预算）+ 自然语言 query | listing | 缺失（兜底不重排） | rerank 未调用 | 3ms | 只发 SLOT_UNAVAILABLE(fallback=mechanical)，无 DEGRADED |

## 四、独立复审（子代理 r10-reviewer）

复审范围：R10 代码与文档（子代理 r10-reviewer 只读，未改文件）。结论：**P0 = 0**；P1 = 2 条（均为文档与实现的一致性措辞，代码逻辑与七条红线无误）；P2 = 2 条。按 AGENTS.md §3.6「P0/P1 全部落地后才进入交付」，两条 P1 修复后复跑 check-doc 通过即闭环。

- **P1-1（已修，`docs/architecture/04-tools.md` §4.0a 列表 bullet）**：原句「语义排序只在搜索 / 多文档候选来源开启」「不满足条件仍调模型属于为了治理而治理」与 `process_listing` 实现矛盾——预算内且不满足跳过条件即一律 rerank，不判来源；>32 KiB 无 query 大列表仍用通用意图串排序。已改写为「其余在预算内（≤1000 候选且估算 ≤2s）即调 rerank，不区分来源」，并补已知边界说明。
- **P1-2（已修，`docs/architecture/18-deployment.md` §18.7）**：原句把 `not_provided` 收窄成「未排序且无需排序，不带 `queryApplied`」，与同节「`queryApplied` 仅 rerank 实际执行时输出」冲突（大列表无 query 时 rerank 实际执行并带 `queryApplied=not_provided`，实测第 6 条 4952ms 佐证）。已改为「此跳过情形下 `not_provided` 表示未排序且无需排序、不带 `queryApplied`；超出该范围的大列表无 query 仍按通用意图串排序，此时 `queryApplied` 记 `not_provided`」。
- **P2-1（已处理）**：`docs/implementation-status.md` R7 历史行残留已删常量 `LONG_TEXT_SAMPLE_TRIGGER_BYTES=64KiB` / `SUMMARY_INTENT_MAX_BYTES=120KiB`，已在 R7 行加「已由 R10 调整」指针（代码层已零引用）。
- **P2-2（已处理）**：把「>32 KiB 无 query 大列表仍 rerank、按行数估算未计行长可到约 5s」的已知边界补入 §4.0a 正文。

验证：复审实跑 5 个相关单测（`listing_small_without_query_skips_rerank` / `listing_over_32kib_without_query_still_reranks` / `listing_over_candidate_limit_skips_rerank` / `long_text_over_trigger_samples_before_summary` / `long_text_under_trigger_keeps_full_input`）→ 5 passed；R10 重测 6 条数字与 `reports/bench-r10-2026-11-15.txt` 逐一对齐；P1 修复后 `./scripts/check-doc.sh` 通过（7064 行）。

## 五、未覆盖 / 已知限制

- **缺陷 E（R11 已修复）**：第三级 classify 曾把 60 行普通 info 日志（intent=debug/summary）判为 long_text 并摘要为空（45B）。R11 加 `classify_plausible` 体量自洽护栏（long_text 要求 >50KiB / 2000 行或含 ANSI，listing 要求 ≥20 非空行），该样本现 pass_through 原样返回（5709B / 468ms），回归 600 行 summary 仍 long_text sampled=true；见 implementation-status R11 与 docs/decisions/0008。
- **rerank 预算按行数估算的盲点（已修复，ADR 0009 P1）**：`rerank_estimated_ms` 原为 `100 + 15*lines`，只看条数未计行长；120 行 × 约 330B（39KB）无 query 曾走 rerank 实测 4952ms、超 2s。现加 `0.005ms/字节` 项，39KiB/120 行估算越 2s → 走确定性兜底（实测 1ms），120 行短带 query 仍 applied（955ms）。
- **>1000 行列表不排序**：分段 rerank 成本约 35–40s（1000 docs ≈ 11–13.6s × 3）远高于 2s 预算，明确不做，维持毫秒级兜底。
- 三份 HTML 评估报告已按 R10 数据重跑为 2026-11-15 版（final-assessment / slot-comparison / govern-comparison），旧 2026-11-14 版保留作历史。
- `meta.slmStatus` 的 ready 只反映「绑定 + 构建能力」，不含服务存活探测。
- **R12 收尾清理（本次）**：合并 `LISTING_SKIP_RERANK_MAX_BYTES` / `LISTING_BODY_MAX_BYTES` 为单一 `LISTING_BODY_MAX_BYTES=32KiB`（同一列表正文上限两用，避免边界漂移）；补 `clamp_head_bytes` 首行超限返 `Some(0)` 与 `listing_chunk_rerank_respects_max_total_ms`（`maxTotalMs` 超限不启用、走兜底）两个单测；§4.0a 明确「32KiB 跳过只覆盖 ≤max_lines 且无 query 的小列表，少行超字节的 39KiB 列表仍 rerank」；`sample_ends` 补回收顺序注释、`natural_query` META 表注释指向 §18.7。**保留不改**：daemon 冷启首请求 19.4s（预热策略，已评估 ADR 0009 P2：大样本预热有害、保留轻量）、rerank 估算按条数未计行长（已修复 ADR 0009 P1）、`dedup_model_input` 的 `#[cfg(test)]` wrapper（仅测试便利，无生产影响）。
