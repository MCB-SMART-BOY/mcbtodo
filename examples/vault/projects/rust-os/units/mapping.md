---
schema: mcbtodo.work-unit/v1
id: 30000000-0000-4000-8000-000000000002
project_id: 10000000-0000-4000-8000-000000000001
parent_task_id: 20000000-0000-4000-8000-000000000001
status: in_progress
priority: high
estimated_minutes: 120
depends_on: [30000000-0000-4000-8000-000000000001]
deadline:
  kind: date
  value: 2026-11-25
time_blocks:
  - when:
      start: "2026-11-20T09:00:00+08:00"
      end: "2026-11-20T11:00:00+08:00"
      timezone: Asia/Shanghai
---

# 实现 Sv39 页表映射

- [ ] 正确映射虚拟页
- [ ] 通过对应的单元测试
