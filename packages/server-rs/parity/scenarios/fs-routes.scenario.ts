import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: {
    "hello.txt": "hello parity\n",
    "notes.md": "# Notes\n",
    "docs/readme.txt": "docs parity\n",
  },
  steps: [
    {
      http: {
        method: "GET",
        path: "/api/fs/read/hello.txt",
        headers: { Range: "bytes=0-4" },
      },
    },
    {
      http: {
        method: "GET",
        path: "/api/fs/read/hello.txt",
        headers: { Range: "bytes=-3" },
      },
    },
    {
      http: {
        method: "GET",
        path: "/api/fs/read/hello.txt",
        headers: { Range: "bytes=99-100" },
      },
    },
    {
      http: {
        method: "GET",
        path: "/api/fs/read/hello.txt",
        headers: { "If-None-Match": "*" },
      },
    },
    { http: { method: "GET", path: "/api/fs/list", query: { path: "." } } },
    { http: { method: "GET", path: "/api/fs/list", query: { path: "docs" } } },
    { http: { method: "GET", path: "/api/fs/find", query: { query: "readme" } } },
    {
      http: {
        method: "POST",
        path: "/api/experimental/fs/write",
        query: { path: "nested/new.txt" },
        headers: { "content-type": "application/octet-stream" },
        body: "written\n",
        as: "written",
      },
    },
    { http: { method: "GET", path: "/api/fs/read/nested/new.txt" } },
    {
      http: {
        method: "POST",
        path: "/api/experimental/fs/watch",
        body: { path: "hello.txt" },
      },
    },
    {
      http: {
        method: "DELETE",
        path: "/api/experimental/fs/watch",
        body: { path: "hello.txt" },
      },
    },
  ],
})
