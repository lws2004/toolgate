//! decide 槽位的调用编排：绑定 → 构造 → 发送 → 解析 → 契约校验 → §7.2 门控 → §3.11 状态机。
//!
//! HTTP 发送在 #[cfg(feature = "external-protocols")] 后面：默认构建不含外部协议客户端
//! （§19.6 可选层），此时 decide 直接返回 ClientUnavailable，其余编排逻辑照常可测。
//!
//! 架构依据：03-slots.md §3.2（decide 槽位契约）、§3.11（能力声明与失效状态机）、
//! 06-cost.md §7.2（置信度门控）、18-deployment.md §19.6（依赖分层与可选层）。

use std::sync::Mutex;
#[cfg(feature = "external-protocols")]
use std::time::Instant;

use crate::config::{ModelSource, ResolvedConfig};
#[cfg(feature = "external-protocols")]
use crate::slot_state::FailureKind;
use crate::slot_state::{DEFAULT_MAX_FAILURES, SlotHealth, SlotHealthMap};
use crate::slots::{
    ClassifyInput, ClassifyOutput, ConfidenceGate, DecideInput, DecideOutput, RerankInput,
    RerankOutput, SlotKind, SummarizeInput, SummarizeOutput, VisionInput, VisionOutput,
    confidence_gate,
};

#[cfg(feature = "external-protocols")]
use super::{classify_protocol, ordered_protocol, systemone, text_protocol, vision_protocol};

/// 单次外部协议请求的超时（架构暂无该项配置，先用常量）。
pub const REQUEST_TIMEOUT_SECONDS: u64 = 30;

/// classify 调用结果。
#[derive(Debug, Clone)]
pub struct ClassifyOutcome {
    /// 槽位契约形状的输出：每个标签的分数（§3.2）。
    pub output: ClassifyOutput,
    /// 端到端耗时（毫秒）。
    pub latency_ms: u64,
    /// 概率性质：json 适配器产出的是模型自报值、非校准（§3.10）。
    pub calibrated: bool,
}

/// decide 调用结果。
#[derive(Debug, Clone)]
pub struct DecideOutcome {
    /// 槽位契约形状的输出：各问题的概率。
    pub output: DecideOutput,
    /// §7.2 门控结论；多问题取各问题概率的最小值。
    pub gate: ConfidenceGate,
    /// 端到端耗时（毫秒）。
    pub latency_ms: u64,
    /// 概率是否校准；systemone 的 noul 是校准概率 → true。
    pub calibrated: bool,
}

/// summarize 调用结果。
#[derive(Debug, Clone)]
pub struct SummarizeOutcome {
    /// 槽位契约形状的输出：无 schema 给 text，有 schema 给 data。
    pub output: SummarizeOutput,
    /// 端到端耗时（毫秒）。
    pub latency_ms: u64,
}

/// rerank 调用结果。
#[derive(Debug, Clone)]
pub struct RerankOutcome {
    /// 槽位契约形状的输出：按 score 降序、覆盖全部输入文档（§3.2）。
    pub output: RerankOutput,
    /// 端到端耗时（毫秒）。
    pub latency_ms: u64,
}

/// vision 调用结果。
#[derive(Debug, Clone)]
pub struct VisionOutcome {
    /// 槽位契约形状的输出：文本描述或元素坐标（§3.2）。
    pub output: VisionOutput,
    /// 端到端耗时（毫秒）。
    pub latency_ms: u64,
}

/// 槽位调用失败。
#[derive(Debug, thiserror::Error)]
pub enum SlotCallError {
    /// 槽位未绑定模型（§3.8）。
    #[error("槽位 {slot:?} 未绑定模型")]
    NotBound {
        /// 出问题的槽位。
        slot: SlotKind,
    },
    /// 客户端不可用：feature 未开、来源不支持、槽位已禁用、缺地址。
    #[error("槽位 {slot:?} 不可用：{reason}")]
    ClientUnavailable {
        /// 出问题的槽位。
        slot: SlotKind,
        /// 可行动的原因说明。
        reason: String,
    },
    /// 传输失败：连接、超时、非 2xx。
    #[error("槽位 {slot:?} 调用失败：{reason}")]
    Transport {
        /// 出问题的槽位。
        slot: SlotKind,
        /// 失败原因。
        reason: String,
    },
    /// 响应形状不符（§3.11）。
    #[error("槽位 {slot:?} 响应形状不符：{reason}")]
    Contract {
        /// 出问题的槽位。
        slot: SlotKind,
        /// 失败原因。
        reason: String,
    },
}

/// 槽位客户端：持有配置绑定与槽位健康状态（§3.11）。
pub struct SlotClient {
    cfg: ResolvedConfig,
    health: Mutex<SlotHealthMap>,
    /// HTTP 客户端：构造时建一次并复用，保留连接池（§18.1 的实例管理）。
    #[cfg(feature = "external-protocols")]
    http: Option<reqwest::Client>,
}

/// SlotClient 手写 Debug：内部有锁与 HTTP 客户端，只暴露可读摘要。
/// Clone 语义由 Arc 提供（共享同一个客户端，§22.5 状态是进程内缓存）。
impl std::fmt::Debug for SlotClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let health = self.health.lock().expect("健康状态锁中毒");
        let slots: Vec<String> = SlotKind::ALL
            .iter()
            .map(|kind| {
                let state = health.status(*kind).state;
                format!("{}={}", kind.as_str(), state.as_str())
            })
            .collect();
        f.debug_struct("SlotClient").field("slots", &slots).finish()
    }
}

