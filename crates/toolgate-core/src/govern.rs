//! 读侧统一治理入口 govern 的六类处理器（§4.0a / §18.7）。
//!
//! 与 MCP 六工具解耦：本模块不关心「这是哪个工具」，只按**返回形状**分派到
//! 视觉 / 长文 / 错误 / 列表 / 结构化 / 元信息 六类处理器。免白名单：截获在宿主
//! 全局钩子完成，路由看形状；形状不识别 → 退回纯 L0 机械治理（§5.3）。
//!
//! 本层只做确定性 L0（截断 / 落盘 / 形态归一 / 错误可行动化）；依赖 SLM 的
//! summarize / rerank / vision 描述在槽位不可用时按 §3.8 降级为 L0 并告知，
//! 不假装成功（§4.6）。协议契约见 18-deployment.md §18.7。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ErrorView;
use crate::media;
use crate::notice::{Notice, SlotFallback, TruncUnit, budgeted_notices};
use crate::spill::SpillStore;
use crate::truncate::{TruncationOptions, format_size, truncate_tail};

/// 治理来源。kind 取 web|file|tool；tool / url / path 视来源可空。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    #[serde(default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// 一条待治理内容块。协议里 type 为 text 或 image。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// 宿主持有的不透明附件句柄（如 DSH 附件 sha256）；daemon 不解释、不取字节。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            kind: "text".to_owned(),
            text: Some(s.into()),
            ..Default::default()
        }
    }

    fn is_image(&self) -> bool {
        self.kind == "image"
    }

    fn text_str(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }
}

/// 治理策略。v2 首版只有读侧隔离档，缺省不启用强隔离。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    /// 反向委派出参（§18.7）：endpoint 为宿主回调地址，token 为单次不透明凭据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate: Option<DelegateRef>,
}

/// 反向委派端点（§18.7）。`unix:/abs.sock` 或 `http://127.0.0.1:port`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateRef {
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// 反向委派回调。core 不持任何网络/框架依赖：宿主宿主侧（daemon）实现本 trait，
/// 把单条内容回投宿主会话模型（§18.7 三约束）。`input` / 响应均为 JSON 值。
pub trait Delegate {
    fn call(
        &self,
        endpoint: &str,
        task: &str,
        token: Option<&str>,
        input: Value,
    ) -> Result<Value, String>;
}

/// govern 请求（§18.7）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernRequest {
    #[serde(default)]
    pub source: Source,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Policy>,
}

/// govern 响应（§18.7）。structuredContent / isError 按协议驼峰命名。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernResponse {
    pub content: Vec<ContentBlock>,
    pub structured_content: Value,
    pub meta: Value,
    pub is_error: bool,
}

/// 形状分类：路由依据是**内容形状**，不是工具名（§4.0a）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Vision,
    LongText,
    Error,
    Listing,
    Structured,
    MetaInfo,
    /// 兜底：形状不识别，原样透传（§5.3）。
    PassThrough,
}

impl Shape {
    pub fn as_str(self) -> &'static str {
        match self {
            Shape::Vision => "vision",
            Shape::LongText => "long_text",
            Shape::Error => "error",
            Shape::Listing => "listing",
            Shape::Structured => "structured",
            Shape::MetaInfo => "meta_info",
            Shape::PassThrough => "pass_through",
        }
    }
}

/// 三级信号里最确定的两级：确定性命中 + 结构自证（§4.0a）。
/// 第三级（classify/decide 按内容判）在无 SLM 时不可用，故此处只做前两级。
pub fn sniff_shape(req: &GovernRequest) -> Shape {
    // ① 确定性命中：图片块 / data URI / 图片魔数 base64。
    if req.content.iter().any(ContentBlock::is_image) {
        return Shape::Vision;
    }
    let joined = join_text(&req.content);
    let head = joined.trim_start();
    if head.starts_with("data:image/") || looks_like_image_base64(head) {
        return Shape::Vision;
    }

    // ② 结构自证：整段文本解析为单个 JSON 值（对象 / 数组）。
    if looks_like_structured(head) {
        return Shape::Structured;
    }

    // 确定性错误标记：错误位 / 退出码 / 栈。
    if looks_like_error(&joined) {
        return Shape::Error;
    }

    // 列表形状：多行且多数行呈现列表/命中行特征。
    if looks_like_listing(&joined) {
        return Shape::Listing;
    }

    // 长文：超双阈值或含 ANSI 控制序列。
    let opts = TruncationOptions::default();
    let lines = count_lines(&joined);
    if joined.len() > opts.max_bytes || lines > opts.max_lines || contains_ansi(&joined) {
        return Shape::LongText;
    }

    // 元信息：来源是文件且内容不可读为文本 / 二进制 / 未知 MIME。
    if req.source.kind == "file" && (joined.is_empty() || media::looks_binary(joined.as_bytes())) {
        return Shape::MetaInfo;
    }

    Shape::PassThrough
}

