# Collaboration: design (Stage 1a foundation)

Status: Stage 1a design, 2026-11-24. Code lives in `src/collab.rs`, migration 11 in `src/db.rs`, the auth hook in
`src/server.rs`, and the UI in `ui/js/views/collab.js` (Settings › Collaborators and Settings › Join a project).

## 1. Topology: the owner hosts

- The **owner's** Model Roamer is the project's home. It keeps the project's threads, history, memory, artifacts
  and rules. Nothing about a project moves to a central service; there is no account server.
- Each **collaborator** runs their own Model Roamer (Windows or Linux) and joins one host project with a
  **per-person invite link**. Their agents run on their own machine, under their own Claude / Codex / Antigravity
  logins. Logins and API keys never cross machines.
- Two Model Roamers talk **server to server**: the guest's app calls the host's API with its member token. The
  guest's browser windows only ever talk to their own local app, which proxies. The host token never reaches a
  browser, a cookie or an agent's environment.
- Transport is whatever already reaches the host: LAN, Tailscale, WireGuard, or the owner's domain (Settings ›
  Remote access). The owner must have sharing turned on.

```
 guest UI ──loopback──▶ guest Model Roamer ──HTTPS / tailnet, Bearer member token──▶ host Model Roamer
 guest agents ─MCP──▶ guest Model Roamer ─(Stage 1b bridge)───────────────────────▶ /api/collab/host/*
```

## 2. Identity

Host side (migration 11):

| table | what |
|---|---|
| `collab_users` | a person known to this host: `id` (usr_…), `name`, `instance_id` of their app, `created_at`, `revoked_at` |
| `collab_members` | that person's membership of one project: `role`, `perms` overrides, `token_hash`, `last_seen`, `last_ip`, `revoked_at` |
| `collab_invites` | one per person: `project_id`, `name`, `role`, `perms`, `secret_hash`, `expires_at`, `used_at`, `used_by`, `revoked_at` |
| `collab_rules` | per project: what each role may do (`{ "collaborator": {perm: bool}, "viewer": {...} }`) |
| `threads.user_id` | who runs the thread: `''` = the owner (every existing thread), else a collab user id |

Guest side: `collab_links` (one row per joined host project: host URL, host id/name, project id/name, member id,
role, perms, **token**, status, last error, last sync).

- **The owner is implicit.** The local token and the owner's paired devices (`Who::Local`, `Who::Device`) are the
  owner. No owner row is needed, so a project with no collaborators behaves exactly as before.
- **Roles:** `owner` (implicit, everything), `collaborator`, `viewer`. Role sets the defaults; the owner can
  override single permissions per person.
- **Tokens:** one random 256-bit token per membership (so per person *and* per project: least privilege). Only its
  SHA-256 is stored on the host. The plain token is returned once, at join, and kept in the guest's DB (never sent
  to the guest's UI).
- **Revocation:** revoking a member (or the whole person, or a still-unused invite) takes effect on the next request:
  the token lookup requires `revoked_at IS NULL`. The guest sees 401, marks the link `revoked` and stops syncing.
  A guest can also leave (`POST host/leave` revokes its own membership).
- Member tokens are accepted **only** under `/api/collab/host/*`. They are not the local token or a device token,
  so every other route (`/api/*`, `/api/ws`, `/auth` cookies) rejects them with 401.

## 3. Invite and join

1. Owner: Settings › Collaborators › pick project › **Invite a person** (name, role, expiry, which address). The
   host makes `inv_<id>.<secret>` (secret hashed in the DB), and shows a link and QR:
   `https://host.tailnet.ts.net:4630/#/join?invite=inv_x.SECRET`.
2. The owner sends it to that one person (chat, email). Single use, default expiry 7 days, revocable until used.
3. Guest: Settings › **Join a project** › paste the link, confirm their display name. Their app probes
   `GET /api/hello` on the host (must be Model Roamer, must not be itself), then `POST /api/collab/join`
   `{invite, name, instance_id}`.
4. Host checks the invite (exists, not used/expired/revoked; 12 failures per 10 minutes per host, like pairing),
   makes or reuses the `collab_users` row (same `instance_id` = same person), creates the membership with a fresh
   token, marks the invite used, tells the owner's UI, and posts a short note to the project lead
   (`<wake reason="collaborator-joined">`, not a turn on its own: no wake cost).
5. Guest stores the link and opens the host project view.

## 4. How the guest mirrors the host

