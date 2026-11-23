import type { SchemaRepresentation } from "effect"

type R = SchemaRepresentation.Representation
type Refs = SchemaRepresentation.References

export type SlotReferences = { typed: Refs; wire: Refs }

const absent = new Set(["Undefined", "Void", "Never"])

export function decodeSlot(
  slot: "params" | "query" | "headers",
  typeName: string,
  typedRoot: R,
  wireRoot: R,
  references: SlotReferences,
): string {
  const typed = unwrap(typedRoot, references.typed)
  const wire = unwrap(wireRoot, references.wire)
  if (typed._tag !== "Objects") throw new Error(`request: ${slot} typed root is ${typed._tag}`)
  if (wire._tag !== "Objects") throw new Error(`request: ${slot} wire root is ${wire._tag}`)
  const lines = typed.propertySignatures.map((property) => {
    const key = String(property.name)
    const match = wire.propertySignatures.find((candidate) => String(candidate.name) === key)
    if (match === undefined) throw new Error(`request: ${typeName}.${key} has no wire property`)
    return field(key, property.type, match.type, references, typeName)
  })
  const mutates = typed.propertySignatures.some((property) => {
    const shape = strip(property.type, references.typed)
    return shape.core !== undefined && unwrap(shape.core, references.typed)._tag !== "Objects"
  })
  return `    let parsed = ${source(slot)};
    let ${mutates ? "mut " : ""}object = serde_json::Map::new();
    let mut path = crate::codec::Path::new();
${lines.join("\n")}
    let value = serde_json::Value::Object(object);
    <${typeName} as crate::codec::Decode>::decode(&value, &mut path)`
}

function source(slot: "params" | "query" | "headers") {
  if (slot === "query") return "crate::request::parse_query(&raw.query)"
  if (slot === "params") return "crate::request::value_pairs(&raw.path_params)"
  return "crate::request::value_pairs(&raw.headers)"
}

function field(key: string, typed: R, wire: R, references: SlotReferences, root: string): string {
  const literal = JSON.stringify(key)
  const values = `crate::request::field_values(&parsed, ${literal})`
  const shape = strip(typed, references.typed)
  if (shape.core === undefined) throw new Error(`request: ${root}.${key} is only null or absent`)
  const core = unwrap(shape.core, references.typed)
  if (core._tag === "Objects") {
    if (!shape.optional) throw new Error(`request: ${root}.${key} is a required nested object`)
    return `    if ${values}.is_some() {
        path.push_key(${literal});
        return Err(path.at("expected an object"));
    }`
  }
  if (core._tag === "Arrays") {
    const element = arrayElement(core, wire, references, root, key)
    return `    if let Some(values) = ${values} {
        path.push_key(${literal});
        let value = {
            let mut items = Vec::with_capacity(values.len());
            for (index, text) in values.iter().enumerate() {
                path.push_index(index);
                items.push(${element("text.as_str()")});
                path.pop();
            }
            serde_json::Value::Array(items)
        };
        path.pop();
        object.insert(${literal}.to_owned(), value);
    }`
  }
  const value = singleValue(core, wire, references, "text", shape.nullable, root, key)
  return `    if let Some(values) = ${values} {
        path.push_key(${literal});
        let text = crate::request::single(values, &path)?;
        let value = ${value};
        path.pop();
        object.insert(${literal}.to_owned(), value);
    }`
}

function arrayElement(
  typed: SchemaRepresentation.Arrays,
  wire: R,
  references: SlotReferences,
  root: string,
  key: string,
): (text: string) => string {
  if (typed.elements.length !== 0 || typed.rest.length !== 1) {
    throw new Error(`request: ${root}.${key} is neither a Vec nor a tuple`)
  }
  const element = typed.rest[0]!
  const wireElement = arrayWireElement(wire, references.wire)
  if (wireElement === undefined) throw new Error(`request: ${root}.${key} wire form has no array`)
  return (text) => singleValue(unwrap(element, references.typed), wireElement, references, text, false, root, `${key}[]`)
}

