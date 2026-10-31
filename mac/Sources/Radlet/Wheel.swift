import AppKit
import SwiftUI
import RadletKit
import RadletUI

/// Receives menu choices for "More…".
final class MenuTarget: NSObject {
    var onPick: ((String) -> Void)?

    @objc func pick(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        onPick?(id)
    }
}

/// Borderless panel that can take keyboard focus without activating the app.
final class WheelPanel: NSPanel {
    override var canBecomeKey: Bool { true }
}

/// Receives the drop, tracks the pointer, and handles the keyboard.
final class WheelDropView: NSView {
    weak var controller: WheelController?

    override init(frame: NSRect) {
        super.init(frame: frame)
        registerForDraggedTypes([.fileURL])
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Radlet wheel")
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override var acceptsFirstResponder: Bool { true }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        track(sender)
        return operation(for: sender)
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        track(sender)
        return operation(for: sender)
    }

    /// Holding Command or Control narrows what the drag source allows
    /// (generic or link instead of copy). Answer with an operation inside
    /// that mask, or AppKit refuses the drop. Radlet never moves the file.
    private func operation(for sender: NSDraggingInfo) -> NSDragOperation {
        let mask = sender.draggingSourceOperationMask
        for op: NSDragOperation in [.copy, .generic, .link, .every] where mask.contains(op) {
            return op
        }
        return .copy
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        // Accept the click so the matching mouseUp comes here.
    }

    /// Still dragging with the key held: open presets stay out for when
    /// the pointer comes back.
    override func draggingExited(_ sender: NSDraggingInfo?) {
        MainActor.assumeIsolated { controller?.pointerLeft(keepOpen: true) }
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let urls = sender.draggingPasteboard.readObjects(forClasses: [NSURL.self],
                                                         options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        Log.wheel("drop received: \(describeFiles(urls)), source mask \(sender.draggingSourceOperationMask.rawValue)")
        return MainActor.assumeIsolated { controller?.commit(droppedFiles: urls) ?? false }
    }

    private func offset(_ windowPoint: NSPoint) -> (dx: Double, dy: Double) {
        let p = convert(windowPoint, from: nil)
        return WheelGeometry.offset(appKitPoint: p, side: Double(bounds.width))
    }

    private func track(_ sender: NSDraggingInfo) {
        let o = offset(sender.draggingLocation)
        MainActor.assumeIsolated { controller?.pointer(dx: o.dx, dy: o.dy) }
    }

    /// Mouse (not drag) moves. Beyond the wheel they let go of a pick the
    /// mouse made, but leave one the arrow keys made.
    override func mouseMoved(with event: NSEvent) {
        let o = offset(event.locationInWindow)
        MainActor.assumeIsolated {
            guard let c = controller else { return }
            if (o.dx * o.dx + o.dy * o.dy).squareRoot() > c.reach {
                if c.mouseOwnsPick { c.pointerLeft() }
            } else {
                c.pointer(dx: o.dx, dy: o.dy)
            }
        }
    }

    /// A click beyond the wheel cancels; on it, chooses.
    override func mouseUp(with event: NSEvent) {
        let o = offset(event.locationInWindow)
        MainActor.assumeIsolated {
            guard let c = controller else { return }
            if (o.dx * o.dx + o.dy * o.dy).squareRoot() > c.reach {
                c.hide()
                return
            }
            c.pointer(dx: o.dx, dy: o.dy)
            _ = c.commit(droppedFiles: [])
        }
    }

    override func keyDown(with event: NSEvent) {
        let code = event.keyCode
        let handled = MainActor.assumeIsolated { () -> Bool in
            guard let c = controller else { return false }
            switch code {
            case 53: c.hide()                           // Escape
            case 36, 76: _ = c.commit(droppedFiles: []) // Return, Enter
            case 123, 126: c.step(-1)                   // Left, Up
            case 124, 125: c.step(1)                    // Right, Down
            case 48: c.cycleMode()                      // Tab
            default: return false
            }
            return true
        }
        if !handled { super.keyDown(with: event) }
    }
}

/// Owns the wheel panel: shows it for a selection, asks the engine what
/// the selection can become, follows the pointer and runs the choice.
@MainActor
final class WheelController {
    let model: WheelModel
    var onRun: ((Slot, [URL]) -> Void)?
    var recipesDir: () -> URL? = { nil }

    /// Follows the pointer and updates the model (RadletUI/WheelDriver.swift).
    private let driver: WheelDriver
    private var panel: WheelPanel?
    private var dropView: WheelDropView?
    /// The drawing, centred in the panel's margin.
    private var host: NSView?
    private let menuTarget = MenuTarget()
    private var files: [URL] = []
    private var inspectTask: Task<Void, Never>?
    /// Bumped on every show and hide, so a late fade-out can't hide a
    /// wheel that has since reopened.
    private var generation = 0
    private var hiding = false
    /// Opened from the menu or Services: the keyboard drives it.
    private var keyboard = false

