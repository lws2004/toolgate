//! 工具契约层。
//!
//! 所有工具共享同一套上下文与返回形态，因此传输层（MCP / CLI / 嵌入式）
//! 只需要做序列化，不必理解每个工具的细节。

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::bash_path::intent_tag;
use crate::compat::{CompatConfig, resolve_intent};
use crate::decision::Intent;
use crate::locale::Language;
use crate::notice::{ConfidenceViews, Notice};
use crate::slm::SlotClient;
use crate::slot_state::SlotState;
use crate::slots::{ConfidenceGate, SlotKind, SlotSet};
use crate::spill::SpillStore;
use crate::truncate::TruncationOptions;

pub mod bash;
pub mod edit;
pub mod read;
pub mod search;
pub mod task;
pub mod write;

/// 工具运行上下文：工作目录、两处存储、截断阈值。
///
/// 两处存储刻意分开：
///
/// - spill 放系统临时目录，装「输出太大放不下」的副本，随系统清理无妨；
/// - snapshots 放项目内的 .toolgate/snapshots，装「改之前的旧内容」，
///   必须活到用户想起来回滚为止。
///
/// 合成一处会导致快照被系统的临时目录清理策略带走——回滚就失效了。
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub spill: SpillStore,
    pub snapshots: SpillStore,
    pub limits: TruncationOptions,
    /// 请求级上游视觉能力（§18.1b）：来自 _meta.toolgate/upstream.vision，缺省 false。
    pub upstream_vision: bool,
    /// 请求级用户语言（§18.1b）：由传输层按 _meta / 环境注入。
    pub language: Language,
    /// §22 可丢弃预热缓存；Arc 共享，clear() 后行为不变。
    pub warm: crate::warm::WarmCache,
    /// 槽位客户端（§18.1）：由传输层启动时按配置注入；None = 未配置任何模型。
    pub slm: Option<std::sync::Arc<crate::slm::SlotClient>>,
}

impl ToolContext {
    /// 用工作目录创建上下文。
    pub fn new(cwd: impl Into<PathBuf>) -> std::io::Result<Self> {
        let cwd = cwd.into();
        let spill = SpillStore::with_default_dir()?;
        let snapshots = SpillStore::new(cwd.join(".toolgate").join("snapshots"))?;
        Ok(Self {
            cwd,
            spill,
            snapshots,
            limits: TruncationOptions::default(),
            upstream_vision: false,
            language: Language::default(),
            warm: crate::warm::WarmCache::default(),
            slm: None,
        })
    }

    /// 把入参里的相对路径解析成绝对路径（相对工作目录）。
    pub fn resolve(&self, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }
}

/// 工具返回的内容块，对应 MCP 的 content block。
#[derive(Debug, Clone)]
pub enum Content {
    /// 文本块。
    Text(String),
    /// 图片块：base64 数据 + MIME + 内容哈希。
    Image {
        mime_type: String,
        data_base64: String,
        sha256: String,
    },
}

impl Content {
    /// 序列化成 MCP 的 content block。
    pub fn to_mcp(&self) -> Value {
        match self {
            Self::Text(t) => json!({ "type": "text", "text": t }),
            Self::Image {
                mime_type,
                data_base64,
                sha256,
            } => json!({
                "type": "image",
                "mimeType": mime_type,
                "data": data_base64,
                "imageHash": sha256,
            }),
        }
    }
}

/// 一次工具调用的结果。
#[derive(Debug, Clone)]
pub struct ToolResponse {
    pub content: Vec<Content>,
    pub is_error: bool,
    /// 结构化元数据，给调用方做程序化判断用，不占模型上下文。
    pub meta: Value,
}

impl ToolResponse {
    pub fn text(body: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(body.into())],
            is_error: false,
            meta: Value::Null,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(message.into())],
            is_error: true,
            meta: Value::Null,
        }
    }

    pub fn with_meta(mut self, meta: Value) -> Self {
        self.meta = meta;
        self
    }
}

/// 工具的自描述信息，供 MCP tools/list 返回。
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// 英文描述（§18.1b 用户语言）；中文是源语言，缺省取 description。
    pub description_en: &'static str,
    pub input_schema: Value,
}

impl ToolSpec {
    /// §4.3 / §4.7 / §13.3：结构化输出的公共 schema，六个工具共用一份声明。
    ///
    /// 只固定所有工具都遵守的 notices 契约（§4.4b 一个事实源、两个通道）；
    /// 各工具特有的程序字段由 additionalProperties 放开，避免六份 schema
    /// 与各自的 meta 实现形成双源。intent 的实际协商形态见 §4.2 与工具描述。
    pub fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "notices": {
                    "type": "array",
                    "description": "§4.4b 告知数组；无告知时省略该字段。与 content 文本同源",
                    "items": { "type": "object" }
                }
            },
            "additionalProperties": true,
            "description": "structuredContent 是程序可读通道（§4.4）；notices 之外的键为各工具特有的程序事实（路径、行数、退出码等）"
        })
    }
}

