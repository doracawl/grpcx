# grpcx-pb-derive

`proto!` — compile `.proto` (editions 2023 or proto3) into
typed Rust at compile time, entirely in-process via
`grpcx-pb`. The repair for tonic-build's most-reported
pain (docs/dx-scope.md § P1):

- no system `protoc`, no `PROTOC` env, no version drift
- no `build.rs`, no `OUT_DIR` indirection
- errors point at the `.proto` file with line/col
- generated items expand exactly where you invoke the
  macro — you control the module

```rust
grpcx_pb_derive::proto!("proto/greeter.proto"); // CARGO_MANIFEST_DIR-relative

struct MyGreeter;
impl GreeterHandler for MyGreeter {
    fn say_hello(&self, req: HelloReq, ctx: &mut grpcx::CallContext)
        -> Result<HelloResp, grpcx::Status> { … }
}
```

`proto_inline!` takes the source text directly (tests,
generated pipelines).
