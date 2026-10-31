import Foundation

/// Batch renaming: a pattern such as "Trip {date} {n}" turned into new
/// names, never clashing with each other or with files already there.
public enum RenamePlan {
    public struct Input: Equatable, Sendable {
        public let url: URL
        /// When the photo or video was taken, or the file was created.
        public let date: Date?
        /// A name for what's in it ("Golden retriever on a beach"), made
        /// on-device; nil when there's nothing to go on.
        public let content: String?

        public init(url: URL, date: Date?, content: String? = nil) {
            self.url = url
            self.date = date
            self.content = content
        }
    }

    public struct Change: Equatable, Sendable {
        public let from: URL
        public let to: URL
    }

    public enum Problem: Error, Equatable, Sendable {
        case empty
        case unknownToken(String)
        case emptyExtension
        case badExtension(String)

        public var message: String {
            switch self {
            case .empty: return "The pattern makes an empty name."
            case .unknownToken(let t): return "\"{\(t)}\" isn't something Radlet can fill in."
            case .emptyExtension: return "Type the new extension, such as jpg."
            case .badExtension(let e): return "\"\(e)\" can't be an extension: no spaces, slashes or colons."
            }
        }
    }

    /// What happens to the part after the dot. Keep is the default:
    /// renaming a file never changes what it is.
    public enum ExtensionRule: Equatable, Sendable {
        case keep
        case lowercase
        case custom(String)

        /// The extension a file ends up with, or a problem with a custom one.
        func apply(to ext: String) -> Result<String, Problem> {
            switch self {
            case .keep: return .success(ext)
            case .lowercase: return .success(ext.lowercased())
            case .custom(let typed):
                var e = typed.trimmingCharacters(in: .whitespacesAndNewlines)
                while e.hasPrefix(".") { e.removeFirst() }
                while e.hasSuffix(".") { e.removeLast() }
                if e.isEmpty { return .failure(.emptyExtension) }
                if e.contains(where: { $0.isWhitespace || $0 == "/" || $0 == ":" }) { return .failure(.badExtension(e)) }
                return .success(e)
            }
        }
    }

    /// Extensions that name the same kind of file.
    static let sameKind: [Set<String>] = [
        ["jpg", "jpeg", "jpe", "jfif"], ["tif", "tiff"], ["htm", "html"], ["yml", "yaml"],
        ["txt", "text", "md", "markdown", "mdown", "log"], ["heic", "heif"], ["aif", "aiff", "aifc"],
        ["mpg", "mpeg"], ["mp4", "m4v"], ["mid", "midi"], ["ts", "mts", "m2ts"],
    ]

    /// A word of caution when a custom extension names a different kind of
    /// file than some of them are: renaming doesn't convert.
    public static func extensionWarning(_ files: [URL], rule: ExtensionRule) -> String? {
        guard case .custom = rule, case .success(let new) = rule.apply(to: "") else { return nil }
        let to = new.lowercased()
        let changesKind = files.contains { url in
            let from = url.pathExtension.lowercased()
            guard !from.isEmpty, from != to else { return false }
            return !sameKind.contains { $0.contains(from) && $0.contains(to) }
        }
        return changesKind
            ? "Only the name changes; what's inside stays the same. To change the format, use Convert (Shift)."
            : nil
    }

    public static let tokens = ["name", "n", "date", "time", "folder", "content"]

    /// Whether the pattern uses capture dates, which load in the background.
    public static func needsDates(_ pattern: String) -> Bool {
        let p = pattern.lowercased()
        return p.contains("{date") || p.contains("{time")
    }

    /// Whether the pattern uses what's in each file, which is read in the
    /// background (and only when asked for: it takes a moment per file).
    public static func needsContent(_ pattern: String) -> Bool {
        pattern.lowercased().contains("{content")
    }

    /// `url` if nothing's there, otherwise the first free "name 2.ext",
    /// "name 3.ext"…
    public static func freeName(_ url: URL, exists: (URL) -> Bool) -> URL {
        guard exists(url) else { return url }
        let dir = url.deletingLastPathComponent()
        let stem = url.deletingPathExtension().lastPathComponent
        let ext = url.pathExtension
        var k = 2
        while true {
            let name = ext.isEmpty ? "\(stem) \(k)" : "\(stem) \(k).\(ext)"
            let candidate = dir.appendingPathComponent(name)
            if !exists(candidate) { return candidate }
            k += 1
        }
    }

