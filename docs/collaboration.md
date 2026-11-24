# Collaboration: design (Stage 1a foundation)

Status: Stage 1b, 2026-11-24. Code lives in `src/collab.rs` and `src/collab_bridge.rs`, migrations 11–12 in `src/db.rs`, the auth hook in
`src/server.rs`, and the UI in `ui/js/views/collab.js` (Settings › Collaborators and Settings › Join a project).

## 1. Topology: the owner hosts

- The **owner's** Model Roamer is the project's home. It keeps the project's threads, history, memory, artifacts
  and rules. Nothing about a project moves to a central service; there is no account server.
- Each **collaborator** runs their own Model Roamer (Windows or Linux) and joins one host project with a
  **per-person invite link**. Their agents run on their own machine, under their own Claude / Codex / Antigravity
  logins. Logins and API keys never cross machines.
- **Terms of service:** every agent runs only on its own user's account, with prompts that user wrote or approved.
  The host never runs a collaborator's thread (`collab::runs_here`, checked in `runner::runnable`), and nothing a
  collaborator sends is injected into the owner's agents: it is a visible message the owner's permissions allowed.
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
| `collab_members` | that person's membership of one project: `role`, `perms` overrides, `token_hash`, `requested_role` / `requested_at` (a role they asked for), `last_seen`, `last_ip`, `revoked_at` |
| `collab_invites` | one per person: `project_id`, `name`, `role`, `perms`, `secret_hash`, `expires_at`, `used_at`, `used_by`, `revoked_at` |
| `collab_rules` | per project: what each role may do (`{ "collaborator": {perm: bool}, "viewer": {...} }`) |
| `threads.user_id` | who runs the thread: `''` = the owner (every existing thread), else a collab user id |

Guest side: `collab_links` (one row per joined host project: host URL, host id/name, project id/name, member id,
role, perms, **token**, status, last error, last sync).

- **The owner is implicit.** The local token and the owner's paired devices (`Who::Local`, `Who::Device`) are the
  owner. No owner row is needed, so a project with no collaborators behaves exactly as before.
- **Roles:** `owner` (implicit, everything), `collaborator`, `operator`, `viewer`. Role sets the defaults; the
  owner can override single permissions per person.
- **Operator:** a collaborator the owner trusts to move work around. A collaborator asks **once**
  (`POST host/request-role {role: "operator"}`; one open request at a time, a second ask gets 409); the owner
  approves or declines **once** in Settings › Collaborators (`PATCH members/{id} {approve_request}`). An operator has
  `transfer_threads`: in Stage 2 they take threads over from the owner or others without per-thread approval;
  everyone else asks per transfer. The owner can also invite someone straight in as an operator.
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
   `{invite, name, instance_id, identity_proof}`. The proof is private to this pair of installations. An existing
   identity is reused only when the proof matches. A pre-1b identity is retained by its current membership;
   its first rejoin creates a new identity because the old installation provided no proof.
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
| `GET memory`, `GET memory/{name}` | | project memory, global with `read_global_memory`, or the caller's private host notes with `scope=collaborator` |
| `POST memory` | `edit_memory` for project scope | write shared project memory or private host notes with `scope: "collaborator"` |
| `GET artifacts` | | project artifacts with signed host URLs |
| `POST artifacts` | `publish_artifacts` | publish text/markdown/html into the project |
| `POST threads` | `create_threads` | open a thread that **runs on the guest's machine** (`threads.user_id` = them) |
| `POST threads/{id}/messages` | own thread, or `message_threads` / `message_lead` | post into a thread. Host threads run on the **owner's** agents, so this is its own permission |
| `POST threads/{id}/merge` | `merge` | merge a thread's branch into the base branch |
| `POST request-role` | | ask the owner once to become an operator |
| `POST leave` | | revoke own membership |
| `POST mcp` | per tool | invoke one of the bridge tools listed below |
| `POST presence`, `POST threads/{id}/transfer` | | named extension points |

Guest side: `/api/collab/links` (list/join/leave) and `/api/collab/links/{id}/host/{path}`, a narrow proxy that
adds the token. Proxy rules: connect timeout 6 s, request timeout 20 s (long polls: `wait` + 10 s), two retries with
backoff for idempotent GETs on connection errors only, never for POSTs. A 401 from the host marks the link
`revoked`; a network error marks it `offline` with the error and keeps the last-known cursor so it resumes cleanly.

**Live events and offline:** the guest view runs one long-poll loop (`changes?since=cursor&wait=20`). On success it
applies the delta and loops at once; on failure it backs off 2 s → 4 s → … → 30 s and shows "offline, retrying".
Nothing is lost while offline: the cursor is a host timestamp and the next call returns everything after it (with a
1 ms overlap; the client de-duplicates by id). Agents fetch shared data on demand through their tools.

**The guest's runner and the host.** Bind an existing local agent thread with
`POST /api/collab/links/{link_id}/agent-threads/{local_thread_id}`. Pass `{"host_thread_id":"th_…"}` to associate
one of the member's own threads on the host. An empty object enables project-level calls without a host thread.
`DELETE` at the same URL removes the binding. The guest starts the agent with the usual local thread controls.

