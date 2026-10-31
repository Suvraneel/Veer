import Foundation

/// Any JSON value. Wheel slots carry their action as raw JSON, which the
/// app sends back to the engine unchanged, so the app never needs to know
/// how conversions or tools are described.
public enum JSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case int(Int64)
    case double(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let b = try? c.decode(Bool.self) {
            self = .bool(b)
        } else if let i = try? c.decode(Int64.self) {
            self = .int(i)
        } else if let d = try? c.decode(Double.self) {
            self = .double(d)
        } else if let s = try? c.decode(String.self) {
            self = .string(s)
        } else if let a = try? c.decode([JSONValue].self) {
            self = .array(a)
        } else {
            self = .object(try c.decode([String: JSONValue].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .int(let i): try c.encode(i)
        case .double(let d): try c.encode(d)
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }

    public subscript(key: String) -> JSONValue? {
        if case .object(let o) = self { return o[key] }
        return nil
    }

    public var string: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    public var int: Int? {
        switch self {
        case .int(let i): return Int(i)
        case .double(let d): return Int(d)
        default: return nil
        }
    }

    public var double: Double? {
        switch self {
        case .int(let i): return Double(i)
        case .double(let d): return d
        default: return nil
        }
    }

    public var bool: Bool? {
        if case .bool(let b) = self { return b }
        return nil
    }

    /// Decodes this value as a concrete `Decodable` type.
    public func decode<T: Decodable>(_ type: T.Type) throws -> T {
        let data = try JSONEncoder().encode(self)
        return try JSONDecoder().decode(type, from: data)
    }

    /// Parses one line of the helper protocol.
    public static func parse(_ data: Data) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: data)
    }

    /// One protocol line, newline included.
    public func line() throws -> Data {
        var data = try JSONEncoder().encode(self)
        data.append(0x0A)
        return data
    }
}

/// One segment of the wheel, as described by the engine.
public struct Slot: Codable, Identifiable, Equatable, Sendable {
    public let id: String
    public let label: String
    /// SF Symbol name.
    public let symbol: String
    /// "convert", "tool" or "recipe".
    public let group: String
    public let action: JSONValue
    /// Tool window to open before running ("resize", "crop", "redact",
    /// "compress", "split_pdf", "pages_pdf"); `action` is its default.
    public let window: String?
    /// Quick presets, most useful first: the Orbit style's outer ring
    /// shows two, Bloom up to four. They run without a window.
    public let variants: [SlotVariant]
    /// Set when this can't run until something is got (see `Need`): the
    /// wheel marks it, and choosing it offers the install first.
    public let needs: String?
    /// A conversion fewer people want (a less known format): it waits
    /// behind "More…" even when the wheel has room.
    public let more: Bool

    public init(id: String, label: String, symbol: String, group: String, action: JSONValue, window: String? = nil,
                variants: [SlotVariant] = [], needs: String? = nil, more: Bool = false) {
        self.id = id
        self.label = label
        self.symbol = symbol
        self.group = group
        self.action = action
        self.window = window
        self.variants = variants
        self.needs = needs
        self.more = more
    }

    private enum CodingKeys: String, CodingKey {
        case id, label, symbol, group, action, window, variants, needs, more
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        label = try c.decode(String.self, forKey: .label)
        symbol = try c.decode(String.self, forKey: .symbol)
        group = try c.decode(String.self, forKey: .group)
        action = try c.decode(JSONValue.self, forKey: .action)
        window = try c.decodeIfPresent(String.self, forKey: .window)
        variants = try c.decodeIfPresent([SlotVariant].self, forKey: .variants) ?? []
        needs = try c.decodeIfPresent(String.self, forKey: .needs)
        more = try c.decodeIfPresent(Bool.self, forKey: .more) ?? false
    }

    /// What this needs first, when it's something the app knows how to get.
    public var need: Need? { needs.flatMap(Need.init(rawValue:)) }

    /// The same slot, ready to run once what it needed is there.
    public var unlocked: Slot {
        Slot(id: id, label: label, symbol: symbol, group: group, action: action, window: window, variants: variants,
             more: more)
    }

    /// The same slot running a different action, without its window.
    public func with(action: JSONValue) -> Slot {
        Slot(id: id, label: label, symbol: symbol, group: group, action: action, window: nil, needs: needs, more: more)
    }

    /// One of this slot's presets as a slot of its own, for running and
    /// for History: "JPEG · Smart".
    public func running(_ variant: SlotVariant) -> Slot {
        let base = label.hasSuffix("…") ? String(label.dropLast()) : label
        return Slot(id: variant.id, label: "\(base) · \(variant.label)", symbol: symbol, group: group,
                    action: variant.action, window: nil, needs: needs)
    }

