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
    /// §17.2 路径穿越：read / write / edit / search 的文件路径必须落在这些根内。
    /// 默认只有工作目录；调用方按 paths.allowed_roots 注入扩大范围。
    pub allowed_roots: Vec<PathBuf>,
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
    ///
    /// §17.2：允许根存 canonicalize 形态——macOS 上 /var 是 /private/var 的
    /// 符号链接，不统一形态会让根校验把根内路径误判成越界。cwd 本身保持调用方
    /// 传入的形态，bash / task 的子进程工作目录与模型可见路径都按它表示。
    pub fn new(cwd: impl Into<PathBuf>) -> std::io::Result<Self> {
        let cwd = cwd.into();
        let spill = SpillStore::with_default_dir()?;
        let snapshots = SpillStore::new(cwd.join(".toolgate").join("snapshots"))?;
        let allowed_roots = vec![std::fs::canonicalize(&cwd).unwrap_or_else(|_| cwd.clone())];
        Ok(Self {
            allowed_roots,
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
    ///
    /// 不做根校验：bash / task 的 cwd 本来就执行任意命令，限制它不构成边界
    /// （§17.2）。文件路径一律走 ToolContext::resolve_file。
    pub fn resolve(&self, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }

    /// §17.2：覆盖允许根目录（默认只有工作目录）。
    pub fn set_allowed_roots(&mut self, roots: Vec<PathBuf>) {
        self.allowed_roots = roots;
    }

    /// §17.2 路径穿越：解析文件路径并校验它落在允许根内。
    ///
    /// 先绝对化（相对路径按工作目录），再取 canonicalize 形态：目标存在时解析
    /// 符号链接；不存在（write 新建）时对最近的存在祖先 canonicalize 后拼回剩余
    /// 组件——父目录在根外一样被拒。允许根同样按 canonicalize 形态比较，
    /// 因此根内的符号链接指向根外时也逃不掉。
    pub fn resolve_file(&self, raw: &str) -> Result<PathBuf, crate::ToolError> {
        let requested = self.absolute(raw);
        let refused = || {
            crate::ToolError::isolation_refused(
                requested.display().to_string(),
                &self.allowed_roots,
            )
        };
        match canonical_target(&requested) {
            Some(resolved) if self.within_allowed_roots(&resolved) => {
                Ok(self.rebase_on_cwd(resolved))
            }
            _ => Err(refused()),
        }
    }

    /// 校验用 canonicalize 形态，返回时把工作目录前缀换回 cwd 的原有形态：
    /// 模型可见路径（display_path）与 bash / task 的 cwd 因此保持同一表示。
    fn rebase_on_cwd(&self, resolved: PathBuf) -> PathBuf {
        let Ok(canon_cwd) = std::fs::canonicalize(&self.cwd) else {
            return resolved;
        };
        match resolved.strip_prefix(&canon_cwd) {
            Ok(rel) => self.cwd.join(rel),
            Err(_) => resolved,
        }
    }

    fn absolute(&self, raw: &str) -> PathBuf {
        let path = Path::new(raw);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    /// 允许根按 canonicalize 形态比较；根本身解析失败时按原样比较（不静默扩大）。
    fn within_allowed_roots(&self, target: &Path) -> bool {
        self.allowed_roots.iter().any(|root| {
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            target.starts_with(root)
        })
    }
}

/// §17.2：把绝对路径解析成 canonicalize 形态。
///
/// 目标存在时直接 canonicalize；不存在时逐级向上找最近的存在祖先，
/// canonicalize 后拼回剩余组件。连根都不存在时返回 None（调用方按拒绝处理）。
fn canonical_target(absolute: &Path) -> Option<PathBuf> {
    if let Ok(canon) = std::fs::canonicalize(absolute) {
        return Some(canon);
    }
    let mut base = absolute.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while let Some(name) = base.file_name() {
        rest.push(name.to_owned());
        if !base.pop() {
            break;
        }
        if let Ok(canon) = std::fs::canonicalize(&base) {
            let mut out = canon;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
    }
    None
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
                "notices": { "type": "array" }
            },
            "additionalProperties": true
        })
    }
}