Host API for members (`/api/collab/host/*`, Bearer member token, every call permission-checked):

| route | perm | what |
|---|---|---|
| `GET me` | | member, role, effective perms, project, host |
| `GET project` | | project summary (no local paths), repos (URLs only), visible threads, a `cursor` |
| `GET changes?since=&wait=` | | **incremental sync**: threads, messages, memory and artifacts changed after `since`, capped, plus the next cursor. Long-polls up to `wait` seconds (max 25) on the host's event bus, so idle guests cost one parked request, not a polling loop |
| `GET threads/{id}/messages?before=&limit=` | view | one page of a thread's history |
| `GET memory`, `GET memory/{name}` | | project memory (+ global with `read_global_memory`) |
| `POST memory` | `edit_memory` | write a project memory entry, attributed to the person |
| `GET artifacts` | | project artifacts with signed host URLs |
| `POST artifacts` | `publish_artifacts` | publish text/markdown/html into the project |
| `POST threads` | `create_threads` | open a thread that **runs on the guest's machine** (`threads.user_id` = them) |
| `POST threads/{id}/messages` | own thread, or `message_threads` / `message_lead` | post into a thread. Host threads run on the **owner's** agents, so this is its own permission |
| `POST threads/{id}/merge` | `merge` | merge a thread's branch into the base branch |
| `POST leave` | | revoke own membership |
| `POST presence`, `POST mcp`, `POST threads/{id}/transfer` | | named extension points (501 until their stage) |

Guest side: `/api/collab/links` (list/join/leave) and `/api/collab/links/{id}/host/{path}`, a narrow proxy that
adds the token. Proxy rules: connect timeout 6 s, request timeout 20 s (long polls: `wait` + 10 s), two retries with
backoff for idempotent GETs on connection errors only, never for POSTs. A 401 from the host marks the link
`revoked`; a network error marks it `offline` with the error and keeps the last-known cursor so it resumes cleanly.

**Live events and offline:** the guest view runs one long-poll loop (`changes?since=cursor&wait=20`). On success it
applies the delta and loops at once; on failure it backs off 2 s → 4 s → … → 30 s and shows "offline, retrying".
Nothing is lost while offline: the cursor is a host timestamp and the next call returns everything after it (with a
1 ms overlap; the client de-duplicates by id). Stage 1b moves this loop into the guest's server so its agents get
the same feed.

**The guest's runner and the host (Stage 1b, extension point `collab::bridge`):** a guest thread runs locally like
any thread, but its MCP tools that touch shared state (memory, artifacts, other threads, messages to the lead) are
forwarded by the guest's app to `POST host/mcp` with the member token, and the host answers them with the same
`tools.rs` code, scoped to that member's permissions and project. Local-only tools (git in the guest's worktree)
stay local. Same timeouts and retries as `mcp.rs` (3 tries, backoff), plus an "host unreachable, retrying" tool
error after 20 s so the agent can carry on with local work.

## 5. Keeping the host responsive and cheap

- **Cursors, not reloads:** `changes` returns only rows with `updated_at`/`created_at` > cursor, capped at 200
  messages, bodies of list entries clipped to 240 characters. Full message bodies come from paged history.
- **Long-poll, not websockets, for members:** one parked request per guest, woken by the existing bus. Max 2
  concurrent long-polls per member (extras return at once). No member ever subscribes to the raw bus (which carries
  every project's events and the owner's terminals).
- **Digests for agents:** a guest's agent is never handed the whole host history. Its prompt gets the memory index
  (names only, as today), its own thread, and a short "since your last turn" digest (who is doing what, threads
  changed, memory changed: Stage 1d). It reads more with paged tools (`read_thread(id, before)`).
- **No wake per event:** joins, guest memory writes and guest thread activity reach the owner's lead as batched,
  non-waking notes (`pending = 0`) or through the existing wake batching; only an explicit `message_lead` wakes it.
- **Indexes:** migration 11 indexes the token hash and `threads(project_id, updated_at)` already exists.

## 6. Permissions the owner sets (Settings › Collaborators › Rules)

