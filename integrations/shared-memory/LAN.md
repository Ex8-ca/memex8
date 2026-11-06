# One memex8 server across your LAN

Run the memory service once on `192.168.1.3`; run the small stdio MCP bridge on
each computer using Claude Code or Codex. Hermes's existing memory plugin connects
directly to the same REST service. All clients use the same server API key.

```text
Desktop: Claude Code + bridge ─┐
Laptop:  Codex + bridge ───────┼── http://192.168.1.3:8080 ── memex8 / Qdrant
Any PC:  Hermes plugin ───────┘
```

Port `8080` is assumed from the repository defaults; substitute the actual port
if your existing deployment uses another one. No direct Qdrant access or separate
MCP network listener is needed on the clients.

## Server

1. Update/rebuild the existing memex8 deployment with this revision's tag
   persistence change. Keep its existing Qdrant data, embeddings configuration,
   and volumes. The new bridge does not migrate or copy your memory database.
2. Ensure the server has `MEMEX8_API_KEY` configured. Clients must use that key.
3. The REST service must listen on the LAN interface (or `0.0.0.0`) on port 8080.
   The repository's default server config and Docker port mapping already use
   this port. Permit access from the intended LAN clients in the host firewall.

The bridge uses `/api/v1` with Bearer authentication. The existing web UI, legacy
MCP, and webhook routes have different authentication behavior; this change does
not harden those routes or make the whole service suitable for public exposure.

## Claude Code and Codex computers

Copy `bridge.py` to each computer (or keep a checkout of this repository). Install
Python 3.10 or later. No Python packages or local Qdrant server are required.

Provide `MEMEX8_API_KEY` securely in the environment that launches the agent.
Use the **same project ID** on every computer, for example `Ex8-ca/memex8`.
It is independent of the local checkout directory or worktree path.

The following commands use a private LAN HTTP connection explicitly. HTTP sends
the API key and memory content in plaintext on that connection. For encryption,
use a trusted HTTPS endpoint and omit `--allow-lan-http`, or an SSH tunnel to the
server and a loopback base URL. The HTTP flag only accepts numeric RFC1918 IPv4
or IPv6 ULA addresses; it does not permit public IPs or hostnames. Local/LAN HTTP
also bypasses environment proxies.

### Claude Code

Run from the project directory on each computer:

```sh
claude mcp add --transport stdio --scope local memex8 -- \
  python3 /absolute/local/path/to/bridge.py \
  --agent claude-code --project Ex8-ca/memex8 \
  --base-url http://192.168.1.3:8080 --allow-lan-http
```

Optionally append `--vault /absolute/local/path/to/ObsidianVault`. The agent
process must inherit `MEMEX8_API_KEY`. Restart Claude Code after configuration.

### Codex

Merge into the Codex configuration on each computer, adjusting the script path:

```toml
[mcp_servers.memex8]
command = "python3"
args = ["/absolute/local/path/to/bridge.py", "--agent", "codex", "--project", "Ex8-ca/memex8", "--base-url", "http://192.168.1.3:8080", "--allow-lan-http"]
env_vars = ["MEMEX8_API_KEY"]
```

Optionally append `"--vault", "/absolute/local/path/to/ObsidianVault"` to `args`.
Restart Codex after configuration. On Windows, use the local Python executable
and local script/vault paths; the bridge itself uses portable Python APIs, but
this revision's tests were run on Linux.

The hostname becomes the `device:` tag on new memories. Add `--device laptop` or
`--device desktop` to override it. Device tags record origin; they do not restrict
which of your other computers can recall a memory.

## Hermes computers

Keep the existing memex8 memory provider enabled. Set these values in that Hermes
installation's `.env` (normally `~/.hermes/.env`):

```dotenv
MEMEX8_BASE_URL=http://192.168.1.3:8080
MEMEX8_API_KEY=the-existing-server-key
```

Restart Hermes. Its plugin already supports a remote base URL and does not need
the bridge's LAN flag. It retains its existing automatic recall and capture.
Hermes-originated records retain the existing plugin provenance format; only
bridge-originated writes get the new `device:` tag.

The bridge defaults to project-scoped recall. To find existing untagged Hermes
memories, use `scope="all"` in `memex8_search` and `memex8_get`. Hermes can recall
the bridge's project memories using its existing search. Sharing among your
authenticated clients does not require setting memories to A2A `public`.

## Obsidian across computers

Configure each bridge's `--vault` to point to the same logical vault, available
locally through your existing sync process or mounted shared directory. No sync
service is installed by this integration.

For example, these can refer to the same note:

```text
Desktop vault: /home/marc/Documents/Obsidian/Obsidian
Laptop vault:  /home/marc/Notes/Obsidian
Stored path:  memex8/memex8.md
```

`obsidian_get(path="memex8/memex8.md")` resolves against the current computer's
vault. The tool returns that local copy's current content; how fresh it is depends
on your sync or shared filesystem. Use matching vault names in Obsidian if you
also want the stored `obsidian://` links to open directly on each computer.
Different vault names do not prevent lookup through `obsidian_get` by relative
path. The bridge does not translate Windows backslashes in stored note paths;
use the forward-slash paths returned by `obsidian_search` on every platform.

If a computer has no vault copy/mount, omit `--vault`: it can still recall stored
memories and their references, but cannot retrieve the current note contents.

## Read-only connection check

With the server API key already in your environment:

```sh
python3 /absolute/local/path/to/bridge.py \
  --agent codex --project Ex8-ca/memex8 \
  --base-url http://192.168.1.3:8080 --allow-lan-http --check
```

This requests one memory through the authenticated list endpoint and prints only
connection metadata. It does not print memory contents, write memories, or prove
that the running server has the tag-persistence update.

After deployment, verify the full path with a short identifiable test memory:
save it from Claude Code on one computer, recall it from Codex on another, then
search for it in Hermes. For Obsidian, read a referenced note from both computers
and confirm that your sync/shared filesystem presents the expected version.

When the server is unreachable, memory calls report failure. This bridge does
not queue offline writes or fall back to a separate local database.
