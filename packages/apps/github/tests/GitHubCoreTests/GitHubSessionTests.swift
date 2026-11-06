import Foundation
import Testing
@testable import GitHubCore

@Suite("Native credential coordination")
struct GitHubSessionTests {
    @Test("Two native scenes share one refresh and persist the rotated token")
    func simultaneousRefresh() async throws {
        let store = SessionCredentialStore()
        let transport = SessionRefreshTransport()
        let coordinator = GitHubCredentialCoordinator()
        let first = GitHubSession(store: store, transport: transport, coordinator: coordinator)
        let second = GitHubSession(store: store, transport: transport, coordinator: coordinator)
        let taskA = Task { try await first.activeCredentials() }
        await transport.waitUntilStarted()
        let taskB = Task { try await second.activeCredentials() }
        await store.waitForLoads(2)
        await transport.release()
        let a = try await taskA.value, b = try await taskB.value
        #expect(a.token == "new-token" && b.token == "new-token")
        #expect(await transport.requests == 1)
        #expect(try await store.load()?.token == "new-token")
        #expect(await store.saves == 1)
    }
    @Test("Disconnecting a scene prevents an old refresh from restoring credentials")
    func disconnectDuringRefresh() async throws {
        let store = SessionCredentialStore()
        let transport = SessionRefreshTransport()
        let coordinator = GitHubCredentialCoordinator()
        let first = GitHubSession(store: store, transport: transport, coordinator: coordinator)
        let second = GitHubSession(store: store, transport: transport, coordinator: coordinator)
        let task = Task { try await first.activeCredentials() }
        await transport.waitUntilStarted()
        try await second.replace(nil)
        await transport.release()
        await #expect(throws: CancellationError.self) { _ = try await task.value }
        #expect(try await store.load() == nil)
        #expect(await store.saves == 0)
    }
}

private actor SessionCredentialStore: GitHubCredentialStore {
    var saved: GitHubCredentials? = GitHubCredentials(token: "old-token", expiresAt: Date(timeIntervalSince1970: 1), refreshToken: "old-refresh")
    var saves = 0
    private var loads = 0
    private var loadWaiter: (Int, CheckedContinuation<Void, Never>)?
    func waitForLoads(_ count: Int) async {
        if loads >= count { return }
        await withCheckedContinuation { loadWaiter = (count, $0) }
    }
    func load() async throws -> GitHubCredentials? {
        loads += 1
        if let (count, waiter) = loadWaiter, loads >= count { waiter.resume(); loadWaiter = nil }
        return saved
    }
    func save(_ credentials: GitHubCredentials) async throws { saved = credentials; saves += 1 }
    func remove() async throws { saved = nil }
}

private actor SessionRefreshTransport: GitHubHTTPTransport {
    var requests = 0
    private var started: CheckedContinuation<Void, Never>?
    private var response: CheckedContinuation<Void, Never>?
    private var released = false
    func waitUntilStarted() async {
        if requests > 0 { return }
        await withCheckedContinuation { started = $0 }
    }
    func release() { released = true; response?.resume(); response = nil }
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        requests += 1; started?.resume(); started = nil
        if !released { await withCheckedContinuation { response = $0 } }
        let data = Data(#"{"access_token":"new-token","token_type":"bearer","expires_in":3600,"refresh_token":"new-refresh","refresh_token_expires_in":7200}"#.utf8)
        return (data, HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: [:])!)
    }
}
