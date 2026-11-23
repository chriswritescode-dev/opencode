import type { SchemaRepresentation } from "effect"
import { pascal, snake } from "../../../cli-rs/script/rust/types.ts"

type R = SchemaRepresentation.Representation
type Objects = SchemaRepresentation.Objects
type Check = SchemaRepresentation.Check
type Filter = SchemaRepresentation.Filter
type LiteralValue = SchemaRepresentation.Literal["literal"]

type Field = { type: string; decode: string; encode: string }
type Condition = { condition: string; reason: string }

export class StrictTypes {
  readonly items: Array<string> = []
  private readonly names = new Set<string>()
  private readonly referenced = new Map<string, string>()
  private readonly visiting = new Set<string>()
  private readonly suspending = new Set<SchemaRepresentation.Suspend>()
  private root = ""

  constructor(private readonly references: SchemaRepresentation.References) {}

  type(r: R, hint: string): string {
    this.root = hint
    return this.render(r, hint)
  }

  name(seed: string) {
    const base = pascal(seed)
    let name = base
    for (let index = 2; this.names.has(name); index++) name = `${base}${index}`
    this.names.add(name)
    return name
  }

  private render(r: R, hint: string): string {
    this.unchecked(r)
    switch (r._tag) {
      case "Reference":
        return this.reference(r.$ref)
      case "Suspend":
        return this.suspend(r, hint)
      case "String":
      case "TemplateLiteral":
        return "String"
      case "Number":
        return "f64"
      case "Boolean":
        return "bool"
      case "Literal":
        return this.literalEnum([r.literal], hint)
      case "Union":
        return this.union(r, hint)
      case "Arrays":
        return this.arrays(r, hint)
      case "Objects":
        return this.objectsType(r, hint)
      case "Unknown":
      case "Any":
        return "serde_json::Value"
      case "Declaration":
        if (r.representation?.id === "effect/schema/Json") return "serde_json::Value"
        return this.fail(r, `unsupported declaration ${r.representation?.id ?? r._tag}`)
      case "Null":
      case "Undefined":
      case "Void":
      case "Never":
        return this.fail(r, "only valid inside a union or an optional field")
      default:
        return this.fail(r, `unsupported representation ${(r as { readonly _tag: string })._tag}`)
    }
  }

  private fail(r: R, detail: string): never {
    throw new Error(`strict: ${this.root} ${r._tag}: ${detail}`)
  }

  private unchecked(r: R) {
    const checks = this.filters(checksOf(r))
    if (checks.length === 0) return
    this.fail(r, `unsupported check ${checks[0]?.representation?.id ?? checks[0]?._tag ?? "check"}`)
  }

  private flattenUnion(types: ReadonlyArray<R>): Array<R> {
    return types.flatMap((type) => {
      if (type._tag !== "Union") return [type]
      this.unchecked(type)
      return this.flattenUnion(type.types)
    })
  }

  private deref(r: R): R {
    if (r._tag === "Reference") {
      const target = this.references[r.$ref]
      if (target === undefined) throw new Error(`strict: ${this.root} Reference: unknown reference ${r.$ref}`)
      return this.deref(target)
    }
    if (r._tag === "Suspend") return this.deref(r.thunk)
    return r
  }

  private tag(r: R) {
    return this.deref(r)._tag
  }

  private suspend(r: SchemaRepresentation.Suspend, hint: string) {
    if (this.suspending.has(r)) return this.fail(r, "recursive suspend without a named reference")
    this.suspending.add(r)
    const type = this.render(r.thunk, hint)
    this.suspending.delete(r)
    return type
  }

  private reference(ref: string): string {
    const existing = this.referenced.get(ref)
    if (existing !== undefined) return this.visiting.has(ref) ? `Box<${existing}>` : existing
    const target = this.references[ref]
    if (target === undefined) throw new Error(`strict: ${this.root} Reference: unknown reference ${ref}`)
    const name = this.name(ref)
    this.referenced.set(ref, name)
    this.visiting.add(ref)
    this.referenceBody(ref, name, target)
    this.visiting.delete(ref)
    return name
  }