function arrayWireElement(wire: R, refs: Refs): R | undefined {
  const resolved = unwrap(wire, refs)
  if (resolved._tag === "Arrays") return resolved.rest[0]
  if (resolved._tag !== "Union") return undefined
  for (const member of resolved.types) {
    const found = arrayWireElement(member, refs)
    if (found !== undefined) return found
  }
  return undefined
}

function singleValue(
  typed: R,
  wire: R,
  references: SlotReferences,
  text: string,
  nullable: boolean,
  root: string,
  key: string,
): string {
  const resolved = unwrap(typed, references.typed)
  const wireResolved = unwrap(wire, references.wire)
  const nullValue = (value: string) => {
    if (!hasNullLiteral(wireResolved, references.wire)) {
      throw new Error(`request: ${root}.${key} is nullable but its wire form has no null literal`)
    }
    return nullOr(text, value)
  }
  if (resolved._tag === "Union") {
    if (!stringLike(resolved, references.typed) || !stringLike(wireResolved, references.wire)) {
      throw new Error(`request: ${root}.${key} has unsupported union ${describe(resolved)}`)
    }
    return nullable ? nullValue(stringLiteral(text)) : stringLiteral(text)
  }
  if (resolved._tag === "Number") {
    if (!numberWire(dropNull(wireResolved, references.wire), references.wire)) {
      throw new Error(`request: ${root}.${key} has number wire ${describe(wireResolved)}`)
    }
    return nullable
      ? nullValue(`crate::request::number_value(${text}, &path)?`)
      : `crate::request::number_value(${text}, &path)?`
  }
  if (resolved._tag === "Boolean") {
    if (!booleanWire(dropNull(wireResolved, references.wire), references.wire)) {
      throw new Error(`request: ${root}.${key} has boolean wire ${describe(wireResolved)}`)
    }
    return nullable
      ? nullValue(`crate::request::boolean_value(${text}, &path)?`)
      : `crate::request::boolean_value(${text}, &path)?`
  }
  if (resolved._tag === "String" || resolved._tag === "TemplateLiteral") {
    if (!stringLike(wireResolved, references.wire)) {
      throw new Error(`request: ${root}.${key} has string wire ${describe(wireResolved)}`)
    }
    return nullable ? nullValue(stringLiteral(text)) : stringLiteral(text)
  }
  if (resolved._tag === "Literal" && typeof resolved.literal === "string") {
    if (!stringLike(wireResolved, references.wire)) {
      throw new Error(`request: ${root}.${key} has literal wire ${describe(wireResolved)}`)
    }
    return nullable ? nullValue(stringLiteral(text)) : stringLiteral(text)
  }
  throw new Error(`request: ${root}.${key} has unsupported value ${describe(resolved)}`)
}

function stringLiteral(text: string) {
  return `serde_json::Value::String(${text}.to_owned())`
}

function dropNull(r: R, refs: Refs): R {
  const resolved = unwrap(r, refs)
  if (resolved._tag !== "Union") return resolved
  const types = resolved.types.filter((member) => {
    const current = unwrap(member, refs)
    return current._tag !== "Null" && !(current._tag === "Literal" && current.literal === "null")
  })
  return { ...resolved, types }
}

function hasNullLiteral(r: R, refs: Refs): boolean {
  const resolved = unwrap(r, refs)
  if (resolved._tag === "Literal") return resolved.literal === "null"
  if (resolved._tag !== "Union") return false
  return resolved.types.some((member) => hasNullLiteral(member, refs))
}

function booleanWire(r: R, refs: Refs): boolean {
  const values = literals(collapse(r, refs), refs)
  if (values === undefined) return false
  const set = new Set(values)
  return set.size === 2 && set.has("true") && set.has("false")
}

const numberAnnotations = new Set([
  "a string that will be decoded as a number",
  "a string that will be decoded as a finite number",
])