/// §7.2 门控 → 告知：采用且足够确定时不告知；不够确定或两种视图时给 LOW_CONFIDENCE。
/// bash 与 task 共用，避免各写一份（§28 DRY）。
pub(crate) fn gate_notices(gate: ConfidenceGate, confidence: f64) -> Vec<Notice> {
    match gate {
        ConfidenceGate::Adopt => vec![],
        ConfidenceGate::AdoptWithLowConfidence => vec![Notice::LowConfidence {
            p: confidence as f32,
            views: ConfidenceViews::Local,
        }],
        ConfidenceGate::BothViews => vec![Notice::LowConfidence {
            p: confidence as f32,
            views: ConfidenceViews::Both,
        }],
    }
}

/// §7.3 返回契约 / §13.3 / §18.7：返回出口统一的协商状态。
///
/// 槽位状态给三档（§7.3）：没有绑定模型 → unavailable；有绑定且健康 → ready；
/// 有绑定但被连续失败禁用 → degraded。loading（embedded 懒加载）本轮不产出。
/// 没有判定输出时 confidence 给 null，不编造（§4.6）。
/// 一个函数产出、六个工具共用，避免各工具自行拼装形成双源。
pub fn negotiation_meta(intent: Intent, slots: &SlotSet, client: Option<&SlotClient>) -> Value {
    let mut status = serde_json::Map::new();
    for kind in SlotKind::ALL {
        let state = match client {
            Some(client) => slot_state(
                client.is_bound(kind),
                client.slot_health(kind).state == SlotState::Disabled,
            ),
            None => {
                if slots.is_available(kind) {
                    "ready"
                } else {
                    "unavailable"
                }
            }
        };
        status.insert(kind.as_str().to_owned(), json!(state));
    }
    json!({
        "intent": intent_tag(intent),
        "slmStatus": Value::Object(status),
        "confidence": Value::Null,
    })
}

/// §7.3 的槽位状态三档（纯函数）：未绑定 → unavailable；已禁用 → degraded；其余 → ready。
fn slot_state(bound: bool, disabled: bool) -> &'static str {
    if !bound {
        "unavailable"
    } else if disabled {
        "degraded"
    } else {
        "ready"
    }
}

/// 把协商状态并入一次工具返回的 meta；工具已写过的同名键不覆盖。
fn inject_negotiation_meta(resp: &mut ToolResponse, intent: Intent, client: Option<&SlotClient>) {
    let extra = negotiation_meta(intent, &SlotSet::none(), client);
    if !resp.meta.is_object() {
        resp.meta = json!({});
    }
    let target = resp.meta.as_object_mut().expect("meta 已确保为对象");
    for (key, value) in extra.as_object().expect("协商状态是对象") {
        target.entry(key.clone()).or_insert(value.clone());
    }
}

/// 全部工具的规格表。顺序即 MCP tools/list 的顺序。
pub fn all_specs() -> Vec<ToolSpec> {
    vec![
        read::spec(),
        write::spec(),
        edit::spec(),
        search::spec(),
        bash::spec(),
        task::spec(),
    ]
}

/// 按名字分发一次工具调用。
pub async fn dispatch(
    name: &str,
    args: &Value,
    ctx: &ToolContext,
) -> Result<ToolResponse, crate::ToolError> {
    // §13.3 / §7.3：协商状态在返回出口统一注入，六个工具共用一份事实源。
    let intent =
        resolve_intent(args, &CompatConfig::default()).map_err(crate::ToolError::bad_argument)?;
    let lang = ctx.language;
    let mut resp = match name {
        "read" => read::run(args, ctx).await,
        "write" => write::run(args, ctx).await,
        "edit" => edit::run(args, ctx).await,
        "search" => search::run(args, ctx).await,
        "bash" => bash::run(args, ctx).await,
        "task" => task::run(args, ctx).await,
        other => Err(crate::ToolError::unknown_tool(other.to_owned())),
    }
    .map_err(|e| e.localized(lang))?;
    inject_negotiation_meta(&mut resp, intent, ctx.slm.as_deref());
    Ok(resp)
}

/// 从入参对象里取必填字符串。
pub(crate) fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, crate::ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| crate::ToolError::bad_argument(format!("缺少字符串参数 {key}")))
}

