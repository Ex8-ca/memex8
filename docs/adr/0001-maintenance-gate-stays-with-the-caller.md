# The maintenance gate is held by whoever builds the response, not by the Engine

An erasure must not commit between fetching a memory and finishing the response that carries it. The gate covers response *serialization*, which happens outside the Engine (MCP publication, the REST middleware, CLI printing), so `Engine` exposes the gate (`Engine::gate()`) and callers hold its read side through response construction. `Engine::search`, `get` and `recall` do not take it themselves: a gate inside them would be released before the response is built, and would look like it protected something it does not.

## Consequences

- Every transport that returns memory content must hold the read gate. `mcp::publish` does this for tools, and `tests/erasure_publication.rs` checks that every advertised tool is classified as a gated read or a writer.
- A new read path that skips the gate compiles and works, and reopens the erasure window. The tests above are the guard, not the type system.
- Reads re-enter the gate safely (nested reads do not deadlock behind a waiting erasure), but writers that take the exclusive side must stay out of gated scopes.

## Considered options

- **Gate inside Engine read methods.** Rejected: it ends before serialization, so the guarantee would be weaker than it appears.
- **Return a guard-carrying result type from Engine reads.** Not pursued: it would force every transport to carry the guard through its serializer; revisit if a transport is found that cannot hold the gate itself.
