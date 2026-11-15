//! 配置层：解析配置文件，做语法 / 结构 / 语义三层校验，并解析六个槽位的绑定。
//!
//! 只判定「配置错误」（§18.9 前三层）：语法错、结构错、语义错一律 fail loud，
//! 一次列全所有错误。资源失败（模型文件缺失、显存不足）属运行期降级，不在本模块（§3.8）。
//!
//! 解析路径固定为「noyalib::Value → 手工遍历校验」：serde derive 遇第一个错就失败，
//! 满足不了 §18.9「一次报全部错误」的要求。
//!
//! 架构依据：18-deployment.md §18.1（模型来源与配置形态）、§18.9（配置校验与错误处理）、
//! 03-slots.md §3.8（槽位可空）、§3.10（槽位、模型与绑定 / 形状适配）、§3.11（能力声明）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use noyalib::{Mapping, Value as YamlValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::decision::{ContentRule, ContentRuleKind, ExposureLevel, FallbackStep, PathRule};
use crate::locale::Language;
use crate::slots::SlotKind;

// ---------- 错误类型（§18.9「错误格式：可行动」） ----------

/// 单条配置错误。字段路径必须到叶子，每条都带可执行的下一步（§18.9 / §1.4.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigError {
    /// 机器可读错误码，如 CONFIG_UNKNOWN_FIELD。
    pub code: String,
    /// 字段路径，如 models.lfm3-vl.serves[0]。
    pub field: String,
    /// 实际值回显；密钥类字段只回显变量名，不回显密钥值（§18.9）。
    pub value: Option<String>,
    /// 期望的类型或取值集合。
    pub expected: String,
    /// 可执行的下一步。
    pub fix: String,
}

impl ConfigError {
    /// 按用户语言返回同一条错误：只译 expected 与 fix（§18.1b）。
    /// code / field / value 不译——field 是字段路径，value 是实际值，都不是自然语言。
    pub fn localized(&self, lang: Language) -> ConfigError {
        ConfigError {
            code: self.code.clone(),
            field: self.field.clone(),
            value: self.value.clone(),
            expected: localize_text(lang, &self.expected),
            fix: localize_text(lang, &self.fix),
        }
    }
}

/// 配置错误集合：一次列全（§18.9「多错误聚合返回」）。
#[derive(Debug, Clone, thiserror::Error)]
#[error("配置校验未通过：共 {count} 个错误", count = self.items.len())]
pub struct ConfigErrors {
    /// 全部错误，按「结构层 → 语义层」顺序排列。
    pub items: Vec<ConfigError>,
}

impl ConfigErrors {
    /// 是否没有错误。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 转成 §18.9 的 JSON 形态：{"errors":[{"code","field","value","expected","fix"}]}。
    pub fn to_json(&self) -> JsonValue {
        json!({
            "errors": self
                .items
                .iter()
                .map(|e| json!({
                    "code": e.code,
                    "field": e.field,
                    "value": e.value,
                    "expected": e.expected,
                    "fix": e.fix,
                }))
                .collect::<Vec<_>>(),
        })
    }

    /// 按用户语言返回同一组错误：逐条本地化 expected 与 fix（§18.1b）。
    pub fn localized(&self, lang: Language) -> ConfigErrors {
        ConfigErrors {
            items: self.items.iter().map(|e| e.localized(lang)).collect(),
        }
    }
}

// ---------- 枚举 ----------

/// 模型来源（§18.1）：来源类型与协议合成一个字段。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// 按 path / base_url 保守推断。
    Auto,
    /// 本地权重。
    Local,
    /// OpenAI Chat Completions。
    Openai,
    /// Anthropic Messages。
    Anthropic,
    /// OpenAI Responses。
    Responses,
    /// TypeSafe /v1/systemone（jev 类判定模型的原生协议，§18.1）。
    SystemOne,
    /// 原生 /v1/rerank 重排序端点（ADR 0007，§18.1）。
    Rerank,
    /// 原生 /v1/embeddings 向量端点（§18.1）。
    Embedding,
}

impl ModelSource {
    /// 字符串形态，与 YAML 取值一致。
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelSource::Auto => "auto",
            ModelSource::Local => "local",
            ModelSource::Openai => "openai",
            ModelSource::Anthropic => "anthropic",
            ModelSource::Responses => "responses",
            ModelSource::SystemOne => "systemone",
            ModelSource::Rerank => "rerank",
            ModelSource::Embedding => "embedding",
        }
    }

    /// 是否外部协议来源（§18.1：外部来源只能是 loopback，且 mode 只能 http）。
    fn is_external(&self) -> bool {
        matches!(
            self,
            ModelSource::Openai
                | ModelSource::Anthropic
                | ModelSource::Responses
                | ModelSource::SystemOne
                | ModelSource::Rerank
                | ModelSource::Embedding
        )
    }
}

/// 模型运行模式（§18.1）：进程内 embedded 或进程外 http。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SlmMode {
    /// 按槽位分档自动选择。
    Auto,
    /// 进程内加载权重。
    Embedded,
    /// 调用进程外模型服务。
    Http,
}

impl SlmMode {
    /// 字符串形态，与 YAML 取值一致。
    pub fn as_str(&self) -> &'static str {
        match self {
            SlmMode::Auto => "auto",
            SlmMode::Embedded => "embedded",
            SlmMode::Http => "http",
        }
    }
}

/// 形状适配器（§3.10）：能力与形状分开，适配器负责把模型输出转回槽位契约。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    /// 按来源与槽位推断。
    Auto,
    /// 模型原生按槽位契约输出。
    Native,
    /// 要求模型输出 JSON schema，解析成槽位形状。
    Json,
    /// 纯文本或 JSON 直接映射。
    Text,
    /// 输出排序列表或分数。
    Ordered,
}

impl Adapter {
    /// 字符串形态，与 YAML 取值一致。
    pub fn as_str(&self) -> &'static str {
        match self {
            Adapter::Auto => "auto",
            Adapter::Native => "native",
            Adapter::Json => "json",
            Adapter::Text => "text",
            Adapter::Ordered => "ordered",
        }
    }
}

/// 能力校验时机（§3.11）：首次使用校验 / 启动探测 / 关闭。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ProbeTiming {
    /// 第一次真实调用即视为校验。
    FirstUse,
    /// 启动时探测。
    Startup,
    /// 不做校验。
    Off,
}

impl ProbeTiming {
    /// 字符串形态，与 YAML 取值一致。
    pub fn as_str(&self) -> &'static str {
        match self {
            ProbeTiming::FirstUse => "first_use",
            ProbeTiming::Startup => "startup",
            ProbeTiming::Off => "off",
        }
    }
}

// ---------- 配置数据结构 ----------

/// 单个模型的配置（§18.1 / §3.10）。只允许下面这些字段，未知字段一律拒绝（§18.9）。
#[derive(Debug, Clone, Serialize)]
pub struct ModelConfig {
    /// 模型来源；缺省 auto。
    pub source: Option<ModelSource>,
    /// 运行模式；缺省继承 slm.mode。
    pub mode: Option<SlmMode>,
    /// 形状适配器；缺省 auto。
    pub adapter: Option<Adapter>,
    /// 本地权重路径。
    pub path: Option<String>,
    /// 外部协议接口地址。
    pub base_url: Option<String>,
    /// 上游模型名。
    pub model: Option<String>,
    /// 密钥环境变量名；密钥只从环境读，不落配置（§18.1）。
    pub api_key_env: Option<String>,
    /// 是否允许非 loopback 地址；缺省 false（§18.1）。
    pub allow_remote: bool,
    /// 该模型声明能服务的槽位（§3.10）；缺省空。
    pub serves: Vec<SlotKind>,
}

/// 模型清单里的一项（§3.10）。
#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    /// 模型 id（models 下的键）。
    pub id: String,
    /// 模型配置。
    pub config: ModelConfig,
}

/// 进程内加载配置（§18.1）。
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddedConfig {
    /// 后端名，如 llama-cpp / mlx。
    pub backend: Option<String>,
    /// 权重目录。
    pub model_dir: Option<String>,
    /// 最大常驻模型数。
    pub max_resident_models: Option<usize>,
    /// 冷槽位空闲卸载秒数。
    pub idle_ttl_seconds: Option<u64>,
}

/// 进程外模型服务配置（§18.1）。
#[derive(Debug, Clone, Serialize)]
pub struct HttpConfig {
    /// 模型服务地址。
    pub base_url: Option<String>,
}

/// SLM 全局配置（§18.1 / §3.11）。
#[derive(Debug, Clone, Serialize)]
pub struct SlmConfig {
    /// 默认运行模式；缺省 auto。
    pub mode: Option<SlmMode>,
    /// 能力校验时机。
    pub probe: Option<ProbeTiming>,
    /// 连续失败多少次禁用槽位。
    pub disable_after_failures: Option<u32>,
    /// 进程内加载配置。
    pub embedded: Option<EmbeddedConfig>,
    /// 进程外服务配置。
    pub http: Option<HttpConfig>,
}

/// 路径语义配置（§17.2 / §8.11 / 附录 D）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct PathsConfig {
    /// 文件访问允许根（§17.2）：read / write / edit / search 的路径必须落在其中。
    /// 缺省空 = 只用启动时工作目录。
    pub allowed_roots: Vec<String>,
    /// §8.11 路径呈现：默认 false，模型可见路径一律工作区相对；
    /// 仅 raw 类输出显式配 true 才回传绝对路径（仍过 path_username 规则）。
    /// 由外部配置决定（§2-6），模型不能经 intent 绕过。
    pub expose_absolute: bool,
}

/// 隔离策略配置（§8 / 附录 D）：isolation.* 段。
///
/// 缺省值取自附录 D：default=summary、max_exposure=raw、内置路径规则、内容规则
/// redact、回退链 redact→structure→summarize→refuse、保留率 0.5。消费点：daemon
/// govern 入口（govern_with_policy）按本策略算输出硬顶，none 档拒绝、其余把 intent
/// 收紧到不超过硬顶（§8.3 模型不能经 intent 绕过外部隔离配置）。
#[derive(Debug, Clone, Serialize)]
pub struct IsolationConfig {
    /// 默认隔离档（intent 为 auto 时采用）。
    pub default: ExposureLevel,
    /// 外部策略硬上限（§8.3）。
    pub max_exposure: ExposureLevel,
    /// 路径规则：命中即把该来源的硬顶收紧到规则 level。
    pub rules: Vec<PathRule>,
    /// 内容规则（§8.11）：命中即走对应回退动作。
    pub content_rules: Vec<ContentRule>,
    /// 脱敏无意义时的降级顺序（§8.6）。
    pub fallback_chain: Vec<FallbackStep>,
    /// 脱敏后保留率下限（低于此值降级）。
    pub redact_min_retention: f32,
}

impl Default for IsolationConfig {
    fn default() -> Self {
        Self {
            default: ExposureLevel::Summary,
            max_exposure: ExposureLevel::Raw,
            rules: vec![
                PathRule { glob: ".env*".to_owned(), level: ExposureLevel::None },
                PathRule { glob: "secrets/**".to_owned(), level: ExposureLevel::None },
                PathRule { glob: "*.log".to_owned(), level: ExposureLevel::Summary },
            ],
            content_rules: vec![
                ContentRule { kind: ContentRuleKind::SecretPattern, level: FallbackStep::Redact },
                ContentRule { kind: ContentRuleKind::PathUsername, level: FallbackStep::Redact },
                ContentRule { kind: ContentRuleKind::HostMetadata, level: FallbackStep::Redact },
            ],
            fallback_chain: vec![
                FallbackStep::Redact,
                FallbackStep::Structure,
                FallbackStep::Summarize,
                FallbackStep::Refuse,
            ],
            redact_min_retention: 0.5,
        }
    }
}

impl IsolationConfig {
    /// 按请求级 isolation 覆盖与硬上限算有效硬顶（§8.3）：只能收紧不能放宽。
    pub fn effective_cap(&self, requested: Option<ExposureLevel>) -> ExposureLevel {
        match requested {
            Some(level) => level.min(self.max_exposure),
            None => self.max_exposure,
        }
    }

    /// 按来源路径匹配内置/追加路径规则，返回最严（level 最小）的命中规则。
    /// 不含 '/' 的模式（如 `.env*` / `*.log`）按文件名匹配，可用于任意目录。
    pub fn path_rule_for(&self, path: &str) -> Option<PathRule> {
        let norm = path.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        self.rules
            .iter()
            .filter(|r| {
                if r.glob.contains('/') {
                    glob_match(&r.glob, &norm)
                } else {
                    glob_match(&r.glob, base)
                }
            })
            .min_by_key(|r| r.level)
            .cloned()
    }
}

/// 极简 glob 匹配：`**` 跨目录任意串，`*` 段内任意串，`?` 单字符，其余按字面。
fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[u8], t: &[u8]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            b'*' if p.get(1) == Some(&b'*') => (0..=t.len()).any(|i| rec(&p[2..], &t[i..])),
            b'*' => {
                for i in 0..=t.len() {
                    if rec(&p[1..], &t[i..]) {
                        return true;
                    }
                    if i < t.len() && t[i] == b'/' {
                        break;
                    }
                }
                false
            }
            b'?' => !t.is_empty() && t[0] != b'/' && rec(&p[1..], &t[1..]),
            c => !t.is_empty() && t[0] == c && rec(&p[1..], &t[1..]),
        }
    }
    rec(pattern.as_bytes(), text.as_bytes())
}

