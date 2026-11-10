use toolgate_core::config::parse_config;
use toolgate_core::slm::SlotClient;
use toolgate_core::slots::{ClassifyInput, SlotKind};

#[tokio::main]
async fn main() {
    let yaml = "models:\n  d3:\n    source: systemone\n    base_url: http://127.0.0.1:8082\n    model: d3-nano\n    mode: http\n    serves: [classify]\n";
    let cfg = parse_config(yaml, "e2e.yaml").expect("配置解析失败");
    let binding = cfg
        .bindings
        .iter()
        .find(|b| b.slot == SlotKind::Classify)
        .unwrap();
    println!(
        "绑定: model={:?} source={:?} adapter={:?}",
        binding.model_id, binding.source, binding.adapter
    );
    let client = SlotClient::new(cfg);
    let input = ClassifyInput {
        text: "一段构建日志：error: cannot find crate foo".to_owned(),
        labels: vec!["has_error".to_owned(), "is_binary".to_owned()],
    };
    match client.classify(&input).await {
        Ok(out) => println!(
            "结果: scores={:?} calibrated={} latency_ms={}",
            out.output.scores, out.calibrated, out.latency_ms
        ),
        Err(e) => {
            eprintln!("调用失败: {e:?}");
            std::process::exit(1);
        }
    }
}
