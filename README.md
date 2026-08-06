# ABAP Language Server
A language server for the ABAP programming language, largely based on [ziege](https://github.com/kennyhml/ziege) tooling

> "But language servers for ABAP already exist, including a SAP language server!"

Correct, currently the idea is not for this language server to compete with the adt-ls, but to embrace it. In the literal sense, in the form of a wrapper.
This approach gives us the best of both worlds: a fully open source language server, tooling to enhance it with, and the more battle tested official
language tooling to fall back on for functionality that is difficult to support.

Nevertheless, in the long run, the goal is to bring more language server capability into the project natively, for several reasons:
1. Official tooling is closed source and, at least in my opinion, shows signs of entropy. Headless Eclipse is not a viable long-term.
2. Current implementations just proxy the ADT backend to an editor. LS capabilities should run much more locally where possible.
3. ADT-LS can not be fully implemented with the current scope of the language server protocol. This makes it a pain to integrate with certain editors, such as Neovim, or Zed, due to its closed source nature.

> [!WARNING]
> All of the below is unstable and experimental.

The language server currently runs as a daemon and listens on `127.0.0.1:9257`, it only shuts down after
a set timeout of connection inactivity.

## Configuration
Project configuration lives locally in a `.ziege` and is parsed by the server:

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

User destinations live in `~/.ziegerc`. 
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