/// 工具输出双阈值截断配置（附录 D）：limits.max_lines / limits.max_bytes。
///
/// 缺省值与 truncate::DEFAULT_MAX_LINES / DEFAULT_MAX_BYTES 一致；
/// 消费点为六工具上下文的 ToolContext.limits（bash / read / search / task）。
#[derive(Debug, Clone, Serialize)]
pub struct LimitsConfig {
    /// 单次工具返回允许的最大行数。
    pub max_lines: usize,
    /// 单次工具返回允许的最大字节数。
    pub max_bytes: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_lines: crate::truncate::DEFAULT_MAX_LINES,
            max_bytes: crate::truncate::DEFAULT_MAX_BYTES,
        }
    }
}

/// 落盘副本存活与配额配置（附录 D）：spill.ttl_days / spill.max_bytes。
///
/// 缺省值与 spill::DEFAULT_SPILL_TTL_DAYS / DEFAULT_SPILL_MAX_BYTES 一致；
/// 消费点为 ToolContext.spill 的 TTL 清理与每工作区配额。
#[derive(Debug, Clone, Serialize)]
pub struct SpillConfig {
    /// 落盘副本存活天数。
    pub ttl_days: u64,
    /// 每工作区落盘总字节上限。
    pub max_bytes: u64,
}

impl Default for SpillConfig {
    fn default() -> Self {
        Self {
            ttl_days: crate::spill::DEFAULT_SPILL_TTL_DAYS,
            max_bytes: crate::spill::DEFAULT_SPILL_MAX_BYTES,
        }
    }
}

/// 写前快照生命周期与数据面配置（附录 D）：snapshot.* 段。
///
/// 消费点：ToolContext.snapshots 的快照 GC（TTL / 每文件保留 / 每工作区上限 /
/// 最短保护期）与创建前 min_free_bytes 空闲检查；data 数据面与 gc_interval_hours
/// 后台巡检留待后续（本期仅保留配置，默认值为 auto / 24h）。
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotConfig {
    /// 数据面策略：auto / reflink / copy（本期未消费，保留 auto）。
    pub data: String,
    /// 创建快照前的空闲空间下限（字节）；低于此值拒绝创建。
    pub min_free_bytes: u64,
    /// 每文件保留最近 N 份。
    pub keep_per_file: usize,
    /// 最短保护期（小时），期内不清理。
    pub min_retention_hours: u64,
    /// 过期时间（天）。
    pub ttl_days: u64,
    /// 每工作区快照总字节上限。
    pub max_bytes: u64,
    /// 后台巡检间隔（小时，本期未消费）。
    pub gc_interval_hours: u64,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            data: "auto".to_owned(),
            min_free_bytes: crate::spill::DEFAULT_SNAPSHOT_MIN_FREE_BYTES,
            keep_per_file: crate::spill::DEFAULT_SNAPSHOT_KEEP_PER_FILE,
            min_retention_hours: crate::spill::DEFAULT_SNAPSHOT_MIN_RETENTION_HOURS,
            ttl_days: crate::spill::DEFAULT_SNAPSHOT_TTL_DAYS,
            max_bytes: crate::spill::DEFAULT_SNAPSHOT_MAX_BYTES,
            gc_interval_hours: 24,
        }
    }
}

/// 日志级别配置（附录 D）：logging.level。
#[derive(Debug, Clone, Serialize)]
pub struct LoggingConfig {
    /// 日志级别：trace / debug / info / warn / error / off。
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "warn".to_owned(),
        }
    }
}

/// 槽位绑定结果（§3.10）：某个槽位由哪个模型承担。
#[derive(Debug, Clone, Serialize)]
pub struct SlotBinding {
    /// 槽位。
    pub slot: SlotKind,
    /// 绑定的模型 id；为空表示该槽位没有模型（§3.8）。
    pub model_id: Option<String>,
    /// 该模型的来源。
    pub source: Option<ModelSource>,
    /// 该模型的运行模式。
    pub mode: Option<SlmMode>,
    /// 该模型的形状适配器。
    pub adapter: Option<Adapter>,
    /// 是否来自 slots 的显式覆盖。
    pub explicit: bool,
}

/// 解析完成的配置：模型清单 + 显式覆盖 + 六个槽位的绑定。
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedConfig {
    /// 模型清单，保持 YAML 声明顺序（§3.10 的自动绑定按此顺序取第一个）。
    pub models: Vec<ModelEntry>,
    /// 显式覆盖的槽位绑定：槽位名 → 模型 id。
    pub slots: BTreeMap<String, String>,
    /// SLM 全局配置。
    pub slm: SlmConfig,
    /// 路径语义配置（§17.2）。
    pub paths: PathsConfig,
    /// 工具输出截断阈值（附录 D）。
    pub limits: LimitsConfig,
    /// 落盘副本存活与配额（附录 D）。
    pub spill: SpillConfig,
    /// 写前快照生命周期与数据面（附录 D）。
    pub snapshot: SnapshotConfig,
    /// 日志级别（附录 D）。
    pub logging: LoggingConfig,
    /// 隔离策略（§8 / 附录 D）。
    pub isolation: IsolationConfig,
    /// 六个槽位的绑定，顺序同 SlotKind::ALL。
    pub bindings: Vec<SlotBinding>,
}

// ---------- 合法取值表 ----------

/// 顶层合法段（§18.9：未知字段一律拒绝）。
const TOP_LEVEL_FIELDS: &str =
    "models / slots / slm / paths / limits / spill / snapshot / logging / isolation";
/// paths 段合法字段。
const PATHS_FIELDS: &str = "allowed_roots / expose_absolute";
/// isolation 段合法字段。
const ISOLATION_FIELDS: &str =
    "default / max_exposure / rules / content_rules / fallback_chain / redact_min_retention";
/// limits 段合法字段。
const LIMITS_FIELDS: &str = "max_lines / max_bytes";
/// spill 段合法字段。
const SPILL_FIELDS: &str = "ttl_days / max_bytes";
/// snapshot 段合法字段。
const SNAPSHOT_FIELDS: &str =
    "data / min_free_bytes / keep_per_file / min_retention_hours / ttl_days / max_bytes / gc_interval_hours";
/// logging 段合法字段。
const LOGGING_FIELDS: &str = "level";
/// 模型配置合法字段。
const MODEL_FIELDS: &str =
    "source / mode / adapter / path / base_url / model / api_key_env / allow_remote / serves";
/// SLM 段合法字段。
const SLM_FIELDS: &str = "mode / probe / disable_after_failures / embedded / http";
/// embedded 段合法字段。
const EMBEDDED_FIELDS: &str = "backend / model_dir / max_resident_models / idle_ttl_seconds";
/// http 段合法字段。
const HTTP_FIELDS: &str = "base_url";
/// 六个槽位名。
const SLOT_NAMES: &str = "classify / rerank / decide / summarize / vision / embedding";
/// source 枚举的合法值。
const SOURCE_VALUES: &str =
    "auto / local / openai / anthropic / responses / systemone / rerank / embedding";
/// mode 枚举的合法值。
const MODE_VALUES: &str = "auto / embedded / http";
/// adapter 枚举的合法值。
const ADAPTER_VALUES: &str = "auto / native / json / text / ordered";
/// probe 枚举的合法值。
const PROBE_VALUES: &str = "first_use / startup / off";

// ---------- 本地化（§18.1b） ----------

/// 英文译文表：中文源文作 msgid（与 locale.rs 同口径），未登记的回退源语言。
/// 只登记用户可见的说明文字；code / field / value 不译（field 是字段路径，value 是实际值）。
const EN_CONFIG_TEXT: &[(&str, &str)] = &[
    // expected：期望的类型与取值集合
    ("合法 YAML", "valid YAML"),
    ("可读的配置文件", "a readable configuration file"),
    ("字符串", "string"),
    ("布尔值", "boolean"),
    ("非负整数", "non-negative integer"),
    (
        "映射（models / slots / slm / paths）",
        "mapping (models / slots / slm / paths)",
    ),
    ("映射（路径语义配置）", "mapping (path semantics config)"),
    ("字符串数组（绝对路径）", "array of absolute paths"),
    ("绝对路径（字符串）", "absolute path (string)"),
    ("绝对路径", "absolute path"),
    (
        "映射（模型 id → 模型配置）",
        "mapping (model id → model config)",
    ),
    ("映射（模型配置）", "mapping (model config)"),
    ("映射（槽位名 → 模型 id）", "mapping (slot name → model id)"),
    ("映射（SLM 全局配置）", "mapping (global SLM config)"),
    ("映射（embedded 配置）", "mapping (embedded config)"),
    ("映射（http 配置）", "mapping (http config)"),
    ("槽位名列表（数组）", "list of slot names (array)"),
    ("槽位名（字符串）", "slot name (string)"),
    ("模型 id（字符串）", "model id (string)"),
    ("本地权重路径", "local weight path"),
    (
        "外部协议接口地址（或在 slm.http.base_url 统一配置）",
        "external protocol endpoint (or set one default in slm.http.base_url)",
    ),
    (
        "mode: http（外部协议只能走 http）",
        "mode: http (external protocols can only run over http)",
    ),
    ("已定义的模型 id", "a defined model id"),
    (
        "loopback 地址（127.0.0.1 / localhost / ::1），或显式 allow_remote: true",
        "a loopback address (127.0.0.1 / localhost / ::1), or an explicit allow_remote: true",
    ),
    // fix：可执行的下一步
    (
        "该配置段本版本尚未实现，请移除该段或等待后续版本",
        "This configuration section is not implemented in this version; remove it or wait for a later version",
    ),
    (
        "该字段不在模型配置的合法字段表内，请检查拼写或移除该字段",
        "This field is not in the legal field set of a model config; check the spelling or remove it",
    ),
    (
        "该字段不在 slm 段的合法字段表内，请检查拼写或移除该字段",
        "This field is not in the legal field set of the slm section; check the spelling or remove it",
    ),
    (
        "该字段不在 embedded 段的合法字段表内，请检查拼写或移除该字段",
        "This field is not in the legal field set of the embedded section; check the spelling or remove it",
    ),
    (
        "该字段不在 http 段的合法字段表内，请检查拼写或移除该字段",
        "This field is not in the legal field set of the http section; check the spelling or remove it",
    ),
    (
        "把槽位名改成六个合法槽位之一",
        "Change the slot name to one of the six legal slots",
    ),
    (
        "槽位名只能是六个合法槽位之一",
        "A slot name must be one of the six legal slots",
    ),
    (
        "在 models 里定义该模型，或把这里改成已存在的模型 id",
        "Define this model under models, or point to an existing model id",
    ),
    (
        "source 为 local 时必须写 path",
        "A local source requires a path",
    ),
    (
        "补 base_url，或在 slm.http.base_url 里给默认地址",
        "Add base_url, or set a default endpoint in slm.http.base_url",
    ),
    (
        "把地址改成本机模型服务；确需远程时显式写 allow_remote: true 并自行承担数据出境风险",
        "Point the address at a local model service; if a remote endpoint is required, set allow_remote: true explicitly and accept the data-exfiltration risk",
    ),
    (
        "embedded 只适用于 source: local；外部协议改用 mode: http",
        "embedded only applies to source: local; use mode: http for external protocols",
    ),
    (
        "导出该环境变量后再启动，或删除 api_key_env 配置",
        "Export this environment variable before starting, or remove the api_key_env setting",
    ),
    (
        "显式写 source（local / openai / anthropic / responses）",
        "Set source explicitly (local / openai / anthropic / responses / systemone)",
    ),
];

/// 前缀规则：中文前缀换成英文前缀，余下部分递归翻译（用于带插值的 fix）。
const EN_CONFIG_PREFIXES: &[(&str, &str)] = &[
    ("把该字段改成", "Change this field to "),
    ("把取值改成合法值之一：", "Use one of the legal values: "),
    ("修正 YAML 语法后重试：", "Fix the YAML syntax and retry: "),
    (
        "确认文件存在且当前用户可读：",
        "Check that the file exists and is readable: ",
    ),
];

/// 按语言取错误文案：精确表 → 前缀规则 → 位置片段；未登记回退源语言中文（§18.1b）。
fn localize_text(lang: Language, text: &str) -> String {
    if lang == Language::Zh {
        return text.to_owned();
    }
    if let Some((_, en)) = EN_CONFIG_TEXT.iter().find(|(zh, _)| *zh == text) {
        return (*en).to_owned();
    }
    for (zh, en) in EN_CONFIG_PREFIXES {
        if let Some(rest) = text.strip_prefix(*zh) {
            return format!("{en}{}", localize_text(lang, rest));
        }
    }
    // 语法错误的位置片段：只出现在这一种文案里，单独翻译。
    if text.contains("（位置：") {
        return text
            .replace("（位置：行 ", " (position: line ")
            .replace(" 列 ", " column ")
            .replace("）", ")");
    }
    text.to_owned()
}