/// 终止整个进程组（§14 阶段 4）：先按负 PID 发 SIGKILL，再杀直接子进程。
///
/// 进程组由启动时的 `process_group(0)` 建立；`kill` 是 POSIX 必备命令，
/// 不引入新依赖。调用方仍要 `child.kill()` 兜底（例如进程尚未入组）。
pub(crate) async fn kill_process_group(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = tokio::process::Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pid}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
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
/// §3.10 准入：能力矩阵判「不可用」的 source × slot 一律 unavailable，不因绑定而谎报 ready。
/// 没有判定输出时 confidence 给 null，不编造（§4.6）。
/// 一个函数产出、六个工具共用，避免各工具自行拼装形成双源。
pub fn negotiation_meta(intent: Intent, slots: &SlotSet, client: Option<&SlotClient>) -> Value {
    let mut status = serde_json::Map::new();
    let mut detail = serde_json::Map::new();
    for kind in SlotKind::ALL {
        let state = match client {
            Some(client) => {
                // §3.10 / §19.6 准入与原因来自同一份判定（`slot_blocker`）：
                // 能力矩阵判「不可用」或构建缺协议客户端 → unavailable；
                // 连续失败禁用 → degraded；其余 ready。不因绑定而谎报 ready（§7.3）。
                match client.slot_blocker(kind) {
                    Some(crate::capability::SlotBlocker::Disabled) => "degraded",
                    Some(_) => "unavailable",
                    None => "ready",
                }
            }
            None => {
                if slots.is_available(kind) {
                    "ready"
                } else {
                    "unavailable"
                }
            }
        };
        if let Some(client) = client
            && let Some(blocker) = client.slot_blocker(kind)
        {
            detail.insert(
                kind.as_str().to_owned(),
                json!({
                    "reason": blocker.as_str(),
                    "nextSteps": blocker.next_steps(),
                }),
            );
        }
        status.insert(kind.as_str().to_owned(), json!(state));
    }
    let mut meta = json!({
        "intent": intent_tag(intent),
        "slmStatus": Value::Object(status),
        "confidence": Value::Null,
    });
    // ready / loading 时整个键不出现，不输出空对象（§6.1 契约）。
    if !detail.is_empty() {
        meta.as_object_mut()
            .expect("协商 meta 是对象")
            .insert("slmStatusDetail".to_owned(), Value::Object(detail));
    }
    meta
}

