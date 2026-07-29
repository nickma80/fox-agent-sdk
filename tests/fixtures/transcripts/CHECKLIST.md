# Golden Transcript 用例清单

录制流程：

1. 配置真实的 LLM API key
2. 运行 `cargo run --example record_transcript -- <场景ID>`
3. 生成的 JSONL 存入 `tests/fixtures/transcripts/`

回放测试（不需要 API key）：

```
cargo test --test golden_transcripts
```

| ID | 场景 | 难度 | 状态 |
|----|------|------|------|
| 001 | 创建 Rust 项目并编译 | 简单 | 已实现 (MockProvider) |
| 002 | 多文件编辑 | 中等 | 已实现 (MockProvider) |
| 003 | 全库搜索 | 中等 | 已实现 (MockProvider) |
| 004 | Git log 分析 | 中等 | 已实现 (MockProvider) |
| 005 | 错误诊断与修复 | 困难 | 已实现 (MockProvider) |
| 006 | 权限被拒绝恢复 | 困难 | 已实现 (MockProvider) |
| 007 | 工具超时处理 | 中等 | 已实现 (MockProvider) |
| 008 | 压实后行为一致 | 困难 | 已实现 (MockProvider) |
| 009 | MCP 工具调用 | 中等 | 已实现 (MockProvider) |
| 010 | 子 Agent 委派 | 困难 | 已实现 (MockProvider) |
| 011 | 多 turn 对话 | 中等 | 已实现 (MockProvider) |
| 012 | 大文件读取 | 中等 | 已实现 (MockProvider) |
| 013 | 并发工具执行 | 中等 | 已实现 (MockProvider) |
| 014 | plan + todo 一致性 | 中等 | 已实现 (MockProvider) |
| 015 | 环境初始化 | 简单 | 已实现 (MockProvider) |
