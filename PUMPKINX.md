# PumpkinX

PumpkinX is a fork of [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) (Minecraft Java 26.3) with three things built in:

| Feature | What it does | Default |
| --- | --- | --- |
| Multiversion | Admits Java clients older than 26.3 and translates their packets, using the translator from [pumpkin-java-multiversion](https://github.com/Pumpkin-MC/pumpkin-java-multiversion) natively inside the server. Only some versions can finish joining so far, see the status below. | on |
| PatchBukkit | Runs Bukkit, Spigot and Paper plugins through [PatchBukkit](https://github.com/Pumpkin-MC/PatchBukkit). Linked into the binary, started only when switched on. Needs Java 25+. | off |
| Keystone | The all-in-one plugin (economy, shop, auctions, roles, sidebar, login, skins), embedded in the binary as a Wasm plugin. | on |
| Web dashboard | Status, live console, players, every `pumpkin.toml` setting and the PumpkinX toggles, in the browser. | on, port 8080 |

## Layout

```
crates/pumpkinx/          PumpkinX layer: config, multiversion glue, PatchBukkit toggle, dashboard
crates/pumpkinx/assets/   Dashboard page (HTML, CSS, JS), embedded in the binary
crates/pumpkin-core/src/pumpkinx.rs   Small hooks in the core (protocol gate, chunk writer, log tap)
builtin/multiversion/     Vendored pumpkin-java-multiversion (see builtin/UPSTREAM_multiversion)
builtin/patchbukkit/      Vendored PatchBukkit (see builtin/UPSTREAM_patchbukkit)
builtin/keystone/         Copy of the Keystone Pumpkin plugin (see builtin/UPSTREAM_keystone)
build.sh                  Linux build; --install deploys to /opt/pumpkinx and restarts PumpkinXTest
build.ps1                 Windows build
```

Changes to upstream Pumpkin files are kept small so upstream merges stay easy:

- `net/java/handshake.rs`, `server/connection_cache.rs`: ask `pumpkinx::accepts_protocol` before rejecting an older client.
- `net/java/mod.rs`, `net/chunk_sender.rs`: chunk packets for older clients go through the registered chunk writer.
- `plugin/loader/mod.rs`, `plugin/mod.rs`: `BuiltinLoader` and `PluginManager::load_builtin_plugin` for plugins compiled into the binary.
- `lib.rs`: the log tap layer that feeds the dashboard console.
- `pumpkin-api-macros`: `#[plugin_method]` no longer calls `block_on` just to box a future, which panicked once a plugin shares the host's tokio.
- `crates/pumpkin/src/main.rs`: loads `pumpkinx.toml` and starts the PumpkinX layer.

## Explosions

PumpkinX explosions are much cheaper than upstream's, with the same results as vanilla. Measured with 147 TNT in dirt, the total explosion time went from about 7900 ms to 620 ms, and with 600 stacked TNT from about 1500 ms to 310 ms.

- Drops merge into stacks of up to 16 and spawn after all blocks are gone, like vanilla's `ServerExplosion`. Upstream spawned an entity per block.
- Ray directions are computed once, and each block is inspected once per explosion in a flat grid.
- Exposure of entities is reused within a tick for an identical explosion center and entity box, as long as no block changed.

The server also identifies itself as `PumpkinX` in the F3 screen.

### Chain explosions

When an explosion hits a cluster of at least 64 TNT blocks, the cluster no longer turns into one entity per block. PumpkinX walks through the TNT and removes it, then sends a single pressure wave outward from all of it. The wave loses energy per block, so soft blocks go far and obsidian stops it. A blast uses at most 10 ms of each tick, so the crater grows over a few seconds while the server keeps running.

A 25x25x25 TNT cube in dirt took about 190 seconds with a peak of 1782 mspt the vanilla way, and 4 seconds with a peak of 11 mspt this way (one Windows machine, one bot connected).

This is not vanilla behavior. Craters have a different shape, fewer blocks drop items, and everything nearby takes one hit instead of many. Plugins see one block explode event for the explosion that started the chain, not one per block. A blast in progress is not saved: if the server stops halfway, the TNT that was not reached stays in the world as blocks.

Change it in `pumpkinx.toml`:

```toml
[explosions]
merge_chains = true   # false restores the vanilla chain reaction
min_tnt = 64          # smallest cluster that gets merged
tick_budget_ms = 10   # share of each 50 ms tick a blast may use
```

## Building

Needs Rust stable, and for PatchBukkit a JDK 25 (Gradle builds the Java bridge).

```bash
./build.sh                  # release build with PatchBukkit
./build.sh --no-patchbukkit # leave PatchBukkit out of the binary
./build.sh --install        # build, copy to /opt/pumpkinx/pumpkinx, restart the pumpkinxtest service
```

On Windows, with the Visual Studio C++ Build Tools installed:

```powershell
.uild.ps1                 # release build with PatchBukkit, gives target
elease\pumpkin.exe
.uild.ps1 -Dev            # faster unoptimized build
.uild.ps1 -NoPatchBukkit  # leave PatchBukkit out of the binary
```

## Configuration

`pumpkinx.toml` sits next to `pumpkin.toml` and is created on first start:

```toml
[dashboard]
enabled = true
address = "0.0.0.0:8080"
token = "one-time setup token, gone after the first account"

[multiversion]
enabled = true

[patchbukkit]
enabled = false

[keystone]
enabled = true
```

Sign in with a username and password. The token is only a one-time setup token: use it to create the first account, after which it is removed. Add more accounts on the PumpkinX features page. Everyone with an account has full control, including console commands and file uploads.

Passwords are stored as Argon2 hashes. After 5 failed logins an address is blocked for 5 minutes, and after 20 failures a username is. Sessions last 7 days but end when the server restarts.

PatchBukkit reads plugin jars from `plugins/data/patchbukkit/patchbukkit-plugins/`.

## Multiversion status

Tested with offline-mode bots (`node-minecraft-protocol`):

| Client | Result |
| --- | --- |
| 26.1, 1.21.11 | Join and spawn; 1.21.11 chat reached the server |
| 1.21.4, 1.21.1, 1.20.6 | Join and receive chunks, then get kicked: the server can't decode a play packet they send (id 6 or 7, 39 bytes). 1.21.4 also misses the command tree packet. |
| 1.20.4, 1.20.2 | Time out in configuration (cause not confirmed, registry data format is the suspect) |
| 1.20.1, 1.19.4 | Time out right after login (these versions have no configuration phase) |
| 1.18.2, 1.16.5, 1.12.2, 1.8.8 | Kicked during login (the server can't read their login packet) |

Not tested: 26.2, 1.7.x, and a real 26.3 client (the bot library stops at 26.1). No test used the real Minecraft game client.

Upstream's Wasm plugin API has no login, configuration or chunk data access, and stock Pumpkin's handshake rejects anything below 26.3. Older versions need payload translation for login and configuration, which is the next piece of work.

## Known issues

- On Windows, the very first PatchBukkit boot stalled for about 4.5 minutes before Paper started. The cause is unknown. Later boots took 14 to 22 seconds.
- A dashboard stop hung once at "Starting save." during a heavy build, and systemd had to restart the service. The cause is unknown. It did not happen again on the release build.

## Platform support

PumpkinX builds and runs on Linux and Windows. Use `build.sh` on Linux and `build.ps1` on Windows.

## File manager

The Files page browses, edits, uploads, downloads, renames and deletes inside the server folder only. Paths are resolved through the real filesystem, so `..` and symbolic links can't leave it.

- `pumpkinx.toml` and `nethernet-key.der` are hidden and refused, because they hold secrets.
- `pumpkin.toml` is read only here, so changes keep going through the validation on the settings page.
- Deleted files go to `.pumpkinx-trash`. Deleting inside that folder is permanent.
- Uploads have no size limit: the dashboard sends files in 32 MB pieces, which also gets them past Cloudflare's 100 MB request limit. The editor opens text up to 2 MB, and folders can't be downloaded yet.
- Saving a file that someone else changed in the meantime is refused.
- Every change is written to the server log.

Every account can upload a plugin, which runs as code on the machine. On Linux the service is sandboxed to limit that: it can only write to its own server folder. Turn the file manager off with `file_manager = false` under `[dashboard]` in `pumpkinx.toml`.

## Remote access

The dashboard is published through a Cloudflare tunnel by the `pumpkinx-tunnel` systemd service. It is a quick tunnel, so the address changes whenever the service restarts:

```bash
sudo journalctl -u pumpkinx-tunnel | grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' | tail -1
```

For a fixed address, use a named tunnel on a domain in your Cloudflare account, ideally with Cloudflare Access in front. The console polls `/api/logs` because quick tunnels don't pass server-sent events. The token only travels in the `Authorization` header, never in a URL.

## Third-party code

PumpkinX as a whole is distributed under GPL-3.0, the license of Pumpkin. It contains:

| Part | Source | License |
| --- | --- | --- |
| Pumpkin | [Pumpkin-MC/Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) | GPL-3.0 |
| `builtin/patchbukkit` | [Pumpkin-MC/PatchBukkit](https://github.com/Pumpkin-MC/PatchBukkit) | GPL-3.0 |
| `builtin/multiversion` | [Pumpkin-MC/pumpkin-java-multiversion](https://github.com/Pumpkin-MC/pumpkin-java-multiversion) | MIT or Apache-2.0, with mapping data from the ViaVersion projects (see `builtin/multiversion/assets/NOTICE.md`) |
| `builtin/keystone` | Keystone by JustMrKoala | GPL-3.0-or-later |

PumpkinX is an independent fork and is not an official Pumpkin project. Please report PumpkinX problems here, not to the Pumpkin team.