    /// The label without its trailing "…", for titles.
    public var plainLabel: String {
        label.hasSuffix("…") ? String(label.dropLast()) : label
    }

    /// The HUD's title while this runs on `files`: "Converting a.png to
    /// JPEG", "Resize: 3 files", "Images to JPEG: Trip".
    public func jobTitle(files: [URL]) -> String {
        let what = files.count == 1 ? files[0].lastPathComponent : "\(files.count) files"
        if group == "convert" && action["kind"]?.string != "convert_inside" {
            return "Converting \(what) to \(label)"
        }
        return "\(plainLabel): \(what)"
    }

    /// Whether a result stands in for its original, so "Replace
    /// originals" may move the original to the Trash: a changed copy of
    /// the same thing (converted, resized, compressed, cleaned, locked),
    /// not something made from it (a QR code, icons, a translation,
    /// subtitles, a report, an archive, a folder's converted contents).
    public var replacesOriginal: Bool {
        switch action["kind"]?.string {
        case "steps":
            guard case .array(let steps)? = action["steps"], !steps.isEmpty else { return false }
            return !steps.contains { step in
                switch step["op"]?.string {
                case "translate", "transcribe": return true
                case "convert": return ["zip", "tar", "folder"].contains(step["to"]?.string ?? "")
                default: return false
                }
            }
        case "protect_pdf", "unlock_pdf":
            return true
        default:
            return false
        }
    }

    public var isChecksum: Bool { action["kind"]?.string == "checksum" }
    /// "SHA-256", or the algorithm this checksum uses.
    public var checksumName: String {
        switch action["algorithm"]?.string {
        case "md5": return "MD5"
        case "sha1": return "SHA-1"
        case "sha512": return "SHA-512"
        default: return "SHA-256"
        }
    }
    /// A syntax check: its verdict is shown, not copied.
    public var isCheckSyntax: Bool { action["kind"]?.string == "check_syntax" }
    public var isDataURI: Bool { action["kind"]?.string == "data_uri" }
    public var isCopyText: Bool { action["kind"]?.string == "copy_text" }
    /// Alt text for pictures, for the clipboard.
    public var isDescribe: Bool { action["kind"]?.string == "describe" }
    public var isWordCount: Bool { action["kind"]?.string == "word_count" }
    public var isReadCodes: Bool { action["kind"]?.string == "read_codes" }
    public var isCombine: Bool { action["kind"]?.string == "combine_pdf" }
    /// Runs once over the whole selection (one item in the HUD), rather
    /// than once per file.
    public var isWholeSelection: Bool {
        ["combine_pdf", "combine_text", "combine_workbook", "zip_all", "zip_locked", "merge_folders",
         "find_duplicates", "join_media", "burn_subtitles", "scan_documents"]
            .contains(action["kind"]?.string ?? "")
    }
}

/// A preset of a slot: "Q 95" for JPEG, "50 %" for Resize.
public struct SlotVariant: Codable, Identifiable, Equatable, Sendable {
    public let id: String
    /// Short, for the wheel.
    public let label: String
    /// What it does, in a few words.
    public let hint: String
    public let action: JSONValue

    public init(id: String, label: String, hint: String, action: JSONValue) {
        self.id = id
        self.label = label
        self.hint = hint
        self.action = action
    }
}

public struct Wheel: Codable, Equatable, Sendable {
    public var convert: [Slot]
    public var tools: [Slot]
    public var recipes: [Slot]

    public init(convert: [Slot], tools: [Slot], recipes: [Slot]) {
        self.convert = convert
        self.tools = tools
        self.recipes = recipes
    }

    public static let empty = Wheel(convert: [], tools: [], recipes: [])
}

public struct FileInfo: Codable, Sendable {
    public let path: String
    public let format: String?
    public let label: String?
    /// The extension the file should have, when its name doesn't match
    /// what it holds (a .png that is really a JPEG) or it has none.
    public let fixExtension: String?

    private enum CodingKeys: String, CodingKey {
        case path, format, label
        case fixExtension = "fix_extension"
    }
}

public struct InspectResult: Codable, Sendable {
    public let files: [FileInfo]
    public let wheel: Wheel
}

public struct EngineErrorInfo: Codable, Sendable {
    public let code: String
    public let message: String
    /// Set when getting something would fix it (see `Need`).
    public let need: String?
}

public struct Outcome: Codable, Sendable {
    /// Everything this outcome created, for Show in Finder and Undo.
    public var createdItems: [String] {
        if let outputs, !outputs.isEmpty { return outputs }
        return output.map { [$0] } ?? []
    }

