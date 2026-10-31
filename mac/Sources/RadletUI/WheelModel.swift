import AppKit
import SwiftUI
import RadletKit

/// What the wheel shows, and where the pointer is on it. Drawn by
/// WheelView (WheelDrawing.swift); driven by a WheelDriver.
@MainActor
package final class WheelModel: ObservableObject {
    @Published package var mode: WheelMode = .convert
    /// Slots in ring order, slot 0 at 12 o'clock.
    @Published package private(set) var positions: [Slot] = []
    @Published package private(set) var spec = WheelSpec(style: .dial, count: 0)
    @Published package private(set) var look: WheelLook
    @Published package var pick: WheelPick?
    /// The slot whose presets are all out (Bloom's fan, Orbit's widened
    /// ring), and the last one that was.
    @Published package var focus: Int?
    @Published package var lastFocus = 0
    /// Where the highlights point, in degrees; they may wind past 360 so
    /// they always glide the short way.
    @Published package var ringAngle = 0.0
    @Published package var presetAngle = 0.0
    @Published package var presetWidth = 22.5
    /// False while a highlight should snap into place instead of gliding:
    /// when it first appears, or comes back from the centre.
    @Published package var ringGlides = false
    @Published package var presetGlides = false
    /// Orbit: how close the pointer is to the outer ring, 0 to 1.
    @Published package var proximity = 0.0
    /// A brief flash on the rim each time the pick changes.
    @Published package var flash = false
    @Published package var shown = false
    @Published package var committed = false
    @Published package var title = ""
    @Published package var subtitle = ""
    @Published package var loading = true
    @Published package var message: String?
    /// The slot chosen last time in this mode, marked on the rim.
    @Published package var lastID: String?

    package var full: Wheel = .empty
    /// The files the wheel is for, for the app's own tools.
    package var files: [URL] = []
    /// Extensions files should have, by path, when their names are wrong.
    package var fixes: [String: String] = [:]
    package private(set) var style: WheelStyle = .dial
    package private(set) var slotCount = WheelSpec.defaultCount
    /// Slots that didn't fit, behind "More…".
    package private(set) var overflow: [Slot] = []

    package init() {
        look = WheelLook(Skin())
    }

    package var slots: [Slot] { positions }

    /// `reduceMotion` overrides the system setting (the Wheel Lab).
    package func configure(style: WheelStyle, count: Int, skin: Skin, reduceMotion: Bool? = nil) {
        self.style = style
        slotCount = WheelSpec.slotCounts.contains(count) ? count : WheelSpec.defaultCount
        let reduce = reduceMotion ?? NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        if look.skin != skin || look.reduceMotion != reduce { look = WheelLook(skin, reduceMotion: reduce) }
        spec = WheelSpec(style: style, count: positions.count)
    }

    package func apply(_ mode: WheelMode, usage: [String: Int]) {
        self.mode = mode
        let source: [Slot]
        switch mode {
        case .convert: source = full.convert
        case .tools: source = loading ? [] : WheelLayout.withAppTools(full.tools, files: files, fixes: fixes)
        case .recipes: source = full.recipes
        }
        let laid = WheelLayout.fit(source, max: slotCount, usage: usage)
        positions = WheelPlacement.arrange(laid.visible)
        overflow = laid.overflow
        spec = WheelSpec(style: style, count: positions.count)
        pick = nil
        focus = nil
        lastFocus = 0
        proximity = 0
        lastID = UserDefaults.standard.string(forKey: WheelModel.lastKey(mode))
        if loading {
            message = nil
        } else if positions.isEmpty {
            switch mode {
            case .convert: message = "No conversions for these files"
            case .tools: message = "No tools for these files"
            case .recipes: message = "No loadouts for these files"
            }
        } else {
            message = nil
        }
        subtitle = mode.title
    }

    package static func lastKey(_ mode: WheelMode) -> String { "wheelLast.\(mode.rawValue)" }

    package func remember(_ slot: Slot) {
        guard slot.id != WheelLayout.moreID else { return }
        UserDefaults.standard.set(slot.id, forKey: WheelModel.lastKey(mode))
    }

    package var lastIndex: Int? { lastID.flatMap { id in positions.firstIndex { $0.id == id } } }

    /// Every preset a position offers: the slot's own, or for "More…" the
    /// first slots behind it.
    package func presets(at i: Int) -> [SlotVariant] {
        guard positions.indices.contains(i) else { return [] }
        let slot = positions[i]
        return slot.id == WheelLayout.moreID ? WheelLayout.morePresets(overflow) : slot.variants
    }

    /// The presets this style can show for a position (Orbit draws the
    /// first two until its ring widens: see `orbitCells`).
    package func shownPresets(at i: Int) -> [SlotVariant] {
        let all = presets(at: i)
        return Array(all.prefix(spec.shownPresets(all.count)))
    }

    /// Orbit: the preset cells drawn now.
    package var orbitCells: [OrbitCell] {
        spec.orbitCells(presets: (0..<spec.count).map { presets(at: $0).count }, focus: focus)
    }

    package var hoveredBase: Slot? {
        guard let p = pick, positions.indices.contains(p.slot) else { return nil }
        return positions[p.slot]
    }

    package var hoveredPreset: SlotVariant? {
        guard let p = pick, let v = p.preset else { return nil }
        let list = shownPresets(at: p.slot)
        return list.indices.contains(v) ? list[v] : nil
    }

    /// What letting go now would run.
    package var hoveredSlot: Slot? {
        guard let base = hoveredBase else { return nil }
        guard let preset = hoveredPreset else { return base }
        if base.id == WheelLayout.moreID { return overflow.first { $0.id == preset.id } }
        return base.running(preset)
    }

    /// A still, open wheel of `slots`, for previews.
    package static func preview(style: WheelStyle, count: Int, skin: Skin, slots: [Slot] = WheelSamples.images,
                                title: String = "3 photos", reduceMotion: Bool? = nil) -> WheelModel {
        let m = WheelModel()
        m.full = Wheel(convert: slots, tools: [], recipes: [])
        m.loading = false
        m.title = title
        m.configure(style: style, count: count, skin: skin, reduceMotion: reduceMotion)
        m.apply(.convert, usage: [:])
        m.shown = true
        return m
    }
}

