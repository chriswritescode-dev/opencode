import "./capture.js"
import { FIXED_EPOCH_MS } from "./capture.js"
import { Cause, Effect, Exit } from "effect"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"

const tree = process.env.OPENCODE_UPSTREAM_TREE
if (!tree) throw new Error("adc: OPENCODE_UPSTREAM_TREE is not set")

const KEY = `-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDboNZroYuLXww6
mg1nJvCpk3tVSkdoNP3yyaaY8a0qj+urtEsKscbequDzZZNb0Yt4376do6yFGOsd
aC9NT7TQnpkTl3R9eFhUoc8Patjqin2VPKw38rcr3Zsh4efYlPLRLr25OKjyvWV8
NSD3X0xyltQTOb+LvkLwH7juJ8/IZVbok6WoN6WF0Tmam9NHt/Bj17HUeINfwsEZ
G1jIkQu+CsCeSulgAijrXeHryBFQ9dbQtsGGxt9+VAwQhmPD6EDJRb2ZKB6doaSc
T6FaiK6QGBaRtJyuEqY7PZQ+RBQOlkkTGGJJpGbycGgS82Mbb1/3iQdxROeG2arT
BVjUo6fTAgMBAAECggEACQQ02U1T/8BCrcM2SqZQGf4agZlfjhki1c1OgBfn3ofz
CmUH8aPzwJ9uRb/FSW80VR2GY01dX0dzG41z564eg7QHH8VHfIEsyyirafwedJ6D
4lUP0uotd+kMndr63CrdPxY2prx7Px/kYzGBLXalmkyz3ygvQ2rnr5j1zB0pTBY9
dxCtoDqvv1UTeKKrnRFKVtidpRZMopCVCeCfLqCVOH9wwg1sKCWy2vYgVGauuaPy
JJNLKjbH6NeaHZuMaf+YM9UVVOM7F59mzFKHOI51huRCNH3RhIUyW+S15grnpSCc
3XNC8Z4g/jbldcI56vsPWz/HNTTTAj46EqQ3AD3/QQKBgQD7W4wGtd54Jk/7+RFh
cu1slQVkTX9ICJuxEdY6iugz7eTbXCymw5BkDnYGv2+aC+JuYRXtaTtsek3jCPww
wk0f6A0e4SxmKXWSkzPcP1zSH+qVUynKI3lvEDkDSqh0Hkm2ZAeb9rmu3UFYJJWd
x1Iy6UvxrZ3hzazSOZg+2HQ3EwKBgQDfr0Ud7sAhnsvPJGfVhn350/a++U0VxO8/
XoHFNGeh5zTbdWL8YlJej9YVQmtLIyOCRGOHhO4PYeNW60/3M0XzT2CRNdAMfB4j
eWbKkbXqS/I3q3yAZSMA7JfUe9VQrQ0oEA76GMYHQxqi081E87rCXQ6n9o3uEn+M
cdEK6HEkQQKBgH/spaU8pk6r4QUyS6A255LdfPMBZgf5KZPCqi79RxQ2vMTPcoIl
AbWKjpL8lwXlioX2rzw21GFvFXi4t/Dw0mOsrBDNNNWBwC6q8Dfln43pZHA+9QPF
biYSMZH1Sypdx9IlsqGzvei6/minpeW47aKvjVEgtTuYe546ZbuGWg81AoGBAL+T
27yROVkSKjiVU6EbeVW5ZwGVf6H/c3FwvfBqu798yFaVqVvD0gxPYOoAYuiNheIS
NLRi84mVKyLMk0aDUYvcFjcIQdIPk7iKR7GDLq0msqNfaliFRn+KI1fHX6GjNTt0
1X76LNEXdf6zLIG7xd6l2OGeV1zp1vgwmvuerRiBAoGAUudFwVk1YJEbDU2BikgM
HGr/ARpWrMYpb8J6OQBQSvu3YAliWuU8xzJqjMCD8BUHUGNsO7bFXBJzUYnon+c9
7FqvDSrW+G2+Mwv1R6F0ca4t4BzBQ6V1pDev5vMEUllwnyydvyBHMHFmJ2GUdQs2
jnvIwL5JEwNa32TT8tC3+SM=
-----END PRIVATE KEY-----`

type AdcResponse = {
  status: number
  headers: Record<string, string>
  body: string
}

type AdcRequest = {
  method: string
  url: string
  headers: Record<string, string>
  body: string
  response: AdcResponse
}

type AdcStep = {
  requests: Array<AdcRequest>
  token?: string
  error?: string
}

type AdcCase = {
  name: string
  files: Record<string, string>
  env: Record<string, string>
  expiresIn?: number
  calls: number
}

type AdcResult = {
  name: string
  files: Record<string, string>
  env: Record<string, string>
  steps: Array<AdcStep>
}

type AdcOutput = {
  clockEpochMs: number
  cases: Array<AdcResult>
}

let requests: Array<AdcRequest> = []
let expiresIn = 3600
let tokenSequence = 1

const JSON_HEADERS: Record<string, string> = { "content-type": "application/json" }

