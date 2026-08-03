# AnythingUse — 当前边界与交付状态

版本：v3.2
日期：2026-08-03
分支：`main`
核心功能基线：`4b55c19`

## 当前状态

macOS 核心功能已经完成并合入 `main`：

- 人类与 Agent 共用 `lcu`；Agent 只增加 Skill。
- 全局串行 FIFO 队列，支持暂停、恢复、取消、批准和 SQLite 恢复。
- macOS 使用严格目标窗口控制，不主动激活窗口或抢占用户输入。
- Chrome 使用真实用户 Chrome、扩展和 Native Messaging，在非激活任务标签运行。
- 用户接管同一目标时任务暂停；目标消失时任务失败并释放资源。
- Runtime 只接受模型显式 `Done`；普通动作成功或重复动作不能伪造任务完成。

固定边界：不提供 MCP、Playwright、独立自动化浏览器或公共 TCP 控制面。

## 最终关键验证

- `cargo build -p lcu-desktop --release` 通过。
- TextEdit 后台任务完成路径：`set_value -> 新观察 -> 显式 Done`。
- 目标内容写入成功；193 次前台应用采样中 TextEdit 出现 0 次。
- Chrome 后台任务标签、用户标签保持和 debugger/tab lease 释放已在最终完成语义修复前通过；该修复未修改 Chrome 控制面。

这些结果用于结束 macOS 核心功能开发，不扩展为 Top100、长稳或重复压力测试门槛。

## 后续独立工作

- macOS Developer ID 签名与安装包。
- Windows 兼容实现。

两项均不属于当前 macOS 核心功能的完成阻断。