// ---------- 公开入口 ----------

/// 默认配置文件路径：~/.toolgate/config.yaml。
pub fn default_config_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"));
    home.join(".toolgate").join("config.yaml")
}

/// 解析配置文本：语法 / 结构 / 语义三层校验，返回模型清单与槽位绑定（§18.9 / §3.10）。
///
/// origin 是配置来源（通常是文件路径），用于语法错误的字段路径。
pub fn parse_config(text: &str, origin: &str) -> Result<ResolvedConfig, ConfigErrors> {
    // 空文件等价于空配置：无配置文件也能启动（附录 D）。
    let root = if text.trim().is_empty() {
        YamlValue::Null
    } else {
        match noyalib::from_str::<YamlValue>(text) {
            Ok(value) => value,
            Err(err) => {
                return Err(ConfigErrors {
                    items: vec![syntax_error(origin, &err)],
                });
            }
        }
    };

    let mut errors: Vec<ConfigError> = Vec::new();

    let root_map = match root {
        YamlValue::Null => Mapping::new(),
        YamlValue::Mapping(map) => map,
        other => {
            errors.push(type_error(
                origin,
                &other,
                "映射（models / slots / slm / paths）",
            ));
            return Err(ConfigErrors { items: errors });
        }
    };

    // 顶层：只允许 models / slots / slm / paths（§18.9）。
    for (key, _) in root_map.iter() {
        if !matches!(
            key.as_str(),
            "models" | "slots" | "slm" | "paths" | "limits" | "spill" | "snapshot" | "logging"
                | "isolation"
        ) {
            errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field: key.clone(),
                value: None,
                expected: TOP_LEVEL_FIELDS.to_owned(),
                fix: "该配置段本版本尚未实现，请移除该段或等待后续版本".to_owned(),
            });
        }
    }

    let models = parse_models(root_map.get("models"), &mut errors);
    let slots = parse_slots(root_map.get("slots"), &mut errors);
    let slm = parse_slm(root_map.get("slm"), &mut errors);
    let paths = parse_paths(root_map.get("paths"), &mut errors);
    let limits = parse_limits(root_map.get("limits"), &mut errors);
    let spill = parse_spill(root_map.get("spill"), &mut errors);
    let snapshot = parse_snapshot(root_map.get("snapshot"), &mut errors);
    let logging = parse_logging(root_map.get("logging"), &mut errors);
    let isolation = parse_isolation(root_map.get("isolation"), &mut errors);

    // 结构层有错即返回：结构不完整时语义校验只会产生连锁噪音（§18.9 同层一次列全）。
    if !errors.is_empty() {
        return Err(ConfigErrors { items: errors });
    }

    // 语义层：slots 指向的模型必须已定义（§3.10）。
    let model_ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    for (slot_name, model_id) in &slots {
        if !model_ids.contains(&model_id.as_str()) {
            errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_MODEL".to_owned(),
                field: format!("slots.{slot_name}"),
                value: Some(model_id.clone()),
                expected: "已定义的模型 id".to_owned(),
                fix: "在 models 里定义该模型，或把这里改成已存在的模型 id".to_owned(),
            });
        }
    }

    // 语义层：逐模型校验来源、模式、地址与环境变量（§18.1）。
    for entry in &models {
        let prefix = format!("models.{}", entry.id);
        let source = match resolve_source(&entry.config) {
            Ok(source) => Some(source),
            Err(err) => {
                errors.push(fill_model_id(err, &entry.id));
                None
            }
        };

        if source == Some(ModelSource::Local) && entry.config.path.is_none() {
            errors.push(ConfigError {
                code: "CONFIG_MISSING_PATH".to_owned(),
                field: format!("{prefix}.path"),
                value: None,
                expected: "本地权重路径".to_owned(),
                fix: "source 为 local 时必须写 path".to_owned(),
            });
        }

        if source.is_some_and(|source| source.is_external()) {
            let effective_url = entry
                .config
                .base_url
                .clone()
                .or_else(|| slm.http.as_ref().and_then(|h| h.base_url.clone()));
            match effective_url {
                None => errors.push(ConfigError {
                    code: "CONFIG_MISSING_BASE_URL".to_owned(),
                    field: format!("{prefix}.base_url"),
                    value: None,
                    expected: "外部协议接口地址（或在 slm.http.base_url 统一配置）".to_owned(),
                    fix: "补 base_url，或在 slm.http.base_url 里给默认地址".to_owned(),
                }),
                Some(url) => {
                    if !is_loopback(&url) && !entry.config.allow_remote {
                        errors.push(ConfigError {
                            code: "CONFIG_REMOTE_NOT_ALLOWED".to_owned(),
                            field: format!("{prefix}.base_url"),
                            value: Some(url),
                            expected:
                                "loopback 地址（127.0.0.1 / localhost / ::1），或显式 allow_remote: true"
                                    .to_owned(),
                            fix: "把地址改成本机模型服务；确需远程时显式写 allow_remote: true 并自行承担数据出境风险"
                                .to_owned(),
                        });
                    }
                }
            }
        }

        let mode = match resolve_mode(&entry.config, &slm) {
            Ok(mode) => Some(mode),
            Err(err) => {
                errors.push(fill_model_id(err, &entry.id));
                None
            }
        };
        if let (Some(source), Some(mode)) = (source, mode)
            && mode == SlmMode::Embedded
            && source.is_external()
        {
            errors.push(ConfigError {
                code: "CONFIG_MODE_CONFLICT".to_owned(),
                field: format!("{prefix}.mode"),
                value: Some(source.as_str().to_owned()),
                expected: "mode: http（外部协议只能走 http）".to_owned(),
                fix: "embedded 只适用于 source: local；外部协议改用 mode: http".to_owned(),
            });
        }

        if let Some(var_name) = &entry.config.api_key_env
            && std::env::var(var_name.as_str()).is_err()
        {
            errors.push(ConfigError {
                code: "CONFIG_MISSING_ENV".to_owned(),
                field: format!("{prefix}.api_key_env"),
                value: Some(var_name.clone()),
                expected: "已在启动环境中设置的环境变量".to_owned(),
                fix: "导出该环境变量后再启动，或删除 api_key_env 配置".to_owned(),
            });
        }
    }

    if !errors.is_empty() {
        return Err(ConfigErrors { items: errors });
    }

    let bindings = build_bindings(&models, &slots, &slm);
    Ok(ResolvedConfig {
        models,
        slots,
        slm,
        paths,
        limits,
        spill,
        snapshot,
        logging,
        isolation,
        bindings,
    })
}

/// 读取配置文件并解析；文件不存在返回 Ok(None)（无配置文件也能启动，附录 D）。
pub fn load_config(path: &Path) -> Result<Option<ResolvedConfig>, ConfigErrors> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ConfigErrors {
                items: vec![ConfigError {
                    code: "CONFIG_IO".to_owned(),
                    field: path.display().to_string(),
                    value: None,
                    expected: "可读的配置文件".to_owned(),
                    fix: format!("确认文件存在且当前用户可读：{err}"),
                }],
            });
        }
    };
    parse_config(&text, &path.display().to_string()).map(Some)
}

/// 解析模型来源（§18.1）：显式 source 优先；auto 按 path → URL 路径 → host 的顺序保守推断。
///
/// 推断不出来就是配置错误（CONFIG_SOURCE_UNRESOLVED），不做启动探测。
pub fn resolve_source(model: &ModelConfig) -> Result<ModelSource, ConfigError> {
    if let Some(source) = model.source
        && source != ModelSource::Auto
    {
        return Ok(source);
    }
    if model.path.is_some() {
        return Ok(ModelSource::Local);
    }
    if let Some(url) = model.base_url.as_deref()
        && let Some(source) = infer_source_from_url(url)
    {
        return Ok(source);
    }
    Err(ConfigError {
        code: "CONFIG_SOURCE_UNRESOLVED".to_owned(),
        field: "models.<id>.source".to_owned(),
        value: model.base_url.clone(),
        expected: SOURCE_VALUES.to_owned(),
        fix: "显式写 source（见 §18.1 的 source 表；systemone / rerank / embedding 不参与 auto 推断）".to_owned(),
    })
}

/// 解析运行模式（§18.1）：模型级 mode 覆盖 slm.mode；两边都是 auto 时按槽位分档。
///
/// 分档：classify / decide → embedded；rerank / summarize / vision → http。
pub fn resolve_mode(model: &ModelConfig, slm: &SlmConfig) -> Result<SlmMode, ConfigError> {
    let model_mode = model.mode.filter(|m| *m != SlmMode::Auto);
    if model_mode == Some(SlmMode::Embedded) {
        if model.source.is_some_and(|source| source.is_external()) {
            return Err(ConfigError {
                code: "CONFIG_MODE_CONFLICT".to_owned(),
                field: "models.<id>.mode".to_owned(),
                value: model.source.map(|source| source.as_str().to_owned()),
                expected: "mode: http（外部协议只能走 http）".to_owned(),
                fix: "embedded 只适用于 source: local；外部协议改用 mode: http".to_owned(),
            });
        }
        return Ok(SlmMode::Embedded);
    }
    if model_mode == Some(SlmMode::Http) {
        return Ok(SlmMode::Http);
    }
    match slm.mode {
        Some(SlmMode::Embedded) => Ok(SlmMode::Embedded),
        Some(SlmMode::Http) => Ok(SlmMode::Http),
        _ => {
            // 两边都是 auto：按模型 serves 里第一个槽位分档；没有 serves 走 http 默认档。
            let slot_mode = match model.serves.first() {
                Some(slot) => default_mode_for_slot(*slot),
                None => SlmMode::Http,
            };
            // 外部来源只能是 http（§18.1）。
            let effective_source = match model.source {
                Some(source) if source != ModelSource::Auto => Some(source),
                _ => resolve_source(model).ok(),
            };
            if effective_source.is_some_and(|source| source.is_external()) {
                return Ok(SlmMode::Http);
            }
            Ok(slot_mode)
        }
    }
}

/// 解析形状适配器（§3.10）：显式 adapter 优先；auto 时按来源与槽位推断。
pub fn resolve_adapter(model: &ModelConfig, slot: SlotKind, source: ModelSource) -> Adapter {
    if let Some(adapter) = model.adapter
        && adapter != Adapter::Auto
    {
        return adapter;
    }
    match source {
        ModelSource::Local
        | ModelSource::SystemOne
        | ModelSource::Rerank
        | ModelSource::Embedding => Adapter::Native,
        ModelSource::Auto => Adapter::Json,
        ModelSource::Openai | ModelSource::Anthropic | ModelSource::Responses => match slot {
            SlotKind::Classify | SlotKind::Decide => Adapter::Json,
            SlotKind::Summarize | SlotKind::Vision => Adapter::Text,
            SlotKind::Rerank => Adapter::Ordered,
            SlotKind::Embedding => Adapter::Json,
        },
    }
}

// ---------- 内部：结构层解析 ----------

/// 解析 models 段：键是模型 id，值是模型配置映射。
fn parse_models(value: Option<&YamlValue>, errors: &mut Vec<ConfigError>) -> Vec<ModelEntry> {
    let mut models = Vec::new();
    let map = match value {
        None => return models,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("models", other, "映射（模型 id → 模型配置）"));
            return models;
        }
    };
    for (id, model_value) in map.iter() {
        let prefix = format!("models.{id}");
        let config = match model_value {
            YamlValue::Mapping(model_map) => parse_model_config(model_map, &prefix, errors),
            other => {
                errors.push(type_error(&prefix, other, "映射（模型配置）"));
                continue;
            }
        };
        models.push(ModelEntry {
            id: id.clone(),
            config,
        });
    }
    models
}

/// 解析单个模型配置：未知字段拒绝，类型与枚举逐项校验（§18.9）。
fn parse_model_config(map: &Mapping, prefix: &str, errors: &mut Vec<ConfigError>) -> ModelConfig {
    let mut config = ModelConfig {
        source: None,
        mode: None,
        adapter: None,
        path: None,
        base_url: None,
        model: None,
        api_key_env: None,
        allow_remote: false,
        serves: Vec::new(),
    };
    for (key, value) in map.iter() {
        let field = format!("{prefix}.{key}");
        match key.as_str() {
            "source" => {
                config.source =
                    parse_enum_value(value, &field, SOURCE_VALUES, parse_source_value, errors);
            }
            "mode" => {
                config.mode =
                    parse_enum_value(value, &field, MODE_VALUES, parse_mode_value, errors);
            }
            "adapter" => {
                config.adapter =
                    parse_enum_value(value, &field, ADAPTER_VALUES, parse_adapter_value, errors);
            }
            "path" => config.path = as_string(value, &field, errors),
            "base_url" => config.base_url = as_string(value, &field, errors),
            "model" => config.model = as_string(value, &field, errors),
            "api_key_env" => config.api_key_env = as_string(value, &field, errors),
            "allow_remote" => config.allow_remote = as_bool(value, &field, errors),
            "serves" => config.serves = parse_serves(value, &field, errors),
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(value)),
                expected: MODEL_FIELDS.to_owned(),
                fix: "该字段不在模型配置的合法字段表内，请检查拼写或移除该字段".to_owned(),
            }),
        }
    }
    config
}

