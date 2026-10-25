# peerconnect-ffi

PeerConnect groups ([GROUP.md](../../GROUP.md)) as a C library, for game engines (Unity, Unreal, Godot through GDExtension or GDNative) and any language with a C FFI.

```sh
cargo build --release -p peerconnect-ffi
# target/release/libpeerconnect_ffi.so | .dylib | peerconnect_ffi.dll, and the static library
```

The API is [`include/peerconnect.h`](include/peerconnect.h). It is the group state machine itself, without I/O or threads: your engine owns the WebSocket, and once per frame you feed the member what happened and carry out what it asks for.

```c
pc_member *m = pc_member_new(invite, seed, (const uint8_t *)"alice", 5, now_ms());

void every_frame(void) {
  pc_member_handle_timeout(m, now_ms());       /* harmless when nothing is due */
  for (/* each message the socket received */) pc_member_handle_binary(m, now_ms(), data, len);

  pc_output out;
  while (pc_member_poll(m, &out) != PC_NONE) {
    switch (out.kind) {
      case PC_CONNECT: open_websocket((const char *)out.data); break;   /* then pc_member_handle_open */
      case PC_SEND: send_binary(out.data, out.len, out.class_); break;  /* most urgent class first */
      case PC_PING: send_text("ping"); break;
      case PC_DISCONNECT: close_websocket(); break;
      case PC_MESSAGE: on_message(out.member, out.channel, out.data, out.len); break;
      case PC_VOICE: jitter_buffer_push(out.member, out.seq, out.data, out.len); break;
      /* PC_JOINED, PC_LEFT, PC_SYNC_REQUEST, PC_SNAPSHOT, … */
    }
  }
}

pc_member_send(m, 0 /* command */, (const uint8_t *)"place 7", 7, NULL);
```

`PC_CONNECT` gives the group's URL on its relay. For a `wss://` or `ws://` relay that is the WebSocket to open; for one that also offers QUIC it starts `https://` or `quic://`: open the WebSocket it falls back to, `wss://` or `ws://` with the rest unchanged, unless your engine speaks QUIC ([GROUP.md §3.6](../../GROUP.md)).

When the socket closes, call `pc_member_handle_close` with its close code (or -1). The member reconnects on its own, by asking for `PC_CONNECT` again. Pointers in a `pc_output` are valid until the next call on that member; copy what you keep.

Voice frames interoperate with the browser SDK when they use its format, `hold[1] ‖ Opus packet` ([GROUP.md §7.2](../../GROUP.md)).
