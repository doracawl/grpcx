//! Snapshot-style tests for the TS codegen backend
//! (`grpcx-pb::codegen_ts`).
//!
//! The tests parse a small `.proto`, run `codegen_ts`,
//! and assert the emitted source contains the expected
//! TS fragments (interfaces, enums, classes, descriptor
//! tables). Lexical-level checks only — full TypeScript
//! semantic validation (`tsc --strict`) is deferred to
//! T2.2 when the `@grpcx/core` / `@grpcx/native` packages
//! ship and a TS toolchain is wired into CI.

use grpcx_pb::{codegen_ts, import};

fn compile(proto_src: &str) -> String {
    let schema = import(proto_src).expect("proto parses");
    codegen_ts(&schema)
}

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected emitted TS to contain `{needle}`\n----\n{haystack}\n----"
    );
}

// ============================================================
// Message interfaces + descriptor tables
// ============================================================

#[test]
fn emits_message_interface_and_descriptor_for_scalar_fields() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Person {
            string name = 1;
            uint32 age = 2;
            int64 balance = 3;
            bytes avatar = 4;
            bool active = 5;
            double score = 6;
        }
    "#;
    let out = compile(src);

    assert_contains(&out, "export interface Person {");
    assert_contains(&out, "name?: string;");
    assert_contains(&out, "age?: number;");
    assert_contains(&out, "balance?: bigint;");
    assert_contains(&out, "avatar?: Uint8Array;");
    assert_contains(&out, "active?: boolean;");
    assert_contains(&out, "score?: number;");

    assert_contains(&out, "export const PersonDescriptor: FieldDescriptor[]");
    assert_contains(
        &out,
        "{ tag: 1, name: 'name', type: 'string', label: 'singular' },",
    );
    assert_contains(
        &out,
        "{ tag: 3, name: 'balance', type: 'int64', label: 'singular' },",
    );
    assert_contains(
        &out,
        "{ tag: 4, name: 'avatar', type: 'bytes', label: 'singular' },",
    );
}

#[test]
fn emits_repeated_field_as_array() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Doc {
            repeated string tags = 1;
            repeated uint64 ids = 2;
        }
    "#;
    let out = compile(src);
    assert_contains(&out, "tags?: string[];");
    assert_contains(&out, "ids?: bigint[];");
    assert_contains(
        &out,
        "{ tag: 1, name: 'tags', type: 'string', label: 'repeated' },",
    );
}

#[test]
fn emits_imports_for_grpcx_core_and_native() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Empty {}
    "#;
    let out = compile(src);
    assert_contains(
        &out,
        "import { Connection, CallContext, Server, Status, StatusCode } from '@grpcx/core';",
    );
    assert_contains(
        &out,
        "import { encodeMessage, decodeMessage, FieldDescriptor } from '@grpcx/native';",
    );
}

// ============================================================
// Enums
// ============================================================

#[test]
fn emits_enum_with_numeric_values() {
    let src = r#"
        syntax = "proto3";
        package demo;
        enum Status {
            UNKNOWN = 0;
            ACTIVE = 1;
            ARCHIVED = 2;
        }
    "#;
    let out = compile(src);
    assert_contains(&out, "export enum Status {");
    assert_contains(&out, "UNKNOWN = 0,");
    assert_contains(&out, "ACTIVE = 1,");
    assert_contains(&out, "ARCHIVED = 2,");
}

// ============================================================
// Services — all four RPC kinds
// ============================================================

#[test]
fn emits_unary_method_as_promise() {
    let src = r#"
        syntax = "proto3";
        package demo.v1;
        message Req {}
        message Resp {}
        service Greeter {
            rpc SayHello (Req) returns (Resp);
        }
    "#;
    let out = compile(src);

    assert_contains(
        &out,
        "export const GREETER_SERVICE_NAME = 'demo.v1.Greeter';",
    );
    assert_contains(&out, "export class GreeterClient {");
    assert_contains(&out, "constructor(private readonly conn: Connection) {}");
    assert_contains(
        &out,
        "async sayHello(request: Req, ctx: CallContext = new CallContext()): Promise<Resp> {",
    );
    assert_contains(&out, "encodeMessage(ReqDescriptor, request)");
    assert_contains(
        &out,
        "this.conn.callUnary(__service, __method, ctx, __body)",
    );
    assert_contains(&out, "decodeMessage(RespDescriptor, __respBody) as Resp;");

    assert_contains(&out, "export interface GreeterHandler {");
    assert_contains(
        &out,
        "sayHello(request: Req, ctx: CallContext): Promise<Resp>;",
    );
}

#[test]
fn emits_server_streaming_as_async_iterable() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Req {}
        message Resp {}
        service S {
            rpc Listen (Req) returns (stream Resp);
        }
    "#;
    let out = compile(src);
    assert_contains(
        &out,
        "async *listen(request: Req, ctx: CallContext = new CallContext()): AsyncIterable<Resp> {",
    );
    assert_contains(
        &out,
        "this.conn.callServerStreaming(__service, __method, ctx, __body)",
    );
    assert_contains(
        &out,
        "listen(request: Req, ctx: CallContext): AsyncIterable<Resp>;",
    );
}

#[test]
fn emits_client_streaming_with_iterable_input() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Req {}
        message Resp {}
        service S {
            rpc Upload (stream Req) returns (Resp);
        }
    "#;
    let out = compile(src);
    assert_contains(
        &out,
        "async upload(requests: AsyncIterable<Req>, ctx: CallContext = new CallContext()): Promise<Resp> {",
    );
    assert_contains(
        &out,
        "this.conn.callClientStreaming(__service, __method, ctx, __requestStream)",
    );
}

#[test]
fn emits_bidi_streaming() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Req {}
        message Resp {}
        service S {
            rpc Chat (stream Req) returns (stream Resp);
        }
    "#;
    let out = compile(src);
    assert_contains(
        &out,
        "async *chat(requests: AsyncIterable<Req>, ctx: CallContext = new CallContext()): AsyncIterable<Resp> {",
    );
    assert_contains(
        &out,
        "this.conn.callBidi(__service, __method, ctx, __requestStream)",
    );
}

// ============================================================
// Server-side `register*` function
// ============================================================

#[test]
fn emits_register_function_with_method_dispatch() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Req {}
        message Resp {}
        service Foo {
            rpc Bar (Req) returns (Resp);
        }
    "#;
    let out = compile(src);
    assert_contains(
        &out,
        "export function registerFoo(server: Server, impl_: FooHandler): void {",
    );
    assert_contains(
        &out,
        "server.registerService(FOO_SERVICE_NAME, async (method, ctx, requestBodies) => {",
    );
    assert_contains(&out, "case 'Bar': {");
    assert_contains(
        &out,
        "const req = decodeMessage(ReqDescriptor, requestBodies[0]);",
    );
    assert_contains(&out, "const resp = await impl_.bar(req as any, ctx);");
    assert_contains(&out, "return [encodeMessage(RespDescriptor, resp)];");
}

// ============================================================
// Cross-cutting — header and ergonomics
// ============================================================

#[test]
fn emitted_file_starts_with_generation_banner() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Empty {}
    "#;
    let out = compile(src);
    assert!(
        out.starts_with("// Generated by grpcx-pb (TypeScript backend). DO NOT EDIT."),
        "missing banner; first 200 chars: {:?}",
        &out[..out.len().min(200)]
    );
}

#[test]
fn emits_eslint_disable_pragma() {
    let src = r#"
        syntax = "proto3";
        package demo;
        message Empty {}
    "#;
    let out = compile(src);
    assert_contains(&out, "/* eslint-disable */");
}