const TRANSPORT_HEADERS = new Set([
  "user-agent",
  "x-goog-api-client",
  "accept",
  "accept-encoding",
  "connection",
  "host",
  "content-length",
  "transfer-encoding",
])

const respond = (url: string): AdcResponse => {
  const parsed = new URL(url)
  if (parsed.pathname.endsWith("/computeMetadata/v1/instance")) {
    return { status: 200, headers: { "metadata-flavor": "Google", ...JSON_HEADERS }, body: "{}" }
  }
  if (parsed.pathname.includes("/computeMetadata/v1/instance/service-accounts/")) {
    const body = JSON.stringify({
      access_token: `metadata-token-${tokenSequence++}`,
      expires_in: expiresIn,
      token_type: "Bearer",
    })
    return { status: 200, headers: { "metadata-flavor": "Google", ...JSON_HEADERS }, body }
  }
  if (parsed.pathname === "/token") {
    const body = JSON.stringify({
      access_token: `access-token-${tokenSequence++}`,
      expires_in: expiresIn,
      token_type: "Bearer",
    })
    return { status: 200, headers: JSON_HEADERS, body }
  }
  return { status: 404, headers: JSON_HEADERS, body: JSON.stringify({ error: "not_found" }) }
}

const adapter = async function (this: any, config: any) {
  const body = config.body
  const text =
    body === undefined || body === null
      ? ""
      : typeof body === "string"
        ? body
        : body instanceof URLSearchParams
          ? body.toString()
          : JSON.stringify(body)
  const headers: Record<string, string> = {}
  new Headers(config.headers).forEach((value: string, key: string) => (headers[key] = value))
  const response = respond(String(config.url))
  if (!headers["content-type"] && body instanceof URLSearchParams)
    headers["content-type"] = "application/x-www-form-urlencoded;charset=UTF-8"
  const recorded: Record<string, string> = {}
  for (const [key, value] of Object.entries(headers)) if (!TRANSPORT_HEADERS.has(key)) recorded[key] = value
  requests.push({ method: config.method ?? "GET", url: String(config.url), headers: recorded, body: text, response })
  const res = new Response(response.body, { status: response.status, headers: response.headers })
  const data = await (this as any).getResponseData(config, res)
  return Object.assign(res, { config, data })
}

const patchGaxios = async (packageDirectory: string) => {
  const gaxiosDirectory = Bun.resolveSync("gaxios/package.json", packageDirectory).replace(/\/package\.json$/, "")
  const gaxios: any = await import(`${gaxiosDirectory}/build/cjs/src/index.js`)
  gaxios.Gaxios.prototype._defaultAdapter = adapter
}

const gal = Bun.resolveSync("google-auth-library", `${tree}/packages/ai`)
const galDirectory = gal.replace(/\/[^/]+$/, "")
await patchGaxios(galDirectory)
const gcpDirectory = Bun.resolveSync("gcp-metadata/package.json", galDirectory).replace(/\/package\.json$/, "")
const gcpMetadata: any = await import(`${gcpDirectory}/build/src/index.js`)
await patchGaxios(gcpDirectory)

const { GoogleVertexShared } = await import(`${tree}/packages/ai/src/providers/google-vertex-shared.ts`)
const { Auth } = await import(`${tree}/packages/ai/src/route/auth.ts`)

const ENV_KEYS = [
  "HOME",
  "CLOUDSDK_CONFIG",
  "GOOGLE_APPLICATION_CREDENTIALS",
  "google_application_credentials",
  "METADATA_SERVER_DETECTION",
  "GCE_METADATA_HOST",
  "GCE_METADATA_IP",
  "GOOGLE_CLOUD_QUOTA_PROJECT",
]

const authorizedUser = (token: string) =>
  JSON.stringify({
    type: "authorized_user",
    client_id: "adc-client.apps.googleusercontent.com",
    client_secret: "adc-client-secret",
    refresh_token: token,
    quota_project_id: "adc-quota-project",
  })

const serviceAccount = () =>
  JSON.stringify({
    type: "service_account",
    client_email: "adc-service@adc-project.iam.gserviceaccount.com",
    private_key_id: "adc-key-id",
    private_key: KEY,
    project_id: "adc-project",
    token_uri: "https://oauth2.googleapis.com/token",
  })