/// Sample slots for previews.
package enum WheelSamples {
    /// Image conversions with a few presets: what three photos offer.
    package static let images: [Slot] = {
        func variant(_ id: String, _ label: String) -> SlotVariant {
            SlotVariant(id: id, label: label, hint: label, action: .null)
        }
        let photo = "photo"
        return [
            Slot(id: "to_jpeg", label: "JPEG", symbol: photo, group: "convert", action: .null,
                 variants: [variant("j.smart", "Smart"), variant("j.best", "Best"), variant("j.small", "Small")]),
            Slot(id: "to_png", label: "PNG", symbol: photo, group: "convert", action: .null),
            Slot(id: "to_webp", label: "WebP", symbol: photo, group: "convert", action: .null,
                 variants: [variant("w.smart", "Smart"), variant("w.best", "Best")]),
            Slot(id: "to_heic", label: "HEIC", symbol: photo, group: "convert", action: .null,
                 variants: [variant("h.best", "Best"), variant("h.small", "Small")]),
            Slot(id: "to_pdf", label: "PDF", symbol: "doc.richtext", group: "convert", action: .null,
                 variants: [variant("p.one", "One PDF")]),
            Slot(id: "to_tiff", label: "TIFF", symbol: photo, group: "convert", action: .null),
            Slot(id: "to_gif", label: "GIF", symbol: photo, group: "convert", action: .null),
            Slot(id: "to_bmp", label: "BMP", symbol: photo, group: "convert", action: .null),
            Slot(id: "to_ico", label: "ICO", symbol: photo, group: "convert", action: .null),
            Slot(id: "to_tga", label: "TGA", symbol: photo, group: "convert", action: .null),
            Slot(id: "to_qoi", label: "QOI", symbol: photo, group: "convert", action: .null),
            Slot(id: "to_txt", label: "Text", symbol: "doc.plaintext", group: "convert", action: .null),
        ]
    }()
}
