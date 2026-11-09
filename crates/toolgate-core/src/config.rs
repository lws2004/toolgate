//! 配置层：解析配置文件，做语法 / 结构 / 语义三层校验，并解析五个槽位的绑定。
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

/// 路径语义配置（§17.2 / 附录 D）。只接线 allowed_roots；expose_absolute 等字段尚未接线。
#[derive(Debug, Clone, Default, Serialize)]
pub struct PathsConfig {
    /// 文件访问允许根（§17.2）：read / write / edit / search 的路径必须落在其中。
    /// 缺省空 = 只用启动时工作目录。
    pub allowed_roots: Vec<String>,
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

/// 解析完成的配置：模型清单 + 显式覆盖 + 五个槽位的绑定。
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
    /// 五个槽位的绑定，顺序同 SlotKind::ALL。
    pub bindings: Vec<SlotBinding>,
}

// ---------- 合法取值表 ----------

/// 顶层合法段（§18.9：未知字段一律拒绝）。
const TOP_LEVEL_FIELDS: &str = "models / slots / slm / paths";
/// paths 段合法字段。
const PATHS_FIELDS: &str = "allowed_roots";
/// 模型配置合法字段。
const MODEL_FIELDS: &str =
    "source / mode / adapter / path / base_url / model / api_key_env / allow_remote / serves";
/// SLM 段合法字段。
const SLM_FIELDS: &str = "mode / probe / disable_after_failures / embedded / http";
/// embedded 段合法字段。
const EMBEDDED_FIELDS: &str = "backend / model_dir / max_resident_models / idle_ttl_seconds";
/// http 段合法字段。
const HTTP_FIELDS: &str = "base_url";
/// 五个槽位名。
const SLOT_NAMES: &str = "classify / rerank / decide / summarize / vision";
/// source 枚举的合法值。
const SOURCE_VALUES: &str = "auto / local / openai / anthropic / responses / systemone / rerank";
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
        "把槽位名改成五个合法槽位之一",
        "Change the slot name to one of the five legal slots",
    ),
    (
        "槽位名只能是五个合法槽位之一",
        "A slot name must be one of the five legal slots",
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
            errors.push(type_error(origin, &other, "映射（models / slots / slm / paths）"));
            return Err(ConfigErrors { items: errors });
        }
    };

    // 顶层：只允许 models / slots / slm / paths（§18.9）。
    for (key, _) in root_map.iter() {
        if !matches!(key.as_str(), "models" | "slots" | "slm" | "paths") {
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
        fix: "显式写 source（见 §18.1 的 source 表；systemone / rerank 不参与 auto 推断）".to_owned(),
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
        ModelSource::Local | ModelSource::SystemOne | ModelSource::Rerank => Adapter::Native,
        ModelSource::Auto => Adapter::Json,
        ModelSource::Openai | ModelSource::Anthropic | ModelSource::Responses => match slot {
            SlotKind::Classify | SlotKind::Decide => Adapter::Json,
            SlotKind::Summarize | SlotKind::Vision => Adapter::Text,
            SlotKind::Rerank => Adapter::Ordered,
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
                    fix: "把槽位名改成五个合法槽位之一".to_owned(),
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
                fix: "槽位名只能是五个合法槽位之一".to_owned(),
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

/// 解析 paths 段：只允许 allowed_roots（§17.2）。本版本其余 paths 字段尚未接线。
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
            _ => errors.push(ConfigError {
                code: "CONFIG_UNKNOWN_FIELD".to_owned(),
                field,
                value: Some(value_repr(item)),
                expected: PATHS_FIELDS.to_owned(),
                fix: "该字段不在 paths 段的合法字段表内（expose_absolute 等字段本版本尚未接线），请检查拼写或移除"
                    .to_owned(),
            }),
        }
    }
    paths
}

// ---------- 内部：绑定与取值辅助 ----------