function numberWire(r: R, refs: Refs): boolean {
  const resolved = collapse(r, refs)
  if (resolved._tag === "String") return numberStringWire(resolved)
  if (resolved._tag !== "Union") return false
  const string = resolved.types.find((member) => unwrap(member, refs)._tag === "String")
  if (string === undefined) return false
  const stringResolved = unwrap(string, refs)
  if (stringResolved._tag !== "String" || !numberStringWire(stringResolved)) return false
  const values = new Set(
    resolved.types
      .filter((member) => member !== string)
      .flatMap((member) => literals(member, refs) ?? []),
  )
  return values.size === 3 && ["Infinity", "-Infinity", "NaN"].every((value) => values.has(value))
}

function numberStringWire(r: SchemaRepresentation.String): boolean {
  const expected = r.annotations?.expected
  if (typeof expected === "string" && numberAnnotations.has(expected)) return true
  return (
    r.checks.length > 0 &&
    r.checks.every((check) => check._tag === "Filter" && check.representation?.id === "effect/schema/isStringFinite")
  )
}

function nullOr(text: string, value: string) {
  return `if ${text} == "null" { serde_json::Value::Null } else { ${value} }`
}

function literals(r: R, refs: Refs): Array<string> | undefined {
  const resolved = unwrap(r, refs)
  if (resolved._tag === "Literal" && typeof resolved.literal === "string") return [resolved.literal]
  if (resolved._tag !== "Union") return undefined
  const values: Array<string> = []
  for (const member of resolved.types) {
    const nested = literals(member, refs)
    if (nested === undefined) return undefined
    values.push(...nested)
  }
  return values
}

function stringLike(r: R, refs: Refs): boolean {
  const resolved = unwrap(r, refs)
  switch (resolved._tag) {
    case "String":
    case "TemplateLiteral":
      return true
    case "Literal":
      return typeof resolved.literal === "string"
    case "Union":
      return resolved.types.every((member) => stringLike(member, refs))
    case "Null":
    case "Undefined":
    case "Never":
    case "Void":
    case "Unknown":
    case "Any":
      return true
    default:
      return false
  }
}

function strip(r: R, refs: Refs): { optional: boolean; nullable: boolean; core: R | undefined } {
  const resolved = unwrap(r, refs)
  if (resolved._tag !== "Union") return { optional: false, nullable: false, core: resolved }
  let optional = false
  let nullable = false
  const members: Array<R> = []
  const collect = (member: R) => {
    const current = unwrap(member, refs)
    if (current._tag === "Union") {
      for (const nested of current.types) collect(nested)
      return
    }
    if (absent.has(current._tag)) {
      optional = true
      return
    }
    if (current._tag === "Null") {
      nullable = true
      return
    }
    members.push(member)
  }
  for (const member of resolved.types) collect(member)
  if (members.length === 1) return { optional, nullable, core: members[0] }
  if (members.length === 0) return { optional, nullable, core: undefined }
  return { optional, nullable, core: optional || nullable ? ({ ...resolved, types: members } as R) : resolved }
}

function collapse(r: R, refs: Refs): R {
  const resolved = unwrap(r, refs)
  if (resolved._tag !== "Union") return resolved
  const members = resolved.types.filter((member) => !absent.has(unwrap(member, refs)._tag))
  if (members.length !== 1) return resolved
  return collapse(members[0]!, refs)
}

function unwrap(r: R, refs: Refs): R {
  let current = r
  for (let index = 0; index < 64; index++) {
    if (current._tag === "Reference") {
      const target = refs[current.$ref]
      if (target === undefined) throw new Error(`request: unknown reference ${current.$ref}`)
      current = target
      continue
    }
    if (current._tag === "Suspend") {
      current = current.thunk
      continue
    }
    return current
  }
  throw new Error("request: recursive reference")
}

