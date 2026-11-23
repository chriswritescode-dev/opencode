import { OpenApi } from "effect/http-api"
import { api } from "../contract/model"

const root = new URL("../..", import.meta.url).pathname

await Bun.write(`${root}crates/contract/served-openapi.json`, JSON.stringify(OpenApi.fromApi(api)))
