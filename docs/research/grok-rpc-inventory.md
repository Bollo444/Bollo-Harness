# Official Grok tool-service RPC signature inventory

SOURCE: [grok-tools.proto](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-tools-api/proto/grok-tools.proto)
at `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`. Mechanically extracted signatures from that file only.

These are **upstream protobuf service definitions**, not Bollo endpoints and not
confirmed public internet services. Transport addresses, auth, deployment and exposure
cannot be inferred from a method name. Field-level protobuf messages remain authoritative
at the linked pinned source; no guessed REST translations are supplied.

| Service | Method | Request message | Response message | Shape |
|---|---|---|---|---|
| `GrokToolsService` | `ExecuteTool` | `ExecuteToolRequest` | `ExecuteToolResponse` | unary |
| `GrokToolsService` | `ExecuteToolStream` | `ExecuteToolRequest` | `ToolStreamChunk` | server stream |
| `GrokToolsService` | `ListTools` | `ListToolsRequest` | `ListToolsResponse` | unary |
| `GrokToolsService` | `GetToolInfo` | `GetToolInfoRequest` | `ToolInfo` | unary |
| `GrokToolsService` | `FinalizeToolConfigRequest` | `FinalizeToolServerConfigRequest` | `FinalizeToolServerConfigResponse` | unary |
| `GrokToolsService` | `GetToolState` | `GetToolStateRequest` | `GetToolStateResponse` | unary |
| `GrokToolsService` | `EnableTool` | `EnableToolRequest` | `EnableToolResponse` | unary |
| `GrokToolsService` | `DisableTool` | `DisableToolRequest` | `DisableToolResponse` | unary |
| `GrokToolsService` | `SetToolOptions` | `SetToolOptionsRequest` | `SetToolOptionsResponse` | unary |
| `GrokToolsService` | `GetToolOptions` | `GetToolOptionsRequest` | `GetToolOptionsResponse` | unary |
| `GrokToolsService` | `ResetToolOptions` | `ResetToolOptionsRequest` | `ResetToolOptionsResponse` | unary |
| `GrokToolsService` | `SetToolOverride` | `SetToolOverrideRequest` | `SetToolOverrideResponse` | unary |
| `GrokToolsService` | `ClearToolOverride` | `ClearToolOverrideRequest` | `ClearToolOverrideResponse` | unary |
| `GrokToolsService` | `SetSystemReminders` | `SetSystemRemindersRequest` | `SetSystemRemindersResponse` | unary |
| `GrokToolsService` | `GetSystemReminders` | `GetSystemRemindersRequest` | `GetSystemRemindersResponse` | unary |
| `GrokToolsService` | `SetTruncationConfig` | `SetTruncationConfigRequest` | `SetTruncationConfigResponse` | unary |
| `GrokToolsService` | `GetTruncationConfig` | `GetTruncationConfigRequest` | `GetTruncationConfigResponse` | unary |
| `GrokToolsService` | `GetSystemPrompt` | `GetSystemPromptRequest` | `GetSystemPromptResponse` | unary |
| `GrokToolsService` | `GetAgentInfo` | `GetAgentInfoRequest` | `GetAgentInfoResponse` | unary |
| `GrokToolsService` | `GetCompletionState` | `GetCompletionStateRequest` | `GetCompletionStateResponse` | unary |
| `GrokToolsService` | `ResetCompletionState` | `ResetCompletionStateRequest` | `ResetCompletionStateResponse` | unary |
| `GrokToolsService` | `FinalizeAgent` | `FinalizeAgentRequest` | `FinalizeAgentResponse` | unary |
| `GrokToolsCallbackService` | `SendNotification` | `ToolNotificationMsg` | `NotificationAck` | unary |
| `GrokToolsCallbackService` | `SpawnSubagent` | `SpawnSubagentRequest` | `SubagentResultMsg` | unary |

## Reading guidance

Execution/list/info methods concern invocation and discovery; enable/disable/options/
override methods concern tool configuration; completion/finalization methods concern
agent lifecycle state; callback methods include notifications and subagent requests.
This grouping is descriptive, not a tested access-control model. Bollo's own proposed
HTTP API is separately defined in [API reference](../reference/api.md).