/// 解析 serves：必须是槽位名数组（§3.10）。
fn parse_serves(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Vec<SlotKind> {
    let items = match value {
        YamlValue::Sequence(items) => items,
        other => {
            errors.push(type_error(field, other, "槽位名列表（数组）"));
            return Vec::new();
        }
    };
    let mut serves = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let item_field = format!("{field}[{index}]");
        match item {
            YamlValue::String(name) => match slot_from_str(name) {
                Some(slot) => serves.push(slot),
                None => errors.push(ConfigError {
                    code: "CONFIG_UNKNOWN_SLOT".to_owned(),
                    field: item_field,
                    value: Some(name.clone()),
                    expected: SLOT_NAMES.to_owned(),
                    fix: "把槽位名改成六个合法槽位之一".to_owned(),
                }),
            },
            other => errors.push(type_error(&item_field, other, "槽位名（字符串）")),
        }
    }
    serves
}

/// 解析 slots 段：槽位名 → 模型 id。
fn parse_slots(
    value: Option<&YamlValue>,
    errors: &mut Vec<ConfigError>,
) -> BTreeMap<String, String> {
    let mut slots = BTreeMap::new();
    let map = match value {
        None => return slots,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("slots", other, "映射（槽位名 → 模型 id）"));
            return slots;
        }
    };
    for (name, model_id) in map.iter() {
        let field = format!("slots.{name}");
        if slot_from_str(name).is_none() {
            errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_SLOT".to_owned(),
                field,
                value: Some(name.clone()),
                expected: SLOT_NAMES.to_owned(),
                fix: "槽位名只能是六个合法槽位之一".to_owned(),
            });
            continue;
        }
        match model_id {
            YamlValue::String(id) => {
                slots.insert(name.clone(), id.clone());
            }
            other => errors.push(type_error(&field, other, "模型 id（字符串）")),
        }
    }
    slots
}

/// 解析 slm 段。
fn parse_slm(value: Option<&YamlValue>, errors: &mut Vec<ConfigError>) -> SlmConfig {
    let empty = SlmConfig {
        mode: None,
        probe: None,
        disable_after_failures: None,
        embedded: None,
        http: None,
    };
    let map = match value {
        None => return empty,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("slm", other, "映射（SLM 全局配置）"));
            return empty;
        }
    };
    let mut slm = empty;
    for (key, value) in map.iter() {
        let field = format!("slm.{key}");
        match key.as_str() {
            "mode" => {
                slm.mode = parse_enum_value(value, &field, MODE_VALUES, parse_mode_value, errors)
            }
            "probe" => {
                slm.probe = parse_enum_value(value, &field, PROBE_VALUES, parse_probe_value, errors)
            }
            "disable_after_failures" => slm.disable_after_failures = as_u32(value, &field, errors),
            "embedded" => slm.embedded = parse_embedded(value, &field, errors),
            "http" => slm.http = parse_http(value, &field, errors),
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(value)),
                expected: SLM_FIELDS.to_owned(),
                fix: "该字段不在 slm 段的合法字段表内，请检查拼写或移除该字段".to_owned(),
            }),
        }
    }
    slm
}

/// 解析 slm.embedded 段。
fn parse_embedded(
    value: &YamlValue,
    field: &str,
    errors: &mut Vec<ConfigError>,
) -> Option<EmbeddedConfig> {
    let map = match value {
        YamlValue::Mapping(map) => map,
        other => {
            errors.push(type_error(field, other, "映射（embedded 配置）"));
            return None;
        }
    };
    let mut embedded = EmbeddedConfig {
        backend: None,
        model_dir: None,
        max_resident_models: None,
        idle_ttl_seconds: None,
    };
    for (key, item) in map.iter() {
        let item_field = format!("{field}.{key}");
        match key.as_str() {
            "backend" => embedded.backend = as_string(item, &item_field, errors),
            "model_dir" => embedded.model_dir = as_string(item, &item_field, errors),
            "max_resident_models" => {
                embedded.max_resident_models = as_usize(item, &item_field, errors)
            }
            "idle_ttl_seconds" => embedded.idle_ttl_seconds = as_u64(item, &item_field, errors),
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field: item_field,
                value: Some(value_repr(item)),
                expected: EMBEDDED_FIELDS.to_owned(),
                fix: "该字段不在 embedded 段的合法字段表内，请检查拼写或移除该字段".to_owned(),
            }),
        }
    }
    Some(embedded)
}

/// 解析 slm.http 段。
fn parse_http(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<HttpConfig> {
    let map = match value {
        YamlValue::Mapping(map) => map,
        other => {
            errors.push(type_error(field, other, "映射（http 配置）"));
            return None;
        }
    };
    let mut http = HttpConfig { base_url: None };
    for (key, item) in map.iter() {
        let item_field = format!("{field}.{key}");
        match key.as_str() {
            "base_url" => http.base_url = as_string(item, &item_field, errors),
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field: item_field,
                value: Some(value_repr(item)),
                expected: HTTP_FIELDS.to_owned(),
                fix: "该字段不在 http 段的合法字段表内，请检查拼写或移除该字段".to_owned(),
            }),
        }
    }
    Some(http)
}

/// 解析 paths 段：allowed_roots（§17.2）与 expose_absolute（§8.11）。
fn parse_paths(value: Option<&YamlValue>, errors: &mut Vec<ConfigError>) -> PathsConfig {
    let mut paths = PathsConfig::default();
    let map = match value {
        None => return paths,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("paths", other, "映射（路径语义配置）"));
            return paths;
        }
    };
    for (key, item) in map.iter() {
        let field = format!("paths.{key}");
        match key.as_str() {
            "allowed_roots" => {
                let items = match item {
                    YamlValue::Sequence(items) => items,
                    other => {
                        errors.push(type_error(&field, other, "字符串数组（绝对路径）"));
                        continue;
                    }
                };
                for (index, entry) in items.iter().enumerate() {
                    let item_field = format!("{field}[{index}]");
                    match entry {
                        YamlValue::String(path) if Path::new(path).is_absolute() => {
                            paths.allowed_roots.push(path.clone());
                        }
                        YamlValue::String(path) => errors.push(ConfigError {
                            code: "CONFIG_INVALID_VALUE".to_owned(),
                            field: item_field,
                            value: Some(path.clone()),
                            expected: "绝对路径".to_owned(),
                            fix: "允许根必须写绝对路径：相对路径会随启动目录漂移，不构成稳定边界"
                                .to_owned(),
                        }),
                        other => errors.push(type_error(&item_field, other, "绝对路径（字符串）")),
                    }
                }
            }
            "expose_absolute" => match item {
                YamlValue::Bool(v) => paths.expose_absolute = *v,
                other => errors.push(type_error(&field, other, "布尔（true / false）")),
            },
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(item)),
                expected: PATHS_FIELDS.to_owned(),
                fix: "该字段不在 paths 段的合法字段表内，请检查拼写或移除".to_owned(),
            }),
        }
    }
    paths
}

/// 解析 limits 段（附录 D）：只允许 max_lines / max_bytes 两个正整数阈值。
fn parse_limits(value: Option<&YamlValue>, errors: &mut Vec<ConfigError>) -> LimitsConfig {
    let mut limits = LimitsConfig::default();
    let map = match value {
        None => return limits,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("limits", other, "映射（截断阈值配置）"));
            return limits;
        }
    };
    for (key, item) in map.iter() {
        let field = format!("limits.{key}");
        match key.as_str() {
            "max_lines" => {
                if let Some(v) = positive_usize(item, &field, errors) {
                    limits.max_lines = v;
                }
            }
            "max_bytes" => {
                if let Some(v) = positive_usize(item, &field, errors) {
                    limits.max_bytes = v;
                }
            }
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(item)),
                expected: LIMITS_FIELDS.to_owned(),
                fix: "该字段不在 limits 段的合法字段表内，请检查拼写或移除".to_owned(),
            }),
        }
    }
    limits
}

/// 取正整数阈值：非数字报类型错，0 报取值错。
fn positive_usize(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<usize> {
    let v = as_u64(value, field, errors)?;
    if v == 0 {
        errors.push(ConfigError {
            code: "CONFIG_INVALID_VALUE".to_owned(),
            field: field.to_owned(),
            value: Some(value_repr(value)),
            expected: "正整数".to_owned(),
            fix: "截断阈值必须为正整数".to_owned(),
        });
        return None;
    }
    Some(v as usize)
}

/// 解析 isolation 段（§8 / 附录 D）：default / max_exposure / rules /
/// content_rules / fallback_chain / redact_min_retention。
fn parse_isolation(value: Option<&YamlValue>, errors: &mut Vec<ConfigError>) -> IsolationConfig {
    let mut iso = IsolationConfig::default();
    let map = match value {
        None => return iso,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("isolation", other, "映射（隔离策略配置）"));
            return iso;
        }
    };
    for (key, item) in map.iter() {
        let field = format!("isolation.{key}");
        match key.as_str() {
            "default" => {
                if let Some(l) = exposure_level(item, &field, errors) {
                    iso.default = l;
                }
            }
            "max_exposure" => {
                if let Some(l) = exposure_level(item, &field, errors) {
                    iso.max_exposure = l;
                }
            }
            "rules" => {
                if let Some(rules) = parse_path_rules(item, &field, errors) {
                    iso.rules = rules;
                }
            }
            "content_rules" => {
                if let Some(rules) = parse_content_rules(item, &field, errors) {
                    iso.content_rules = rules;
                }
            }
            "fallback_chain" => {
                if let Some(chain) = parse_fallback_chain(item, &field, errors) {
                    iso.fallback_chain = chain;
                }
            }
            "redact_min_retention" => {
                if let Some(v) = parse_ratio(item, &field, errors) {
                    iso.redact_min_retention = v;
                }
            }
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(item)),
                expected: ISOLATION_FIELDS.to_owned(),
                fix: "该字段不在 isolation 段的合法字段表内，请检查拼写或移除".to_owned(),
            }),
        }
    }
    iso
}

