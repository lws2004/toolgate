//! 本地 SLM 槽位后端（§3.8 降级链第一档）：把同步 LocalSlots 回调桥接到异步 SlotClient。
//!
//! daemon 的 route 在 tokio 任务内同步执行，不能在 runtime 内直接 block_on（会 panic）。
//! 故持一份专属 Runtime，方法在独立 OS 线程上 block_on 该 Runtime 的 future。

use std::cmp::Ordering;
use std::future::Future;
use std::sync::Arc;

use toolgate_core::govern::LocalSlots;
use toolgate_core::slm::SlotClient;
use toolgate_core::slots::{ClassifyInput, RerankInput, RerankResult, SummarizeInput};

/// 上游 /v1/rerank 单请求文档上限（jina 服务 --rerank-max-documents 实测 1000）。
const RERANK_BATCH_DOCUMENTS: usize = 1000;

/// 本地槽位后端：持 SlotClient 与专属 Runtime。
pub struct LocalSlotBackend {
    client: Arc<SlotClient>,
    rt: Arc<tokio::runtime::Runtime>,
}

impl LocalSlotBackend {
    pub fn new(client: Arc<SlotClient>) -> Option<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .ok()?;
        Some(Self {
            client,
            rt: Arc::new(rt),
        })
    }

    /// 预热 summarize 模型：冷启可能 20s 级，daemon 启动后后台调用一次。
    pub fn prewarm(&self) {
        let client = Arc::clone(&self.client);
        let rt = Arc::clone(&self.rt);
        std::thread::spawn(move || {
            let input = SummarizeInput {
                text: "预热".to_owned(),
                schema: None,
                max_tokens: Some(8),
            };
            let _ = rt.block_on(async move { client.summarize(&input).await.ok() });
        });
    }

    /// 在独立线程上 block_on 给定 future，规避「runtime 内 block_on」panic。
    /// 返回值再摊平一层 Option（future 内已 .ok() 成 Option）。
    fn run<F, T>(&self, fut: F) -> Option<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let rt = Arc::clone(&self.rt);
        std::thread::spawn(move || rt.block_on(fut)).join().ok()
    }
}

impl LocalSlots for LocalSlotBackend {
    fn summarize(&self, text: &str) -> Option<String> {
        let input = SummarizeInput {
            text: text.to_owned(),
            schema: None,
            max_tokens: Some(512),
        };
        let client = Arc::clone(&self.client);
        self.run(async move { client.summarize(&input).await.ok() })
            .flatten()
            .and_then(|outcome| outcome.output.text)
    }

    fn classify_shape(&self, text: &str) -> Option<String> {
        let sample: String = text.chars().take(2000).collect();
        let input = ClassifyInput {
            text: sample,
            labels: vec![
                "long_text".to_owned(),
                "listing".to_owned(),
                "error".to_owned(),
                "pass_through".to_owned(),
            ],
        };
        let client = Arc::clone(&self.client);
        let scores = self
            .run(async move { client.classify(&input).await.ok() })
            .flatten()?
            .output
            .scores;
        let (label, score) = scores
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))?;
        if *score >= 0.5 {
            Some(label.clone())
        } else {
            None
        }
    }

    fn rerank(&self, text: &str, limit: usize) -> Option<String> {
        let documents: Vec<String> = text.lines().map(str::to_owned).collect();
        if documents.is_empty() {
            return None;
        }
        // §6.4 输入有界：按上游单请求文档上限分片，逐批取回分数后全局合并排序。
        let mut batches: Vec<(usize, Vec<RerankResult>)> = Vec::new();
        for (offset, chunk) in documents.chunks(RERANK_BATCH_DOCUMENTS).enumerate() {
            let input = RerankInput {
                query: "保留与工具输出最相关、最有信息量的条目".to_owned(),
                documents: chunk.to_vec(),
                top_n: None,
            };
            let client = Arc::clone(&self.client);
            let out = self
                .run(async move { client.rerank(&input).await.ok() })
                .flatten()?;
            batches.push((offset * RERANK_BATCH_DOCUMENTS, out.output.results));
        }
        let ranked = merge_ranked(&batches, limit);
        let mut picked: Vec<&str> = Vec::new();
        for index in ranked {
            if let Some(doc) = documents.get(index) {
                picked.push(doc.as_str());
            }
        }
        if picked.is_empty() {
            None
        } else {
            Some(picked.join("\n"))
        }
    }
}

/// 把各批的局部 index 加回全局偏移，按分数降序（同分按原 index 升序）取前 limit 个。
fn merge_ranked(batches: &[(usize, Vec<RerankResult>)], limit: usize) -> Vec<usize> {
    let mut all: Vec<(usize, f64)> = Vec::new();
    for (base, results) in batches {
        for result in results {
            all.push((base.saturating_add(result.index), result.score));
        }
    }
    all.sort_by(
        |a, b| match b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal) {
            Ordering::Equal => a.0.cmp(&b.0),
            ordering => ordering,
        },
    );
    all.into_iter()
        .take(limit)
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(index: usize, score: f64) -> RerankResult {
        RerankResult { index, score }
    }

    #[test]
    fn merge_ranked_offsets_and_orders_globally() {
        let batches = vec![
            (0, vec![result(0, 0.2), result(1, 0.9)]),
            (1000, vec![result(0, 0.5), result(1, 0.1)]),
        ];
        assert_eq!(merge_ranked(&batches, 3), vec![1, 1000, 0]);
    }

    #[test]
    fn merge_ranked_caps_at_limit_and_breaks_ties_by_index() {
        let batches = vec![(0, vec![result(1, 0.4), result(0, 0.4)])];
        assert_eq!(merge_ranked(&batches, 1), vec![0]);
    }
}
