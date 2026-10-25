# Multiplayer on PeerConnect groups

How to build a shared world, a co-op game or a collaborative app on [groups](../GROUP.md): what the protocol gives you, what it leaves to the application, and patterns that fill the gap. The last section applies them to one game, Pip's World.

## What a group gives you

- **Membership with proven identities.** Every member is an Ed25519 public key (its member ID), chosen and kept by the application. When a `joined` event names a member, its key has signed for the connection, and every frame attributed to it was encrypted with that connection's key. Profiles (a display name, an avatar) ride along, but they are only what the member claims; the ID is what it proves.
- **Two reliable streams per member, commands and chat.** Each member's messages reach every member present, including the sender itself, in the order sent, exactly once while both keep running. A member that drops out and comes back gets what it missed, as far as the sender still keeps (by default the last 8,192 messages or 8 MiB). Messages sent while offline go out on reconnection.
- **Priorities.** Commands go first everywhere: in the sender's queue, in the relay's queue for each recipient, and in the kernel's buffer, which is kept small. Voice comes next, then chat, then state transfer. A chat flood or a 50 MB snapshot does not hold up a command.
- **Real-time data.** Voice frames and datagrams are dropped rather than delayed, wherever they would wait.
- **State transfer.** A newcomer asks a member for a snapshot. That member's application provides one, and the SDK marks exactly which commands it contains. The newcomer gets the snapshot, then precisely the commands that came after it.

## What it leaves to you

**Order between members.** Alice's commands arrive in Alice's order, and Bob's in Bob's, but whether Alice's or Bob's comes first can differ from one member to the next. If two members move the same block at the same moment, two members can end up disagreeing. The relay could have imposed one order, but it is untrusted: it could also show different orders to different members. So the application decides, with one of these patterns.

**Authority.** Anyone with the invite can join and send anything. Whether a member may do something is an application rule, checked against the sender's member ID.

**Persistence.** The relay keeps nothing. The world lives in the members' applications, and someone has to save it.

## Patterns

### Owner per object

Give every object one owner (a member ID), and let only the owner change it. Every member applies a command only if it comes from the owner of what it changes. Concurrent commands then touch different objects and commute: every member ends up with the same state, whatever order the streams interleave in.

This needs no host and works with anyone offline. It fits building games where players mostly change their own things, and collaborative tools with per-section ownership. Handing an object over is itself a command from the current owner.

### Host authority

One member is the **host**. The others send it requests: `sendTo(host, "command", …)`, or broadcast commands it listens to. The host checks each request, applies it, and broadcasts the result as one of its own commands. Everyone, the host included, applies only the host's stream. It is one stream in one order, so there are no conflicts to resolve. State transfer from the host copies its state exactly.

This is what most games do with a server, with the host as the server. It is the right choice when the simulation is not deterministic (physics engines, real-time clocks), because then only one member can simulate. That member is the host, and it broadcasts the results.

Choosing the host:

- **Fixed.** The world's creator hosts; while it is offline, the world is read-only or closed.
- **Deterministic.** Among the members present that may host (the owners, say), the one with the smallest member ID. Every member computes the same answer from the `joined` and `left` events.

When the host leaves, the next one takes over from its own state: it has applied the old host's stream as far as it got. A member that got further than the new host would then hold commands the new host never applied. To avoid that, every member, when the host changes, asks the new host for a snapshot (`requestSync`). That costs one state transfer per change of host, and brings everyone exactly level with the new host.

### Lockstep

Every member applies every member's commands in an agreed order, for example by (game tick, member ID), waiting until it has everyone's commands for a tick. This only works if the simulation is deterministic down to the bit, and it stalls on the slowest member. It is rarely worth it outside fighting and RTS games. Prefer one of the above.

## Bringing a newcomer up to date

1. The newcomer joins and waits for `joined` events, then calls `requestSync(member)`: on the host, or on any member when ownership is per object.
2. The member's application gets `syncrequest`. It must take its snapshot **in that handler**, before handling the next event: the SDK has recorded, at that moment, which commands its application has applied. It may send the snapshot later, with `provideSync`; compressing it on a worker thread is fine.
3. The newcomer's SDK holds back live commands meanwhile. On `snapshot`, the application replaces its state with the snapshot. The commands that follow are exactly those the snapshot lacks, including any the provider had not yet received when it took the snapshot.