/// 治理入口：按形状分派到六类处理器之一，产出一致形状的响应。
///
/// 无委派时只做确定性 L0（`govern(req, spill)` 等价于 `govern_with(req, spill, None)`）。
pub fn govern(req: &GovernRequest, spill: &SpillStore) -> GovernResponse {
    govern_with(req, spill, None)
}

/// 带反向委派的治理入口（§18.7）。委派仅用于 SLM 槽位增强；不可用/失败即降级 L0。
pub fn govern_with(
    req: &GovernRequest,
    spill: &SpillStore,
    delegate: Option<&dyn Delegate>,
) -> GovernResponse {
    let intent = req.intent.clone().unwrap_or_else(|| "raw".to_owned());
    let deterministic = sniff_shape(req);
    // 第三级信号（§4.0a）：确定性嗅探无结论时，才把「仍模糊」的文本交 classify。
    // 仅 intent != raw 时启用：raw 直接透传，避免普通小输出每次多打一次模型。
    let classified = if deterministic == Shape::PassThrough && intent != "raw" {
        classify_shape(req, delegate)
    } else {
        None
    };
    let shape = classified.unwrap_or(deterministic);
    let mut resp = match shape {
        Shape::Vision => process_vision(req, &intent, delegate),
        Shape::LongText => process_long_text(req, spill, &intent, delegate),
        Shape::Error => process_error(req, &intent, delegate),
        Shape::Listing => process_listing(req, &intent, delegate),
        Shape::Structured => process_structured(req, &intent),
        Shape::MetaInfo => process_meta(req, &intent),
        Shape::PassThrough => process_pass_through(req, &intent),
    };
    if classified.is_some() && shape != Shape::PassThrough {
        resp.meta["slmStatus"]["classify"] = json!("delegated");
        if let Some(obj) = resp.structured_content.as_object_mut() {
            obj.insert("classified".to_owned(), json!(true));
            obj.insert("classifiedShape".to_owned(), json!(shape.as_str()));
        }
    }
    resp
}

/// 第三级形状判定：只解决确定性嗅探无法归类的文本，候选限定为长文 / 列表 / 错误。
/// 视觉与元信息已有确定性信号（image block / 文件来源），不在此重复判定，避免误伤。
fn classify_shape(req: &GovernRequest, delegate: Option<&dyn Delegate>) -> Option<Shape> {
    let joined = join_text(&req.content);
    if joined.trim().is_empty() {
        return None;
    }
    let sample: String = joined.chars().take(4000).collect();
    let input = json!({
        "instruction": "判断这段工具输出属于哪种返回形状；只输出一个 JSON 对象 {\"shape\":\"long_text|listing|error|pass_through\"}，不要解释。",
        "text": sample,
        "totalChars": joined.chars().count(),
    });
    let (text, _model) = call_delegate(req, delegate, "classify", input)?;
    let label = parse_shape_label(&text)?;
    match label.as_str() {
        "long_text" => Some(Shape::LongText),
        "listing" => Some(Shape::Listing),
        "error" => Some(Shape::Error),
        _ => None,
    }
}

fn parse_shape_label(text: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<Value>(text.trim())
        && let Some(s) = v.get("shape").and_then(Value::as_str)
    {
        return Some(s.trim().to_ascii_lowercase());
    }
    let cleaned = text.trim().trim_matches('"').trim().to_ascii_lowercase();
    if matches!(
        cleaned.as_str(),
        "long_text" | "listing" | "error" | "pass_through"
    ) {
        Some(cleaned)
    } else {
        None
    }
}

// ---------- 六类处理器 ----------

