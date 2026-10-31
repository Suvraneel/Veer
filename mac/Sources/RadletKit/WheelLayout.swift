import Foundation

/// Fits a list of slots onto the wheel. When there are more than fit, the
/// last segment becomes "More…", which opens a menu of the rest.
public enum WheelLayout {
    public static let moreID = "more"

    public static let moreSlot = Slot(id: moreID, label: "More…", symbol: "ellipsis.circle", group: "tool",
                                      action: .object(["kind": .string("more")]))

    public static func fit(_ slots: [Slot], max: Int) -> (visible: [Slot], overflow: [Slot]) {
        guard max > 1, slots.count > max else { return (Array(slots.prefix(Swift.max(max, 0))), []) }
        let shown = Array(slots.prefix(max - 1))
        return (shown + [moreSlot], Array(slots.dropFirst(max - 1)))
    }

    /// Like `fit`, but the slots used most are the ones that stay on the
    /// wheel. They keep the engine's order (most useful first), so a slot
    /// never moves just because another got used more; the rest go behind
    /// "More…", most used first.
    ///
    /// Slots marked `more` (conversions to formats fewer people want) wait
    /// behind "More…" even when there's room, unless there's only one of
    /// them: "More…" would take its place anyway.
    public static func fit(_ slots: [Slot], max: Int, usage: [String: Int]) -> (visible: [Slot], overflow: [Slot]) {
        let rare = slots.indices.filter { slots[$0].more }
        let popular = slots.indices.filter { !slots[$0].more }
        guard max > 1, rare.count >= 2, !popular.isEmpty else { return byUse(slots, max: max, usage: usage) }
        let uses: (Int) -> Int = { usage[slots[$0].id] ?? 0 }
        let ranked = popular.sorted { a, b in uses(a) != uses(b) ? uses(a) > uses(b) : a < b }
        let keep = Set(ranked.prefix(max - 1))
        let shown = popular.filter { keep.contains($0) }.map { slots[$0] }
        // Most used first; on a tie, popular ones before the rest.
        let rest = (Array(ranked.dropFirst(max - 1)) + rare).sorted { a, b in
            if uses(a) != uses(b) { return uses(a) > uses(b) }
            if slots[a].more != slots[b].more { return !slots[a].more }
            return a < b
        }
        return (shown + [moreSlot], rest.map { slots[$0] })
    }

    private static func byUse(_ slots: [Slot], max: Int, usage: [String: Int]) -> (visible: [Slot], overflow: [Slot]) {
        guard max > 1, slots.count > max else { return fit(slots, max: max) }
        // By position, not id: two recipes can share a name.
        let ranked = slots.indices.sorted { a, b in
            let ca = usage[slots[a].id] ?? 0, cb = usage[slots[b].id] ?? 0
            return ca != cb ? ca > cb : a < b
        }
        let keep = Set(ranked.prefix(max - 1))
        let shown = slots.indices.filter { keep.contains($0) }.map { slots[$0] }
        let rest = ranked.dropFirst(max - 1).map { slots[$0] }
        return (shown + [moreSlot], rest)
    }

    /// Presets for "More…": the first slots behind it, so Orbit and Bloom
    /// reach them without the menu. Their ids are the slots' own.
    public static func morePresets(_ overflow: [Slot]) -> [SlotVariant] {
        overflow.prefix(WheelSpec.maxPresets).map {
            let short = $0.label.hasSuffix("…") ? String($0.label.dropLast()) : $0.label
            return SlotVariant(id: $0.id, label: short, hint: $0.label, action: $0.action)
        }
    }

    /// The app's own Rename tool, offered for any selection.
    public static let renameSlot = Slot(id: "rename", label: "Rename…", symbol: "character.cursor.ibeam",
                                        group: "tool", action: .object(["kind": .string("app_rename")]),
                                        window: "rename")

    /// The app's own tools for a selection: Rename, then the quick actions
    /// that fit it (Copy image only for one picture).
    public static func appTools(for files: [URL]) -> [Slot] {
        [renameSlot] + QuickAction.offered(for: files).map(\.slot)
    }

    /// Adds app-side tools to the engine's list, just before "Copy
    /// SHA-256", which stays last (folders have no checksum: then at the
    /// end). Files named for something they aren't get "Rename to .jpg"
    /// first, where it's quickest to reach.
    public static func withAppTools(_ tools: [Slot], files: [URL] = [], fixes: [String: String] = [:]) -> [Slot] {
        guard !tools.contains(where: { $0.id == renameSlot.id }) else { return tools }
        var out = tools
        let at = out.firstIndex { $0.id == "checksum" } ?? out.endIndex
        out.insert(contentsOf: appTools(for: files), at: at)
        if let fix = ExtensionFix.slot(for: files, fixes: fixes) { out.insert(fix, at: 0) }
        return out
    }
}

/// Files whose names don't match what they hold (a .png that is really a
/// JPEG, or a download with no extension), renamed to the extension the
/// engine found. Offered only when every selected file needs one.
public enum ExtensionFix {
    public static let kind = "app_fix_extension"

    /// "photo.png" with "jpg" becomes "photo.jpg"; "scan" becomes "scan.jpg".
    public static func target(_ url: URL, ext: String) -> URL {
        url.pathExtension.isEmpty ? url.appendingPathExtension(ext)
            : url.deletingPathExtension().appendingPathExtension(ext)
    }

    /// The slot for `files`, given the engine's fixes by path.
    public static func slot(for files: [URL], fixes: [String: String]) -> Slot? {
        guard !files.isEmpty else { return nil }
        var map: [String: JSONValue] = [:]
        for f in files {
            guard let ext = fixes[f.path] else { return nil }
            map[f.path] = .string(ext)
        }
        let exts = Set(map.values.compactMap(\.string))
        let label = exts.count == 1 ? "Rename to .\(exts.first!)" : "Fix extensions"
        return Slot(id: "fix_extension", label: label, symbol: "wand.and.stars", group: "tool",
                    action: .object(["kind": .string(kind), "extensions": .object(map)]))
    }

    /// The renames a slot asks for, for those of `files` it covers.
    public static func changes(_ slot: Slot, files: [URL]) -> [RenamePlan.Change] {
        guard slot.action["kind"]?.string == kind, case .object(let map)? = slot.action["extensions"] else { return [] }
        return files.compactMap { url in
            map[url.path]?.string.map { RenamePlan.Change(from: url, to: target(url, ext: $0)) }
        }
    }
}
