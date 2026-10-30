# ACP bidirectional request-ID regression

JSON-RPC request IDs are independent in each direction. A server permission
request can use the same numeric ID as the client's pending session/prompt.
The client must deny that permission, then keep waiting for the actual prompt
response. A handled request is not a terminal prompt response.

The independent `acp_rpc_collision.rs` test reuses the transport fixture through
a new wrapper, `hermes-acp-id-collision.py`. The wrapper only replaces the fake's
string permission ID with its pending prompt ID. It does not edit the frozen
shared fixture. The test requires successful prompt completion, verifies the
actual collision, checks that permission was denied and requires settled state.

`acp-rpc-collision-red.txt` records the intended behavioral failure. The client
mistook the incoming server request for the outbound prompt's response, then
lost its completion receipt. The tests compile and use no live Hermes process.
The new test, wrapper and contract are frozen before response-matching changes.