fn process_vision(
    req: &GovernRequest,
    intent: &str,
    delegate: Option<&dyn Delegate>,
) -> GovernResponse {
    // 先判 need_pixels（§4.0a ①/⑦）：要像素就保留原图块，绝不 base64 搬运。
    let mut facts = Vec::new();
    for block in &req.content {
        if block.is_image() {
            let mime = block.mime_type.clone().unwrap_or_else(|| "unknown".to_owned());
            facts.push(json!({
                "type": "image",
                "attachmentId": block.attachment_id,
                "mimeType": mime,
                "bytes": block.bytes.unwrap_or(0),
                "width": block.width,
                "height": block.height,
                "sha256": block.sha256,
            }));
        }
    }

    if intent == "visual" || source_returns_pixels(&req.source) {
        let mut blocks = Vec::new();
        for b in &req.content {
            if !b.is_image() {
                blocks.push(ContentBlock::text(b.text_str()));
            }
        }
        for b in &req.content {
            if b.is_image() {
                blocks.push(b.clone());
            }
        }
        let mut resp = finish(
            blocks,
            json!({ "shape": "vision", "needPixels": true, "images": facts }),
            intent,
            Vec::new(),
            false,
            None,
            None,
        );
        resp.meta["slmStatus"]["vision"] = json!("not_required");
        return resp;
    }

    // 只要文字：反向委派宿主会话模型做 OCR / 描述（§18.7）。
    let input = json!({
        "instruction": "OCR 或描述这张图；只需文字的场景优先逐字转录，无法确认的内容如实说明。",
        "images": facts,
    });
    if let Some((text, model)) = call_delegate(req, delegate, "vision", input) {
        let mut blocks = Vec::new();
        for b in &req.content {
            if !b.is_image() {
                blocks.push(ContentBlock::text(b.text_str()));
            }
        }
        blocks.push(ContentBlock::text(text));
        let mut resp = finish(
            blocks,
            json!({ "shape": "vision", "needPixels": false, "delegated": true, "model": model, "images": facts }),
            intent,
            Vec::new(),
            false,
            None,
            None,
        );
        resp.meta["slmStatus"]["vision"] = json!("delegated");
        return resp;
    }

    // 无委派或委派失败：保留原图（避免丢信息）+ 元信息告知降级（§3.8 / §4.6）。
    let mut blocks = vec![ContentBlock::text(
        "图片（元信息档）：已识别 image block；视觉槽位不可用，未做 OCR / 描述".to_owned(),
    )];
    for b in &req.content {
        if b.is_image() {
            blocks.push(b.clone());
        }
    }
    let notices = vec![Notice::SlotUnavailable {
        slot: "vision".to_owned(),
        fallback: SlotFallback::Metadata,
        reason: None,
    }];
    let mut resp = finish(
        blocks,
        json!({ "shape": "vision", "needPixels": false, "delegated": false, "images": facts }),
        intent,
        notices,
        false,
        None,
        None,
    );
    resp.meta["slmStatus"]["vision"] = json!("unavailable");
    resp
}

fn process_long_text(
    req: &GovernRequest,
    spill: &SpillStore,
    intent: &str,
    delegate: Option<&dyn Delegate>,
) -> GovernResponse {
    let joined = join_text(&req.content);
    let result = truncate_tail(&joined, TruncationOptions::default());
    let mut notices = Vec::new();
    let mut pointer = None;

    if result.truncated {
        // 落盘完整原文，回指针（§9.1）。
        if let Ok(sp) = spill.persist("govern", &joined) {
            pointer = Some(sp.path.display().to_string());
        }
        notices.push(Notice::Truncated {
            kept: result.output_lines as u64,
            total: result.total_lines as u64,
            unit: TruncUnit::Lines,
            pointer: pointer.clone(),
        });
    }

    // 反向委派 summarize：单条内容回投宿主会话模型（§18.7）。
    let input = json!({
        "instruction": "把这段工具输出压成高信噪比摘要，保留关键事实、数字、路径与错误线索；不要编造。",
        "text": result.content,
        "totalLines": result.total_lines,
    });
    if let Some((summary, model)) = call_delegate(req, delegate, "summarize", input) {
        let body = if result.truncated {
            format!(
                "{summary}\n\n[已截断：保留末 {} 行 / 共 {} 行，{} 字节；摘要经会话模型]",
                result.output_lines,
                result.total_lines,
                format_size(result.total_bytes)
            )
        } else {
            summary
        };
        let mut resp = finish(
            vec![ContentBlock::text(body)],
            json!({
                "shape": "long_text",
                "lines": result.total_lines,
                "bytes": result.total_bytes,
                "truncated": result.truncated,
                "delegated": true,
                "model": model,
            }),
            intent,
            notices,
            false,
            if result.truncated { Some(true) } else { None },
            pointer,
        );
        resp.meta["slmStatus"]["summarize"] = json!("delegated");
        return resp;
    }

    // summarize 槽位不可用：机械截断兜底。
    let mut body = joined.clone();
    if result.truncated {
        notices.push(Notice::SlotUnavailable {
            slot: "summarize".to_owned(),
            fallback: SlotFallback::Mechanical,
            reason: None,
        });
        body = format!(
            "{}\n\n[已截断：保留末 {} 行 / 共 {} 行，{} 字节]",
            result.content,
            result.output_lines,
            result.total_lines,
            format_size(result.total_bytes)
        );
    }

    finish(
        vec![ContentBlock::text(body)],
        json!({
            "shape": "long_text",
            "lines": result.total_lines,
            "bytes": result.total_bytes,
            "truncated": result.truncated,
        }),
        intent,
        notices,
        false,
        if result.truncated { Some(true) } else { None },
        pointer,
    )
}

