//! §14 阶段 5 / §21.7：本地 agent 消费形态下的行为校验。
//!
//! 不经过 MCP 传输，直接调 `dispatch`——验证「程序消费」依赖的四个契约：
//! intent=raw 不触发模型、错误带 code + nextSteps、structuredContent 稳定、
//! 告知同时进 content 与 structuredContent（§4.4b）。

use std::path::PathBuf;

use serde_json::{Value, json};
use toolgate_core::error::govern_error;
use toolgate_core::tools::{Content, ToolContext, ToolResponse, dispatch};

/// 每个测试一个独立工作区（不引入 tempfile 依赖，§1.5）。
fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toolgate-agent-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时钟")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时工作区");
    dir
}

fn text_of(resp: &ToolResponse) -> String {
    resp.content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.as_str()),
            Content::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// §21.7.1 / §4.2：intent=raw（缺省）时六个工具都不调模型，状态如实为 unavailable，
/// 也不产生 decision / summary / 非空 confidence——本地 agent 形态靠这条才可预测。
#[tokio::test]
async fn raw_intent_keeps_every_tool_off_the_model() {
    let dir = workspace("raw");
    std::fs::write(dir.join("a.txt"), "hello\n").expect("写测试文件");
    let ctx = ToolContext::new(&dir).expect("上下文");

    let cases = [
        ("read", json!({ "path": "a.txt" })),
        ("write", json!({ "path": "b.txt", "content": "x\n" })),
        (
            "edit",
            json!({ "path": "a.txt", "edits": [{ "oldText": "hello", "newText": "hi" }] }),
        ),
        ("search", json!({ "mode": "content", "pattern": "hi" })),
        ("bash", json!({ "binary": "/bin/echo", "args": ["ok"] })),
        ("task", json!({ "action": "list" })),
    ];
    for (tool, args) in cases {
        let resp = dispatch(tool, &args, &ctx)
            .await
            .unwrap_or_else(|e| panic!("{tool} 在槽位全空时应当成功：{e:?}"));
        let status = resp.meta["slmStatus"]
            .as_object()
            .unwrap_or_else(|| panic!("{tool} 缺 slmStatus：{:?}", resp.meta));
        assert_eq!(status.len(), 6, "{tool} slmStatus 应有六个槽位");
        for (slot, state) in status {
            assert_eq!(
                state, "unavailable",
                "{tool}/{slot} 槽位全空时必须 unavailable（§7.3 状态要真实）"
            );
        }
        assert!(
            resp.meta["confidence"].is_null(),
            "{tool} 没有判定时 confidence 必须是 null（§4.6）"
        );
        assert!(
            resp.meta.get("decision").is_none(),
            "{tool} raw 不应产生判定事实"
        );
        assert!(
            resp.meta.get("summary").is_none(),
            "{tool} raw 不应产生摘要"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// §1.4.5 第三层 / §21.7.3：错误必须带稳定 errorCode 与非空 nextSteps，
/// 本地 agent 才能按 code 分支、按 steps 自行修正。
#[tokio::test]
async fn errors_carry_code_and_next_steps() {
    let dir = workspace("err");
    let ctx = ToolContext::new(&dir).expect("上下文");

    let cases: [(&str, Value, &str); 8] = [
        ("nope", json!({}), "UNKNOWN_TOOL"),
        ("read", json!({ "path": "missing.txt" }), "PATH_NOT_FOUND"),
        ("read", json!({ "path": "/etc/hosts" }), "ISOLATION_REFUSED"),
        ("bash", json!({}), "BAD_ARGUMENT"),
        ("read", json!({ "path": "a.txt", "intent": "bogus" }), "BAD_ARGUMENT"),
        ("read", json!({ "path": "." }), "BAD_ARGUMENT"),
        // 超时是 TIMEOUT，不是笼统 FAILED；下一步要求缩小动作或加时。
        (
            "bash",
            json!({ "binary": "sleep", "args": ["2"], "timeout_secs": 1 }),
            "TIMEOUT",
        ),
        // 任务不存在是调用方输入错，不是内部故障。
        (
            "task",
            json!({ "action": "read", "task_id": "missing-task" }),
            "BAD_ARGUMENT",
        ),
    ];
    for (tool, args, expected) in cases {
        let err = match dispatch(tool, &args, &ctx).await {
            Ok(resp) => panic!("{tool} 应当报错，实际成功：{:?}", resp.meta),
            Err(e) => e,
        };
        let view = govern_error(&err);
        assert_eq!(view.code, expected, "{tool} 的错误码");
        assert!(!view.fact.is_empty(), "{tool} 缺事实");
        assert!(!view.next_steps.is_empty(), "{tool} 缺可执行下一步");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// §5.1 L1.5 / §17.2：输出敏感度只标注不改原文；classify 不可用时回退机械并标 source。
#[tokio::test]
async fn sensitive_output_is_marked_without_redaction() {
    let dir = workspace("sensitive");
    let mut text = String::new();
    for i in 0..600 {
        text.push_str(&format!("line {i}: user@example.com\n"));
    }
    std::fs::write(dir.join("contacts.txt"), &text).expect("写测试文件");
    let ctx = ToolContext::new(&dir).expect("上下文");

    let resp = dispatch(
        "bash",
        &json!({ "binary": "/bin/cat", "args": ["contacts.txt"], "intent": "auto" }),
        &ctx,
    )
    .await
    .expect("bash 读取");
    let body = text_of(&resp);

    assert!(
        body.contains("user@example.com"),
        "只标注，不改原文：{body}"
    );
    assert_eq!(resp.meta["sensitivity"]["level"], "personal");
    assert_eq!(resp.meta["sensitivity"]["source"], "mechanical");
    let notices = resp.meta["notices"].as_array().expect("notices 数组");
    assert!(
        notices
            .iter()
            .any(|n| n["code"] == "SENSITIVE" && n["params"]["level"] == "personal"),
        "缺 SENSITIVE 告知：{notices:?}"
    );
    assert!(
        body.contains("[SENSITIVE] level=personal"),
        "content 通道缺 SENSITIVE：{body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// §21.7.2：结构化事实字段名是程序契约，不能随实现漂移。
#[tokio::test]
async fn structured_content_stays_stable() {
    let dir = workspace("facts");
    let ctx = ToolContext::new(&dir).expect("上下文");

    let write = dispatch(
        "write",
        &json!({ "path": "f.txt", "content": "a\nb\n" }),
        &ctx,
    )
    .await
    .expect("write 成功");
    let facts = &write.meta["facts"];
    assert!(facts["changed"].is_object(), "write facts.changed");
    assert!(facts["file"].is_object(), "write facts.file");
    // §5.7：同目录没有构建描述文件时 nearby 是说明字符串，有则给清单数组。
    assert!(
        facts["nearby"].is_array() || facts["nearby"].is_string(),
        "write facts.nearby"
    );
    assert!(facts["diff"].is_string(), "write facts.diff");

    let edit = dispatch(
        "edit",
        &json!({ "path": "f.txt", "edits": [{ "oldText": "a", "newText": "A" }] }),
        &ctx,
    )
    .await
    .expect("edit 成功");
    assert_eq!(edit.meta["changed"], json!(true));
    assert!(edit.meta.get("snapshot").is_some(), "edit 返回快照");
    assert!(edit.meta["facts"]["diff"].is_string(), "edit facts.diff");

    let search = dispatch("search", &json!({ "mode": "content", "pattern": "A" }), &ctx)
        .await
        .expect("search 成功");
    assert!(search.meta["hits"].is_u64(), "search hits");
    assert_eq!(search.meta["rerank"], json!("skipped"));

    let bash = dispatch("bash", &json!({ "binary": "/bin/echo", "args": ["ok"] }), &ctx)
        .await
        .expect("bash 成功");
    assert!(bash.meta["exitCode"].is_i64(), "bash exitCode");
    assert_eq!(bash.meta["success"], json!(true));

    let read = dispatch("read", &json!({ "path": "f.txt" }), &ctx)
        .await
        .expect("read 成功");
    assert!(read.meta["totalLines"].is_u64(), "read totalLines");
    let _ = std::fs::remove_dir_all(&dir);
}

/// §4.4b：告知是一个事实源、两个通道——content 给模型读，structuredContent 给程序判断。
#[tokio::test]
async fn notices_go_to_both_channels() {
    let dir = workspace("notices");
    let ctx = ToolContext::new(&dir).expect("上下文");
    // 超过截断阈值（2000 行 / 50KB）→ TRUNCATED 告知。
    let big = "x".repeat(80_000);
    let resp = dispatch(
        "bash",
        &json!({ "binary": "/bin/echo", "args": [big] }),
        &ctx,
    )
    .await
    .expect("bash 成功");

    let notices = resp.meta["notices"]
        .as_array()
        .unwrap_or_else(|| panic!("缺 notices：{:?}", resp.meta));
    assert!(!notices.is_empty(), "截断必须有告知");
    let codes: Vec<&str> = notices
        .iter()
        .filter_map(|n| n["code"].as_str())
        .collect();
    assert!(codes.contains(&"TRUNCATED"), "告知码应含 TRUNCATED：{codes:?}");
    let body = text_of(&resp);
    assert!(
        body.contains("TRUNCATED") || body.contains("截断"),
        "content 通道也要有告知：{body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// §5.2：files 只枚举路径——绑定语义槽位、给了 query 且 intent=auto 也不重排、
/// 不报 SLOT_UNAVAILABLE。补 `plan` 单测覆盖不到的工具级路径（绑定与否都不改变行为）。
#[tokio::test]
async fn files_mode_skips_rerank_even_with_bound_slots() {
    use std::sync::Arc;

    use toolgate_core::config::load_config;
    use toolgate_core::slm::SlotClient;

    let dir = workspace("files-slot");
    std::fs::write(dir.join("alpha.txt"), "alpha\n").expect("写测试文件");
    let cfg_path = dir.join("config.yaml");
    std::fs::write(
        &cfg_path,
        "models:\n  fake-emb:\n    source: embedding\n    mode: http\n    base_url: http://127.0.0.1:1\n    model: fake\n    serves: [embedding]\n",
    )
    .expect("写配置");
    let cfg = load_config(&cfg_path).expect("配置合法").expect("配置存在");
    let mut ctx = ToolContext::new(&dir).expect("上下文");
    ctx.slm = Some(Arc::new(SlotClient::new(cfg)));

    let resp = dispatch(
        "search",
        &json!({
            "mode": "files",
            "pattern": "txt",
            "query": "按语义排序",
            "intent": "auto",
            "path": "."
        }),
        &ctx,
    )
    .await
    .expect("search 成功");

    assert_eq!(resp.meta["rerank"], json!("skipped"));
    assert!(resp.meta["embedding"].is_null(), "files 不应拿 embedding");
    let body = text_of(&resp);
    assert!(
        !body.contains("SLOT_UNAVAILABLE"),
        "files 不应报槽位不可用：{body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
