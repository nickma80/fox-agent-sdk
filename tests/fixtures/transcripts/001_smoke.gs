# 冒烟测试：验证 Goldenscript 框架集成正常。
# 此用例使用空 MockProvider（无预录脚本），预期 Agent 报错。
# 使用 ! 前缀表示期望命令失败。

!run_agent "create a file named hello.txt"
---
Error: Agent error: provider: provider error: mock provider has no more scripted responses
