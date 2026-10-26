/*
 * PeerConnect groups (GROUP.md) for C, C++, C# and anything else with a C
 * FFI. Groups travel over QUIC only (GROUP.md §3.6), in one of two ways:
 *
 * - pc_client: the library connects over QUIC by itself, on two threads of
 *   its own, and reconnects as needed. Call pc_client_poll once per frame for
 *   events, and the pc_client_send functions whenever you like. Most
 *   applications want this.
 * - pc_member: the group state machine alone, without I/O or threads, for an
 *   application that has its own QUIC stack: it opens the connection, frames
 *   messages as GROUP.md §3.6 says, feeds the member what happens and carries
 *   out what pc_member_poll asks for.
 *
 * Times are milliseconds on any monotonic clock. Member IDs are 32 bytes.
 * Use a pc_client or pc_member from one thread at a time.
 */
#ifndef PEERCONNECT_H
#define PEERCONNECT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct pc_client pc_client;
typedef struct pc_member pc_member;

/* Results of the functions that can fail. */
enum {
  PC_OK = 0,
  PC_TOO_LARGE = 1,     /* over 16,352 bytes (voice and datagrams: 16,359) */
  PC_NOT_PRESENT = 2,   /* no such member in the group right now */
  PC_BUSY = 3,          /* a state transfer is already under way, or none to answer */
  PC_CLOSED = 4,        /* the member has left */
  PC_INVALID = -1,      /* a malformed argument */
};

/* What pc_client_poll and pc_member_poll return. */
typedef enum {
  PC_NONE = 0,
  /* pc_member only. Open a QUIC connection to `data`, the group's https:// or
   * quic:// URL (NUL-terminated, `len` without the NUL), as GROUP.md §3.6 says. */
  PC_CONNECT = 1,
  /* pc_member only. Send `data` as one message on the stream of `class_`: 0
   * command, 1 voice, 2 chat, 3 bulk, 4 video (voice and video as a datagram
   * where it fits). Voice and video may be dropped when the connection is
   * backed up (video oldest first). */
  PC_SEND = 2,
  /* pc_member only. Send the empty message (ping) on the command stream. */
  PC_PING = 3,
  /* pc_member only. Close the connection (code 1000) and forget it. */
  PC_DISCONNECT = 4,
  /* Events, from both. */
  PC_STATE = 10,        /* `state`: 0 offline, 1 online, 2 closed */
  PC_JOINED = 11,       /* `member` admitted; `data` is its profile */
  PC_LEFT = 12,         /* `member` left */
  PC_MESSAGE = 13,      /* from `member` on `channel` (0 command, 1 chat), `direct` if to us alone */
  PC_VOICE = 14,        /* from `member`, `seq`, `data` */
  PC_DATAGRAM = 15,     /* from `member`, `data` */
  PC_MISSED = 16,       /* `count` messages of `member`'s `channel` were lost */
  PC_SYNC_REQUEST = 17, /* `member` asks for a snapshot (`transfer`): take it now, answer with pc_member_provide_sync */
  PC_SNAPSHOT = 18,     /* the snapshot from `member`: replace your state with `data` */
  PC_SYNC_FAILED = 19,  /* the state transfer from `member` failed */
  PC_REPLACED = 20,     /* our identity joined elsewhere and took our slot */
  PC_FULL = 21,         /* the group is full on this relay */
  PC_VIDEO = 22,        /* a whole video frame from `member`'s `track`: `keyframe`, `timestamp`, `data` */
  PC_KEYFRAME_REQUEST = 23, /* members need a keyframe of our `track`: send one soon */
} pc_kind;

typedef struct {
  int32_t kind;          /* a pc_kind */
  uint8_t member[32];
  uint8_t channel;
  uint8_t direct;
  uint8_t class_;
  uint8_t state;
  uint16_t seq;
  uint32_t transfer;
  uint64_t count;
  /* Valid until the next call on this client or member. */
  const uint8_t *data;
  size_t len;
  /* PC_VIDEO and PC_KEYFRAME_REQUEST. */
  uint8_t track;
  uint8_t keyframe;
  uint32_t timestamp;
} pc_output;

/* Creates a group on `relay` (an https:// or quic:// URL; a quic:// relay needs no domain or
 * certificate) and returns its invite, or NULL. Free it with pc_string_free. The invite is a secret. */
char *pc_group_create(const char *relay);
void pc_string_free(char *s);

/* Fills `seed` with a new identity. Persist it to be recognised again. */
void pc_identity_generate(uint8_t seed[32]);
/* The member ID of an identity. */
void pc_identity_id(const uint8_t seed[32], uint8_t id[32]);

/* Joins the group with this invite over QUIC and stays in it, reconnecting as needed;
 * NULL if the invite or profile (at most 1,024 bytes) is invalid. Where UDP does not get
 * through, it stays offline and keeps trying: there is no other transport. */