impl SlotClient {
    /// 用解析后的配置建立客户端；健康状态是进程内可丢弃缓存（§22.5）。
    pub fn new(cfg: ResolvedConfig) -> Self {
        let max_failures = cfg
            .slm
            .disable_after_failures
            .unwrap_or(DEFAULT_MAX_FAILURES);
        // 客户端只建一次并复用，避免每次调用重建连接池（§18.1）。
        #[cfg(feature = "external-protocols")]
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
            .build()
            .ok();
        Self {
            cfg,
            health: Mutex::new(SlotHealthMap::with_max_failures(max_failures)),
            #[cfg(feature = "external-protocols")]
            http,
        }
    }

    /// 五个槽位的健康状态，供 toolgate list 打印（§3.11）。
    pub fn health(&self) -> Vec<(SlotKind, SlotHealth)> {
        self.health.lock().expect("健康状态锁中毒").all()
    }

    /// 某槽位是否绑定了模型（配置层，§3.10）。
    pub fn is_bound(&self, slot: SlotKind) -> bool {
        self.cfg
            .bindings
            .iter()
            .find(|binding| binding.slot == slot)
            .and_then(|binding| binding.model_id.as_ref())
            .is_some()
    }

    /// 某槽位最终绑定模型的来源（§18.1）；供返回出口按 §3.10 能力矩阵判真实可用性。
    pub fn source_for_slot(&self, slot: SlotKind) -> Option<ModelSource> {
        self.cfg
            .bindings
            .iter()
            .find(|binding| binding.slot == slot)
            .and_then(|binding| binding.source)
    }

    /// 某槽位的健康状态（§3.11）；未绑定的槽位是 declared。
    pub fn slot_health(&self, slot: SlotKind) -> SlotHealth {
        self.health
            .lock()
            .expect("健康状态锁中毒")
            .status(slot)
            .clone()
    }