function describe(r: R): string {
  if (r._tag === "Reference") return `reference ${r.$ref}`
  if (r._tag === "Literal") return `literal ${JSON.stringify(r.literal)}`
  if (r._tag === "Union") return `union of ${r.types.map(describe).join(", ")}`
  return r._tag
}

const missing = new Set(["Undefined", "Void", "Never", "Null"])
const transformChecks = new WeakSet<SchemaRepresentation.Filter>()

export function isTransformCheck(check: SchemaRepresentation.Filter) {
  return transformChecks.has(check)
}

export function applyStringTransforms(
  typed: R,
  wire: R,
  typedRefs: Refs,
  wireRefs: Refs,
  seen = new Map<R, Set<R>>(),
): void {
  const typedResolved = unwrap(typed, typedRefs)
  const wireResolved = unwrap(wire, wireRefs)
  const pairs = seen.get(typedResolved)
  if (pairs?.has(wireResolved)) return
  if (pairs === undefined) seen.set(typedResolved, new Set([wireResolved]))
  else pairs.add(wireResolved)

  const typedCore = coreOf(typedResolved, typedRefs)
  const wireCore = coreOf(wireResolved, wireRefs)
  if (typedCore !== typedResolved || wireCore !== wireResolved) {
    applyStringTransforms(typedCore, wireCore, typedRefs, wireRefs, seen)
    return
  }

  if (typedResolved._tag === "Objects" && wireResolved._tag === "Objects") {
    for (const property of typedResolved.propertySignatures) {
      const match = wireResolved.propertySignatures.find(
        (candidate) => String(candidate.name) === String(property.name),
      )
      if (match !== undefined) applyStringTransforms(property.type, match.type, typedRefs, wireRefs, seen)
    }
    return
  }
  if (typedResolved._tag === "Arrays" && wireResolved._tag === "Arrays") {
    const typedElement = typedResolved.rest[0]
    const wireElement = wireResolved.rest[0]
    if (typedElement !== undefined && wireElement !== undefined)
      applyStringTransforms(typedElement, wireElement, typedRefs, wireRefs, seen)
    return
  }
  if (typedResolved._tag === "Union" && wireResolved._tag === "Union") {
    const typedMembers = typedResolved.types.filter((member) => !missing.has(unwrap(member, typedRefs)._tag))
    const wireMembers = wireResolved.types.filter((member) => !missing.has(unwrap(member, wireRefs)._tag))
    for (const typedMember of typedMembers) {
      for (const wireMember of wireMembers) {
        const typedTag = unwrap(typedMember, typedRefs)._tag
        const wireTag = unwrap(wireMember, wireRefs)._tag
        if (typedTag === wireTag) applyStringTransforms(typedMember, wireMember, typedRefs, wireRefs, seen)
      }
    }
    return
  }
  if (typedResolved._tag !== "String" && typedResolved._tag !== "TemplateLiteral") return
  if (wireResolved._tag !== "String" && wireResolved._tag !== "TemplateLiteral") return
  const filters = flattenChecks(typedResolved.checks ?? []).filter(
    (filter) =>
      filter.representation?.id !== undefined &&
      !wireResolved.checks.some((check) => check.representation?.id === filter.representation?.id),
  )
  if (filters.length === 0) return
  for (const filter of filters) transformChecks.add(filter)
  const target = wireResolved as unknown as { checks: Array<SchemaRepresentation.Check> }
  target.checks = [...target.checks, ...filters]
}

function flattenChecks(checks: ReadonlyArray<SchemaRepresentation.Check>): Array<SchemaRepresentation.Filter> {
  return checks.flatMap((check) =>
    check._tag === "FilterGroup" ? flattenChecks(check.checks) : check._tag === "Filter" ? [check] : [],
  )
}

function coreOf(r: R, refs: Refs): R {
  if (r._tag !== "Union") return r
  const members = r.types.filter((member) => !missing.has(unwrap(member, refs)._tag))
  return members.length === 1 ? coreOf(unwrap(members[0]!, refs), refs) : r
}
