// HZIPFinderSync.swift
//
// hzip Finder Sync extension (`.appex`).
//
// This extension is the "top-level Finder context menu" track: it inserts a
// top-level `hzip` entry (with submenu) into Finder's contextual menu.
//
// Hard-won platform facts this code relies on (verified on macOS 15.x):
//   * A FinderSync `.appex` MUST be sandboxed, otherwise `pluginkit`/`pkd`
//     rejects it ("plug-ins must be sandboxed").
//   * A sandboxed extension can only exchange data with the (non-sandboxed)
//     host through its App Group container:
//       FileManager.default.containerURL(forSecurityApplicationGroupIdentifier:)
//   * A sandboxed extension cannot inject argv into the host; there is no
//     reliable argv / URL-scheme channel.  Action dispatch is therefore a
//     *file queue*: the extension appends one intent line per user action and
//     the host drains the queue.
//
// Contract (see `local://macos-ctxmenu-contract.md`):
//   * group container : ~/Library/Group Containers/group.io.github.hzip.hzip/
//   * menu.json       : written by the host, read here (menu items + folders).
//   * intents.jsonl   : appended here, drained by the host.
//   * This side NEVER builds argv; it only emits `key` + selected paths.

import Cocoa
import FinderSync

private let kAppGroupIdentifier = "group.io.github.hzip.hzip"
private let kMenuFileName = "menu.json"
private let kIntentsFileName = "intents.jsonl"
private let kLogPrefix = "HZIPFS"
private let kMenuTitle = "hzip"

// MARK: - menu.json model

/// One host-configured menu entry.
private struct MenuItemSpec {
    let key: String
    let label: String
    let scopes: Set<String>
}

/// Menu scopes as defined by the platform layer (`MenuScope::as_str()`).
private enum MenuScope {
    static let archiveFile = "archive_file"
    static let any = "any"
    static let dirBackground = "dir_background"
}

// MARK: - Extension principal class

/// Finder Sync extension principal class (ObjC name must match
/// `NSExtensionPrincipalClass` in Info.plist).
@objc(HZIPFinderSync)
final class HZIPFinderSync: FIFinderSync {

    private var menuItems: [MenuItemSpec] = []
    private var archiveExtensions: Set<String> = []
    private var lastMenuModificationDate: Date?

    /// Serial queue guarding `intents.jsonl` appends so concurrent actions
    /// cannot interleave partial lines.
    private let intentQueue = DispatchQueue(label: "io.github.hzip.hzip.findersync.intents")

    // MARK: Lifecycle

    override init() {
        super.init()

        let folders = loadConfiguration()
        applyDirectoryURLs(folders: folders)
        lastMenuModificationDate = menuFileModificationDate()

        NSLog("%@ initialized; container=%@ items=%d folders=%d",
              kLogPrefix, containerURL().path, menuItems.count, folders.count)
    }

    // MARK: Configuration

