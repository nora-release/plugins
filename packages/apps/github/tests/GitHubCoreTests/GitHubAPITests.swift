import Foundation
import Testing
@testable import GitHubCore

@Suite("GitHub native data and feed")
struct GitHubAPITests {
    @Test("Search includes only actionable items and explicit managed repositories")
    func relatedFeedScope() async throws {
        let transport = APITransport(response: .object([
            "total_count": .number(2), "incomplete_results": .bool(false),
            "items": .array([Self.item(number: 1), Self.item(number: 2).setting("state", .string("closed"))]),
        ]))
        let result = try await GitHubAPI(token: "test", transport: transport).relatedItems(login: "elonehoo", kind: .pullRequests, managedRepositories: ["nora-release/nora"], page: 1)
        #expect(result.items.count == 1)
        #expect(result.items[0]["repository"]["full_name"].text == "nora-release/nora")
        let request = try #require(await transport.requests.first)
        let query = try #require(URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems)
        let search = try #require(query.first { $0.name == "q" }?.value)
        #expect(search.contains("state:open archived:false"))
        #expect(search.contains("involves:elonehoo OR review-involves:elonehoo OR user:elonehoo OR repo:nora-release/nora"))
        #expect(query.first { $0.name == "advanced_search" }?.value == "true")
    }
    @Test("Managed repository scope requires admin or maintain, excluding archived repositories")
    func managedRepositoryScope() {
        let base: GitHubValue = .object(["full_name": .string("org/repo")])
        #expect(GitHubService.isManagedRepository(base.setting("permissions", .object(["admin": .bool(true)])), login: "me"))
        #expect(GitHubService.isManagedRepository(base.setting("permissions", .object(["maintain": .bool(true)])), login: "me"))
        #expect(!GitHubService.isManagedRepository(base.setting("permissions", .object(["push": .bool(true)])), login: "me"))
        #expect(!GitHubService.isManagedRepository(base.setting("permissions", .object(["admin": .bool(true)])).setting("archived", .bool(true)), login: "me"))
    }
    @Test("Specific unowned repository still requires personal involvement")
    func personalRepositoryScope() async throws {
        let transport = APITransport(response: .object(["total_count": .number(0), "incomplete_results": .bool(false), "items": .array([])]))
        _ = try await GitHubAPI(token: "test", transport: transport).relatedItems(login: "me", kind: .pullRequests, repository: "vitest-dev/vitest")
        let request = try #require(await transport.requests.first)
        #expect(request.url?.absoluteString.contains("review-involves") == true)
        #expect(request.url?.absoluteString.contains("repo:vitest-dev/vitest") == true)
    }
    @Test("Incomplete searches fail instead of presenting a partial list")
    func incompleteSearch() async {
        let transport = APITransport(response: .object(["total_count": .number(0), "incomplete_results": .bool(true), "items": .array([])]))
        await #expect(throws: GitHubAPIError.invalidResponse) {
            _ = try await GitHubAPI(token: "test", transport: transport).relatedItems(login: "me", kind: .issues)
        }
    }
    @Test("Repository input cannot change the API origin or escape a repository path", arguments: ["../repo", "owner/repo?x", "https://other.test/repo", "owner/../repo", "-owner/repo", "owner/repo/name"])
    func invalidRepository(_ repository: String) async {
        #expect(!GitHubAPI.isValidRepository(repository))
        let transport = APITransport(response: .array([]))
        await #expect(throws: GitHubAPIError.invalidRepository) {
            _ = try await GitHubAPI(token: "test", transport: transport).issues(repository: repository)
        }
        #expect(await transport.requests.isEmpty)
    }
    @Test("Native credentials use the existing host store and epoch-millisecond dates")
    func nativeCredentials() async throws {
        let expiry = Date(timeIntervalSince1970: 2_000_000_000)
        let credentials = GitHubCredentials(token: "test", expiresAt: expiry, refreshToken: "refresh", refreshExpiresAt: expiry)
        let data = try JSONEncoder().encode(credentials)
        let value = try JSONDecoder().decode(GitHubValue.self, from: data)
        #expect(value["expiresAt"].int == 2_000_000_000_000)
        #expect(try JSONDecoder().decode(GitHubCredentials.self, from: data) == credentials)
        let host = CoreHostFixture(credentials: String(decoding: data, as: UTF8.self))
        let store = GitHubHostCredentialStore { method, parameters in try await host.request(method, parameters) }
        #expect(try await store.load() == credentials)
        try await store.remove()
        #expect(try await store.load() == nil)
        #expect(await host.methods == ["credentials.get", "credentials.delete", "credentials.get"])
    }
    @Test("API errors distinguish stale merge heads and authorization failures", arguments: [(409, GitHubAPIError.conflict), (401, .unauthenticated), (403, .forbidden), (422, .validationFailed), (503, .serverUnavailable)])
    func responseErrors(_ status: Int, _ error: GitHubAPIError) async {
        let transport = APITransport(response: .object([:]), status: status)
        await #expect(throws: error) { _ = try await GitHubAPI(token: "test", transport: transport).currentUser() }
    }
    @Test("Nora handoff respects UTF-8 byte limits without splitting characters")
    func handoffByteLimit() {
        let value = String(repeating: "🧩", count: 100)
        #expect(GitHubService.boundedUTF8(value, maximumBytes: 255).utf8.count == 252)
        #expect(GitHubService.boundedUTF8(value, maximumBytes: 2).isEmpty)
        #expect(GitHubService.boundedUTF8("GitHub", maximumBytes: 256) == "GitHub")
    }
    static func item(number: Int = 1) -> GitHubValue {
        .object(["id": .number(Double(number)), "number": .number(Double(number)), "title": .string("Change"), "state": .string("open"),
                 "repository_url": .string("https://api.github.com/repos/nora-release/nora"), "pull_request": .object([:])])
    }
}