Snapshots travel in the bulk class, after everything else, and the sender paces them by the delay they add to the path: about 50 ms at most, so that voice stays smooth while a newcomer loads. On a 20 Mbit/s link, 2 MB take 2 s; on 2 Mbit/s, 10 s. On long, jittery paths they slow down further rather than delay voice; compress snapshots (a few hundred kilobytes load in seconds anywhere), and raise `Config::transfer_delay` on the providing side if loading fast matters more. If the provider leaves mid-transfer, the newcomer gets `syncfailed` and the held commands. Ask another member.

## Moving things: datagrams

Positions, poses and animation states change many times a second, and only the latest value matters. Send them as datagrams, at 10–20 per second per moving thing, each carrying the full current value and a sequence number or time. Receivers keep the newest and interpolate. A lost or dropped datagram costs nothing, because the next one replaces it.

Do not send them as commands: a reliable stream would deliver every stale position in order after a stall, and their bulk would sit in front of real commands.

## Voice and chat

Chat is the `chat` channel. Voice in browsers is `startVoice(conn)` from [`peerconnect/voice`](../js/voice.js): Opus, sent only while speaking, an adaptive jitter buffer per speaker, and push-to-talk through `setMuted`. Native clients use the same frame format, `hold[1] ‖ Opus packet`, so that they and browsers hear each other ([GROUP.md §7.2](../GROUP.md)).

## Example: Pip's World

Pip's World is a Godot game: an empty world that its player fills by talking with Pip, an AI agent with a body in the game. (Paths below are in the game's repository.) The aim: several owners, each with their own Pip, building in one world, with visitors, and voice and text between them. What the game already has:

- **The world** changes only through validated ops: `create`, `set`, `remove`, `attach`, `detach` and `copy`, applied by `World::plan` and `World::apply` in `crates/world/src/ops.rs`.
- **Saving** writes every part as a `create` op (`crates/world/src/save.rs`, format `myworld/1`), and loading replays them. A real world of 9,604 parts saves to 4.3 MB of JSON (3.4 MB of it water), about 260 KB with zstd.
- **The simulation is not deterministic.** Jolt physics, real time of day and weather, frame-rate-dependent steps and `HashMap` iteration order all differ between machines.
- **Networking** is polled from Godot's main loop (`WebSocketPeer` in `crates/game/src/bridge.rs`). There is deliberately no Tokio, so that the web export keeps working.

The mapping that follows from that:

| Game | Group |
|---|---|
| One world | One group. Its invite comes from the world's first owner and is stored with the world. |
| Owners | A list of member IDs in the world's metadata, changed only by commands from an owner. |
| Visitors | Any other member. The host ignores their build requests. They can chat, talk and walk around. |
| A player | An `Identity`, its seed kept in the game's settings. The same player is the same member in every world. |
| Applying ops | The host, by host authority, since only one member can run the physics. |
| Pip's build tool | The owner's client sends the ops as a request to the host (`sendTo(host, "command", ops)`), tagged with the Pip that made them. The host checks that the sender owns that Pip, runs `World::plan`, and broadcasts the committed ops as a host command. Every client, the host included, applies host commands only. |
| Each Pip obeys its owner | Each owner's client keeps its own ClientMCP link to its own agent, so Pip only hears its owner. Its body is a part tagged with its owner's member ID, and the host refuses ops for it from anyone else. |
| Physics, fire, water, weather | Simulated by the host only. It broadcasts the resulting changes: poses of moving parts as datagrams, about 15 per second, and changes of state (burnt, broken, filled) as commands, batched every few hundred ms. |
| Avatars | Each client sends its own player's pose as datagrams. |
| A newcomer | `requestSync(host)`. The host answers with `save::to_json`, zstd-compressed, taken in the `syncrequest` handler. |
| The host leaves | The owner with the smallest member ID present takes over, and everyone resyncs from it. While no owner is present, the world is frozen for visitors. |
| Saving | Every owner saves its copy as today; the host's copy is the one that counts. |
| Voice | The web export uses `startVoice`. Native builds capture with `AudioEffectCapture` (already in `mic.rs`), encode with Opus (libopus through a Rust binding), and use the same frame format. |

In the game's Rust code, the native client is `peerconnect::group::Member`, polled from `Game::process` beside the existing `Bridge`. Its `Output::Connect` opens a Godot `WebSocketPeer`; each frame, the game feeds the peer's packets to `handle_binary`, sends what `poll_output` returns (commands first), and calls `handle_timeout`. No Tokio is needed, and the same machine runs in the web export through the JavaScript SDK. Two things change in the world model: ops carry who asked for them (a member ID next to `Actor`), and parts can carry an owner.
