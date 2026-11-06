import Foundation
import Testing
@testable import GitHubCore

@Suite("Native GitHub mutations") @MainActor
struct GitHubServiceTests {
    @Test("Merge requires explicit confirmation and sends the selected method with the reviewed head")
    func mergeConfirmation() async throws {
        let host = CoreHostFixture()
        let model = GitHubService { method, parameters in try await host.request(method, parameters) }
        await model.restore()
        await model.switchKind(.pullRequests)
        await model.selectItem(item)
        await model.prepareMerge(.squash)
        #expect(model.mergeProposal?.method == .squash)
        #expect(await host.networkRequests.allSatisfy { $0["method"].text != "PUT" })
        await model.confirmMerge()
        let writes = await host.networkRequests.filter { $0["method"].text == "PUT" }
        #expect(writes.count == 1)
        let body = try JSONDecoder().decode(GitHubValue.self, from: Data(writes[0]["body"].text.utf8))
        #expect(body["merge_method"].text == "squash")
        #expect(body["sha"].text == String(repeating: "a", count: 40))
        #expect(model.detail?["merged"].bool == true)
    }
    @Test("A new head after merge confirmation prevents a write")
    func staleHead() async {
        let host = CoreHostFixture()
        let model = GitHubService { method, parameters in try await host.request(method, parameters) }
        await model.restore(); await model.switchKind(.pullRequests); await model.selectItem(item)
        await model.prepareMerge(.merge)
        await host.changeHead()
        await model.confirmMerge()
        #expect(model.mergeError == GitHubAPIError.conflict.localizedDescription)
        #expect(model.mergeProposal == nil)
        #expect(await host.networkRequests.allSatisfy { $0["method"].text != "PUT" })
    }
    @Test("A disabled repository merge method cannot be prepared")
    func disabledMethod() async {
        let host = CoreHostFixture()
        let model = GitHubService { method, parameters in try await host.request(method, parameters) }
        await model.restore(); await model.switchKind(.pullRequests); await model.selectItem(item)
        await model.prepareMerge(.rebase)
        #expect(model.mergeProposal == nil)
        #expect(model.mergeError != nil)
    }
    @Test("Comments use the native host connection and the issue comments endpoint")
    func comment() async {
        let host = CoreHostFixture()
        let model = GitHubService { method, parameters in try await host.request(method, parameters) }
        await model.restore(); await model.switchKind(.pullRequests); await model.selectItem(item)
        await model.postComment("Comment")
        let writes = await host.networkRequests.filter { $0["method"].text == "POST" }
        #expect(writes.count == 1)
        #expect(writes[0]["url"].text == "https://api.github.com/repos/org/repo/issues/1/comments")
        #expect(model.commentNotice != nil)
    }
    @Test("Disconnect removes host credentials and all account content")
    func disconnect() async {
        let host = CoreHostFixture()
        let model = GitHubService { method, parameters in try await host.request(method, parameters) }
        await model.restore(); await model.switchKind(.pullRequests); await model.selectItem(item)
        await model.disconnect()
        #expect(model.user == nil && model.detail == nil && model.items.isEmpty)
        #expect(await host.credentials == nil)
    }
    @Test("Independent native panels keep their selection and filters separate")
    func separatePanels() async {
        let host = CoreHostFixture()
        let first = GitHubService { method, parameters in try await host.request(method, parameters) }
        let second = GitHubService { method, parameters in try await host.request(method, parameters) }
        await first.restore(); await second.restore()
        await first.switchKind(.pullRequests); await first.selectItem(item)
        first.query = "first panel"; first.sidebarVisible = false
        #expect(first.detail != nil)
        #expect(second.detail == nil)
        #expect(second.kind == .issues && second.query.isEmpty && second.sidebarVisible)
        await second.selectRepository("another/repository")
        #expect(first.repository.isEmpty)
        #expect(first.detail != nil)
    }
    @Test("Discussion history automatically loads beyond two hundred timeline events")
    func completeTimeline() async {
        let host = CoreHostFixture()
        await host.enableHistory(count: 250)
        let model = GitHubService { method, parameters in try await host.request(method, parameters) }
        await model.restore(); await model.switchKind(.pullRequests); await model.selectItem(item)
        #expect(model.sections[.timeline]?.items.count == 250)
        #expect(model.sections[.timeline]?.nextPage == nil)
        let requests = await host.networkRequests.filter { $0["url"].text.contains("/timeline?") }
        #expect(requests.count == 3)
    }
    @Test("Escape is handled only when a panel has something to dismiss")
    func escapeHandling() async {
        let host = CoreHostFixture()
        let model = GitHubService { method, parameters in try await host.request(method, parameters) }
        #expect(await model.performPanelAction("closeDetails") == false)
        model.onUIAction = { $0 == "closeDetails" }
        #expect(await model.performPanelAction("closeDetails"))
        model.onUIAction = nil
        await model.restore(); await model.selectItem(item)
        #expect(await model.performPanelAction("closeDetails"))
        #expect(model.detail == nil)
    }
    private var item: GitHubValue { .object(["id": .number(1), "number": .number(1), "title": .string("Change"), "state": .string("open"), "repository": .object(["full_name": .string("org/repo")])]) }
}