    public let index: Int
    public let inputs: [String]
    /// "done", "skipped", "failed" or "cancelled".
    public let status: String
    public let output: String?
    /// Every top-level item created (files, or one folder of many files).
    public let outputs: [String]?
    public let value: String?
    public let error: EngineErrorInfo?
    public let bytesIn: UInt64
    public let bytesOut: UInt64?
    public let millis: UInt64

    enum CodingKeys: String, CodingKey {
        case index, inputs, status, output, outputs, value, error, millis
        case bytesIn = "bytes_in"
        case bytesOut = "bytes_out"
    }
}

public struct Summary: Codable, Sendable, Equatable {
    public let done: Int
    public let skipped: Int
    public let failed: Int
    public let cancelled: Int

    public init(done: Int, skipped: Int, failed: Int, cancelled: Int) {
        self.done = done
        self.skipped = skipped
        self.failed = failed
        self.cancelled = cancelled
    }

    public static let zero = Summary(done: 0, skipped: 0, failed: 0, cancelled: 0)
}

public enum JobEvent: Sendable {
    case started(index: Int)
    case progress(index: Int, fraction: Double)
    case finished(Outcome)
    case jobFinished(Summary, cancelled: Bool)

    /// Interprets one job event line from the helper, or nil if the line
    /// isn't a job event.
    public init?(message msg: JSONValue) {
        switch msg["event"]?.string {
        case "item_started":
            self = .started(index: msg["index"]?.int ?? 0)
        case "item_progress":
            self = .progress(index: msg["index"]?.int ?? 0, fraction: msg["fraction"]?.double ?? 0)
        case "item_finished":
            guard let outcome = try? msg["outcome"]?.decode(Outcome.self) else { return nil }
            self = .finished(outcome)
        case "job_finished":
            let summary = (try? msg["summary"]?.decode(Summary.self)) ?? .zero
            self = .jobFinished(summary, cancelled: msg["cancelled"]?.bool ?? false)
        default:
            return nil
        }
    }

    public var isFinal: Bool {
        if case .jobFinished = self { return true }
        return false
    }
}

public struct Capabilities: Codable, Sendable {
    public let ffmpeg: Bool
    public let ffprobe: Bool
    public let sips: Bool
    /// Missing from older helpers.
    public let pdfium: Bool?
    /// The Vision helper: text recognition and background removal.
    public let vision: Bool?
    /// The helper converts audio and video with AVFoundation when FFmpeg is
    /// missing. Missing from older helpers.
    public let avfoundation: Bool?
    /// This Mac can have Apple Intelligence (macOS 26, Apple silicon).
    /// Missing from older helpers.
    public let appleIntelligence: Bool?

    enum CodingKeys: String, CodingKey {
        case ffmpeg, ffprobe, sips, pdfium, vision, avfoundation
        case appleIntelligence = "apple_intelligence"
    }
}

public struct Hello: Codable, Sendable {
    public let version: String
    public let `protocol`: Int
    public let capabilities: Capabilities
}

/// Where outputs go.
public enum OutputChoice: String, CaseIterable, Identifiable, Sendable {
    case besideSource
    case downloads
    case custom

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .besideSource: return "Next to the original"
        case .downloads: return "Downloads folder"
        case .custom: return "A folder I choose"
        }
    }
}

/// Orders slots by how often they've been used, most first. Ties keep the
/// engine's order, which is most useful first.
public func orderByUsage(_ slots: [Slot], counts: [String: Int]) -> [Slot] {
    slots.enumerated()
        .sorted { a, b in
            let ca = counts[a.element.id] ?? 0
            let cb = counts[b.element.id] ?? 0
            return ca != cb ? ca > cb : a.offset < b.offset
        }
        .map(\.element)
}

/// A privacy-safe description of files for logs: counts and extensions,
/// never names or paths. "3 files (png ×2, jpg)".
public func describeFiles(_ urls: [URL]) -> String {
    guard !urls.isEmpty else { return "no files" }
    var counts: [String: Int] = [:]
    var order: [String] = []
    for url in urls {
        let ext = url.pathExtension.isEmpty ? "no extension" : url.pathExtension.lowercased()
        if counts[ext] == nil { order.append(ext) }
        counts[ext, default: 0] += 1
    }
    let parts = order.map { ext -> String in
        let n = counts[ext] ?? 0
        return n > 1 ? "\(ext) ×\(n)" : ext
    }
    let noun = urls.count == 1 ? "file" : "files"
    return "\(urls.count) \(noun) (\(parts.joined(separator: ", ")))"
}