fn process_error(
    req: &GovernRequest,
    intent: &str,
    delegate: Option<&dyn Delegate>,
) -> GovernResponse {
    let joined = join_text(&req.content);

    // 反向委派 classify：把错误归成 fact / reason / nextSteps（§18.7）。
    let input = json!({
        "instruction": "把错误归成三段式 JSON：{code,fact,reason,nextSteps:[...]}；nextSteps 给可执行下一步。",
        "text": joined,
    });
    if let Some((text, model)) = call_delegate(req, delegate, "classify", input) {
        let parsed = serde_json::from_str::<Value>(text.trim()).ok();
        if let Some(view) = parsed.as_ref().and_then(error_view_from_json) {
            let mut resp = finish(
                vec![ContentBlock::text(view.to_content_text())],
                json!({ "shape": "error", "error": view.to_structured(), "delegated": true, "model": model }),
                intent,
                Vec::new(),
                true,
                None,
                None,
            );
            resp.meta["slmStatus"]["classify"] = json!("delegated");
            return resp;
        }
    }

    let view = error_view_from_text(&joined);
    let text = view.to_content_text();
    let notices = vec![Notice::SlotUnavailable {
        slot: "decide".to_owned(),
        fallback: SlotFallback::Mechanical,
        reason: None,
    }];
    finish(
        vec![ContentBlock::text(text)],
        json!({ "shape": "error", "error": view.to_structured() }),
        intent,
        notices,
        true,
        None,
        None,
    )
}

fn process_listing(
    req: &GovernRequest,
    intent: &str,
    delegate: Option<&dyn Delegate>,
) -> GovernResponse {
    let joined = join_text(&req.content);
    let lines: Vec<&str> = joined.lines().collect();
    let total = lines.len();
    let opts = TruncationOptions::default();

    // 反向委派 rerank：按意图重排并保留最相关条目（§18.7）。
    let input = json!({
        "instruction": "按用户意图重排这批条目，保留最相关的若干条，返回纯文本列表；不要编造条目。",
        "text": joined,
        "limit": opts.max_lines,
        "total": total,
    });
    if let Some((ranked, model)) = call_delegate(req, delegate, "rerank", input) {
        let mut resp = finish(
            vec![ContentBlock::text(ranked)],
            json!({
                "shape": "listing",
                "items": total,
                "delegated": true,
                "model": model,
            }),
            intent,
            Vec::new(),
            false,
            None,
            None,
        );
        resp.meta["slmStatus"]["rerank"] = json!("delegated");
        return resp;
    }

    let mut notices = vec![Notice::SlotUnavailable {
        slot: "rerank".to_owned(),
        fallback: SlotFallback::Mechanical,
        reason: None,
    }];
    let mut body = joined.clone();
    let mut truncated = false;
    if total > opts.max_lines {
        let kept: Vec<&str> = lines[..opts.max_lines].to_vec();
        body = kept.join("\n");
        truncated = true;
        notices.push(Notice::Truncated {
            kept: opts.max_lines as u64,
            total: total as u64,
            unit: TruncUnit::Lines,
            pointer: None,
        });
    }
    finish(
        vec![ContentBlock::text(body)],
        json!({ "shape": "listing", "items": total, "truncated": truncated }),
        intent,
        notices,
        false,
        if truncated { Some(true) } else { None },
        None,
    )
}