    init() {
        let model = WheelModel()
        self.model = model
        driver = WheelDriver(model: model)
    }

    /// The current pick came from the mouse, not the arrow keys.
    var mouseOwnsPick: Bool { driver.mouseOwnsPick }

    var isVisible: Bool { panel?.isVisible ?? false }

    private func makePanel(size: Double) -> WheelPanel {
        let panel = WheelPanel(contentRect: NSRect(x: 0, y: 0, width: size, height: size),
                               styleMask: [.borderless, .nonactivatingPanel],
                               backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        // Set, not left to default: the clear margin still takes the
        // pointer and the drop instead of passing them to what's behind.
        panel.ignoresMouseEvents = false
        panel.hasShadow = false
        panel.level = .popUpMenu
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.acceptsMouseMovedEvents = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient, .ignoresCycle]

        // The SwiftUI drawing sits underneath a transparent drop view, which
        // receives every click, drag and key press for the wheel.
        let bounds = NSRect(x: 0, y: 0, width: size, height: size)
        let container = NSView(frame: bounds)
        let host = NSHostingView(rootView: WheelView(model: model))
        // `show` sets the size; SwiftUI mustn't fight it when a style
        // change resizes the drawing.
        host.sizingOptions = []
        host.frame = bounds
        container.addSubview(host)
        self.host = host
        let drop = WheelDropView(frame: bounds)
        drop.controller = self
        drop.autoresizingMask = [.width, .height]
        container.addSubview(drop)
        dropView = drop
        panel.contentView = container
        return panel
    }

