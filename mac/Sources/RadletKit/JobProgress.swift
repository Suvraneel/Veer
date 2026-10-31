import Combine
import Foundation

/// Live state of the job shown in the HUD.
@MainActor
public final class JobProgress: ObservableObject {
    public enum Phase: Sendable { case running, finished, failed }
    /// What a job that writes no files computes, which decides how the
    /// results go on the clipboard.
    /// What a job's values are: copied to the clipboard, except a syntax
    /// check's verdict, which is only shown.
    public enum ValueKind: Sendable { case checksum, text, count, code, description, check, dataURI }

    /// A value computed for one file.
    public struct NamedValue: Equatable, Sendable {
        /// Position in the selection.
        public let index: Int
        public let name: String
        public let value: String
    }

    @Published public var title = ""
    @Published public var total = 0
    @Published public var completed = 0
    @Published public var fraction: Double = 0
    @Published public var phase: Phase = .running
    @Published public var outputs: [URL] = []
    @Published public var failures: [String] = []
    @Published public var skipped: [String] = []
    /// What failed items need first (`Need` raw values), in the order met.
    @Published public var needs: [String] = []
    /// The files of those items by need, to try again once it's got.
    public private(set) var needing: [String: [URL]] = [:]
    @Published public var savedBytes: Int64 = 0
    /// Checksums or recognised text, one per file.
    @Published public private(set) var values: [NamedValue] = []
    /// Set after `reset` for jobs that compute values.
    public var valueKind: ValueKind = .checksum
    /// "SHA-256", or the algorithm a checksum job uses.
    public var checksumName = "SHA-256"
    /// A one-off status line, such as the result of Undo.
    @Published public var message: String?
    @Published public var hovering = false

    /// Items that made one new file from one original, for "Replace
    /// originals".
    public private(set) var replaceable: [ReplaceCandidate] = []
    public var job: Int?
    /// Identifies the job this state belongs to.
    public var generation = UUID()
    private var partial: [Int: Double] = [:]

    public init() {}

    public func reset(title: String, total: Int) {
        self.title = title
        self.total = total
        completed = 0
        fraction = 0
        phase = .running
        outputs = []
        failures = []
        skipped = []
        needs = []
        needing = [:]
        savedBytes = 0
        values = []
        valueKind = .checksum
        checksumName = "SHA-256"
        message = nil
        replaceable = []
        partial = [:]
        job = nil
    }

    public func handle(_ event: JobEvent) {
        switch event {
        case .started(let i):
            partial[i] = 0
        case .progress(let i, let f):
            partial[i] = min(max(f, 0), 1)
        case .finished(let o):
            partial.removeValue(forKey: o.index)
            completed += 1
            let name = o.inputs.first.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "file"
            switch o.status {
            case "done":
                let made = o.createdItems.map { URL(fileURLWithPath: $0) }
                outputs.append(contentsOf: made)
                if o.inputs.count == 1, made.count == 1, o.value == nil {
                    replaceable.append(ReplaceCandidate(input: URL(fileURLWithPath: o.inputs[0]), output: made[0]))
                }
                if let b = o.bytesOut { savedBytes += Int64(o.bytesIn) - Int64(b) }
                if let v = o.value { values.append(NamedValue(index: o.index, name: name, value: v)) }
            case "skipped":
                skipped.append("\(name): \(o.error?.message ?? "skipped")")
            case "failed":
                failures.append("\(name): \(o.error?.message ?? "failed")")
                if let need = o.error?.need {
                    if !needs.contains(need) { needs.append(need) }
                    needing[need, default: []].append(contentsOf: o.inputs.map { URL(fileURLWithPath: $0) })
                }
            default:
                break
            }
        case .jobFinished(_, let cancelled):
            partial.removeAll()
            phase = failures.isEmpty && !cancelled ? .finished : .failed
            if cancelled && failures.isEmpty { failures.append("Cancelled") }
        }
        let running = partial.values.reduce(0, +)
        fraction = total > 0 ? min(1, (Double(completed) + running) / Double(total)) : 0
    }

    public var detail: String {
        switch phase {
        case .running:
            return "\(completed) of \(total)"
        case .finished, .failed:
            if let message { return message }
            if valueKind == .check {
                // Problems are failures, each with its line and column.
                let problems = failures.filter { $0 != "Cancelled" }.count + skipped.count
                if problems == 0, values.count == 1 { return values[0].value }
                if problems == 0 { return "All \(values.count) valid" }
                return values.isEmpty ? "\(problems) with problems" : "\(values.count) valid · \(problems) with problems"
            }
            if !values.isEmpty {
                // One word count or code is worth showing, not just
                // copying.
                if valueKind == .count || valueKind == .code, values.count == 1, skipped.isEmpty, failures.isEmpty {
                    return "\(values[0].value) · copied"
                }
                let what: String
                switch valueKind {
                case .text: what = "Text"
                case .checksum: what = checksumName
                case .dataURI: what = values.count == 1 ? "Data URI" : "Data URIs"
                case .check: what = "Results"
                case .count: what = "Word counts"
                case .code: what = "Codes"
                case .description: what = "Alt text"
                }
                let problems = skipped.count + failures.filter { $0 != "Cancelled" }.count
                return problems > 0 ? "\(what) copied to the clipboard · \(problems) without" : "\(what) copied to the clipboard"
            }
            var parts: [String] = []
            if !outputs.isEmpty { parts.append("\(outputs.count) done") }
            if !skipped.isEmpty { parts.append("\(skipped.count) skipped") }
            if !failures.isEmpty, failures != ["Cancelled"] { parts.append("\(failures.count) failed") }
            if savedBytes > 0 {
                parts.append(ByteCountFormatter.string(fromByteCount: savedBytes, countStyle: .file) + " smaller")
            }
            // A shortcut that sends or shares its file finishes with
            // nothing to show.
            if parts.isEmpty && completed > 0 && failures.isEmpty && skipped.isEmpty { return "Done" }
            return parts.isEmpty ? "Nothing to do" : parts.joined(separator: " · ")
        }
    }

    /// Text for the clipboard. Checksums: the bare hash for one file,
    /// "hash  name" lines (as `shasum` prints them) for several. Text: the
    /// text itself for one file; for several, each under its file name.
    /// Word counts: the count, or "name: count" lines.
    public var clipboardText: String? {
        guard !values.isEmpty, valueKind != .check else { return nil }
        // Files finish in any order; the clipboard follows the selection.
        let values = self.values.sorted { $0.index < $1.index }
        switch valueKind {
        case .checksum:
            if values.count == 1 { return values[0].value }
            return values.map { "\($0.value)  \($0.name)" }.joined(separator: "\n")
        case .text, .code, .description, .dataURI, .check:
            if values.count == 1 { return values[0].value }
            return values.map { "\($0.name)\n\n\($0.value)" }.joined(separator: "\n\n")
        case .count:
            if values.count == 1 { return values[0].value }
            return values.map { "\($0.name): \($0.value)" }.joined(separator: "\n")
        }
    }
}
