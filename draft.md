
# PeerConnect Protocol

Version 0.1.0 · Draft

A minimal, end-to-end encrypted communication protocol for two peers over a WebSocket relay.

## 1. 概述

PeerConnect 是一个轻量级的双端通信协议，允许两个独立应用通过 WebSocket Relay 建立安全、持续、双向的通信。

无论两个应用运行在同一设备、不同设备，还是不同网络中，都可以使用相同的协议。

PeerConnect 只负责建立通信通道和传递消息，不关心消息的内容与用途。

### 核心原则

1. Two Peers：每个 Room 最多绑定两个 Peer。
2. One Room：Room 是两个 Peer 的逻辑通信空间。
3. E2EE：通信内容只由两个 Peer 加密和解密。
4. Untrusted Relay：Relay 负责转发，无法读取或伪造经过认证的业务消息。
5. No Persistence：Relay 无需数据库或消息持久化。
6. Protocol Agnostic：不定义任何应用层消息协议。
7. Self-hostable：任何人都可以部署兼容的 Relay。

## 2. 架构

```
    Application A                Application B
          │                            │
          ▼                            ▼
    PeerConnect SDK              PeerConnect SDK
          │                            │
          │◄══════ E2EE ═════════════►│
          │                            │
          │ WebSocket        WebSocket │
          │                            │
          └────────── Relay ───────────┘
                         │
                    Room: abc123
                    Max Peers: 2
```

### 基本概念


| 概念    | 定义                                               |
| --------- | ---------------------------------------------------- |
| Peer    | 具有独立加密身份的通信端点                         |
| Room    | 最多容纳两个 Peer 的逻辑通信空间                   |
| Relay   | 负责连接管理、Room 路由和密文转发的 WebSocket 服务 |
| Message | 两个 Peer 之间交换的任意数据                       |

一个 Room 中，双方地位对等，都可以主动发送和接收消息。

Room 独立于 WebSocket 连接。连接断开不意味着两个 Peer 的信任关系终止。

## 3. Room 生命周期

```
CREATED
   │
   │ Second Peer joins
   ▼
ACTIVE
   │
   │ Peer disconnects
   ▼
WAITING
   │
   ├── Reconnect ──► ACTIVE
   │
   └── Expire ─────► CLOSED

ACTIVE ── Close ───► CLOSED
```


| 状态      | 含义                         |
| ----------- | ------------------------------ |
| `CREATED` | Room 已创建，等待第二个 Peer |
| `ACTIVE`  | 两个 Peer 均在线             |
| `WAITING` | 至少一方临时断线             |
| `CLOSED`  | Room 已终止                  |

Room 状态可以仅保存在 Relay 内存中。

当 Relay 重启或丢失内存状态时，Room 可以根据客户端保留的身份和配对信息重新建立，而不是依赖服务器恢复旧记录。

## 4. 建立连接

### 4.1 创建 Room

Peer A 生成安全随机的 Room ID，并连接 Relay。

```
{
  "type": "create",
  "room": "abc123"
}
```

Relay 返回：

```
{
  "type": "created",
  "room": "abc123"
}
```

Room ID 只负责定位 Room，不是访问凭证。

### 4.2 邀请 Peer

Peer A 生成邀请信息，并通过 QR Code、Deep Link、复制链接等方式传递给 Peer B。

```
{
  "protocol": "peerconnect",
  "version": "0.1",
  "relay": "wss://relay.example.com",
  "room": "abc123",
  "publicKey": "PEER_A_PUBLIC_KEY",
  "inviteToken": "SECRET"
}
```

邀请包含：

- Relay 地址
- Room ID
- Peer A 的身份公钥
- 高熵加入凭证

邀请必须通过适当的安全渠道传递，防止凭证泄漏或身份被替换。

### 4.3 加入 Room

Peer B 连接 Relay：

```
{
  "type": "join",
  "room": "abc123",
  "inviteToken": "SECRET"
}
```

Relay 验证加入权限与 Room 容量，随后通知 Peer A：

```
{
  "type": "control",
  "event": "peer_joined"
}
```