/// 从入参对象里取可选字符串。
pub(crate) fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// 从入参对象里取可选整数。
pub(crate) fn optional_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

/// 从入参对象里取可选布尔。
pub(crate) fn optional_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::Intent;
    use crate::slots::{SlotKind, SlotSet};

    #[test]
    fn negotiation_meta_marks_all_slots_unavailable_without_fabricated_confidence() {
        let meta = negotiation_meta(Intent::Raw, &SlotSet::none(), None);
        assert_eq!(meta["intent"], json!("raw"));
        assert!(meta["confidence"].is_null());
        let status = meta["slmStatus"].as_object().expect("slmStatus 是对象");
        assert_eq!(status.len(), 5);
        for kind in SlotKind::ALL {
            assert_eq!(status[kind.as_str()], json!("unavailable"));
        }
        let ready = negotiation_meta(Intent::Auto, &SlotSet::all(), None);
        assert_eq!(ready["intent"], json!("auto"));
        assert_eq!(ready["slmStatus"]["decide"], json!("ready"));
    }

    #[test]
    fn injection_keeps_existing_meta_and_fills_public_fields() {
        let mut resp = ToolResponse::text("hi").with_meta(json!({ "intent": "raw", "custom": 7 }));
        inject_negotiation_meta(&mut resp, Intent::Raw, None);
        assert_eq!(resp.meta["intent"], json!("raw"));
        assert_eq!(resp.meta["custom"], json!(7));
        assert_eq!(resp.meta["slmStatus"]["decide"], json!("unavailable"));
        assert!(resp.meta["confidence"].is_null());

        let mut plain = ToolResponse::text("hi");
        inject_negotiation_meta(&mut plain, Intent::Auto, None);
        assert_eq!(plain.meta["intent"], json!("auto"));
        assert!(plain.meta["confidence"].is_null());
    }

    /// §7.3：槽位状态三档（纯函数）：未绑定 unavailable、健康 ready、禁用 degraded。
    #[test]
    fn slot_state_covers_three_states() {
        assert_eq!(slot_state(false, false), json!("unavailable"));
        assert_eq!(slot_state(true, false), json!("ready"));
        assert_eq!(slot_state(true, true), json!("degraded"));
    }

    /// §7.3 / §13.3：注入路径按真实绑定给状态，不再恒为全 unavailable。
    #[test]
    fn negotiation_meta_reflects_real_binding() {
        let empty = crate::config::parse_config("", "test.yaml").expect("空配置应当解析成功");
        let client = SlotClient::new(empty);
        let meta = negotiation_meta(Intent::Raw, &SlotSet::none(), Some(&client));
        assert_eq!(meta["slmStatus"]["decide"], json!("unavailable"));
        assert!(meta["confidence"].is_null());

        let bound = crate::config::parse_config(
            "models:\n  jev:\n    source: systemone\n    base_url: http://127.0.0.1:8090\n    serves: [decide]\n",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        let client = SlotClient::new(bound);
        let meta = negotiation_meta(Intent::Raw, &SlotSet::none(), Some(&client));
        assert_eq!(meta["slmStatus"]["decide"], json!("ready"));
        assert_eq!(meta["slmStatus"]["summarize"], json!("unavailable"));
        assert!(meta["confidence"].is_null());
    }

    /// §13.3 / §4.6：六个工具的规格声明必须齐全且一致。
    #[test]
    fn all_specs_declare_intent_output_schema_and_three_part_description() {
        let specs = all_specs();
        assert_eq!(specs.len(), 6);
        for spec in &specs {
            let intent = &spec.input_schema["properties"]["intent"];
            assert_eq!(intent["type"], json!("string"), "{} 缺 intent", spec.name);
            assert_eq!(
                intent["enum"],
                json!(["raw", "summary", "structured", "visual", "auto"]),
                "{} 的 intent 取值不全",
                spec.name
            );
            assert!(
                spec.description.contains("能做什么"),
                "{} 缺第一段",
                spec.name
            );
            assert!(
                spec.description.contains("不能做什么"),
                "{} 缺第二段",
                spec.name
            );
            assert!(
                spec.description.contains("什么时候用别的工具"),
                "{} 缺第三段",
                spec.name
            );
            let schema = spec.output_schema();
            assert_eq!(schema["type"], json!("object"), "{}", spec.name);
            assert_eq!(
                schema["properties"]["notices"]["type"],
                json!("array"),
                "{}",
                spec.name
            );
            assert_eq!(schema["additionalProperties"], json!(true), "{}", spec.name);
        }
    }
}