  private referenceBody(ref: string, name: string, target: R) {
    const resolved = this.deref(target)
    if (resolved._tag === "Objects" && resolved.propertySignatures.length > 0) {
      this.objects(resolved, name, name)
      return
    }
    if (resolved._tag === "Union") {
      if (resolved.types.some((member) => this.tag(member) === "Null")) {
        throw new Error(`strict: ${this.root} Reference: nullable reference body ${ref}`)
      }
      const type = this.union(resolved, name, name)
      if (type !== name) this.items.push(`pub type ${name} = ${type};`)
      return
    }
    if (resolved._tag === "String" || resolved._tag === "TemplateLiteral") {
      this.newtype(name, resolved)
      return
    }
    this.items.push(`pub type ${name} = ${this.render(resolved, name)};`)
  }

  private newtype(name: string, r: SchemaRepresentation.String | SchemaRepresentation.TemplateLiteral) {
    const conditions = this.conditions(r.checks, r, "value.as_str()")
    const decode =
      conditions.length === 0
        ? `        <String as crate::codec::Decode>::decode(value, path).map(Self)`
        : `        let value = <String as crate::codec::Decode>::decode(value, path)?;
${this.guards(conditions, 8)}        Ok(Self(value))`
    this.items.push(`#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ${name}(pub String);

impl crate::codec::Decode for ${name} {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
${decode}
    }
}

impl crate::codec::Encode for ${name} {
    fn encode(&self, out: &mut String) {
        crate::codec::write_string(out, &self.0);
    }
}`)
  }

  private objectsType(r: Objects, hint: string): string {
    this.unchecked(r)
    if (r.propertySignatures.length === 0 && r.indexSignatures.length > 0) {
      if (r.indexSignatures.length > 1) return this.fail(r, "more than one index signature")
      const index = r.indexSignatures[0]!
      this.unchecked(index.parameter)
      if (!["String", "TemplateLiteral"].includes(this.tag(index.parameter)))
        return this.fail(r, "an index signature with a non-string key")
      return `crate::codec::JsonMap<${this.render(index.type, `${hint}Value`)}>`
    }
    if (r.propertySignatures.length === 0) return this.fail(r, "an object without properties")
    if (r.indexSignatures.length > 0) return this.fail(r, "mixed properties and index signatures")
    return this.objects(r, this.name(hint), hint)
  }

  private objects(r: Objects, name: string, hint: string): string {
    this.unchecked(r)
    const used = new Set<string>()
    const lines: Array<string> = []
    const decodes: Array<string> = []
    const encodes: Array<string> = []
    for (const property of r.propertySignatures) {
      const key = String(property.name)
      const ident = this.ident(key, used)
      const field = this.field(property, `${hint}${pascal(key)}`, ident)
      lines.push(`    pub ${ident}: ${field.type},`)
      decodes.push(`            ${ident}: ${field.decode},`)
      encodes.push(`        ${field.encode};`)
    }
    this.items.push(`#[derive(Debug, Clone, PartialEq)]
pub struct ${name} {
${lines.join("\n")}
}

impl crate::codec::Decode for ${name} {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
        let object = crate::codec::as_object(value, path)?;
        Ok(Self {
${decodes.join("\n")}
        })
    }
}

impl crate::codec::Encode for ${name} {
    fn encode(&self, out: &mut String) {
        out.push('{');
        let mut first = true;
${encodes.join("\n")}
        out.push('}');
    }
}`)
    return name
  }

  private ident(key: string, used: Set<string>) {
    const base = snake(key)
    let ident = base
    for (let index = 2; used.has(ident); index++) ident = `${base}_${index}`
    used.add(ident)
    return ident
  }

