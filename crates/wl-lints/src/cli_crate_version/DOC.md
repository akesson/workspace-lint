# cli-crate-version

Verifies that a locally installed CLI tool's version matches the version of
a crate in the workspace — useful when a build step depends on a companion
binary (e.g. `wasm-bindgen`) whose version must track the library.

A policy lint: it does nothing until you give it at least one rule.

## What it checks

For each rule, the `command` is run, its output is matched against
`pattern` (a regex with one capture group for the version), and the captured
version is compared against the version of the named `crate` in the
workspace. A mismatch is reported.

## Configuration

One `[[cli-crate-version.rules]]` table per tool. Presence of the table
enables the lint.

```toml
[[cli-crate-version.rules]]
command = ["wasm-bindgen", "--version"]
pattern = "wasm-bindgen (\\S+)"
crate = "wasm-bindgen"
```

- `command` — the argv to run to print the tool's version.
- `pattern` — a regex whose first capture group is the version.
- `crate` — the workspace crate whose version must match.

Ad-hoc (no config) form:

```sh
workspace-lint check cli-crate-version \
    --command "wasm-bindgen --version" \
    --pattern "wasm-bindgen (\S+)" \
    --crate-name wasm-bindgen
```

## Silencing

Change the level (or turn it off) through `[lints]`:

```toml
[lints]
cli-crate-version = "allow"
```

Prefer `expect` over the permanent `allow` where a directive can anchor.