The guest server sends `{tool, thread_id, arguments}` to `POST host/mcp` with the member token. The host determines
the project from that token and checks visibility or ownership for thread operations. The supported calls are
`read_memory`, `write_memory`, `list_threads`, `read_thread`, `check_status`, `list_artifacts`, `publish_artifact`,
`list_claims`, `claim_paths`, `release_paths` and `set_status`. Publishing accepts inline content and folder paths
inside the shared project. Claim and status changes require a bound host thread owned by the caller and the
corresponding permission. The endpoint never starts an agent on the host.

The guest agent's default memory index and search combine local collaborator and global memory with shared host
project memory. Reads by name prefer collaborator, then project, then global. Explicit collaborator and global
tool calls stay on the guest installation. The host's private notes and permitted host global memory remain
available through explicit scoped host API calls. Read-only bridge calls can retry twice within 20 seconds.
Writes have a 20-second request timeout and are sent once. If the host is unavailable, local memory remains usable.

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

| permission | collaborator | operator | viewer | meaning |
|---|---|---|---|---|
| `view_threads` | yes | yes | yes | see the project's shared threads (lead and workers) and their history |
| `view_others_chats` | no | no | no | see other people's per-person chats (Stage 1c) |
| `create_threads` | yes | yes | no | open threads of their own (they run on their machine) |
| `message_threads` | no | no | no | post into threads run by the owner's agents (uses the owner's plan) |
| `message_lead` | no | no | no | talk to the owner's project lead (wakes it, uses the owner's plan) |
| `edit_memory` | yes | yes | no | write project memory |
| `read_global_memory` | no | no | no | read the owner's global memory (it spans all the owner's projects) |
| `publish_artifacts` | yes | yes | no | publish artifacts into the project |
| `manage_claims` | yes | yes | no | claim paths and release claims for the member's own threads |
| `update_thread_status` | yes | yes | no | report status on the member's own threads |
| `merge` | no | no | no | merge threads into the base branch |
| `transfer_threads` | no | yes | no | take threads over from the owner or others without asking each time (Stage 2) |

Always owner-only: inviting, rules, project settings, repositories, deleting anything, agent and machine settings,
terminals, files outside the project. Effective permissions = built-in role defaults ← project rules for that role ←
the person's overrides. Unknown keys are ignored, so later stages can add permissions without a migration.

## 7. Data scopes

| scope | lives on | who sees it |
|---|---|---|
| global memory | owner's host (`memory.project_id` and `memory.user_id` are NULL) | owner; members only with `read_global_memory`. A guest's own global memory stays on the guest's machine |
| project memory | host | every member; written with `edit_memory` |
| collaborator memory | agent's installation, or the host if written via the scoped host API | only the caller and their agents. Sharing a note is an explicit write to project memory |
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
- **Artifact links** for members are signed per artifact and membership. Every request, including asset files,
  checks that the membership and project are active. A re-invite issues a fresh membership and the old links
  stay invalid. Stage 1b retires the old owner/member URL format, so pre-upgrade bookmarks need fresh links.
- **Transport:** plain `http://` is fine over Tailscale or WireGuard (already encrypted) and on a trusted LAN.
  Anywhere else use HTTPS: Tailscale `serve` (one button in Remote access), a TLS cert/key, or a reverse proxy at
  the owner's domain. The join screen warns when an invite uses plain http to a public address.
- **Tailscale guidance:** share the host with the collaborator's tailnet (Tailscale "share machine") rather than
  inviting them into the owner's whole tailnet; set Listen on to the Tailscale IP to keep the host off the LAN.
- Revocation is immediate; invites are single use and expire; tokens are hashed at rest on the host.

## 9. Later stages plug in here

- **1c per-person project chats:** `collab::personal_chat` makes or finds the `chat` thread with `user_id` for a
  member; `view_others_chats` gates the others.
- **1d presence:** `POST host/presence {thread, doing, files}` keeps an in-memory map with a 90 s TTL (`collab::Presence`);
  the digest and the lead's prompt list who is working on what; claims (`claims` table) already stop file overlap.
- **Runner gate (done in 1a):** `collab::runs_here(thread)` is false for threads with a `user_id`, and
  `runner::runnable` checks it, so a guest's thread is never run on the owner's accounts, even if the owner writes to
  it. Members' messages only wake threads this machine runs.
- **Conflict notices (planned, `collab::conflict_notice`):** when someone is about to work on something another
  person's thread is already doing (from presence and file claims), the project chat tells them (owner,
  collaborators and operators alike) and offers to send that thread a note. The note is a request: the thread's
  owner approves it, and it then arrives as a plain, visible message in the owner's own session. Never automatic
  injection into anyone's agent.
- **Stage 2 thread transfer:** the thread's branch is pushed to the shared GitHub repo by its current runner;
  `POST host/threads/{id}/transfer {to}` flips `threads.user_id` after the push; the new holder fetches the branch,
  makes a worktree, and starts from a digest (title, instructions, criteria, last messages, a summary) instead of the
  full history, which stays readable through paged `read_thread`. Members with `transfer_threads` (operators) take
  a thread at once; anyone else's request (or an agent's `request_transfer` tool call) becomes a decision for the
  current holder. The new holder's agents start on prompts that holder approved.
- **Stage 3 universal chat and operator:** a project `group` thread on the host that every member sees (each message
  carries `user_id`); `collab_rules.operator` names the operator (a user's local agent or an API agent with their own
  key). `@operator` routes to that user's runner via the bridge with the group digest; `POST host/threads/{id}/claim`
  sets `user_id` so only the claimer's agents pick it up; the operator posts the hand-off into the claimer's
  personal chat.
