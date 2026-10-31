import AppKit
import AVFoundation
import ImageIO
import SwiftUI
import RadletKit

/// Batch rename: a pattern such as "Trip {date} {n}", previewed live, then
/// applied in place. Names never clash, and History can undo it.
@MainActor
final class RenameModel: ObservableObject {
    @Published var pattern = "{name} {n}" {
        didSet {
            loadContentIfNeeded()
            plan()
        }
    }
    @Published var startText = "1" { didSet { plan() } }
    /// What happens to the part after the dot: kept unless asked.
    @Published var extensionMode: ExtensionMode = .keep { didSet { plan() } }
    @Published var newExtension = "" { didSet { plan() } }

    enum ExtensionMode: String, CaseIterable, Identifiable {
        case keep, lowercase, change
        var id: String { rawValue }
        var title: String {
            switch self {
            case .keep: return "Keep"
            case .lowercase: return "Lowercase"
            case .change: return "Change to"
            }
        }
    }

    var extensionRule: RenamePlan.ExtensionRule {
        switch extensionMode {
        case .keep: return .keep
        case .lowercase: return .lowercase
        case .change: return .custom(newExtension)
        }
    }

    /// Renaming doesn't convert: said when a new extension names another
    /// kind of file.
    var extensionWarning: String? { RenamePlan.extensionWarning(files, rule: extensionRule) }
    @Published private(set) var changes: [RenamePlan.Change] = []
    @Published private(set) var problem: String?
    @Published private(set) var working = false
    /// Capture dates load in the background; {date} and {time} wait.
    @Published private(set) var datesLoaded = false
    /// Names from what's in each file, read only once {content} is used.
    @Published private(set) var contentLoaded = false
    private var contentRequested = false
    private var contents: [URL: String] = [:]
    let files: [URL]
    private var dates: [URL: Date] = [:]
    private let done: ([HistoryEntry.Rename]) -> Void

    init(files: [URL], done: @escaping ([HistoryEntry.Rename]) -> Void) {
        // Rename in the order Finder lists them: by name, numbers sorted
        // as people expect ("IMG_2" before "IMG_10").
        self.files = files.sorted {
            $0.lastPathComponent.localizedStandardCompare($1.lastPathComponent) == .orderedAscending
        }
        self.done = done
        plan()
        Task { [weak self] in await self?.loadDates() }
    }

    /// Capture dates for photos and videos; creation dates otherwise.
    private func loadDates() async {
        for url in files {
            if let d = Self.photoDate(url) {
                dates[url] = d
            } else if let d = await Self.videoDate(url) {
                dates[url] = d
            } else if let d = try? url.resourceValues(forKeys: [.creationDateKey]).creationDate {
                dates[url] = d
            }
        }
        datesLoaded = true
        plan()
    }

    /// Reads what's in each file, on-device, the first time the pattern
    /// asks for {content}.
    private func loadContentIfNeeded() {
        guard RenamePlan.needsContent(pattern), !contentRequested else { return }
        contentRequested = true
        Task { [weak self] in
            guard let self else { return }
            let paths = self.files.map { JSONValue.string($0.path) }
            do {
                let result = try await EngineClient.shared.request("content_names", ["paths": .array(paths)])
                if case .array(let names)? = result["names"] {
                    for (url, name) in zip(self.files, names) {
                        if let n = name.string { self.contents[url] = n }
                    }
                }
                Log.app("rename: named \(self.contents.count) of \(self.files.count) files by content")
            } catch {
                Log.app("rename: content names failed: \(Log.code(error))")
            }
            self.contentLoaded = true
            self.plan()
        }
    }