  private field(property: SchemaRepresentation.PropertySignature, hint: string, ident: string): Field {
    const key = JSON.stringify(String(property.name))
    if (property.type._tag === "Union") this.unchecked(property.type)
    const members = property.type._tag === "Union" ? this.flattenUnion(property.type.types) : [property.type]
    const present = members.filter((member) => !["Undefined", "Void", "Never"].includes(this.tag(member)))
    if (present.length === 0) return this.fail(property.type, "a field that is only undefined")
    const nullable = present.some((member) => this.tag(member) === "Null")
    const rest = present.filter((member) => this.tag(member) !== "Null")
    if (rest.length === 0) return this.fail(property.type, "a field that is only null")
    const single = rest.length === 1 ? rest[0]! : undefined
    const source = single ?? (property.type as R)
    const inner =
      single === undefined
        ? this.render({ ...(property.type as SchemaRepresentation.Union), types: rest }, hint)
        : this.render(withoutChecks(single), hint)
    const checkedValue = (() => {
      if (single === undefined) return "*value"
      const tag = this.tag(single)
      if (tag === "String" || tag === "TemplateLiteral") return "value.as_str()"
      if (tag === "Arrays") return "value.as_slice()"
      return "*value"
    })()
    const conditions = single === undefined ? [] : this.conditions(checksOf(single), single, checkedValue)
    const closure = conditions.length > 0 ? this.closure(conditions, inner) : ""
    const checked = closure !== ""

    if (property.isOptional && nullable) {
      return {
        type: `Option<Option<${inner}>>`,
        decode: checked
          ? `crate::codec::decode_optional_nullable_checked(object, ${key}, path, ${closure})?`
          : `crate::codec::decode_optional_nullable(object, ${key}, path)?`,
        encode: `crate::codec::write_optional_nullable(out, &mut first, ${key}, &self.${ident})`,
      }
    }
    if (property.isOptional) {
      return {
        type: `Option<${inner}>`,
        decode: checked
          ? `crate::codec::decode_optional_checked(object, ${key}, path, ${closure})?`
          : `crate::codec::decode_optional(object, ${key}, path)?`,
        encode: `crate::codec::write_optional(out, &mut first, ${key}, &self.${ident})`,
      }
    }
    if (nullable) {
      if (checked) return this.fail(source, "a required nullable checked field")
      return {
        type: `Option<${inner}>`,
        decode: `crate::codec::decode_field::<Option<${inner}>>(object, ${key}, path)?`,
        encode: `crate::codec::write_value(out, &mut first, ${key}, &self.${ident})`,
      }
    }
    return {
      type: inner,
      decode: checked
        ? `crate::codec::decode_field_checked(object, ${key}, path, ${closure})?`
        : `crate::codec::decode_field(object, ${key}, path)?`,
      encode: `crate::codec::write_value(out, &mut first, ${key}, &self.${ident})`,
    }
  }

  private arrays(r: SchemaRepresentation.Arrays, hint: string): string {
    this.unchecked(r)
    for (const element of r.elements) this.unchecked(element.type)
    if (r.rest.length === 0 && r.elements.every((element) => !element.isOptional)) {
      if (r.elements.length > 8) return this.fail(r, "a tuple with more than 8 elements")
      const types = r.elements.map((element, index) => this.render(element.type, `${hint}${index}`))
      return `(${types.join(", ")})`
    }
    if (r.rest.length === 1 && r.elements.length === 0) {
      return `Vec<${this.render(r.rest[0]!, `${hint}Item`)}>`
    }
    if (
      r.rest.length === 1 &&
      r.elements.every((element) => !element.isOptional && this.shape(element.type) === this.shape(r.rest[0]!))
    ) {
      return `Vec<${this.render(r.rest[0]!, `${hint}Item`)}>`
    }
    return this.fail(r, "an array that is neither a Vec nor a tuple")
  }

  private shape(r: R) {
    return JSON.stringify(r, (_, value: unknown) => (typeof value === "bigint" ? `${value}n` : value))
  }

