//! 告知协议（§4.4b）：一个事实源（Notice），两个通道（content 文本 / structuredContent JSON）。
//!
//! 本模块只负责「告知」的表示、渲染、排序与预算合并，是纯函数层：
//! 同输入同输出（§16.3），不读配置、不读模型、无副作用。
//! 生成分两段（decide_output / finalize_notices，§4.5）与各工具接入属后续阶段。
//!
//! 架构依据：04-tools.md §4.4b（枚举、模板、渲染顺序、预算、敏感信息）、§15.7 测试策略。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// ---------- 值枚举（§4.4b Rust 草案，共 13 个） ----------

fn value_str<T: Serialize>(v: T) -> String {
    serde_json::to_value(v)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TruncUnit {
    Lines,
    Bytes,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum OutputShape {
    Raw,
    Summary,
    Structured,
    Visual,
    None,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DegradedActual {
    Raw,
    Summary,
    Structured,
    Visual,
    None,
    Metadata,
    Compressed,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    None,
    Understand,
    Summary,
    Raw,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum FallbackAction {
    Redact,
    Structure,
    Summarize,
    Refuse,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SlotFallback {
    Metadata,
    Text,
    Mechanical,
    Loading,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    Native,
    Json,
    Text,
    Ordered,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SniffActual {
    Binary,
    Text,
    Png,
    Jpeg,
    Gif,
    Webp,
    Pdf,
    Zip,
    Gzip,
    Other,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceViews {
    Both,
    Local,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum NormalizeKind {
    Eol,
    Whitespace,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum NormalizeLevel {
    #[serde(rename = "2")]
    L2,
    #[serde(rename = "3")]
    L3,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DegradeReason {
    CapabilityMissing,
    UpstreamNoVision,
    LatencyOverBudget,
    LossyCompression,
    CompressionExceeded,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum IsolationReason {
    RetentionBelowThreshold,
    NoRedactablePattern,
    RedactOptimal,
}

// ---------- Notice（§4.4b，10 个 code） ----------

/// 序列化为 {"code": "TRUNCATED", "params": {...}}；tag 判别式即 notice code。
/// 新增变体必须补所有 match（无 _ 通配），否则编译失败（§15.7）。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(
    tag = "code",
    content = "params",
    rename_all = "SCREAMING_SNAKE_CASE",
    rename_all_fields = "camelCase"
)]
pub enum Notice {
    Truncated {
        kept: u64,
        total: u64,
        unit: TruncUnit,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<String>,
    },
    Degraded {
        requested: OutputShape,
        actual: DegradedActual,
        reason: DegradeReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot: Option<String>,
    },
    LowConfidence {
        p: f32,
        views: ConfidenceViews,
    },
    IsolationFallback {
        requested: IsolationLevel,
        actual: FallbackAction,
        rule: String,
        reason: IsolationReason,
    },
    SlotUnavailable {
        slot: String,
        fallback: SlotFallback,
    },
    Redacted {
        count: u32,
        retention: f32,
    },
    Normalized {
        level: NormalizeLevel,
        kind: NormalizeKind,
    },
    Uncalibrated {
        slot: String,
        adapter: AdapterKind,
    },
    LossyDecode {
        encoding: String,
        replacements: u32,
    },
    Sniffed {
        ext: String,
        actual: SniffActual,
    },
}

/// §4.4b 敏感信息：绝对路径规则不得把用户名与绝对路径前缀带进 content。
/// 无工作区上下文可用时统一降级为 <external>；正常 glob 原样保留。
pub fn sanitize_rule(rule: &str) -> String {
    if rule.starts_with('/') {
        "<external>".to_owned()
    } else {
        rule.to_owned()
    }
}

/// 确定性缩短自由字符串字段（§4.4b 超预算第 4 步）：前 12 字符 + 内容哈希前 8 位。
fn shorten(s: &str) -> String {
    if s.len() <= 24 {
        return s.to_owned();
    }
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    let head: String = s.chars().take(12).collect();
    format!("{}-{}", head, hex::encode(&digest[..4]))
}

impl Notice {
    /// 程序分支用；字符串必须与 serde tag 完全一致（§15.7 锁定）。
    pub fn code(&self) -> &'static str {
        match self {
            Notice::Truncated { .. } => "TRUNCATED",
            Notice::Degraded { .. } => "DEGRADED",
            Notice::LowConfidence { .. } => "LOW_CONFIDENCE",
            Notice::IsolationFallback { .. } => "ISOLATION_FALLBACK",
            Notice::SlotUnavailable { .. } => "SLOT_UNAVAILABLE",
            Notice::Redacted { .. } => "REDACTED",
            Notice::Normalized { .. } => "NORMALIZED",
            Notice::Uncalibrated { .. } => "UNCALIBRATED",
            Notice::LossyDecode { .. } => "LOSSY_DECODE",
            Notice::Sniffed { .. } => "SNIFFED",
        }
    }

    /// 固定渲染顺序（§4.4b:194-195），属于契约。
    fn rank(&self) -> u8 {
        match self {
            Notice::IsolationFallback { .. } => 0,
            Notice::Redacted { .. } => 1,
            Notice::Truncated { .. } => 2,
            Notice::SlotUnavailable { .. } => 3,
            Notice::Degraded { .. } => 4,
            Notice::LowConfidence { .. } => 5,
            Notice::Normalized { .. } => 6,
            Notice::Uncalibrated { .. } => 7,
            Notice::LossyDecode { .. } => 8,
            Notice::Sniffed { .. } => 9,
        }
    }

    /// 预算优先级（§4.4b:222-224）：安全 > 完整性 > 可用性 > 认知。
    fn class_rank(&self) -> u8 {
        match self {
            Notice::IsolationFallback { .. } | Notice::Redacted { .. } => 0,
            Notice::Truncated { .. } => 1,
            Notice::SlotUnavailable { .. } | Notice::Degraded { .. } => 2,
            Notice::LowConfidence { .. }
            | Notice::Normalized { .. }
            | Notice::Uncalibrated { .. }
            | Notice::LossyDecode { .. }
            | Notice::Sniffed { .. } => 3,
        }
    }

    /// 同 code 多条的二级键：slot → pointer → level，缺省按 params 序列化字典序（§4.4b:190-191）。
    fn secondary_key(&self) -> String {
        match self {
            Notice::Degraded { slot, .. } => slot.clone().unwrap_or_default(),
            Notice::SlotUnavailable { slot, .. } => slot.clone(),
            Notice::Uncalibrated { slot, .. } => slot.clone(),
            Notice::Truncated { pointer, .. } => pointer.clone().unwrap_or_default(),
            Notice::Normalized { level, .. } => value_str(*level),
            _ => serde_json::to_string(self).unwrap(),
        }
    }

    /// 固定模板 [CODE] key1=value1 …；字段顺序即 §4.4b 表顺序，空值字段省略。
    pub fn render(&self) -> String {
        let mut fields: Vec<String> = Vec::new();
        match self {
            Notice::Truncated {
                kept,
                total,
                unit,
                pointer,
            } => {
                fields.push(format!("kept={kept}"));
                fields.push(format!("total={total}"));
                fields.push(format!("unit={}", value_str(*unit)));
                if let Some(p) = pointer {
                    fields.push(format!("pointer={p}"));
                }
            }
            Notice::Degraded {
                requested,
                actual,
                reason,
                slot,
            } => {
                fields.push(format!("requested={}", value_str(*requested)));
                fields.push(format!("actual={}", value_str(*actual)));
                fields.push(format!("reason={}", value_str(*reason)));
                if let Some(s) = slot {
                    fields.push(format!("slot={s}"));
                }
            }
            Notice::LowConfidence { p, views } => {
                fields.push(format!("p={p:.2}"));
                fields.push(format!("views={}", value_str(*views)));
            }
            Notice::IsolationFallback {
                requested,
                actual,
                rule,
                reason,
            } => {
                fields.push(format!("requested={}", value_str(*requested)));
                fields.push(format!("actual={}", value_str(*actual)));
                fields.push(format!("rule={}", sanitize_rule(rule)));
                fields.push(format!("reason={}", value_str(*reason)));
            }
            Notice::SlotUnavailable { slot, fallback } => {
                fields.push(format!("slot={slot}"));
                fields.push(format!("fallback={}", value_str(*fallback)));
            }
            Notice::Redacted { count, retention } => {
                fields.push(format!("count={count}"));
                fields.push(format!("retention={:.1}%", *retention * 100.0_f32));
            }
            Notice::Normalized { level, kind } => {
                fields.push(format!("level={}", value_str(*level)));
                fields.push(format!("kind={}", value_str(*kind)));
            }
            Notice::Uncalibrated { slot, adapter } => {
                fields.push(format!("slot={slot}"));
                fields.push(format!("adapter={}", value_str(*adapter)));
            }
            Notice::LossyDecode {
                encoding,
                replacements,
            } => {
                fields.push(format!("encoding={encoding}"));
                fields.push(format!("replacements={replacements}"));
            }
            Notice::Sniffed { ext, actual } => {
                fields.push(format!("ext={ext}"));
                fields.push(format!("actual={}", value_str(*actual)));
            }
        }
        format!("[{}] {}", self.code(), fields.join(" "))
    }

    /// 超 400 字节时缩短自由字符串字段（§4.4b:229），不删 code。
    fn shorten_strings(&mut self) {
        match self {
            Notice::Truncated { pointer, .. } => {
                if let Some(p) = pointer {
                    *p = shorten(p);
                }
            }
            Notice::Degraded { slot, .. } => {
                if let Some(s) = slot {
                    *s = shorten(s);
                }
            }
            Notice::IsolationFallback { rule, .. } => {
                *rule = shorten(rule);
            }
            Notice::SlotUnavailable { slot, .. } => {
                *slot = shorten(slot);
            }
            Notice::Uncalibrated { slot, .. } => {
                *slot = shorten(slot);
            }
            Notice::LossyDecode { encoding, .. } => {
                *encoding = shorten(encoding);
            }
            Notice::Sniffed { ext, .. } => {
                *ext = shorten(ext);
            }
            Notice::LowConfidence { .. } | Notice::Redacted { .. } | Notice::Normalized { .. } => {}
        }
    }

    /// 序列化成 structuredContent.notices 的一项。
    pub fn to_structured(&self) -> Value {
        serde_json::to_value(self).unwrap()
    }
}

fn by_render_order(a: &Notice, b: &Notice) -> std::cmp::Ordering {
    a.rank()
        .cmp(&b.rank())
        .then_with(|| a.secondary_key().cmp(&b.secondary_key()))
        .then_with(|| {
            serde_json::to_string(a)
                .unwrap()
                .cmp(&serde_json::to_string(b).unwrap())
        })
}

/// content 通道：按固定顺序渲染，每条一行，用换行拼接（§4.4b:190-200）。
pub fn render_notices(notices: &[Notice]) -> String {
    let mut sorted: Vec<&Notice> = notices.iter().collect();
    sorted.sort_by(|a, b| by_render_order(a, b));
    sorted
        .iter()
        .map(|n| n.render())
        .collect::<Vec<_>>()
        .join("\n")
}

/// structuredContent 通道：原样携带同一份 notices，顺序与 content 一致。
pub fn structured_notices(notices: &[Notice]) -> Value {
    let mut sorted = notices.to_vec();
    sorted.sort_by(by_render_order);
    json!(sorted.iter().map(Notice::to_structured).collect::<Vec<_>>())
}

pub struct NoticeBudget {
    pub notices: Vec<Notice>,
    /// 省略条数，写进 meta.noticeOverflow（§4.4b:226）。
    pub overflow: usize,
    /// 渲染后字节数（UTF-8）。
    pub bytes: usize,
}

/// 超预算合并算法（§4.4b:218-231），确定性：同输入同输出。
pub fn apply_budget(notices: Vec<Notice>) -> NoticeBudget {
    // 1. 同 code 合并：架构明确举例 REDACTED count 求和、retention 取最差值。
    let mut redacted: Vec<(u32, f32)> = Vec::new();
    let mut merged: Vec<Notice> = Vec::new();
    for n in notices {
        match n {
            Notice::Redacted { count, retention } => redacted.push((count, retention)),
            other => merged.push(other),
        }
    }
    if !redacted.is_empty() {
        let count = redacted.iter().map(|(c, _)| *c).sum();
        let retention = redacted
            .iter()
            .map(|(_, r)| *r)
            .fold(f32::INFINITY, f32::min);
        merged.push(Notice::Redacted { count, retention });
    }

    // 2. 按类优先级保留，类内沿用固定渲染顺序。
    merged.sort_by(|a, b| {
        a.class_rank()
            .cmp(&b.class_rank())
            .then_with(|| a.rank().cmp(&b.rank()))
            .then_with(|| a.secondary_key().cmp(&b.secondary_key()))
    });

    // 3. 超 6 条：从认知类里渲染顺序最靠后的开始整条省略；安全与完整性类永不省略。
    let mut overflow = 0;
    while merged.len() > 6 {
        let idx = merged
            .iter()
            .enumerate()
            .filter(|(_, n)| n.class_rank() == 3)
            .max_by_key(|(_, n)| n.rank())
            .map(|(i, _)| i);
        match idx {
            Some(i) => {
                merged.remove(i);
                overflow += 1;
            }
            None => break,
        }
    }

    // 4. 仍超 400 字节：缩短自由字符串字段，不删 code。
    let mut bytes = render_notices(&merged).len();
    if bytes > 400 {
        for n in &mut merged {
            n.shorten_strings();
        }
        bytes = render_notices(&merged).len();
    }

    merged.sort_by(by_render_order);
    NoticeBudget {
        notices: merged,
        overflow,
        bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 个 code 各一组固定 params（§4.4b 表 151-162 的渲染示例）。
    fn all_notices() -> Vec<Notice> {
        vec![
            Notice::Truncated {
                kept: 2000,
                total: 8734,
                unit: TruncUnit::Lines,
                pointer: Some("spill://ab12cd34".to_owned()),
            },
            Notice::Degraded {
                requested: OutputShape::Summary,
                actual: DegradedActual::None,
                reason: DegradeReason::CapabilityMissing,
                slot: Some("summarize".to_owned()),
            },
            Notice::LowConfidence {
                p: 0.62,
                views: ConfidenceViews::Local,
            },
            Notice::IsolationFallback {
                requested: IsolationLevel::Raw,
                actual: FallbackAction::Structure,
                rule: "**/.env*".to_owned(),
                reason: IsolationReason::RetentionBelowThreshold,
            },
            Notice::SlotUnavailable {
                slot: "vision".to_owned(),
                fallback: SlotFallback::Metadata,
            },
            Notice::Redacted {
                count: 3,
                retention: 0.994,
            },
            Notice::Normalized {
                level: NormalizeLevel::L2,
                kind: NormalizeKind::Eol,
            },
            Notice::Uncalibrated {
                slot: "decide".to_owned(),
                adapter: AdapterKind::Json,
            },
            Notice::LossyDecode {
                encoding: "gbk".to_owned(),
                replacements: 3,
            },
            Notice::Sniffed {
                ext: ".txt".to_owned(),
                actual: SniffActual::Binary,
            },
        ]
    }

    /// 告知枚举穷举：match 无 _ 通配；code() 与 serde tag 一致。
    #[test]
    fn notice_variants_exhaustive_and_tag_stable() {
        for n in all_notices() {
            match &n {
                Notice::Truncated { .. } => {}
                Notice::Degraded { .. } => {}
                Notice::LowConfidence { .. } => {}
                Notice::IsolationFallback { .. } => {}
                Notice::SlotUnavailable { .. } => {}
                Notice::Redacted { .. } => {}
                Notice::Normalized { .. } => {}
                Notice::Uncalibrated { .. } => {}
                Notice::LossyDecode { .. } => {}
                Notice::Sniffed { .. } => {}
            }
            assert_eq!(serde_json::to_value(&n).unwrap()["code"], json!(n.code()));
        }
    }

    /// 告知渲染快照：每个 code 断言完整字节。
    #[test]
    fn render_snapshot_all_codes() {
        let expected = [
            (
                "TRUNCATED",
                "[TRUNCATED] kept=2000 total=8734 unit=lines pointer=spill://ab12cd34",
            ),
            (
                "DEGRADED",
                "[DEGRADED] requested=summary actual=none reason=capability_missing slot=summarize",
            ),
            ("LOW_CONFIDENCE", "[LOW_CONFIDENCE] p=0.62 views=local"),
            (
                "ISOLATION_FALLBACK",
                "[ISOLATION_FALLBACK] requested=raw actual=structure rule=**/.env* reason=retention_below_threshold",
            ),
            (
                "SLOT_UNAVAILABLE",
                "[SLOT_UNAVAILABLE] slot=vision fallback=metadata",
            ),
            ("REDACTED", "[REDACTED] count=3 retention=99.4%"),
            ("NORMALIZED", "[NORMALIZED] level=2 kind=eol"),
            ("UNCALIBRATED", "[UNCALIBRATED] slot=decide adapter=json"),
            ("LOSSY_DECODE", "[LOSSY_DECODE] encoding=gbk replacements=3"),
            ("SNIFFED", "[SNIFFED] ext=.txt actual=binary"),
        ];
        assert_eq!(all_notices().len(), 10);
        for n in all_notices() {
            let want = expected.iter().find(|(c, _)| *c == n.code()).unwrap().1;
            assert_eq!(n.render(), want, "code {} 渲染不符", n.code());
        }
    }

    /// 告知完备性：10 个 code 每个至少一条路径，content 含对应 code。
    #[test]
    fn notice_completeness() {
        let content = render_notices(&all_notices());
        for n in all_notices() {
            assert!(content.contains(n.code()), "缺 code {}", n.code());
        }
    }

    /// 渲染顺序固定，与输入顺序无关（§4.4b:194-195）。
    #[test]
    fn render_order_is_fixed() {
        let mut reversed = all_notices();
        reversed.reverse();
        let rendered = render_notices(&reversed);
        let codes: Vec<&str> = rendered
            .lines()
            .map(|l| l.split(']').next().unwrap().trim_start_matches('['))
            .collect();
        assert_eq!(
            codes,
            vec![
                "ISOLATION_FALLBACK",
                "REDACTED",
                "TRUNCATED",
                "SLOT_UNAVAILABLE",
                "DEGRADED",
                "LOW_CONFIDENCE",
                "NORMALIZED",
                "UNCALIBRATED",
                "LOSSY_DECODE",
                "SNIFFED",
            ]
        );
    }

    /// 空值字段直接省略，不渲染 key=（§4.4b:171）。
    #[test]
    fn empty_optional_field_omitted() {
        let n = Notice::Truncated {
            kept: 10,
            total: 20,
            unit: TruncUnit::Bytes,
            pointer: None,
        };
        assert_eq!(n.render(), "[TRUNCATED] kept=10 total=20 unit=bytes");
        assert!(n.to_structured()["params"].get("pointer").is_none());
    }

    /// 告知预算：10 code 全触发 → ≤6 条、≤400 字节、params ≤4、安全与完整性类均在。
    #[test]
    fn budget_merge_and_overflow() {
        let b = apply_budget(all_notices());
        assert!(b.notices.len() <= 6, "实际 {} 条", b.notices.len());
        assert!(b.bytes <= 400, "实际 {} 字节", b.bytes);
        assert!(b.overflow > 0);
        for n in &b.notices {
            let params = n.to_structured()["params"].as_object().unwrap().len();
            assert!(params <= 4, "code {} params 有 {} 个", n.code(), params);
        }
        let codes: Vec<&str> = b.notices.iter().map(Notice::code).collect();
        for must in ["ISOLATION_FALLBACK", "REDACTED", "TRUNCATED"] {
            assert!(codes.contains(&must), "安全/完整性类 {must} 被省略");
        }
        assert_eq!(render_notices(&b.notices).lines().count(), b.notices.len());
    }

    /// 同 code 合并：两处 REDACTED → count 求和、retention 取最差值。
    #[test]
    fn same_code_redacted_merges() {
        let mut v = all_notices();
        v.push(Notice::Redacted {
            count: 2,
            retention: 0.98,
        });
        let b = apply_budget(v);
        let r = b.notices.iter().find(|n| n.code() == "REDACTED").unwrap();
        match r {
            Notice::Redacted { count, retention } => {
                assert_eq!(*count, 5);
                assert!((*retention - 0.98).abs() < 1e-6);
            }
            _ => panic!("REDACTED 合并结果类型不对"),
        }
    }

    /// 仍超 400 字节时缩短自由字符串字段，不删 code。
    #[test]
    fn budget_shortens_long_strings() {
        let mut v = all_notices();
        let long = "/Users/lanws/workspace/toolgate/config/".to_owned() + &"a".repeat(200);
        for n in &mut v {
            if let Notice::IsolationFallback { rule, .. } = n {
                *rule = long.clone();
            }
            if let Notice::Truncated { pointer, .. } = n {
                *pointer = Some(long.clone());
            }
        }
        let b = apply_budget(v);
        assert!(b.bytes <= 400, "实际 {} 字节", b.bytes);
        assert!(b.notices.iter().any(|n| n.code() == "ISOLATION_FALLBACK"));
    }

    /// 告知敏感信息：绝对路径规则下渲染不含用户名与绝对路径前缀。
    #[test]
    fn sensitive_rule_no_absolute_path() {
        let n = Notice::IsolationFallback {
            requested: IsolationLevel::Raw,
            actual: FallbackAction::Redact,
            rule: "/Users/lanws/workspace/toolgate/**/.env*".to_owned(),
            reason: IsolationReason::RetentionBelowThreshold,
        };
        let r = n.render();
        assert!(!r.contains("/Users/"));
        assert!(!r.contains("lanws"));
        assert!(r.contains("<external>"));
        assert!(r.contains("rule=<external>"));
    }

    /// 值枚举序列化为 snake_case，NormalizeLevel 为 "2"/"3"。
    #[test]
    fn value_enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_value(TruncUnit::Lines).unwrap(),
            json!("lines")
        );
        assert_eq!(
            serde_json::to_value(OutputShape::Summary).unwrap(),
            json!("summary")
        );
        assert_eq!(
            serde_json::to_value(DegradedActual::None).unwrap(),
            json!("none")
        );
        assert_eq!(
            serde_json::to_value(IsolationLevel::Raw).unwrap(),
            json!("raw")
        );
        assert_eq!(
            serde_json::to_value(FallbackAction::Structure).unwrap(),
            json!("structure")
        );
        assert_eq!(
            serde_json::to_value(SlotFallback::Metadata).unwrap(),
            json!("metadata")
        );
        assert_eq!(
            serde_json::to_value(AdapterKind::Json).unwrap(),
            json!("json")
        );
        assert_eq!(
            serde_json::to_value(SniffActual::Binary).unwrap(),
            json!("binary")
        );
        assert_eq!(
            serde_json::to_value(ConfidenceViews::Local).unwrap(),
            json!("local")
        );
        assert_eq!(
            serde_json::to_value(NormalizeKind::Eol).unwrap(),
            json!("eol")
        );
        assert_eq!(
            serde_json::to_value(NormalizeLevel::L2).unwrap(),
            json!("2")
        );
        assert_eq!(
            serde_json::to_value(NormalizeLevel::L3).unwrap(),
            json!("3")
        );
        assert_eq!(
            serde_json::to_value(DegradeReason::CapabilityMissing).unwrap(),
            json!("capability_missing")
        );
        assert_eq!(
            serde_json::to_value(IsolationReason::RetentionBelowThreshold).unwrap(),
            json!("retention_below_threshold")
        );
    }

    /// 同输入同输出（§16.3 对策二）。
    #[test]
    fn budget_is_deterministic() {
        let a = apply_budget(all_notices());
        let b = apply_budget(all_notices());
        assert_eq!(a.notices, b.notices);
        assert_eq!(a.overflow, b.overflow);
        assert_eq!(a.bytes, b.bytes);
        assert_eq!(render_notices(&a.notices), render_notices(&b.notices));
        assert_eq!(
            structured_notices(&a.notices),
            structured_notices(&b.notices)
        );
    }

    /// structuredContent 与 content 出自同一份 notices，顺序一致。
    #[test]
    fn two_channels_share_one_source() {
        let b = apply_budget(all_notices());
        let structured = structured_notices(&b.notices);
        let codes: Vec<&str> = structured
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["code"].as_str().unwrap())
            .collect();
        let rendered = render_notices(&b.notices);
        let content_codes: Vec<&str> = rendered
            .lines()
            .map(|l| l.split(']').next().unwrap().trim_start_matches('['))
            .collect();
        assert_eq!(codes, content_codes);
    }
}