| permission | collaborator default | viewer default | meaning |
|---|---|---|---|
| `view_threads` | yes | yes | see the project's shared threads (lead and workers) and their history |
| `view_others_chats` | no | no | see other people's per-person chats (Stage 1c) |
| `create_threads` | yes | no | open threads of their own (they run on their machine) |
| `message_threads` | no | no | post into threads run by the owner's agents (uses the owner's plan) |
| `message_lead` | no | no | talk to the owner's project lead (wakes it, uses the owner's plan) |
| `edit_memory` | yes | no | write project memory |
| `read_global_memory` | no | no | read the owner's global memory (it spans all the owner's projects) |
| `publish_artifacts` | yes | no | publish artifacts into the project |
| `merge` | no | no | merge threads into the base branch |

Always owner-only: inviting, rules, project settings, repositories, deleting anything, agent and machine settings,
terminals, files outside the project. Effective permissions = built-in role defaults ← project rules for that role ←
the person's overrides. Unknown keys are ignored, so later stages can add permissions without a migration.

## 7. Data scopes

| scope | lives on | who sees it |
|---|---|---|
| global memory | owner's host (`memory.project_id IS NULL`) | owner; members only with `read_global_memory`. A guest's own global memory stays on the guest's machine |
| project memory | host | every member; written with `edit_memory` |
| collaborator memory | host, `memory.user_id` (Stage 1b) | only that person (and their agents); the owner can see it exists, not read it, unless shared |
| per-person project chat | host, a `chat` thread with `user_id` (Stage 1c) | that person; others with `view_others_chats` |
| shared threads | host `threads` (`user_id` says whose agents run it) | members with `view_threads` |
| artifacts | host files + signed URLs | members; publish with `publish_artifacts` |

## 8. Security

- **Auth on every request.** Member routes need the Bearer token; every handler re-checks membership, project and
  permission. Join is the only public collab route and is rate limited.
- **No secret leakage across users:** member responses never include settings, `github_token`, the local token,
  local folder paths (`projects.folder`, `repos.folder`, `threads.workspace`), session ids, terminals, logs,
  devices or other projects. Message `meta` is reduced to safe keys. Guest tokens are stored only in the guest's DB
  and never returned by its API.
- **Artifact links** are signed per artifact (`/a/{id}/{sig}`), so a member sees only the project's artifacts. A
  revoked member who kept a link can still open that one artifact; Stage 1b can add per-member artifact grants.
- **Transport:** plain `http://` is fine over Tailscale or WireGuard (already encrypted) and on a trusted LAN.
  Anywhere else use HTTPS: Tailscale `serve` (one button in Remote access), a TLS cert/key, or a reverse proxy at
  the owner's domain. The join screen warns when an invite uses plain http to a public address.
- **Tailscale guidance:** share the host with the collaborator's tailnet (Tailscale "share machine") rather than
  inviting them into the owner's whole tailnet; set Listen on to the Tailscale IP to keep the host off the LAN.
- Revocation is immediate; invites are single use and expire; tokens are hashed at rest on the host.

## 9. Later stages plug in here

- **1b collaborator memory:** `memory.user_id` column + unique index on `(project, user, name)`; `collab::memory_scope`
  picks the scope for a member; the `write_memory` tool gains `scope: "me"`. Bridge `host/mcp` goes live.
- **1c per-person project chats:** `collab::personal_chat` makes or finds the `chat` thread with `user_id` for a
  member; `view_others_chats` gates the others.
- **1d presence:** `POST host/presence {thread, doing, files}` keeps an in-memory map with a 90 s TTL (`collab::Presence`);
  the digest and the lead's prompt list who is working on what; claims (`claims` table) already stop file overlap.
- **Runner gate:** `collab::runs_here(thread)` is false for threads with a `user_id`. The host runner must call it
  in `runnable()` so a guest's thread is never run on the owner's accounts (one-line hook in `runner.rs`).
- **Stage 2 thread transfer:** the thread's branch is pushed to the shared GitHub repo by its current runner;
  `POST host/threads/{id}/transfer {to}` flips `threads.user_id` after the push; the new holder fetches the branch,
  makes a worktree, and starts from a digest (title, instructions, criteria, last messages, a summary) instead of the
  full history, which stays readable through paged `read_thread`. Agents ask with a `request_transfer` tool, which
  becomes a decision for the current holder.
- **Stage 3 universal chat and operator:** a project `group` thread on the host that every member sees (each message
  carries `user_id`); `collab_rules.operator` names the operator (a user's local agent or an API agent with their own
  key). `@operator` routes to that user's runner via the bridge with the group digest; `POST host/threads/{id}/claim`
  sets `user_id` so only the claimer's agents pick it up; the operator posts the hand-off into the claimer's
  personal chat.