    /// Shows the wheel centred on `point` (screen coordinates).
    func show(files: [URL], at point: NSPoint, mode: WheelMode, keyboard: Bool) {
        self.files = files
        generation += 1
        hiding = false
        self.keyboard = keyboard
        driver.haptics = Prefs.haptics
        SkinStore.shared.refreshIfChanged()
        model.configure(style: Prefs.wheelStyle, count: Prefs.wheelSlots, skin: SkinStore.shared.current)
        let size = model.spec.panelSize
        let drawing = model.spec.size
        let margin = WheelSpec.panelMargin
        let panel = self.panel ?? makePanel(size: size)
        self.panel = panel
        if abs(Double(panel.frame.width) - size) > 0.5 {
            panel.setContentSize(NSSize(width: size, height: size))
        }
        host?.frame = NSRect(x: margin, y: margin, width: drawing, height: drawing)

        // The drawing stays on screen; the margin round it may hang off.
        var origin = NSPoint(x: Double(point.x) - size / 2, y: Double(point.y) - size / 2)
        if let screen = NSScreen.screens.first(where: { NSMouseInRect(point, $0.frame, false) }) ?? NSScreen.main {
            let f = screen.visibleFrame
            origin.x = min(max(origin.x, f.minX - margin), f.maxX - drawing - margin)
            origin.y = min(max(origin.y, f.minY - margin), f.maxY - drawing - margin)
        }
        panel.setFrameOrigin(origin)

        Log.wheel("shown: \(mode.title), \(model.style.title), \(describeFiles(files)), keyboard \(keyboard)")
        model.full = .empty
        model.files = files
        model.fixes = [:]
        model.loading = true
        model.title = files.count == 1 ? files[0].lastPathComponent : "\(files.count) files"
        apply(mode)
        var still = Transaction()
        still.disablesAnimations = true
        withTransaction(still) {
            model.shown = false
            model.committed = false
        }
        panel.alphaValue = 1
        panel.orderFrontRegardless()
        if keyboard {
            NSApp.activate()
            panel.makeKey()
            panel.makeFirstResponder(dropView)
        }
        // Next turn of the run loop, so the closed state draws first and
        // the opening animates.
        let opening = generation
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0 / 60) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.generation == opening else { return }
                withAnimation(self.model.look.spring) { self.model.shown = true }
            }
        }

        inspectTask?.cancel()
        inspectTask = Task { [weak self] in
            do {
                let result = try await EngineClient.shared.inspect(files, recipesDir: self?.recipesDir())
                guard let self, !Task.isCancelled else { return }
                self.model.full = result.wheel
                self.model.fixes = Dictionary(result.files.compactMap { f in f.fixExtension.map { (f.path, $0) } },
                                              uniquingKeysWith: { a, _ in a })
                self.model.loading = false
                withAnimation(self.model.look.spring) { self.apply(self.model.mode) }
                if keyboard, !self.model.positions.isEmpty { self.select(WheelPick(slot: 0), announce: true) }
                Log.wheel("slots: \(result.wheel.convert.count) convert, \(result.wheel.tools.count) tools, \(result.wheel.recipes.count) recipes")
            } catch {
                guard let self, !Task.isCancelled else { return }
                Log.wheel("inspect failed: \(error.localizedDescription)")
                self.model.loading = false
                self.apply(self.model.mode)
                self.model.message = error.localizedDescription
            }
        }
    }

    /// Lays out `mode` and starts tracking afresh.
    private func apply(_ mode: WheelMode) {
        model.apply(mode, usage: Usage.shared.counts)
        driver.restart(dwell: Prefs.bloomDwell)
    }

    func setMode(_ mode: WheelMode) {
        guard isVisible, !hiding, mode != model.mode else { return }
        apply(mode)
    }

    func cycleMode() {
        guard !hiding else { return }
        apply(model.mode.next)
        if keyboard, !model.positions.isEmpty { select(WheelPick(slot: 0), announce: true) }
    }

    /// The pointer is at (`dx`, `dy`) from the centre, y down.
    func pointer(dx: Double, dy: Double) {
        guard !hiding else { return }
        driver.pointer(dx: dx, dy: dy)
    }

    func pointerLeft(keepOpen: Bool = false) {
        guard !hiding else { return }
        driver.pointerLeft(keepOpen: keepOpen)
    }

    /// How far out the mouse still counts when it isn't dragging: past the
    /// rim, or past Bloom's petals.
    var reach: Double { driver.reach }

    private func select(_ pick: WheelPick?, announce: Bool = false) {
        driver.select(pick)
        if announce, let panel, let label = model.hoveredSlot?.label {
            NSAccessibility.post(element: panel, notification: .announcementRequested,
                                 userInfo: [.announcement: label,
                                            .priority: NSAccessibilityPriorityLevel.high.rawValue])
        }
    }

    func step(_ delta: Int) {
        guard !hiding else { return }
        let n = model.positions.count
        guard n > 0 else { return }
        let current = model.pick?.slot ?? (delta > 0 ? -1 : 0)
        select(WheelPick(slot: ((current + delta) % n + n) % n), announce: true)
    }

    /// Runs the hovered slot or preset. Letting go in the centre cancels.
    @discardableResult
    func commit(droppedFiles: [URL]) -> Bool {
        // Already chosen: a second Return or click mustn't run it twice.
        guard !hiding else { return false }
        let targets = droppedFiles.isEmpty ? files : droppedFiles
        guard let slot = model.hoveredSlot, !targets.isEmpty else {
            Log.wheel("cancelled: no slot under the pointer")
            hide()
            return false
        }
        Log.wheel("chose \(slot.id) for \(describeFiles(targets))")
        if let base = model.hoveredBase {
            model.remember(base)
            // A preset counts for its slot too, so the slot stays on the wheel.
            if model.hoveredPreset != nil, base.id != WheelLayout.moreID { Usage.shared.record(base) }
        }
        let overflow = model.overflow
        hide(committed: true)
        if slot.id == WheelLayout.moreID {
            // After the drop has finished: a menu here would hold the drag
            // source until it closed.
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated { self?.showMore(overflow, files: targets) }
            }
        } else {
            onRun?(slot, targets)
        }
        return true
    }

    /// The slots that didn't fit, as a menu at the pointer.
    private func showMore(_ slots: [Slot], files: [URL]) {
        let menu = NSMenu()
        for slot in slots {
            let title = slot.need.map { "\(slot.label) (needs \($0.name))" } ?? slot.label
            let item = NSMenuItem(title: title, action: #selector(MenuTarget.pick(_:)), keyEquivalent: "")
            item.image = NSImage(systemSymbolName: slot.symbol, accessibilityDescription: nil)
            item.target = menuTarget
            item.representedObject = slot.id
            menu.addItem(item)
        }
        menuTarget.onPick = { [weak self] id in
            guard let slot = slots.first(where: { $0.id == id }) else { return }
            Log.wheel("chose \(slot.id) from More")
            self?.onRun?(slot, files)
        }
        NSApp.activate()
        menu.popUp(positioning: nil, at: NSEvent.mouseLocation, in: nil)
    }

    /// Called when the drag ends anywhere else.
    func dragEnded() {
        // Give performDragOperation a moment to arrive first.
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 150_000_000)
            self?.hide()
        }
    }

    /// Fades the wheel out: outward after a choice, inward when cancelled.
    func hide(committed: Bool = false) {
        inspectTask?.cancel()
        driver.cancelDwell()
        guard let panel, panel.isVisible else {
            driver.clear()
            return
        }
        // Already fading out (a drop, then the drag ending).
        guard !hiding else { return }
        hiding = true
        generation += 1
        let mine = generation
        withAnimation(.timingCurve(0.2, 0.8, 0.2, 1, duration: committed ? 0.2 : 0.14)) {
            model.committed = committed
            model.shown = false
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + (committed ? 0.2 : 0.14)) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.generation == mine else { return }
                self.hiding = false
                panel.orderOut(nil)
                self.driver.clear()
            }
        }
    }
}
