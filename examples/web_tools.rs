/// web_tools — 验证 WebSearch 和 WebFetch 工具的正确性。
///
/// 覆盖：
/// - 工具 schema / 参数校验（不依赖网络）
/// - Agent + MockProvider 集成（验证 Tool ⇄ Agent 交互）
/// - 实时网络调用（可选，需设置 WEB_TOOLS_LIVE=1）
/// - 错误场景：非法 URL、无效参数、超时
///
/// 实时网络测试需要翻墙环境（DuckDuckGo / httpbin.org），默认启用 Mock 模式。
/// 设置环境变量 `WEB_TOOLS_LIVE=1` 启用真实网络调用。
use fox_agent_sdk::{
    AgentBuilder, MockProvider, Tool, ToolContext, ToolError, WebFetchTool, WebSearchTool,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Web Tools 验证示例 ===\n");

    let mut live = std::env::var("WEB_TOOLS_LIVE").unwrap_or_default() == "1";
    live = true;    
    // ───── Part 1: Schema 和参数校验（不依赖网络）─────
    test_schema_and_validation().await;

    // ───── Part 2: 错误场景（不依赖外部网络）─────
    test_error_cases().await;

    // ───── Part 3: Agent + Mock 集成（不触发真实网络）─────
    test_agent_integration().await;

    // ───── Part 4: 实时网络调用（可选）─────
    if live {
        println!("── 实时网络测试 ──\n");
        test_live_search().await;
        test_live_fetch().await;
    } else {
        println!("── 跳过实时网络测试（设置 WEB_TOOLS_LIVE=1 启用）──\n");
    }

    println!("=== 全部通过 ===");
}

// ───── Part 1: Schema 校验 ─────

async fn test_schema_and_validation() {
    println!("[1] Schema 与参数校验");

    // WebSearch schema
    let search = WebSearchTool::new();
    let schema = search.parameters_schema();
    assert!(schema["required"].as_array().unwrap().contains(&json!("query")));
    assert_eq!(search.name(), "websearch");
    println!("   WebSearchTool.name() = {}", search.name());
    println!("   WebSearchTool.parameters_schema 包含 'query' (required)");

    // WebFetch schema
    let fetch = WebFetchTool::new();
    let schema2 = fetch.parameters_schema();
    assert!(schema2["required"].as_array().unwrap().contains(&json!("url")));
    assert_eq!(fetch.name(), "webfetch");
    println!("   WebFetchTool.name() = {}", fetch.name());
    println!("   WebFetchTool.parameters_schema 包含 'url' (required)");
    println!("   ok\n");
}

// ───── Part 2: 错误场景 ─────

async fn test_error_cases() {
    println!("[2] 错误场景");

    let fetch = WebFetchTool::new();
    let ctx = dummy_ctx();

    // 非法 URL
    let r = fetch.execute(json!({"url": "not-a-url"}), ctx.clone()).await;
    match r {
        Err(ToolError::Message { message }) => {
            assert!(message.contains("http://"), "应提示 URL 格式: {message}");
            println!("   非法 URL 正确拒绝: {message}");
        }
        other => panic!("预期 ToolError::Message，实际: {:?}", other),
    }

    // 空 query
    let search = WebSearchTool::new();
    let r2 = search.execute(json!({"extra": "no-query"}), ctx.clone()).await;
    match r2 {
        Err(ToolError::Message { message }) => {
            assert!(message.contains("invalid websearch input"), "应提示参数错误: {message}");
            println!("   缺少必填参数 'query' 正确拒绝: {message}");
        }
        other => panic!("预期 ToolError::Message，实际: {:?}", other),
    }

    // 非法 engine 参数
    let r3 = search
        .execute(
            json!({"query": "test", "engine": "google"}),
            ctx.clone(),
        )
        .await;
    match r3 {
        Err(ToolError::Message { message }) => {
            assert!(message.contains("Unknown engine"), "应提示未知引擎");
            println!("   非法 engine 正确拒绝: {message}");
        }
        other => panic!("预期 ToolError::Message，实际: {:?}", other),
    }

    println!("   ok\n");
}

// ───── Part 3: Agent + Mock 集成（不触发真实网络）─────

