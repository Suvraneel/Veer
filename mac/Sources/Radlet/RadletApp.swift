import AppKit
import Combine
import SwiftUI
import RadletKit

@main
struct RadletApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var state = AppState.shared

    var body: some Scene {
        MenuBarExtra("Radlet", systemImage: "circle.circle") {
            MenuContent(state: state)
        }
        Settings {
            SettingsView(state: state)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        // Writing to a helper that just died must fail with an error,
        // not terminate the app.
        signal(SIGPIPE, SIG_IGN)
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Cancels tool-window previews and removes their temporary files.
        MainActor.assumeIsolated { AppState.shared.tools.close() }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        MainActor.assumeIsolated { AppState.shared.start(showWelcome: true) }
    }

    /// Finder's Open With → Radlet, shared loadout and skin files, and
    /// radlet:// links. On a cold launch this arrives before
    /// applicationDidFinishLaunching, so start here, without the welcome
    /// window (it would steal the keyboard from the wheel).
    func application(_ application: NSApplication, open urls: [URL]) {
        MainActor.assumeIsolated {
            AppState.shared.start(showWelcome: false)
            // Loadouts and skins open their own windows; other files go to
            // the wheel.
            let files = SharedImport.shared.take(urls).filter(\.isFileURL)
            guard !files.isEmpty else { return }
            Log.app("open with: \(describeFiles(files))")
            AppState.shared.showWheel(for: files, mode: .convert)
        }
    }
}

private struct MenuContent: View {
    @ObservedObject var state: AppState
    @ObservedObject private var updates = Updates.shared

    var body: some View {
        Button("Convert Files…") { state.pickFiles(mode: .convert) }
            .keyboardShortcut("o")
        Button("Tools for Files…") { state.pickFiles(mode: .tools) }
        Button("Run a Loadout…") { state.pickFiles(mode: .recipes) }
        Button("Convert Clipboard") { state.convertClipboard() }
            .keyboardShortcut("v", modifiers: [.command, .shift])
        Button("Convert Finder Selection") { state.convertFinderSelection() }
            .keyboardShortcut("v", modifiers: [.control, .option])
        Divider()
        Button("Undo Last Job") { state.undo() }
            .disabled(!state.canUndo)
        Button("History…") { state.history.show() }
            .keyboardShortcut("y")
        Button("Open Loadouts Folder") { state.openRecipesFolder() }
        Divider()
        Text(state.engineStatus)
        Button("Setup and Tutorial…") { state.welcome.show() }
        if updates.isAvailable {
            Button("Check for Updates…") { updates.checkForUpdates() }
                .disabled(!updates.canCheck)
        }
        Menu("Troubleshooting") {
            Button("Copy Diagnostics") { Diagnostics.copy(engineStatus: state.engineStatus) }
            Button("Show Log File") { Diagnostics.revealLog() }
        }
        SettingsLink { Text("Settings…") }
            .keyboardShortcut(",")
        Button("Quit Radlet") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }
}

/// Carries an earlier build's settings and files over: from Radlet before
/// 0.1.0 (bundle ID com.usemonoma.radlet) and from Veer (com.usemonoma.veer).
/// Settings are copied once; Veer's files are moved on every launch while
/// its folder is still there (moving never overwrites, so anything that
/// couldn't move is tried again).
enum LegacySettings {
    private static let doneKey = "migratedFromVeer"
    private static let noticeKey = "veerNoticeShown"

    @MainActor
    static func migrate() {
        quitOldApp()
        let defaults = UserDefaults.standard
        var copied = 0
        var fromVeer = 0
        if !defaults.bool(forKey: doneKey) {
            defaults.set(true, forKey: doneKey)
            // Newest first, so an older build only fills in what's missing.
            for id in LegacyFiles.oldBundleIDs {
                guard let old = defaults.persistentDomain(forName: id) else { continue }
                for (key, value) in old where LegacyFiles.carriesOver(key) && defaults.object(forKey: key) == nil {
                    defaults.set(value, forKey: key)
                    copied += 1
                    if id == LegacyFiles.veerBundleID { fromVeer += 1 }
                }
            }
        }
        let moved = LegacyFiles.migrate(support: Prefs.applicationSupport)
        if copied + moved > 0 { Log.app("carried over \(copied) settings and \(moved) files from earlier builds") }
        // The notice is about Veer; an earlier Radlet build needs none.
        if fromVeer + moved > 0 && !defaults.bool(forKey: noticeKey) { needsNotice = true }
    }