const cases: Array<AdcCase> = [
  {
    name: "authorized-user-cloudsdk-config",
    files: { "/adc/sdk/application_default_credentials.json": authorizedUser("adc-refresh-token") },
    env: { HOME: "/adc/home", CLOUDSDK_CONFIG: "/adc/sdk" },
    calls: 1,
  },
  {
    name: "authorized-user-home",
    files: { "/adc/home/.config/gcloud/application_default_credentials.json": authorizedUser("adc-refresh-token") },
    env: { HOME: "/adc/home" },
    calls: 1,
  },
  {
    name: "authorized-user-environment",
    files: { "/adc/creds/user.json": authorizedUser("adc-refresh-token") },
    env: { HOME: "/adc/home", GOOGLE_APPLICATION_CREDENTIALS: "/adc/creds/user.json" },
    calls: 1,
  },
  {
    name: "service-account-environment",
    files: { "/adc/creds/service.json": serviceAccount() },
    env: { HOME: "/adc/home", GOOGLE_APPLICATION_CREDENTIALS: "/adc/creds/service.json" },
    calls: 1,
  },
  {
    name: "authorized-user-cached",
    files: { "/adc/sdk/application_default_credentials.json": authorizedUser("adc-refresh-token") },
    env: { HOME: "/adc/home", CLOUDSDK_CONFIG: "/adc/sdk" },
    calls: 2,
  },
  {
    name: "authorized-user-refresh",
    files: { "/adc/sdk/application_default_credentials.json": authorizedUser("adc-refresh-token") },
    env: { HOME: "/adc/home", CLOUDSDK_CONFIG: "/adc/sdk" },
    expiresIn: 300,
    calls: 2,
  },
  {
    name: "service-account-cached",
    files: { "/adc/creds/service.json": serviceAccount() },
    env: { HOME: "/adc/home", GOOGLE_APPLICATION_CREDENTIALS: "/adc/creds/service.json" },
    calls: 2,
  },
  {
    name: "missing-credentials",
    files: {},
    env: { HOME: "/adc/home", METADATA_SERVER_DETECTION: "none" },
    calls: 1,
  },
  {
    name: "missing-cloudsdk-config",
    files: {},
    env: { HOME: "/adc/home", CLOUDSDK_CONFIG: "/adc/sdk", METADATA_SERVER_DETECTION: "none" },
    calls: 1,
  },
  {
    name: "missing-credentials-file",
    files: {},
    env: { HOME: "/adc/home", GOOGLE_APPLICATION_CREDENTIALS: "/adc/creds/missing.json" },
    calls: 1,
  },
  {
    name: "metadata-server",
    files: {},
    env: { HOME: "/adc/home", GCE_METADATA_HOST: "metadata.google.internal" },
    calls: 2,
  },
]

const results: Array<AdcResult> = []
for (const testCase of cases) {
  const scratch = await mkdtemp(join(tmpdir(), "opencode-adc-"))
  const normalize = (value: string) => value.replaceAll(scratch, "/adc")
  try {
    for (const [path, content] of Object.entries(testCase.files)) {
      const actual = path.replace(/^\/adc/, scratch)
      await mkdir(dirname(actual), { recursive: true })
      await writeFile(actual, content)
    }
    for (const key of ENV_KEYS) delete process.env[key]
    for (const [key, value] of Object.entries(testCase.env)) process.env[key] = value.replace(/^\/adc/, scratch)
    gcpMetadata.resetIsAvailableCache()
    gcpMetadata.setGCPResidency(false)
    expiresIn = testCase.expiresIn ?? 3600
    tokenSequence = 1
    requests = []

    const definition = GoogleVertexShared.oauth({}, "adc-project")
    const apply = Auth.toEffect(definition)
    const steps: Array<AdcStep> = []
    for (let call = 0; call < testCase.calls; call++) {
      requests = []
      const exit = await Effect.runPromiseExit(
        apply({ method: "GET", url: "https://aiplatform.googleapis.com/", body: "", headers: {} }),
      )
      if (Exit.isSuccess(exit)) {
        const headers: any = exit.value
        steps.push({ requests, token: String(headers.authorization).replace(/^Bearer /, "") })
        continue
      }
      const error: any = Cause.findErrorOption(exit.cause)
      if (error._tag === "Some") {
        const failure: any = error.value
        steps.push({ requests, error: failure.reason?.message ?? String(failure) })
        continue
      }
      steps.push({ requests, error: Cause.pretty(exit.cause) })
    }
    const recordedFiles: Record<string, string> = {}
    for (const [path, content] of Object.entries(testCase.files)) recordedFiles[normalize(path)] = content
    const recordedEnv: Record<string, string> = {}
    for (const [key, value] of Object.entries(testCase.env)) recordedEnv[key] = normalize(value)
    results.push({ name: testCase.name, files: recordedFiles, env: recordedEnv, steps })
  } finally {
    for (const key of ENV_KEYS) delete process.env[key]
    await rm(scratch, { recursive: true, force: true })
  }
}

verify(results)
console.log(JSON.stringify({ clockEpochMs: FIXED_EPOCH_MS, cases: results } satisfies AdcOutput, null, 2))

function verify(results: Array<AdcResult>) {
  const first = results[0]
  if ((first?.steps[0]?.requests.length ?? 0) === 0) throw new Error("adc: first case recorded no request")
  for (const result of results) {
    if (result.steps.some((step) => step.token !== undefined) && (result.steps[0]?.requests.length ?? 0) === 0)
      throw new Error(`adc: ${result.name} produced a token without a first-step request`)
  }
  const metadata = results.find((result) => result.name === "metadata-server")
  if (metadata === undefined) throw new Error("adc: metadata-server case is missing")
  const detection = metadata.steps[0]?.requests.some((request) => request.url.endsWith("/computeMetadata/v1/instance"))
  const token = metadata.steps[0]?.requests.some((request) =>
    request.url.includes("/computeMetadata/v1/instance/service-accounts/"),
  )
  if (!detection || !token) throw new Error("adc: metadata-server step 0 missed detection or token request")
}