    static func photoDate(_ url: URL) -> Date? {
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil),
              let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any] else { return nil }
        let exif = props[kCGImagePropertyExifDictionary] as? [CFString: Any]
        let tiff = props[kCGImagePropertyTIFFDictionary] as? [CFString: Any]
        guard let text = (exif?[kCGImagePropertyExifDateTimeOriginal] as? String)
                ?? (tiff?[kCGImagePropertyTIFFDateTime] as? String) else { return nil }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy:MM:dd HH:mm:ss"
        return f.date(from: text)
    }

    static func videoDate(_ url: URL) async -> Date? {
        let asset = AVURLAsset(url: url)
        guard let item = try? await asset.load(.creationDate) else { return nil }
        return try? await item.load(.dateValue)
    }

    var start: Int { Int(startText.trimmingCharacters(in: .whitespaces)) ?? 1 }

    private func plan() {
        let inputs = files.map { RenamePlan.Input(url: $0, date: dates[$0], content: contents[$0]) }
        switch RenamePlan.make(inputs, pattern: pattern, start: start, extensionRule: extensionRule,
                               exists: { FileManager.default.fileExists(atPath: $0.path) }) {
        case .success(let c):
            changes = c
            problem = nil
        case .failure(let p):
            changes = []
            problem = p.message
        }
    }

    func insert(_ token: String) {
        pattern += pattern.hasSuffix(" ") || pattern.isEmpty ? "{\(token)}" : " {\(token)}"
    }

    /// Dates are still loading and the pattern needs them.
    var waitingForDates: Bool { !datesLoaded && RenamePlan.needsDates(pattern) }

    /// What's in the files is still being read and the pattern needs it.
    var waitingForContent: Bool { !contentLoaded && RenamePlan.needsContent(pattern) }

    var canApply: Bool { problem == nil && !changes.isEmpty && !working && !waitingForDates && !waitingForContent }

    var note: String {
        if let problem { return problem }
        if waitingForDates { return "Reading the dates photos and videos were taken…" }
        if waitingForContent { return "Looking at what's in each file, on this Mac…" }
        if changes.isEmpty { return "Every name stays the same." }
        let same = files.count - changes.count
        return "Renames \(changes.count) file\(changes.count == 1 ? "" : "s")" + (same > 0 ? " (\(same) already match)" : "")
            + ". Undo from History if you change your mind."
    }

    /// Renames in two steps (to hidden temporary names, then the new ones)
    /// so files can swap names. Anything that fails goes back.
    func apply() {
        guard canApply else { return }
        working = true
        let fm = FileManager.default
        var staged: [(temp: URL, change: RenamePlan.Change)] = []
        for c in changes {
            let temp = c.from.deletingLastPathComponent().appendingPathComponent(".radlet-rename-\(UUID().uuidString)")
            do {
                try fm.moveItem(at: c.from, to: temp)
                staged.append((temp, c))
            } catch {
                Log.app("rename: couldn't move a file aside: \(Log.code(error))")
                for s in staged.reversed() { putBack(s.temp, as: s.change.from) }
                working = false
                problem = "Couldn't rename \(c.from.lastPathComponent): \(error.localizedDescription)"
                return
            }
        }
        var renamed: [HistoryEntry.Rename] = []
        var failed = 0
        for s in staged {
            // Something may have appeared there meanwhile.
            let target = RenamePlan.freeName(s.change.to) { fm.fileExists(atPath: $0.path) }
            do {
                try fm.moveItem(at: s.temp, to: target)
                renamed.append(HistoryEntry.Rename(from: s.change.from.path, to: target.path))
            } catch {
                Log.app("rename: couldn't give a file its new name: \(Log.code(error))")
                failed += 1
                if let back = putBack(s.temp, as: s.change.from), back != s.change.from {
                    // Its old name went to another file in the batch.
                    renamed.append(HistoryEntry.Rename(from: s.change.from.path, to: back.path))
                }
            }
        }
        Log.app("renamed \(renamed.count) of \(changes.count) files")
        working = false
        if failed > 0 {
            problem = "\(failed) file\(failed == 1 ? "" : "s") couldn't be renamed. Check they aren't locked or open elsewhere."
            if renamed.isEmpty { return }
            let alert = NSAlert()
            alert.messageText = "Renamed \(renamed.count) of \(changes.count) files"
            alert.informativeText = problem ?? ""
            alert.runModal()
        }
        done(renamed)
    }

    /// Moves a file from its hidden temporary name back to `name`, or a
    /// numbered variant if that's taken, so it never stays hidden.
    @discardableResult
    private func putBack(_ temp: URL, as name: URL) -> URL? {
        let fm = FileManager.default
        let target = RenamePlan.freeName(name) { fm.fileExists(atPath: $0.path) }
        do {
            try fm.moveItem(at: temp, to: target)
            return target
        } catch {
            Log.app("rename: couldn't put a file back: \(Log.code(error))")
            return nil
        }
    }

    func cancel() { done([]) }
}

struct RenameView: View {
    @ObservedObject var model: RenameModel

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    TextField("Pattern", text: $model.pattern)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(.body, design: .monospaced))
                    Text("Start at")
                    TextField("", text: $model.startText)
                        .multilineTextAlignment(.trailing)
                        .frame(width: 50)
                }
                HStack(spacing: 6) {
                    Text("Insert:").font(.caption).foregroundStyle(.secondary)
                    Button("Name") { model.insert("name") }
                    Button("Number") { model.insert("n") }
                    Button("Date taken") { model.insert("date") }
                    Button("Time") { model.insert("time") }
                    Button("Folder") { model.insert("folder") }
                    Button("What's in it") { model.insert("content") }
                        .help("A short name for what's in each picture or document, made on this Mac")
                }
                .controlSize(.small)
                HStack(spacing: 8) {
                    Text("Extension")
                    Picker("Extension", selection: $model.extensionMode) {
                        ForEach(RenameModel.ExtensionMode.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 270)
                    if model.extensionMode == .change {
                        TextField("jpg", text: $model.newExtension)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(.body, design: .monospaced))
                            .frame(width: 90)
                    }
                    Spacer()
                }
                if let warning = model.extensionWarning {
                    Label(warning, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(14)
            Divider()
            List {
                ForEach(model.changes, id: \.from) { c in
                    HStack(spacing: 8) {
                        Text(c.from.lastPathComponent)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Image(systemName: "arrow.right").foregroundStyle(.tertiary)
                        Text(c.to.lastPathComponent)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            Divider()
            ToolFooter(note: model.note, applyTitle: "Rename", canApply: model.canApply,
                       cancel: { model.cancel() }, apply: { model.apply() })
        }
        .frame(minWidth: 520, idealWidth: 620, maxWidth: .infinity, minHeight: 360, idealHeight: 480, maxHeight: .infinity)
    }
}