    /// Something came over from Veer and the user hasn't been told yet.
    @MainActor private(set) static var needsNotice = false

    /// Says once that Veer is now Radlet, and what to redo.
    @MainActor
    static func showNoticeIfNeeded() {
        guard needsNotice else { return }
        needsNotice = false
        UserDefaults.standard.set(true, forKey: noticeKey)
        let alert = NSAlert()
        alert.messageText = "Veer is now Radlet"
        alert.informativeText = "Your settings, loadouts (formerly recipes), skins and History came along.\n\n"
            + "Delete Veer.app from Applications so it doesn't open at login. If you used Start at login "
            + "or the command-line tool, turn them on again in Radlet's Settings: the command is now radlet."
        alert.addButton(withTitle: "OK")
        NSApp.activate()
        alert.runModal()
    }

    /// An earlier build still running would open a second wheel on every
    /// drag and keep writing to its own settings: ask it to quit.
    static func quitOldApp() {
        let old = LegacyFiles.oldBundleIDs.flatMap { NSRunningApplication.runningApplications(withBundleIdentifier: $0) }
        guard !old.isEmpty else { return }
        old.forEach { $0.terminate() }
        Log.app("asked \(old.count) older build(s) to quit")
    }
}

/// Wires the drag monitor, wheel, engine and HUD together.
@MainActor
final class AppState: ObservableObject {
    static let shared = AppState()

    @Published var engineStatus = "Starting engine…"
    @Published private(set) var canUndo = false

    let wheel = WheelController()
    let hud = HUDController()
    let drag = DragWatcher()
    let welcome = WelcomeController()
    let tools = ToolWindowController()
    let history = HistoryWindowController()
    private let services = ServiceProvider()
    private var started = false
    /// The History entry for the job the HUD shows, for its Undo button.
    private var hudEntry: UUID?
    /// Files chosen by Convert Clipboard; their results go back on the
    /// clipboard.
    private var clipboardFiles: Set<URL>?
    /// True while results are dragged out of the HUD, so that drag doesn't
    /// open the wheel.
    private var draggingOut = false
    /// The wheel on screen was opened by a Finder drag (not the menu or ⌃⌥V).
    private var openedByDrag = false

    private init() {
        // Before anything reads history, loadouts, skins or settings.
        LegacySettings.migrate()
        HistoryStore.shared.$history
            .map { $0.nextUndo != nil }
            .removeDuplicates()
            .assign(to: &$canUndo)
    }

