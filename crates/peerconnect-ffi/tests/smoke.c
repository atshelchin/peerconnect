/* Compiled by tests/c_abi.rs: the header in use from C. */
#include <stdio.h>
#include <string.h>

#include "peerconnect.h"

int main(void) {
  char *invite = pc_group_create("wss://relay.example.com");
  if (!invite || strncmp(invite, "wss://relay.example.com#", 24) != 0) return 1;

  uint8_t seed[32], id[32], again[32];
  pc_identity_generate(seed);
  pc_identity_id(seed, id);
  const char *name = "player one";
  pc_member *m = pc_member_new(invite, seed, (const uint8_t *)name, strlen(name), 0);
  pc_string_free(invite);
  if (!m) return 2;
  pc_member_id(m, again);
  if (memcmp(id, again, 32) != 0) return 3;

  /* The first thing asked: connect to the group's endpoint. */
  pc_output out;
  if (pc_member_poll(m, &out) != PC_CONNECT || strstr((const char *)out.data, "/g1/") == NULL) return 4;
  if ((size_t)strlen((const char *)out.data) != out.len) return 5;
  if (pc_member_poll(m, &out) != PC_NONE) return 6;
  if (pc_member_poll_timeout(m) < 0) return 7;
  /* Offline: commands queue for later, voice is not sent. */
  uint64_t message = 42;
  if (pc_member_send(m, 0, (const uint8_t *)"hi", 2, &message) != PC_OK || message != 0) return 8;
  if (pc_member_poll(m, &out) != PC_MESSAGE || out.len != 2 || memcmp(out.data, "hi", 2) != 0) return 9;
  if (pc_member_send_voice(m, 0, (const uint8_t *)"x", 1) != 0) return 10;
  pc_member_handle_close(m, 5, -1);
  pc_member_leave(m);
  while (pc_member_poll(m, &out) != PC_NONE) {
  }
  if (pc_member_send(m, 0, (const uint8_t *)"x", 1, NULL) != PC_CLOSED) return 11;
  pc_member_free(m);
  printf("ok\n");
  return 0;
}