/// 解析隔离档字符串：none / understand / summary / raw。
fn exposure_level(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<ExposureLevel> {
    match value {
        YamlValue::String(s) => match s.as_str() {
            "none" => Some(ExposureLevel::None),
            "understand" => Some(ExposureLevel::Understand),
            "summary" => Some(ExposureLevel::Summary),
            "raw" => Some(ExposureLevel::Raw),
            other => {
                errors.push(ConfigError {
                    code: "CONFIG_INVALID_VALUE".to_owned(),
                    field: field.to_owned(),
                    value: Some(other.to_owned()),
                    expected: "隔离档 none / understand / summary / raw".to_owned(),
                    fix: "改为四档之一".to_owned(),
                });
                None
            }
        },
        other => {
            errors.push(type_error(field, other, "隔离档字符串"));
            None
        }
    }
}

/// 解析回退动作字符串：redact / structure / summarize / refuse。
fn fallback_step(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<FallbackStep> {
    match value {
        YamlValue::String(s) => match s.as_str() {
            "redact" => Some(FallbackStep::Redact),
            "structure" => Some(FallbackStep::Structure),
            "summarize" => Some(FallbackStep::Summarize),
            "refuse" => Some(FallbackStep::Refuse),
            other => {
                errors.push(ConfigError {
                    code: "CONFIG_INVALID_VALUE".to_owned(),
                    field: field.to_owned(),
                    value: Some(other.to_owned()),
                    expected: "回退动作 redact / structure / summarize / refuse".to_owned(),
                    fix: "改为四档之一".to_owned(),
                });
                None
            }
        },
        other => {
            errors.push(type_error(field, other, "回退动作字符串"));
            None
        }
    }
}

/// 解析内容规则类型字符串。
fn content_rule_kind(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<ContentRuleKind> {
    match value {
        YamlValue::String(s) => match s.as_str() {
            "secret_pattern" => Some(ContentRuleKind::SecretPattern),
            "sensitive_path_mention" => Some(ContentRuleKind::SensitivePathMention),
            "path_username" => Some(ContentRuleKind::PathUsername),
            "host_metadata" => Some(ContentRuleKind::HostMetadata),
            "high_entropy" => Some(ContentRuleKind::HighEntropy),
            other => {
                errors.push(ConfigError {
                    code: "CONFIG_INVALID_VALUE".to_owned(),
                    field: field.to_owned(),
                    value: Some(other.to_owned()),
                    expected: "内容规则 secret_pattern / sensitive_path_mention / path_username / host_metadata / high_entropy".to_owned(),
                    fix: "改为上述取值之一".to_owned(),
                });
                None
            }
        },
        other => {
            errors.push(type_error(field, other, "内容规则字符串"));
            None
        }
    }
}

/// 解析 rules：路径 glob 列表，每项 {glob, level}。
fn parse_path_rules(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<Vec<PathRule>> {
    let seq = match value {
        YamlValue::Sequence(seq) => seq,
        other => {
            errors.push(type_error(field, other, "规则数组（每项 {glob, level}）"));
            return None;
        }
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let f = format!("{field}[{i}]");
        let m = match item {
            YamlValue::Mapping(m) => m,
            other => {
                errors.push(type_error(&f, other, "映射 {glob, level}"));
                continue;
            }
        };
        let glob = match m.get("glob") {
            Some(YamlValue::String(g)) => g.clone(),
            Some(other) => {
                errors.push(type_error(&format!("{f}.glob"), other, "字符串"));
                continue;
            }
            None => {
                errors.push(ConfigError {
                    code: "CONFIG_MISSING_FIELD".to_owned(),
                    field: format!("{f}.glob"),
                    value: None,
                    expected: "路径 glob 字符串".to_owned(),
                    fix: "补 glob 字段".to_owned(),
                });
                continue;
            }
        };
        let level = match m.get("level").and_then(|v| exposure_level(v, &format!("{f}.level"), errors)) {
            Some(l) => l,
            None => continue,
        };
        out.push(PathRule { glob, level });
    }
    Some(out)
}

/// 解析 content_rules：每项 {kind, level}。
fn parse_content_rules(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<Vec<ContentRule>> {
    let seq = match value {
        YamlValue::Sequence(seq) => seq,
        other => {
            errors.push(type_error(field, other, "规则数组（每项 {kind, level}）"));
            return None;
        }
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let f = format!("{field}[{i}]");
        let m = match item {
            YamlValue::Mapping(m) => m,
            other => {
                errors.push(type_error(&f, other, "映射 {kind, level}"));
                continue;
            }
        };
        let kind = match m.get("kind").and_then(|v| content_rule_kind(v, &format!("{f}.kind"), errors)) {
            Some(k) => k,
            None => continue,
        };
        let level = match m.get("level").and_then(|v| fallback_step(v, &format!("{f}.level"), errors)) {
            Some(l) => l,
            None => continue,
        };
        out.push(ContentRule { kind, level });
    }
    Some(out)
}

/// 解析 fallback_chain：回退动作字符串数组。
fn parse_fallback_chain(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<Vec<FallbackStep>> {
    let seq = match value {
        YamlValue::Sequence(seq) => seq,
        other => {
            errors.push(type_error(field, other, "回退动作数组"));
            return None;
        }
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        if let Some(step) = fallback_step(item, &format!("{field}[{i}]"), errors) {
            out.push(step);
        }
    }
    Some(out)
}

/// 解析保留率：0.0–1.0 之间的小数。
fn parse_ratio(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<f32> {
    match value {
        YamlValue::Number(n) => {
            let v = n.as_f64();
            if (0.0..=1.0).contains(&v) {
                Some(v as f32)
            } else {
                errors.push(ConfigError {
                    code: "CONFIG_INVALID_VALUE".to_owned(),
                    field: field.to_owned(),
                    value: Some(value_repr(value)),
                    expected: "0.0–1.0 之间的小数".to_owned(),
                    fix: "把保留率限定在 0.0 到 1.0 之间".to_owned(),
                });
                None
            }
        }
        other => {
            errors.push(type_error(field, other, "0.0–1.0 的小数"));
            None
        }
    }
}

/// 解析 spill 段（附录 D）：只允许 ttl_days / max_bytes 两个正整数。
fn parse_spill(value: Option<&YamlValue>, errors: &mut Vec<ConfigError>) -> SpillConfig {
    let mut spill = SpillConfig::default();
    let map = match value {
        None => return spill,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("spill", other, "映射（落盘副本配置）"));
            return spill;
        }
    };
    for (key, item) in map.iter() {
        let field = format!("spill.{key}");
        match key.as_str() {
            "ttl_days" => {
                if let Some(v) = positive_u64(item, &field, errors) {
                    spill.ttl_days = v;
                }
            }
            "max_bytes" => {
                if let Some(v) = positive_u64(item, &field, errors) {
                    spill.max_bytes = v;
                }
            }
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(item)),
                expected: SPILL_FIELDS.to_owned(),
                fix: "该字段不在 spill 段的合法字段表内，请检查拼写或移除".to_owned(),
            }),
        }
    }
    spill
}

/// 解析 snapshot 段（附录 D）：数据面、空闲下限与快照 GC 生命周期参数。
fn parse_snapshot(value: Option<&YamlValue>, errors: &mut Vec<ConfigError>) -> SnapshotConfig {
    let mut snap = SnapshotConfig::default();
    let map = match value {
        None => return snap,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("snapshot", other, "映射（写前快照配置）"));
            return snap;
        }
    };
    for (key, item) in map.iter() {
        let field = format!("snapshot.{key}");
        match key.as_str() {
            "data" => match item.as_str() {
                Some(v @ ("auto" | "reflink" | "copy")) => snap.data = v.to_owned(),
                _ => errors.push(ConfigError {
                    code: "CONFIG_INVALID_VALUE".to_owned(),
                    field,
                    value: Some(value_repr(item)),
                    expected: "auto / reflink / copy".to_owned(),
                    fix: "snapshot.data 只能是 auto、reflink 或 copy".to_owned(),
                }),
            },
            "min_free_bytes" => {
                if let Some(v) = positive_u64(item, &field, errors) {
                    snap.min_free_bytes = v;
                }
            }
            "keep_per_file" => {
                if let Some(v) = positive_usize(item, &field, errors) {
                    snap.keep_per_file = v;
                }
            }
            "min_retention_hours" => {
                if let Some(v) = positive_u64(item, &field, errors) {
                    snap.min_retention_hours = v;
                }
            }
            "ttl_days" => {
                if let Some(v) = positive_u64(item, &field, errors) {
                    snap.ttl_days = v;
                }
            }
            "max_bytes" => {
                if let Some(v) = positive_u64(item, &field, errors) {
                    snap.max_bytes = v;
                }
            }
            "gc_interval_hours" => {
                if let Some(v) = positive_u64(item, &field, errors) {
                    snap.gc_interval_hours = v;
                }
            }
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(item)),
                expected: SNAPSHOT_FIELDS.to_owned(),
                fix: "该字段不在 snapshot 段的合法字段表内，请检查拼写或移除".to_owned(),
            }),
        }
    }
    snap
}

/// 解析 logging 段（附录 D）：只允许 level，取值限定为常见级别。
fn parse_logging(value: Option<&YamlValue>, errors: &mut Vec<ConfigError>) -> LoggingConfig {
    let mut logging = LoggingConfig::default();
    let map = match value {
        None => return logging,
        Some(YamlValue::Mapping(map)) => map,
        Some(other) => {
            errors.push(type_error("logging", other, "映射（日志配置）"));
            return logging;
        }
    };
    for (key, item) in map.iter() {
        let field = format!("logging.{key}");
        match key.as_str() {
            "level" => match item.as_str() {
                Some(v @ ("trace" | "debug" | "info" | "warn" | "error" | "off")) => {
                    logging.level = v.to_owned();
                }
                _ => errors.push(ConfigError {
                    code: "CONFIG_INVALID_VALUE".to_owned(),
                    field,
                    value: Some(value_repr(item)),
                    expected: "trace / debug / info / warn / error / off".to_owned(),
                    fix: "logging.level 只能是 trace、debug、info、warn、error 或 off".to_owned(),
                }),
            },
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(item)),
                expected: LOGGING_FIELDS.to_owned(),
                fix: "该字段不在 logging 段的合法字段表内，请检查拼写或移除".to_owned(),
            }),
        }
    }
    logging
}