  private union(r: SchemaRepresentation.Union, hint: string, name?: string): string {
    this.unchecked(r)
    const members = this.flattenUnion(r.types)
    const present = members.filter((member) => !["Undefined", "Void", "Never"].includes(this.tag(member)))
    if (present.length === 0) return this.fail(r, "no member")
    const nullable = present.some((member) => this.tag(member) === "Null")
    const rest = present.filter((member) => this.tag(member) !== "Null")
    if (rest.length === 0) return this.fail(r, "only null members")
    for (const member of rest) this.unchecked(member)
    const wrap = (type: string) => (nullable ? `Option<${type}>` : type)
    if (rest.length === 1) return wrap(this.render(rest[0]!, hint))
    const resolved = rest.map((member) => this.deref(member))
    if (resolved.every((member) => member._tag === "Literal")) {
      const values = resolved.map((member) => (member as SchemaRepresentation.Literal).literal)
      return wrap(this.literalEnum(values, hint, name))
    }
    const tagged = this.discriminator(rest)
    if (tagged !== undefined) return wrap(this.tagged(rest, tagged, hint, name))
    return wrap(this.untagged(rest, hint, name, r.options?.mode === "oneOf"))
  }

  private literalEnum(values: ReadonlyArray<LiteralValue>, hint: string, name?: string): string {
    const kind = typeof values[0]
    if (values.some((value) => typeof value !== kind)) {
      throw new Error(`strict: ${this.root} Union: mixed literal kinds`)
    }
    const enumName = name ?? this.name(hint)
    const variants = uniqueVariants(
      values.map((value) => {
        if (typeof value === "string") return pascal(value)
        if (typeof value === "boolean") return value ? "True" : "False"
        return `V${String(value).replace(/[^A-Za-z0-9]/g, "")}`
      }),
    )
    const arms = values.map((value, index) => ({ value, variant: variants[index]! }))
    if (kind === "string") {
      const expected =
        values.length === 1
          ? `expected ${JSON.stringify(values[0])}`
          : `expected one of ${values.map((value) => JSON.stringify(value)).join(", ")}`
      this.items.push(`#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ${enumName} {
${arms.map(({ variant }) => `    ${variant},`).join("\n")}
}

impl crate::codec::Decode for ${enumName} {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
        match value.as_str() {
${arms.map(({ value, variant }) => `            Some(${JSON.stringify(value)}) => Ok(Self::${variant}),`).join("\n")}
            _ => Err(path.at(${JSON.stringify(expected)})),
        }
    }
}

impl crate::codec::Encode for ${enumName} {
    fn encode(&self, out: &mut String) {
        crate::codec::write_string(out, match self { ${arms.map(({ value, variant }) => `Self::${variant} => ${JSON.stringify(value)}`).join(", ")} });
    }
}`)
      return enumName
    }
    if (kind === "boolean") {
      this.items.push(`#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ${enumName} {
${arms.map(({ variant }) => `    ${variant},`).join("\n")}
}

impl crate::codec::Decode for ${enumName} {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
        match value.as_bool() {
${arms.map(({ value, variant }) => `            Some(${value}) => Ok(Self::${variant}),`).join("\n")}
            _ => Err(path.at("expected a boolean literal")),
        }
    }
}

impl crate::codec::Encode for ${enumName} {
    fn encode(&self, out: &mut String) {
        crate::codec::Encode::encode(&match self { ${arms.map(({ value, variant }) => `Self::${variant} => ${value}`).join(", ")} }, out);
    }
}`)
      return enumName
    }
    this.items.push(`#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ${enumName} {
${arms.map(({ variant }) => `    ${variant},`).join("\n")}
}

impl crate::codec::Decode for ${enumName} {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
        match value.as_f64() {
${arms.map(({ value, variant }) => `            Some(value) if value == ${String(value)} => Ok(Self::${variant}),`).join("\n")}
            _ => Err(path.at("expected a number literal")),
        }
    }
}

impl crate::codec::Encode for ${enumName} {
    fn encode(&self, out: &mut String) {
        crate::codec::Encode::encode(&match self { ${arms.map(({ value, variant }) => `Self::${variant} => ${String(value)}f64`).join(", ")} }, out);
    }
}`)
    return enumName
  }

