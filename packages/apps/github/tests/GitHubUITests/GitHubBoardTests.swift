import AppKit
import SwiftUI
import Testing
@testable import GitHubCore
@testable import GitHubUI

@MainActor
struct GitHubBoardTests {
    @Test func columnsGroupByRepositoryWithTheMostRecentlyUpdatedFirst() {
        let items = [
            item("acme/web", 1, updated: "2026-11-10T08:00:00Z"),
            item("acme/api", 7, updated: "2026-11-12T08:00:00Z", pullRequest: true),
            item("acme/web", 2, updated: "2026-11-11T08:00:00Z"),
            item("ACME/Web", 3, updated: "2026-11-09T08:00:00Z"),
        ]
        let columns = GitHubBoardColumn.columns(items, filter: .all, query: "")
        #expect(columns.map(\.repository) == ["acme/api", "acme/web"])
        #expect(columns[1].items.map { $0["number"].int } == [2, 1, 3])
        #expect(columns[1].owner == "acme" && columns[1].name == "web")
    }

    @Test func filterAndSearchNarrowTheColumns() {
        let items = [
            item("acme/web", 1, title: "Sidebar collapses"),
            item("acme/api", 7, title: "Paginate projects", pullRequest: true),
        ]
        #expect(GitHubBoardColumn.columns(items, filter: .issues, query: "").map(\.repository) == ["acme/web"])
        #expect(GitHubBoardColumn.columns(items, filter: .pullRequests, query: "").map(\.repository) == ["acme/api"])
        #expect(GitHubBoardColumn.columns(items, filter: .all, query: "sidebar").map(\.repository) == ["acme/web"])
        #expect(GitHubBoardColumn.columns(items, filter: .all, query: "7").map(\.repository) == ["acme/api"])
        #expect(GitHubBoardColumn.columns(items, filter: .pullRequests, query: "sidebar").isEmpty)
    }

    @Test func aColumnShowsItsOwnersPicture() {
        let column = GitHubBoardColumn(repository: "vue-hooks-form/vue-hooks-form", items: [])
        #expect(GitHubRepositoryOwnerAvatar(column: column).url?.absoluteString == "https://github.com/vue-hooks-form.png?size=64")
        #expect(GitHubRepositoryOwnerAvatar(column: GitHubBoardColumn(repository: "bad owner/x", items: [])).url == nil)
    }

    @Test func aCardShowsAtMostThreeLabelsAndCountsTheRest() {
        let labels = ["bug", "ui", "p1", "macos", ""].map { GitHubValue.object(["name": .string($0), "color": .string("f2726f")]) }
        let value = item("acme/web", 1).setting("labels", .array(labels))
        let shown = value.boardLabels()
        #expect(shown.labels.map(\.name) == ["bug", "ui", "p1"])
        #expect(shown.more == 1)
        #expect(item("acme/web", 2).boardLabels().labels.isEmpty)
    }

    @Test func theBoardLaysOutOneColumnPerRepository() async throws {
        let service = GitHubService()
        service.user = .object(["id": .number(1), "login": .string("board-test")])
        service.items = [item("acme/web", 1), item("acme/api", 2, pullRequest: true), item("acme/web", 3)]
        let host = NSHostingView(rootView: GitHubPanel(service: service))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 760), styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        window.contentView = host
        for _ in 0 ..< 8 {
            try await Task.sleep(for: .milliseconds(10))
            host.layoutSubtreeIfNeeded()
        }
        // The board scrolls sideways, and each repository's column scrolls on its own.
        let scrollViews = descendants(of: host).compactMap { $0 as? NSScrollView }
        #expect(scrollViews.count == 3)
        #expect(scrollViews.filter { $0.frame.width < 400 }.count == 2)
    }

    private func item(_ repository: String, _ number: Int, title: String = "An item", updated: String = "2026-11-12T08:00:00Z",
                      pullRequest: Bool = false) -> GitHubValue {
        var value: [String: GitHubValue] = [
            "id": .number(Double(number)), "number": .number(Double(number)), "title": .string(title), "state": .string("open"),
            "updated_at": .string(updated), "user": .object(["login": .string("someone")]),
            "repository": .object(["full_name": .string(repository)]),
        ]
        if pullRequest { value["pull_request"] = .object([:]) }
        return .object(value)
    }

    private func descendants(of view: NSView) -> [NSView] {
        view.subviews.flatMap { [$0] + descendants(of: $0) }
    }
}

@MainActor
struct GitHubBoardAccountTests {
    @Test func aLoadedAvatarStaysAvatarSizedBesideTheAccountMenu() throws {
        let service = GitHubService()
        service.user = .object(["id": .number(1), "login": .string("a-long-github-account-name"),
                                "avatar_url": .string("https://avatars.githubusercontent.com/u/1")])
        let host = NSHostingView(rootView: GitHubBoardAccount(service: service, reconnect: {}))
        host.frame = NSRect(x: 0, y: 0, width: 320, height: 30)
        host.layoutSubtreeIfNeeded()
        #expect(host.fittingSize.height <= 30.5)
        let controls = descendants(of: host).compactMap { $0 as? NSControl }
        #expect(!controls.isEmpty)
        #expect(controls.allSatisfy { $0.frame.height <= 30 })
    }

    private func descendants(of view: NSView) -> [NSView] {
        view.subviews.flatMap { [$0] + descendants(of: $0) }
    }
}