加入 Room 只代表建立了传输连接，不代表完成 Peer 身份认证。

### 4.4 建立 E2EE

双方通过 Relay 交换握手消息，完成身份验证和会话密钥协商。

推荐使用经过安全审查的密码学协议，例如 Noise Protocol Framework 中合适的握手模式。

握手必须将双方身份与当前 Room 绑定，防止中间人攻击及跨 Room 消息混淆。

只有完成端到端身份认证后，Room 才允许向应用交付业务消息。

## 5. 消息协议

PeerConnect 只定义两类消息。

### Control Message

用于 Peer 与 Relay 之间的连接和 Room 管理。

```
{
  "type": "control",
  "event": "peer_left"
}
```

基本操作：


| 操作     | 作用          |
| ---------- | --------------- |
| `create` | 创建 Room     |
| `join`   | 加入 Room     |
| `leave`  | 主动退出 Room |
| `close`  | 终止 Room     |

Relay 可以返回 `created`、`joined`、`peer_joined`、`peer_left`、`closed`、`error` 等事件。

控制消息由 Relay 产生，不作为远端 Peer 身份认证的依据。

### Data Message

用于两个 Peer 交换加密数据。

```
{
  "type": "data",
  "payload": "ENCRYPTED_DATA"
}
```

Relay 不解析 `payload`，只负责转发到 Room 中的另一个 Peer。

正式实现推荐通过 WebSocket Binary Frame 承载加密数据，减少额外编码开销。

Relay 不需要知道请求方、响应方、方法名或业务类型。

## 6. 安全模型

PeerConnect 将 Relay 视为不可信基础设施。


| 安全目标             | 实现方式                 |
| ---------------------- | -------------------------- |
| 消息保密             | 端到端加密               |
| 消息完整性           | AEAD                     |
| Peer 身份认证        | 经过认证的密钥交换       |
| 防止未授权加入       | 加入凭证及 Peer 身份绑定 |
| 防止重放攻击         | 消息序号及重放检测       |
| 前向保密             | 临时密钥协商             |
| 防止 Relay 冒充 Peer | 身份验证与握手绑定       |

客户端只接受经过身份验证、完整性检查及解密成功的 Peer 消息。

Relay 仍然能够观察连接地址、消息长度、通信时间和频率，也可能丢弃、延迟、重排消息或拒绝提供服务。

因此，PeerConnect 提供消息保密性和完整性，但不保证 Relay 的可用性。

## 7. 断线与重连

PeerConnect 区分三种行为：


| 行为         | 结果                               |
| -------------- | ------------------------------------ |
| `disconnect` | WebSocket 意外断开，保留 Peer 身份 |
| `leave`      | 主动退出 Room，关闭当前连接        |
| `close`      | 终止 Room，结束通信关系            |

当连接意外断开时：

1. 客户端自动重新连接 Relay。
2. 使用原有 Room ID 定位 Room。
3. 使用密钥证明身份及恢复权限。
4. 双方重新建立或安全恢复 E2EE 会话。
5. 恢复消息交换。

如果 Relay 已丢失全部内存状态，两个 Peer 也可以通过重新验证配对信息重建 Room。

无持久化意味着 Relay 无法永久记住身份绑定、关闭记录及已使用的邀请凭证。 因此，客户端必须保存必要的恢复信息，协议也必须定义可验证的重建授权。

第一版不保证断线期间消息的投递，不提供离线消息存储。

## 8. 无持久化 Relay

Relay 仅维护运行时状态。

```
Relay
 └── Rooms (In Memory)
      ├── Room A
      │    ├── Peer 1 → WebSocket
      │    └── Peer 2 → WebSocket
      │
      └── Room B
           ├── Peer 1 → WebSocket
           └── Peer 2 → WebSocket
```

Relay 不需要保存：

- 用户账户
- 历史消息
- 长期身份私钥
- 离线消息
- Room 的持久化记录

当两个 Peer 均断开连接后，Relay 可以释放对应的 Room 内存。

Relay 重启不会破坏双方已经建立的长期身份信任关系，但需要重新建立网络连接和加密会话。