  private discriminator(members: ReadonlyArray<R>) {
    const shapes = members.map((member) => this.deref(member))
    if (!shapes.every((member) => member._tag === "Objects")) return undefined
    const objects = shapes as Array<Objects>
    const keys = objects[0]!.propertySignatures.map((property) => String(property.name))
    const ordered = [
      ...keys.filter((key) => key === "type" || key === "_tag"),
      ...keys.filter((key) => key !== "type" && key !== "_tag"),
    ]
    for (const key of ordered) {
      const values = objects.map((object) => {
        const property = object.propertySignatures.find((field) => String(field.name) === key)
        if (property === undefined || property.isOptional) return undefined
        const type = this.deref(property.type)
        if (type._tag !== "Literal") return undefined
        if (typeof type.literal !== "string" && typeof type.literal !== "boolean") return undefined
        return type.literal
      })
      if (values.some((value) => value === undefined)) continue
      const exact = values as Array<string | boolean>
      if (new Set(exact.map((value) => JSON.stringify(value))).size !== exact.length) continue
      if (new Set(exact.map((value) => typeof value)).size !== 1) continue
      return { key, values: exact }
    }
    for (const key of ordered) {
      const prefixes = objects.map((object) => {
        const property = object.propertySignatures.find((field) => String(field.name) === key)
        if (property === undefined || property.isOptional) return undefined
        return this.prefix(property.type)
      })
      if (prefixes.every((prefix) => prefix !== undefined)) {
        throw new Error(`strict: ${this.root} Union: prefix tag ${key} is not supported`)
      }
    }
    return undefined
  }

  private prefix(r: R) {
    const resolved = this.deref(r)
    if (resolved._tag !== "TemplateLiteral") return undefined
    const [head, tail] = resolved.parts
    if (head?._tag !== "Literal" || typeof head.literal !== "string") return undefined
    if (resolved.parts.length !== 2 || tail === undefined || this.tag(tail) !== "String") return undefined
    return head.literal
  }

  private tagged(
    members: ReadonlyArray<R>,
    tag: { key: string; values: Array<string | boolean> },
    hint: string,
    name?: string,
  ): string {
    const enumName = name ?? this.name(hint)
    const variants = uniqueVariants(tag.values.map((value) => pascal(String(value))))
    const types = members.map((member, index) => this.render(member, `${enumName}${variants[index]}`))
    const boolean = typeof tag.values[0] === "boolean"
    const read = boolean
      ? `        let tag: bool = crate::codec::decode_field(object, ${JSON.stringify(tag.key)}, path)?;`
      : `        let tag: String = crate::codec::decode_field(object, ${JSON.stringify(tag.key)}, path)?;`
    const scrutinee = boolean ? "tag" : "tag.as_str()"
    const arms = tag.values
      .map(
        (value, index) =>
          `            ${boolean ? value : JSON.stringify(value)} => <${types[index]} as crate::codec::Decode>::decode(value, path).map(Self::${variants[index]}),`,
      )
      .join("\n")
    this.items.push(`#[derive(Debug, Clone, PartialEq)]
pub enum ${enumName} {
${variants.map((variant, index) => `    ${variant}(${types[index]}),`).join("\n")}
}

impl crate::codec::Decode for ${enumName} {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
        let object = crate::codec::as_object(value, path)?;
${read}
        match ${scrutinee} {
${arms}
            _ => Err(path.at(${JSON.stringify(`expected a known ${tag.key}`)})),
        }
    }
}

impl crate::codec::Encode for ${enumName} {
    fn encode(&self, out: &mut String) {
        match self {
${variants.map((variant, index) => `            Self::${variant}(value) => <${types[index]} as crate::codec::Encode>::encode(value, out),`).join("\n")}
        }
    }
}`)
    return enumName
  }

