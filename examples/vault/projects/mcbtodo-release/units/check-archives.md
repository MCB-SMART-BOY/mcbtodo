---
schema: mcbtodo.work-unit/v1
id: d229d4ae-8b92-4f7d-af65-6925641236a5
project_id: 7738f8d8-bc82-4ffc-be28-5bf3e9bf3da8
parent_task_id: f31fc5bc-0b06-47c3-a858-5afbb5797619
status: todo
depends_on: [bb50ab51-4a8b-4545-8550-a1884a7347a9]
---

# 检查 crate 归档

- [ ] 运行 `cargo package --workspace --exclude mcbtodo-xtask --locked --no-verify` 生成归档。
- [ ] 运行 `cargo xtask verify-crate-packages` 检查许可证和归档内容。
