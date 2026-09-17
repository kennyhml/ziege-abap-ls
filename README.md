# ziege-abap-ls

A language server implementation for the ABAP programming language based on
the ziege tooling library.

## Background

Several ABAP language servers already exist, including SAP's official server.
This project is a way to explore a different design, without depending on Eclipse
or closed source tooling. I started it before the official language server was announced,
paused it while I was still using VSCode in favor of the official extension, then decided 
to pick it back up after switching to Neovim.

ABAP development needs repository browsing, remote files, locks and activation. 
Much of this falls outside the LSP specification, so supporting editors like Neovim 
and Zed takes more than standard language features.

There is also room to do more locally. Caching, parsing and indexing could reduce
requests to ADT and support analysis beyond the available ADT calls.

## Running & Connecting

Run the server with
```sh
ziege-abap-ls           
```
Use `--help` for more usage information.

The server listens on `ZIEGE_LSP_ADDRESS` (`127.0.0.1:9257` by default). It supports multi-client
connection based on client connection behavior. That means a client may connect to an existing
lanuage server or spawn a dedicated process. The server will not shut down while any clients
are connected to it. This allows different clients to benefit from shared caching layers.

You can also pass a `--daemon` flag to the launch arguments. The server will then stay alive without
any active connections for up to `ZIEGE_IDLE_TIMEOUT_SECONDS` seconds. The default is 10 minutes.

## Configuration

Configuration files use TOML and currently require `version = 1`. There are multiple levels of configuration.

### Project-local

A local `ziege.toml` selects destinations to use in the current project and their repository mounts. 

```toml
version = 1

[systems.DEV]
folder = "SAP-DEV"

[[systems.DEV.mounts]]
kind = "package"
label = "Flight"
package = "/DMO/FLIGHT"
```

### User

`~/.config/ziege/config.toml` holds user preferences. An absolute `XDG_CONFIG_HOME` replaces `~/.config`, 
and `ZIEGE_CONFIG` overrides the full file path. This is currently not used.

### Destinations

`destinations.toml` lives beside the user configuration and defines connections
shared across projects. Values are literal, including credentials.

```toml
version = 1

[destinations.DEV]
url = "https://example.invalid"
client = "100"
language = "EN"
username = "DEVELOPER"
password = "example-password"
```

`language` defaults to `EN`. The other connection fields are required.

## Protocol

Protocol version **1** must currently be used exclusively.

The `initialize` request supplies one `workspaceFolders` entry with a local file
URI such as `file:///home/user/dev/my%20project`. Older clients may use `rootUri`
instead. The server decodes and canonicalizes this once into a
[`ProjectRoot`](src/config/project.rs). Each worker stays bound to that root for
its connection. Multiple projects use separate connections to the same daemon.

`ziege/project/systems` can be called to get the system portals to display in the editor.

```json
{ "systems": [{ "folder": "SAP-DEV", "uri": "abap://DEV/vfs/" }] }
```

Opening a portal follows the returned root URI. The client does not need a separate
system alias or destination field.

`ziege/fileSystem/readDirectory` takes a URI and an optional refresh flag:

```json
{
  "uri": "abap://DEV/vfs/Flight/Classes/",
  "refresh": false
}
```

The response contains `entries` with `uri`, `name`, and `kind` (`folder`,
`package`, or `object`). Open the returned `uri` when entering a directory.

### Resource URIs

```text
abap://DEV/vfs/Flight/Classes/
abap://DEV/zcl_example.clas.abap
abap://DEV/zcl_example.clas.json
```

The authority is the actual destination ID. A `/vfs/` path addresses the project view.
A single file name under the destination addresses a projected document without
depending on the mounts used to find it. 