    func start(showWelcome: Bool) {
        guard !started else { return }
        started = true
        ScratchFolder.removeLeftovers()
        ClipboardInput.removeOld()
        let info = Bundle.main.infoDictionary ?? [:]
        Log.app("launched \(info["CFBundleShortVersionString"] as? String ?? "dev") on "
                + ProcessInfo.processInfo.operatingSystemVersionString)
        try? FileManager.default.createDirectory(at: Prefs.loadoutsDir, withIntermediateDirectories: true)
        Updates.shared.start()

        // Finder right-click → Services.
        NSApp.servicesProvider = services
        NSUpdateDynamicServices()

        wheel.recipesDir = { Prefs.loadoutsDir }
        wheel.onRun = { [weak self] slot, files in
            self?.notePractice(slot)
            self?.run(slot, files: files)
        }
        tools.onApply = { [weak self] slot, files in self?.run(slot, files: files) }
        tools.onRenamed = { [weak self] renames in self?.renamed(renames) }

        drag.isEnabled = { [weak self] in Prefs.gestureEnabled && !(self?.draggingOut ?? false) }
        drag.excludedApps = { Set(Prefs.excludedApps) }
        drag.onTrigger = { [weak self] mode, files, point in
            self?.clipboardFiles = nil
            self?.openedByDrag = true
            self?.wheel.show(files: files, at: point, mode: mode, keyboard: false)
        }
        drag.onModeChange = { [weak self] mode in self?.wheel.setMode(mode) }
        drag.onDragEnded = { [weak self] in self?.wheel.dragEnded() }
        drag.start()

        hud.onCancel = { [weak self] in
            if let job = self?.hud.progress.job { EngineClient.shared.cancel(job: job) }
        }
        // Only the job shown: until it's recorded (originals still going to
        // the Trash) there's nothing to undo yet.
        hud.onUndo = { [weak self] in
            guard let self else { return }
            if let id = self.hudEntry {
                self.undo(id)
            } else {
                self.notify("Undo", detail: "Not ready yet; try again in a moment")
            }
        }
        hud.onDragging = { [weak self] dragging in self?.draggingOut = dragging }
        // Next…: the wheel again, for what the job made.
        hud.onNext = { [weak self] outputs in
            guard let self else { return }
            let still = outputs.filter { FileManager.default.fileExists(atPath: $0.path) }
            guard !still.isEmpty else { return }
            self.hud.hide()
            self.showWheel(for: still, mode: .convert)
        }

        FinderShortcut.shared.onPress = { [weak self] result in self?.openFinderSelection(result) }
        FinderShortcut.shared.setEnabled(Prefs.finderShortcut)

        WatchManager.shared.start()
        Task { await checkEngine() }
        if showWelcome { welcome.showIfFirstLaunch() }

        // An earlier build may open later (it's often a login item): it quits again.
        NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didLaunchApplicationNotification, object: nil, queue: .main
        ) { note in
            let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
            if LegacyFiles.isOldBuild(app?.bundleIdentifier) { LegacySettings.quitOldApp() }
        }
        Task { LegacySettings.showNoticeIfNeeded() }
    }

    func checkEngine() async {
        do {
            let hello = try await EngineClient.shared.hello()
            let caps = hello.capabilities
            let media = caps.ffmpeg ? "audio and video on"
                : caps.avfoundation == true ? "audio and video through macOS (FFmpeg adds more)"
                : "install FFmpeg for audio and video"
            let vision = hello.capabilities.vision == true ? " · text recognition on" : ""
            engineStatus = "Engine \(hello.version) · \(media)\(vision)"
        } catch {
            engineStatus = error.localizedDescription
        }
        Log.app("engine status: \(engineStatus)")
    }

    /// Keyboard and menu path: choose files, then pick from the wheel with
    /// the arrow keys, Return, Escape and Tab.
    func pickFiles(mode: WheelMode) {
        NSApp.activate()
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.prompt = "Choose"
        guard panel.runModal() == .OK, !panel.urls.isEmpty else { return }
        showWheel(for: panel.urls, mode: mode)
    }

    /// Opens the wheel for files chosen without a drag (menu, Services,
    /// Open With), centred on the pointer's screen, for the keyboard.
    func showWheel(for files: [URL], mode: WheelMode) {
        let mouse = NSEvent.mouseLocation
        let screen = (NSScreen.screens.first { NSMouseInRect(mouse, $0.frame, false) } ?? NSScreen.main)?.visibleFrame ?? .zero
        clipboardFiles = nil
        openedByDrag = false
        wheel.show(files: files, at: NSPoint(x: screen.midX, y: screen.midY), mode: mode, keyboard: true)
    }

    /// The tutorial's checklist: a convert or a tool chosen from a wheel
    /// that a Finder drag opened.
    private func notePractice(_ slot: Slot) {
        guard openedByDrag else { return }
        switch slot.group {
        case "convert": Practice.post(.convert)
        case "recipe": break
        default: Practice.post(.tools)
        }
    }

    /// ⌃⌥V and the menu: the wheel for the files selected in Finder.
    func convertFinderSelection() {
        openFinderSelection(FinderSelection.read())
    }

    private func openFinderSelection(_ result: Result<[URL], FinderSelectionError>) {
        switch result {
        case .success(let files):
            Log.app("Finder selection: \(describeFiles(files))")
            Practice.post(.shortcut)
            showWheel(for: files, mode: .convert)
        case .failure(let problem):
            notify("Finder selection", detail: problem.message)
        }
    }

    /// Opens the wheel for what's on the clipboard: copied files, or a
    /// copied image saved as a PNG.
    func convertClipboard() {
        guard let files = ClipboardInput.read() else {
            notify("Nothing to convert on the clipboard", detail: "Copy files in Finder, or an image, then try again")
            return
        }
        Log.app("convert clipboard: \(describeFiles(files))")
        showWheel(for: files, mode: .convert)
        clipboardFiles = Set(files.map(\.standardizedFileURL))
    }

    /// How one job's results are handled once it finishes.
    private struct JobContext {
        let generation: UUID
        /// Results go back on the clipboard (Convert Clipboard).
        let toClipboard: Bool
        /// The inputs are copied images in Caches, never replaced.
        let temporary: Bool
        /// The results stand in for the originals (see Slot.replacesOriginal).
        let replaces: Bool
        /// What ran, to run again on the files that needed something first.
        let slot: Slot
    }

    func run(_ slot: Slot, files: [URL]) {
        // Copy path, Copy name and Open in Terminal happen here and now.
        if let quick = QuickAction(slot: slot) {
            Usage.shared.record(slot)
            runQuick(quick, slot: slot, files: files)
            return
        }
        // Something to install first (FFmpeg, Keynote…). It may have been
        // installed since the wheel was drawn (from the App Store, or with
        // Homebrew in Terminal), so look again; otherwise offer it, then
        // carry on as if it had been there all along.
        if let need = slot.need {
            Task { [self] in
                if let found = try? await EngineClient.shared.refresh(), need.isMet(by: found) == true {
                    Log.app("\(need.rawValue) is here now")
                    if need == .ffmpeg { await checkEngine() }
                    run(slot.unlocked, files: files)
                    return
                }
                InstallWindowController.shared.offer(need, task: slot.plainLabel) { [weak self] in
                    self?.run(slot.unlocked, files: files)
                }
            }
            return
        }
        // Tools with options (Crop, Compress, Split…) open their window
        // first; it calls back here with the finished action.
        if slot.window != nil {
            tools.open(slot, files: files)
            return
        }
        Usage.shared.record(slot)
        let fromClipboard = clipboardFiles == Set(files.map(\.standardizedFileURL))
        clipboardFiles = nil
        let context = JobContext(generation: UUID(), toClipboard: fromClipboard,
                                 temporary: ClipboardInput.isTemporary(files), replaces: slot.replacesOriginal,
                                 slot: slot)
        let title = slot.jobTitle(files: files)
        let total = slot.isWholeSelection ? 1 : files.count
        let kind: JobProgress.ValueKind = slot.isCopyText ? .text
            : slot.isDescribe ? .description
            : slot.isReadCodes ? .code
            : slot.isCheckSyntax ? .check
            : slot.isDataURI ? .dataURI
            : slot.isWordCount ? .count : .checksum
        Log.app("running \(slot.id) on \(describeFiles(files))\(fromClipboard ? " from the clipboard" : "")")
        // The HUD shows the newest job. Each job also keeps its own record,
        // so one the HUD has moved on from still reaches History.
        let record = JobProgress()
        record.reset(title: title, total: total)
        record.valueKind = kind
        record.checksumName = slot.checksumName
        let p = hud.progress
        p.reset(title: title, total: total)
        p.valueKind = kind
        p.checksumName = slot.checksumName
        p.generation = context.generation
        hudEntry = nil
        hud.show()
        // AppState lives for the whole app run, so strong captures are fine.
        Task { [self] in
            do {
                let job = try await EngineClient.shared.run(
                    files: files, action: slot.action, output: Prefs.outputJSON(temporarySource: context.temporary),
                    isolate: Prefs.isolate
                ) { event in
                    record.handle(event)
                    if self.hud.progress.generation == context.generation { self.hud.progress.handle(event) }
                    if case .jobFinished = event { self.finished(record, context) }
                }
                if hud.progress.generation == context.generation { hud.progress.job = job }
            } catch {
                guard hud.progress.generation == context.generation else { return }
                hud.progress.failures = [error.localizedDescription]
                hud.progress.phase = .failed
                hud.reposition()
            }
        }
    }

    private func runQuick(_ action: QuickAction, slot: Slot, files: [URL]) {
        Log.app("quick action \(action.rawValue) on \(describeFiles(files))")
        switch action {
        case .copyPath, .copyName:
            guard let text = action.clipboardText(for: files) else { return }
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
            notify(action.doneTitle(count: files.count), detail: files.count == 1 ? text : "On the clipboard, one per line")
        case .openTerminal:
            let folders = QuickAction.terminalFolders(for: files) { url in
                (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            }
            guard !folders.isEmpty,
                  let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal") else {
                notify("Open in Terminal", detail: "Terminal isn't on this Mac")
                return
            }
            let config = NSWorkspace.OpenConfiguration()
            config.activates = true
            NSWorkspace.shared.open(folders, withApplicationAt: terminal, configuration: config) { _, error in
                if let error { Log.app("open in Terminal failed: \(Log.code(error))") }
            }
        case .share:
            if let problem = Sharing.shared.share(files, via: QuickAction.shareVia(slot)) {
                notify("Share", detail: problem)
            }
        case .copyImage:
            guard let file = files.first, Sharing.shared.copyImage(file) else {
                notify("Copy image", detail: "Couldn't read this picture")
                return
            }
            notify(action.doneTitle(count: 1), detail: "Paste it into any app")
        }
    }

    private func finished(_ record: JobProgress, _ context: JobContext) {
        let current = hud.progress.generation == context.generation
        Log.app("job finished: \(record.outputs.count) done, \(record.skipped.count) skipped, "
                + "\(record.failures.count) failed\(current ? "" : " (behind a newer job)")")
        if current, let text = record.clipboardText {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
        }
        // Items that failed for want of something this Mac can get: offer
        // each thing, then try again the items that needed it.
        if current {
            for need in record.needs.compactMap(Need.init(rawValue:)) {
                var seen = Set<URL>()
                let again = (record.needing[need.rawValue] ?? []).filter { seen.insert($0).inserted }
                InstallWindowController.shared.offer(need, task: context.slot.plainLabel) { [weak self] in
                    if !again.isEmpty { self?.run(context.slot.unlocked, files: again) }
                }
            }
        }
        if Prefs.replaceOriginals && context.replaces && !context.temporary && !record.replaceable.isEmpty {
            // Recorded once the originals are in the Trash, so Undo can put
            // them back.
            replaceOriginals(record, context)
        } else {
            complete(record, outputs: record.outputs, restores: [], context)
        }
    }

    /// Sends originals to the Trash and renames same-format results.
    private func replaceOriginals(_ record: JobProgress, _ context: JobContext) {
        Replacer.run(record.replaceable) { [weak self] result in
            guard let self else { return }
            MainActor.assumeIsolated {
                let n = result.restores.count
                Log.app("replaced \(n) original\(n == 1 ? "" : "s")")
                let outputs = record.outputs.map { result.renamed[$0] ?? $0 }
                self.complete(record, outputs: outputs, restores: result.restores, context)
            }
        }
    }

    /// Records a finished job in History and, if the HUD still shows it,
    /// says how it went.
    private func complete(_ record: JobProgress, outputs: [URL], restores: [HistoryEntry.Restore],
                          _ context: JobContext) {
        let id = HistoryStore.shared.record(title: record.title, outputs: outputs, restores: restores)
        let copied = context.toClipboard && !outputs.isEmpty && record.clipboardText == nil
        if copied { ClipboardInput.copy(outputs) }
        let p = hud.progress
        guard p.generation == context.generation else { return }
        hudEntry = id
        p.outputs = outputs
        var extras: [String] = []
        if !restores.isEmpty { extras.append("\(restores.count) original\(restores.count == 1 ? "" : "s") in the Trash") }
        if copied { extras.append("on the clipboard") }
        if !extras.isEmpty {
            let base = p.detail
            p.message = ([base] + extras).joined(separator: " · ")
        }
        hud.finish()
    }

    /// Batch Rename renamed files in its window; record it for Undo.
    private func renamed(_ renames: [HistoryEntry.Rename]) {
        let n = renames.count
        let title = "Renamed \(n) file\(n == 1 ? "" : "s")"
        let id = HistoryStore.shared.record(title: title, outputs: [], renames: renames)
        guard notify(title, detail: "Undo puts the old names back") else { return }
        hud.progress.outputs = renames.map { URL(fileURLWithPath: $0.to) }
        hudEntry = id
        hud.reposition()
    }

    /// Undoes one job from History, or the newest one not yet undone.
    func undo(_ id: UUID? = nil) {
        let store = HistoryStore.shared
        // A job named but no longer listed has nothing to undo; with no
        // job named, the newest one not yet undone.
        let found: HistoryEntry? = id.map { id in store.history.entries.first { $0.id == id } } ?? store.nextUndo
        guard let entry = found else {
            notify("Undo", detail: "Nothing to undo")
            return
        }
        store.undo(entry.id) { [weak self] message in
            self?.notify(entry.title, detail: message)
        }
    }

    /// Shows a short message in the HUD, unless a job is running there.
    /// Returns whether it was shown.
    @discardableResult
    func notify(_ title: String, detail: String) -> Bool {
        let p = hud.progress
        guard p.phase != .running || p.title.isEmpty else { return false }
        p.reset(title: title, total: 0)
        p.message = detail
        p.phase = .finished
        p.generation = UUID()
        hudEntry = nil
        hud.show()
        hud.finish()
        return true
    }

    func openRecipesFolder() {
        let dir = Prefs.loadoutsDir
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        NSWorkspace.shared.open(dir)
    }
}
