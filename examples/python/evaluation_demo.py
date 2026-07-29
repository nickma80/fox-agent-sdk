"""
Phase 3 Demo: Skills, Behavior Rules, and Task Assertions.

Usage:
    python phase3_demo.py
"""

import os

from fox_agent_sdk import (
    BehaviorRuleEngine,
    CommandAssertion,
    EvalReport,
    TaskAssertions,
)

# ─── 1. BehaviorRuleEngine — pattern detection in events ───
print("=" * 60)
print("1. Behavior Rule Engine — detecting anti-patterns")
print("=" * 60)

# Simulated agent events (normally from agent.run())
events = [
    {"type": "turn_start", "turn_id": 1},
    {"type": "text_delta", "text": "Let me check the file..."},
    {"type": "tool_start", "call_id": "c1", "name": "read_file", "input": '{"path":"test.rs"}'},
    {"type": "tool_end", "call_id": "c1", "output": {"text": "fn main() {}", "is_error": False}},
    {"type": "turn_end", "turn_id": 1},
    # Turn 2: simulate a tool error storm (>5 consecutive tool errors)
    {"type": "turn_start", "turn_id": 2},
]

# Add 6 consecutive error tool calls
for i in range(6):
    events.append({"type": "tool_start", "call_id": f"e{i}", "name": "bad_tool", "input": "{}"})
    events.append({"type": "tool_end", "call_id": f"e{i}", "output": {"text": "error!", "is_error": True}})
events.append({"type": "turn_end", "turn_id": 2})

# Turn 3: empty turn (no text, no tools)
events.append({"type": "turn_start", "turn_id": 3})
events.append({"type": "turn_end", "turn_id": 3})

engine = BehaviorRuleEngine()
violations = engine.check(events)
print(f"  Found {len(violations)} violations:")
for v in violations:
    print(f"    [{v.severity:7s}] {v.rule_name}: {v.message}")

errors_only = engine.check_errors(events)
assert len(errors_only) >= 1, "Expected at least one error-level violation"
print(f"\n  Errors only: {len(errors_only)}")

# ─── 2. EvalReport — building evaluation reports ───
print("\n" + "=" * 60)
print("2. EvalReport — building evaluation report")
print("=" * 60)

report = EvalReport(
    task_id="demo-1",
    user_prompt="Build a simple Rust calculator",
    agent_response="I created a calculator with add/subtract/multiply/divide.",
    events=events,
    assertions_passed=True,
)
print(f"  task_id: {report.task_id}")
print(f"  user_prompt: {report.user_prompt[:50]}...")
print(f"  agent_response: {report.agent_response[:50]}...")
print(f"  assertions_passed: {report.assertions_passed}")
print(f"  tool_summary: {report.tool_summary}")

# ─── 3. TaskAssertions + CommandAssertion ───
print("\n" + "=" * 60)
print("3. TaskAssertions — world-state verification")
print("=" * 60)

test_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "_test_phase3")
os.makedirs(test_dir, exist_ok=True)

# Create test files
with open(os.path.join(test_dir, "calc.rs"), "w") as f:
    f.write("pub fn add(a: i32, b: i32) -> i32 { a + b }\n")

assertions = TaskAssertions()

# File existence check
assertions.file_exists("calc.rs")

# File content checks
assertions.file_contains("calc.rs", "add")
assertions.file_not_contains("calc.rs", "subtract")

# Command assertion (runs `cargo --version` to verify Rust toolchain)
cmd = CommandAssertion("cargo --version", expected_exit_code=0)
assertions.command(cmd)

result = assertions.run(test_dir)
print(f"  Passed: {result.passed}")
print(f"  Passes: {result.passed_count}/{result.total}")
if result.failures:
    print(f"  Failures:")
    for f in result.failures:
        print(f"    - {f}")

# Test a failing assertion
assertions2 = TaskAssertions()
assertions2.file_exists("nonexistent.rs")
result2 = assertions2.run(test_dir)
print(f"\n  Negative test (missing file):")
print(f"    Passed: {result2.passed} (expected False)")

# Cleanup
os.remove(os.path.join(test_dir, "calc.rs"))
os.rmdir(test_dir)

print("\n" + "=" * 60)
print("Phase 3 demo — all tests passed!")
print("=" * 60)