fn process_structured(req: &GovernRequest, intent: &str) -> GovernResponse {
    let joined = join_text(&req.content);
    let parsed: Value = serde_json::from_str(joined.trim()).unwrap_or(Value::Null);
    // 保留结构，仅补 meta / notices（§4.0a）。
    finish(
        vec![ContentBlock::text(joined)],
        json!({ "shape": "structured", "data": parsed }),
        intent,
        Vec::new(),
        false,
        None,
        None,
    )
}

fn process_meta(req: &GovernRequest, intent: &str) -> GovernResponse {
    let path = req.source.path.clone().unwrap_or_default();
    let joined = join_text(&req.content);
    let bytes = joined.len();
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_owned();
    let mime = media::sniff_mime(joined.as_bytes());
    let text = format!(
        "[元信息] path={} type={} bytes={}",
        if path.is_empty() { "<unknown>" } else { &path },
        mime.as_deref().unwrap_or(if ext.is_empty() { "unknown" } else { &ext }),
        bytes
    );
    finish(
        vec![ContentBlock::text(text)],
        json!({
            "shape": "meta_info",
            "path": path,
            "mimeType": mime,
            "bytes": bytes,
        }),
        intent,
        Vec::new(),
        false,
        None,
        None,
    )
}

fn process_pass_through(req: &GovernRequest, intent: &str) -> GovernResponse {
    // 形状不识别：原样返回，只补元数据（§5.3 兜底）。
    let mut blocks = Vec::new();
    for block in &req.content {
        if block.is_image() {
            blocks.push(block.clone());
        } else {
            blocks.push(ContentBlock::text(block.text_str()));
        }
    }
    finish(
        blocks,
        json!({ "shape": "pass_through" }),
        intent,
        Vec::new(),
        false,
        None,
        None,
    )
}

// ---------- 组装与工具函数 ----------

/// 来源工具本身即「把图返回给模型」时保留像素（§4.0a ⑦）。
fn source_returns_pixels(source: &Source) -> bool {
    let Some(tool) = source.tool.as_deref() else {
        return false;
    };
    matches!(
        tool,
        "read_image" | "read-image" | "view_image" | "screenshot" | "browser_screenshot"
    )
}

/// 调反向委派；未配置端点、无委派实现、失败或 `ok:false` 均返回 None（降级 L0）。
fn call_delegate(
    req: &GovernRequest,
    delegate: Option<&dyn Delegate>,
    task: &str,
    input: Value,
) -> Option<(String, Option<String>)> {
    let delegate = delegate?;
    let d = req.policy.as_ref()?.delegate.as_ref()?;
    match delegate.call(&d.endpoint, task, d.token.as_deref(), input) {
        Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => {
            let text = v.get("text").and_then(Value::as_str).unwrap_or("").to_owned();
            if text.trim().is_empty() {
                return None;
            }
            let model = v.get("model").and_then(Value::as_str).map(str::to_owned);
            Some((text, model))
        }
        _ => None,
    }
}

/// 从委派响应解析三段式 ErrorView（classify 任务）；形状不符返回 None。
fn error_view_from_json(v: &Value) -> Option<ErrorView> {
    let obj = v.as_object()?;
    let fact = obj.get("fact")?.as_str()?.trim().to_owned();
    if fact.is_empty() {
        return None;
    }
    let reason = obj
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let next_steps = obj
        .get("nextSteps")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(ErrorView {
        code: obj
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("FAILED")
            .to_owned(),
        fact,
        reason,
        next_steps,
    })
}

#[allow(clippy::too_many_arguments)]
fn finish(
    content: Vec<ContentBlock>,
    mut structured: Value,
    intent: &str,
    notices: Vec<Notice>,
    is_error: bool,
    truncated: Option<bool>,
    spill_pointer: Option<String>,
) -> GovernResponse {
    let channels = budgeted_notices(notices);
    if let Some(obj) = structured.as_object_mut() {
        obj.insert("notices".to_owned(), channels.structured.clone());
    }
    let mut out_blocks = content;
    if !channels.text.is_empty()
        && let Some(first) = out_blocks.first_mut()
        && first.kind == "text"
    {
        // 模型可见通道：notices 文本追加到首块文本（§4.4b 两通道）。
        let existing = first.text.take().unwrap_or_default();
        let sep = if existing.is_empty() { "" } else { "\n\n" };
        first.text = Some(format!("{existing}{sep}{}", channels.text));
    }
    let meta = json!({
        "intent": intent,
        "slmStatus": default_slm_status(),
        "confidence": Value::Null,
        "redacted": false,
        "truncated": truncated.unwrap_or(false),
        "spillPointer": spill_pointer,
        "noticeOverflow": channels.overflow,
    });
    GovernResponse {
        content: out_blocks,
        structured_content: structured,
        meta,
        is_error,
    }
}

