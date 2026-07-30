/// general_agent: demonstrates using the SDK for a generic (non-coding)
/// agent application — a customer-support bot with custom domain tools and a
/// completely custom system prompt.
///
/// Run: cargo run --example non_coding_agent
use fox_agent_sdk::{
    AgentBuilder, AgentEvent, FoxAgentSdkConfig, MockProvider, Provider, StreamEvent, Tool,
    ToolContext, ToolError, ToolOutput, TurnOutcome,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

// ── Domain Tools ──

struct OrderLookupTool;

#[async_trait::async_trait]
impl Tool for OrderLookupTool {
    fn name(&self) -> &str {
        "lookup_order"
    }

    fn description(&self) -> &str {
        "Look up an order by ID. Returns status, items, and tracking info."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "order_id": { "type": "string", "description": "The order ID to look up" }
            },
            "required": ["order_id"]
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let order_id = input["order_id"].as_str().unwrap_or("unknown");
        Ok(ToolOutput {
            text: format!(
                "Order {order_id}: Shipped on 2026-06-20, tracking #TRACK-{order_id}-01, \
                 items: Wireless Mouse x1, USB-C Hub x1. Estimated delivery: 2026-06-25."
            ),
            is_error: false,
            json: Some(json!({
                "order_id": order_id,
                "status": "shipped",
                "tracking": format!("TRACK-{order_id}-01"),
                "items": [{"name": "Wireless Mouse", "qty": 1}, {"name": "USB-C Hub", "qty": 1}],
                "eta": "2026-06-25"
            })),
        })
    }
}

struct KbSearchTool;

#[async_trait::async_trait]
impl Tool for KbSearchTool {
    fn name(&self) -> &str {
        "search_knowledge_base"
    }

    fn description(&self) -> &str {
        "Search the knowledge base for product info, policies, and FAQs."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let query = input["query"].as_str().unwrap_or_default();
        match query.to_lowercase().as_str() {
            q if q.contains("return") || q.contains("refund") => Ok(ToolOutput {
                text: "Returns are accepted within 30 days of delivery. \
                       Go to https://example.com/returns to start a return. \
                       Refunds are processed within 5-7 business days."
                    .into(),
                is_error: false,
                json: Some(json!({"topic": "returns", "policy": "30-day"})),
            }),
            q if q.contains("shipping") => Ok(ToolOutput {
                text: "Free shipping on orders over $50. Standard: 5-7 days, Express: 2-3 days. \
                       International shipping available to 40+ countries."
                    .into(),
                is_error: false,
                json: Some(json!({"topic": "shipping"})),
            }),
            _ => Ok(ToolOutput {
                text: "I found several articles about your request. Please narrow your query."
                    .into(),
                is_error: false,
                json: Some(json!({"topic": "general", "count": 3})),
            }),
        }
    }
}

// ── Custom System Prompt ──

const CUSTOMER_SUPPORT_PROMPT: &str = r#"You are a professional customer support agent for Acme Corp,
an e-commerce company selling electronics and accessories.

## Your Role
- Greet customers warmly and address them by name when known.
- Answer questions about orders, shipping, returns, and product details.
- Be empathetic when customers are frustrated. Apologize sincerely for issues.
- Never share personal or payment information.

## Tools Available
- lookup_order: use this when a customer asks about their order
- search_knowledge_base: use this for policies, FAQs, and product information
- If the question doesn't match any tool, answer from general knowledge.

## Tone
- Friendly, concise, and professional.
- Use plain language — no technical jargon.

## Escalation
- If the customer asks for a manager, apologize and note that it will be escalated.
- If you cannot resolve the issue after two attempts, offer escalation.
"#;

// ── Main ──

#[tokio::main]
async fn main() {
    println!("=== General Agent Demo: Customer Support Bot ===\n");

    // ── Build agent with custom system prompt ──
    //
    // This is the key demo point: AgentBuilder::with_system_prompt()
    // replaces the compiled-in coding-oriented template with our
    // customer-support persona.

    let provider = Arc::new(MockProvider::new("mock"));

    // Script the model's behavior for a deterministic demo.
    // Turn 1: agent calls lookup_order tool, then responds.
    provider.push_script(vec![
        StreamEvent::ToolUse {
            id: "call1".into(),
            name: "lookup_order".into(),
            input: json!({"order_id": "ORD-98765"}),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);
    provider.push_script(vec![
        StreamEvent::TextDelta {
            text: "Your order ORD-98765 (Wireless Mouse + USB-C Hub) was shipped on \
                   June 20 and should arrive by June 25. Is there anything else I can help with?"
                .into(),
        },
        StreamEvent::MessageStop { stop_reason: None },
    ]);

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg = FoxAgentSdkConfig::load_from_file(project_root.join("agent.toml"))
        .unwrap_or_else(|_| FoxAgentSdkConfig::default());

    let agent = AgentBuilder::new()
        .working_dir(&project_root)
        .sdk_config(cfg)
        .with_global_agents_md_path(project_root.join("AGENTS.md"))
        .with_provider(provider as Arc<dyn Provider>)
        .model_id("mock-1")
        .with_system_prompt(CUSTOMER_SUPPORT_PROMPT)
        .with_tool(Arc::new(OrderLookupTool))
        .with_tool(Arc::new(KbSearchTool))
        .build()
        .await
        .expect("build agent");

    // ── Verify custom prompt is active ──
    let (split, info) = agent
        .harness()
        .build_system_prompt_split(None, None, None)
        .await;
    assert!(
        split
            .static_part
            .contains("customer support agent for Acme Corp"),
        "custom system prompt should be in effect"
    );
    println!(
        "> Custom system prompt active ({} chars)\n",
        info.total_chars
    );

    // ── Run a turn with streaming events ──
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);

    let handle = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ModelTextDelta { text } => print!("{text}"),
                AgentEvent::ModelThinkingDelta { text } => {
                    print!("\x1b[90m{text}\x1b[0m");
                }
                AgentEvent::ToolCallStart { name, .. } => {
                    println!("\n[tool: {name}]");
                }
                AgentEvent::ModelUsage { ref usage } => {
                    eprintln!(
                        "\n[tokens: {}/{}; cache: {:?}]",
                        usage.input_tokens, usage.output_tokens, usage.cache_read_input_tokens,
                    );
                }
                _ => {}
            }
        }
    });

    println!("User: Where is my order ORD-98765?\n");

    let outcome = agent
        .run_once_streaming("Where is my order ORD-98765?", &tx)
        .await
        .expect("run_once_streaming");

    drop(tx);

    match outcome {
        TurnOutcome::Completed { ref text } => {
            println!("\n\n> Final response ({len} chars)", len = text.len());
        }
        TurnOutcome::Failed { ref error } => {
            eprintln!("\n> Error: {error}");
        }
        TurnOutcome::RequiresUserDecision { ref request } => {
            println!(
                "\n> Permission needed: {} ({}) — {}",
                request.tool_name, request.risk_level, request.policy_source,
            );
        }
        _ => {}
    }

    handle.await.ok();
    println!("\nDone.");
}
