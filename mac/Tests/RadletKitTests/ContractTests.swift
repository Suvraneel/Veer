import Foundation
import XCTest
@testable import RadletKit

/// A recorded `radlet serve` session, one JSON message per line.
struct Transcript {
    let messages: [JSONValue]
    let raw: [Data]

    init(url: URL) throws {
        let text = try String(contentsOf: url, encoding: .utf8)
        raw = text.split(separator: "\n").map { Data($0.utf8) }
        messages = try raw.map(JSONValue.parse)
    }

    func reply(_ id: Int) -> JSONValue? {
        messages.first { $0["id"]?.int == id }
    }

    func rawReply(_ id: Int) -> Data? {
        zip(messages, raw).first { $0.0["id"]?.int == id }?.1
    }

    func events(job: Int) -> [JobEvent] {
        messages.filter { $0["job"]?.int == job && $0["event"] != nil }.compactMap(JobEvent.init(message:))
    }
}

/// Checks the app's decoders against real helper output. The committed
/// fixture was recorded with `mac/scripts/contract-session.sh`; in CI the
/// same script runs against the freshly built helper and the result is
/// checked too (`RADLET_CONTRACT_DIR`), so a protocol change in the engine
/// that would break the app fails the build.
@MainActor
final class ContractTests: XCTestCase {
    private func fixture() throws -> Transcript {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "session", withExtension: "jsonl", subdirectory: "Fixtures"))
        return try Transcript(url: url)
    }

    func testRecordedSession() throws {
        try verify(fixture())
    }

    func testLiveSessionFromCI() throws {
        guard let dir = ProcessInfo.processInfo.environment["RADLET_CONTRACT_DIR"] else {
            throw XCTSkip("RADLET_CONTRACT_DIR not set; CI records a live session")
        }
        try verify(Transcript(url: URL(fileURLWithPath: dir).appendingPathComponent("session.jsonl")))
    }

    private func verify(_ t: Transcript) throws {
        // hello
        let hello = try XCTUnwrap(t.reply(1)?["result"]).decode(Hello.self)
        XCTAssertEqual(hello.protocol, 1)

        // inspect: an SRT and a Markdown file can both become text.
        let inspect = try XCTUnwrap(t.reply(2)?["result"]).decode(InspectResult.self)
        XCTAssertEqual(inspect.files.map(\.format), ["srt", "md"])
        XCTAssertTrue(inspect.wheel.convert.contains { $0.id == "to_txt" })
        let checksum = try XCTUnwrap(inspect.wheel.tools.first { $0.id == "checksum" })
        XCTAssertTrue(checksum.isChecksum)

        // Actions go back to the engine unchanged: re-encoding must give the
        // same JSON the engine sent.
        let rawInspect = try XCTUnwrap(t.rawReply(2))
        let original = try XCTUnwrap(JSONSerialization.jsonObject(with: rawInspect) as? [String: Any])
        let result = try XCTUnwrap(original["result"] as? [String: Any])
        let wheel = try XCTUnwrap(result["wheel"] as? [String: Any])
        let rawSlots = ["convert", "tools", "recipes"].flatMap { wheel[$0] as? [[String: Any]] ?? [] }
        let slots = inspect.wheel.convert + inspect.wheel.tools + inspect.wheel.recipes
        XCTAssertEqual(rawSlots.count, slots.count)
        for (slot, raw) in zip(slots, rawSlots) {
            let reencoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(slot.action)) as? NSDictionary
            XCTAssertEqual(reencoded, raw["action"] as? NSDictionary, "action for \(slot.id) changed in transit")
        }

        // run: two conversions succeed, the fake PNG fails on its own.
        let job = try XCTUnwrap(t.reply(3)?["result"]?["job"]?.int)
        let events = t.events(job: job)
        let outcomes: [Outcome] = events.compactMap {
            if case .finished(let o) = $0 { return o }
            return nil
        }
        XCTAssertEqual(outcomes.count, 3)
        XCTAssertEqual(outcomes.filter { $0.status == "done" }.count, 2)
        let failed = try XCTUnwrap(outcomes.first { $0.status == "failed" })
        XCTAssertEqual(failed.error?.code, "unsupported_input")
        XCTAssertNil(failed.output)
        guard case .jobFinished(let summary, let cancelled)? = events.last else {
            return XCTFail("job_finished must be the last event")
        }
        XCTAssertEqual(summary, Summary(done: 2, skipped: 0, failed: 1, cancelled: 0))
        XCTAssertFalse(cancelled)

        // The HUD state those events produce.
        let progress = JobProgress()
        progress.reset(title: "test", total: 3)
        events.forEach(progress.handle)
        XCTAssertEqual(progress.phase, .failed)
        XCTAssertEqual(progress.outputs.count, 2)
        XCTAssertEqual(progress.failures.count, 1)
        XCTAssertEqual(progress.fraction, 1)
        XCTAssertTrue(progress.detail.contains("2 done"))
        XCTAssertTrue(progress.detail.contains("1 failed"))

        // checksum job: the hash reaches the clipboard text.
        let checksumJob = try XCTUnwrap(t.reply(4)?["result"]?["job"]?.int)
        let sums = JobProgress()
        sums.reset(title: "sum", total: 1)
        t.events(job: checksumJob).forEach(sums.handle)
        XCTAssertEqual(sums.phase, .finished)
        XCTAssertEqual(sums.clipboardText?.count, 64)

        // Unknown methods are answered with an error, not silence.
        XCTAssertEqual(t.reply(5)?["error"]?["code"]?.string, "unknown_method")

        // Tools with options name their window; one-step tools don't.
        let image = try XCTUnwrap(t.reply(7)?["result"]).decode(InspectResult.self)
        func window(_ w: Wheel, _ id: String) -> String?? { w.tools.first { $0.id == id }.map(\.window) }
        XCTAssertEqual(window(image.wheel, "resize"), "resize")
        XCTAssertEqual(window(image.wheel, "crop"), "crop")
        XCTAssertEqual(window(image.wheel, "redact"), "redact")
        XCTAssertEqual(window(image.wheel, "compress"), "compress")
        XCTAssertEqual(window(image.wheel, "rotate_right"), .some(nil))
        // Presets for the Orbit and Bloom styles travel with their slot,
        // and their actions reach the engine unchanged too.
        let resize = try XCTUnwrap(image.wheel.tools.first { $0.id == "resize" })
        XCTAssertEqual(resize.variants.first?.id, "resize.half")
        XCTAssertEqual(resize.variants.first?.label, "50 %")
        XCTAssertEqual(image.wheel.tools.first { $0.id == "checksum" }?.variants.map(\.label), ["MD5", "SHA-1", "SHA-512"])
        XCTAssertEqual(image.wheel.tools.first { $0.id == "checksum" }?.variants.first?.action["algorithm"]?.string, "md5")
        XCTAssertEqual(image.wheel.convert.first { $0.id == "to_jpeg" }?.variants.map(\.label), ["Smart", "Best", "Small"])
        let run = resize.running(resize.variants[0])
        XCTAssertNil(run.window)
        XCTAssertEqual(run.label, "Resize · 50 %")
        let rawImage = try XCTUnwrap(JSONSerialization.jsonObject(with: try XCTUnwrap(t.rawReply(7))) as? [String: Any])
        let rawTools = try XCTUnwrap(((rawImage["result"] as? [String: Any])?["wheel"] as? [String: Any])?["tools"] as? [[String: Any]])
        let rawResize = try XCTUnwrap(rawTools.first { $0["id"] as? String == "resize" }?["variants"] as? [[String: Any]])
        for (v, raw) in zip(resize.variants, rawResize) {
            let reencoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(v.action)) as? NSDictionary
            XCTAssertEqual(reencoded, raw["action"] as? NSDictionary, "action for \(v.id) changed in transit")
        }
        let pdf = try XCTUnwrap(t.reply(8)?["result"]).decode(InspectResult.self)
        XCTAssertEqual(pdf.files.map(\.format), ["pdf"])
        XCTAssertEqual(window(pdf.wheel, "split_pdf"), "split_pdf")
        XCTAssertEqual(window(pdf.wheel, "pages_pdf"), "pages_pdf")

        // One PDF split into three pages: every file is listed, and the
        // job isn't a candidate for "Replace originals".
        let splitJob = try XCTUnwrap(t.reply(9)?["result"]?["job"]?.int)
        let split = JobProgress()
        split.reset(title: "split", total: 1)
        t.events(job: splitJob).forEach(split.handle)
        XCTAssertEqual(split.phase, .finished)
        XCTAssertEqual(split.outputs.count, 3)
        XCTAssertTrue(split.replaceable.isEmpty)

        // Recipes: a saved file is listed after the built-ins, with its
        // path, and every step survives the Settings editor unchanged.
        let saved = try XCTUnwrap(t.reply(10)?["result"]?["path"]?.string)
        XCTAssertTrue(saved.hasSuffix(".toml"))
        let listing = RecipeListing(try XCTUnwrap(t.reply(11)?["result"]))
        XCTAssertTrue(listing.problems.isEmpty)
        XCTAssertTrue(listing.recipes.first?.builtin ?? false)
        let mine = try XCTUnwrap(listing.recipes.first { !$0.builtin })
        XCTAssertEqual(mine.name, "Contract Test")
        XCTAssertEqual(mine.description, "Saved by the contract script")
        XCTAssertEqual(mine.id, RecipeEntry.key(URL(fileURLWithPath: saved)))
        // Privacy check: one report per file, decoded by the window's model.
        let reports = try PrivacyReport.list(from: try XCTUnwrap(t.reply(12)?["result"]))
        XCTAssertEqual(reports.map(\.format), ["png", "pdf"])
        XCTAssertTrue(reports.allSatisfy { $0.error == nil })
        for recipe in listing.recipes {
            for step in recipe.steps {
                XCTAssertEqual(try StepFields(step).build().get(), step, "\(recipe.name) changed in the editor")
            }
        }
        // A folder offers conversions of the files inside it, titled for
        // the HUD without "Converting … to".
        let folder = try XCTUnwrap(t.reply(13)?["result"]).decode(InspectResult.self)
        let inside = try XCTUnwrap(folder.wheel.convert.first { $0.id == "inside_jpeg" })
        XCTAssertEqual(inside.action["kind"]?.string, "convert_inside")
        XCTAssertEqual(inside.jobTitle(files: [URL(fileURLWithPath: "/x/Album")]), "\(inside.label): Album")
        // Image tools' new presets come through intact (checked above for
        // every image preset); the icon set names its kind.
        let icons = try XCTUnwrap(image.wheel.tools.first { $0.id == "make_icons" })
        XCTAssertEqual(icons.action["set"]?.string, "favicon")
        XCTAssertEqual(icons.variants.map(\.label), ["Favicons", "macOS", "iOS"])
    }

    func testIntegersStayIntegers() throws {
        // The engine parses quality as an integer; 80.0 would be rejected.
        let json = Data(#"{"kind":"steps","steps":[{"op":"convert","quality":80,"to":"webp"}]}"#.utf8)
        let value = try JSONValue.parse(json)
        let text = String(decoding: try JSONEncoder().encode(value), as: UTF8.self)
        XCTAssertTrue(text.contains("\"quality\":80"), text)
        XCTAssertFalse(text.contains("80.0"), text)
    }

    func testProtocolLineEndsWithNewline() throws {
        let line = try JSONValue.object(["id": .int(1), "method": .string("hello")]).line()
        XCTAssertEqual(line.last, 0x0A)
        XCTAssertEqual(line.filter { $0 == 0x0A }.count, 1)
    }

    func testUnknownEventsAreIgnored() {
        XCTAssertNil(JobEvent(message: .object(["job": .int(1), "event": .string("future_event")])))
        XCTAssertNil(JobEvent(message: .object(["job": .int(1), "event": .string("item_finished")])))
    }
}
