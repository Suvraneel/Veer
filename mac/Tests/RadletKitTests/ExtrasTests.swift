import XCTest
@testable import RadletKit

final class ExtraActionTests: XCTestCase {
    private func json(_ v: JSONValue) throws -> String {
        let enc = JSONEncoder()
        enc.outputFormatting = .sortedKeys
        return String(decoding: try enc.encode(v), as: UTF8.self)
    }

    /// Also checked by the engine's `tool_window_actions_are_accepted`.
    func testShapes() throws {
        XCTAssertEqual(try json(ToolAction.pageNumbers(position: .bottomRight, style: .pageOfTotal, start: 5,
                                                       size: 10, skipFirst: true)),
                       #"{"kind":"steps","steps":[{"op":"page_numbers","position":"bottom_right","size":10,"skip_first":true,"start":5,"style":"page_of_total"}]}"#)
        XCTAssertEqual(try json(ToolAction.watermark(text: "DRAFT", opacity: 0.2, diagonal: true)),
                       #"{"kind":"steps","steps":[{"diagonal":true,"op":"watermark","opacity":0.2,"text":"DRAFT"}]}"#)
        XCTAssertEqual(try json(ToolAction.protectPDF(password: "pw")), #"{"kind":"protect_pdf","password":"pw"}"#)
        XCTAssertEqual(try json(ToolAction.unlockPDF(password: "pw")), #"{"kind":"unlock_pdf","password":"pw"}"#)
        XCTAssertEqual(try json(ToolAction.zipLocked(password: "pw")), #"{"kind":"zip_locked","password":"pw"}"#)
        XCTAssertEqual(try json(ToolAction.unzipLocked(password: "pw")), #"{"kind":"unzip_locked","password":"pw"}"#)
        XCTAssertEqual(try json(ToolAction.redactText(words: [" Jane Doe ", "", "  "], emails: true, phones: false,
                                                      cards: false)),
                       #"{"kind":"steps","steps":[{"emails":true,"op":"redact_text","words":["Jane Doe"]}]}"#)
        XCTAssertEqual(try json(ToolAction.trim(start: 1.5, end: 4)),
                       #"{"kind":"steps","steps":[{"end":4,"op":"trim","start":1.5}]}"#)
        XCTAssertEqual(try json(ToolAction.gif(start: 1, end: 2)),
                       #"{"kind":"steps","steps":[{"end":2,"op":"trim","start":1},{"op":"convert","to":"gif"}]}"#)
        XCTAssertEqual(try json(ToolAction.gif(start: 0, end: nil)),
                       #"{"kind":"steps","steps":[{"op":"convert","to":"gif"}]}"#)
        XCTAssertEqual(try json(ToolAction.pages([2, 1], rotate: [2: 90, 3: 180, 1: 360])),
                       #"{"kind":"steps","steps":[{"op":"pages","pages":[2,1],"rotate":[{"degrees":90,"page":2}]}]}"#)
        XCTAssertEqual(try json(ToolAction.pages([2, 1], rotate: [:])),
                       #"{"kind":"steps","steps":[{"op":"pages","pages":[2,1]}]}"#)
    }

    func testTimes() {
        XCTAssertEqual(TimeText.parse("90"), 90)
        XCTAssertEqual(TimeText.parse("1:30"), 90)
        XCTAssertEqual(TimeText.parse(" 1:02:03.5 "), 3723.5)
        XCTAssertNil(TimeText.parse("soon"))
        XCTAssertNil(TimeText.parse("1:2:3:4"))
        XCTAssertNil(TimeText.parse("-5"))
        XCTAssertEqual(TimeText.format(65.3), "1:05.3")
        XCTAssertEqual(TimeText.format(3723.5), "1:02:03.5")
        XCTAssertEqual(TimeText.format(0), "0:00.0")
    }

    func testStepDescriptions() {
        XCTAssertEqual(StepText.describe(.object(["op": .string("convert"), "to": .string("webp"), "quality": .int(80)])),
                       "Convert to WebP (quality 80)")
        XCTAssertEqual(StepText.describe(.object(["op": .string("resize"), "percent": .int(50)])), "Resize to 50%")
        XCTAssertEqual(StepText.describe(.object(["op": .string("trim"), "start": .double(1), "end": .double(65.3)])),
                       "Trim 0:01.0 to 1:05.3")
        XCTAssertEqual(StepText.describe(.object(["op": .string("page_numbers"), "style": .string("fraction")])),
                       "Page numbers (3 / 12)")
        XCTAssertEqual(StepText.describe(.object(["op": .string("future_thing")])), "Future Thing")
        XCTAssertEqual(StepText.describe(.object(["op": .string("trace")])), "Trace to SVG")
        XCTAssertEqual(StepText.describe(.object(["op": .string("trace"), "color": .bool(false),
                                                  "detail": .string("low")])),
                       "Trace to SVG in black and white, simple")
    }
}

final class DeveloperToolTests: XCTestCase {
    func testDeveloperStepsDescribeThemselves() {
        XCTAssertEqual(StepText.describe(.object(["op": .string("format_json"), "sort_keys": .bool(true)])),
                       "Format JSON, keys sorted")
        XCTAssertEqual(StepText.describe(.object(["op": .string("format_json"), "minify": .bool(true)])), "Minify JSON")
        XCTAssertEqual(StepText.describe(.object(["op": .string("format_xml"), "minify": .bool(true)])), "Minify XML")
        XCTAssertEqual(StepText.describe(.object(["op": .string("format_xml")])), "Format XML")
        XCTAssertEqual(StepText.describe(.object(["op": .string("optimize_svg")])), "Optimize SVG")
        XCTAssertTrue(StepText.templates.contains { $0.step["op"]?.string == "optimize_svg" })
    }

    func testChecksumSlotsNameTheirAlgorithm() {
        let md5 = Slot(id: "checksum.md5", label: "MD5", symbol: "number", group: "tool",
                       action: .object(["kind": .string("checksum"), "algorithm": .string("md5")]))
        XCTAssertEqual(md5.checksumName, "MD5")
        let plain = Slot(id: "checksum", label: "Copy SHA-256", symbol: "number", group: "tool",
                         action: .object(["kind": .string("checksum")]))
        XCTAssertEqual(plain.checksumName, "SHA-256")
        XCTAssertTrue(Slot(id: "check_syntax", label: "Check syntax", symbol: "checkmark.seal", group: "tool",
                           action: .object(["kind": .string("check_syntax")])).isCheckSyntax)
    }
}

final class WheelLayoutTests: XCTestCase {
    private func slots(_ n: Int) -> [Slot] {
        (0..<n).map { Slot(id: "s\($0)", label: "S\($0)", symbol: "circle", group: "tool", action: .null) }
    }

    func testOverflowGoesBehindMore() {
        let fitting = WheelLayout.fit(slots(12), max: 12)
        XCTAssertEqual(fitting.visible.count, 12)
        XCTAssertTrue(fitting.overflow.isEmpty)
        let over = WheelLayout.fit(slots(15), max: 12)
        XCTAssertEqual(over.visible.count, 12)
        XCTAssertEqual(over.visible.last?.id, WheelLayout.moreID)
        XCTAssertEqual(over.overflow.map(\.id), ["s11", "s12", "s13", "s14"])
    }

    func testAppToolsSitBeforeChecksum() {
        let tools = slots(2) + [Slot(id: "checksum", label: "Copy SHA-256", symbol: "number", group: "tool",
                                     action: .object(["kind": .string("checksum")]))]
        let with = WheelLayout.withAppTools(tools)
        XCTAssertEqual(with.map(\.id), ["s0", "s1", "rename", "copy_path", "copy_name", "open_terminal", "checksum"])
        XCTAssertEqual(WheelLayout.withAppTools(with).count, 7)  // added once
        // Folders have no checksum: the app's tools go last.
        XCTAssertEqual(WheelLayout.withAppTools([]).map(\.id), ["rename", "copy_path", "copy_name", "open_terminal"])
    }

    func testQuickActions() {
        let files = [URL(fileURLWithPath: "/Users/me/My Files/report.pdf"), URL(fileURLWithPath: "/Users/me/Photos")]
        XCTAssertEqual(QuickAction.copyPath.clipboardText(for: files), "/Users/me/My Files/report.pdf\n/Users/me/Photos")
        XCTAssertEqual(QuickAction.copyName.clipboardText(for: files), "report.pdf\nPhotos")
        XCTAssertNil(QuickAction.openTerminal.clipboardText(for: files))
        XCTAssertNil(QuickAction.copyPath.clipboardText(for: []))
        XCTAssertEqual(QuickAction.copyPath.doneTitle(count: 1), "Path copied")
        XCTAssertEqual(QuickAction.copyName.doneTitle(count: 3), "3 names copied")
        let more = files + [URL(fileURLWithPath: "/Users/me/My Files/notes.md")]
        let dirs = QuickAction.terminalFolders(for: more) { $0.lastPathComponent == "Photos" }
        XCTAssertEqual(dirs.map(\.path), ["/Users/me/My Files", "/Users/me/Photos"])
        for q in QuickAction.allCases {
            XCTAssertEqual(QuickAction(slot: q.slot), q)
            XCTAssertNil(q.slot.window)
        }
        let engine = Slot(id: "zip_all", label: "Zip together", symbol: "doc.zipper", group: "tool",
                          action: .object(["kind": .string("zip_all")]))
        XCTAssertNil(QuickAction(slot: engine))
        XCTAssertTrue(engine.isWholeSelection)
        XCTAssertFalse(QuickAction.copyPath.slot.isWholeSelection)
    }

    func testShareAndCopyImage() {
        let photo = [URL(fileURLWithPath: "/p/IMG_1.HEIC")]
        let two = photo + [URL(fileURLWithPath: "/p/IMG_2.jpg")]
        XCTAssertEqual(QuickAction.offered(for: photo), [.copyPath, .copyName, .openTerminal, .share, .copyImage])
        XCTAssertEqual(QuickAction.offered(for: two), [.copyPath, .copyName, .openTerminal, .share])
        XCTAssertFalse(QuickAction.offered(for: [URL(fileURLWithPath: "/p/a.pdf")]).contains(.copyImage))
        XCTAssertEqual(WheelLayout.withAppTools([], files: photo).map(\.id),
                       ["rename", "copy_path", "copy_name", "open_terminal", "share", "copy_image"])

        // Share opens the share menu; its presets name a service.
        let share = QuickAction.share.slot
        XCTAssertNil(QuickAction.shareVia(share))
        XCTAssertEqual(share.variants.map(\.label), ["AirDrop", "Mail", "Messages"])
        let mail = share.running(share.variants[1])
        XCTAssertEqual(QuickAction(slot: mail), .share)
        XCTAssertEqual(QuickAction.shareVia(mail), .mail)
        XCTAssertEqual(mail.label, "Share · Mail")
    }

    func testJobTitles() {
        let one = [URL(fileURLWithPath: "/p/Trip")]
        let jpeg = Slot(id: "to_jpeg", label: "JPEG", symbol: "photo", group: "convert",
                        action: .object(["kind": .string("steps")]))
        XCTAssertEqual(jpeg.jobTitle(files: [URL(fileURLWithPath: "/p/a.png")]), "Converting a.png to JPEG")
        let inside = Slot(id: "inside_jpeg", label: "Images to JPEG", symbol: "photo", group: "convert",
                          action: .object(["kind": .string("convert_inside")]))
        XCTAssertEqual(inside.jobTitle(files: one), "Images to JPEG: Trip")
        let translate = Slot(id: "translate", label: "Translate…", symbol: "character.bubble", group: "tool",
                             action: .object(["kind": .string("steps")]), window: "translate")
        XCTAssertEqual(translate.jobTitle(files: one + one), "Translate: 2 files")
        let book = Slot(id: "combine_workbook", label: "Combine into workbook", symbol: "tablecells", group: "tool",
                        action: .object(["kind": .string("combine_workbook")]))
        XCTAssertTrue(book.isWholeSelection)
        for kind in ["join_media", "burn_subtitles", "scan_documents", "zip_locked"] {
            let slot = Slot(id: kind, label: kind, symbol: "x", group: "tool", action: .object(["kind": .string(kind)]))
            XCTAssertTrue(slot.isWholeSelection, kind)
        }
        XCTAssertEqual(StepText.describe(.object(["op": .string("redact_text"),
                                                  "words": .array(["a", "b", "c"].map { .string($0) }),
                                                  "cards": .bool(true)])),
                       "Black out \u{201C}a\u{201D}, \u{201C}b\u{201D}, 1 more, card numbers")
        XCTAssertEqual(StepText.formatName("jxl"), "JPEG XL")
        let codes = Slot(id: "read_codes", label: "Read QR code", symbol: "qrcode", group: "tool",
                         action: .object(["kind": .string("read_codes")]))
        XCTAssertTrue(codes.isReadCodes)
    }

    func testOnlyReplacementsReplaceOriginals() {
        func slot(_ action: JSONValue) -> Slot {
            Slot(id: "x", label: "X", symbol: "x", group: "tool", action: action)
        }
        func steps(_ list: [JSONValue]) -> JSONValue { .object(["kind": .string("steps"), "steps": .array(list)]) }
        let convert = steps([.object(["op": .string("convert"), "to": .string("jpeg")])])
        XCTAssertTrue(slot(convert).replacesOriginal)
        XCTAssertTrue(slot(steps([.object(["op": .string("compress"), "level": .string("smart")])])).replacesOriginal)
        XCTAssertTrue(slot(.object(["kind": .string("protect_pdf")])).replacesOriginal)
        XCTAssertFalse(slot(ToolAction.translate(to: "fr", from: nil)).replacesOriginal)
        XCTAssertFalse(slot(ToolAction.transcribe(to: "srt", language: nil)).replacesOriginal)
        XCTAssertFalse(slot(steps([.object(["op": .string("convert"), "to": .string("zip")])])).replacesOriginal)
        for kind in ["make_qr", "icon_set", "convert_inside", "list_contents", "flatten", "sort_by_kind", "copy_text"] {
            XCTAssertFalse(slot(.object(["kind": .string(kind)])).replacesOriginal, kind)
        }
        XCTAssertFalse(slot(steps([])).replacesOriginal)
    }

    func testTranslation() {
        let action = ToolAction.translate(to: "fr", from: "auto")
        XCTAssertEqual(action, .object(["kind": .string("steps"),
                                        "steps": .array([.object(["op": .string("translate"), "to": .string("fr")])])]))
        let from = ToolAction.translate(to: "de", from: "en-US")
        guard case .array(let steps)? = from["steps"] else { return XCTFail("no steps") }
        XCTAssertEqual(steps.first?["from"]?.string, "en-US")
        XCTAssertEqual(StepText.describe(.object(["op": .string("translate"), "to": .string("fr")])),
                       "Translate into French")
        XCTAssertEqual(StepText.describe(.object(["op": .string("translate"), "to": .string("de"),
                                                  "from": .string("en")])),
                       "Translate English into German")
        XCTAssertEqual(StepText.describe(.object(["op": .string("blur_faces"), "style": .string("black")])),
                       "Cover faces in black")
        XCTAssertEqual(StepText.describe(.object(["op": .string("blur_faces")])), "Pixelate faces")
        XCTAssertEqual(StepText.formatName("xlsx"), "Excel")
        XCTAssertTrue(StepText.templates.contains { $0.title == "Translate" })

        let all = ToolAction.translationLanguages
        XCTAssertTrue(all.allSatisfy(ToolAction.isLanguageTag))
        XCTAssertEqual(ToolAction.defaultTranslationTarget(preferred: ["fr-CA", "en-US"], available: all), "fr")
        XCTAssertEqual(ToolAction.defaultTranslationTarget(preferred: ["en-US"], available: all), "en-US")
        XCTAssertEqual(ToolAction.defaultTranslationTarget(preferred: ["en-AU"], available: all), "en-GB")
        XCTAssertEqual(ToolAction.defaultTranslationTarget(preferred: ["zh-Hant-TW"], available: all), "zh-Hant")
        XCTAssertEqual(ToolAction.defaultTranslationTarget(preferred: ["zh-CN"], available: all), "zh-Hans")
        XCTAssertEqual(ToolAction.defaultTranslationTarget(preferred: ["pt-PT"], available: all), "pt-BR")
        XCTAssertEqual(ToolAction.defaultTranslationTarget(preferred: ["cy-GB"], available: all), "en-GB")
    }
}

final class HistoryTests: XCTestCase {
    func testNewestFirstCappedAndUndoOrder() {
        var h = JobHistory()
        for i in 0..<(JobHistory.limit + 5) {
            h.add(HistoryEntry(title: "job \(i)", outputs: ["/out/\(i).jpg"]))
        }
        XCTAssertEqual(h.entries.count, JobHistory.limit)
        XCTAssertEqual(h.entries.first?.title, "job \(JobHistory.limit + 4)")
        let first = h.nextUndo!
        h.markUndone(first.id)
        XCTAssertNotEqual(h.nextUndo?.id, first.id)
        XCTAssertEqual(h.entries.first?.summary, "1 file · undone")
        h.add(HistoryEntry(title: "checksum"))  // nothing to undo
        XCTAssertNotEqual(h.nextUndo?.title, "checksum")
    }

    func testSurvivesARoundTrip() {
        var h = JobHistory()
        h.add(HistoryEntry(title: "Rename", renames: [.init(from: "/a/x.jpg", to: "/a/y.jpg")]))
        h.add(HistoryEntry(title: "Compress", outputs: ["/a/z.jpg"],
                           restores: [.init(trashed: "/T/z.jpg", original: "/a/z.jpg")]))
        let back = JobHistory.decode(h.encoded())
        XCTAssertEqual(back.entries.map(\.title), ["Compress", "Rename"])
        XCTAssertEqual(back.entries[1].summary, "Renamed 1 file")
        XCTAssertEqual(back.entries[0].summary, "1 file · 1 original in the Trash")
        XCTAssertEqual(JobHistory.decode(Data("nope".utf8)).entries.count, 0)
    }
}

final class RenamePlanTests: XCTestCase {
    private let utc: Calendar = {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        return c
    }()

    private func input(_ path: String, _ date: TimeInterval? = nil) -> RenamePlan.Input {
        RenamePlan.Input(url: URL(fileURLWithPath: path), date: date.map { Date(timeIntervalSince1970: $0) })
    }

    func testPatternsNumbersAndDates() throws {
        let files = (1...12).map { input("/p/IMG_\($0).JPG", 1_700_000_000 + Double($0) * 3600) }
        let changes = try RenamePlan.make(files, pattern: "Trip {date} {n}", calendar: utc, exists: { _ in false }).get()
        XCTAssertEqual(changes.count, 12)
        XCTAssertEqual(changes[0].to.lastPathComponent, "Trip 2023-11-14 01.JPG")
        XCTAssertEqual(changes[11].to.lastPathComponent, "Trip 2023-11-15 12.JPG")
        let timed = try RenamePlan.make([input("/p/a.png", 0)], pattern: "{name} {time} {n:3}", start: 7,
                                        calendar: utc, exists: { _ in false }).get()
        XCTAssertEqual(timed[0].to.lastPathComponent, "a 00.00.00 007.png")
    }

    func testNamesFromWhatsInside() throws {
        XCTAssertTrue(RenamePlan.needsContent("{date} {Content}"))
        XCTAssertFalse(RenamePlan.needsContent("{name} {n}"))
        let files = [
            RenamePlan.Input(url: URL(fileURLWithPath: "/p/IMG_1.jpg"), date: nil, content: "Red apple on a table"),
            RenamePlan.Input(url: URL(fileURLWithPath: "/p/IMG_2.jpg"), date: nil, content: "Red apple on a table"),
            RenamePlan.Input(url: URL(fileURLWithPath: "/p/IMG_3.jpg"), date: nil, content: nil),
        ]
        let changes = try RenamePlan.make(files, pattern: "{content}", exists: { _ in false }).get()
        XCTAssertEqual(changes.map(\.to.lastPathComponent), ["Red apple on a table.jpg", "Red apple on a table 2.jpg"],
                       "the same content twice gets a number; nothing to go on keeps the name")
    }

    func testNeverClashes() throws {
        // b.txt exists and isn't being renamed; a.txt is.
        let exists: (URL) -> Bool = { ["/d/b.txt", "/d/a.txt"].contains($0.path) }
        let changes = try RenamePlan.make([input("/d/a.txt"), input("/d/c.txt")], pattern: "b",
                                          exists: exists).get()
        XCTAssertEqual(changes.map(\.to.lastPathComponent), ["b 2.txt", "b 3.txt"])
        // Swapping names within the batch is fine (applied in two steps).
        let swap = try RenamePlan.make([input("/d/a.txt"), input("/d/b.txt")], pattern: "{n}", start: 1,
                                       exists: exists).get()
        XCTAssertEqual(swap.map(\.to.lastPathComponent), ["1.txt", "2.txt"])
    }

    func testUnchangedNamesAreSkippedAndBadPatternsExplained() {
        let same = try? RenamePlan.make([input("/d/a.txt")], pattern: "{name}", exists: { _ in true }).get()
        XCTAssertEqual(same, [])
        XCTAssertEqual(RenamePlan.make([input("/d/a.txt")], pattern: "  ", exists: { _ in false }),
                       .failure(.empty))
        XCTAssertEqual(RenamePlan.make([input("/d/a.txt")], pattern: "{colour}", exists: { _ in false }),
                       .failure(.unknownToken("colour")))
        let cleaned = try? RenamePlan.make([input("/d/a.txt")], pattern: "a/b:c", exists: { _ in false }).get()
        XCTAssertEqual(cleaned?.first?.to.lastPathComponent, "a-b-c.txt")
    }
}

final class RecipeEditingTests: XCTestCase {
    private func obj(_ v: JSONValue) -> [String: JSONValue] {
        if case .object(let o) = v { return o }
        return [:]
    }

    func testFieldsRoundTripAndKeepUnknownOptions() throws {
        let step: JSONValue = .object(["op": .string("compress"), "level": .string("strong"),
                                       "target_bytes": .int(500_000)])
        let f = StepFields(step)
        XCTAssertEqual(f.level, "strong")
        XCTAssertEqual(try f.build().get(), step)
        let crop: JSONValue = .object(["op": .string("crop"),
                                       "rect": .object(["x": .double(0.1), "y": .int(0), "w": .double(0.5), "h": .int(1)])])
        XCTAssertFalse(StepFields(crop).hasOptions)
        XCTAssertEqual(try StepFields(crop).build().get(), crop)
    }

    func testTraceOptions() throws {
        let plain: JSONValue = .object(["op": .string("trace")])
        var f = StepFields(plain)
        XCTAssertTrue(f.hasOptions)
        XCTAssertTrue(f.traceColor)
        XCTAssertEqual(f.traceDetail, .medium)
        XCTAssertEqual(try f.build().get(), plain, "defaults stay out of the loadout")
        f.traceColor = false
        f.traceDetail = .high
        XCTAssertEqual(try f.build().get(),
                       .object(["op": .string("trace"), "color": .bool(false), "detail": .string("high")]))
        XCTAssertTrue(StepText.templates.contains { $0.step["op"]?.string == "trace" })
        XCTAssertTrue(StepText.convertTargets.contains("svg"))
    }

    func testEditingChangesOnlyWhatsShown() throws {
        var resize = StepFields(.object(["op": .string("resize"), "max": .int(2048)]))
        XCTAssertEqual(resize.sizeMode, .max)
        resize.sizeMode = .percent
        resize.size = "50"
        XCTAssertEqual(obj(try resize.build().get()), ["op": .string("resize"), "percent": .int(50)])
        resize.size = "150"
        XCTAssertThrowsError(try resize.build().get())

        var convert = StepFields(.object(["op": .string("convert"), "to": .string("avif"), "quality": .int(60)]))
        XCTAssertEqual(convert.formatChoices.last, "avif")
        convert.quality = ""
        XCTAssertEqual(obj(try convert.build().get()), ["op": .string("convert"), "to": .string("avif")])
        convert.quality = "101"
        XCTAssertThrowsError(try convert.build().get())

        var trim = StepFields(.object(["op": .string("trim"), "start": .double(90)]))
        XCTAssertEqual(trim.start, "1:30.0")
        XCTAssertEqual(trim.end, "")
        trim.end = "1:00"
        XCTAssertThrowsError(try trim.build().get())
        trim.end = "2:00"
        XCTAssertEqual(obj(try trim.build().get())["end"], .double(120))

        var mark = StepFields(.object(["op": .string("watermark"), "text": .string("DRAFT")]))
        mark.opacity = 0.1234
        XCTAssertEqual(obj(try mark.build().get())["opacity"], .double(0.12))
        mark.text = "  "
        XCTAssertThrowsError(try mark.build().get())
    }

    func testWholeRecipeChecks() throws {
        let steps = [StepFields(.object(["op": .string("strip_metadata")]))]
        XCTAssertEqual(RecipeCheck.build(name: " ", description: "", steps: steps).failureMessage,
                       "Give the loadout a name.")
        XCTAssertEqual(RecipeCheck.build(name: "A", description: "", steps: []).failureMessage,
                       "Add at least one step.")
        let bad = [StepFields(.object(["op": .string("watermark"), "text": .string("")]))]
        XCTAssertEqual(RecipeCheck.build(name: "A", description: "", steps: bad).failureMessage,
                       "Step 1: Type the watermark's text.")
        let ok = try RecipeCheck.build(name: " Web ", description: " For the blog ", steps: steps).get()
        XCTAssertEqual(ok, .object(["name": .string("Web"), "description": .string("For the blog"),
                                    "steps": .array([.object(["op": .string("strip_metadata")])])]))
    }
}

private extension Result where Failure == EditProblem {
    var failureMessage: String? {
        if case .failure(let p) = self { return p.message }
        return nil
    }
}

final class RenameHelpersTests: XCTestCase {
    func testFreeNamesAndDates() {
        let taken: Set<String> = ["/d/a.txt", "/d/a 2.txt"]
        XCTAssertEqual(RenamePlan.freeName(URL(fileURLWithPath: "/d/a.txt")) { taken.contains($0.path) }.path, "/d/a 3.txt")
        XCTAssertEqual(RenamePlan.freeName(URL(fileURLWithPath: "/d/b.txt")) { taken.contains($0.path) }.path, "/d/b.txt")
        XCTAssertTrue(RenamePlan.needsDates("Trip {Date} {n}"))
        XCTAssertFalse(RenamePlan.needsDates("{name} {n}"))
    }

    func testTrimTimesKeepTheirPrecision() throws {
        let f = StepFields(.object(["op": .string("trim"), "start": .double(1.234), "end": .double(5.678)]))
        XCTAssertEqual(f.start, "0:01.2")
        let built = try f.build().get()
        XCTAssertEqual(built["start"], .double(1.234))
        XCTAssertEqual(built["end"], .double(5.678))
    }
}

final class MigrationTests: XCTestCase {
    private let fm = FileManager.default

    private func write(_ text: String, _ url: URL) throws {
        try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try text.write(to: url, atomically: true, encoding: .utf8)
    }

    private func read(_ url: URL) -> String? { try? String(contentsOf: url, encoding: .utf8) }

    func testFilesFromVeerMoveOnceAndNothingIsOverwritten() throws {
        let support = fm.temporaryDirectory.appendingPathComponent("migrate-\(UUID().uuidString)")
        defer { try? fm.removeItem(at: support) }
        let old = support.appendingPathComponent("Veer")
        let new = support.appendingPathComponent("Radlet")
        try write("web", old.appendingPathComponent("recipes/web.toml"))
        try write("old", old.appendingPathComponent("Skins/mine.css"))
        try write("other", old.appendingPathComponent("Skins/other.css"))
        try write("[]", old.appendingPathComponent("history.json"))
        // A skin of the same name is already in the new place: it stays.
        try write("new", new.appendingPathComponent("Skins/mine.css"))

        XCTAssertEqual(LegacyFiles.migrate(support: support), 3)
        XCTAssertEqual(read(new.appendingPathComponent("Loadouts/web.toml")), "web")
        XCTAssertEqual(read(new.appendingPathComponent("Skins/mine.css")), "new")
        XCTAssertEqual(read(new.appendingPathComponent("Skins/other.css")), "other")
        XCTAssertEqual(read(new.appendingPathComponent("history.json")), "[]")
        // What couldn't move is left where it was, and running again does nothing.
        XCTAssertEqual(read(old.appendingPathComponent("Skins/mine.css")), "old")
        XCTAssertEqual(LegacyFiles.migrate(support: support), 0)

        // Once everything has moved, the old folder goes.
        let clean = fm.temporaryDirectory.appendingPathComponent("migrate-\(UUID().uuidString)")
        defer { try? fm.removeItem(at: clean) }
        try write("a", clean.appendingPathComponent("Veer/recipes/a.toml"))
        XCTAssertEqual(LegacyFiles.migrate(support: clean), 1)
        XCTAssertFalse(fm.fileExists(atPath: clean.appendingPathComponent("Veer").path))
    }

    func testEarlierBuildsAreRecognised() {
        XCTAssertEqual(LegacyFiles.oldBundleIDs.first, "com.usemonoma.radlet", "newest first")
        XCTAssertTrue(LegacyFiles.oldBundleIDs.contains(LegacyFiles.veerBundleID))
        XCTAssertTrue(LegacyFiles.isOldBuild("com.usemonoma.veer"))
        XCTAssertTrue(LegacyFiles.isOldBuild("com.usemonoma.radlet"))
        XCTAssertFalse(LegacyFiles.isOldBuild("com.suvraneel.radlet"), "not this build")
        XCTAssertFalse(LegacyFiles.isOldBuild(nil))
    }

    func testOnlyTheAppsOwnSettingsCarryOver() {
        XCTAssertTrue(LegacyFiles.carriesOver("wheelStyle"))
        XCTAssertTrue(LegacyFiles.carriesOver("watchRules"))
        XCTAssertFalse(LegacyFiles.carriesOver("NSWindow Frame Settings"))
        XCTAssertFalse(LegacyFiles.carriesOver("AppleLanguages"))
    }
}

final class NeedsTests: XCTestCase {
    func testSlotsCarryWhatTheyNeed() throws {
        let json = #"{"id":"to_mp3","label":"MP3","symbol":"music.note","group":"convert","action":{"kind":"steps"},"needs":"ffmpeg","variants":[{"id":"to_mp3.small","label":"Small","hint":"x","action":{"kind":"steps"}}]}"#
        let slot = try JSONDecoder().decode(Slot.self, from: Data(json.utf8))
        XCTAssertEqual(slot.need, .ffmpeg)
        XCTAssertEqual(slot.running(slot.variants[0]).need, .ffmpeg, "a preset needs it too")
        XCTAssertEqual(slot.with(action: .null).need, .ffmpeg)
        XCTAssertNil(slot.unlocked.need)
        XCTAssertEqual(slot.unlocked.variants.count, 1)
        let plain = try JSONDecoder().decode(Slot.self, from: Data(#"{"id":"a","label":"A","symbol":"x","group":"tool","action":{}}"#.utf8))
        XCTAssertNil(plain.need)
        let odd = try JSONDecoder().decode(Slot.self, from: Data(#"{"id":"a","label":"A","symbol":"x","group":"tool","action":{},"needs":"moon"}"#.utf8))
        XCTAssertEqual(odd.needs, "moon")
        XCTAssertNil(odd.need, "a need this app doesn't know runs as usual")
    }

    func testEveryNeedIsExplained() {
        for need in Need.allCases {
            XCTAssertFalse(need.title.isEmpty)
            XCTAssertTrue(need.explanation(task: "Convert to MP3").contains("Convert to MP3"))
            XCTAssertEqual(need.destination == nil, need == .ffmpeg, "\(need)")
        }
        XCTAssertEqual(Need(rawValue: "speech_language"), .speechLanguage)
        XCTAssertEqual(Need.pages.badge, "Needs Pages")
    }

    func testRefreshSaysWhatsHereNow() throws {
        let found = try JSONValue.parse(Data(#"{"ffmpeg":true,"keynote":false,"pages":true,"numbers":false}"#.utf8))
        XCTAssertEqual(Need.ffmpeg.isMet(by: found), true)
        XCTAssertEqual(Need.keynote.isMet(by: found), false)
        XCTAssertEqual(Need.pages.isMet(by: found), true)
        XCTAssertNil(Need.speechLanguage.isMet(by: found), "only the helper can tell")
    }

    func testOnlyACompletePinDownloads() {
        let good = #"{"version":"8.0.1","url":"https://github.com/o/r/releases/download/ffmpeg-8.0.1-1/ffmpeg-8.0.1-macos.zip","sha256":"\#(String(repeating: "a", count: 64))","size":62000000}"#
        XCTAssertEqual(PinnedFFmpeg.parse(Data(good.utf8))?.version, "8.0.1")
        let unpinned = #"{"version":"8.0.1","url":"","sha256":"","size":0}"#
        XCTAssertNil(PinnedFFmpeg.parse(Data(unpinned.utf8)))
        let plainHTTP = good.replacingOccurrences(of: "https://", with: "http://")
        XCTAssertNil(PinnedFFmpeg.parse(Data(plainHTTP.utf8)))
        let shortSum = good.replacingOccurrences(of: String(repeating: "a", count: 64), with: "abc")
        XCTAssertNil(PinnedFFmpeg.parse(Data(shortSum.utf8)))
    }

    func testThePinLeadsToItsSource() throws {
        let sum = String(repeating: "b", count: 64)
        let pin = PinnedFFmpeg(version: "8.0.1", url: "https://github.com/o/r/releases/download/ffmpeg-8.0.1-1/ffmpeg-8.0.1-macos.zip",
                               sha256: sum, size: 1)
        XCTAssertEqual(pin.releasePage?.absoluteString, "https://github.com/o/r/releases/tag/ffmpeg-8.0.1-1")
        let elsewhere = PinnedFFmpeg(version: "8.0.1", url: "https://example.com/ffmpeg.zip", sha256: sum, size: 1)
        XCTAssertNil(elsewhere.releasePage)
    }
}

final class SharedItemTests: XCTestCase {
    let toml = """
    name = "Web-ready image"
    description = "Longest side 2048 px, WebP at quality 80, no metadata"

    [[steps]]
    op = "resize"
    max = 2048
    """

    func testLinksRoundTrip() throws {
        let item = try XCTUnwrap(SharedItem(kind: .loadout, text: toml))
        let link = item.link
        XCTAssertEqual(link.scheme, "radlet")
        XCTAssertTrue(link.absoluteString.hasPrefix("radlet://add/loadout?v=1&data="), link.absoluteString)
        XCTAssertFalse(link.absoluteString.contains("+") || link.absoluteString.contains("/data"))
        XCTAssertEqual(try SharedItem.parse(link: link).get(), item)
        let skin = try XCTUnwrap(SharedItem(kind: .skin, text: ":root { --name: \"Mint\"; --accent: #3FD9A3; }\n"))
        XCTAssertEqual(try SharedItem.parse(link: skin.link).get(), skin)
    }

    func testBadLinksSayWhy() {
        func problem(_ s: String) -> String? {
            guard let url = URL(string: s), case .failure(let p) = SharedItem.parse(link: url) else { return nil }
            return p.message
        }
        XCTAssertNotNil(problem("https://example.com/add/loadout?data=eA"))
        XCTAssertTrue(problem("radlet://add/theme?data=eA")?.contains("doesn't have") == true)
        XCTAssertTrue(problem("radlet://add/loadout?v=2&data=eA")?.contains("newer Radlet") == true)
        XCTAssertTrue(problem("radlet://add/loadout?v=1")?.contains("damaged") == true)
        XCTAssertTrue(problem("radlet://add/loadout?v=1&data=*not*base64*")?.contains("damaged") == true)
        // Empty, and too big.
        XCTAssertTrue(problem("radlet://add/loadout?data=ICAg")?.contains("empty") == true)
        let huge = SharedItem.base64URL(Data(String(repeating: "x", count: 20_000).utf8))
        XCTAssertNotNil(problem("radlet://add/loadout?data=\(huge)"))
    }

    func testPaddedAndSpacedLinksStillOpen() throws {
        let item = try XCTUnwrap(SharedItem(kind: .loadout, text: "name = \"a?b>c\"\n"))
        var standard = Data(item.text.utf8).base64EncodedString()
        XCTAssertTrue(standard.contains("+") || standard.contains("/") || standard.hasSuffix("="))
        standard = standard.replacingOccurrences(of: "+", with: " ")
        let url = try XCTUnwrap(URL(string: "radlet://add/loadout?data=\(standard.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? "")"))
        XCTAssertEqual(try SharedItem.parse(link: url).get(), item)
    }

    func testFiles() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let item = try XCTUnwrap(SharedItem(kind: .loadout, text: toml))
        XCTAssertEqual(item.fileName(for: "Web/ready: image"), "Web-ready- image.radletloadout")
        XCTAssertEqual(item.fileName(for: "  "), "Loadout.radletloadout")
        let file = dir.appendingPathComponent(item.fileName(for: "Web-ready image"))
        try toml.write(to: file, atomically: true, encoding: .utf8)
        XCTAssertEqual(SharedItem.kind(ofFile: file), .loadout)
        XCTAssertEqual(try SharedItem.read(file: file).get(), item)
        XCTAssertEqual(SharedItem.kind(ofFile: dir.appendingPathComponent("Mint.RADLETSKIN")), .skin)
        XCTAssertNil(SharedItem.kind(ofFile: dir.appendingPathComponent("notes.toml")))
        let big = dir.appendingPathComponent("big.radletloadout")
        try String(repeating: "#", count: 17_000).write(to: big, atomically: true, encoding: .utf8)
        XCTAssertThrowsError(try SharedItem.read(file: big).get())
    }

    func testImportedNamesDontClash() {
        XCTAssertEqual(ImportNaming.free("Web-ready image", taken: ["Email-friendly photo"]), "Web-ready image")
        XCTAssertEqual(ImportNaming.free("Web-ready image", taken: ["web-ready image", "Web-ready image 2"]),
                       "Web-ready image 3")
    }
}