    /// decide 槽位调用：绑定 → 构造 → 发送 → 解析 → 契约校验 → 门控。
    pub async fn decide(&self, input: &DecideInput) -> Result<DecideOutcome, SlotCallError> {
        let slot = SlotKind::Decide;

        let binding = self.cfg.bindings.iter().find(|b| b.slot == slot);
        let model_id = match binding.and_then(|b| b.model_id.as_ref()) {
            Some(model_id) => model_id.clone(),
            None => return Err(SlotCallError::NotBound { slot }),
        };
        let source = binding.and_then(|b| b.source).unwrap_or(ModelSource::Auto);
        // §3.10 准入：能力矩阵是唯一事实源，不在客户端里再写一份「支持哪些来源」。
        let capability = crate::capability::source_slot_capability(source, slot);
        if capability.grade != crate::capability::CapabilityGrade::Available {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: format!(
                    "source {} 的 decide 槽位能力={}：{}；当前只实现 systemone 客户端（§18.1 / §19.6）",
                    source.as_str(),
                    capability.grade.as_str(),
                    capability.reason
                ),
            });
        }
        let model = self.cfg.models.iter().find(|m| m.id == model_id);
        let base_url = model
            .and_then(|m| m.config.base_url.clone())
            .or_else(|| self.cfg.slm.http.as_ref().and_then(|h| h.base_url.clone()));
        let base_url = match base_url {
            Some(base_url) => base_url,
            None => {
                return Err(SlotCallError::ClientUnavailable {
                    slot,
                    reason: "模型没有配置 base_url".to_owned(),
                });
            }
        };
        let api_key_env = model.and_then(|m| m.config.api_key_env.clone());

        // 连续失败达上限后不再尝试（§3.11）。
        if !self.health.lock().expect("健康状态锁中毒").is_usable(slot) {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: "槽位已因连续失败被禁用，改配置并重启后恢复".to_owned(),
            });
        }

        #[cfg(feature = "external-protocols")]
        {
            let http = match &self.http {
                Some(http) => http,
                None => {
                    return Err(SlotCallError::ClientUnavailable {
                        slot,
                        reason: "构造 HTTP 客户端失败".to_owned(),
                    });
                }
            };
            self.decide_over_http(http, input, &base_url, api_key_env.as_deref())
                .await
        }
        #[cfg(not(feature = "external-protocols"))]
        {
            let _ = (input, base_url, api_key_env);
            Err(SlotCallError::ClientUnavailable {
                slot,
                reason:
                    "默认构建不含外部协议客户端（§19.6），需 cargo build --features external-protocols"
                        .to_owned(),
            })
        }
    }

    /// classify 槽位调用：绑定 → 准入 → 构造 → 发送 → 解析 → 契约校验 → 状态机。
    pub async fn classify(&self, input: &ClassifyInput) -> Result<ClassifyOutcome, SlotCallError> {
        let slot = SlotKind::Classify;
        let binding = self.cfg.bindings.iter().find(|b| b.slot == slot);
        let model_id = match binding.and_then(|b| b.model_id.as_ref()) {
            Some(model_id) => model_id.clone(),
            None => return Err(SlotCallError::NotBound { slot }),
        };
        let source = binding.and_then(|b| b.source).unwrap_or(ModelSource::Auto);
        // §3.10 准入：json 适配器只在通用 LLM 协议来源上可用（systemone 的 choice 映射未落）。
        let capability = crate::capability::source_slot_capability(source, slot);
        if capability.grade != crate::capability::CapabilityGrade::Available {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: format!(
                    "source {} 的 classify 槽位能力={}：{}",
                    source.as_str(),
                    capability.grade.as_str(),
                    capability.reason
                ),
            });
        }
        let model = self.cfg.models.iter().find(|m| m.id == model_id);
        let base_url = model
            .and_then(|m| m.config.base_url.clone())
            .or_else(|| self.cfg.slm.http.as_ref().and_then(|h| h.base_url.clone()));
        let base_url = match base_url {
            Some(base_url) => base_url,
            None => {
                return Err(SlotCallError::ClientUnavailable {
                    slot,
                    reason: "模型没有配置 base_url".to_owned(),
                });
            }
        };
        let model_name = model
            .and_then(|m| m.config.model.clone())
            .unwrap_or(model_id);
        let api_key_env = model.and_then(|m| m.config.api_key_env.clone());

        if !self.health.lock().expect("健康状态锁中毒").is_usable(slot) {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: "槽位已因连续失败被禁用，改配置并重启后恢复".to_owned(),
            });
        }

        #[cfg(feature = "external-protocols")]
        {
            let http = match &self.http {
                Some(http) => http,
                None => {
                    return Err(SlotCallError::ClientUnavailable {
                        slot,
                        reason: "构造 HTTP 客户端失败".to_owned(),
                    });
                }
            };
            self.classify_over_http(http, input, &base_url, &model_name, api_key_env.as_deref())
                .await
        }
        #[cfg(not(feature = "external-protocols"))]
        {
            let _ = (input, base_url, model_name, api_key_env);
            Err(SlotCallError::ClientUnavailable {
                slot,
                reason:
                    "默认构建不含外部协议客户端（§19.6），需 cargo build --features external-protocols"
                        .to_owned(),
            })
        }
    }

    /// classify 的 HTTP 调用：只在开启 external-protocols 时存在（§19.6）。
    #[cfg(feature = "external-protocols")]
    async fn classify_over_http(
        &self,
        http: &reqwest::Client,
        input: &ClassifyInput,
        base_url: &str,
        model: &str,
        api_key_env: Option<&str>,
    ) -> Result<ClassifyOutcome, SlotCallError> {
        let slot = SlotKind::Classify;
        let request = match classify_protocol::build_request(input, model) {
            Ok(request) => request,
            Err(violation) => {
                return Err(SlotCallError::Contract {
                    slot,
                    reason: format!("请求构造失败（输入形态问题）：{}", violation.fact),
                });
            }
        };
        let url = text_protocol::endpoint(base_url);
        let mut builder = http.post(&url).json(&request);
        if let Some(env_name) = api_key_env
            && let Ok(value) = std::env::var(env_name)
        {
            builder = builder.bearer_auth(value);
        }
        let started = Instant::now();
        let response = match builder.send().await {
            Ok(response) => response,
            Err(err) => {
                let reason = format!("请求 {url} 失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                let reason = format!("读取 {url} 响应失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        if !status.is_success() {
            let reason = format!("上游返回 {status}：{}", summarize(&body));
            self.record_failure(slot, reason.clone(), FailureKind::Transient);
            return Err(SlotCallError::Transport { slot, reason });
        }
        let parsed: text_protocol::ChatResponse = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                let reason = format!("响应不是合法 JSON：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        let output = match classify_protocol::parse_output(&parsed, input) {
            Ok(output) => output,
            Err(violation) => {
                let reason = violation.fact.clone();
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        if let Err(violation) = output.validate(input) {
            let reason = violation.fact.clone();
            self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
            return Err(SlotCallError::Contract { slot, reason });
        }
        self.record_success(slot);
        Ok(ClassifyOutcome {
            output,
            latency_ms: started.elapsed().as_millis() as u64,
            calibrated: false,
        })
    }

    /// summarize 槽位调用：绑定 → 准入 → 构造 → 发送 → 解析 → 契约校验 → 状态机。
    pub async fn summarize(
        &self,
        input: &SummarizeInput,
    ) -> Result<SummarizeOutcome, SlotCallError> {
        let slot = SlotKind::Summarize;

        let binding = self.cfg.bindings.iter().find(|b| b.slot == slot);
        let model_id = match binding.and_then(|b| b.model_id.as_ref()) {
            Some(model_id) => model_id.clone(),
            None => return Err(SlotCallError::NotBound { slot }),
        };
        let source = binding.and_then(|b| b.source).unwrap_or(ModelSource::Auto);
        // §3.10 准入：能力矩阵是唯一事实源（systemone 只做判定、不生成文本 → 不可用）。
        let capability = crate::capability::source_slot_capability(source, slot);
        if capability.grade != crate::capability::CapabilityGrade::Available {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: format!(
                    "source {} 的 summarize 槽位能力={}：{}",
                    source.as_str(),
                    capability.grade.as_str(),
                    capability.reason
                ),
            });
        }
        let model = self.cfg.models.iter().find(|m| m.id == model_id);
        let base_url = model
            .and_then(|m| m.config.base_url.clone())
            .or_else(|| self.cfg.slm.http.as_ref().and_then(|h| h.base_url.clone()));
        let base_url = match base_url {
            Some(base_url) => base_url,
            None => {
                return Err(SlotCallError::ClientUnavailable {
                    slot,
                    reason: "模型没有配置 base_url".to_owned(),
                });
            }
        };
        let model_name = model
            .and_then(|m| m.config.model.clone())
            .unwrap_or(model_id);
        let api_key_env = model.and_then(|m| m.config.api_key_env.clone());

        if !self.health.lock().expect("健康状态锁中毒").is_usable(slot) {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: "槽位已因连续失败被禁用，改配置并重启后恢复".to_owned(),
            });
        }

        #[cfg(feature = "external-protocols")]
        {
            let http = match &self.http {
                Some(http) => http,
                None => {
                    return Err(SlotCallError::ClientUnavailable {
                        slot,
                        reason: "构造 HTTP 客户端失败".to_owned(),
                    });
                }
            };
            self.summarize_over_http(http, input, &base_url, &model_name, api_key_env.as_deref())
                .await
        }
        #[cfg(not(feature = "external-protocols"))]
        {
            let _ = (input, base_url, model_name, api_key_env);
            Err(SlotCallError::ClientUnavailable {
                slot,
                reason:
                    "默认构建不含外部协议客户端（§19.6），需 cargo build --features external-protocols"
                        .to_owned(),
            })
        }
    }

    /// summarize 的 HTTP 调用：只在开启 external-protocols 时存在（§19.6）。
    #[cfg(feature = "external-protocols")]
    async fn summarize_over_http(
        &self,
        http: &reqwest::Client,
        input: &SummarizeInput,
        base_url: &str,
        model: &str,
        api_key_env: Option<&str>,
    ) -> Result<SummarizeOutcome, SlotCallError> {
        let slot = SlotKind::Summarize;
        let request = match text_protocol::build_request(input, model) {
            Ok(request) => request,
            Err(violation) => {
                return Err(SlotCallError::Contract {
                    slot,
                    reason: format!("请求构造失败（输入形态问题）：{}", violation.fact),
                });
            }
        };
        let url = text_protocol::endpoint(base_url);
        let mut builder = http.post(&url).json(&request);
        if let Some(env_name) = api_key_env
            && let Ok(value) = std::env::var(env_name)
        {
            builder = builder.bearer_auth(value);
        }
        let started = Instant::now();
        let response = match builder.send().await {
            Ok(response) => response,
            Err(err) => {
                let reason = format!("请求 {url} 失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                let reason = format!("读取 {url} 响应失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        if !status.is_success() {
            // summarize() 是同名的自由函数（截断错误响应体），不是本方法。
            let reason = format!("上游返回 {status}：{}", summarize(&body));
            self.record_failure(slot, reason.clone(), FailureKind::Transient);
            return Err(SlotCallError::Transport { slot, reason });
        }
        let parsed: text_protocol::ChatResponse = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                let reason = format!("响应不是合法 JSON：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        let output = match text_protocol::parse_output(&parsed, input) {
            Ok(output) => output,
            Err(violation) => {
                let reason = violation.fact.clone();
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        if let Err(violation) = output.validate(input) {
            let reason = violation.fact.clone();
            self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
            return Err(SlotCallError::Contract { slot, reason });
        }
        self.record_success(slot);
        Ok(SummarizeOutcome {
            output,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// rerank 槽位调用：绑定 → 准入 → 构造 → 发送 → 解析 → 契约校验 → 状态机。
    pub async fn rerank(&self, input: &RerankInput) -> Result<RerankOutcome, SlotCallError> {
        let slot = SlotKind::Rerank;
        let binding = self.cfg.bindings.iter().find(|b| b.slot == slot);
        let model_id = match binding.and_then(|b| b.model_id.as_ref()) {
            Some(model_id) => model_id.clone(),
            None => return Err(SlotCallError::NotBound { slot }),
        };
        let source = binding.and_then(|b| b.source).unwrap_or(ModelSource::Auto);
        // §3.10 准入：能力矩阵判「不可用」才拒绝；通用 LLM 的 ordered 适配器是
        // 「受限」但可用（分数非校准），由调用侧按 §18.1 标注。
        let capability = crate::capability::source_slot_capability(source, slot);
        if capability.grade == crate::capability::CapabilityGrade::Unavailable {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: format!(
                    "source {} 的 rerank 槽位能力={}：{}",
                    source.as_str(),
                    capability.grade.as_str(),
                    capability.reason
                ),
            });
        }
        let model = self.cfg.models.iter().find(|m| m.id == model_id);
        let base_url = model
            .and_then(|m| m.config.base_url.clone())
            .or_else(|| self.cfg.slm.http.as_ref().and_then(|h| h.base_url.clone()));
        let base_url = match base_url {
            Some(base_url) => base_url,
            None => {
                return Err(SlotCallError::ClientUnavailable {
                    slot,
                    reason: "模型没有配置 base_url".to_owned(),
                });
            }
        };
        let model_name = model
            .and_then(|m| m.config.model.clone())
            .unwrap_or(model_id);
        let api_key_env = model.and_then(|m| m.config.api_key_env.clone());

        if !self.health.lock().expect("健康状态锁中毒").is_usable(slot) {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: "槽位已因连续失败被禁用，改配置并重启后恢复".to_owned(),
            });
        }

        #[cfg(feature = "external-protocols")]
        {
            let http = match &self.http {
                Some(http) => http,
                None => {
                    return Err(SlotCallError::ClientUnavailable {
                        slot,
                        reason: "构造 HTTP 客户端失败".to_owned(),
                    });
                }
            };
            self.rerank_over_http(http, input, &base_url, &model_name, api_key_env.as_deref())
                .await
        }
        #[cfg(not(feature = "external-protocols"))]
        {
            let _ = (input, base_url, model_name, api_key_env);
            Err(SlotCallError::ClientUnavailable {
                slot,
                reason:
                    "默认构建不含外部协议客户端（§19.6），需 cargo build --features external-protocols"
                        .to_owned(),
            })
        }
    }

    /// rerank 的 HTTP 调用：只在开启 external-protocols 时存在（§19.6）。
    #[cfg(feature = "external-protocols")]
    async fn rerank_over_http(
        &self,
        http: &reqwest::Client,
        input: &RerankInput,
        base_url: &str,
        model: &str,
        api_key_env: Option<&str>,
    ) -> Result<RerankOutcome, SlotCallError> {
        let slot = SlotKind::Rerank;
        let request = match ordered_protocol::build_request(input, model) {
            Ok(request) => request,
            Err(violation) => {
                return Err(SlotCallError::Contract {
                    slot,
                    reason: format!("请求构造失败（输入形态问题）：{}", violation.fact),
                });
            }
        };
        let url = text_protocol::endpoint(base_url);
        let mut builder = http.post(&url).json(&request);
        if let Some(env_name) = api_key_env
            && let Ok(value) = std::env::var(env_name)
        {
            builder = builder.bearer_auth(value);
        }
        let started = Instant::now();
        let response = match builder.send().await {
            Ok(response) => response,
            Err(err) => {
                let reason = format!("请求 {url} 失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                let reason = format!("读取 {url} 响应失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        if !status.is_success() {
            // summarize() 是同名的自由函数（截断错误响应体），不是本方法。
            let reason = format!("上游返回 {status}：{}", summarize(&body));
            self.record_failure(slot, reason.clone(), FailureKind::Transient);
            return Err(SlotCallError::Transport { slot, reason });
        }
        let parsed: text_protocol::ChatResponse = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                let reason = format!("响应不是合法 JSON：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        let output = match ordered_protocol::parse_output(&parsed) {
            Ok(output) => output,
            Err(violation) => {
                let reason = violation.fact.clone();
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        // §3.2：覆盖全部输入、index 指向原始位置、按 score 降序。
        if let Err(violation) = output.validate(input) {
            let reason = violation.fact.clone();
            self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
            return Err(SlotCallError::Contract { slot, reason });
        }
        self.record_success(slot);
        Ok(RerankOutcome {
            output,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// vision 槽位调用：绑定 → 准入 → 构造 → 发送 → 解析 → 契约校验 → 状态机。
    pub async fn vision(&self, input: &VisionInput) -> Result<VisionOutcome, SlotCallError> {
        let slot = SlotKind::Vision;
        let binding = self.cfg.bindings.iter().find(|b| b.slot == slot);
        let model_id = match binding.and_then(|b| b.model_id.as_ref()) {
            Some(model_id) => model_id.clone(),
            None => return Err(SlotCallError::NotBound { slot }),
        };
        let source = binding.and_then(|b| b.source).unwrap_or(ModelSource::Auto);
        // §3.10 准入：只有能力矩阵判 Available 的来源可走 chat 多模态端点；
        // local / auto 的 embedded 后端本轮未实现（§19.6）。
        let capability = crate::capability::source_slot_capability(source, slot);
        if capability.grade != crate::capability::CapabilityGrade::Available {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: format!(
                    "source {} 的 vision 槽位能力={}：{}",
                    source.as_str(),
                    capability.grade.as_str(),
                    capability.reason
                ),
            });
        }
        let model = self.cfg.models.iter().find(|m| m.id == model_id);
        let base_url = model
            .and_then(|m| m.config.base_url.clone())
            .or_else(|| self.cfg.slm.http.as_ref().and_then(|h| h.base_url.clone()));
        let base_url = match base_url {
            Some(base_url) => base_url,
            None => {
                return Err(SlotCallError::ClientUnavailable {
                    slot,
                    reason: "模型没有配置 base_url".to_owned(),
                });
            }
        };
        let model_name = model
            .and_then(|m| m.config.model.clone())
            .unwrap_or(model_id);
        let api_key_env = model.and_then(|m| m.config.api_key_env.clone());

        if !self.health.lock().expect("健康状态锁中毒").is_usable(slot) {
            return Err(SlotCallError::ClientUnavailable {
                slot,
                reason: "槽位已因连续失败被禁用，改配置并重启后恢复".to_owned(),
            });
        }

        #[cfg(feature = "external-protocols")]
        {
            let http = match &self.http {
                Some(http) => http,
                None => {
                    return Err(SlotCallError::ClientUnavailable {
                        slot,
                        reason: "构造 HTTP 客户端失败".to_owned(),
                    });
                }
            };
            self.vision_over_http(http, input, &base_url, &model_name, api_key_env.as_deref())
                .await
        }
        #[cfg(not(feature = "external-protocols"))]
        {
            let _ = (input, base_url, model_name, api_key_env);
            Err(SlotCallError::ClientUnavailable {
                slot,
                reason:
                    "默认构建不含外部协议客户端（§19.6），需 cargo build --features external-protocols"
                        .to_owned(),
            })
        }
    }

    /// vision 的 HTTP 调用：只在开启 external-protocols 时存在（§19.6）。
    #[cfg(feature = "external-protocols")]
    async fn vision_over_http(
        &self,
        http: &reqwest::Client,
        input: &VisionInput,
        base_url: &str,
        model: &str,
        api_key_env: Option<&str>,
    ) -> Result<VisionOutcome, SlotCallError> {
        let slot = SlotKind::Vision;
        let request = match vision_protocol::build_request(input, model) {
            Ok(request) => request,
            Err(violation) => {
                return Err(SlotCallError::Contract {
                    slot,
                    reason: format!("请求构造失败（输入形态问题）：{}", violation.fact),
                });
            }
        };
        let url = text_protocol::endpoint(base_url);
        let mut builder = http.post(&url).json(&request);
        if let Some(env_name) = api_key_env
            && let Ok(value) = std::env::var(env_name)
        {
            builder = builder.bearer_auth(value);
        }
        let started = Instant::now();
        let response = match builder.send().await {
            Ok(response) => response,
            Err(err) => {
                let reason = format!("请求 {url} 失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                let reason = format!("读取 {url} 响应失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        if !status.is_success() {
            // summarize() 是同名的自由函数（截断错误响应体），不是本方法。
            let reason = format!("上游返回 {status}：{}", summarize(&body));
            self.record_failure(slot, reason.clone(), FailureKind::Transient);
            return Err(SlotCallError::Transport { slot, reason });
        }
        let parsed: text_protocol::ChatResponse = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                let reason = format!("响应不是合法 JSON：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        let output = match vision_protocol::parse_output(&parsed, input) {
            Ok(output) => output,
            Err(violation) => {
                let reason = violation.fact.clone();
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        // §3.2：task=ground 时 boxes 必填且 bbox 有限非负。
        if let Err(violation) = output.validate(input) {
            let reason = violation.fact.clone();
            self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
            return Err(SlotCallError::Contract { slot, reason });
        }
        self.record_success(slot);
        Ok(VisionOutcome {
            output,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// 真正的 HTTP 调用：只在开启 external-protocols 时存在（§19.6）。
    #[cfg(feature = "external-protocols")]
    async fn decide_over_http(
        &self,
        http: &reqwest::Client,
        input: &DecideInput,
        base_url: &str,
        api_key_env: Option<&str>,
    ) -> Result<DecideOutcome, SlotCallError> {
        let slot = SlotKind::Decide;

        // 请求构造失败是「我们的请求不合法」，不是模型不可用：不计入槽位失败，健康状态不变。
        let request = match systemone::build_request(input) {
            Ok(request) => request,
            Err(violation) => {
                let reason = format!("请求构造失败（输入形态问题）：{}", violation.fact);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };

        let url = systemone::endpoint(base_url);
        let mut builder = http.post(&url).json(&request);
        // 密钥只从环境读，不落配置、不进日志（§18.1）。
        if let Some(env_name) = api_key_env
            && let Ok(value) = std::env::var(env_name)
        {
            builder = builder.bearer_auth(value);
        }

        let started = Instant::now();
        let response = match builder.send().await {
            Ok(response) => response,
            Err(err) => {
                let reason = format!("请求 {url} 失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                let reason = format!("读取 {url} 响应失败：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::Transient);
                return Err(SlotCallError::Transport { slot, reason });
            }
        };
        if !status.is_success() {
            let reason = format!("上游返回 {status}：{}", summarize(&body));
            self.record_failure(slot, reason.clone(), FailureKind::Transient);
            return Err(SlotCallError::Transport { slot, reason });
        }

        let parsed: systemone::SystemOneResponse = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                let reason = format!("响应不是合法 JSON：{err}");
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        let output = match systemone::parse_decide_output(&parsed, &input.questions) {
            Ok(output) => output,
            Err(violation) => {
                let reason = violation.fact.clone();
                self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
                return Err(SlotCallError::Contract { slot, reason });
            }
        };
        if let Err(violation) = output.validate(input) {
            let reason = violation.fact.clone();
            self.record_failure(slot, reason.clone(), FailureKind::ShapeMismatch);
            return Err(SlotCallError::Contract { slot, reason });
        }

        self.record_success(slot);
        let gate = gate_for_output(&output);
        Ok(DecideOutcome {
            output,
            gate,
            latency_ms: started.elapsed().as_millis() as u64,
            calibrated: true,
        })
    }

    /// 记一次成功：首次真实调用即视为校验通过（§3.11）。
    #[cfg(feature = "external-protocols")]
    fn record_success(&self, slot: SlotKind) {
        let mut health = self.health.lock().expect("健康状态锁中毒");
        health.on_success(slot);
    }

    /// 记一次失败：形状不符直接禁用，其余按连续失败计数（§3.11）。
    #[cfg(feature = "external-protocols")]
    fn record_failure(&self, slot: SlotKind, reason: String, kind: FailureKind) {
        let mut health = self.health.lock().expect("健康状态锁中毒");
        health.on_failure(slot, reason, kind);
    }
}

/// 多问题时取各问题概率的最小值作为本次结论（最不确定的那个决定结论，§7.2）。
pub fn gate_for_output(output: &DecideOutput) -> ConfidenceGate {
    let min = output
        .answers
        .values()
        .map(|answer| answer.probability)
        .fold(f64::INFINITY, f64::min);
    confidence_gate(min)
}

/// 上游响应体的摘要：只留前若干字符，避免把大 body 塞进错误信息。
#[cfg(feature = "external-protocols")]
fn summarize(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= 200 {
        return trimmed.to_owned();
    }
    let head: String = trimmed.chars().take(200).collect();
    format!("{head}…")
}

/// 测试用：造一个绑定了 systemone 模型的配置。
#[cfg(test)]
fn systemone_config() -> ResolvedConfig {
    use std::collections::BTreeMap;

    use crate::config::{ModelConfig, ModelEntry, SlmConfig, SlotBinding};
    ResolvedConfig {
        models: vec![ModelEntry {
            id: "jev".to_owned(),
            config: ModelConfig {
                source: Some(ModelSource::SystemOne),
                mode: None,
                adapter: None,
                path: None,
                base_url: Some("http://127.0.0.1:8090".to_owned()),
                model: None,
                api_key_env: None,
                allow_remote: false,
                serves: vec![SlotKind::Decide],
            },
        }],
        slots: BTreeMap::new(),
        slm: SlmConfig {
            mode: None,
            probe: None,
            disable_after_failures: None,
            embedded: None,
            http: None,
        },
        bindings: vec![SlotBinding {
            slot: SlotKind::Decide,
            model_id: Some("jev".to_owned()),
            source: Some(ModelSource::SystemOne),
            mode: None,
            adapter: None,
            explicit: false,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::DecideAnswer;
    use serde_json::json;

    fn output(probabilities: &[(&str, f64)]) -> DecideOutput {
        DecideOutput {
            answers: probabilities
                .iter()
                .map(|(key, probability)| {
                    (
                        (*key).to_owned(),
                        DecideAnswer {
                            probability: *probability,
                            span: None,
                        },
                    )
                })
                .collect(),
        }
    }

    /// §7.2：多问题取最小概率作为整体结论。
    #[test]
    fn gate_uses_minimum_probability_across_questions() {
        assert_eq!(
            gate_for_output(&output(&[("a", 0.97), ("b", 0.70)])),
            ConfidenceGate::AdoptWithLowConfidence
        );
        assert_eq!(
            gate_for_output(&output(&[("a", 0.97), ("b", 0.92)])),
            ConfidenceGate::Adopt
        );
        assert_eq!(
            gate_for_output(&output(&[("a", 0.97), ("b", 0.10)])),
            ConfidenceGate::BothViews
        );
    }

    /// §7.2：三档边界。
    #[test]
    fn gate_boundaries_match_seven_two() {
        assert_eq!(
            gate_for_output(&output(&[("a", 0.85)])),
            ConfidenceGate::Adopt
        );
        assert_eq!(
            gate_for_output(&output(&[("a", 0.849)])),
            ConfidenceGate::AdoptWithLowConfidence
        );
        assert_eq!(
            gate_for_output(&output(&[("a", 0.60)])),
            ConfidenceGate::AdoptWithLowConfidence
        );
        assert_eq!(
            gate_for_output(&output(&[("a", 0.599)])),
            ConfidenceGate::BothViews
        );
    }

    fn classify_config(source: &str) -> crate::config::ResolvedConfig {
        crate::config::parse_config(
            &format!(
                "models:
  llm:
    source: {source}
    base_url: http://127.0.0.1:8090/v1
    model: demo
    serves: [classify]
"
            ),
            "test.yaml",
        )
        .expect("配置应当解析成功")
    }

    fn classify_input() -> ClassifyInput {
        ClassifyInput {
            text: "一段构建日志".to_owned(),
            labels: vec!["has_error".to_owned(), "is_binary".to_owned()],
        }
    }

    /// §3.8：classify 槽位未绑定模型时返回 NotBound。
    #[tokio::test]
    async fn classify_without_binding_reports_not_bound() {
        let cfg = crate::config::parse_config("", "test.yaml").unwrap();
        let client = SlotClient::new(cfg);
        match client.classify(&classify_input()).await {
            Err(SlotCallError::NotBound { slot }) => assert_eq!(slot, SlotKind::Classify),
            other => panic!("应当是 NotBound：{other:?}"),
        }
    }

    /// §3.10 准入：systemone 的 choice 映射未落，classify 受限，不是 Available。
    #[tokio::test]
    async fn classify_with_systemone_source_is_rejected() {
        let client = SlotClient::new(classify_config("systemone"));
        match client.classify(&classify_input()).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Classify);
                assert!(reason.contains("limited"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    /// §19.6：默认构建不含外部协议客户端，classify 走降级。
    #[cfg(not(feature = "external-protocols"))]
    #[tokio::test]
    async fn classify_without_feature_reports_client_unavailable() {
        let client = SlotClient::new(classify_config("openai"));
        match client.classify(&classify_input()).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Classify);
                assert!(reason.contains("external-protocols"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    fn summarize_config(source: &str) -> crate::config::ResolvedConfig {
        crate::config::parse_config(
            &format!(
                "models:
  llm:
    source: {source}
    base_url: http://127.0.0.1:8090/v1
    model: demo
    serves: [summarize]
"
            ),
            "test.yaml",
        )
        .expect("配置应当解析成功")
    }

    /// §3.8：summarize 槽位未绑定模型时返回 NotBound。
    #[tokio::test]
    async fn summarize_without_binding_reports_not_bound() {
        let cfg = crate::config::parse_config("", "test.yaml").unwrap();
        let client = SlotClient::new(cfg);
        let input = SummarizeInput {
            text: "一段日志".to_owned(),
            schema: None,
            max_tokens: None,
        };
        match client.summarize(&input).await {
            Err(SlotCallError::NotBound { slot }) => assert_eq!(slot, SlotKind::Summarize),
            other => panic!("应当是 NotBound：{other:?}"),
        }
    }

    /// §3.10 准入：systemone 只做判定、不生成文本，summarize 不可用。
    #[tokio::test]
    async fn summarize_with_systemone_source_is_rejected() {
        let client = SlotClient::new(summarize_config("systemone"));
        let input = SummarizeInput {
            text: "一段日志".to_owned(),
            schema: None,
            max_tokens: None,
        };
        match client.summarize(&input).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Summarize);
                assert!(reason.contains("unavailable"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    /// §19.6：默认构建不含外部协议客户端，summarize 走降级。
    #[cfg(not(feature = "external-protocols"))]
    #[tokio::test]
    async fn summarize_without_feature_reports_client_unavailable() {
        let client = SlotClient::new(summarize_config("openai"));
        let input = SummarizeInput {
            text: "一段日志".to_owned(),
            schema: None,
            max_tokens: None,
        };
        match client.summarize(&input).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Summarize);
                assert!(reason.contains("external-protocols"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    fn rerank_config(source: &str) -> crate::config::ResolvedConfig {
        crate::config::parse_config(
            &format!(
                "models:
  llm:
    source: {source}
    base_url: http://127.0.0.1:8090/v1
    model: demo
    serves: [rerank]
"
            ),
            "test.yaml",
        )
        .expect("配置应当解析成功")
    }

    fn rerank_input() -> RerankInput {
        RerankInput {
            query: "超时".to_owned(),
            documents: vec!["a.rs:1:timeout".to_owned(), "b.rs:3:error".to_owned()],
            top_n: None,
        }
    }

    /// §3.8：rerank 槽位未绑定模型时返回 NotBound。
    #[tokio::test]
    async fn rerank_without_binding_reports_not_bound() {
        let cfg = crate::config::parse_config("", "test.yaml").unwrap();
        let client = SlotClient::new(cfg);
        match client.rerank(&rerank_input()).await {
            Err(SlotCallError::NotBound { slot }) => assert_eq!(slot, SlotKind::Rerank),
            other => panic!("应当是 NotBound：{other:?}"),
        }
    }

    /// §3.10 准入：systemone 无排序接口，rerank 不可用。
    #[tokio::test]
    async fn rerank_with_systemone_source_is_rejected() {
        let client = SlotClient::new(rerank_config("systemone"));
        match client.rerank(&rerank_input()).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Rerank);
                assert!(reason.contains("unavailable"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    /// §19.6：默认构建不含外部协议客户端，rerank 走降级。
    #[cfg(not(feature = "external-protocols"))]
    #[tokio::test]
    async fn rerank_without_feature_reports_client_unavailable() {
        let client = SlotClient::new(rerank_config("openai"));
        match client.rerank(&rerank_input()).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Rerank);
                assert!(reason.contains("external-protocols"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    fn vision_config(source: &str) -> crate::config::ResolvedConfig {
        crate::config::parse_config(
            &format!(
                "models:
  llm:
    source: {source}
    base_url: http://127.0.0.1:8090/v1
    model: demo
    path: /tmp/model
    serves: [vision]
"
            ),
            "test.yaml",
        )
        .expect("配置应当解析成功")
    }

    fn vision_input() -> VisionInput {
        VisionInput {
            image: serde_json::json!({
                "type": "image_url",
                "image_url": { "url": "data:image/png;base64,AA" }
            }),
            task: crate::slots::VisionTask::Describe,
            query: None,
        }
    }

    /// §3.8：vision 槽位未绑定模型时返回 NotBound。
    #[tokio::test]
    async fn vision_without_binding_reports_not_bound() {
        let cfg = crate::config::parse_config("", "test.yaml").unwrap();
        let client = SlotClient::new(cfg);
        match client.vision(&vision_input()).await {
            Err(SlotCallError::NotBound { slot }) => assert_eq!(slot, SlotKind::Vision),
            other => panic!("应当是 NotBound：{other:?}"),
        }
    }

    /// §3.10 准入：local 来源的 embedded 后端未实现，vision 不可用。
    #[tokio::test]
    async fn vision_with_local_source_is_rejected() {
        let client = SlotClient::new(vision_config("local"));
        match client.vision(&vision_input()).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Vision);
                assert!(reason.contains("unspecified"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    /// §19.6：默认构建不含外部协议客户端，vision 走降级。
    #[cfg(not(feature = "external-protocols"))]
    #[tokio::test]
    async fn vision_without_feature_reports_client_unavailable() {
        let client = SlotClient::new(vision_config("systemone"));
        match client.vision(&vision_input()).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Vision);
                assert!(reason.contains("external-protocols"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    /// 槽位未绑定模型时返回 NotBound（§3.8）。
    #[tokio::test]
    async fn decide_without_binding_reports_not_bound() {
        let mut cfg = systemone_config();
        cfg.bindings[0].model_id = None;
        let client = SlotClient::new(cfg);
        let input = DecideInput {
            state: json!("一段日志"),
            images: None,
            questions: vec!["有错吗".to_owned()],
        };
        match client.decide(&input).await {
            Err(SlotCallError::NotBound { slot }) => assert_eq!(slot, SlotKind::Decide),
            other => panic!("应当是 NotBound：{other:?}"),
        }
    }

    /// 来源不是 systemone 时返回 ClientUnavailable，并写清原因。
    #[tokio::test]
    async fn decide_with_unsupported_source_reports_client_unavailable() {
        let mut cfg = systemone_config();
        cfg.bindings[0].source = Some(ModelSource::Openai);
        let client = SlotClient::new(cfg);
        let input = DecideInput {
            state: json!("一段日志"),
            images: None,
            questions: vec!["有错吗".to_owned()],
        };
        match client.decide(&input).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Decide);
                assert!(reason.contains("openai"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    /// 默认构建不含外部协议客户端：decide 返回 ClientUnavailable（§19.6）。
    #[cfg(not(feature = "external-protocols"))]
    #[tokio::test]
    async fn decide_without_feature_reports_client_unavailable() {
        let client = SlotClient::new(systemone_config());
        let input = DecideInput {
            state: json!("一段日志"),
            images: None,
            questions: vec!["有错吗".to_owned()],
        };
        match client.decide(&input).await {
            Err(SlotCallError::ClientUnavailable { slot, reason }) => {
                assert_eq!(slot, SlotKind::Decide);
                assert!(reason.contains("external-protocols"), "{reason}");
            }
            other => panic!("应当是 ClientUnavailable：{other:?}"),
        }
    }

    /// 开启 feature 时也不该在真实网络前失败——这里只验证编排能走到发送阶段之前的错误分支。
    #[cfg(feature = "external-protocols")]
    #[tokio::test]
    async fn decide_over_http_reports_transport_error_for_dead_endpoint() {
        let mut cfg = systemone_config();
        // 指向本机必然连不通的端口（不发真实请求到模型服务）。
        cfg.models[0].config.base_url = Some("http://127.0.0.1:1".to_owned());
        let client = SlotClient::new(cfg);
        let input = DecideInput {
            state: json!("一段日志"),
            images: None,
            questions: vec!["有错吗".to_owned()],
        };
        match client.decide(&input).await {
            Err(SlotCallError::Transport { slot, .. }) => assert_eq!(slot, SlotKind::Decide),
            other => panic!("应当是 Transport：{other:?}"),
        }
        // 传输失败计入连续失败计数（§3.11）。
        let health = client.health();
        let decide_health = health
            .iter()
            .find(|(kind, _)| *kind == SlotKind::Decide)
            .unwrap();
        assert_eq!(decide_health.1.failure_count, 1);
    }
}
