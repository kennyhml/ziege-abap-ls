# ABAP Language Server

A read-only ABAP language server and repository filesystem for Ziege. The server
uses `zadt` for ADT, `zvfs` for lazy repository traversal, `zaff` for source-file
projection, and `tower-lsp-server` for LSP transport.

The daemon listens on `127.0.0.1:9257`. It retains repository trees across
client reconnects and exits ten minutes after the last client disconnects.

## Configuration

Project configuration lives in `.ziege` and is parsed by the server:

```yaml
version: 1
systems:
  S4:
    folder: SAP-DEV
    destination: DEV
    mounts:
      - kind: package
        label: Flight
        package: /DMO/FLIGHT
        facets: [GROUP, TYPE]
      - kind: systemLibrary
        label: System Library
```

User destinations live in `~/.ziegerc`. Set `ZIEGE_CONFIG` to override the
path. Passwords must come from an environment variable; other fields may use a
literal value or a corresponding `_env` field.

```toml
version = 1

[destinations.DEV]
url = "https://example.invalid"
client = "100"
language = "EN"
username_env = "DEV_USERNAME"
password_env = "DEV_PASSWORD"
```

Mounts are ordered. Omitting `mounts` creates one `System Library` mount.
Package mounts accept `package`, an optional `label`, and an optional `facets`
list. A facet can be a string or an adaptive level:

```yaml
facets:
  - GROUP
  - facet: TYPE
    minimumObjects: 10
```

Selection mounts accept `filters` with `facet`, `values`, and optional
`exclude` values.

## Protocol

The custom protocol is advertised as `capabilities.experimental.ziege` version
1 and consists of:

- `ziege/project/systems`
- `ziege/fileSystem/readDirectory`
- `ziege/fileSystem/readFile`
- `ziege/objectCreation/options`
- `ziege/objectCreation/refreshTransports`

The two object-creation discovery methods currently return `supported: false`.
No create method is registered or advertised until typed ADT object-lifecycle
and CTS transport APIs are available.

Opening a project only reads `.ziege`. The first directory request for a system
resolves `~/.ziegerc`, authenticates, performs ADT discovery, and constructs its
lazy `zvfs` tree. Supported `PROG/P`, `PROG/I`, and `CLAS/OC` object leaves are
projected as read-only AFF main-source files.

Clients can choose how AFF namespace delimiters are displayed per connection:

```json
{
  "initializationOptions": {
    "presentation": {
      "namespaceDelimiter": "slash"
    }
  }
}
```

Accepted values are `parentheses` (the default) and `slash`. This changes only
the outward filename; source resolution and shared repository contexts continue
to use canonical AFF names.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
