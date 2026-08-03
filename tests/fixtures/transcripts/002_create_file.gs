# 002_create_file：Agent 创建文件
# MockProvider 预录了 write 工具调用，验证 run_agent 能正常驱动 Agent 并返回 token 报告。
# 录制命令：UPDATE_GOLDENFILES=1 cargo test --test golden_integration transcript::test_002_create_file

run_agent "create a file named hello.txt with content Hello World"
summary
---
tokens_in=0 tokens_out=0 tools=1
Total: 0 tokens, 1 tool calls, 0 API calls