fn default_slm_status() -> Value {
    json!({
        "decide": "unavailable",
        "summarize": "unavailable",
        "vision": "unavailable",
        "rerank": "unavailable",
        "classify": "unavailable",
    })
}

fn join_text(blocks: &[ContentBlock]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in blocks {
        if !b.is_image() {
            parts.push(b.text_str().to_owned());
        }
    }
    parts.join("\n")
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let newlines = text.bytes().filter(|b| *b == b'\n').count();
    if text.ends_with('\n') { newlines } else { newlines + 1 }
}

fn contains_ansi(text: &str) -> bool {
    text.contains('\u{1b}')
}

fn looks_like_image_base64(head: &str) -> bool {
    head.starts_with("iVBORw0KGgo") // PNG
        || head.starts_with("/9j/") // JPEG
        || head.starts_with("R0lGOD") // GIF
        || head.starts_with("UklGR") // WEBP
}

fn looks_like_structured(head: &str) -> bool {
    if !(head.starts_with('{') || head.starts_with('[')) {
        return false;
    }
    serde_json::from_str::<Value>(head).is_ok()
}

fn looks_like_error(text: &str) -> bool {
    // 只认开头几行里的错误标记：整篇文档正文偶尔出现 "Error:" / "exit code"
    // 不代表这次工具调用失败，之前按任意位置 contains 会把 Markdown 技能文档
    // 误判成错误并改写成错误视图（实测 2026-11-14 读取 SKILL.md 被误治理）。
    let headlines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(8)
        .collect();
    for line in &headlines {
        let t = line.trim_start();
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("traceback (most recent call last)")
            || lower.starts_with("panicked at")
            || lower.starts_with("panicked:")
            || lower.starts_with("error:")
            || lower.starts_with("exit code")
            || lower.starts_with("exit status")
            || lower.starts_with("command not found")
        {
            return true;
        }
        // 典型栈帧：首行是错误描述，后续以 "at " 缩进缩进。
        if t.starts_with("at ") && headlines.first().is_some_and(|h| h.to_ascii_lowercase().contains("error")) {
            return true;
        }
    }
    false
}

fn looks_like_listing(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 20 {
        return false;
    }
    let hits = lines
        .iter()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("- ") || t.starts_with("* ") || looks_like_path_line(t)
        })
        .count();
    hits * 2 >= lines.len()
}

fn looks_like_path_line(s: &str) -> bool {
    // 形如 path:line: 或 path:line:col: 的命中行（search / grep 常见形状）。
    if s.is_empty() {
        return false;
    }
    let looks_path =
        s.contains('/') || s.contains('\\') || s.contains(".rs") || s.contains(".md");
    let has_digits = s.bytes().any(|b| b.is_ascii_digit());
    looks_path && has_digits && s.contains(':')
}

/// 把一段错误文本归成三段式 ErrorView（§1.4.4）：事实 / 原因 / 可选下一步。
fn error_view_from_text(text: &str) -> ErrorView {
    let first_error = text
        .lines()
        .find(|l| {
            let t = l.to_ascii_lowercase();
            t.contains("error") || t.contains("panic") || t.contains("traceback")
        })
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| first_nonempty_line(text));
    let exit = extract_exit_code(text);
    let fact = match exit {
        Some(code) => format!("命令以退出码 {code} 失败：{first_error}"),
        None => format!("返回携带错误标记：{first_error}"),
    };
    ErrorView {
        code: "FAILED".to_owned(),
        fact,
        reason: "工具返回了错误标记（isError / 非零退出 / 栈）".to_owned(),
        next_steps: vec![
            "先按上面的失败点修正输入或环境后重试".to_owned(),
            "若为偶发，重跑一次确认；持续失败改用更简单的等价调用".to_owned(),
        ],
    }
}

fn first_nonempty_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("（空输出）")
        .to_owned()
}