/// 概率类数值只保留两位小数：超过两位的精度对调用方没有信息量（§4.7 数值精度）。
/// MCP 返回与 CLI 调试输出共用这一份实现，同一事实不因通道不同而精度不同。
pub fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
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
    // 工具自己写过的 confidence 同样收敛到两位小数。
    if let Some(confidence) = target.get_mut("confidence")
        && let Some(value) = confidence.as_f64()
    {
        *confidence = json!(round2(value));
    }
    // §4.4b：noticeOverflow 只在真的发生省略时才有信息量，0 值省略。
    if target.get("noticeOverflow").and_then(Value::as_u64) == Some(0) {
        target.remove("noticeOverflow");
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

    /// 建一个工作目录上下文，并返回它的工作目录（根内路径的期望形态）。
    fn guard_root(name: &str) -> (ToolContext, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tg-guard-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ctx = ToolContext::new(&dir).expect("ctx");
        let root = ctx.cwd.clone();
        (ctx, root)
    }

    /// 根外的同级临时目录，用于构造越界目标。
    fn outside_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root
            .parent()
            .expect("root 有父目录")
            .join(format!("tg-guard-out-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir outside");
        dir
    }

    /// §17.2：根内路径解析成 canonicalize 形态；父目录还不存在的新建路径同样通过。
    #[test]
    fn resolve_file_allows_paths_inside_root() {
        let (ctx, root) = guard_root("inside");
        std::fs::write(root.join("a.txt"), "x").expect("write");
        assert_eq!(
            ctx.resolve_file("a.txt").expect("根内路径应通过"),
            root.join("a.txt")
        );
        assert_eq!(
            ctx.resolve_file("sub/dir/b.txt").expect("根内新建应通过"),
            root.join("sub/dir/b.txt")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// §17.2：根外绝对路径与 ../ 逃逸一律拒绝，错误码是 ISOLATION_REFUSED。
    #[test]
    fn resolve_file_refuses_outside_and_parent_escape() {
        let (ctx, root) = guard_root("escape");
        let outside = outside_dir(&root, "abs");
        for raw in [
            outside.join("x.txt").display().to_string(),
            "../x.txt".to_owned(),
            "a/../../../etc/passwd".to_owned(),
        ] {
            let err = ctx.resolve_file(&raw).expect_err("越界路径应拒绝");
            assert_eq!(
                crate::error::govern_error(&err).code,
                "ISOLATION_REFUSED",
                "{raw} 应按路径穿越拒绝"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(outside);
    }

    /// §17.2：write 新建文件时校验父目录——父目录在根外同样拒绝。
    #[test]
    fn resolve_file_checks_parent_for_new_files() {
        let (ctx, root) = guard_root("parent");
        let outside = outside_dir(&root, "parent");
        let target = outside.join("new.txt");
        let err = ctx
            .resolve_file(&target.display().to_string())
            .expect_err("根外父目录应拒绝");
        assert_eq!(crate::error::govern_error(&err).code, "ISOLATION_REFUSED");
        assert!(
            ctx.resolve_file("deep/new.txt").is_ok(),
            "根内新建（父目录不存在）不应被拒"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(outside);
    }

    /// §17.2：根内的符号链接指向根外时，按解析后的真实路径拒绝。
    #[cfg(unix)]
    #[test]
    fn resolve_file_refuses_symlink_escape() {
        let (ctx, root) = guard_root("symlink");
        let outside = outside_dir(&root, "symlink");
        std::fs::write(outside.join("secret.txt"), "s").expect("write");
        std::os::unix::fs::symlink(&outside, root.join("link")).expect("symlink");
        for raw in ["link/secret.txt", "link/new.txt"] {
            let err = ctx.resolve_file(raw).expect_err("符号链接逃逸应拒绝");
            assert_eq!(
                crate::error::govern_error(&err).code,
                "ISOLATION_REFUSED",
                "{raw} 应被符号链接逃逸检查拒绝"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(outside);
    }

    /// §17.2：扩大允许根后，原本被拒的路径通过。
    #[test]
    fn set_allowed_roots_extends_scope() {
        let (mut ctx, root) = guard_root("extend");
        let extra = outside_dir(&root, "extend");
        assert!(
            ctx.resolve_file(&extra.display().to_string()).is_err(),
            "默认只有工作目录，根外应拒绝"
        );
        ctx.set_allowed_roots(vec![root.clone(), extra.clone()]);
        assert_eq!(
            ctx.resolve_file(&extra.display().to_string())
                .expect("扩大后应通过"),
            std::fs::canonicalize(&extra).expect("canonical")
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(extra);
    }

    #[test]
    fn negotiation_meta_marks_all_slots_unavailable_without_fabricated_confidence() {
        let meta = negotiation_meta(Intent::Raw, &SlotSet::none(), None);
        assert_eq!(meta["intent"], json!("raw"));
        assert!(meta["confidence"].is_null());
        let status = meta["slmStatus"].as_object().expect("slmStatus 是对象");
        assert_eq!(status.len(), 6);
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

        // 概率类字段只保留两位小数。
        let mut rounded = ToolResponse::text("hi").with_meta(json!({ "confidence": 0.123456 }));
        inject_negotiation_meta(&mut rounded, Intent::Raw, None);
        assert_eq!(rounded.meta["confidence"], json!(0.12));

        let mut plain = ToolResponse::text("hi");
        inject_negotiation_meta(&mut plain, Intent::Auto, None);
        assert_eq!(plain.meta["intent"], json!("auto"));
        assert!(plain.meta["confidence"].is_null());
    }

    /// §19.6：外部协议来源在默认构建里没有客户端，绑定也不等于可用。
    fn external_bound_state() -> &'static str {
        if cfg!(feature = "external-protocols") {
            "ready"
        } else {
            "unavailable"
        }
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
        assert_eq!(meta["slmStatus"]["decide"], json!(external_bound_state()));
        assert_eq!(meta["slmStatus"]["summarize"], json!("unavailable"));
        assert!(meta["confidence"].is_null());
    }

    /// §3.10 准入：能力矩阵判「不可用」的 source × slot 不能谎报 ready。
    /// systemone 只做判定、不生成文本，serves 里写了 summarize 也不该是 ready。
    #[test]
    fn capability_unavailable_slot_is_not_reported_ready() {
        let cfg = crate::config::parse_config(
            "models:
  jev:
    source: systemone
    base_url: http://127.0.0.1:8090
    serves: [decide, summarize]
",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        let client = SlotClient::new(cfg);
        let meta = negotiation_meta(Intent::Raw, &SlotSet::none(), Some(&client));
        assert_eq!(meta["slmStatus"]["decide"], json!(external_bound_state()));
        assert_eq!(meta["slmStatus"]["summarize"], json!("unavailable"));
        // 未绑定的槽位仍按绑定状态，不受能力矩阵影响。
        assert_eq!(meta["slmStatus"]["rerank"], json!("unavailable"));
        assert!(meta["confidence"].is_null());
    }

    /// 能力矩阵标「受限」的槽位仍按绑定状态给 ready（受限由调用侧标注，不是不可用）。
    #[test]
    fn limited_capability_still_reports_bound_state() {
        let cfg = crate::config::parse_config(
            "models:
  jev:
    source: systemone
    base_url: http://127.0.0.1:8090
    serves: [classify, decide]
",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        let client = SlotClient::new(cfg);
        let meta = negotiation_meta(Intent::Raw, &SlotSet::none(), Some(&client));
        assert_eq!(meta["slmStatus"]["classify"], json!(external_bound_state()));
        assert_eq!(meta["slmStatus"]["decide"], json!(external_bound_state()));
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
