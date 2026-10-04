# Mermaid visual architecture

All diagrams are proposed Bollo behavior. Upstream conceptual diagrams live in research.
Render with Mermaid-compatible Markdown; code blocks remain readable without rendering.

## 1. System context

```mermaid
flowchart LR
    USER[Developer] --> CLI[Terminal or headless client]
    CLI --> CORE[Bollo core]
    CORE --> PROV[Provider adapter]
    PROV --> API[Anthropic or xAI HTTPS API]
    CORE --> GATE[Policy and approval gate]
    GATE --> BROKER[Execution broker]
    BROKER --> REPO[Workspace files and git]
    BROKER --> CHILD[Isolated shell / hooks / MCP]
    CORE --> STORE[Local SQLite and artifacts]
    FUT[Future editor or HTTP client] -.-> CORE
```

## 2. Agent loop and trust boundaries

```mermaid
flowchart TD
    REQ[User request] --> CTX[Build bounded context]
    CTX --> LLM[Provider inference]
    LLM --> INTENT[Complete tool proposal]
    INTENT --> VAL[Schema and capability validation]
    VAL --> POL[Normalize and evaluate policy]
    POL -->|deny| DENIED[Return denied result]
    POL -->|ask| ASK[Exact scoped approval]
    ASK -->|approved and still valid| LOG[Journal operation intent]
    POL -->|allow| LOG
    LOG --> EXEC[Enforced execution boundary]
    EXEC --> RESULT[Record result and verification]
    RESULT --> CTX
    DENIED --> CTX
    LLM -->|done| FINAL[Final summary with evidence]
```

## 3. Approval sequence

```mermaid
sequenceDiagram
    participant M as Model
    participant R as Runtime
    participant P as Policy
    participant S as Store
    participant U as User
    participant E as Executor
    M->>R: complete tool arguments
    R->>P: normalized intent + policy revision
    P-->>R: ask + matched rule
    R->>S: pending approval with intent hash + expiry
    R->>U: command/diff, scope, isolation, reason
    U->>R: approve once
    R->>P: recheck current revision and target handles
    R->>S: consume approval and journal started
    R->>E: execute with host-issued capability
    E-->>R: result / unknown effect
    R->>S: durable outcome
    R-->>M: typed tool result
```

## 4. Run state machine

```mermaid
stateDiagram-v2
    [*] --> queued
    queued --> running
    running --> waiting_approval
    waiting_approval --> running: decision
    running --> completed
    running --> blocked: ask without UI
    queued --> cancelled
    running --> cancelled
    waiting_approval --> cancelled
    queued --> failed
    running --> failed
    waiting_approval --> failed
    queued --> interrupted: restart recovery
    running --> interrupted: restart recovery
    waiting_approval --> interrupted: restart recovery
    completed --> [*]
    blocked --> [*]
    cancelled --> [*]
    failed --> [*]
    interrupted --> [*]
```

## 5. Policy resolution

```mermaid
flowchart TD
    A[Tool intent] --> V[Valid schema, caller and executable capability?]
    V -->|no| D[Deny]
    V -->|yes| C[Within host or managed ceiling?]
    C -->|no| D
    C -->|yes| H[Trusted pre-hook veto?]
    H -->|yes| D
    H -->|no| R[Matching explicit deny?]
    R -->|yes| D
    R -->|no| Q[Matching explicit ask?]
    Q -->|yes| ASK[Ask or consume exact valid approval]
    Q -->|no| L[Matching explicit allow?]
    L -->|yes| AL[Allow subject to sandbox]
    L -->|no| PRE[Preset fallback]
    PRE --> D
    PRE --> ASK
    PRE --> AL
```

## 6. Store entities

```mermaid
erDiagram
    WORKSPACE ||--o{ SESSION : contains
    SESSION ||--o{ RUN : contains
    SESSION ||--o{ EVENT : journals
    RUN ||--o{ OPERATION : schedules
    OPERATION ||--o{ APPROVAL : requests
    OPERATION ||--o{ CHECKPOINT : captures
    RUN ||--o{ USAGE : accrues
    CHECKPOINT }o--|| ARTIFACT : references
```

## 7. MCP lifecycle

```mermaid
sequenceDiagram
    participant U as User
    participant B as Bollo client
    participant S as MCP server
    U->>B: trust executable and explicit capabilities
    B->>S: spawn with filtered env and isolation
    B->>S: initialize(version, capabilities)
    S-->>B: supported version and capabilities
    B->>S: notifications/initialized
    B->>S: tools/list
    S-->>B: schemas
    B->>B: validate, namespace, register
    B->>B: policy and approval for selected call
    B->>S: tools/call
    S-->>B: tool result (untrusted content)
    B->>S: close stdin / bounded shutdown
```

## 8. Release dependencies

```mermaid
flowchart LR
    P0[Evidence and architecture spikes] --> P1A[Policy + store + tools]
    P1A --> P1B[Provider loop + CLI]
    P1B --> P1C[MCP + hooks + TUI]
    P1C --> G[Adversarial tests and release gate]
    G --> MVP[MVP release]
    MVP --> P2[API + remote MCP + ACP]
    P2 --> P3[Bounded subagents]
```
