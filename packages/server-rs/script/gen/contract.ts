import { mkdir } from "node:fs/promises"
import { Schema, SchemaAST, SchemaRepresentation } from "effect"
import { pascal } from "../../../cli-rs/script/rust/types.ts"
import { contractErrors, contractRoutes, normalize, type Route } from "../contract/model"
import { StrictTypes } from "../rust/strict"
import { pin } from "../upstream"

const root = new URL("../..", import.meta.url).pathname

const commit = await pin()
const result = Bun.spawnSync(["git", "show", `${commit}:packages/protocol/openapi.json`], { cwd: root })
if (result.exitCode !== 0) {
  console.error(`contract: git show ${commit}:packages/protocol/openapi.json failed`)
  process.exit(1)
}
const document = JSON.parse(result.stdout.toString())
const methods = new Set(["get", "put", "post", "delete", "options", "head", "patch", "trace"])
const order = new Map<string, number>()
for (const item of Object.values(document.paths) as Array<Record<string, { readonly operationId: string }>>) {
  for (const [method, operation] of Object.entries(item)) {
    if (!methods.has(method)) continue
    if (order.has(operation.operationId))
      throw new Error(`contract: ${operation.operationId} appears twice in openapi.json`)
    order.set(operation.operationId, order.size)
  }
}

const routes = (await contractRoutes()).toSorted((a, b) => orderOf(a) - orderOf(b))
if (routes.length !== order.size) {
  throw new Error(`contract: openapi.json has ${order.size} operations, the contract model has ${routes.length}`)
}

function orderOf(route: Route) {
  const index = order.get(route.operationId)
  if (index === undefined) throw new Error(`contract: ${route.operationId} is missing from openapi.json`)
  return index
}

const lines = routes.map(
  (route) => `    Route {
        operation_id: ${literal(route.operationId)},
        identifier: ${literal(route.identifier)},
        group: ${literal(route.group)},
        tag: ${literal(route.tag)},
        method: Method::${methodVariant(route.method)},
        path: ${literal(route.path)},
        scope: Scope::${route.scope},
        stability: Stability::${route.stability},
        body: BodyKind::${route.body},
        success: SuccessKind::${route.success},
        success_status: ${route.successStatus},
        wildcard: ${route.wildcard},
        errors: &[${route.errors.map((error) => `(${literal(error.tag)}, ${error.status})`).join(", ")}],
    },`,
)

const errors = await contractErrors()
const asts = [...errors.map((error) => Schema.toEncoded(error.schema).ast), Schema.toEncoded(Schema.Number).ast]
const typeDocument = SchemaRepresentation.toRepresentations(normalize(asts) as [SchemaAST.AST, ...Array<SchemaAST.AST>])
const types = new StrictTypes(typeDocument.references)
const roots = errors.map((error, index) => ({
  tag: error.tag,
  status: error.status,
  rustName: types.type(typeDocument.representations[index], error.tag),
}))
types.type(typeDocument.representations[errors.length], "numbers")

const used = new Set<string>()
const variants = roots.map((root) => {
  const base = pascal(root.tag)
  let variant = base
  for (let index = 2; used.has(variant); index++) variant = `${base}${index}`
  used.add(variant)
  return { ...root, variant }
})
const corpus = [
  ...roots.map((root) => ({ name: root.tag, rustName: root.rustName })),
  { name: "numbers", rustName: "f64" },
].toSorted((a, b) => (a.name < b.name ? -1 : 1))

