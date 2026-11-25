import { OauthCallbackPage } from "@opencode/core/oauth/page"
import { OauthRedirect } from "@opencode/core/oauth/redirect"
import { Effect } from "effect"

const area = process.argv[2]

if (area !== "oauth") {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

const state = "state-1"
const escapedProvider = `xAI</script><script>alert("provider")</script>`
const escapedTokenPath = `/token</script><script>alert("path")</script>`

function pasted(text: string) {
  const outcome = Effect.runSync(Effect.result(OauthRedirect.pasted(text, state)))
  if (outcome._tag === "Failure") return { kind: "failure" as const, message: outcome.failure.message }
  const value = outcome.success
  const params = Array.from(value.params.entries())
  return "error" in value
    ? { kind: "error" as const, error: value.error, params }
    : { kind: "code" as const, code: value.code, params }
}

const cases = {
  "success.default": {
    call: "success",
    options: null,
    html: OauthCallbackPage.success(),
  },
  "success.provider": {
    call: "success",
    options: { provider: "xAI" },
    html: OauthCallbackPage.success({ provider: "xAI" }),
  },
  "error.default": {
    call: "error",
    detail: "access_denied: The user denied the request.",
    options: null,
    html: OauthCallbackPage.error("access_denied: The user denied the request."),
  },
  "error.special": {
    call: "error",
    detail: `<script>alert("x")</script> & more`,
    options: null,
    html: OauthCallbackPage.error(`<script>alert("x")</script> & more`),
  },
  bootstrap: {
    call: "bootstrap",
    tokenPath: "/callback",
    provider: "xAI",
    html: OauthCallbackPage.bootstrap({ tokenPath: "/callback", provider: "xAI" }),
  },
  "bootstrap.escaped": {
    call: "bootstrap",
    tokenPath: escapedTokenPath,
    provider: escapedProvider,
    html: OauthCallbackPage.bootstrap({ tokenPath: escapedTokenPath, provider: escapedProvider }),
  },
  "pasted.valid": {
    call: "pasted",
    text: "http://127.0.0.1:1455/callback?state=state-1&code=code-1",
    state,
    result: pasted("http://127.0.0.1:1455/callback?state=state-1&code=code-1"),
  },
  "pasted.state-mismatch": {
    call: "pasted",
    text: "http://127.0.0.1:1455/callback?state=other&code=code-1",
    state,
    result: pasted("http://127.0.0.1:1455/callback?state=other&code=code-1"),
  },
  "pasted.error-description": {
    call: "pasted",
    text: "http://127.0.0.1:1455/callback?state=state-1&error=access_denied&error_description=The+user+denied+the+request",
    state,
    result: pasted(
      "http://127.0.0.1:1455/callback?state=state-1&error=access_denied&error_description=The+user+denied+the+request",
    ),
  },
  "pasted.error": {
    call: "pasted",
    text: "http://127.0.0.1:1455/callback?state=state-1&error=access_denied",
    state,
    result: pasted("http://127.0.0.1:1455/callback?state=state-1&error=access_denied"),
  },
  "pasted.missing-code": {
    call: "pasted",
    text: "http://127.0.0.1:1455/callback?state=state-1",
    state,
    result: pasted("http://127.0.0.1:1455/callback?state=state-1"),
  },
  "pasted.non-url": {
    call: "pasted",
    text: "not a url",
    state,
    result: pasted("not a url"),
  },
}

process.stdout.write(`${JSON.stringify(cases, null, 2)}\n`)
