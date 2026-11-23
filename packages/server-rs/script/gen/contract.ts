import { mkdir } from "node:fs/promises"
import { Schema, SchemaAST, SchemaRepresentation } from "effect"
import { pascal } from "../../../cli-rs/script/rust/types.ts"
import { contractErrors, contractEvents, contractRoutes, normalize, type Route } from "../contract/model"
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
const events = await contractEvents()

const publicAsts = events.public.map((event) => Schema.toEncoded(event.schema).ast)
const durableAsts = events.durable.map((event) => Schema.toEncoded(event.schema).ast)
const idAsts = events.ids.map((id) => id.wire.ast)
const asts = [
  ...errors.map((error) => Schema.toEncoded(error.schema).ast),
  Schema.toEncoded(Schema.Number).ast,
  ...publicAsts,
  ...durableAsts,
  Schema.toEncoded(events.publicUnion).ast,
  Schema.toEncoded(events.sessionLog).ast,
  ...idAsts,
]
const typeDocument = SchemaRepresentation.toRepresentations(normalize(asts) as [SchemaAST.AST, ...Array<SchemaAST.AST>])
const types = new StrictTypes(typeDocument.references)

let cursor = 0
const representations = (count: number) => {
  const start = cursor
  cursor += count
  return typeDocument.representations.slice(start, start + count)
}
const errorRepresentations = representations(errors.length)
const numbers = representations(1)[0]!
const publicRepresentations = representations(publicAsts.length)
const durableRepresentations = representations(durableAsts.length)
const unionRepresentation = representations(1)[0]!
const sessionLogRepresentation = representations(1)[0]!
const idRepresentations = representations(idAsts.length)

types.section("ids")
for (const id of events.ids) if (id.transform !== undefined) types.transform(id.identifier, id.transform)
const ids = events.ids.map((id, index) => ({
  ...id,
  rustName: types.type(idRepresentations[index]!, id.identifier),
}))

types.section("types")
const roots = errors.map((error, index) => ({
  tag: error.tag,
  status: error.status,
  rustName: types.type(errorRepresentations[index]!, error.tag),
}))
types.type(numbers, "numbers")
const publicEvents = events.public.map((event, index) => ({
  ...event,
  rustName: types.type(publicRepresentations[index]!, event.identifier),
}))
const durableEvents = events.durable.map((event, index) => ({
  ...event,
  rustName: types.type(durableRepresentations[index]!, event.identifier),
}))

types.section("events")
types.rename("V2Event", "PublicEvent")
types.type(unionRepresentation, "PublicEvent")
const sessionLog = types.type(sessionLogRepresentation, "SessionLogItem")

const used = new Set<string>()
const variants = roots.map((root) => {
  const base = pascal(root.tag)
  let variant = base
  for (let index = 2; used.has(variant); index++) variant = `${base}${index}`
  used.add(variant)
  return { ...root, variant }
})
const durableVariants = durableEvents.map((event) => {
  const base = pascal(event.type)
  let variant = base
  for (let index = 2; used.has(variant); index++) variant = `${base}${index}`
  used.add(variant)
  return { ...event, variant }
})
const corpus = [
  ...roots.map((root) => ({ name: root.tag, rustName: root.rustName })),
  { name: "numbers", rustName: "f64" },
  ...publicEvents.map((event) => ({ name: event.type, rustName: event.rustName })),
  ...durableEvents.map((event) => ({ name: event.key, rustName: event.rustName })),
  { name: "SessionLogItem", rustName: sessionLog },
  ...ids.map((id) => ({ name: id.identifier, rustName: id.rustName })),
].toSorted((a, b) => (a.name < b.name ? -1 : 1))
const duplicate = corpus.find((root, index) => index > 0 && corpus[index - 1]?.name === root.name)
if (duplicate !== undefined) throw new Error(`contract: duplicate corpus root ${duplicate.name}`)

const typesFile = `${root}crates/contract/src/generated/types.rs`
const idsFile = `${root}crates/contract/src/generated/ids.rs`
const eventsFile = `${root}crates/contract/src/generated/events.rs`
const errorsFile = `${root}crates/contract/src/generated/errors.rs`
const routesFile = `${root}crates/contract/src/generated/routes.rs`
const modFile = `${root}crates/contract/src/generated/mod.rs`
await mkdir(`${root}crates/contract/src/generated`, { recursive: true })