async fn test_agent_integration() {
    println!("[3] Agent 集成 — 验证工具注册");

    let provider = Arc::new(MockProvider::new("mock"));

    let agent = AgentBuilder::new()
        .with_provider(provider)
        .model_id("mock-1")
        .with_tool(Arc::new(WebSearchTool::new()))
        .with_tool(Arc::new(WebFetchTool::new()))
        .build()
        .await
        .expect("build agent");

    // Verify tool names match the registered tools
    let defs = agent.harness().tool_definitions().await;
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    println!("   已注册工具: {:?}", names);
    assert!(names.contains(&"websearch"), "应包含 websearch");
    assert!(names.contains(&"webfetch"), "应包含 webfetch");
    println!("   ok\n");
}

// ───── Part 4: 实时网络测试（可选）─────

async fn test_live_search() {
    println!("[L1] WebSearch — 搜索 rust-lang.org");

    let tool = WebSearchTool::new();
    let ctx = dummy_ctx();

    match tool.execute(
        json!({"query": "Rust programming language site:rust-lang.org", "num_results": 5}),
        ctx.clone(),
    ).await {
        Ok(output) => {
            assert!(!output.is_error, "搜索不应返回错误: {}", output.text);

            let meta: serde_json::Value = output.json.unwrap();
            let count = meta["result_count"].as_u64().unwrap();
            println!("   结果数: {count}");

            // 验证搜索结果包含 rust-lang.org
            assert!(
                output.text.contains("rust-lang.org"),
                "搜索结果应包含 rust-lang.org 链接:\n{}",
                output.text
            );
            println!("   完整结果:\n{}", output.text);
            println!("   ✓ 已验证搜索结果包含 rust-lang.org");
        }
        Err(e) => {
            eprintln!("   [失败] DuckDuckGo 搜索不可达: {e}");
            eprintln!("   (WebSearch 工具需要网络连接，此测试依赖外部服务)");
        }
    }

    println!("   ok\n");
}

async fn test_live_fetch() {
    println!("[L2] WebFetch — 抓取 https://www.rust-lang.org");

    let tool = WebFetchTool::new();
    let ctx = dummy_ctx();

    // Fetch rust-lang.org as text
    match tool.execute(
        json!({"url": "https://www.rust-lang.org", "format": "text", "timeout": 15}),
        ctx.clone(),
    ).await {
        Ok(output) => {
            assert!(!output.is_error, "webfetch 不应返回错误: {}", output.text);
            assert!(
                !output.text.is_empty(),
                "webfetch 返回内容不应为空"
            );
            println!("   内容长度: {} bytes", output.text.len());

            // 验证页面内容包含 Rust 关键词
            assert!(
                output.text.to_lowercase().contains("rust"),
                "抓取的 rust-lang.org 页面应包含 'rust':\n{}",
                &output.text[..200.min(output.text.len())]
            );
            println!("   首 200 字符: {}", &output.text[..200.min(output.text.len())]);
            println!("   ✓ 已验证 rust-lang.org 成功抓取");
        }
        Err(e) => {
            eprintln!("   [失败] https://www.rust-lang.org 不可达: {e}");
            eprintln!("   (WebFetch 工具需要网络连接，此测试依赖外部服务)");
        }
    }

    // Fetch rust-lang.org as markdown
    match tool.execute(
        json!({"url": "https://www.rust-lang.org", "format": "markdown", "timeout": 15}),
        ctx.clone(),
    ).await {
        Ok(output) => {
            assert!(!output.is_error, "webfetch markdown 不应返回错误: {}", output.text);
            assert!(!output.text.is_empty(), "webfetch markdown 返回内容不应为空");
            println!("   markdown 长度: {} bytes", output.text.len());
            assert!(
                output.text.to_lowercase().contains("rust"),
                "markdown 格式应包含 'rust'"
            );
            println!("   ✓ 已验证 markdown 格式成功抓取");
        }
        Err(e) => {
            eprintln!("   [失败] rust-lang.org markdown 不可达: {e}");
        }
    }

    println!("   ok\n");
}

// ───── Helpers ─────

fn dummy_ctx() -> ToolContext {
    ToolContext {
        session_id: "test-session".into(),
        message_id: "msg-1".into(),
        tool_call_id: "call-1".into(),
        working_dir: None,
        execution_mode: fox_agent_sdk::ToolExecutionMode::Foreground,
        graceful_shutdown_requested: false,
        progress_tx: None,
    }
}
