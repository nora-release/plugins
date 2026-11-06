import AppKit
import Testing
@testable import GitHubUI

@MainActor
struct GitHubNativeDiffTests {
    @Test func nativeDiffFitsShortFilesAndScrollsLongLinesWithoutWrapping() async throws {
        let controller = GitHubDiffViewController()
        defer { controller.invalidate() }
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 620, height: 150)
        let long = String(repeating: "a", count: 240)
        let patch = "@@ -8,3 +8,3 @@\n context\n-old\n+\(long)\n tail\n"
        controller.configure(patch: patch, layout: .unified, showsFileHeaders: false, fileName: "sample.txt")
        try await finish(controller)
        controller.view.layoutSubtreeIfNeeded()
        let scrolls = controller.view.subviews.compactMap { $0 as? NSScrollView }
        let before = try #require(scrolls.first)
        let table = try #require(before.documentView as? NSTableView)
        #expect(before.hasHorizontalScroller)
        #expect(table.frame.width > before.contentSize.width * 2)
        #expect(controller.contentHeight == CGFloat(table.numberOfRows) * 22)
        #expect(controller.contentHeight < controller.view.bounds.height)
        let deletion = try #require(controller.tableView(table, viewFor: table.tableColumns[0], row: 2))
        let fields = deletion.subviews.compactMap { $0 as? NSTextField }
        #expect(fields.contains { $0.stringValue.contains("9") && $0.stringValue.contains("−") })
        #expect(fields.contains { $0.stringValue == "old" && $0.maximumNumberOfLines == 1 })
    }

    @Test func splitDiffSynchronizesVerticalMovementAndKeepsHorizontalPositionsIndependent() async throws {
        let controller = GitHubDiffViewController()
        defer { controller.invalidate() }
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 640, height: 150)
        let long = String(repeating: "long-line-", count: 30)
        let patch = "@@ -1,20 +1,20 @@\n-old\n+new\n" + Array(repeating: " " + long, count: 19).joined(separator: "\n") + "\n"
        controller.configure(patch: patch, layout: .split, showsFileHeaders: false, fileName: "sample.txt")
        try await finish(controller)
        controller.view.layoutSubtreeIfNeeded()
        let scrolls = controller.view.subviews.compactMap { $0 as? NSScrollView }
        #expect(scrolls.count == 2)
        let before = try #require(scrolls.first), after = try #require(scrolls.last)
        #expect(!before.isHidden && !after.isHidden)
        #expect(abs(before.frame.width - after.frame.width) <= 1)
        before.contentView.scroll(to: NSPoint(x: 120, y: 44))
        before.reflectScrolledClipView(before.contentView)
        NotificationCenter.default.post(name: NSView.boundsDidChangeNotification, object: before.contentView)
        #expect(before.contentView.bounds.origin.y == after.contentView.bounds.origin.y)
        #expect(before.contentView.bounds.origin.x > 0)
        #expect(after.contentView.bounds.origin.x == 0)
        after.contentView.scroll(to: NSPoint(x: 60, y: 88))
        after.reflectScrolledClipView(after.contentView)
        NotificationCenter.default.post(name: NSView.boundsDidChangeNotification, object: after.contentView)
        #expect(before.contentView.bounds.origin.y == after.contentView.bounds.origin.y)
        #expect(before.contentView.bounds.origin.x != after.contentView.bounds.origin.x)
        controller.configure(patch: patch, layout: .unified, showsFileHeaders: false, fileName: "sample.txt")
        try await finish(controller)
        #expect(after.isHidden)
    }

    private func finish(_ controller: GitHubDiffViewController) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(3))
        while controller.isParsing {
            #expect(ContinuousClock.now < deadline)
            if ContinuousClock.now >= deadline { return }
            try await Task.sleep(for: .milliseconds(5))
        }
    }
}