/// 取正整数（u64）：非数字报类型错，0 报取值错。
fn positive_u64(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<u64> {
    let v = as_u64(value, field, errors)?;
    if v == 0 {
        errors.push(ConfigError {
            code: "CONFIG_INVALID_VALUE".to_owned(),
            field: field.to_owned(),
            value: Some(value_repr(value)),
            expected: "正整数".to_owned(),
            fix: "存活天数与配额必须为正整数".to_owned(),
        });
        return None;
    }
    Some(v)
}

// ---------- 内部：绑定与取值辅助 ----------

/// 解析六个槽位的绑定（§3.10）：显式覆盖优先；否则 `source: systemone` 的模型优先，
/// 没有再按 models 声明顺序取第一个 serves 命中的模型（§18.1「能用 systemone 就用它」）。
fn build_bindings(
    models: &[ModelEntry],
    slots: &BTreeMap<String, String>,
    slm: &SlmConfig,
) -> Vec<SlotBinding> {
    let mut bindings = Vec::new();
    for slot in SlotKind::ALL {
        let explicit_model = slots.get(slot.as_str());
        let (model_id, explicit) = match explicit_model {
            Some(model_id) => (Some(model_id.clone()), true),
            None => {
                let candidates: Vec<&ModelEntry> = models
                    .iter()
                    .filter(|entry| entry.config.serves.contains(&slot))
                    .collect();
                let chosen = candidates
                    .iter()
                    .find(|entry| matches!(entry.config.source, Some(ModelSource::SystemOne)))
                    .copied()
                    .or_else(|| candidates.first().copied());
                (chosen.map(|entry| entry.id.clone()), false)
            }
        };
        let entry = model_id
            .as_ref()
            .and_then(|id| models.iter().find(|entry| &entry.id == id));
        let (source, mode, adapter) = match entry {
            Some(entry) => {
                let source = resolve_source(&entry.config).ok();
                let mode = resolve_mode(&entry.config, slm).ok();
                let adapter = source.map(|source| resolve_adapter(&entry.config, slot, source));
                (source, mode, adapter)
            }
            None => (None, None, None),
        };
        bindings.push(SlotBinding {
            slot,
            model_id,
            source,
            mode,
            adapter,
            explicit,
        });
    }
    bindings
}

/// 槽位名 → 槽位。
fn slot_from_str(name: &str) -> Option<SlotKind> {
    SlotKind::ALL.into_iter().find(|slot| slot.as_str() == name)
}

/// auto 分档：classify / decide 走 embedded，其余走 http（§18.1）。
fn default_mode_for_slot(slot: SlotKind) -> SlmMode {
    match slot {
        SlotKind::Classify | SlotKind::Decide => SlmMode::Embedded,
        SlotKind::Rerank | SlotKind::Summarize | SlotKind::Vision | SlotKind::Embedding => {
            SlmMode::Http
        }
    }
}

/// 从 base_url 保守推断协议（§18.1）：路径优先，其次 host；都不命中就是推断失败。
fn infer_source_from_url(url: &str) -> Option<ModelSource> {
    let path = url_path(url);
    if path.contains("/chat/completions") {
        return Some(ModelSource::Openai);
    }
    if path.contains("/messages") {
        return Some(ModelSource::Anthropic);
    }
    if path.contains("/responses") {
        return Some(ModelSource::Responses);
    }
    let host = url_host(url);
    if host == "api.openai.com" {
        return Some(ModelSource::Openai);
    }
    if host == "api.anthropic.com" {
        return Some(ModelSource::Anthropic);
    }
    None
}

/// 取 URL 的 path 部分（去掉 query 与 fragment）。
fn url_path(url: &str) -> &str {
    let rest = match url.find("://") {
        Some(index) => &url[index + 3..],
        None => url,
    };
    let start = rest.find('/').unwrap_or(rest.len());
    rest[start..].split(['?', '#']).next().unwrap_or("")
}

/// 取 URL 的 host 部分（去掉 userinfo 与端口，IPv6 保留方括号）。
fn url_host(url: &str) -> String {
    let rest = match url.find("://") {
        Some(index) => &url[index + 3..],
        None => url,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let authority = match authority.rfind('@') {
        Some(index) => &authority[index + 1..],
        None => authority,
    };
    if let Some(close) = authority.find(']') {
        return authority[..=close].to_ascii_lowercase();
    }
    let host = match authority.rfind(':') {
        Some(index) => &authority[..index],
        None => authority,
    };
    host.to_ascii_lowercase()
}

/// 是否 loopback 地址（§18.1）：外部来源默认只接受本机地址。
fn is_loopback(url: &str) -> bool {
    let host = url_host(url);
    let host = host.trim_matches(|c| c == '[' || c == ']');
    host == "127.0.0.1" || host == "localhost" || host == "::1"
}

/// 把错误里的模型 id 占位符补成真实 id（<id> → 实际 id）。
fn fill_model_id(mut error: ConfigError, model_id: &str) -> ConfigError {
    error.field = error.field.replace("<id>", model_id);
    error
}

/// 语法错误：带上行列位置（§18.9「报文件:行:列」）。
fn syntax_error(origin: &str, err: &noyalib::Error) -> ConfigError {
    let message = err.to_string();
    let position = match err.location() {
        Some(location) => format!("行 {} 列 {}", location.line(), location.column()),
        None => "位置未知（行号与列号不可用）".to_owned(),
    };
    ConfigError {
        code: "CONFIG_SYNTAX".to_owned(),
        field: origin.to_owned(),
        value: Some(message.clone()),
        expected: "合法 YAML".to_owned(),
        fix: format!("修正 YAML 语法后重试：{message}（位置：{position}）"),
    }
}

/// 类型错误：回显实际值与期望类型。
fn type_error(field: &str, value: &YamlValue, expected: &str) -> ConfigError {
    ConfigError {
        code: "CONFIG_TYPE".to_owned(),
        field: field.to_owned(),
        value: Some(value_repr(value)),
        expected: expected.to_owned(),
        fix: format!("把该字段改成{expected}"),
    }
}

/// 取字符串字段。
fn as_string(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<String> {
    match value {
        YamlValue::String(text) => Some(text.clone()),
        other => {
            errors.push(type_error(field, other, "字符串"));
            None
        }
    }
}

/// 取布尔字段。
fn as_bool(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> bool {
    match value {
        YamlValue::Bool(flag) => *flag,
        other => {
            errors.push(type_error(field, other, "布尔值"));
            false
        }
    }
}

/// 取非负整数字段（u32）。
fn as_u32(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<u32> {
    match value {
        YamlValue::Number(number) => match number.as_u64() {
            Some(number) => u32::try_from(number).ok(),
            None => {
                errors.push(type_error(field, value, "非负整数"));
                None
            }
        },
        other => {
            errors.push(type_error(field, other, "非负整数"));
            None
        }
    }
}

/// 取非负整数字段（usize）。
fn as_usize(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<usize> {
    match value {
        YamlValue::Number(number) => match number.as_u64() {
            Some(number) => usize::try_from(number).ok(),
            None => {
                errors.push(type_error(field, value, "非负整数"));
                None
            }
        },
        other => {
            errors.push(type_error(field, other, "非负整数"));
            None
        }
    }
}

/// 取非负整数字段（u64）。
fn as_u64(value: &YamlValue, field: &str, errors: &mut Vec<ConfigError>) -> Option<u64> {
    match value {
        YamlValue::Number(number) => number.as_u64(),
        other => {
            errors.push(type_error(field, other, "非负整数"));
            None
        }
    }
}

/// 取枚举字段：非字符串报类型错，非法取值报枚举错。
fn parse_enum_value<T: Copy>(
    value: &YamlValue,
    field: &str,
    expected: &str,
    parse: fn(&str) -> Option<T>,
    errors: &mut Vec<ConfigError>,
) -> Option<T> {
    match value {
        YamlValue::String(text) => match parse(text) {
            Some(parsed) => Some(parsed),
            None => {
                errors.push(ConfigError {
                    code: "CONFIG_ENUM".to_owned(),
                    field: field.to_owned(),
                    value: Some(text.clone()),
                    expected: expected.to_owned(),
                    fix: format!("把取值改成合法值之一：{expected}"),
                });
                None
            }
        },
        other => {
            errors.push(type_error(field, other, "字符串"));
            None
        }
    }
}

/// 解析 source 取值。
fn parse_source_value(text: &str) -> Option<ModelSource> {
    match text {
        "auto" => Some(ModelSource::Auto),
        "local" => Some(ModelSource::Local),
        "openai" => Some(ModelSource::Openai),
        "anthropic" => Some(ModelSource::Anthropic),
        "responses" => Some(ModelSource::Responses),
        "systemone" => Some(ModelSource::SystemOne),
        "rerank" => Some(ModelSource::Rerank),
        "embedding" => Some(ModelSource::Embedding),
        _ => None,
    }
}

/// 解析 mode 取值。
fn parse_mode_value(text: &str) -> Option<SlmMode> {
    match text {
        "auto" => Some(SlmMode::Auto),
        "embedded" => Some(SlmMode::Embedded),
        "http" => Some(SlmMode::Http),
        _ => None,
    }
}

/// 解析 adapter 取值。
fn parse_adapter_value(text: &str) -> Option<Adapter> {
    match text {
        "auto" => Some(Adapter::Auto),
        "native" => Some(Adapter::Native),
        "json" => Some(Adapter::Json),
        "text" => Some(Adapter::Text),
        "ordered" => Some(Adapter::Ordered),
        _ => None,
    }
}

/// 解析 probe 取值。
fn parse_probe_value(text: &str) -> Option<ProbeTiming> {
    match text {
        "first_use" => Some(ProbeTiming::FirstUse),
        "startup" => Some(ProbeTiming::Startup),
        "off" => Some(ProbeTiming::Off),
        _ => None,
    }
}

/// 值的可读形态，用于错误回显。
fn value_repr(value: &YamlValue) -> String {
    match value {
        YamlValue::Null => "null".to_owned(),
        YamlValue::Bool(flag) => flag.to_string(),
        YamlValue::Number(number) => number.to_string(),
        YamlValue::String(text) => text.clone(),
        YamlValue::Sequence(items) => format!("[{} 项]", items.len()),
        YamlValue::Mapping(map) => format!("{{{} 个键}}", map.len()),
        YamlValue::Tagged(_) => "带标签的值".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(config: &ResolvedConfig, slot: SlotKind) -> &SlotBinding {
        config
            .bindings
            .iter()
            .find(|binding| binding.slot == slot)
            .expect("六个槽位都应有绑定")
    }

    fn errors_of(text: &str) -> Vec<ConfigError> {
        parse_config(text, "test.yaml").expect_err("应当报错").items
    }

    /// 附录 D：limits 段缺省取 truncate 的 2000 行 / 51200 字节。
    #[test]
    fn limits_default_when_absent() {
        let config = parse_config("", "test.yaml").expect("空配置应解析成功");
        assert_eq!(config.limits.max_lines, 2000);
        assert_eq!(config.limits.max_bytes, 51200);
    }

    /// 附录 D：limits 段自定义阈值生效（不再按未知段拒绝）。
    #[test]
    fn limits_overrides_applied() {
        let config = parse_config(
            r#"limits:
  max_lines: 100
  max_bytes: 4096
"#,
            "test.yaml",
        )
        .expect("limits 段应被接受");
        assert_eq!(config.limits.max_lines, 100);
        assert_eq!(config.limits.max_bytes, 4096);
    }

    /// §18.9：limits 段未知字段拒绝并回显合法字段表。
    #[test]
    fn limits_unknown_field_rejected() {
        let errors = errors_of("limits:\n  foo: 1\n");
        assert!(errors.iter().any(|e| e.code == "CONFIG_UNKNOWN_FIELD"
            && e.field == "limits.foo"
            && e.expected.contains("max_lines")));
    }

    /// 附录 D：阈值必须为正整数，0 报取值错。
    #[test]
    fn limits_zero_rejected() {
        let errors = errors_of("limits:\n  max_lines: 0\n");
        assert!(errors
            .iter()
            .any(|e| e.code == "CONFIG_INVALID_VALUE" && e.field == "limits.max_lines"));
    }

    /// 附录 D：阈值非数字报类型错。
    #[test]
    fn limits_non_integer_rejected() {
        let errors = errors_of("limits:\n  max_bytes: abc\n");
        assert!(errors
            .iter()
            .any(|e| e.code == "CONFIG_TYPE" && e.field == "limits.max_bytes"));
    }

    /// 附录 D：isolation 段缺省取 default=summary / max_exposure=raw / 内置规则。
    #[test]
    fn isolation_default_when_absent() {
        let config = parse_config("", "test.yaml").expect("空配置应解析成功");
        assert_eq!(config.isolation.default, ExposureLevel::Summary);
        assert_eq!(config.isolation.max_exposure, ExposureLevel::Raw);
        assert!((config.isolation.redact_min_retention - 0.5).abs() < 1e-6);
        assert_eq!(config.isolation.fallback_chain.len(), 4);
        assert!(config
            .isolation
            .rules
            .iter()
            .any(|r| r.glob == ".env*" && r.level == ExposureLevel::None));
    }

    /// §8.3：isolation 段自定义档位、规则、内容规则与回退链生效（不再按未知段拒绝）。
    #[test]
    fn isolation_overrides_applied() {
        let config = parse_config(
            r#"isolation:
  default: understand
  max_exposure: summary
  rules:
    - glob: "db/**"
      level: none
  content_rules:
    - kind: high_entropy
      level: redact
  fallback_chain: [redact, refuse]
  redact_min_retention: 0.8
"#,
            "test.yaml",
        )
        .expect("isolation 段应被接受");
        assert_eq!(config.isolation.default, ExposureLevel::Understand);
        assert_eq!(config.isolation.max_exposure, ExposureLevel::Summary);
        assert_eq!(config.isolation.rules.len(), 1);
        assert_eq!(
            config.isolation.content_rules[0].kind,
            ContentRuleKind::HighEntropy
        );
        assert_eq!(
            config.isolation.fallback_chain,
            vec![FallbackStep::Redact, FallbackStep::Refuse]
        );
        assert!((config.isolation.redact_min_retention - 0.8).abs() < 1e-6);
    }

    /// §18.9：isolation 段未知字段拒绝并回显合法字段表。
    #[test]
    fn isolation_unknown_field_rejected() {
        let errors = errors_of("isolation:\n  foo: 1\n");
        assert!(errors.iter().any(|e| e.code == "CONFIG_UNKNOWN_FIELD"
            && e.field == "isolation.foo"
            && e.expected.contains("max_exposure")));
    }

    /// §8.1：非法隔离档字符串报取值错。
    #[test]
    fn isolation_bad_level_rejected() {
        let errors = errors_of("isolation:\n  default: secret\n");
        assert!(errors
            .iter()
            .any(|e| e.code == "CONFIG_INVALID_VALUE" && e.field == "isolation.default"));
    }

    /// 保留率越界报取值错。
    #[test]
    fn isolation_ratio_out_of_range_rejected() {
        let errors = errors_of("isolation:\n  redact_min_retention: 2.0\n");
        assert!(errors.iter().any(|e| e.code == "CONFIG_INVALID_VALUE"
            && e.field == "isolation.redact_min_retention"));
    }

    /// §8.3：有效硬顶只在请求级覆盖与 max_exposure 之间取最严（不能放宽）。
    #[test]
    fn isolation_effective_cap_only_tightens() {
        let iso = IsolationConfig {
            max_exposure: ExposureLevel::Summary,
            ..IsolationConfig::default()
        };
        assert_eq!(
            iso.effective_cap(Some(ExposureLevel::Raw)),
            ExposureLevel::Summary
        );
        assert_eq!(
            iso.effective_cap(Some(ExposureLevel::None)),
            ExposureLevel::None
        );
    }

    /// §8：路径规则按 glob 命中，最严规则优先。
    #[test]
    fn isolation_path_rule_matches_most_restrictive() {
        let iso = IsolationConfig::default();
        assert_eq!(
            iso.path_rule_for("conf/.env.local").map(|r| r.level),
            Some(ExposureLevel::None)
        );
        assert_eq!(
            iso.path_rule_for("secrets/db/key").map(|r| r.level),
            Some(ExposureLevel::None)
        );
        assert_eq!(
            iso.path_rule_for("var/app.log").map(|r| r.level),
            Some(ExposureLevel::Summary)
        );
        assert!(iso.path_rule_for("src/main.rs").is_none());
    }

    /// 附录 D：spill 段缺省取 7 天 / 1GiB。
    #[test]
    fn spill_default_when_absent() {
        let config = parse_config("", "test.yaml").expect("空配置应解析成功");
        assert_eq!(config.spill.ttl_days, 7);
        assert_eq!(config.spill.max_bytes, 1024 * 1024 * 1024);
    }

    /// 附录 D：spill 段自定义值生效。
    #[test]
    fn spill_overrides_applied() {
        let config = parse_config("spill:\n  ttl_days: 3\n  max_bytes: 1048576\n", "test.yaml")
            .expect("spill 段应被接受");
        assert_eq!(config.spill.ttl_days, 3);
        assert_eq!(config.spill.max_bytes, 1048576);
    }

    /// §18.9：spill 段未知字段拒绝并回显合法字段表。
    #[test]
    fn spill_unknown_field_rejected() {
        let errors = errors_of("spill:\n  foo: 1\n");
        assert!(errors.iter().any(|e| e.code == "CONFIG_UNKNOWN_FIELD"
            && e.field == "spill.foo"
            && e.expected.contains("ttl_days")));
    }

    /// 附录 D：spill 值必须为正整数，0 报取值错。
    #[test]
    fn spill_zero_rejected() {
        let errors = errors_of("spill:\n  ttl_days: 0\n");
        assert!(errors
            .iter()
            .any(|e| e.code == "CONFIG_INVALID_VALUE" && e.field == "spill.ttl_days"));
    }

    /// 附录 D：snapshot 段缺省取 auto / 512MB / 20 / 24h / 30d / 1GiB / 24h。
    #[test]
    fn snapshot_default_when_absent() {
        let config = parse_config("", "test.yaml").expect("空配置应解析成功");
        assert_eq!(config.snapshot.data, "auto");
        assert_eq!(config.snapshot.min_free_bytes, 512 * 1024 * 1024);
        assert_eq!(config.snapshot.keep_per_file, 20);
        assert_eq!(config.snapshot.min_retention_hours, 24);
        assert_eq!(config.snapshot.ttl_days, 30);
        assert_eq!(config.snapshot.max_bytes, 1024 * 1024 * 1024);
        assert_eq!(config.snapshot.gc_interval_hours, 24);
    }

    /// 附录 D：snapshot 段自定义值生效。
    #[test]
    fn snapshot_overrides_applied() {
        let config = parse_config(
            "snapshot:\n  data: copy\n  keep_per_file: 5\n  ttl_days: 10\n  max_bytes: 1048576\n",
            "test.yaml",
        )
        .expect("snapshot 段应被接受");
        assert_eq!(config.snapshot.data, "copy");
        assert_eq!(config.snapshot.keep_per_file, 5);
        assert_eq!(config.snapshot.ttl_days, 10);
        assert_eq!(config.snapshot.max_bytes, 1048576);
    }

    /// §18.9：snapshot 段未知字段拒绝并回显合法字段表。
    #[test]
    fn snapshot_unknown_field_rejected() {
        let errors = errors_of("snapshot:\n  foo: 1\n");
        assert!(errors.iter().any(|e| e.code == "CONFIG_UNKNOWN_FIELD"
            && e.field == "snapshot.foo"
            && e.expected.contains("keep_per_file")));
    }

    /// 附录 D：snapshot.data 只接受 auto / reflink / copy。
    #[test]
    fn snapshot_invalid_data_rejected() {
        let errors = errors_of("snapshot:\n  data: link\n");
        assert!(errors
            .iter()
            .any(|e| e.code == "CONFIG_INVALID_VALUE" && e.field == "snapshot.data"));
    }

    /// 附录 D：snapshot 数值必须为正整数，0 报取值错。
    #[test]
    fn snapshot_zero_rejected() {
        let errors = errors_of("snapshot:\n  ttl_days: 0\n");
        assert!(errors
            .iter()
            .any(|e| e.code == "CONFIG_INVALID_VALUE" && e.field == "snapshot.ttl_days"));
    }

    /// 附录 D：logging 段缺省 warn。
    #[test]
    fn logging_default_when_absent() {
        let config = parse_config("", "test.yaml").expect("空配置应解析成功");
        assert_eq!(config.logging.level, "warn");
    }

    /// 附录 D：logging.level 自定义值生效。
    #[test]
    fn logging_level_override() {
        let config = parse_config("logging:\n  level: debug\n", "test.yaml")
            .expect("logging 段应被接受");
        assert_eq!(config.logging.level, "debug");
    }

    /// 附录 D：logging.level 取值限定，非法值拒绝。
    #[test]
    fn logging_invalid_level_rejected() {
        let errors = errors_of("logging:\n  level: verbose\n");
        assert!(errors
            .iter()
            .any(|e| e.code == "CONFIG_INVALID_VALUE" && e.field == "logging.level"));
    }

    /// §3.10 + §3.8：最小配置能解析，两个模型覆盖四个槽位，rerank 与 embedding 为空。
    #[test]
    fn minimal_config_parses_and_binds() {
        let config = parse_config(
            "models:\n  jev:\n    path: ~/.toolgate/models/jev\n    serves: [classify, decide]\n  lfm3-vl:\n    path: ~/.toolgate/models/lfm3-vl\n    serves: [summarize, vision]\n",
            "test.yaml",
        )
        .expect("最小配置应当解析成功");
        assert_eq!(config.models.len(), 2);
        assert_eq!(config.bindings.len(), 6);
        assert_eq!(
            binding(&config, SlotKind::Classify).model_id.as_deref(),
            Some("jev")
        );
        assert_eq!(
            binding(&config, SlotKind::Decide).model_id.as_deref(),
            Some("jev")
        );
        assert_eq!(
            binding(&config, SlotKind::Summarize).model_id.as_deref(),
            Some("lfm3-vl")
        );
        assert_eq!(
            binding(&config, SlotKind::Vision).model_id.as_deref(),
            Some("lfm3-vl")
        );
        // §3.8：没配的槽位为空，不报错。
        assert_eq!(binding(&config, SlotKind::Rerank).model_id, None);
        assert!(!binding(&config, SlotKind::Classify).explicit);
    }

    /// §3.10：slots 显式覆盖优先，其余槽位仍按 serves 自动解析。
    #[test]
    fn explicit_slot_overrides_auto_binding() {
        let config = parse_config(
            "models:\n  a:\n    path: /tmp/a\n    serves: [summarize]\n  b:\n    path: /tmp/b\n    serves: [summarize]\nslots:\n  summarize: b\n",
            "test.yaml",
        )
        .expect("显式覆盖应当解析成功");
        let summarize = binding(&config, SlotKind::Summarize);
        assert_eq!(summarize.model_id.as_deref(), Some("b"));
        assert!(summarize.explicit);
        assert_eq!(config.slots.get("summarize").map(String::as_str), Some("b"));
    }

    /// §3.10：多个模型服务同一槽位时按声明顺序取第一个。
    #[test]
    fn declaration_order_decides_auto_binding() {
        let config = parse_config(
            "models:\n  first:\n    path: /tmp/first\n    serves: [classify]\n  second:\n    path: /tmp/second\n    serves: [classify]\n",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        assert_eq!(
            binding(&config, SlotKind::Classify).model_id.as_deref(),
            Some("first")
        );
        assert_eq!(config.models[0].id, "first");
    }

    /// §18.1：多模型服务同一槽位时优先绑 source: systemone 的模型（能用原生就用原生）。
    #[test]
    fn systemone_source_wins_auto_binding() {
        let config = parse_config(
            "models:\n  llm:\n    source: openai\n    base_url: http://127.0.0.1:8080/v1\n    model: demo\n    serves: [classify]\n  jev:\n    source: systemone\n    base_url: http://127.0.0.1:8090\n    model: jev\n    mode: http\n    serves: [classify]\n",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        let classify = binding(&config, SlotKind::Classify);
        assert_eq!(classify.model_id.as_deref(), Some("jev"));
        assert!(matches!(classify.source, Some(ModelSource::SystemOne)));
        assert_eq!(classify.adapter, Some(Adapter::Native));
    }

    /// §3.10：只写 serves 不写 slots 时，槽位按模型声明自动绑定（含 embedding）。
    #[test]
    fn serves_alone_auto_binds_every_declared_slot() {
        let config = parse_config(
            "models:\n  bge:\n    source: embedding\n    base_url: http://127.0.0.1:10020\n    model: bge-m3\n    serves: [embedding]\n",
            "test.yaml",
        )
        .expect("应解析成功");
        let bound = binding(&config, SlotKind::Embedding);
        assert_eq!(bound.model_id.as_deref(), Some("bge"));
        assert!(matches!(bound.source, Some(ModelSource::Embedding)));
    }

    /// §3.8：一个模型都不配时，六个槽位全空但仍然成功。
    #[test]
    fn empty_config_leaves_all_slots_unbound() {
        let config = parse_config("", "test.yaml").expect("空配置应当解析成功");
        assert!(config.models.is_empty());
        assert_eq!(config.bindings.len(), 6);
        for slot in SlotKind::ALL {
            assert_eq!(binding(&config, slot).model_id, None);
        }
    }

    /// §18.9：未知字段拒绝，且多错误一次聚合返回。
    #[test]
    fn unknown_top_level_field_is_rejected_with_aggregated_errors() {
        let errors = errors_of(
            "fetch:\n  enabled: true\nintent:\n  default: raw\nmodels:\n  jev:\n    serve: [classify]\n",
        );
        assert!(errors.len() >= 3, "应当一次列全所有错误：{errors:?}");
        let unknown: Vec<&ConfigError> = errors
            .iter()
            .filter(|e| e.code == "CONFIG_UNKNOWN_FIELD")
            .collect();
        assert!(unknown.iter().any(|e| e.field == "fetch"));
        assert!(unknown.iter().any(|e| e.field == "intent"));
        assert!(unknown.iter().any(|e| e.field == "models.jev.serve"));
        for error in &errors {
            assert!(!error.fix.is_empty(), "错误必须带可执行下一步：{error:?}");
        }
    }

    /// §18.9：模型层未知字段被拒。
    #[test]
    fn unknown_model_field_is_rejected() {
        let errors = errors_of("models:\n  jev:\n    path: /tmp/jev\n    protocol: openai\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_UNKNOWN_FIELD");
        assert_eq!(errors[0].field, "models.jev.protocol");
    }

    /// §18.9：枚举非法要列出全部合法值。
    #[test]
    fn invalid_enum_lists_all_legal_values() {
        let errors = errors_of("models:\n  jev:\n    source: gpt\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_ENUM");
        assert_eq!(errors[0].value.as_deref(), Some("gpt"));
        assert_eq!(
            errors[0].expected,
            "auto / local / openai / anthropic / responses / systemone / rerank / embedding"
        );
    }

    /// §18.9：类型错要回显实际值并写明期望类型。
    #[test]
    fn wrong_type_is_reported() {
        let errors = errors_of(
            "models:\n  jev:\n    path: 42\n    allow_remote: 'yes'\n    serves: classify\n",
        );
        assert_eq!(errors.len(), 3);
        assert!(errors.iter().all(|e| e.code == "CONFIG_TYPE"));
        let path = errors
            .iter()
            .find(|e| e.field == "models.jev.path")
            .unwrap();
        assert_eq!(path.value.as_deref(), Some("42"));
        assert_eq!(path.expected, "字符串");
        let serves = errors
            .iter()
            .find(|e| e.field == "models.jev.serves")
            .unwrap();
        assert_eq!(serves.expected, "槽位名列表（数组）");
    }

    /// §18.1：source=auto 的六条推断分支（path / 三条路径 / 两个 host）。
    #[test]
    fn auto_source_infers_six_branches() {
        assert_eq!(
            infer_source_from_url("http://127.0.0.1:8080/v1/chat/completions"),
            Some(ModelSource::Openai)
        );
        assert_eq!(
            infer_source_from_url("http://127.0.0.1:8080/v1/messages"),
            Some(ModelSource::Anthropic)
        );
        assert_eq!(
            infer_source_from_url("http://127.0.0.1:8080/v1/responses"),
            Some(ModelSource::Responses)
        );
        assert_eq!(
            infer_source_from_url("https://api.openai.com/v1"),
            Some(ModelSource::Openai)
        );
        assert_eq!(
            infer_source_from_url("https://api.anthropic.com/v1"),
            Some(ModelSource::Anthropic)
        );
        assert_eq!(infer_source_from_url("http://127.0.0.1:8080/v1"), None);

        // 配了 path 走 local。
        let config = parse_config("models:\n  jev:\n    path: /tmp/jev\n", "test.yaml")
            .expect("本地模型应当解析成功");
        assert_eq!(
            resolve_source(&config.models[0].config).unwrap(),
            ModelSource::Local
        );
    }

    /// §18.1：auto 推断不出来就是配置错误，不做启动探测。
    #[test]
    fn unresolvable_source_is_rejected() {
        let errors = errors_of("models:\n  jev:\n    base_url: http://127.0.0.1:8080/v1\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_SOURCE_UNRESOLVED");
        assert_eq!(
            errors[0].fix,
            "显式写 source（见 §18.1 的 source 表；systemone / rerank / embedding 不参与 auto 推断）"
        );
    }

    /// §18.1：mode=embedded 与外部 source 冲突。
    #[test]
    fn embedded_mode_conflicts_with_external_source() {
        let errors = errors_of(
            "models:\n  jev:\n    source: openai\n    mode: embedded\n    base_url: http://127.0.0.1:8080/v1/chat/completions\n",
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_MODE_CONFLICT");
        assert_eq!(errors[0].field, "models.jev.mode");
    }

    /// §18.1：远程地址未开 allow_remote 被拒。
    #[test]
    fn remote_base_url_requires_allow_remote() {
        let errors = errors_of(
            "models:\n  jev:\n    source: openai\n    base_url: https://api.openai.com/v1/chat/completions\n",
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_REMOTE_NOT_ALLOWED");
        assert_eq!(errors[0].field, "models.jev.base_url");

        // 显式 allow_remote 后放行。
        parse_config(
            "models:\n  jev:\n    source: openai\n    allow_remote: true\n    base_url: https://api.openai.com/v1/chat/completions\n",
            "test.yaml",
        )
        .expect("显式 allow_remote 后应当通过");
    }

    /// §18.9：api_key_env 未设置被拒，且只回显变量名不回显密钥值。
    #[test]
    fn missing_api_key_env_is_rejected() {
        let errors = errors_of(
            "models:\n  jev:\n    source: openai\n    base_url: http://127.0.0.1:8080/v1/chat/completions\n    api_key_env: TOOLGATE_TEST_KEY_ABSENT_9F3A\n",
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_MISSING_ENV");
        assert_eq!(
            errors[0].value.as_deref(),
            Some("TOOLGATE_TEST_KEY_ABSENT_9F3A")
        );
        assert_eq!(errors[0].field, "models.jev.api_key_env");
    }

    /// ADR 0007：`source: rerank` 可解析、走 native、属外部来源。
    #[test]
    fn rerank_source_parses_and_uses_native_adapter() {
        let config = parse_config(
            "models:\n  jina:\n    source: rerank\n    base_url: http://127.0.0.1:10020\n    model: /models/jina\n    serves: [rerank]\n",
            "test.yaml",
        )
        .unwrap();
        let model = &config.models[0];
        assert_eq!(model.config.source, Some(ModelSource::Rerank));
        assert!(ModelSource::Rerank.is_external());
        assert_eq!(resolve_source(&model.config).unwrap(), ModelSource::Rerank);
        assert_eq!(
            resolve_adapter(&model.config, SlotKind::Rerank, ModelSource::Rerank),
            Adapter::Native
        );
    }

    /// §3.10：slots 指向不存在的模型被拒。
    #[test]
    fn slot_pointing_to_unknown_model_is_rejected() {
        let errors = errors_of("models:\n  jev:\n    path: /tmp/jev\nslots:\n  summarize: ghost\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_UNKNOWN_MODEL");
        assert_eq!(errors[0].field, "slots.summarize");
        assert_eq!(errors[0].value.as_deref(), Some("ghost"));
    }

    /// §3.10：serves 含未知槽位被拒，字段路径到下标。
    #[test]
    fn serves_with_unknown_slot_is_rejected() {
        let errors =
            errors_of("models:\n  jev:\n    path: /tmp/jev\n    serves: [classify, embed]\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_UNKNOWN_SLOT");
        assert_eq!(errors[0].field, "models.jev.serves[1]");
    }

    /// §3.10：slots 键不是合法槽位名被拒（embedding 现已是合法槽位）。
    #[test]
    fn slots_key_must_be_a_legal_slot() {
        let errors = errors_of("slots:\n  bogus: jev\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_UNKNOWN_SLOT");
        assert_eq!(errors[0].field, "slots.bogus");
    }

    /// §18.9：YAML 语法错要报出位置。
    #[test]
    fn yaml_syntax_error_reports_position() {
        let errors = errors_of("models:\n  jev:\n   - path: /tmp\n    bad: x\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_SYNTAX");
        assert_eq!(errors[0].field, "test.yaml");
        assert_eq!(errors[0].expected, "合法 YAML");
        assert!(
            errors[0].fix.contains('行') && errors[0].fix.contains('列'),
            "语法错误要带行列位置：{}",
            errors[0].fix
        );
    }

    /// §18.9：错误集合能转成约定的 JSON 形态。
    #[test]
    fn errors_to_json_has_five_fields() {
        let errors = errors_of("models:\n  jev:\n    source: gpt\n");
        let json = ConfigErrors {
            items: errors.clone(),
        }
        .to_json();
        assert_eq!(json["errors"].as_array().unwrap().len(), errors.len());
        let first = &json["errors"][0];
        assert_eq!(first["code"], json!("CONFIG_ENUM"));
        assert_eq!(first["field"], json!("models.jev.source"));
        assert_eq!(first["value"], json!("gpt"));
        assert!(first["expected"].is_string());
        assert!(first["fix"].is_string());
    }

    /// §18.1：auto 分档 —— classify/decide 走 embedded，summarize/vision 走 http。
    #[test]
    fn auto_mode_uses_slot_tier() {
        let config = parse_config(
            "models:\n  jev:\n    path: /tmp/jev\n    serves: [classify, decide]\n  vl:\n    path: /tmp/vl\n    serves: [summarize, vision]\n",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        assert_eq!(
            binding(&config, SlotKind::Classify).mode,
            Some(SlmMode::Embedded)
        );
        assert_eq!(
            binding(&config, SlotKind::Decide).mode,
            Some(SlmMode::Embedded)
        );
        assert_eq!(
            binding(&config, SlotKind::Summarize).mode,
            Some(SlmMode::Http)
        );
        assert_eq!(binding(&config, SlotKind::Vision).mode, Some(SlmMode::Http));
    }

    /// §18.1：模型没有 serves 时走 http 默认档。
    #[test]
    fn model_without_serves_defaults_to_http() {
        let config = parse_config("models:\n  jev:\n    path: /tmp/jev\n", "test.yaml")
            .expect("配置应当解析成功");
        let slm = config.slm.clone();
        assert_eq!(
            resolve_mode(&config.models[0].config, &slm).unwrap(),
            SlmMode::Http
        );
    }

    /// §18.1：模型级 mode 覆盖 slm.mode。
    #[test]
    fn model_mode_overrides_slm_mode() {
        let config = parse_config(
            "slm:\n  mode: embedded\nmodels:\n  jev:\n    path: /tmp/jev\n    mode: http\n",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        let slm = config.slm.clone();
        assert_eq!(
            resolve_mode(&config.models[0].config, &slm).unwrap(),
            SlmMode::Http
        );
    }

    /// §3.10：adapter 的 auto 推断规则。
    #[test]
    fn adapter_auto_inference() {
        let local = ModelConfig {
            source: None,
            mode: None,
            adapter: None,
            path: Some("/tmp/jev".to_owned()),
            base_url: None,
            model: None,
            api_key_env: None,
            allow_remote: false,
            serves: Vec::new(),
        };
        assert_eq!(
            resolve_adapter(&local, SlotKind::Classify, ModelSource::Local),
            Adapter::Native
        );
        assert_eq!(
            resolve_adapter(&local, SlotKind::Classify, ModelSource::Openai),
            Adapter::Json
        );
        assert_eq!(
            resolve_adapter(&local, SlotKind::Summarize, ModelSource::Openai),
            Adapter::Text
        );
        assert_eq!(
            resolve_adapter(&local, SlotKind::Rerank, ModelSource::Anthropic),
            Adapter::Ordered
        );
        assert_eq!(
            resolve_adapter(&local, SlotKind::Decide, ModelSource::Auto),
            Adapter::Json
        );
    }

    /// §18.1：外部来源缺 base_url 被拒；slm.http.base_url 可作统一兜底。
    #[test]
    fn external_source_requires_base_url() {
        let errors = errors_of("models:\n  jev:\n    source: openai\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_MISSING_BASE_URL");
        assert_eq!(errors[0].field, "models.jev.base_url");

        parse_config(
            "slm:\n  http:\n    base_url: http://127.0.0.1:8080/v1/chat/completions\nmodels:\n  jev:\n    source: openai\n",
            "test.yaml",
        )
        .expect("slm.http.base_url 可作统一兜底");
    }

    /// §18.1：source=local 必须有 path。
    #[test]
    fn local_source_requires_path() {
        let errors = errors_of("models:\n  jev:\n    source: local\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_MISSING_PATH");
        assert_eq!(errors[0].field, "models.jev.path");
    }

    /// §18.9：结构层多个错误一次列全（模型层 + slm 层）。
    #[test]
    fn multiple_structural_errors_aggregate() {
        let errors = errors_of(
            "slm:\n  mode: fast\n  embedded:\n    backend: mlx\n    max_resident: 2\nmodels:\n  jev:\n    path: /tmp/jev\n    adapter: rigid\n",
        );
        assert_eq!(errors.len(), 3);
        let mode = errors.iter().find(|e| e.field == "slm.mode").unwrap();
        assert_eq!(mode.code, "CONFIG_ENUM");
        let unknown = errors
            .iter()
            .find(|e| e.field == "slm.embedded.max_resident")
            .unwrap();
        assert_eq!(unknown.code, "CONFIG_UNKNOWN_FIELD");
        let adapter = errors
            .iter()
            .find(|e| e.field == "models.jev.adapter")
            .unwrap();
        assert_eq!(adapter.code, "CONFIG_ENUM");
    }

    /// 附录 D：默认配置文件路径是 ~/.toolgate/config.yaml。
    #[test]
    fn default_config_path_points_to_dot_toolgate() {
        let path = default_config_path();
        assert!(path.ends_with("config.yaml"));
        assert_eq!(path.parent().unwrap().file_name().unwrap(), ".toolgate");
    }

    /// 文件不存在时返回 Ok(None)（无配置文件也能启动）。
    #[test]
    fn load_config_returns_none_when_missing() {
        let path = std::env::temp_dir().join("toolgate-config-not-exist-9F3A.yaml");
        let loaded = load_config(&path).expect("文件不存在不算配置错误");
        assert!(loaded.is_none());
    }

    /// §18.1b：配置错误文案按用户语言输出；code / field / value 不译。
    #[test]
    fn config_errors_localize_to_english() {
        let errors = parse_config(
            "models:\n  jev:\n    path: /tmp/jev\nslots:\n  summarize: ghost\n",
            "test.yaml",
        )
        .expect_err("应当报错");
        let en = errors.localized(Language::En);
        assert_eq!(en.items.len(), 1);
        assert_eq!(en.items[0].code, "CONFIG_UNKNOWN_MODEL");
        assert_eq!(en.items[0].field, "slots.summarize");
        assert_eq!(en.items[0].value.as_deref(), Some("ghost"));
        assert_eq!(en.items[0].expected, "a defined model id");
        assert_eq!(
            en.items[0].fix,
            "Define this model under models, or point to an existing model id"
        );
        // 中文保持源文。
        assert_eq!(errors.items[0].expected, "已定义的模型 id");
        assert_eq!(
            errors.items[0].fix,
            "在 models 里定义该模型，或把这里改成已存在的模型 id"
        );
    }

    /// §18.1b：未接线段与带插值的文案也要本地化。
    #[test]
    fn unimplemented_section_and_interpolated_text_localize() {
        let errors =
            parse_config("cache:\n  enabled: true\n", "test.yaml").expect_err("应当报错");
        let en = errors.localized(Language::En);
        assert_eq!(en.items[0].code, "CONFIG_UNKNOWN_FIELD");
        assert_eq!(en.items[0].field, "cache");
        // 段名是字面标识符，不翻译；fix 必须整句英文。
        assert_eq!(
            en.items[0].expected,
            "models / slots / slm / paths / limits / spill / snapshot / logging / isolation"
        );
        assert_eq!(
            en.items[0].fix,
            "This configuration section is not implemented in this version; remove it or wait for a later version"
        );

        // 类型错：fix 带插值，也要整句英文。
        let type_errors =
            parse_config("models:\n  jev:\n    path: 42\n", "test.yaml").expect_err("应当报错");
        let en_type = type_errors.localized(Language::En);
        assert_eq!(en_type.items[0].expected, "string");
        assert_eq!(en_type.items[0].fix, "Change this field to string");

        // 语法错误：位置片段也要译成英文。
        let syntax = parse_config(
            "models:\n  jev:\n   - path: /tmp\n    bad: x\n",
            "test.yaml",
        )
        .expect_err("应当报错");
        let en_syntax = syntax.localized(Language::En);
        assert_eq!(en_syntax.items[0].code, "CONFIG_SYNTAX");
        assert!(
            en_syntax.items[0]
                .fix
                .starts_with("Fix the YAML syntax and retry: ")
        );
        assert!(en_syntax.items[0].fix.contains("(position: line "));
        assert!(en_syntax.items[0].fix.contains(" column "));
        assert!(en_syntax.items[0].fix.ends_with(')'));
        // 中文仍是中文。
        assert!(syntax.items[0].fix.contains("行"));
        assert!(syntax.items[0].fix.contains("列"));
    }

    /// §17.2：paths.allowed_roots 解析绝对路径；缺省为空 = 只用启动工作目录。
    #[test]
    fn paths_allowed_roots_parses_absolute_paths() {
        let config = parse_config(
            "paths:\n  allowed_roots:\n    - /tmp\n    - /Users/me/project\n",
            "test.yaml",
        )
        .expect("应当解析成功");
        assert_eq!(
            config.paths.allowed_roots,
            vec!["/tmp", "/Users/me/project"]
        );
        let empty = parse_config("", "test.yaml").expect("空配置应当解析成功");
        assert!(empty.paths.allowed_roots.is_empty());
    }

    /// §17.2：允许根必须绝对路径，相对路径 fail loud。
    #[test]
    fn paths_allowed_roots_rejects_relative_paths() {
        let errors = errors_of("paths:\n  allowed_roots:\n    - ./data\n");
        assert_eq!(errors[0].code, "CONFIG_INVALID_VALUE");
        assert_eq!(errors[0].field, "paths.allowed_roots[0]");
        assert!(errors[0].fix.contains("绝对路径"));
    }

    /// §18.9：paths 段未知字段明确拒绝，不静默忽略。
    #[test]
    fn paths_unknown_field_rejected() {
        let errors = errors_of("paths:\n  bogus: true\n");
        assert_eq!(errors[0].code, "CONFIG_UNKNOWN_FIELD");
        assert_eq!(errors[0].field, "paths.bogus");
        assert!(errors[0].expected.contains("allowed_roots"));
        assert!(errors[0].expected.contains("expose_absolute"));
    }

    /// §8.11：expose_absolute 默认 false，显式 true 才打开绝对路径呈现。
    #[test]
    fn paths_expose_absolute_defaults_false() {
        let empty = parse_config("", "test.yaml").expect("空配置应当解析成功");
        assert!(!empty.paths.expose_absolute);
        let config = parse_config("paths:\n  expose_absolute: true\n", "test.yaml")
            .expect("应当解析成功");
        assert!(config.paths.expose_absolute);
        let off = parse_config("paths:\n  expose_absolute: false\n", "test.yaml")
            .expect("应当解析成功");
        assert!(!off.paths.expose_absolute);
    }

    /// §18.9：expose_absolute 非法值 fail loud，不静默当 false。
    #[test]
    fn paths_expose_absolute_rejects_non_bool() {
        let errors = errors_of("paths:\n  expose_absolute: \"true\"\n");
        assert_eq!(errors[0].code, "CONFIG_TYPE");
        assert_eq!(errors[0].field, "paths.expose_absolute");
    }

    /// §18.9：allowed_roots 类型错要 fail loud 并指出字段路径。
    #[test]
    fn paths_allowed_roots_type_errors() {
        let errors = errors_of("paths:\n  allowed_roots: /tmp\n");
        assert_eq!(errors[0].code, "CONFIG_TYPE");
        assert_eq!(errors[0].field, "paths.allowed_roots");
    }
}