actor APITransport: GitHubHTTPTransport {
    var requests: [URLRequest] = []
    let response: GitHubValue
    let status: Int
    init(response: GitHubValue, status: Int = 200) { self.response = response; self.status = status }
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        requests.append(request)
        return (try JSONEncoder().encode(response), HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: [:])!)
    }
}

actor CoreHostFixture {
    var credentials: String?
    var methods: [String] = []
    var networkRequests: [GitHubValue] = []
    var changedHead = false
    var historyCount = 0
    let sha = String(repeating: "a", count: 40)
    init(credentials: String? = #"{"token":"test"}"#) { self.credentials = credentials }
    func changeHead() { changedHead = true }
    func enableHistory(count: Int) { historyCount = count }
    func request(_ method: String, _ parameters: GitHubValue) async throws -> GitHubValue {
        methods.append(method)
        if method == "credentials.get" { return credentials.map(GitHubValue.string) ?? .null }
        if method == "credentials.set" { credentials = parameters["value"].string; return .bool(true) }
        if method == "credentials.delete" { credentials = nil; return .bool(true) }
        guard method == "network.request", let url = URL(string: parameters["url"].text) else { return .bool(true) }
        networkRequests.append(parameters)
        var data: GitHubValue = .array([]), status = 200
        var headers: [String: GitHubValue] = [:]
        switch url.path {
        case "/user": data = .object(["login": .string("me")])
        case "/search/issues": data = .object(["total_count": .number(0), "incomplete_results": .bool(false), "items": .array([])])
        case "/repos/org/repo": data = .object(["full_name": .string("org/repo"), "permissions": .object(["push": .bool(true)]), "allow_merge_commit": .bool(true), "allow_squash_merge": .bool(true), "allow_rebase_merge": .bool(false)])
        case "/repos/org/repo/pulls/1":
            data = .object(["id": .number(1), "number": .number(1), "title": .string("Change"), "state": .string("open"), "mergeable": .bool(true), "mergeable_state": .string("clean"), "head": .object(["sha": .string(changedHead ? String(repeating: "b", count: 40) : sha)]), "base": .object(["ref": .string("main"), "repo": .object(["full_name": .string("org/repo")])])])
        case "/repos/org/repo/pulls/1/merge": data = .object(["merged": .bool(true), "sha": .string(sha)])
        case "/repos/org/repo/issues/1/comments" where parameters["method"].text == "POST":
            status = 201; data = .object(["id": .number(22), "body": .string("Comment")])
        default:
            if url.path.hasSuffix("/timeline"), historyCount > 0 {
                let page = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "page" }?.value.flatMap(Int.init) ?? 1
                let start = (page - 1) * 100, end = min(start + 100, historyCount)
                data = .array((start..<end).map { .object(["id": .number(Double(1000 + $0)), "event": .string("head_ref_force_pushed"), "commit_id": .string(String(format: "%040x", $0))]) })
                if end < historyCount { headers["link"] = .string("<https://api.github.com/repos/org/repo/issues/1/timeline?per_page=100&page=\(page + 1)>; rel=\"next\"") }
            }
            if url.path.hasSuffix("/check-runs") { data = .object(["check_runs": .array([])]) }
            if url.path.hasSuffix("/status") { data = .object(["statuses": .array([])]) }
            if url.path.hasSuffix("/actions/runs") { data = .object(["workflow_runs": .array([])]) }
        }
        return .object(["status": .number(Double(status)), "headers": .object(headers), "bodyText": .string(String(decoding: try JSONEncoder().encode(data), as: UTF8.self))])
    }
}
