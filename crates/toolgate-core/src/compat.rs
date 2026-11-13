//! 阶段 1.5 兼容性层（§14 阶段 1.5 / §18.1b / §4.2 / §13.2）。
//!
//! 两件事：解析 2026-07-28 的每请求 _meta 元数据（无握手），
//! 以及让 v2 首版默认 intent=raw 保持 v1 行为。
//! 纯逻辑：不发起模型调用、不写配置、不跨请求保存状态（§22.5）。

use serde_json::Value;

use crate::decision::{DecisionInput, ExposureLevel, FallbackStep, Intent, UpstreamCapability};
use crate::locale::Language;
use crate::understanding::TurnContext;

/// 支持的 MCP 协议版本，按新到旧排列。
pub const SUPPORTED_VERSIONS: &[&str] = &["2026-07-28", "2025-11-25", "2025-06-18"];
pub const LATEST_VERSION: &str = "2026-07-28";

/// 请求级上游能力（§18.1b）。max_image_edge 供图片尺寸治理阶段使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamMeta {
    pub vision: bool,
    pub max_image_edge: Option<u32>,
}

/// 请求级会话标识（§18.1b / §18.5 的 task 表分区）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub session_id: String,
}

/// 每个请求携带的元数据。全部可选；缺省不报错（§18.1b）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestMeta {
    pub protocol_version: Option<String>,
    pub upstream: Option<UpstreamMeta>,
    pub session: Option<SessionMeta>,
    /// 方案 B 的 turn 载体；本阶段只解析，跨请求聚合留给阶段 1.5 之后（§13.4）。
    pub turn: Option<TurnContext>,
    /// 用户语言（§18.1b）：来自 _meta.toolgate/language，缺省时按环境判定。
    pub language: Option<Language>,
}

impl RequestMeta {
    /// 保守默认：不传 _meta 时 vision=false（§18.1b「不确定就不给」）。
    pub fn upstream_capability(&self) -> UpstreamCapability {
        UpstreamCapability {
            vision: self.upstream.as_ref().is_some_and(|u| u.vision),
        }
    }

    /// 默认跟随用户语言：请求声明 > 环境变量 > 源语言兜底（§18.1b）。
    pub fn effective_language(&self) -> Language {
        self.language.unwrap_or_else(Language::from_env)
    }
}