fn extract_exit_code(text: &str) -> Option<i32> {
    for token in ["exit code", "exit status", "exited with", "exit_code"] {
        if let Some(pos) = text.find(token) {
            let rest = &text[pos + token.len()..];
            let digits: String = rest
                .chars()
                .skip_while(|c| !c.is_ascii_digit() && *c != '-')
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if let Ok(code) = digits.parse::<i32>() {
                return Some(code);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> SpillStore {
        let dir = std::env::temp_dir().join(format!("toolgate-gov-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        SpillStore::new(dir).unwrap()
    }

    fn req_text(s: &str) -> GovernRequest {
        GovernRequest {
            source: Source::default(),
            content: vec![ContentBlock::text(s)],
            intent: None,
            policy: None,
        }
    }

    #[test]
    fn image_block_routes_to_vision() {
        let req = GovernRequest {
            source: Source::default(),
            content: vec![ContentBlock { kind: "image".into(), mime_type: Some("image/png".into()), ..Default::default() }],
            intent: None,
            policy: None,
        };
        assert_eq!(sniff_shape(&req), Shape::Vision);
        let resp = govern(&req, &store("vision"));
        assert!(!resp.is_error);
        assert_eq!(resp.structured_content["shape"], json!("vision"));
    }

    #[test]
    fn short_text_pass_through() {
        let req = req_text("hello world");
        assert_eq!(sniff_shape(&req), Shape::PassThrough);
        let resp = govern(&req, &store("pass"));
        assert_eq!(resp.content[0].text.as_deref(), Some("hello world"));
    }

    #[test]
    fn json_routes_to_structured() {
        let req = req_text("{\"a\":1,\"b\":[2,3]}");
        assert_eq!(sniff_shape(&req), Shape::Structured);
        let resp = govern(&req, &store("struct"));
        assert_eq!(resp.structured_content["data"]["a"], json!(1));
    }

    #[test]
    fn error_marker_routes_to_error() {
        let req = req_text("boom\nerror: something broke\nexit code 2");
        assert_eq!(sniff_shape(&req), Shape::Error);
        let resp = govern(&req, &store("err"));
        assert!(resp.is_error);
        let fact = resp.structured_content["error"]["fact"].as_str().unwrap_or("");
        assert!(fact.contains("退出码 2"), "fact={fact}");
        assert!(!resp.structured_content["error"]["nextSteps"].as_array().unwrap().is_empty());
    }

    #[test]
    fn error_marker_deep_in_document_is_not_error_shape() {
        // 文档正文中偶尔出现 error:/exit code 字样，不应把整篇返回改写成错误视图。
        let mut body: Vec<String> = (0..20).map(|i| format!("paragraph {i}")).collect();
        body.push("Example: Error: this is documentation, not a failure".to_string());
        body.push("exit code 2 appears here only as an example".to_string());
        let req = req_text(&body.join("\n"));
        assert_ne!(sniff_shape(&req), Shape::Error);
    }

    #[test]
    fn long_text_truncates_and_spills() {
        let body = (0..2100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let req = req_text(&body);
        assert_eq!(sniff_shape(&req), Shape::LongText);
        let resp = govern(&req, &store("long"));
        assert_eq!(resp.meta["truncated"], json!(true));
        assert!(resp.meta["spillPointer"].as_str().is_some());
    }

    #[test]
    fn unknown_shape_never_panics() {
        let req = req_text("");
        let resp = govern(&req, &store("empty"));
        // 空内容不 panic，落到某条确定性分支即可。
        assert!(resp.content.iter().all(|b| b.kind == "text"));
    }

    fn image_req(intent: &str, tool: Option<&str>) -> GovernRequest {
        GovernRequest {
            source: Source {
                kind: "tool".into(),
                tool: tool.map(str::to_owned),
                ..Default::default()
            },
            content: vec![ContentBlock {
                kind: "image".into(),
                mime_type: Some("image/png".into()),
                attachment_id: Some("att-123".into()),
                bytes: Some(4096),
                width: Some(800),
                height: Some(600),
                ..Default::default()
            }],
            intent: Some(intent.into()),
            policy: Some(Policy {
                isolation: None,
                delegate: Some(DelegateRef {
                    endpoint: "unix:/tmp/x.sock".into(),
                    token: Some("tok".into()),
                }),
            }),
        }
    }

    struct MockDelegate {
        task: &'static str,
        resp: Result<Value, String>,
    }
    impl Delegate for MockDelegate {
        fn call(
            &self,
            _endpoint: &str,
            task: &str,
            _token: Option<&str>,
            _input: Value,
        ) -> Result<Value, String> {
            if task != self.task {
                return Err(format!("unexpected task {task}"));
            }
            self.resp.clone()
        }
    }

    #[test]
    fn vision_keeps_pixels_for_read_image_tool() {
        let req = image_req("raw", Some("read_image"));
        let resp = govern(&req, &store("vision-pix"));
        assert!(resp.content.iter().any(|b| b.is_image()));
        assert_eq!(resp.structured_content["needPixels"], json!(true));
    }

    #[test]
    fn vision_delegates_and_replaces_image() {
        let req = image_req("summary", None);
        let d = MockDelegate {
            task: "vision",
            resp: Ok(json!({ "ok": true, "text": "OCR: hello world", "model": "m1" })),
        };
        let resp = govern_with(&req, &store("vision-del"), Some(&d));
        assert!(!resp.content.iter().any(|b| b.is_image()), "委派后应替换掉图块");
        assert!(resp.content.iter().any(|b| b.text.as_deref() == Some("OCR: hello world")));
        assert_eq!(resp.structured_content["delegated"], json!(true));
        assert_eq!(resp.meta["slmStatus"]["vision"], json!("delegated"));
    }

    #[test]
    fn vision_delegate_failure_keeps_image() {
        let req = image_req("summary", None);
        let d = MockDelegate {
            task: "vision",
            resp: Err("boom".into()),
        };
        let resp = govern_with(&req, &store("vision-fail"), Some(&d));
        assert!(resp.content.iter().any(|b| b.is_image()), "失败时保留原图");
        assert_eq!(resp.meta["slmStatus"]["vision"], json!("unavailable"));
    }

    #[test]
    fn long_text_delegates_summarize() {
        let body = (0..2100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("
");
        let mut req = req_text(&body);
        req.policy = Some(Policy {
            isolation: None,
            delegate: Some(DelegateRef {
                endpoint: "unix:/tmp/x.sock".into(),
                token: None,
            }),
        });
        let d = MockDelegate {
            task: "summarize",
            resp: Ok(json!({ "ok": true, "text": "SUMMARY" })),
        };
        let resp = govern_with(&req, &store("long-del"), Some(&d));
        assert!(resp.content[0].text.as_deref().unwrap_or("").contains("SUMMARY"));
        assert_eq!(resp.structured_content["delegated"], json!(true));
        assert_eq!(resp.meta["slmStatus"]["summarize"], json!("delegated"));
    }

    #[test]
    fn error_delegates_classify() {
        let mut req = req_text("boom
error: kaboom
exit code 3");
        req.policy = Some(Policy {
            isolation: None,
            delegate: Some(DelegateRef {
                endpoint: "unix:/tmp/x.sock".into(),
                token: None,
            }),
        });
        let d = MockDelegate {
            task: "classify",
            resp: Ok(json!({
                "ok": true,
                "text": "{\"code\":\"E3\",\"fact\":\"命令失败\",\"reason\":\"输入错\",\"nextSteps\":[\"修正输入\"]}"
            })),
        };
        let resp = govern_with(&req, &store("err-del"), Some(&d));
        assert_eq!(resp.structured_content["error"]["code"], json!("E3"));
        assert_eq!(resp.meta["slmStatus"]["classify"], json!("delegated"));
    }

    #[test]
    fn ambiguous_text_classified_by_delegate() {
        let body = (0..10)
            .map(|i| format!("result line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut req = req_text(&body);
        req.intent = Some("summary".into());
        req.policy = Some(Policy {
            isolation: None,
            delegate: Some(DelegateRef {
                endpoint: "unix:/tmp/x.sock".into(),
                token: None,
            }),
        });
        let d = MockDelegate {
            task: "classify",
            resp: Ok(json!({ "ok": true, "text": "{\"shape\":\"listing\"}" })),
        };
        let resp = govern_with(&req, &store("shape-classify"), Some(&d));
        assert_eq!(resp.structured_content["classifiedShape"], json!("listing"));
        assert_eq!(resp.meta["slmStatus"]["classify"], json!("delegated"));
    }

    #[test]
    fn raw_intent_skips_shape_classify() {
        let body = (0..10)
            .map(|i| format!("result line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut req = req_text(&body);
        req.policy = Some(Policy {
            isolation: None,
            delegate: Some(DelegateRef {
                endpoint: "unix:/tmp/x.sock".into(),
                token: None,
            }),
        });
        let d = MockDelegate {
            task: "classify",
            resp: Ok(json!({ "ok": true, "text": "{\"shape\":\"listing\"}" })),
        };
        let resp = govern_with(&req, &store("shape-raw"), Some(&d));
        assert!(resp.structured_content.get("classified").is_none());
        assert_eq!(resp.meta["slmStatus"]["classify"], json!("unavailable"));
    }
}