const idImport = (text: string) =>
  ids.some((id) => new RegExp(`\\b${id.rustName}\\b`).test(text)) ? "use super::ids::*;\n" : ""
const typeItems = types.items().join("\n\n")
const eventItems = types.items("events").join("\n\n")
const eventBody = `${eventItems}

#[derive(Debug, Clone, PartialEq)]
pub enum DurableEvent {
${durableVariants.map((event) => `    ${event.variant}(${event.rustName}),`).join("\n")}
}

impl crate::codec::Decode for DurableEvent {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
        let object = crate::codec::as_object(value, path)?;
        let tag: String = crate::codec::decode_field(object, "type", path)?;
        let version = crate::codec::durable_version(object, path)?;
        match (tag.as_str(), version) {
${durableVariants.map((event) => `            (${literal(event.type)}, ${event.version}) => <${event.rustName} as crate::codec::Decode>::decode(value, path).map(Self::${event.variant}),`).join("\n")}
            _ => Err(path.at("expected a known durable type and version")),
        }
    }
}

impl crate::codec::Encode for DurableEvent {
    fn encode(&self, out: &mut String) {
        match self {
${durableVariants.map((event) => `            Self::${event.variant}(value) => <${event.rustName} as crate::codec::Encode>::encode(value, out),`).join("\n")}
        }
    }
}

pub static PUBLIC_EVENT_TYPES: [&str; ${publicEvents.length}] = [
${publicEvents.map((event) => `    ${literal(event.type)},`).join("\n")}
];

pub static DURABLE_EVENTS: [(&str, u32, &str); ${durableEvents.length}] = [
${durableEvents.map((event) => `    (${literal(event.type)}, ${event.version}, ${literal(event.aggregate)}),`).join("\n")}
];
`

await Bun.write(
  routesFile,
  `${header()}\n#![allow(clippy::all, clippy::pedantic)]\nuse crate::route::{BodyKind, Method, Route, Scope, Stability, SuccessKind};\n\npub static ROUTES: [Route; ${routes.length}] = [\n${lines.join("\n")}\n];\n`,
)
await Bun.write(
  typesFile,
  `${header()}\n#![allow(clippy::all, clippy::pedantic)]\n${idImport(typeItems)}use serde_json::Value;\n\n${typeItems}\n`,
)
await Bun.write(
  idsFile,
  `${header()}\n#![allow(clippy::all, clippy::pedantic)]\nuse serde_json::Value;\n\n${types.items("ids").join("\n\n")}\n`,
)
await Bun.write(
  eventsFile,
  `${header()}
#![allow(clippy::all, clippy::pedantic)]
${idImport(eventBody)}use super::types::*;
use serde_json::Value;

${eventBody}`,
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

pub static ERROR_STATUS: [(&str, u16); ${roots.length}] = [
${roots.map((root) => `    (${literal(root.tag)}, ${root.status}),`).join("\n")}
];
`,
)
await Bun.write(
  modFile,
  `${header()}

mod errors;
mod events;
mod ids;
mod routes;
mod types;

pub use errors::{ApiError, ERROR_STATUS};
pub use events::{DurableEvent, PublicEvent, SessionLogItem, DURABLE_EVENTS, PUBLIC_EVENT_TYPES};
pub use ids::*;
pub use routes::ROUTES;
pub use types::*;

pub type RoundTrip = fn(&str) -> Result<String, crate::codec::DecodeError>;

pub fn corpus_roots() -> &'static [(&'static str, RoundTrip)] {
    &[
${corpus.map((root) => `        (${literal(root.name)}, crate::codec::round_trip::<${root.rustName}>),`).join("\n")}
    ]
}
`,
)

const format = Bun.spawnSync(
  ["rustfmt", "--edition", "2024", modFile, typesFile, idsFile, eventsFile, errorsFile, routesFile],
  {
    stdout: "inherit",
    stderr: "inherit",
  },
)
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
console.log(
  `contract: ${errors.length} errors, ${publicEvents.length + durableEvents.length} events, ${ids.length} ids, ${types.items().length + types.items("ids").length + types.items("events").length} types`,
)

function header() {
  return "// Generated by script/gen/contract.ts from the upstream pin. Do not edit."
}

function literal(value: string) {
  return JSON.stringify(value)
}

function methodVariant(value: string) {
  return `${value[0]?.toUpperCase()}${value.slice(1).toLowerCase()}`
}
