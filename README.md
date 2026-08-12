# Herdr JJ

Jujutsu workspace support for [Herdr](https://herdr.dev). Create, open, remove,
and inspect JJ workspaces with Herdr spaces.

## Features

- Fetch Git remotes and create a JJ workspace from `trunk()`.
- Automatic bookmark creation on workspace creation.
- Open an existing JJ workspace or focus its existing Herdr workspace.
- Remove a workspace when closing a Herdr space.
- Run a configured hook in the new workspace after creation for setup.
- Show JJ change and status values in the Herdr Spaces sidebar.

## Requirements

- Herdr 0.8.0 or newer
- Jujutsu 0.39.0 or newer
- Rust and Cargo during plugin installation
- macOS or Linux

## Install

Install from GitHub:

```sh
herdr plugin install olivergilan/herdr-jj
```

For local development:

```sh
cargo build --release --locked
herdr plugin link .
```

Check the registration:

```sh
herdr plugin action list --plugin olivergilan.herdr-jj
```

## Keybindings

Add these commands to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+shift+a"
type = "plugin_action"
command = "olivergilan.herdr-jj.create"
description = "new JJ workspace"

[[keys.command]]
key = "prefix+a"
type = "plugin_action"
command = "olivergilan.herdr-jj.open"
description = "open JJ workspace"

[[keys.command]]
key = "prefix+d"
type = "plugin_action"
command = "olivergilan.herdr-jj.remove"
description = "remove JJ workspace"
```

Reload the running server after a config change:

```sh
herdr server reload-config
```

## Plugin Config

Get the plugin config directory:

```sh
herdr plugin config-dir olivergilan.herdr-jj
```

Create `config.toml` in that directory. Every field is optional:

```toml
workspace_root = "~/.herdr/jj-workspaces"
create_bookmark = false
post_create = "mise install && npm install"
status_remote = "origin"
```

`workspace_root` must be absolute or start with `~/`. New checkouts use this
layout:

```text
<workspace_root>/<main-repository-name>/<workspace-name-slug>
```

`post_create` is submitted to the new workspace's root shell after Herdr opens
the workspace. A setup failure leaves the workspace open for inspection.

## Sidebar Status

Add the plugin tokens to the Spaces layout:

```toml
[ui.sidebar.spaces]
rows = [
  ["state_icon", "workspace"],
  ["$jj_change", "$jj_status"],
]
```

`$jj_change` shows local bookmarks or the short change ID. `$jj_status` uses:

- `!` for a conflict
- `+N` and `-N` for remote distance
- `*N` for changed files in `@`

Status refreshes at startup, when a workspace opens, and when focus changes.
Run the refresh action after a JJ command when you need an immediate update:

```sh
herdr plugin action invoke olivergilan.herdr-jj.refresh-status
```

## Lifecycle

Creation fetches Git remotes, creates a new JJ working-copy change on `trunk()`,
then opens the checkout in Herdr. If fetching fails, it uses the locally known
`trunk()`. If Herdr creation fails, the plugin forgets the new JJ workspace and
removes only the files created by that operation.

After confirmation, removal snapshots the working copy, moves the checkout to a
temporary sibling path, and forgets the JJ workspace. If `jj workspace forget`
fails, the plugin moves the checkout back. Existing bookmarks remain unchanged.
The temporary checkout is deleted in the background so the Herdr workspace can
close immediately.

The main JJ workspace cannot be removed through the plugin.

## Current Herdr Limit

Herdr plugin v1 can create normal Herdr workspaces. It cannot attach custom
workspace provenance, group JJ workspaces under a parent row, or add native
sidebar context-menu actions. These features need a small Herdr host extension.

## Development

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
cargo build --release --locked
```

Tests use temporary real JJ repositories and a fake Herdr executable. They do
not modify active project workspaces.

## License

MIT
