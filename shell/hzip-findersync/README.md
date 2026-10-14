# hzip Finder Sync extension (`hzipFinderSync.appex`)

The macOS **Finder Sync** extension for hzip. It puts a top-level `hzip` entry
(with submenu) into Finder's contextual menu — the same track as Bandizip's
`BandizipFinderSyncExtension.appex`.

Selecting an entry never runs the action itself: the extension appends one
intent line to a shared file queue and lets the (non-sandboxed) host execute it.

## Layout

| File | Purpose |
| --- | --- |
| `Sources/HZIPFinderSync.swift` | `FIFinderSync` subclass, ObjC name `HZIPFinderSync` |
| `Info.plist` | Bundle metadata + `NSExtension` (`com.apple.FinderSync`) |
| `hzipFinderSync.entitlements` | `app-sandbox` + App Group (mandatory) |
| `build.sh` | Compiles and assembles the `.appex` bundle |

## Build

```sh
# Single arch (host arch), version stamped into Info.plist
bash shell/hzip-findersync/build.sh --out build/appex --version 0.1.2

# Universal (CI packaging)
bash shell/hzip-findersync/build.sh --out build/appex --version 0.1.2 --universal

# Explicit triples
bash shell/hzip-findersync/build.sh --out build/appex \
  --arch x86_64-apple-macos11 --arch arm64-apple-macos11
```

Produces `<out>/hzipFinderSync.appex`, laid out as
`Contents/MacOS/hzipFinderSync` (Mach-O, entry point `_NSExtensionMain`) +
`Contents/Info.plist`.

`build.sh` does **not** sign. Signing belongs to the packaging layer
(`build/package/package.py`), which signs inside-out:
extension executable → appex → host executable → host app. Ad-hoc
(`codesign --sign -`) with these entitlements is enough to register and load.

## Contract with the host app

Identity (frozen project-wide):

| Item | Value |
| --- | --- |
| Host bundle id | `io.github.hzip.hzip` |
| Extension bundle id | `io.github.hzip.hzip.findersync` |
| App Group | `group.io.github.hzip.hzip` |
| Bundle name / executable | `hzipFinderSync` |
| Principal class | `HZIPFinderSync` |
| Install path | `hzip.app/Contents/PlugIns/hzipFinderSync.appex` |

A Finder Sync extension must be sandboxed, and a sandboxed process cannot read
the host's preferences or inject argv into it. The App Group container
(`~/Library/Group Containers/group.io.github.hzip.hzip/`) is therefore the
**only** channel, used in both directions:

**`menu.json` — written by the host, read here**

```json
{"items": [{"key": "extract_here", "label": "解压到当前文件夹", "scopes": ["archive_file"]}],
 "archive_exts": ["zip", "7z"],
 "folders": ["/Users/foo"]}
```

* `items[].key` — a platform-layer `VERBS` key, never translated.
* `items[].label` — already-localized text shown in the menu.
* `items[].scopes` — `archive_file` / `any` / `dir_background`.
* `archive_exts` — extensions the engine can extract (no dot, lowercase);
  drives the "is the selection an archive?" test.
* `folders` — watch folders; empty falls back to the home directory.

Scope resolution: archive-only selection → `archive_file`; any selection
(files or folders) → `any`; folder background (no selection) → `dir_background`.

The file is re-read whenever its modification date changes.

**`intents.jsonl` — appended here, drained by the host**

```json
{"key": "extract_here", "paths": ["/Users/foo/a.zip"]}
```

One JSON line per action, written with `O_APPEND` semantics on a serial
dispatch queue. The file is created if missing; the host empties it after
consuming but keeps the file (deleting it would break the host's watcher).

The extension **never** builds argv — it emits only `key` + selected paths.
The host expands keys via `hzip_platform::context_menu::entry_for(key)`.

After queueing, the extension best-effort activates the host via
`NSWorkspace.openApplication(at:configuration:)` with **no arguments** (argv
injection is discarded for sandboxed extensions). A failed activation is not
fatal: the intent is already durable on disk and the host consumes it on next
launch.

Debug logging uses the `HZIPFS` prefix (`log stream --predicate 'eventMessage CONTAINS "HZIPFS"'`).