const typesFile = `${root}crates/contract/src/generated/types.rs`
const errorsFile = `${root}crates/contract/src/generated/errors.rs`
const routesFile = `${root}crates/contract/src/generated/routes.rs`
const modFile = `${root}crates/contract/src/generated/mod.rs`
await mkdir(`${root}crates/contract/src/generated`, { recursive: true })
await Bun.write(
  routesFile,
  `${header()}\n#![allow(clippy::all, clippy::pedantic)]\nuse crate::route::{BodyKind, Method, Route, Scope, Stability, SuccessKind};\n\npub static ROUTES: [Route; ${routes.length}] = [\n${lines.join("\n")}\n];\n`,
)
await Bun.write(
  typesFile,
  `${header()}\n#![allow(clippy::all, clippy::pedantic)]\nuse serde_json::Value;\n\n${types.items.join("\n\n")}\n`,
)
await Bun.write(
  errorsFile,
  `${header()}
#![allow(clippy::all, clippy::pedantic)]
use super::types::*;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
${variants.map((root) => `    ${root.variant}(${root.rustName}),`).join("\n")}
}

impl ApiError {
    pub fn tag(&self) -> &'static str {
        match self {
${variants.map((root) => `            Self::${root.variant}(_) => ${literal(root.tag)},`).join("\n")}
        }
    }

    pub fn status(&self) -> u16 {
        match self {
${variants.map((root) => `            Self::${root.variant}(_) => ${root.status},`).join("\n")}
        }
    }
}

impl crate::codec::Decode for ApiError {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
        let object = crate::codec::as_object(value, path)?;
        let tag: String = crate::codec::decode_field(object, "_tag", path)?;
        match tag.as_str() {
${variants.map((root) => `            ${literal(root.tag)} => <${root.rustName} as crate::codec::Decode>::decode(value, path).map(Self::${root.variant}),`).join("\n")}
            _ => Err(path.at(format!("unknown error tag {tag}"))),
        }
    }
}

impl crate::codec::Encode for ApiError {
    fn encode(&self, out: &mut String) {
        match self {
${variants.map((root) => `            Self::${root.variant}(value) => crate::codec::Encode::encode(value, out),`).join("\n")}
        }
    }
}

pub fn corpus_roots() -> &'static [(&'static str, fn(&str) -> Result<String, crate::codec::DecodeError>)] {
    &[
${corpus.map((root) => `        (${literal(root.name)}, crate::codec::round_trip::<${root.rustName}>),`).join("\n")}
    ]
}

pub static ERROR_STATUS: [(&str, u16); ${roots.length}] = [
${roots.map((root) => `    (${literal(root.tag)}, ${root.status}),`).join("\n")}
];
`,
)
await Bun.write(
  modFile,
  `${header()}

mod errors;
mod routes;
mod types;

pub use errors::{ApiError, ERROR_STATUS, corpus_roots};
pub use routes::ROUTES;
pub use types::*;
`,
)

const format = Bun.spawnSync(["rustfmt", "--edition", "2024", modFile, typesFile, errorsFile, routesFile], {
  stdout: "inherit",
  stderr: "inherit",
})
if (format.exitCode !== 0) process.exit(format.exitCode ?? 1)

const count = (predicate: (route: Route) => boolean) => routes.filter(predicate).length
const report = (values: ReadonlyArray<string>, of: (route: Route) => string) =>
  values.map((value) => `${value} ${count((route) => of(route) === value)}`).join(", ")
console.log(`contract: ${routes.length} routes`)
console.log(`contract: stability ${report(["Stable", "Experimental"], (route) => route.stability)}`)
console.log(
  `contract: success ${report(["Json", "Data", "Empty", "Binary", "Sse", "WebSocket"], (route) => route.success)}`,
)
console.log(`contract: body ${report(["None", "Json", "Binary"], (route) => route.body)}`)
console.log(`contract: scope ${report(["Global", "Location", "Session", "Form"], (route) => route.scope)}`)
console.log(`contract: ${errors.length} errors, ${types.items.length} types`)

function header() {
  return "// Generated by script/gen/contract.ts from the upstream pin. Do not edit."
}

function literal(value: string) {
  return JSON.stringify(value)
}

function methodVariant(value: string) {
  return `${value[0]?.toUpperCase()}${value.slice(1).toLowerCase()}`
}