    /// The App Group container; the only channel shared with the host.
    /// Falls back to the on-disk location if the group is unavailable
    /// (e.g. unsigned development builds).
    private func containerURL() -> URL {
        if let url = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: kAppGroupIdentifier) {
            return url
        }
        return URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
            .appendingPathComponent("Library/Group Containers/\(kAppGroupIdentifier)", isDirectory: true)
    }

    private func menuFileURL() -> URL {
        return containerURL().appendingPathComponent(kMenuFileName, isDirectory: false)
    }

    private func intentsFileURL() -> URL {
        return containerURL().appendingPathComponent(kIntentsFileName, isDirectory: false)
    }

    private func menuFileModificationDate() -> Date? {
        let url = menuFileURL()
        return (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?
            .contentModificationDate
    }

    /// Re-read `menu.json` when the host rewrote it (mtime changed).
    private func reloadIfNeeded() {
        let current = menuFileModificationDate()
        guard current != lastMenuModificationDate else { return }
        lastMenuModificationDate = current

        let folders = loadConfiguration()
        applyDirectoryURLs(folders: folders)
        NSLog("%@ menu.json reloaded; items=%d folders=%d",
              kLogPrefix, menuItems.count, folders.count)
    }

    /// Parse `menu.json`; returns the configured watch folders.
    /// Tolerant: a missing or malformed file degrades to "no menu items".
    private func loadConfiguration() -> [String] {
        let url = menuFileURL()
        guard let data = try? Data(contentsOf: url) else {
            NSLog("%@ menu.json not readable at %@", kLogPrefix, url.path)
            return []
        }
        guard let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            NSLog("%@ menu.json is not a JSON object at %@", kLogPrefix, url.path)
            return []
        }

        if let rawItems = root["items"] as? [[String: Any]] {
            menuItems = rawItems.compactMap { dict in
                guard let key = dict["key"] as? String, !key.isEmpty else { return nil }
                let label = (dict["label"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? key
                let scopes = Set((dict["scopes"] as? [String]) ?? [])
                return MenuItemSpec(key: key, label: label, scopes: scopes)
            }
        } else {
            menuItems = []
        }

        if let rawExtensions = root["archive_exts"] as? [String] {
            archiveExtensions = Set(rawExtensions.map {
                $0.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "."))
            })
        }

        return (root["folders"] as? [String]) ?? []
    }

    /// Point Finder at the configured folders (default: the home directory).
    private func applyDirectoryURLs(folders: [String]) {
        var urls: Set<URL> = []
        for folder in folders {
            let expanded = (folder as NSString).expandingTildeInPath
            var isDirectory: ObjCBool = false
            guard FileManager.default.fileExists(atPath: expanded, isDirectory: &isDirectory),
                  isDirectory.boolValue else {
                NSLog("%@ skipping non-existent watch folder %@", kLogPrefix, expanded)
                continue
            }
            urls.insert(URL(fileURLWithPath: expanded, isDirectory: true))
        }
        if urls.isEmpty {
            urls.insert(URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true))
        }
        FIFinderSyncController.default().directoryURLs = urls
    }

    // MARK: Menu

    override func menu(for menuKind: FIMenuKind) -> NSMenu? {
        reloadIfNeeded()

        let controller = FIFinderSyncController.default()
        let selected = controller.selectedItemURLs() ?? []

        var applicableScopes: Set<String> = []
        if selected.isEmpty {
            // Right-click on a folder background: no selection.
            applicableScopes.insert(MenuScope.dirBackground)
        } else {
            applicableScopes.insert(MenuScope.any)
            if allSelectedAreArchiveFiles(selected) {
                applicableScopes.insert(MenuScope.archiveFile)
            }
        }

        let visible = menuItems.filter { !$0.scopes.isDisjoint(with: applicableScopes) }
        guard !visible.isEmpty else {
            NSLog("%@ no menu items for scopes=%@", kLogPrefix,
                  applicableScopes.sorted().joined(separator: ","))
            return nil
        }

        let menu = NSMenu(title: "")
        let topItem = NSMenuItem(title: kMenuTitle, action: nil, keyEquivalent: "")
        let submenu = NSMenu(title: kMenuTitle)
        submenu.autoenablesItems = false

        for spec in visible {
            let entry = NSMenuItem(title: spec.label,
                                   action: #selector(handleAction(_:)),
                                   keyEquivalent: "")
            entry.target = self
            entry.representedObject = spec.key
            submenu.addItem(entry)
        }

        topItem.submenu = submenu
        menu.addItem(topItem)
        return menu
    }

    /// True when every selected item is a regular file whose extension the
    /// engine can extract.
    private func allSelectedAreArchiveFiles(_ urls: [URL]) -> Bool {
        guard !urls.isEmpty, !archiveExtensions.isEmpty else { return false }
        for url in urls {
            guard archiveExtensions.contains(url.pathExtension.lowercased()) else { return false }
            if let values = try? url.resourceValues(forKeys: [.isRegularFileKey]),
               values.isRegularFile == false {
                return false
            }
        }
        return true
    }

    // MARK: Action dispatch

    @objc func handleAction(_ sender: NSMenuItem) {
        guard let key = sender.representedObject as? String, !key.isEmpty else {
            NSLog("%@ ignoring action without a verb key", kLogPrefix)
            return
        }

        let controller = FIFinderSyncController.default()
        var paths = (controller.selectedItemURLs() ?? []).map { $0.path }
        if paths.isEmpty, let targeted = controller.targetedURL() {
            paths = [targeted.path]
        }
        guard !paths.isEmpty else {
            NSLog("%@ ignoring %@: no selected paths", kLogPrefix, key)
            return
        }

        enqueueIntent(key: key, paths: paths)
        activateHost()
    }

    /// Append `{"key": ..., "paths": [...]}` as one line to `intents.jsonl`.
    /// Serialized on `intentQueue` so concurrent actions never interleave.
    ///
    /// This is the extension's single durable-output primitive, kept
    /// selection-independent so the queue contract can be exercised without a
    /// live Finder session.
    func enqueueIntent(key: String, paths: [String]) {
        intentQueue.sync {
            let url = intentsFileURL()
            let fileManager = FileManager.default
            try? fileManager.createDirectory(at: url.deletingLastPathComponent(),
                                             withIntermediateDirectories: true)

            guard let data = try? JSONSerialization.data(
                withJSONObject: ["key": key, "paths": paths]
            ) else {
                NSLog("%@ failed to encode intent for %@", kLogPrefix, key)
                return
            }

            if !fileManager.fileExists(atPath: url.path) {
                fileManager.createFile(atPath: url.path, contents: nil)
            }
            guard let handle = FileHandle(forWritingAtPath: url.path) else {
                NSLog("%@ cannot open %@ for appending", kLogPrefix, url.path)
                return
            }
            defer { try? handle.close() }

            do {
                try handle.seekToEnd()
                try handle.write(contentsOf: data)
                try handle.write(contentsOf: Data([0x0A]))
                NSLog("%@ queued key=%@ paths=%d", kLogPrefix, key, paths.count)
            } catch {
                NSLog("%@ failed writing intent for %@: %@", kLogPrefix, key, "\(error)")
            }
        }
    }

    /// Best-effort activation of the host app.  The intent is already durable
    /// on disk, so a failure here only delays processing.
    ///
    /// argv injection is impossible for a sandboxed extension, so this call
    /// carries no arguments — it merely brings the host forward.
    private func activateHost() {
        let hostURL = hostApplicationURL()
        guard let host = hostURL, host.pathExtension == "app" else {
            NSLog("%@ host app not found next to %@", kLogPrefix, Bundle.main.bundleURL.path)
            return
        }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = true
        NSWorkspace.shared.openApplication(at: host, configuration: configuration) { _, error in
            if let error = error {
                NSLog("%@ host activation failed (%@): %@", kLogPrefix, host.path, "\(error)")
            }
        }
    }

    /// `.../hzip.app/Contents/PlugIns/hzipFinderSync.appex` -> `.../hzip.app`
    private func hostApplicationURL() -> URL? {
        let bundleURL = Bundle.main.bundleURL
        guard bundleURL.pathExtension == "appex" else { return nil }
        return bundleURL
            .deletingLastPathComponent()   // PlugIns
            .deletingLastPathComponent()   // Contents
            .deletingLastPathComponent()   // hzip.app
    }
}
