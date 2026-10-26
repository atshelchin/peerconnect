# peerconnect-ffi

PeerConnect groups ([GROUP.md](../../GROUP.md)) as a C library, for game engines (Unity, Unreal, Godot through GDExtension or GDNative) and any language with a C FFI.

```sh
cargo build --release -p peerconnect-ffi
# target/release/libpeerconnect_ffi.so | .dylib | peerconnect_ffi.dll, and the static library
```

The API is [`include/peerconnect.h`](include/peerconnect.h). Groups travel over QUIC only ([GROUP.md §3.6](../../GROUP.md)), and most engines have no QUIC, so the library brings its own: `pc_client_join` connects by itself, on two threads of the library's own, and reconnects as needed. Your game loop polls for events once per frame and sends whenever it likes.

```c
pc_client *c = pc_client_join(invite, seed, (const uint8_t *)"alice", 5);   /* invite: https:// or quic:// */

void every_frame(void) {
  pc_output out;
  while (pc_client_poll(c, &out) != PC_NONE) {   /* never blocks */
    switch (out.kind) {
      case PC_STATE: show_online(out.state == 1); break;
      case PC_JOINED: add_player(out.member, out.data, out.len); break;            /* data: their profile */
      case PC_MESSAGE: on_message(out.member, out.channel, out.data, out.len); break;
      case PC_VOICE: jitter_buffer_push(out.member, out.seq, out.data, out.len); break;
      case PC_DATAGRAM: on_position(out.member, out.data, out.len); break;
      /* PC_LEFT, PC_SYNC_REQUEST, PC_SNAPSHOT, PC_VIDEO, … */
    }
  }
}

pc_client_send(c, 0 /* command */, (const uint8_t *)"place 7", 7, NULL);
pc_client_send_datagram(c, NULL, pose, sizeof pose);   /* to everyone; dropped rather than delayed */
pc_client_free(c);                                     /* leaves */
```

Pointers in a `pc_output` are valid until the next call on that client; copy what you keep. Where UDP does not get through, the client stays offline and keeps trying (`pc_client_connected` says whether it is connected): there is no other transport.

An application with a QUIC stack of its own can drive the group state machine instead, `pc_member_*`, without the library's I/O: it opens the connection `PC_CONNECT` names, sends each `PC_SEND` on its class's stream (voice and video as datagrams where they fit), framed as GROUP.md §3.6 says, and feeds the member what arrives.

Voice frames interoperate with the browser SDK when they use its format, `hold[1] ‖ Opus packet` ([GROUP.md §7.2](../../GROUP.md)).
