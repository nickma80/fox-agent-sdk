# 003_multi_step：多步骤工作流（read → write）
# MockProvider 预录了 read + write 工具调用序列，包含缓存命中统计。
# 录制命令：UPDATE_GOLDENFILES=1 cargo test --test golden_integration transcript::test_003_multi_step

run_agent "read the config file and update the port to 8080"
summary
---
tokens_in=0 tokens_out=0 tools=2
Total: 0 tokens, 2 tool calls, 0 API calls
