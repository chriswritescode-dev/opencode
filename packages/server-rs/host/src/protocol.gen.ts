// Generated from packages/server-rs/crates/sidecar/src/protocol.rs by ts-rs. Do not edit.
export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;
export type LocationRef = { directory: string, workspaceID?: string, };
export type RemoteError = { _tag: string, message: string, data?: JsonValue, };
export type LogLevel = "debug" | "info" | "warn" | "error";
export type Message = { "type": "hello", version: number, maxFrame: number, window: number, heartbeatMs: number, test: boolean, } | { "type": "ready", magic: string, version: number, pid: number, } | { "type": "call", id: number, method: string, location?: LocationRef, parent?: number, params: JsonValue, stream: boolean, } | { "type": "result", id: number, value: JsonValue, } | { "type": "error", id: number, error: RemoteError, } | { "type": "cancel", id: number, } | { "type": "chunk", id: number, value: JsonValue, } | { "type": "credit", id: number, chunks: number, } | { "type": "end", id: number, } | { "type": "ping", seq: number, } | { "type": "pong", seq: number, rss: number, } | { "type": "log", level: LogLevel, message: string, };
export const VERSION = 1;
export const MAGIC = "opencode-sidecar";
