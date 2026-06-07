## API surface guidance

Vertex is a library that gets embedded into a host program. It exposes three API surfaces, in priority order:

1. **FFI** for native embedding. The primary path. Hosts are Dart, Swift, Kotlin/JNI, C++, and other native runtimes (mobile apps, headless agents, the Nexum gateway). A C ABI plus generated bindings (flutter_rust_bridge or equivalent) is the expected pattern.
2. **gRPC** for desktop and server when the consumer is a separate process. Schemas live in `.proto` files and ride tonic + prost. This is the path operators use to script a running node.
3. **wasm-bindgen** for browser embedding. Covered in `docs/agents/wasm.md`. Conceptually a third FFI form, just with a different toolchain.

All three are typed. None of them use HTTP plus JSON. This is the fundamental departure from the reference implementation, which exposes an HTTP+JSON API governed by an OpenAPI spec. Vertex deliberately does not.

### Why no JSON or OpenAPI

- Memory behaviour: serde_json's parsing and allocation patterns are not friendly to long-running streaming workloads. The reference implementation's HTTP path has documented memory issues that we do not want to inherit.
- Type safety: `application/json` erases types at the wire. Recovering them in every consumer is friction we avoid by speaking protobuf and FFI struct layouts that are generated from a schema.
- One source of truth: an OpenAPI YAML, a Go handler, and a Rust client are three places to keep in sync. A `.proto` file plus tonic codegen is one.
- Versioning: protobuf's field-number rules and gRPC's reflection give us a predictable evolution story. Hand-rolled JSON does not.
- Maintenance burden: there is no `openapi/Swarm.yaml` in this repo and there should not be.

### Rules

- Do not add `serde_json`, `serde_yaml`, or any other text-format serde backend to a vertex crate. The crates that currently pull it (`vertex-observability` for the tracing-subscriber JSON formatter, plus the workspace-disabled `swap` and `chequebook`) are the only allowed exceptions, and the disabled crates are disabled precisely for this reason.
- Do not introduce `reqwest`, `axum`, `hyper`, or any HTTP client/server framework for a public API. The OTLP exporter via `opentelemetry-otlp` is the only allowed HTTP transport in the workspace, and it is internal observability, not a public surface.
- Do not write an HTTP handler that serves user-facing requests. If you find yourself wanting one, the answer is a new gRPC service or an FFI export, not a router.
- Schema sources of truth: protobuf `.proto` files under `crates/swarm/rpc/proto/` (and per-protocol under each `vertex-swarm-net-*` crate's wire `.proto`). Generated Rust types are produced by `build.rs`; do not hand-edit generated code.
- For FFI: define the public surface as a Rust trait or struct in a single crate, then generate the C ABI from that. Do not maintain a parallel hand-written C header. flutter_rust_bridge or a similar codegen tool is the working assumption.
- gRPC services live in `crates/swarm/rpc/` (and `crates/rpc/`). New services compose into the `GrpcServiceProvider` trait pattern that the builder already uses; do not register services directly with tonic from a leaf crate.

### Surface ownership

- `crates/swarm/rpc/`: tonic service definitions and the public gRPC surface that operators script against.
- `crates/rpc/{core,server}`: the protocol-agnostic gRPC plumbing.
- A future `crates/ffi/` (or similar) is the planned home for the C ABI and the codegen for Dart, Swift, Kotlin bindings. The crate does not exist yet; when adding it, follow the same builder integration pattern that gRPC uses.
- The `swarm-wasm` surface lives separately, under the wasm guidance.

### Streaming and long-lived calls

- Prefer streaming RPCs (server-stream, client-stream, bidi) for anything that could grow without bound. Avoid returning a `Vec<Chunk>` of unknown size from a unary call.
- Cancellation: tonic gives you the cancellation signal via the request's drop. Wire it through to `vertex_tasks` so cancelled RPCs do not leak background work.
- Backpressure: the gRPC layer must not buffer unbounded. If a consumer is slow, the service yields to a `Pending` poll rather than queueing.
- FFI streaming: prefer a callback or async-stream pattern in the binding's target language over returning a bulk array. flutter_rust_bridge handles this cleanly via Dart `Stream`.

### What this means for new features

When you design a new feature with a public surface:

1. Define the operation as a typed Rust function or trait method first.
2. Decide whether it should be available via FFI, gRPC, or both. If it is for operator scripting, gRPC. If it is for an embedded host, FFI. Often both.
3. Write the protobuf `.proto` (gRPC) and the FFI-exported struct or function signature.
4. Implement once in Rust, expose via both wrappers. Keep the wrappers thin.
5. Do not invent a third surface. If something does not fit gRPC or FFI, talk to the workspace before adding a transport.