  private untagged(members: ReadonlyArray<R>, hint: string, name: string | undefined, oneOf: boolean): string {
    const enumName = name ?? this.name(hint)
    const seen = new Map<string, string>()
    const order: Array<[string, string]> = []
    for (const member of members) {
      const resolved = this.deref(member)
      const label = this.label(member, resolved)
      const type = this.render(member, `${enumName}${label}`)
      if (seen.has(type)) continue
      let variant = label
      for (let index = 2; [...seen.values()].includes(variant); index++) variant = `${label}${index}`
      seen.set(type, variant)
      order.push([variant, type])
    }
    const attempts = order
      .map(([variant, type]) =>
        oneOf
          ? `        if let Ok(member) = <${type} as crate::codec::Decode>::decode(value, path) {
            if matched.is_some() {
                return Err(path.at("expected exactly one member to match"));
            }
            matched = Some(Self::${variant}(member));
        }`
          : `        if let Ok(member) = <${type} as crate::codec::Decode>::decode(value, path) {
            return Ok(Self::${variant}(member));
        }`,
      )
      .join("\n")
    const end = oneOf
      ? `        matched.ok_or_else(|| path.at("expected exactly one member to match"))`
      : `        Err(path.at("no member matched"))`
    const matched = oneOf ? "        let mut matched: Option<Self> = None;\n" : ""
    this.items.push(`#[derive(Debug, Clone, PartialEq)]
pub enum ${enumName} {
${order.map(([variant, type]) => `    ${variant}(${type}),`).join("\n")}
}

impl crate::codec::Decode for ${enumName} {
    fn decode(value: &Value, path: &mut crate::codec::Path) -> Result<Self, crate::codec::DecodeError> {
${matched}${attempts}
${end}
    }
}

impl crate::codec::Encode for ${enumName} {
    fn encode(&self, out: &mut String) {
        match self {
${order.map(([variant, type]) => `            Self::${variant}(value) => <${type} as crate::codec::Encode>::encode(value, out),`).join("\n")}
        }
    }
}`)
    return enumName
  }

  private label(member: R, resolved: R) {
    if (member._tag === "Reference" && !["String", "TemplateLiteral"].includes(resolved._tag)) {
      return pascal(member.$ref.split(".").at(-1) ?? member.$ref)
    }
    switch (resolved._tag) {
      case "String":
      case "TemplateLiteral":
        return "String"
      case "Literal":
        return typeof resolved.literal === "boolean"
          ? "Bool"
          : typeof resolved.literal === "number"
            ? "Number"
            : "String"
      case "Boolean":
        return "Bool"
      case "Number":
        return "Number"
      case "Arrays":
        return "Array"
      case "Objects":
        return "Object"
      default:
        return "Value"
    }
  }

