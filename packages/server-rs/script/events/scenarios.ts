import type { Fixture } from "./typescript"

const project = [
  `INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('prj_1', '/repo', '[]', 1, 2)`,
]

const created = () => ({
  kind: "publish" as const,
  id: "evt_1",
  created: 1000,
  type: "session.created",
  data: { sessionID: "ses_1", projectID: "prj_1", location: { directory: "/repo" }, slug: "slug", version: "1" },
})

export const fixtures: Fixture[] = [
  {
    name: "created-minimal",
    group: "session",
    start: project,
    logAfter: [0, 1],
    steps: [
      created(),
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.renamed",
        data: { sessionID: "ses_1", title: "Renamed" },
      },
    ],
  },
  {
    name: "created-full",
    group: "session",
    start: project,
    logAfter: [0],
    steps: [
      {
        kind: "publish",
        id: "evt_1",
        created: 1000,
        type: "session.created",
        data: {
          sessionID: "ses_1",
          projectID: "prj_1",
          location: { directory: "/repo", workspaceID: "wrk_1" },
          subpath: "packages/app",
          parentID: "ses_0",
          slug: "full-slug",
          title: "Full session",
          agent: "build",
          model: { id: "gpt-5", providerID: "openai", variant: "high" },
          metadata: { owner: "test" },
          permissions: [{ action: "edit", resource: "*", effect: "allow" }],
          version: "1.2.3",
        },
      },
    ],
  },
  {
    name: "created-no-persist",
    group: "session",
    persist: false,
    start: project,
    logAfter: [0],
    steps: [
      created(),
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.renamed",
        data: { sessionID: "ses_1", title: "Renamed" },
      },
    ],
  },
  {
    name: "row-updates",
    group: "session",
    start: project,
    logAfter: [0, 2],
    steps: [
      created(),
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.renamed",
        data: { sessionID: "ses_1", title: "Renamed" },
      },
      {
        kind: "publish",
        id: "evt_3",
        created: 3000,
        type: "session.metadata.updated",
        data: { sessionID: "ses_1", metadata: { owner: "test", flag: true } },
      },
      {
        kind: "publish",
        id: "evt_4",
        created: 4000,
        type: "session.permissions",
        data: { sessionID: "ses_1", permissions: [{ action: "edit", resource: "*", effect: "deny" }] },
      },
      {
        kind: "publish",
        id: "evt_5",
        created: 5000,
        type: "session.archived",
        data: { sessionID: "ses_1" },
      },
      {
        kind: "publish",
        id: "evt_6",
        created: 6000,
        type: "session.unarchived",
        data: { sessionID: "ses_1" },
      },
    ],
  },
  {
    name: "viewed-watermark",
    group: "session",
    start: project,
    logAfter: [0],
    steps: [
      created(),
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.viewed",
        data: { sessionID: "ses_1", idle: 5000 },
      },
      {
        kind: "publish",
        id: "evt_3",
        created: 3000,
        type: "session.viewed",
        data: { sessionID: "ses_1", idle: 3000 },
      },
      {
        kind: "publish",
        id: "evt_4",
        created: 4000,
        type: "session.viewed",
        data: { sessionID: "ses_1", idle: 7000 },
      },
    ],
  },
  {
    name: "execution-started",
    group: "session",
    start: project,
    logAfter: [0],
    steps: [
      created(),
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.execution.started",
        data: { sessionID: "ses_1" },
      },
    ],
  },
  {
    name: "revert",
    group: "session",
    start: project,
    logAfter: [0],
    steps: [
      created(),
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.revert.staged",
        data: {
          sessionID: "ses_1",
          revert: {
            messageID: "msg_1",
            files: [
              {
                file: "src/index.ts",
                patch: "@@ -1 +1 @@\n-old\n+new\n",
                additions: 1,
                deletions: 1,
                status: "modified",
              },
            ],
          },
        },
      },
      {
        kind: "publish",
        id: "evt_3",
        created: 3000,
        type: "session.revert.cleared",
        data: { sessionID: "ses_1" },
      },
    ],
  },
  {
    name: "usage",
    group: "session",
    start: project,
    logAfter: [0],
    steps: [
      created(),
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.usage.recorded",
        data: {
          sessionID: "ses_1",
          source: "generate",
          cost: 0.5,
          tokens: { input: 10, output: 20, reasoning: 0, cache: { read: 3, write: 4 } },
        },
      },
      {
        kind: "publish",
        id: "evt_3",
        created: 3000,
        type: "session.usage.recorded",
        data: {
          sessionID: "ses_1",
          source: "title",
          cost: 1,
          tokens: { input: 1, output: 2, reasoning: 0, cache: { read: 0, write: 0 } },
        },
      },
    ],
  },
  {
    name: "deleted",
    group: "session",
    start: [
      ...project,
      `INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) VALUES ('ses_1', 'prj_1', 'slug-1', '/repo', '1', 1, 1)`,
      `INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) VALUES ('ses_2', 'prj_1', 'slug-2', '/repo', '1', 2, 2)`,
      `INSERT INTO blob (hash, mime, bytes, data, time_created) VALUES ('hash_both', 'text/plain', 2, X'6869', 1)`,
      `INSERT INTO blob (hash, mime, bytes, data, time_created) VALUES ('hash_first', 'text/plain', 2, X'6869', 1)`,
      `INSERT INTO blob_ref (session_id, message_id, hash, seq) VALUES ('ses_1', 'msg_1', 'hash_both', 0)`,
      `INSERT INTO blob_ref (session_id, message_id, hash, seq) VALUES ('ses_2', 'msg_2', 'hash_both', 1)`,
      `INSERT INTO blob_ref (session_id, message_id, hash, seq) VALUES ('ses_1', 'msg_1', 'hash_first', 2)`,
    ],
    logAfter: [0],
    steps: [
      {
        kind: "publish",
        id: "evt_1",
        created: 1000,
        type: "session.deleted",
        data: { sessionID: "ses_1" },
      },
    ],
  },
  {
    name: "publish-metadata-location",
    group: "session",
    start: project,
    logAfter: [0],
    steps: [
      created(),
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.renamed",
        data: { sessionID: "ses_1", title: "Renamed" },
        metadata: { trace: "abc" },
        location: { directory: "/other", workspaceID: "wrk_1" },
      },
    ],
  },
  {
    name: "replay",
    group: "session",
    start: project,
    logAfter: [0, 1],
    steps: [
      {
        kind: "replay",
        id: "evt_1",
        created: 1000,
        seq: 0,
        aggregateID: "ses_1",
        type: "session.created.1",
        data: { sessionID: "ses_1", projectID: "prj_1", location: { directory: "/repo" }, slug: "slug", version: "1" },
        publish: true,
      },
      {
        kind: "publish",
        id: "evt_2",
        created: 2000,
        type: "session.renamed",
        data: { sessionID: "ses_1", title: "Renamed" },
      },
    ],
  },
]