    /// New names for `inputs`, in order, numbered from `start`. Files whose
    /// name wouldn't change are left out. `exists` reports files already on
    /// disk (other than the ones being renamed).
    public static func make(_ inputs: [Input], pattern: String, start: Int = 1,
                            extensionRule: ExtensionRule = .keep,
                            calendar: Calendar = .current,
                            exists: (URL) -> Bool) -> Result<[Change], Problem> {
        // Numbers are padded to the longest one: 01 to 12.
        let width = String(start + max(inputs.count - 1, 0)).count
        let renaming = Set(inputs.map { $0.url.standardizedFileURL.path })
        var taken = Set<String>()
        var changes: [Change] = []
        for (i, input) in inputs.enumerated() {
            let stem: String
            switch fill(pattern, input: input, number: start + i, width: width, calendar: calendar) {
            case .success(let s): stem = s
            case .failure(let p): return .failure(p)
            }
            guard !stem.isEmpty else { return .failure(.empty) }
            let dir = input.url.deletingLastPathComponent()
            let ext: String
            switch extensionRule.apply(to: input.url.pathExtension) {
            case .success(let e): ext = e
            case .failure(let p): return .failure(p)
            }
            func candidate(_ k: Int) -> URL {
                let name = k == 1 ? stem : "\(stem) \(k)"
                return dir.appendingPathComponent(ext.isEmpty ? name : "\(name).\(ext)")
            }
            var k = 1
            var target = candidate(k)
            // A name is free if no other file has it: files being renamed
            // free their names up, and names chosen in this batch are taken.
            while taken.contains(target.standardizedFileURL.path.lowercased())
                    || (exists(target) && !renaming.contains(target.standardizedFileURL.path)
                        && target.standardizedFileURL.path.lowercased() != input.url.standardizedFileURL.path.lowercased()) {
                k += 1
                target = candidate(k)
            }
            taken.insert(target.standardizedFileURL.path.lowercased())
            if target.standardizedFileURL.path != input.url.standardizedFileURL.path {
                changes.append(Change(from: input.url, to: target))
            }
        }
        return .success(changes)
    }

    static func fill(_ pattern: String, input: Input, number: Int, width: Int,
                     calendar: Calendar) -> Result<String, Problem> {
        var out = ""
        var rest = Substring(pattern)
        while let open = rest.firstIndex(of: "{") {
            out += rest[..<open]
            guard let close = rest[open...].firstIndex(of: "}") else {
                out += rest[open...]
                rest = ""
                break
            }
            let token = String(rest[rest.index(after: open)..<close])
            rest = rest[rest.index(after: close)...]
            let parts = token.split(separator: ":", maxSplits: 1).map(String.init)
            let key = parts.first?.lowercased() ?? ""
            switch key {
            case "name":
                out += input.url.deletingPathExtension().lastPathComponent
            case "n":
                let digits = parts.count > 1 ? min(max(Int(parts[1]) ?? width, 1), 9) : width
                out += String(format: "%0\(digits)d", number)
            case "date", "time":
                let date = input.date ?? Date(timeIntervalSince1970: 0)
                let c = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
                if key == "date" {
                    out += String(format: "%04d-%02d-%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0)
                } else {
                    out += String(format: "%02d.%02d.%02d", c.hour ?? 0, c.minute ?? 0, c.second ?? 0)
                }
            case "folder":
                out += input.url.deletingLastPathComponent().lastPathComponent
            case "content":
                // Nothing to go on: the file keeps its name there.
                out += input.content ?? input.url.deletingPathExtension().lastPathComponent
            default:
                return .failure(.unknownToken(token))
            }
        }
        out += rest
        // Characters Finder can't show in a name.
        let cleaned = out.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: ":", with: "-")
        return .success(cleaned.trimmingCharacters(in: .whitespacesAndNewlines))
    }
}