  private conditions(checks: ReadonlyArray<Check>, base: R, value: string): Array<Condition> {
    const filters = this.filters(checks)
    const kind = this.tag(base)
    return filters.flatMap((filter) => {
      const id = filter.representation?.id
      const payload = filter.representation?.payload as { readonly [key: string]: unknown } | null | undefined
      const expected = filter.annotations?.expected
      const reason = typeof expected === "string" ? `expected ${expected}` : undefined
      const one = (condition: string, fallback: string) => ({ condition, reason: reason ?? fallback })
      const json = (key: string) => {
        const item = payload?.[key]
        if (typeof item !== "number") {
          throw new Error(`strict: ${this.root} ${base._tag}: ${id} needs a numeric ${key}`)
        }
        return String(item)
      }
      switch (id) {
        case "effect/schema/isInt":
          return [one(`crate::checks::is_int(${value})`, "expected an integer")]
        case "effect/schema/isFinite":
          return [one(`crate::checks::is_finite(${value})`, "expected a finite number")]
        case "effect/schema/isGreaterThan":
          return [
            one(`crate::checks::is_greater_than(${value}, ${json("exclusiveMinimum")})`, "expected a greater value"),
          ]
        case "effect/schema/isGreaterThanOrEqualTo":
          return [
            one(
              `crate::checks::is_greater_than_or_equal_to(${value}, ${json("minimum")})`,
              "expected a greater or equal value",
            ),
          ]
        case "effect/schema/isLessThan":
          return [one(`crate::checks::is_less_than(${value}, ${json("exclusiveMaximum")})`, "expected a lesser value")]
        case "effect/schema/isLessThanOrEqualTo":
          return [
            one(
              `crate::checks::is_less_than_or_equal_to(${value}, ${json("maximum")})`,
              "expected a lesser or equal value",
            ),
          ]
        case "effect/schema/isBetween": {
          const lower = payload?.exclusiveMinimum === true ? "is_greater_than" : "is_greater_than_or_equal_to"
          const upper = payload?.exclusiveMaximum === true ? "is_less_than" : "is_less_than_or_equal_to"
          return [
            one(`crate::checks::${lower}(${value}, ${json("minimum")})`, "expected a value in range"),
            one(`crate::checks::${upper}(${value}, ${json("maximum")})`, "expected a value in range"),
          ]
        }
        case "effect/schema/isMinLength":
          return [
            kind === "Arrays"
              ? one(`crate::checks::is_min_items(${value}, ${json("minLength")})`, "expected more items")
              : one(`crate::checks::is_min_length(${value}, ${json("minLength")})`, "expected a longer value"),
          ]
        case "effect/schema/isMaxLength":
          return [
            kind === "Arrays"
              ? one(`crate::checks::is_max_items(${value}, ${json("maxLength")})`, "expected fewer items")
              : one(`crate::checks::is_max_length(${value}, ${json("maxLength")})`, "expected a shorter value"),
          ]
        case "effect/schema/isPattern": {
          const source = payload?.source
          if (typeof source !== "string") {
            throw new Error(`strict: ${this.root} ${base._tag}: ${id} needs a string source`)
          }
          return [one(`crate::checks::is_pattern(${value}, ${JSON.stringify(source)})`, "expected a matching value")]
        }
        case "effect/schema/isStartingWith":
          return [
            one(`crate::checks::is_starting_with(${value}, ${string(payload, "startsWith")})`, "expected a prefix"),
          ]
        case "effect/schema/isEndingWith":
          return [one(`crate::checks::is_ending_with(${value}, ${string(payload, "endsWith")})`, "expected a suffix")]
        case "effect/schema/isIncluding":
          return [one(`crate::checks::is_including(${value}, ${string(payload, "includes")})`, "expected a substring")]
        default:
          throw new Error(`strict: ${this.root} ${base._tag}: unsupported check ${id ?? filter._tag}`)
      }
    })
  }

  private filters(checks: ReadonlyArray<Check>): Array<Filter> {
    return checks.flatMap((check) => (check._tag === "FilterGroup" ? this.filters(check.checks) : [check]))
  }

  private guard(condition: Condition, indent: string) {
    return `${indent}if !${condition.condition} {
${indent}    return Err(path.at(${JSON.stringify(condition.reason)}));
${indent}}`
  }

  private guards(conditions: ReadonlyArray<Condition>, spaces: number) {
    const indent = " ".repeat(spaces)
    return `${conditions.map((condition) => this.guard(condition, indent)).join("\n")}\n`
  }

  private closure(conditions: ReadonlyArray<Condition>, type: string) {
    if (conditions.length === 1) {
      const single = conditions[0]!
      return `|value: &${type}, path| {
            if ${single.condition} {
                Ok(())
            } else {
                Err(path.at(${JSON.stringify(single.reason)}))
            }
        }`
    }
    return `|value: &${type}, path| {
${this.guards(conditions, 12)}            Ok(())
        }`
  }
}

function checksOf(r: R): ReadonlyArray<Check> {
  return "checks" in r ? r.checks : []
}

function withoutChecks(r: R): R {
  if (!("checks" in r) || r.checks.length === 0) return r
  return { ...r, checks: [] } as R
}

function uniqueVariants(names: ReadonlyArray<string>) {
  const seen = new Set<string>()
  return names.map((name) => {
    const base = name === "" ? "V" : name
    let unique = base
    for (let index = 2; seen.has(unique); index++) unique = `${base}${index}`
    seen.add(unique)
    return unique
  })
}

function string(payload: { readonly [key: string]: unknown } | null | undefined, key: string) {
  const value = payload?.[key]
  if (typeof value !== "string") throw new Error(`strict: check needs a string ${key}`)
  return JSON.stringify(value)
}