/// 解析 params._meta。宽容读取：未知字段忽略、字段类型不符按缺省处理——
/// 这是请求元数据，不是启动配置，§18.9 的「未知字段一律拒绝」不适用于此。
pub fn parse_request_meta(params: &Value) -> RequestMeta {
    let meta = params.get("_meta");

    let protocol_version = meta
        .and_then(|m| m.get("protocolVersion"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });

    let upstream = meta
        .and_then(|m| m.get("toolgate/upstream"))
        .filter(|u| u.is_object())
        .map(|u| UpstreamMeta {
            vision: u.get("vision").and_then(Value::as_bool).unwrap_or(false),
            max_image_edge: u
                .get("maxImageEdge")
                .and_then(Value::as_u64)
                .map(|n| n as u32),
        });

    let session = meta
        .and_then(|m| m.get("toolgate/session"))
        .and_then(|s| s.get("session_id"))
        .and_then(Value::as_str)
        .map(|id| SessionMeta {
            session_id: id.to_owned(),
        });

    let turn = meta.and_then(|m| m.get("toolgate/turn")).and_then(|t| {
        let turn_id = t.get("turn_id").and_then(Value::as_str)?.to_owned();
        let seq = t.get("seq").and_then(Value::as_u64).unwrap_or(0) as u32;
        let budget = t
            .get("budget")
            .and_then(|b| b.get("max_judgments"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        Some(TurnContext {
            turn_id,
            seq,
            budget,
        })
    });

    let language = meta
        .and_then(|m| m.get("toolgate/language"))
        .and_then(Value::as_str)
        .and_then(Language::parse);

    RequestMeta {
        protocol_version,
        upstream,
        session,
        turn,
        language,
    }
}

/// 2026-07-28 无握手：每个请求独立接受或拒绝版本（§18.1b）。
/// initialize 属于旧版握手，由该分支自己协商，不在这里校验。
/// 请求不带版本 = 旧版无元数据客户端，放行（v1 兼容）。
pub fn check_protocol_version(method: &str, request: &Value) -> Result<Option<String>, String> {
    if method == "initialize" {
        return Ok(None);
    }
    let params = request.get("params");
    let version = params
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get("protocolVersion"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    match version {
        Some(v) if !SUPPORTED_VERSIONS.contains(&v.as_str()) => Err(format!(
            "不支持的协议版本 {v}；支持：{}。请改用受支持版本，或去掉请求里的版本字段按旧版无元数据方式调用",
            SUPPORTED_VERSIONS.join(" / ")
        )),
        other => Ok(other),
    }
}

/// v2 首版默认 raw（§4.2 兼容性约束）。auto 需显式配置开启。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatConfig {
    pub default_intent: Intent,
}

impl Default for CompatConfig {
    fn default() -> Self {
        Self {
            default_intent: Intent::Raw,
        }
    }
}

/// 解析 arguments.intent；缺省取配置默认；非法值 fail loud（§18.9 结构错误口径）。
pub fn resolve_intent(arguments: &Value, config: &CompatConfig) -> Result<Intent, String> {
    match arguments.get("intent") {
        None => Ok(config.default_intent),
        Some(v) if !v.is_string() => {
            Err("intent 必须是字符串（raw / summary / structured / visual / auto）".to_owned())
        }
        Some(v) => match v.as_str().unwrap_or("") {
            "raw" => Ok(Intent::Raw),
            "summary" => Ok(Intent::Summary),
            "structured" => Ok(Intent::Structured),
            "visual" => Ok(Intent::Visual),
            "auto" => Ok(Intent::Auto),
            other => Err(format!(
                "未知 intent {other}；允许：raw / summary / structured / visual / auto"
            )),
        },
    }
}

/// 默认决策输入（§8.5 的默认配置）。保守缺省：按「槽位未绑定」处理，
/// 让 decide_output 按 §3.8 诚实标注降级（§7.3）；
/// 能拿到 SlotClient 的调用方必须用 `SlotClient::slot_blocker` 覆盖
/// `slot_available` / `slot_blocker`（§4.4b 一份事实源）。
pub fn default_decision_input(intent: Intent, meta: &RequestMeta) -> DecisionInput {
    DecisionInput {
        intent,
        isolation_default: ExposureLevel::Summary,
        max_exposure: ExposureLevel::Raw,
        fallback_chain: vec![
            FallbackStep::Redact,
            FallbackStep::Structure,
            FallbackStep::Summarize,
            FallbackStep::Refuse,
        ],
        redact_min_retention: 0.5,
        path_rule: None,
        content_rules: vec![],
        upstream_capability: meta.upstream_capability(),
        slot_available: false,
        slot_blocker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_params() -> Value {
        json!({
            "name": "read",
            "arguments": { "path": "shot.png", "intent": "auto" },
            "_meta": {
                "protocolVersion": "2026-07-28",
                "toolgate/upstream": { "vision": false, "maxImageEdge": 1568 },
                "toolgate/session": { "session_id": "sess_01" },
                "toolgate/turn": {
                    "turn_id": "turn_01", "seq": 3,
                    "budget": { "max_judgments": 8 }
                }
            }
        })
    }

    #[test]
    fn parse_full_meta() {
        let meta = parse_request_meta(&full_params());
        assert_eq!(meta.protocol_version.as_deref(), Some("2026-07-28"));
        let upstream = meta.upstream.as_ref().unwrap();
        assert!(!upstream.vision);
        assert_eq!(upstream.max_image_edge, Some(1568));
        assert_eq!(meta.session.as_ref().unwrap().session_id, "sess_01");
        let turn = meta.turn.as_ref().unwrap();
        assert_eq!(turn.turn_id, "turn_01");
        assert_eq!(turn.seq, 3);
        assert_eq!(turn.budget, 8);
    }

    #[test]
    fn missing_meta_defaults_to_conservative() {
        let meta = parse_request_meta(&json!({ "name": "read" }));
        assert!(meta.protocol_version.is_none());
        assert!(meta.upstream.is_none());
        assert!(meta.session.is_none());
        assert!(meta.turn.is_none());
        assert!(!meta.upstream_capability().vision);
    }

    #[test]
    fn version_check_accepts_supported_and_rejects_unknown() {
        let ok = json!({
            "method": "tools/call",
            "params": { "_meta": { "protocolVersion": "2026-07-28" } }
        });
        assert_eq!(
            check_protocol_version("tools/call", &ok).unwrap(),
            Some("2026-07-28".to_owned())
        );
        let bad = json!({
            "method": "tools/call",
            "params": { "_meta": { "protocolVersion": "1999-01-01" } }
        });
        let err = check_protocol_version("tools/call", &bad).unwrap_err();
        assert!(err.contains("1999-01-01"));
        assert!(err.contains("2026-07-28"));
    }

    #[test]
    fn initialize_bypasses_per_request_check() {
        let req = json!({
            "method": "initialize",
            "params": { "protocolVersion": "1999-01-01" }
        });
        assert_eq!(check_protocol_version("initialize", &req).unwrap(), None);
    }

    #[test]
    fn request_without_version_is_allowed() {
        let req = json!({ "method": "tools/call", "params": { "name": "read" } });
        assert_eq!(check_protocol_version("tools/call", &req).unwrap(), None);
    }

    #[test]
    fn intent_defaults_to_raw() {
        let config = CompatConfig::default();
        assert_eq!(config.default_intent, Intent::Raw);
        assert_eq!(resolve_intent(&json!({}), &config).unwrap(), Intent::Raw);
        let auto_config = CompatConfig {
            default_intent: Intent::Auto,
        };
        assert_eq!(
            resolve_intent(&json!({}), &auto_config).unwrap(),
            Intent::Auto
        );
    }

    #[test]
    fn intent_explicit_values_and_fail_loud() {
        let config = CompatConfig::default();
        for (raw, want) in [
            ("raw", Intent::Raw),
            ("summary", Intent::Summary),
            ("structured", Intent::Structured),
            ("visual", Intent::Visual),
            ("auto", Intent::Auto),
        ] {
            assert_eq!(
                resolve_intent(&json!({ "intent": raw }), &config).unwrap(),
                want
            );
        }
        assert!(resolve_intent(&json!({ "intent": "bogus" }), &config).is_err());
        assert!(resolve_intent(&json!({ "intent": 7 }), &config).is_err());
    }

    #[test]
    fn default_decision_input_is_conservative() {
        let meta = parse_request_meta(&json!({}));
        let input = default_decision_input(Intent::Auto, &meta);
        assert_eq!(input.intent, Intent::Auto);
        assert_eq!(input.max_exposure, ExposureLevel::Raw);
        assert_eq!(input.isolation_default, ExposureLevel::Summary);
        assert_eq!(input.fallback_chain.len(), 4);
        assert!(!input.upstream_capability.vision);
        assert!(!input.slot_available);
    }

    #[test]
    fn upstream_vision_from_meta_is_honored() {
        let meta = parse_request_meta(&json!({
            "_meta": { "toolgate/upstream": { "vision": true } }
        }));
        assert!(meta.upstream_capability().vision);
    }
}