对于已经主动关闭的 Room，双方客户端应删除相应恢复信息。若要求服务器永久阻止旧 Room ID 被再次使用，就必须额外引入持久化撤销机制；这不属于第一版的保证。

## 9. 部署架构

PeerConnect 不依赖特定服务器平台。

推荐首个实现使用 Cloudflare Workers + Durable Objects。

```
Peer A                         Peer B
   │                              │
   │ WebSocket          WebSocket │
   ▼                              ▼
┌────────────────────────────────────┐
│        Cloudflare Workers          │
│         WebSocket Routing          │
│                  │                 │
│                  ▼                 │
│           Durable Object           │
│             Room ID                │
│                                    │
│          Peer A ↔ Peer B           │
└────────────────────────────────────┘
```

一个 Room 映射到一个 Durable Object。

通过 WebSocket Hibernation API，可以在空闲时休眠 Durable Object，同时保留 WebSocket 连接。

当双方都退出后，实例可以被运行时回收。

不需要 D1、KV、Redis 或外部数据库。

需要注意：连接的 Hibernation 恢复与 Room 全部断线后的重建不是同一回事。后者必须重新进行授权验证。

同一协议也可以通过 Rust、Go、Node.js 等实现自托管 Relay。

## 10. Client SDK

SDK 对应用提供统一接口。

```
// Peer A
const room = await peerconnect.createRoom({
  relay: "wss://relay.example.com"
});

const invite = room.invite;

// Peer B
const room = await peerconnect.joinRoom(invite);

// Both peers
room.send(data);

room.onMessage((data) => {
  // Authenticated and decrypted peer message
});

room.on("peer_joined", () => {});
room.on("peer_left", () => {});
room.on("reconnected", () => {});
room.on("error", (error) => {});

await room.close();
```

SDK 负责：

- WebSocket 连接和重连
- Room 管理
- Peer 身份认证
- E2EE 密钥协商
- 消息加密和解密
- 控制消息与业务消息分离

`room.onMessage()` 只会收到经过认证并成功解密的远端 Peer 消息，不会收到 Relay 控制消息。

以上是 SDK 接口示例，而非已经定稿的线级协议。

## 11. 协议边界


| 功能            | PeerConnect |
| ----------------- | ------------- |
| 双 Peer 通信    | 支持        |
| Room 管理       | 支持        |
| WebSocket Relay | 支持        |
| E2EE            | 支持        |
| 身份验证        | 支持        |
| 断线重连        | 支持        |
| 无数据库部署    | 支持        |
| RPC             | 不定义      |
| 发布订阅        | 不定义      |
| MCP             | 不包含      |
| 文件传输        | 不定义      |
| 离线消息队列    | 不包含      |
| 多人通信        | 不支持      |

PeerConnect 仅负责两个端点之间的安全消息传输。

上层可以自由传输文本、JSON、二进制数据或自行定义的应用协议。

## 12. v1.0 互操作规范待定事项

目前 v0.1.0 已定义架构和主要行为。为了让不同开发者独立实现后能够互通，正式 v1.0 还必须明确以下内容：

- Wire Format：控制帧和数据帧的二进制编码、字段及错误码。
- Identity & Invitation：身份密钥格式、邀请凭证和验证规则。
- Cryptography：固定握手模式、加密算法、Nonce、序号和密钥更新规则。
- Recovery：双方完全离线后的身份绑定验证、同时重连冲突处理。
- Limits：最大消息大小、超时、心跳和连接限制。
- Conformance：规范测试向量及兼容性测试。

在这些细节固定之前，该草案适合指导实现，但尚不能保证第三方实现之间的互操作性。

## 13. Design Philosophy

## One Room. Two Peers. End-to-End Encryption.

PeerConnect 只解决两个独立应用之间的安全通信问题。

Relay 管理连接，Peer 掌握通信安全，Application 决定消息的含义。

PeerConnect 不需要知道双方在做什么，也不需要为任何具体业务提供特殊支持。

最终定位：一个轻量级、开放、可自部署、无持久化依赖的双端加密通信协议。