/// 解析五个槽位的绑定（§3.10）：显式覆盖优先，否则按 models 声明顺序取第一个 serves 命中的模型。
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
            None => (
                models
                    .iter()
                    .find(|entry| entry.config.serves.contains(&slot))
                    .map(|entry| entry.id.clone()),
                false,
            ),
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
        SlotKind::Rerank | SlotKind::Summarize | SlotKind::Vision => SlmMode::Http,
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
            .expect("五个槽位都应有绑定")
    }

    fn errors_of(text: &str) -> Vec<ConfigError> {
        parse_config(text, "test.yaml").expect_err("应当报错").items
    }

    /// §3.10 + §3.8：最小配置能解析，两个模型覆盖四个槽位，rerank 为空。
    #[test]
    fn minimal_config_parses_and_binds() {
        let config = parse_config(
            "models:\n  jev:\n    path: ~/.toolgate/models/jev\n    serves: [classify, decide]\n  lfm3-vl:\n    path: ~/.toolgate/models/lfm3-vl\n    serves: [summarize, vision]\n",
            "test.yaml",
        )
        .expect("最小配置应当解析成功");
        assert_eq!(config.models.len(), 2);
        assert_eq!(config.bindings.len(), 5);
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

    /// §3.8：一个模型都不配时，五个槽位全空但仍然成功。
    #[test]
    fn empty_config_leaves_all_slots_unbound() {
        let config = parse_config("", "test.yaml").expect("空配置应当解析成功");
        assert!(config.models.is_empty());
        assert_eq!(config.bindings.len(), 5);
        for slot in SlotKind::ALL {
            assert_eq!(binding(&config, slot).model_id, None);
        }
    }

    /// §18.9：未知字段拒绝，且多错误一次聚合返回。
    #[test]
    fn unknown_top_level_field_is_rejected_with_aggregated_errors() {
        let errors = errors_of(
            "isolation:\n  default: summary\nintent:\n  default: raw\nmodels:\n  jev:\n    serve: [classify]\n",
        );
        assert!(errors.len() >= 3, "应当一次列全所有错误：{errors:?}");
        let unknown: Vec<&ConfigError> = errors
            .iter()
            .filter(|e| e.code == "CONFIG_UNKNOWN_FIELD")
            .collect();
        assert!(unknown.iter().any(|e| e.field == "isolation"));
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
            "auto / local / openai / anthropic / responses / systemone / rerank"
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
            "显式写 source（见 §18.1 的 source 表；systemone / rerank 不参与 auto 推断）"
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

    /// §3.10：slots 键不是合法槽位名被拒。
    #[test]
    fn slots_key_must_be_a_legal_slot() {
        let errors = errors_of("slots:\n  embedding: jev\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "CONFIG_UNKNOWN_SLOT");
        assert_eq!(errors[0].field, "slots.embedding");
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
            parse_config("isolation:\n  default: summary\n", "test.yaml").expect_err("应当报错");
        let en = errors.localized(Language::En);
        assert_eq!(en.items[0].code, "CONFIG_UNKNOWN_FIELD");
        assert_eq!(en.items[0].field, "isolation");
        // 段名是字面标识符，不翻译；fix 必须整句英文。
        assert_eq!(en.items[0].expected, "models / slots / slm / paths");
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
        assert_eq!(config.paths.allowed_roots, vec!["/tmp", "/Users/me/project"]);
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

    /// §18.9：paths 段未接线的字段明确拒绝，不静默忽略。
    #[test]
    fn paths_unknown_field_rejected() {
        let errors = errors_of("paths:\n  expose_absolute: true\n");
        assert_eq!(errors[0].code, "CONFIG_UNKNOWN_FIELD");
        assert_eq!(errors[0].field, "paths.expose_absolute");
        assert!(errors[0].expected.contains("allowed_roots"));
    }

    /// §18.9：allowed_roots 类型错要 fail loud 并指出字段路径。
    #[test]
    fn paths_allowed_roots_type_errors() {
        let errors = errors_of("paths:\n  allowed_roots: /tmp\n");
        assert_eq!(errors[0].code, "CONFIG_TYPE");
        assert_eq!(errors[0].field, "paths.allowed_roots");
    }
}