pc_client *pc_client_join(const char *invite, const uint8_t seed[32], const uint8_t *profile, size_t profile_len);
/* Leaves the group (what was sent is still delivered) and frees the client. */
void pc_client_free(pc_client *c);
void pc_client_id(const pc_client *c, uint8_t id[32]);
/* 1 while a QUIC connection to the relay is open, else 0. */
int32_t pc_client_connected(const pc_client *c);
/* The admitted members' IDs, at most `max`, as of the events polled so far; returns how many there are. */
size_t pc_client_members(const pc_client *c, uint8_t (*ids)[32], size_t max);
/* The next event into `out`; returns its kind, PC_NONE when there is none. Never blocks. */
int32_t pc_client_poll(pc_client *c, pc_output *out);
/* As the pc_member functions of the same names. Voice, datagrams and video are dropped
 * rather than delayed when the connection backs up. */
int32_t pc_client_send(pc_client *c, uint8_t channel, const uint8_t *data, size_t len, uint64_t *id);
int32_t pc_client_send_to(pc_client *c, const uint8_t member[32], uint8_t channel, const uint8_t *data, size_t len);
void pc_client_send_voice(pc_client *c, uint16_t seq, const uint8_t *data, size_t len);
void pc_client_send_datagram(pc_client *c, const uint8_t *member, const uint8_t *data, size_t len);
void pc_client_send_video(pc_client *c, uint8_t track, uint8_t keyframe, uint32_t timestamp, const uint8_t *data,
                          size_t len);
int32_t pc_client_request_sync(pc_client *c, const uint8_t from[32]);
int32_t pc_client_provide_sync(pc_client *c, const uint8_t to[32], uint32_t transfer, const uint8_t *data, size_t len);
void pc_client_decline_sync(pc_client *c, const uint8_t to[32], uint32_t transfer);

/* Starts a member of the group with this invite, for an application with its own QUIC;
 * NULL if the invite or profile (at most 1,024 bytes) is invalid. Its first output asks to connect. */
pc_member *pc_member_new(const char *invite, const uint8_t seed[32], const uint8_t *profile, size_t profile_len,
                         uint64_t now);
void pc_member_free(pc_member *m);
void pc_member_id(const pc_member *m, uint8_t id[32]);
/* The admitted members' IDs, at most `max`; returns how many there are. */
size_t pc_member_members(const pc_member *m, uint8_t (*ids)[32], size_t max);
/* The latest round trip to the relay in ms, or -1. */
int64_t pc_member_rtt(const pc_member *m);

/* What happened to the connection. `code` is the close code, or -1 for none. pc_member_handle_text
 * takes "pong" for the empty message from the relay. */
void pc_member_handle_open(pc_member *m, uint64_t now);
void pc_member_handle_binary(pc_member *m, uint64_t now, const uint8_t *data, size_t len);
void pc_member_handle_text(pc_member *m, uint64_t now, const char *text, size_t len);
void pc_member_handle_close(pc_member *m, uint64_t now, int32_t code);
void pc_member_handle_timeout(pc_member *m, uint64_t now);
/* When to call pc_member_handle_timeout next, or -1. */
int64_t pc_member_poll_timeout(const pc_member *m);
/* The next output into `out`; returns its kind, PC_NONE when there is none. */
int32_t pc_member_poll(pc_member *m, pc_output *out);

/* Broadcasts a message on `channel` (0 command, 1 chat); its id in `*id` if not NULL. */
int32_t pc_member_send(pc_member *m, uint8_t channel, const uint8_t *data, size_t len, uint64_t *id);
int32_t pc_member_send_to(pc_member *m, const uint8_t member[32], uint8_t channel, const uint8_t *data, size_t len);
/* Returns 1 if sent, 0 if not. */
int32_t pc_member_send_voice(pc_member *m, uint16_t seq, const uint8_t *data, size_t len);
/* To one member, or to everyone if `member` is NULL. Returns 1 if sent, 0 if not. */
int32_t pc_member_send_datagram(pc_member *m, const uint8_t *member, const uint8_t *data, size_t len);
/* A video frame of `track` (at most 255,000 bytes), to everyone. Returns 1 if sent, 0 if not. */
int32_t pc_member_send_video(pc_member *m, uint8_t track, uint8_t keyframe, uint32_t timestamp, const uint8_t *data,
                             size_t len);
int32_t pc_member_request_sync(pc_member *m, const uint8_t from[32]);
int32_t pc_member_provide_sync(pc_member *m, const uint8_t to[32], uint32_t transfer, const uint8_t *data, size_t len);
void pc_member_decline_sync(pc_member *m, const uint8_t to[32], uint32_t transfer);
void pc_member_leave(pc_member *m);

#ifdef __cplusplus
}
#endif

#endif
